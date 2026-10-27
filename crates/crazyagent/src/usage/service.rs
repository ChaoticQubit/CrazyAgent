use super::capture::{Attribution, Format, RunKind, scan_home, scan_runs};
use super::poll::{CLAUDE_USAGE_URL, CODEX_USAGE_URL, Provider, poll};
use super::{LimitSource, failure, record_failure, record_observation, storage};
use crate::config::{ModelAccess, ModelConnection};
use crate::engine::bootstrap::NativeProfileUsePurpose;
use crate::engine::owner::HostStamp;
use crate::engine::{EngineError, NativeProfileCustody};
use crate::pools::MemberKind;
use crate::protocol::{CredentialRef, ProjectId, RequestId};
use rusqlite::{Connection, OptionalExtension, params};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

pub const SCAN_EVERY: Duration = Duration::from_secs(5);
pub const POLL_EVERY_MS: i64 = 10 * 60 * 1000;
const STOPPED_SCAN_MS: i64 = 10 * 60 * 1000;
const AGENTS_PER_SCAN_MAX: i64 = 256;

/// Reads the live model access; none means no pool is configured.
pub type AccessSource = Box<dyn Fn() -> Option<ModelAccess> + Send>;

/// Asks the usage service to poll every signed-in account at its next scan, as the owner's
/// Refresh does. Inputs: the store and the time. Output: done, or `usage_unavailable`.
pub fn request_refresh(db: &Connection, now_ms: i64) -> Result<(), EngineError> {
    db.execute(
        "INSERT INTO usage_refresh (singleton,requested_ms) VALUES (1,?1)
         ON CONFLICT(singleton) DO UPDATE SET requested_ms=MAX(requested_ms,excluded.requested_ms)",
        params![now_ms],
    )
    .map_err(storage)?;
    Ok(())
}

fn refresh_requested(db: &Connection) -> Result<Option<i64>, EngineError> {
    db.query_row(
        "SELECT requested_ms FROM usage_refresh WHERE singleton=1",
        [],
        |row| row.get(0),
    )
    .optional()
    .map_err(storage)
}

/// The code stored for a failed poll: the engine's own code when it is one, so raw engine
/// text such as a path never reaches the store or the page. Input: the failure. Output:
/// the code, or `usage_poll_failed`.
pub fn failure_code(problem: &EngineError) -> String {
    let code = problem.0.as_str();
    let shaped = !code.is_empty()
        && code.len() <= super::FAILURE_BYTES_MAX
        && code
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_');
    if shaped { code } else { "usage_poll_failed" }.to_owned()
}

/// The pool that serves a project's runs on an account: the first pool, in settings order,
/// that lists both. Inputs: the access, the project and the account. Output: the pool.
pub fn pool_of(
    access: &ModelAccess,
    project: &str,
    account: &CredentialRef,
) -> Option<CredentialRef> {
    access
        .pools
        .iter()
        .find(|pool| {
            pool.members.contains(account) && pool.projects.iter().any(|p| p.as_str() == project)
        })
        .map(|pool| pool.id.clone())
}

/// One engine's usage upkeep: it counts what every team agent and research run's records
/// gained and polls every signed-in subscription account's limits.
pub struct Usage {
    db: Connection,
    team: PathBuf,
    custody: Result<NativeProfileCustody, EngineError>,
    access: AccessSource,
    executables: BTreeMap<&'static str, PathBuf>,
    endpoints: (url::Url, url::Url, bool),
    polled_ms: Option<i64>,
    polls: u64,
}

impl Usage {
    /// Opens the upkeep for an engine database. Inputs: the database, the team state folder,
    /// the live access and the harness executables by harness id. Output: the upkeep.
    pub fn open(
        database: &Path,
        team: &Path,
        access: AccessSource,
        executables: BTreeMap<&'static str, PathBuf>,
    ) -> Result<Self, EngineError> {
        let root = database
            .parent()
            .filter(|root| root.is_absolute())
            .ok_or_else(|| failure("usage_unavailable"))?;
        let db = super::open(root)?;
        let custody = std::fs::symlink_metadata(database)
            .map_err(storage)
            .and_then(|metadata| NativeProfileCustody::capture(database, &metadata));
        let endpoint = |text: &str| url::Url::parse(text).map_err(storage);
        Ok(Self {
            db,
            team: team.to_path_buf(),
            custody,
            access,
            executables,
            endpoints: (
                endpoint(CLAUDE_USAGE_URL)?,
                endpoint(CODEX_USAGE_URL)?,
                false,
            ),
            polled_ms: None,
            polls: 0,
        })
    }

    /// Points the polls at other usage calls, such as a loopback server in a test.
    /// Inputs: the Claude and Codex URLs. Output: the upkeep.
    pub fn with_endpoints(mut self, claude: url::Url, codex: url::Url) -> Self {
        self.endpoints = (claude, codex, true);
        self
    }

    /// Runs one upkeep step at `now_ms`: counts every live or lately stopped agent's and
    /// recent research run's records, then polls every account when ten minutes passed
    /// since the last poll or the owner asked to refresh since. Output: done, or the
    /// store's failure.
    pub fn step(&mut self, now_ms: i64) -> Result<(), EngineError> {
        let access = (self.access)().unwrap_or_default();
        self.scan_team(&access, now_ms)?;
        scan_runs(
            &self.db,
            |project, account| pool_of(&access, project, account),
            now_ms,
        )?;
        let refresh = refresh_requested(&self.db)?;
        let due = match self.polled_ms {
            None => true,
            Some(polled) => {
                now_ms - polled >= POLL_EVERY_MS || refresh.is_some_and(|at| at > polled)
            }
        };
        if due {
            self.polled_ms = Some(now_ms);
            self.poll_all(&access, now_ms)?;
        }
        Ok(())
    }

    fn scan_team(&self, access: &ModelAccess, now_ms: i64) -> Result<(), EngineError> {
        let rows: Vec<(i64, String, String, String, String)> = {
            let mut statement = match self.db.prepare(
                "SELECT id,project,role,harness,connection FROM team_agents
                 WHERE connection IS NOT NULL
                   AND (status IN ('starting','idle','working') OR stopped_ms>=?1)
                 ORDER BY id DESC LIMIT ?2",
            ) {
                Ok(statement) => statement,
                Err(_) => return Ok(()),
            };
            statement
                .query_map(
                    params![now_ms - STOPPED_SCAN_MS, AGENTS_PER_SCAN_MAX],
                    |row| {
                        Ok((
                            row.get(0)?,
                            row.get(1)?,
                            row.get(2)?,
                            row.get(3)?,
                            row.get(4)?,
                        ))
                    },
                )
                .map_err(storage)?
                .collect::<Result<_, _>>()
                .map_err(storage)?
        };
        for (id, project, role, harness, connection) in rows {
            let (Some(format), Ok(project), Ok(account)) = (
                Format::of(&harness),
                ProjectId::new(project),
                CredentialRef::new(connection),
            ) else {
                continue;
            };
            let attribution = Attribution {
                kind: if role == "master" {
                    RunKind::Master
                } else {
                    RunKind::Agent
                },
                pool: pool_of(access, project.as_str(), &account),
                project,
                account,
                agent: format!("team-{id}"),
                harness,
            };
            let home = self
                .team
                .join("agents")
                .join(format!("agent-{id}"))
                .join("home");
            scan_home(&self.db, &home, format, &attribution, now_ms)?;
        }
        Ok(())
    }

    fn poll_all(&mut self, access: &ModelAccess, now_ms: i64) -> Result<(), EngineError> {
        for connection in &access.connections {
            if !matches!(
                MemberKind::of(connection),
                MemberKind::Claude | MemberKind::Chatgpt
            ) {
                continue;
            }
            match self.poll_one(connection, now_ms) {
                Ok(Some(observation)) => record_observation(
                    &self.db,
                    &connection.id,
                    &observation,
                    LimitSource::Poll,
                    now_ms,
                )?,
                Ok(None) => {}
                Err(problem) => {
                    record_failure(&self.db, &connection.id, &failure_code(&problem), now_ms)?;
                }
            }
        }
        Ok(())
    }

    /// Polls one managed account: proves its sign-in as an agent turn does, takes a shared
    /// lease on its profile only to read the access token, then makes the provider's usage
    /// call with the lease released. Inputs: the connection and the time. Output: the
    /// observation, None for an account that is not signed in or is busy signing in, or
    /// the poll's plain failure.
    fn poll_one(
        &mut self,
        connection: &ModelConnection,
        now_ms: i64,
    ) -> Result<Option<super::Observation>, EngineError> {
        let (provider, harness) = match MemberKind::of(connection) {
            MemberKind::Claude => (Provider::Claude, "claude-code"),
            MemberKind::Chatgpt => (Provider::Codex, "codex"),
            _ => return Ok(None),
        };
        let Some(executable) = self.executables.get(harness).cloned() else {
            return Ok(None);
        };
        let tx = self.db.unchecked_transaction().map_err(storage)?;
        let host = tx
            .query_row(
                "SELECT db_instance,engine_epoch FROM owner_meta WHERE singleton=1 AND schema_version=1 AND runtime_state='active'",
                [],
                |row| {
                    Ok(HostStamp {
                        db_instance: row.get(0)?,
                        engine_epoch: row.get(1)?,
                    })
                },
            )
            .optional()
            .map_err(storage)?;
        let Some(host) = host else {
            return Ok(None);
        };
        let Ok(fact) = crate::storage::native_sign_in(&tx, &host, connection, None) else {
            return Ok(None);
        };
        if crate::model_gateway::sign_in_rejected(&tx, connection)? {
            return Err(failure("usage_sign_in_rejected"));
        }
        drop(tx);
        self.polls += 1;
        let operation = RequestId::new(format!("usage-poll-{}", self.polls)).map_err(storage)?;
        let custody = self.custody.as_mut().map_err(|error| error.clone())?;
        let lease = match custody.acquire_use(
            &host,
            &fact.binding,
            &operation,
            1,
            NativeProfileUsePurpose::Execution,
        ) {
            Ok(lease) => lease,
            Err(problem) if problem.0 == "native_profile_in_use" => return Ok(None),
            Err(problem) => return Err(problem),
        };
        let credential = lease.usage_credential(&executable);
        drop(lease);
        let credential = credential?;
        let (claude, codex, loopback) = &self.endpoints;
        let url = match provider {
            Provider::Claude => claude.clone(),
            Provider::Codex => codex.clone(),
        };
        poll(provider, url, &credential, *loopback, now_ms).map(Some)
    }
}

/// The running usage upkeep thread.
pub struct UsageService {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl UsageService {
    /// Stops the thread and waits for its current step. Output: none.
    pub fn stop(mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

/// Starts the usage upkeep for a running engine: it steps every `SCAN_EVERY`, reading the
/// pools live from the engine's settings files. Inputs: the runtime folder, both settings
/// files and the harness executables by harness id. Output: the service, or a reason.
pub fn start_for_engine(
    runtime: &Path,
    global: &Path,
    project: &Path,
    executables: BTreeMap<&'static str, PathBuf>,
) -> Result<UsageService, String> {
    let runtime = runtime
        .canonicalize()
        .map_err(|e| format!("usage runtime: {e}"))?;
    let (global, project) = (global.to_path_buf(), project.to_path_buf());
    let access: AccessSource = Box::new(move || {
        crate::config::resolve_config(&global, &project)
            .ok()?
            .settings
            .model_access
    });
    let mut usage = Usage::open(
        &runtime.join("engine.db"),
        &runtime.join("team"),
        access,
        executables,
    )
    .map_err(|e| e.0)?;
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    let thread = std::thread::Builder::new()
        .name("usage".into())
        .spawn(move || {
            let mut last = None;
            while !flag.load(Ordering::SeqCst) {
                let problem = usage.step(now_ms()).err().map(|e| e.0);
                if problem.is_some() && problem != last {
                    eprintln!("usage: {}", problem.as_deref().unwrap_or_default());
                }
                last = problem;
                let mut waited = Duration::ZERO;
                while waited < SCAN_EVERY && !flag.load(Ordering::SeqCst) {
                    std::thread::sleep(Duration::from_millis(100));
                    waited += Duration::from_millis(100);
                }
            }
        })
        .map_err(|e| format!("usage thread: {e}"))?;
    Ok(UsageService {
        stop,
        thread: Some(thread),
    })
}
