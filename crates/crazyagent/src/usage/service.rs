use super::capture::{Attribution, Format, RunKind, run_attribution, scan_agent_home, scan_runs};
use super::health::{UsageStep, clear, record_panic, record_recovered};
use super::poll::{CLAUDE_USAGE_URL, CODEX_USAGE_URL, Provider, poll};
use super::recount;
use super::report::{mark_unreported, reports_usage};
use super::{LimitSource, failure, record_failure, record_observation, storage};
use crate::config::{ModelAccess, ModelConnection};
use crate::engine::bootstrap::NativeProfileUsePurpose;
use crate::engine::owner::HostStamp;
use crate::engine::{EngineError, NativeProfileCustody};
use crate::pools::MemberKind;
use crate::protocol::{CredentialRef, ProjectId, RequestId};
use rusqlite::{Connection, OptionalExtension, params};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

pub const SCAN_EVERY: Duration = Duration::from_secs(5);
pub const POLL_EVERY_MS: i64 = 10 * 60 * 1000;
const STOPPED_SCAN_MS: i64 = 10 * 60 * 1000;
const AGENTS_PER_SCAN_MAX: i64 = 256;
const RECOUNT_AGENTS_MAX: i64 = 8 * 1024;
const RECOUNT_RUNS_MAX: i64 = 8 * 1024;
const STEP_PANICKED: &str = "usage_step_panicked";
const PANIC_PAUSE_MAX: Duration = Duration::from_secs(10 * 60);
const PANIC_DOUBLINGS_MAX: u32 = 8;

const _: () = assert!(SCAN_EVERY.as_millis() <= PANIC_PAUSE_MAX.as_millis());

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

/// Who a team agent's tokens count against: the master as `master-<project>`, a sub-agent
/// as `team-<id>`, with no project or account when its row names no valid one. Inputs:
/// the agent id, its project, whether it is the master, its harness, its connection and
/// the live access. Output: the attribution.
pub fn team_attribution(
    id: i64,
    project: &str,
    master: bool,
    harness: &str,
    connection: Option<&str>,
    access: &ModelAccess,
) -> Attribution {
    let project = ProjectId::new(project).ok();
    let account = connection.and_then(|connection| CredentialRef::new(connection).ok());
    let kind = if master {
        RunKind::Master
    } else {
        RunKind::Agent
    };
    let agent = team_agent_key(id, project.as_ref(), master);
    Attribution {
        kind,
        pool: match (&project, &account) {
            (Some(project), Some(account)) => pool_of(access, project.as_str(), account),
            _ => None,
        },
        project,
        account,
        agent,
        harness: harness.to_owned(),
    }
}

/// The usage key a team agent's tokens and turns count against: `master_agent` for the
/// master, `team-<id>` for a sub-agent. Inputs: the agent id, its project and whether it is
/// the master. Output: the key.
pub fn team_agent_key(id: i64, project: Option<&ProjectId>, master: bool) -> String {
    if master {
        master_agent(project)
    } else {
        format!("team-{id}")
    }
}

/// Names a project's master as an agent row: `master-<project>`, or `master` when the run
/// is tied to no project. Input: the project. Output: the agent key.
pub fn master_agent(project: Option<&ProjectId>) -> String {
    project.map_or_else(
        || "master".to_owned(),
        |project| format!("master-{}", project.as_str()),
    )
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
/// gained and polls every signed-in subscription account's limits. `doing` names the part
/// of the step running now, so a panic is counted against it, and `unrecovered` holds the
/// parts that panicked and have not completed since.
pub struct Usage {
    db: Connection,
    team: PathBuf,
    custody: Result<NativeProfileCustody, EngineError>,
    access: AccessSource,
    executables: BTreeMap<&'static str, PathBuf>,
    endpoints: (url::Url, url::Url, bool),
    polled_ms: Option<i64>,
    polls: u64,
    doing: UsageStep,
    unrecovered: BTreeSet<UsageStep>,
}

impl Usage {
    /// Opens the upkeep for an engine database and forgets the step panics a previous engine
    /// run counted. Inputs: the database, the team state folder, the live access and the
    /// harness executables by harness id. Output: the upkeep.
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
        clear(&db)?;
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
            doing: UsageStep::Poll,
            unrecovered: BTreeSet::new(),
        })
    }

    /// Points the polls at other usage calls, such as a loopback server in a test.
    /// Inputs: the Claude and Codex URLs. Output: the upkeep.
    pub fn with_endpoints(mut self, claude: url::Url, codex: url::Url) -> Self {
        self.endpoints = (claude, codex, true);
        self
    }

    /// Runs one upkeep step at `now_ms`: polls every account when ten minutes passed
    /// since the last poll or the owner asked to refresh since, then counts every live or
    /// lately stopped agent's and recent research run's records, so a run's own limit
    /// readings meet a poll as fresh as the step allows. Output: done, or the store's
    /// failure.
    pub fn step(&mut self, now_ms: i64) -> Result<(), EngineError> {
        self.doing = UsageStep::Poll;
        let access = (self.access)().unwrap_or_default();
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
            self.completed(UsageStep::Poll, now_ms)?;
        }
        self.doing = UsageStep::Team;
        self.scan_team(&access, now_ms)?;
        self.completed(UsageStep::Team, now_ms)?;
        self.doing = UsageStep::Runs;
        scan_runs(
            &self.db,
            |project, account| pool_of(&access, project, account),
            now_ms,
        )?;
        self.completed(UsageStep::Runs, now_ms)?;
        self.guarded_recount(|usage| usage.recount(&access, now_ms), now_ms)
    }

    /// Runs the recount's part of a step. A panic is counted against the recount and returns
    /// done, so that the live scan's pace does not double for it; the file being read has
    /// already been charged a try. Inputs: the part and the time. Output: done, or the store's
    /// failure.
    fn guarded_recount(
        &mut self,
        part: impl FnOnce(&Usage) -> Result<(), EngineError>,
        now_ms: i64,
    ) -> Result<(), EngineError> {
        self.doing = UsageStep::Recount;
        let usage = &*self;
        match guarded(|| part(usage)) {
            Some(code) if code == STEP_PANICKED => {
                self.unrecovered.insert(UsageStep::Recount);
                record_panic(&self.db, UsageStep::Recount, now_ms)
            }
            Some(code) => Err(EngineError(code)),
            None => self.completed(UsageStep::Recount, now_ms),
        }
    }

    /// Starts the once-only recount, then reads one bounded part of it each step. Inputs: the
    /// live access and the time. Output: done, or the store's failure.
    fn recount(&self, access: &ModelAccess, now_ms: i64) -> Result<(), EngineError> {
        recount::forget_copy(&self.db, now_ms)?;
        if !recount::started(&self.db)? {
            recount::begin(&self.db, &self.recount_homes(access)?, now_ms)?;
        }
        recount::step(&self.db, recount::BYTES_PER_STEP, now_ms)?;
        Ok(())
    }

    /// Lists the homes the upkeep already scans. A store without team tables has only its
    /// runs; any other failure returns, so the once-only recount starts on a later step.
    /// Input: the live access. Output: the homes, or the store's failure.
    fn recount_homes(&self, access: &ModelAccess) -> Result<Vec<recount::Home>, EngineError> {
        let mut homes = Vec::new();
        let has_team: bool = self
            .db
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name='team_agents')",
                [],
                |row| row.get(0),
            )
            .map_err(storage)?;
        if has_team {
            let mut statement = self
                .db
                .prepare(
                    "SELECT id,project,role,harness,connection FROM team_agents
                     WHERE role<>'teammate' ORDER BY id DESC LIMIT ?1",
                )
                .map_err(storage)?;
            let rows: Vec<(i64, String, String, String, Option<String>)> = statement
                .query_map([RECOUNT_AGENTS_MAX], |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                })
                .map_err(storage)?
                .collect::<Result<_, _>>()
                .map_err(storage)?;
            for (id, project, role, harness, connection) in rows {
                let Some(format) = Format::of(&harness) else {
                    continue;
                };
                homes.push(recount::Home {
                    path: self
                        .team
                        .join("agents")
                        .join(format!("agent-{id}"))
                        .join("home"),
                    format,
                    attribution: team_attribution(
                        id,
                        &project,
                        role == "master",
                        &harness,
                        connection.as_deref(),
                        access,
                    ),
                    removed: false,
                });
            }
        }
        let mut runs = self
            .db
            .prepare("SELECT home,kind,project,account,agent,harness FROM usage_runs ORDER BY started_ms DESC LIMIT ?1")
            .map_err(storage)?;
        let rows: Vec<(String, String, String, String, String, String)> = runs
            .query_map([RECOUNT_RUNS_MAX], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            })
            .map_err(storage)?
            .collect::<Result<_, _>>()
            .map_err(storage)?;
        for (home, kind, project, account, agent, harness) in rows {
            let resolve =
                |project: &str, account: &CredentialRef| pool_of(access, project, account);
            let removed = project == super::REMOVED_PROJECTS;
            if let Some((format, attribution)) =
                run_attribution(&kind, project, account, agent, harness, &resolve)
            {
                homes.push(recount::Home {
                    path: PathBuf::from(home),
                    format,
                    attribution,
                    removed,
                });
            }
        }
        Ok(homes)
    }

    /// Marks a part that panicked earlier as working again once it completes, writing only
    /// on that change. Inputs: the part and the time. Output: done, or `usage_unavailable`.
    fn completed(&mut self, part: UsageStep, now_ms: i64) -> Result<(), EngineError> {
        if self.unrecovered.contains(&part) {
            record_recovered(&self.db, part, now_ms)?;
            self.unrecovered.remove(&part);
        }
        Ok(())
    }

    fn scan_team(&self, access: &ModelAccess, now_ms: i64) -> Result<(), EngineError> {
        type Row = (
            i64,
            String,
            String,
            String,
            Option<String>,
            bool,
            Option<i64>,
        );
        let rows: Vec<Row> = {
            let mut statement = match self.db.prepare(
                "SELECT id,project,role,harness,connection,
                    status IN ('starting','idle','working'),stopped_ms FROM team_agents
                 WHERE role<>'teammate' AND (status IN ('starting','idle','working') OR stopped_ms>=?1)
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
                            row.get(5)?,
                            row.get(6)?,
                        ))
                    },
                )
                .map_err(storage)?
                .collect::<Result<_, _>>()
                .map_err(storage)?
        };
        for (id, project, role, harness, connection, live, stopped_ms) in rows {
            let attribution = team_attribution(
                id,
                &project,
                role == "master",
                &harness,
                connection.as_deref(),
                access,
            );
            let Some(format) = Format::of(&harness) else {
                if !reports_usage(&harness) {
                    let ran_ms = match (live, stopped_ms) {
                        (false, Some(stopped)) => stopped.min(now_ms),
                        _ => now_ms,
                    };
                    mark_unreported(&self.db, &attribution, ran_ms)?;
                }
                continue;
            };
            let home = self
                .team
                .join("agents")
                .join(format!("agent-{id}"))
                .join("home");
            scan_agent_home(&self.db, &home, format, &attribution, id, now_ms)?;
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

/// Runs one upkeep step so that a panic inside it ends only that step: its transaction
/// rolls back as it unwinds and the thread steps again at the next scan. Input: the step.
/// Output: its problem code, `usage_step_panicked` for a panic, none when it succeeded.
fn guarded(step: impl FnOnce() -> Result<(), EngineError>) -> Option<String> {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(step)) {
        Ok(Ok(())) => None,
        Ok(Err(problem)) => Some(problem.0),
        Err(_) => Some(STEP_PANICKED.to_owned()),
    }
}

/// Runs one upkeep step under `guarded` and counts a panic against the step kind that was
/// running, which stays unrecovered until that kind completes again, so the usage pages can
/// say counting is degraded. Inputs: the upkeep, the step and the time. Output: the step's
/// problem code, none when it succeeded.
fn counted_step(
    usage: &mut Usage,
    step: impl FnOnce(&mut Usage) -> Result<(), EngineError>,
    now_ms: i64,
) -> Option<String> {
    let problem = guarded(|| step(usage));
    if problem.as_deref() == Some(STEP_PANICKED) {
        usage.unrecovered.insert(usage.doing);
        if let Err(fault) = record_panic(&usage.db, usage.doing, now_ms) {
            eprintln!("usage: {}", fault.0);
        }
    }
    problem
}

/// The upkeep thread's log and pace. A problem code is logged when it first appears. A
/// step that panics again right after a panic is counted, not logged, and the wait before
/// the next step doubles up to `PANIC_PAUSE_MAX`. The default panic hook still reports
/// every panic on its own, so the longer wait is what bounds that report.
#[derive(Debug, Default)]
struct Pace {
    last: Option<String>,
    panics: u32,
}

impl Pace {
    /// Takes one step's outcome. Input: its problem code, none when it succeeded.
    /// Output: the lines to log, at most two, and the wait before the next step.
    fn after(&mut self, problem: Option<String>) -> (Vec<String>, Duration) {
        let panicked = problem.as_deref() == Some(STEP_PANICKED);
        let mut lines = Vec::new();
        if !panicked && self.panics > 1 {
            lines.push(format!(
                "usage: {} steps in a row panicked before this one",
                self.panics
            ));
        }
        self.panics = if panicked {
            self.panics.saturating_add(1)
        } else {
            0
        };
        if problem.is_some() && problem != self.last {
            lines.push(format!("usage: {}", problem.as_deref().unwrap_or_default()));
        }
        self.last = problem;
        let pause = SCAN_EVERY
            .saturating_mul(1 << self.panics.min(PANIC_DOUBLINGS_MAX))
            .min(PANIC_PAUSE_MAX);
        assert!(lines.len() <= 2);
        assert!((SCAN_EVERY..=PANIC_PAUSE_MAX).contains(&pause));
        (lines, pause)
    }
}

/// Starts the usage upkeep for a running engine: it steps every `SCAN_EVERY`, and less
/// often while its steps keep panicking, reading the pools live from the engine's settings
/// files. Inputs: the runtime folder, both settings files and the harness executables by
/// harness id. Output: the service, or a reason.
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
            let mut pace = Pace::default();
            while !flag.load(Ordering::SeqCst) {
                let now = now_ms();
                let (lines, pause) =
                    pace.after(counted_step(&mut usage, |usage| usage.step(now), now));
                for line in lines {
                    eprintln!("{line}");
                }
                let mut waited = Duration::ZERO;
                while waited < pause && !flag.load(Ordering::SeqCst) {
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

#[cfg(all(test, unix))]
mod tests;
