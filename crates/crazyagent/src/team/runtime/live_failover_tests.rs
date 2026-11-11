use super::{TeamLimits, TeamRuntime};
use crate::config::{AccountPool, ModelAccess, ModelConnection};
use crate::engine::NativeProfileCustody;
use crate::engine::owner::HostStamp;
use crate::engine::owner::native_auth::NativeAuthFact;
use crate::project_setup::{HarnessSlot, StoredSetup, TeamPlan};
use crate::protocol::{CredentialRef, ProjectId};
use crate::team::{MessageAuthor, MessageState, append_message, now_ms};
use crate::terminal::tmux::{SOCKET, Tmux, installed};
use crate::terminal::{FOLDER, Host};
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

const PROJECT: &str = "live-failover";
const SIGNED_IN: &str = "CRAZYAGENT_MANAGED_CLAUDE_PROFILE";
const SIGNED_IN_CODEX: &str = "CRAZYAGENT_MANAGED_CODEX_PROFILE";
pub(crate) const START: Duration = Duration::from_secs(120);
pub(crate) const TURN: Duration = Duration::from_secs(300);
pub(crate) const ASK: &str = "Reply with only the word READY.";

/// One latest sign-in row of `native_auth_operations` as the owner's engine stores it: id,
/// profile key, generation, phase, request and fact.
type Operation = (String, String, i64, String, Vec<u8>, Vec<u8>);

/// One of the owner's signed-in Claude or Codex accounts as the owner's engine records it:
/// the connection its proven sign-in names, that sign-in's row and fact, and whether the
/// engine's usage reading has the account at its 5-hour or weekly limit now.
struct Account {
    connection: ModelConnection,
    operation: Operation,
    fact: NativeAuthFact,
    limited: bool,
}

/// The most profile keys the live tests read from the owner's engine.
const PROFILES_MAX: usize = 256;

/// Stops a live run whose profile variable is not a profile slot path.
/// Input: the variable. Output: never returns; panics with the plain BLOCKED message.
fn not_a_slot(variable: &str) -> ! {
    panic!(
        "BLOCKED: {variable} does not name a profile slot <engine folder>/native-profiles/<profile key>"
    )
}

/// The running engine of the app that holds the managed profile `variable` names, found
/// from that path alone: the slot's profile key (its folder name), the database beside its
/// profiles folder, a read-only connection to that database and the run it is in. The
/// slot, its profiles folder and their files are never opened, read or listed. Input: the
/// variable. Output: the four, or a panic with the plain BLOCKED message.
fn owner_engine(variable: &str) -> (String, PathBuf, Connection, HostStamp) {
    let slot = PathBuf::from(std::env::var_os(variable).unwrap_or_else(|| {
        panic!(
            "BLOCKED: no signed-in managed profile. Run with {variable}=<engine folder>/native-profiles/<profile key>"
        )
    }));
    let key = slot
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|key| {
            key.len() == 64
                && key
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
        .unwrap_or_else(|| not_a_slot(variable))
        .to_owned();
    let profiles = slot
        .parent()
        .filter(|profiles| {
            profiles.is_absolute()
                && profiles.file_name() == Some(std::ffi::OsStr::new("native-profiles"))
        })
        .unwrap_or_else(|| not_a_slot(variable));
    let database = profiles
        .parent()
        .unwrap_or_else(|| not_a_slot(variable))
        .join("engine.db");
    let path = database.to_str().unwrap();
    let uri = format!(
        "file:{}?mode=ro",
        path.replace('%', "%25")
            .replace(' ', "%20")
            .replace('?', "%3f")
            .replace('#', "%23")
    );
    let owner = Connection::open_with_flags(
        &uri,
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_URI
            | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .unwrap_or_else(|_| panic!("BLOCKED: the owner's engine database cannot be read"));
    owner.busy_timeout(Duration::from_secs(5)).unwrap();
    let host = owner
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
        .unwrap()
        .unwrap_or_else(|| {
            panic!("BLOCKED: the app's engine is not running, so no sign-in is proved for this run. Open CrazyAgent first")
        });
    (key, database, owner, host)
}

/// The owner's account behind one profile key, from the read-only engine database alone:
/// the key's latest sign-in row, when its fact names a managed connection of `harness` and
/// passes this run's launch check, as every master turn checks it, with whether the engine
/// reads the account at a limit: a 5-hour or weekly window, observed or polled, at 100
/// percent used now. Inputs: the read-only engine, its run, the key and the connection
/// harness, `claude` or `codex`. Output: the account, or None.
fn signed_in_account(
    owner: &Connection,
    host: &HostStamp,
    key: &str,
    harness: &str,
) -> Option<Account> {
    let operation: Operation = owner
        .query_row(
            "SELECT id,profile_key,generation,phase,request,fact FROM native_auth_operations WHERE profile_key=?1 ORDER BY generation DESC LIMIT 1",
            [key],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?)),
        )
        .optional()
        .unwrap()?;
    let fact = crate::engine::owner::native_auth::decode_fact(&operation.5).unwrap();
    let connection = fact.binding.connection.clone();
    let crate::config::ModelAuthentication::NativeManaged { profile_ref } = &connection.auth else {
        return None;
    };
    assert_eq!(
        crate::memory::hash(profile_ref.as_str().as_bytes()),
        key,
        "a profile's sign-in names that profile"
    );
    if connection.harness.as_str() != harness {
        return None;
    }
    let tx = owner.unchecked_transaction().unwrap();
    let proved = crate::storage::native_sign_in(&tx, host, &connection, None).is_ok();
    drop(tx);
    if !proved {
        return None;
    }
    let now = now_ms();
    let limited = crate::usage::limits(owner, &connection.id)
        .unwrap()
        .is_some_and(|limits| {
            [
                limits.five_hour_at(now),
                limits.weekly_at(now),
                limits.polled_five_hour_at(now),
                limits.polled_weekly_at(now),
            ]
            .into_iter()
            .flatten()
            .any(|window| window.used_percent >= 100.0)
        });
    Some(Account {
        connection,
        operation,
        fact,
        limited,
    })
}

/// Every Claude account of the owner's engine whose latest sign-in passes this run's launch
/// check, from that engine's database alone: one per distinct profile key of its sign-in
/// rows, at most [`PROFILES_MAX`]. Inputs: the read-only engine and its run. Output: the
/// accounts in profile key order.
fn signed_in_accounts(owner: &Connection, host: &HostStamp) -> Vec<Account> {
    let keys: Vec<String> = owner
        .prepare(
            "SELECT DISTINCT profile_key FROM native_auth_operations ORDER BY profile_key LIMIT ?1",
        )
        .unwrap()
        .query_map([i64::try_from(PROFILES_MAX).unwrap()], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert!(keys.len() <= PROFILES_MAX);
    keys.iter()
        .filter_map(|key| signed_in_account(owner, host, key, "claude"))
        .collect()
}

/// The team launch harness and its real executable for an account's connection harness.
/// Input: `claude` or `codex`. Output: `claude-code` or `codex` with its executable.
fn launch(harness: &str) -> (&'static str, PathBuf) {
    match harness {
        "claude" => ("claude-code", super::managed_agent::claude()),
        "codex" => ("codex", super::managed_agent::codex()),
        other => unreachable!("the live tests pick only Claude and Codex accounts, not {other}"),
    }
}

/// Records the owner's engine run and the accounts' proven sign-in rows in a database a
/// test engine left, and raises its revision to theirs, so its runtimes launch on those
/// accounts as the owner's engine run would. Inputs: the database, the owner's run and the
/// accounts. Output: none; it panics unless every row is written.
fn record_run(db: &Connection, host: &HostStamp, accounts: &[&Account]) {
    db.execute(
        "INSERT OR REPLACE INTO owner_meta(singleton,schema_version,db_instance,engine_epoch,runtime_state) VALUES (1,1,?1,?2,'active')",
        params![host.db_instance, host.engine_epoch],
    )
    .unwrap();
    for account in accounts {
        let (id, key, generation, phase, request, fact) = &account.operation;
        db.execute(
            "INSERT INTO native_auth_operations(id,profile_key,generation,phase,request,fact) VALUES (?1,?2,?3,?4,?5,?6)",
            params![id, key, generation, phase, request, fact],
        )
        .unwrap();
    }
    let revision = accounts
        .iter()
        .map(|account| account.fact.binding.at_revision.value())
        .max()
        .expect("at least one signed-in account");
    assert_eq!(
        db.execute(
            "UPDATE state SET revision=MAX(revision,?1) WHERE id=1",
            [i64::try_from(revision).unwrap()],
        )
        .unwrap(),
        1,
        "the engine keeps one revision row"
    );
}

/// The owner's signed-in Claude or Codex account of the profile its variable names, which
/// the running app's engine does not read at a limit, with that engine's database and the
/// run the account's sign-in is proven in.
pub(crate) struct OwnerAccount {
    database: PathBuf,
    host: HostStamp,
    account: Account,
}

impl OwnerAccount {
    /// The Claude account of the profile `CRAZYAGENT_MANAGED_CLAUDE_PROFILE` names.
    /// Input: none. Output: the account, or a panic with the plain BLOCKED message.
    pub(crate) fn claude() -> Self {
        Self::pick(SIGNED_IN, "claude", "Claude")
    }

    /// The ChatGPT account for Codex of the profile `CRAZYAGENT_MANAGED_CODEX_PROFILE` names.
    /// Input: none. Output: the account, or a panic with the plain BLOCKED message.
    pub(crate) fn codex() -> Self {
        Self::pick(SIGNED_IN_CODEX, "codex", "Codex")
    }

    /// Takes the account of the profile `variable` names from the running app's engine
    /// database alone. Inputs: the variable, the connection harness and its plain name.
    /// Output: the account, or a panic with the plain BLOCKED message when its sign-in is
    /// not proven for this run or the engine reads it at a limit.
    fn pick(variable: &str, harness: &str, name: &str) -> Self {
        let (key, database, owner, host) = owner_engine(variable);
        let account = signed_in_account(&owner, &host, &key, harness).unwrap_or_else(|| {
            panic!(
                "BLOCKED: the profile {variable} names has no {name} sign-in the app's engine proves for this run"
            )
        });
        drop(owner);
        if account.limited {
            panic!(
                "BLOCKED: the {name} account {variable} names is at its 5-hour or weekly limit in the engine's usage reading"
            );
        }
        assert_eq!(account.connection.harness.as_str(), harness);
        Self {
            database,
            host,
            account,
        }
    }

    /// The account's connection as the owner's settings name it. Output: the connection.
    pub(crate) fn connection(&self) -> &ModelConnection {
        &self.account.connection
    }

    /// The team launch harness of the account and its real executable. Output: both.
    pub(crate) fn launch(&self) -> (&'static str, PathBuf) {
        launch(self.account.connection.harness.as_str())
    }

    /// Records the owner's engine run and this account's proven sign-in in a database a test
    /// engine left, as [`Team::on_engine`] does. Input: the database. Output: none.
    pub(crate) fn record_run(&self, db: &Connection) {
        record_run(db, &self.host, &[&self.account]);
    }

    /// The owner's engine's custody, which only reads its database and takes a shared lock
    /// on a profile for each launch. Output: the custody.
    pub(crate) fn custody(&self) -> NativeProfileCustody {
        NativeProfileCustody::capture(&self.database, &std::fs::metadata(&self.database).unwrap())
            .unwrap()
    }
}

/// The test's own team engine with one project whose live master is Claude Code, on a pool
/// of the owner's signed-in Claude accounts. Its database carries the owner's engine run and
/// those accounts' proven sign-in rows as they stand, and its custody is the owner's
/// engine's own, which only reads that database and takes a shared lock on a profile for
/// each launch, as every engine run does. The failover test builds it in a new folder; the
/// LMS-1 test opens it on the database a real engine left.
pub(crate) struct Team {
    runtime: TeamRuntime,
    db: Connection,
    project: ProjectId,
    host: Arc<dyn Host>,
    tmux: PathBuf,
    base: PathBuf,
    limited: Option<CredentialRef>,
    working: CredentialRef,
    _root: Option<tempfile::TempDir>,
}

impl Team {
    /// Prepares the engine in a new folder whose pool holds the limited account first, then
    /// the working one. Inputs: the owner's run, the owner's database and the two accounts.
    /// Output: the team, its live switch still off.
    fn new(host: &HostStamp, owner_database: &Path, limited: &Account, working: &Account) -> Self {
        assert!(limited.limited && !working.limited);
        assert_ne!(limited.connection.id, working.connection.id);
        let root = tempfile::Builder::new()
            .prefix("ca-live-failover-")
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let base = root.path().canonicalize().unwrap();
        let db = Connection::open(base.join("engine.db")).unwrap();
        crate::storage::initialize_authority_schemas(&db).unwrap();
        let owner = db.unchecked_transaction().unwrap();
        crate::conversation::initialize(&owner).unwrap();
        owner.commit().unwrap();
        super::super::initialize(&db).unwrap();
        crate::project_setup::initialize(&db).unwrap();
        std::fs::set_permissions(
            base.join("engine.db"),
            std::fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        let project = ProjectId::new(PROJECT).unwrap();
        let folder = base.join("project");
        std::fs::create_dir(&folder).unwrap();
        let setup = StoredSetup {
            project_id: project.clone(),
            name: "Live failover".into(),
            about: String::new(),
            tech_stack: vec![],
            platforms: vec![],
            research: vec![],
            pool: None,
            team: TeamPlan {
                master: "claude-code".into(),
                master_tools: true,
                sub_harnesses: vec![HarnessSlot {
                    harness: "claude-code".into(),
                    agents: 1,
                    tools: true,
                }],
                min_agents: 1,
                max_agents: 2,
            },
            documents: vec![],
            folder: folder.to_str().unwrap().into(),
            created_ms: 1,
            revision: 1,
        };
        let row = crate::protocol::StoredProject {
            project_id: project.clone(),
            spec: crate::protocol::ProjectSpec {
                name: setup.name.clone(),
                repositories: vec![],
            },
        };
        db.execute(
            "INSERT INTO projects VALUES (?1,?2)",
            params![PROJECT, serde_json::to_string(&row).unwrap()],
        )
        .unwrap();
        db.execute(
            "INSERT INTO project_setups VALUES (?1,?2)",
            params![PROJECT, serde_json::to_string(&setup).unwrap()],
        )
        .unwrap();
        let access = ModelAccess {
            accounts: vec![
                limited.connection.account_ref.clone(),
                working.connection.account_ref.clone(),
            ],
            connections: vec![limited.connection.clone(), working.connection.clone()],
            pools: vec![AccountPool {
                id: CredentialRef::new("live-failover-pool").unwrap(),
                name: None,
                members: vec![limited.connection.id.clone(), working.connection.id.clone()],
                projects: vec![CredentialRef::new(PROJECT).unwrap()],
                roles: vec![CredentialRef::new("project_leader").unwrap()],
                fallback_allowed: false,
                provider_quota_policy: Default::default(),
            }],
        };
        let mut team = Self::open(
            base,
            db,
            project,
            host,
            owner_database,
            &[limited, working],
            Box::new(move || Some(access.clone())),
        );
        team.limited = Some(limited.connection.id.clone());
        team._root = Some(root);
        team
    }

    /// Opens the team runtime on the database a real engine left in `base` after it
    /// stopped, with the owner's signed-in account and the pools its settings files hold,
    /// read on every tick as the engine's own team runtime reads them. Inputs: the engine
    /// folder, its global and project settings files, the project and the owner's account.
    /// Output: the team, its live switch as the engine left it.
    pub(crate) fn on_engine(
        base: &Path,
        global: &Path,
        settings: &Path,
        project: &ProjectId,
        owner: &OwnerAccount,
    ) -> Self {
        let base = base.canonicalize().unwrap();
        let db = Connection::open(base.join("engine.db")).unwrap();
        db.busy_timeout(Duration::from_secs(5)).unwrap();
        let (global, settings) = (global.to_path_buf(), settings.to_path_buf());
        Self::open(
            base,
            db,
            project.clone(),
            &owner.host,
            &owner.database,
            &[&owner.account],
            Box::new(move || {
                crate::config::resolve_config(&global, &settings)
                    .ok()?
                    .settings
                    .model_access
            }),
        )
    }

    /// Records the owner's engine run and the accounts' proven sign-in rows in the prepared
    /// database, raises its revision to theirs and opens the runtime on the owner's engine's
    /// custody with tmux and each account's launch harness. Inputs: the folder, its database,
    /// the project, the owner's run and database, the accounts, the last of which is the
    /// working one, and the pool source. Output: the team.
    fn open(
        base: PathBuf,
        db: Connection,
        project: ProjectId,
        host: &HostStamp,
        owner_database: &Path,
        accounts: &[&Account],
        access: super::AccessSource,
    ) -> Self {
        let tmux = installed(&base).expect("BLOCKED: tmux must be installed");
        record_run(&db, host, accounts);
        let executables: BTreeMap<&'static str, PathBuf> = accounts
            .iter()
            .map(|account| launch(account.connection.harness.as_str()))
            .collect();
        assert!(!executables.is_empty());
        let host_terminal: Arc<dyn Host> =
            Arc::new(Tmux::open(Some(tmux.clone()), &base.join(FOLDER)));
        let mut runtime = TeamRuntime::open(
            &base.join("engine.db"),
            &base.join("team"),
            executables,
            access,
            TeamLimits::default(),
        )
        .unwrap()
        .with_terminal(host_terminal.clone())
        .unwrap();
        runtime.custody = Ok(NativeProfileCustody::capture(
            owner_database,
            &std::fs::metadata(owner_database).unwrap(),
        )
        .unwrap());
        let working = accounts.last().expect("a working account");
        Self {
            runtime,
            db,
            project,
            host: host_terminal,
            tmux,
            base,
            limited: None,
            working: working.connection.id.clone(),
            _root: None,
        }
    }

    /// Every master row of the project with its account. Output: (id, connection) by id.
    fn masters(&self) -> Vec<(i64, Option<String>)> {
        self.db
            .prepare("SELECT id,connection FROM team_agents WHERE project=?1 AND role='master' ORDER BY id")
            .unwrap()
            .query_map([self.project.as_str()], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }

    /// The agent the project's live master record runs, if any.
    pub(crate) fn live_agent(&self) -> Option<i64> {
        self.db
            .query_row(
                "SELECT agent FROM team_live_masters WHERE project=?1",
                [self.project.as_str()],
                |row| row.get(0),
            )
            .optional()
            .unwrap()
            .flatten()
    }

    /// Every ended master turn kept for the project. Output: (turn, outcome) by turn.
    pub(crate) fn turns(&self) -> Vec<(i64, String)> {
        self.db
            .prepare("SELECT turn,outcome FROM team_master_turns WHERE project=?1 ORDER BY turn")
            .unwrap()
            .query_map([self.project.as_str()], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }

    /// The accounts resting after a rate limit, named limited, working or other.
    fn rested(&self) -> Vec<&'static str> {
        self.db
            .prepare("SELECT connection FROM team_paused_accounts WHERE until_ms > ?1 ORDER BY connection")
            .unwrap()
            .query_map([now_ms()], |row| row.get::<_, String>(0))
            .unwrap()
            .map(|connection| self.name(Some(connection.unwrap().as_str())))
            .collect()
    }

    /// Which of the accounts a connection is, so the record never prints an account id.
    fn name(&self, connection: Option<&str>) -> &'static str {
        match connection {
            Some(id)
                if self
                    .limited
                    .as_ref()
                    .is_some_and(|limited| id == limited.as_str()) =>
            {
                "limited"
            }
            Some(id) if id == self.working.as_str() => "working",
            Some(_) => "other",
            None => "none",
        }
    }

    /// The one master and the account it runs on, named, once exactly one master row exists.
    pub(crate) fn master(&self) -> Option<(i64, &'static str)> {
        match self.masters().as_slice() {
            [(id, connection)] => Some((*id, self.name(connection.as_deref()))),
            _ => None,
        }
    }

    /// What the engine records for the project, for a failure message: the masters with
    /// their account, status and error, the ended turns, the resting accounts, the switch
    /// and the live master's input facts.
    pub(crate) fn seen(&self) -> String {
        let agents: Vec<(i64, Option<String>, String, Option<String>)> = self
            .db
            .prepare("SELECT id,connection,status,error FROM team_agents WHERE project=?1 AND role='master' ORDER BY id")
            .unwrap()
            .query_map([self.project.as_str()], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        let agents: Vec<String> = agents
            .iter()
            .map(|(id, connection, status, error)| {
                format!(
                    "{id} on {} {status} {error:?}",
                    self.name(connection.as_deref())
                )
            })
            .collect();
        format!(
            "masters {agents:?}; turns {:?}; resting {:?}; live {}; {}",
            self.turns(),
            self.rested(),
            super::super::master::live(&self.db, &self.project).unwrap(),
            input_evidence(&self.facts())
        )
    }

    /// The live master's input facts as the engine holds them now (LMS-1): the project's
    /// messages, its live master row, the engine's input check, the newest master worker's
    /// briefing and last profile wait, and the last rows of its pane read through tmux.
    /// Output: the facts; no master worker leaves the briefing, wait and screen empty.
    pub(crate) fn facts(&self) -> InputFacts {
        let messages = self
            .db
            .prepare("SELECT author,state FROM team_messages WHERE project=?1 ORDER BY id")
            .unwrap()
            .query_map([self.project.as_str()], |row| {
                Ok(format!(
                    "{} {}",
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?
                ))
            })
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        let live = self
            .db
            .query_row(
                "SELECT live,agent,turn_open,input_ms,typed_ms,draft FROM team_live_masters WHERE project=?1",
                [self.project.as_str()],
                |row| {
                    Ok(format!(
                        "live {} agent {:?} turn_open {} input_ms {:?} typed_ms {:?} draft {}",
                        row.get::<_, bool>(0)?,
                        row.get::<_, Option<i64>>(1)?,
                        row.get::<_, bool>(2)?,
                        row.get::<_, Option<i64>>(3)?,
                        row.get::<_, Option<i64>>(4)?,
                        row.get::<_, bool>(5)?
                    ))
                },
            )
            .optional()
            .unwrap();
        let input = format!(
            "{:?}",
            super::super::master::input_open(&self.db, None, &self.project, now_ms())
        );
        let master = self.runtime.workers.iter().rev().find(|(_, worker)| {
            worker.project == self.project && worker.role == crate::team::AgentRole::Master
        });
        let screen = master
            .and_then(|(id, _)| crate::terminal::AgentId::team(*id).ok())
            .and_then(|agent| self.host.screen(&agent).ok())
            .map(|bytes| pane_rows(&bytes))
            .unwrap_or_default();
        InputFacts {
            messages,
            live,
            input,
            briefed: master.map(|(_, worker)| worker.briefed),
            waited: master.and_then(|(id, _)| self.runtime.grant_waited.get(id).cloned()),
            screen,
        }
    }

    /// Types one owner message to the master the way the owner's app sends it.
    pub(crate) fn say(&self, text: &str) {
        let tx = self.db.unchecked_transaction().unwrap();
        append_message(
            &tx,
            &self.project,
            MessageAuthor::Owner,
            text,
            MessageState::Pending,
            now_ms(),
        )
        .unwrap()
        .unwrap();
        tx.commit().unwrap();
    }

    /// Ticks the runtime until a condition holds. Inputs: what is awaited, the time limit and
    /// the condition, which may panic with a BLOCKED message. Panics with what the engine
    /// records when the time runs out, as BLOCKED only when `profile_held` proves it.
    pub(crate) fn until(&mut self, what: &str, limit: Duration, done: impl Fn(&Self) -> bool) {
        let deadline = Instant::now() + limit;
        while !done(self) {
            if Instant::now() >= deadline {
                let blocked = if profile_held(&self.facts()) {
                    "BLOCKED: another process held the owner's profile: "
                } else {
                    ""
                };
                panic!(
                    "{blocked}{what} within {} s: {}",
                    limit.as_secs(),
                    self.seen()
                );
            }
            self.tick();
        }
    }

    /// Runs one runtime tick, then waits a moment, as the engine's team service does.
    pub(crate) fn tick(&mut self) {
        self.runtime.tick().unwrap();
        std::thread::sleep(Duration::from_millis(200));
    }

    /// The project's task with exactly this prompt, as the engine's team status lists it.
    /// Input: the prompt. Output: the task, or None while no task has it.
    pub(crate) fn task(&self, prompt: &str) -> Option<crate::team::TeamTask> {
        let tx = self.db.unchecked_transaction().unwrap();
        let stored = crate::project_setup::load_setup(&tx, &self.project)
            .unwrap()
            .expect("the team's project is set up");
        drop(tx);
        crate::team::status(&self.db, &stored)
            .unwrap()
            .tasks
            .into_iter()
            .find(|task| task.prompt == prompt)
    }

    /// Whether the project's live switch is on in the database. Output: the switch.
    pub(crate) fn live(&self) -> bool {
        super::super::master::live(&self.db, &self.project).unwrap()
    }

    /// The master session the engine holds open for the project's live master, if any.
    pub(crate) fn engine_session(&self) -> Option<String> {
        crate::master_sessions::live_session(&self.db, &self.project).unwrap()
    }

    /// The master session one running agent's live terminal turns are counted in. Input:
    /// the agent. Output: the session, or None for an agent the runtime does not run live.
    pub(crate) fn live_session(&self, agent: i64) -> Option<String> {
        self.runtime
            .workers
            .get(&agent)
            .filter(|worker| worker.live)
            .and_then(|worker| worker.session.clone())
    }

    /// What the message store holds of one master session's live terminal: its turns and
    /// its newest reply. Input: the session. Output: both, or None before any turn.
    pub(crate) fn session_turns(&self, session: &str) -> Option<(i64, Option<String>)> {
        self.db
            .query_row(
                "SELECT COUNT(*),(SELECT r.text FROM session_messages r
                                  WHERE r.session=?1 AND r.project=?2 AND r.speaker='master' AND r.mode='live'
                                  ORDER BY r.seq DESC LIMIT 1)
                 FROM session_messages t
                 WHERE t.session=?1 AND t.project=?2 AND t.speaker IN ('owner','engine') AND t.mode='live'",
                params![session, self.project.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .unwrap()
            .filter(|(turns, _): &(i64, Option<String>)| *turns > 0)
    }
}

impl Drop for Team {
    fn drop(&mut self) {
        let _ = self.runtime.shutdown();
        let _ = self.host.close(&[]);
        let _ = crate::spawn_gate::status(
            Command::new(&self.tmux)
                .arg("-S")
                .arg(self.base.join(FOLDER).join(SOCKET))
                .arg("kill-server")
                .env_clear()
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null()),
        );
    }
}

/// How many of the master pane's last non-empty rows a failure message shows.
const SCREEN_ROWS: usize = 8;

/// What a live master's input shows beside `Team::seen` (LMS-1): each team message's author
/// and state, the live master row, the engine's input check, whether the master was briefed,
/// the custody answer of the runtime's last profile wait and the master screen's last rows.
#[derive(Debug, Clone, Default)]
pub(crate) struct InputFacts {
    messages: Vec<String>,
    live: Option<String>,
    input: String,
    briefed: Option<bool>,
    waited: Option<String>,
    screen: Vec<String>,
}

/// The master pane's last `SCREEN_ROWS` non-empty rows for a failure message, each through
/// the engine's credential redaction, since a pane can show a sign-in code or a token.
/// Input: the pane's screen bytes. Output: the rows, oldest first.
pub(crate) fn pane_rows(screen: &[u8]) -> Vec<String> {
    let rows: Vec<String> = crate::team::events::screen_rows(screen)
        .into_iter()
        .map(|row| crate::project_setup::redact(row.trim()))
        .filter(|row| !row.is_empty())
        .collect();
    rows[rows.len().saturating_sub(SCREEN_ROWS)..].to_vec()
}

/// Words the input facts for a failure message, so a turn that never came reads as queued,
/// refused or typed. Input: the facts. Output: one line.
pub(crate) fn input_evidence(facts: &InputFacts) -> String {
    format!(
        "messages {:?}; live row {:?}; input check {}; briefed {:?}; profile wait {:?}; screen {:?}",
        facts.messages, facts.live, facts.input, facts.briefed, facts.waited, facts.screen
    )
}

/// Whether a turn wait timed out because another process held the owner's profile: an
/// owner message still pending, a master never briefed and the runtime's last profile wait
/// answered `native_profile_in_use`. Input: the facts. Output: the answer.
pub(crate) fn profile_held(facts: &InputFacts) -> bool {
    facts.briefed == Some(false)
        && facts.waited.as_deref() == Some("native_profile_in_use")
        && facts
            .messages
            .iter()
            .any(|message| message == "owner pending")
}

/// LMS-1 evidence: a turn wait's failure names every input fact, so the three silent waits
/// (never briefed, input refused, typed with no turn) read apart.
#[test]
fn a_live_master_turn_failure_names_its_input_facts() {
    let facts = InputFacts {
        messages: vec!["owner pending".into()],
        live: Some(
            "live true agent Some(7) turn_open false input_ms None typed_ms None draft false"
                .into(),
        ),
        input: "Err(NoMaster)".into(),
        briefed: Some(false),
        waited: Some("native_profile_in_use".into()),
        screen: vec!["Welcome to Claude Code".into()],
    };
    let line = input_evidence(&facts);
    for part in [
        "messages [\"owner pending\"]",
        "live row Some(\"live true agent Some(7)",
        "input check Err(NoMaster)",
        "briefed Some(false)",
        "profile wait Some(\"native_profile_in_use\")",
        "screen [\"Welcome to Claude Code\"]",
    ] {
        assert!(line.contains(part), "{part} is missing from: {line}");
    }
}

/// LMS-1 r1 L2: the master pane rows a failure message shows pass through the engine's
/// redaction, so a token on the master's screen never reaches a test log, and only the last
/// `SCREEN_ROWS` non-empty rows are kept.
#[test]
fn the_master_pane_rows_in_a_failure_are_redacted_and_bounded() {
    let token = "e2e0".repeat(10);
    let mut screen = String::new();
    for row in 0..12 {
        screen.push_str(&format!("row {row}\r\n"));
    }
    screen.push_str(&format!("paste this code {token}\r\n\r\n"));
    let rows = pane_rows(screen.as_bytes());
    assert_eq!(rows.len(), SCREEN_ROWS, "{rows:?}");
    assert_eq!(rows[0], "row 5", "{rows:?}");
    assert!(
        rows.last().unwrap().starts_with("paste this code "),
        "{rows:?}"
    );
    assert!(rows.iter().all(|row| !row.contains(&token)), "{rows:?}");
}

/// LMS-1: a turn wait is BLOCKED only with the custody answer of the runtime's own profile
/// wait on a master it never briefed while the owner's message waits; every other timeout
/// stays a FAIL.
#[test]
fn only_a_recorded_profile_wait_on_an_unbriefed_master_blocks_a_turn_wait() {
    let held = InputFacts {
        messages: vec!["owner pending".into()],
        briefed: Some(false),
        waited: Some("native_profile_in_use".into()),
        ..InputFacts::default()
    };
    assert!(profile_held(&held), "the profile wait is proven");
    for (why, facts) in [
        (
            "no profile wait recorded",
            InputFacts {
                waited: None,
                ..held.clone()
            },
        ),
        (
            "a sign-in check, not another process",
            InputFacts {
                waited: Some(super::SIGN_IN_CHECKING.into()),
                ..held.clone()
            },
        ),
        (
            "the master was briefed",
            InputFacts {
                briefed: Some(true),
                ..held.clone()
            },
        ),
        (
            "no master worker",
            InputFacts {
                briefed: None,
                ..held.clone()
            },
        ),
        (
            "the message was typed",
            InputFacts {
                messages: vec!["owner answered".into()],
                ..held.clone()
            },
        ),
    ] {
        assert!(!profile_held(&facts), "{why} stays a FAIL");
    }
}

/// OP-38, N-L1: a live Claude Code master on a pool account the engine reads at its limit
/// stops when its real turn is refused for that limit, and the same master row starts again
/// on the pool's working account, where its next turn answers, with the live switch on.
/// Picks the two accounts from the running app's engine at run time; BLOCKED when none of
/// its signed-in Claude accounts is at a limit, never a pass.
#[test]
#[ignore = "live qualification: needs the running app's engine with two signed-in Claude accounts, one it reads at its limit, CRAZYAGENT_MANAGED_CLAUDE_PROFILE"]
fn live_a_rate_limited_live_master_stops_and_its_next_turn_runs_on_the_working_pool_account() {
    let (_, owner_database, owner, host) = owner_engine(SIGNED_IN);
    let found = signed_in_accounts(&owner, &host);
    drop(owner);
    let limited = found.iter().find(|account| account.limited).unwrap_or_else(|| {
        panic!(
            "BLOCKED: no account at its limit: none of the {} signed-in Claude accounts is at its 5-hour or weekly limit in the engine's usage reading",
            found.len()
        )
    });
    let working = found
        .iter()
        .find(|account| !account.limited)
        .unwrap_or_else(|| {
            panic!(
                "BLOCKED: no working account: all {} signed-in Claude accounts are at a limit",
                found.len()
            )
        });
    let mut team = Team::new(&host, &owner_database, limited, working);
    super::super::master::set_live(&team.db, &team.project, true).unwrap();
    team.until(
        "the live master starts on the limited account",
        START,
        |t| {
            t.master()
                .is_some_and(|(id, account)| account == "limited" && t.live_agent() == Some(id))
        },
    );
    let (master, _) = team.master().unwrap();

    team.say(ASK);
    team.until(
        "the limited account's turn is refused for its limit and the master starts again on the working account",
        TURN,
        |t| {
            let turns = t.turns();
            let refused = turns.iter().any(|(_, outcome)| outcome == "rate_limited");
            if let Some((turn, _)) = turns.iter().find(|(_, outcome)| outcome == "success").filter(|_| !refused) {
                panic!(
                    "BLOCKED: the account the engine read at its limit answered turn {turn}; its limit has reset since the last reading: {}",
                    t.seen()
                );
            }
            refused
                && t.master() == Some((master, "working"))
                && t.live_agent() == Some(master)
        },
    );
    let refused = team.turns().last().unwrap().0;
    assert_eq!(team.rested(), ["limited"], "{}", team.seen());

    team.say(ASK);
    team.until(
        "the master's next turn answers on the working account",
        TURN,
        |t| {
            t.turns()
                .iter()
                .any(|(turn, outcome)| *turn > refused && outcome == "success")
                && t.master() == Some((master, "working"))
        },
    );
    assert_eq!(
        team.masters().len(),
        1,
        "the same master row: {}",
        team.seen()
    );
    assert!(
        super::super::master::live(&team.db, &team.project).unwrap(),
        "a rate limit does not turn the live master off: {}",
        team.seen()
    );
    println!("live failover: {}", team.seen());
}
