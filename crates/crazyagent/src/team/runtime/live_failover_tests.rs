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
use serde_json::Value;
use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

const PROJECT: &str = "live-failover";
const SIGNED_IN: &str = "CRAZYAGENT_MANAGED_CLAUDE_PROFILE";
const START: Duration = Duration::from_secs(120);
const TURN: Duration = Duration::from_secs(300);
const ASK: &str = "Reply with only the word READY.";

/// One latest sign-in row of `native_auth_operations` as the owner's engine stores it: id,
/// profile key, generation, phase, request and fact.
type Operation = (String, String, i64, String, Vec<u8>, Vec<u8>);

/// One of the owner's signed-in Claude accounts as the owner's engine records it: the
/// connection its proven sign-in names, that sign-in's row and fact, and whether the
/// engine's usage reading has the account at its 5-hour or weekly limit now.
struct Account {
    connection: ModelConnection,
    operation: Operation,
    fact: NativeAuthFact,
    limited: bool,
}

/// The running engine of the app that holds the managed profile `CRAZYAGENT_MANAGED_CLAUDE_PROFILE`
/// names, read only: its profiles folder, its database path, a read-only connection to that
/// database and the run it is in. Output: the four, or a panic with the plain BLOCKED message.
fn owner_engine() -> (PathBuf, PathBuf, Connection, HostStamp) {
    let slot = std::env::var_os(SIGNED_IN).unwrap_or_else(|| {
        panic!(
            "BLOCKED: no signed-in managed Claude profile. Run with {SIGNED_IN}=<engine folder>/native-profiles/<profile key>"
        )
    });
    let slot = PathBuf::from(slot)
        .canonicalize()
        .unwrap_or_else(|_| panic!("BLOCKED: {SIGNED_IN} does not name a profile slot"));
    let profiles = slot.parent().unwrap().to_path_buf();
    let database = profiles.parent().unwrap().join("engine.db");
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
    (profiles, database, owner, host)
}

/// Every Claude profile of the owner's engine whose latest sign-in passes this run's launch
/// check, as every master turn checks it, with whether the engine reads the account at a
/// limit: a 5-hour or weekly window, observed or polled, at 100 percent used now. Reads only
/// each slot's identity manifest, never a credential. Inputs: the profiles folder, the
/// read-only engine and its run. Output: the accounts in slot name order.
fn signed_in_accounts(profiles: &Path, owner: &Connection, host: &HostStamp) -> Vec<Account> {
    let mut slots: Vec<PathBuf> = std::fs::read_dir(profiles)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|slot| slot.join("identity.json").is_file())
        .collect();
    slots.sort();
    let now = now_ms();
    let mut found = Vec::new();
    for slot in slots {
        let identity: Value =
            serde_json::from_slice(&std::fs::read(slot.join("identity.json")).unwrap()).unwrap();
        if identity["binding"]["harness"].as_str() != Some("claude") {
            continue;
        }
        let Some(profile_ref) = identity["binding"]["profile_ref"].as_str() else {
            continue;
        };
        let key = crate::memory::hash(profile_ref.as_bytes());
        let operation: Option<Operation> = owner
            .query_row(
                "SELECT id,profile_key,generation,phase,request,fact FROM native_auth_operations WHERE profile_key=?1 ORDER BY generation DESC LIMIT 1",
                [&key],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?)),
            )
            .optional()
            .unwrap();
        let Some(operation) = operation else {
            continue;
        };
        let fact = crate::engine::owner::native_auth::decode_fact(&operation.5).unwrap();
        let connection = fact.binding.connection.clone();
        assert_eq!(
            connection.auth,
            crate::config::ModelAuthentication::NativeManaged {
                profile_ref: CredentialRef::new(profile_ref).unwrap(),
            },
            "a profile's sign-in names that profile"
        );
        let tx = owner.unchecked_transaction().unwrap();
        let proved = crate::storage::native_sign_in(&tx, host, &connection, None).is_ok();
        drop(tx);
        if !proved {
            continue;
        }
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
        found.push(Account {
            connection,
            operation,
            fact,
            limited,
        });
    }
    found
}

/// The test's own team engine in a new folder with one project whose live master is Claude
/// Code and whose pool holds the limited account first, then the working one. Its database
/// carries the owner's engine run and the two accounts' proven sign-in rows as they stand,
/// and its custody is the owner's engine's own, which only reads that database and takes a
/// shared lock on a profile for each launch, as every engine run does.
struct Team {
    runtime: TeamRuntime,
    db: Connection,
    project: ProjectId,
    host: Arc<dyn Host>,
    tmux: PathBuf,
    base: PathBuf,
    limited: CredentialRef,
    working: CredentialRef,
    _root: tempfile::TempDir,
}

impl Team {
    /// Prepares the engine. Inputs: the owner's run, the owner's database and the two
    /// accounts. Output: the team, its live switch still off.
    fn new(host: &HostStamp, owner_database: &Path, limited: &Account, working: &Account) -> Self {
        assert!(limited.limited && !working.limited);
        assert_ne!(limited.connection.id, working.connection.id);
        let root = tempfile::Builder::new()
            .prefix("ca-live-failover-")
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let base = root.path().canonicalize().unwrap();
        let tmux = installed(&base).expect("BLOCKED: tmux must be installed");
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
        db.execute(
            "INSERT OR REPLACE INTO owner_meta(singleton,schema_version,db_instance,engine_epoch,runtime_state) VALUES (1,1,?1,?2,'active')",
            params![host.db_instance, host.engine_epoch],
        )
        .unwrap();
        for account in [limited, working] {
            let (id, key, generation, phase, request, fact) = &account.operation;
            db.execute(
                "INSERT INTO native_auth_operations(id,profile_key,generation,phase,request,fact) VALUES (?1,?2,?3,?4,?5,?6)",
                params![id, key, generation, phase, request, fact],
            )
            .unwrap();
        }
        let revision = limited
            .fact
            .binding
            .at_revision
            .value()
            .max(working.fact.binding.at_revision.value());
        assert_eq!(
            db.execute(
                "UPDATE state SET revision=MAX(revision,?1) WHERE id=1",
                [i64::try_from(revision).unwrap()],
            )
            .unwrap(),
            1,
            "the engine keeps one revision row"
        );
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
        let host_terminal: Arc<dyn Host> =
            Arc::new(Tmux::open(Some(tmux.clone()), &base.join(FOLDER)));
        let mut runtime = TeamRuntime::open(
            &base.join("engine.db"),
            &base.join("team"),
            BTreeMap::from([("claude-code", super::managed_agent::claude())]),
            Box::new(move || Some(access.clone())),
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
        Self {
            runtime,
            db,
            project,
            host: host_terminal,
            tmux,
            base,
            limited: limited.connection.id.clone(),
            working: working.connection.id.clone(),
            _root: root,
        }
    }

    /// Every master row of the project with its account. Output: (id, connection) by id.
    fn masters(&self) -> Vec<(i64, Option<String>)> {
        self.db
            .prepare("SELECT id,connection FROM team_agents WHERE project=?1 AND role='master' ORDER BY id")
            .unwrap()
            .query_map([PROJECT], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }

    /// The agent the project's live master record runs, if any.
    fn live_agent(&self) -> Option<i64> {
        self.db
            .query_row(
                "SELECT agent FROM team_live_masters WHERE project=?1",
                [PROJECT],
                |row| row.get(0),
            )
            .optional()
            .unwrap()
            .flatten()
    }

    /// Every ended master turn kept for the project. Output: (turn, outcome) by turn.
    fn turns(&self) -> Vec<(i64, String)> {
        self.db
            .prepare("SELECT turn,outcome FROM team_master_turns WHERE project=?1 ORDER BY turn")
            .unwrap()
            .query_map([PROJECT], |row| Ok((row.get(0)?, row.get(1)?)))
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

    /// Which of the two accounts a connection is, so the record never prints an account id.
    fn name(&self, connection: Option<&str>) -> &'static str {
        match connection {
            Some(id) if id == self.limited.as_str() => "limited",
            Some(id) if id == self.working.as_str() => "working",
            Some(_) => "other",
            None => "none",
        }
    }

    /// The one master and the account it runs on, named, once exactly one master row exists.
    fn master(&self) -> Option<(i64, &'static str)> {
        match self.masters().as_slice() {
            [(id, connection)] => Some((*id, self.name(connection.as_deref()))),
            _ => None,
        }
    }

    /// What the engine records for the project, for a failure message: the masters with
    /// their account, status and error, the ended turns, the resting accounts and the switch.
    fn seen(&self) -> String {
        let agents: Vec<(i64, Option<String>, String, Option<String>)> = self
            .db
            .prepare("SELECT id,connection,status,error FROM team_agents WHERE project=?1 AND role='master' ORDER BY id")
            .unwrap()
            .query_map([PROJECT], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)))
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
            "masters {agents:?}; turns {:?}; resting {:?}; live {}",
            self.turns(),
            self.rested(),
            super::super::master::live(&self.db, &self.project).unwrap()
        )
    }

    /// Types one owner message to the master the way the owner's app sends it.
    fn say(&self, text: &str) {
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
    /// records when the time runs out.
    fn until(&mut self, what: &str, limit: Duration, done: impl Fn(&Self) -> bool) {
        let deadline = Instant::now() + limit;
        while !done(self) {
            assert!(
                Instant::now() < deadline,
                "{what} within {} s: {}",
                limit.as_secs(),
                self.seen()
            );
            self.runtime.tick().unwrap();
            std::thread::sleep(Duration::from_millis(200));
        }
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

/// OP-38, N-L1: a live Claude Code master on a pool account the engine reads at its limit
/// stops when its real turn is refused for that limit, and the same master row starts again
/// on the pool's working account, where its next turn answers, with the live switch on.
/// Picks the two accounts from the running app's engine at run time; BLOCKED when none of
/// its signed-in Claude accounts is at a limit, never a pass.
#[test]
#[ignore = "live qualification: needs the running app's engine with two signed-in Claude accounts, one it reads at its limit, CRAZYAGENT_MANAGED_CLAUDE_PROFILE"]
fn live_a_rate_limited_live_master_stops_and_its_next_turn_runs_on_the_working_pool_account() {
    let (profiles, owner_database, owner, host) = owner_engine();
    let found = signed_in_accounts(&profiles, &owner, &host);
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
