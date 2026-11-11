use super::{Interrupted, LastTurn, Listed, RunState, list, scan};
use crate::config::{
    AccountPool, ModelAccess, ModelAuthentication, ModelConnection, ModelEndpoint, ModelReference,
};
use crate::harnesses::interactive::{InteractiveCommand, Transcript};
use crate::harnesses::terminal_session::PaneRecord;
use crate::project_setup::{HarnessSlot, StoredSetup, TeamPlan};
use crate::protocol::{CredentialRef, ProjectId};
use crate::sandbox::policy::AgentAccess;
use crate::team::runtime::{TeamLimits, TeamRuntime};
use crate::team::{AgentRole, AgentStatus};
use crate::terminal::tmux::{SOCKET, Tmux, installed};
use crate::terminal::{AgentId, FOLDER, Host};
use rusqlite::{Connection, params};
use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, SystemTime};
#[cfg(target_os = "macos")]
use std::time::{Instant, UNIX_EPOCH};

const PROJECT: &str = "recovery-runs";
const ENDPOINT: &str = "local-a";
const HEARTBEAT: i64 = 1_800_000_000_000;
const TURN_OPEN: &str = r#"{"type":"user","message":{"content":"Plan the page"}}"#;
const TURN_DONE: &str = r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Done"}],"stop_reason":"end_turn"}}"#;
#[cfg(target_os = "macos")]
const WAIT: Duration = Duration::from_secs(30);

fn reference(value: &str) -> CredentialRef {
    CredentialRef::new(value).unwrap()
}

/// The listing of runs that all still wait. Input: the waiting runs, oldest first.
/// Output: the listing with nothing more waiting and nothing settled.
fn waiting(found: &[Interrupted]) -> Listed {
    Listed {
        waiting: found.to_vec(),
        more: 0,
        settled: Vec::new(),
    }
}

#[cfg(target_os = "macos")]
fn now() -> i64 {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap()
}

struct Fixture {
    _root: tempfile::TempDir,
    base: PathBuf,
    tmux: PathBuf,
    connection: Connection,
    access: ModelAccess,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::Builder::new()
            .prefix("ca-recovery-")
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let base = root.path().canonicalize().unwrap();
        let tmux = installed(&base).expect("tmux must be installed on this machine");
        let connection = Connection::open(base.join("engine.db")).unwrap();
        crate::storage::initialize_authority_schemas(&connection).unwrap();
        let owner = connection.unchecked_transaction().unwrap();
        crate::conversation::initialize(&owner).unwrap();
        owner.commit().unwrap();
        crate::team::initialize(&connection).unwrap();
        crate::project_setup::initialize(&connection).unwrap();
        let folder = base.join("project");
        std::fs::create_dir(&folder).unwrap();
        let setup = StoredSetup {
            project_id: ProjectId::new(PROJECT).unwrap(),
            name: "Recovery".into(),
            about: String::new(),
            tech_stack: vec![],
            platforms: vec![],
            research: vec![],
            pool: None,
            team: TeamPlan {
                master: "codex".into(),
                master_tools: false,
                sub_harnesses: vec![HarnessSlot {
                    harness: "codex".into(),
                    agents: 1,
                    tools: false,
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
            project_id: setup.project_id.clone(),
            spec: crate::protocol::ProjectSpec {
                name: setup.name.clone(),
                repositories: vec![],
            },
        };
        connection
            .execute(
                "INSERT INTO projects VALUES (?1,?2)",
                params![PROJECT, serde_json::to_string(&row).unwrap()],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO project_setups VALUES (?1,?2)",
                params![PROJECT, serde_json::to_string(&setup).unwrap()],
            )
            .unwrap();
        let access = ModelAccess {
            accounts: vec![reference("local-account")],
            connections: vec![ModelConnection {
                id: reference(ENDPOINT),
                provider: reference("openai-compatible"),
                harness: reference("rcli"),
                protocol: reference("openai-chat"),
                account_ref: reference("local-account"),
                endpoint: ModelEndpoint::Http {
                    url: "http://127.0.0.1:11434/v1".into(),
                },
                model: ModelReference {
                    id: "qwen2.5:0.5b".into(),
                    author: reference("openai-compatible"),
                },
                auth: ModelAuthentication::None {},
            }],
            pools: vec![AccountPool {
                id: reference("team-pool"),
                name: None,
                members: vec![reference(ENDPOINT)],
                projects: vec![reference(PROJECT)],
                roles: vec![reference("project_leader")],
                fallback_allowed: false,
                provider_quota_policy: Default::default(),
            }],
        };
        Self {
            _root: root,
            base,
            tmux,
            connection,
            access,
        }
    }

    fn project(&self) -> ProjectId {
        ProjectId::new(PROJECT).unwrap()
    }

    fn host(&self) -> Arc<dyn Host> {
        Arc::new(Tmux::open(Some(self.tmux.clone()), &self.base.join(FOLDER)))
    }

    fn runtime(&self) -> TeamRuntime {
        let access = self.access.clone();
        TeamRuntime::open(
            &self.base.join("engine.db"),
            &self.base.join("team"),
            BTreeMap::from([("codex", self.base.join("codex"))]),
            Box::new(move || Some(access.clone())),
            TeamLimits::default(),
        )
        .unwrap()
    }

    fn agent(&self, role: AgentRole, status: AgentStatus, turns: i64) -> i64 {
        let role = match role {
            AgentRole::Master => "master",
            AgentRole::Sub => "sub",
            AgentRole::Teammate => "teammate",
        };
        self.connection
            .execute(
                "INSERT INTO team_agents(project,role,harness,tools,connection,status,turns,created_ms,heartbeat_ms) VALUES (?1,?2,'codex',0,?3,?4,?5,1,?6)",
                params![PROJECT, role, ENDPOINT, status.name(), turns, HEARTBEAT],
            )
            .unwrap();
        self.connection.last_insert_rowid()
    }

    fn running(&self, agent: i64, prompt: &str) -> i64 {
        self.connection
            .execute(
                "INSERT INTO team_tasks(project,source,harness,tools,prompt,status,agent,created_ms,started_ms) VALUES (?1,'master','codex',0,?2,'running',?3,1,1)",
                params![PROJECT, prompt, agent],
            )
            .unwrap();
        let task = self.connection.last_insert_rowid();
        self.connection
            .execute(
                "UPDATE team_agents SET status='working', task=?2 WHERE id=?1",
                params![agent, task],
            )
            .unwrap();
        task
    }

    /// Starts team agent `id`'s pane and keeps its record as a current engine does under the
    /// project's default policy: Sandboxed, with no toolchain, so a later engine's policy
    /// check (AL-2) keeps the pane it takes over. Takes the host and the agent.
    /// Returns the record.
    fn pane(&self, host: &Arc<dyn Host>, id: i64) -> PaneRecord {
        let file = self.base.join(format!("session-{id}.jsonl"));
        std::fs::write(&file, format!("{TURN_OPEN}\n{TURN_DONE}\n")).unwrap();
        let command = InteractiveCommand {
            argv: vec!["/bin/sh".into(), "-c".into(), "exec cat".into()],
            cwd: self.base.clone(),
            env: Vec::new(),
            transcript: Transcript::ClaudeSession { file },
        };
        let since = SystemTime::now();
        let pane = host.spawn(&AgentId::team(id).unwrap(), &command).unwrap();
        let record = PaneRecord {
            access: Some(AgentAccess::Sandboxed),
            toolchains: Some(Vec::new()),
            ..PaneRecord::new(&command.transcript, &command.cwd, since, pane.pid, None).unwrap()
        };
        self.connection
            .execute(
                "INSERT OR REPLACE INTO team_panes(agent,record) VALUES (?1,?2)",
                params![id, record.encode().unwrap()],
            )
            .unwrap();
        record
    }

    fn status(&self, id: i64) -> String {
        self.connection
            .query_row("SELECT status FROM team_agents WHERE id=?1", [id], |row| {
                row.get(0)
            })
            .unwrap()
    }

    fn runs(&self) -> i64 {
        self.connection
            .query_row("SELECT COUNT(*) FROM recovery_runs", [], |row| row.get(0))
            .unwrap()
    }

    #[cfg(target_os = "macos")]
    fn until(&self, team: &mut TeamRuntime, what: &str, done: impl Fn(&Self) -> bool) {
        let deadline = Instant::now() + WAIT;
        while !done(self) {
            assert!(Instant::now() < deadline, "{what}");
            team.tick().unwrap();
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
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

#[test]
fn agent_running_at_stop_without_a_live_pane_is_recorded_interrupted() {
    let fixture = Fixture::new();
    let host = fixture.host();
    let sub = fixture.agent(AgentRole::Sub, AgentStatus::Idle, 2);
    let task = fixture.running(sub, "  Fix the login page\nThen run the tests");
    let record = fixture.pane(&host, sub);
    host.kill(&AgentId::team(sub).unwrap()).unwrap();
    assert!(host.alive().unwrap().is_empty(), "the real pane is gone");

    let mut team = fixture.runtime().with_terminal(host.clone()).unwrap();
    let found = scan(&fixture.base).unwrap();
    assert_eq!(found.len(), 1, "{found:#?}");
    assert_eq!(
        found[0],
        Interrupted {
            id: found[0].id,
            agent: sub,
            project: fixture.project(),
            kind: AgentRole::Sub,
            harness: "codex".into(),
            at: HEARTBEAT,
            last_turn: LastTurn {
                number: 3,
                task: Some(task),
                message: None,
                asked: Some("Fix the login page".into()),
            },
            record: Some(record),
            cwd: Some(fixture.base.clone()),
            record_file: Some(fixture.base.join(format!("session-{sub}.jsonl"))),
            paused: false,
            state: RunState::Interrupted,
            reason: None,
        }
    );
    assert_eq!(
        list(&fixture.base, &fixture.project()).unwrap(),
        waiting(&found)
    );
    assert_eq!(fixture.status(sub), "stopped");
    assert!(host.alive().unwrap().is_empty(), "nothing was started");
    team.shutdown().unwrap();
}

/// A sub-agent the next engine takes over in its live pane while it was in a task keeps
/// the run of that task, recorded interrupted for its resume in that pane, and its pane
/// runs on.
#[test]
fn an_adoptable_live_pane_keeps_the_run_of_the_task_its_agent_had() {
    let fixture = Fixture::new();
    let host = fixture.host();
    let sub = fixture.agent(AgentRole::Sub, AgentStatus::Idle, 0);
    let task = fixture.running(sub, "Write the page");
    fixture.pane(&host, sub);

    let mut team = fixture.runtime().with_terminal(host.clone()).unwrap();
    assert_eq!(
        host.alive().unwrap(),
        vec![AgentId::team(sub).unwrap()],
        "the engine took the live pane over"
    );
    let found = scan(&fixture.base).unwrap();
    assert_eq!(found.len(), 1, "{found:#?}");
    assert_eq!(found[0].agent, sub);
    assert_eq!(found[0].last_turn.task, Some(task));
    assert_eq!(found[0].state, RunState::Interrupted);
    assert_eq!(
        list(&fixture.base, &fixture.project()).unwrap(),
        waiting(&found)
    );
    assert_eq!(fixture.runs(), 1);
    assert_ne!(fixture.status(sub), "stopped", "its pane runs on");
    team.shutdown().unwrap();
    host.close(&[]).unwrap();
}

#[test]
fn paused_team_agents_are_recorded_and_stay_paused() {
    let fixture = Fixture::new();
    let host = fixture.host();
    let project = fixture.project();
    fixture
        .connection
        .execute(
            "INSERT INTO team_paused_projects(project,since_ms) VALUES (?1,1)",
            [PROJECT],
        )
        .unwrap();
    let sub = fixture.agent(AgentRole::Sub, AgentStatus::Idle, 0);
    let task = fixture.running(sub, "Review the change");
    let record = fixture.pane(&host, sub);

    let mut team = fixture.runtime().with_terminal(host.clone()).unwrap();
    assert!(
        host.alive().unwrap().is_empty(),
        "a paused team's pane is not taken over"
    );
    let found = scan(&fixture.base).unwrap();
    assert_eq!(found.len(), 1, "{found:#?}");
    assert_eq!(found[0].agent, sub);
    assert_eq!(found[0].last_turn.task, Some(task));
    assert_eq!(found[0].record, Some(record));
    assert!(found[0].paused, "the record says the team was paused");

    for _ in 0..10 {
        team.tick().unwrap();
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        crate::team::paused(&fixture.connection, &project).unwrap(),
        "the team stays paused"
    );
    assert_eq!(fixture.status(sub), "stopped");
    assert!(host.alive().unwrap().is_empty(), "no agent started");
    let agents: i64 = fixture
        .connection
        .query_row(
            "SELECT COUNT(*) FROM team_agents WHERE project=?1",
            [PROJECT],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(agents, 1, "no agent was added");
    assert_eq!(list(&fixture.base, &project).unwrap(), waiting(&found));
    team.shutdown().unwrap();
}

#[test]
fn scan_twice_records_once() {
    let fixture = Fixture::new();
    let master = fixture.agent(AgentRole::Master, AgentStatus::Working, 4);
    fixture
        .connection
        .execute(
            "INSERT INTO team_messages(project,author,text,state,created_ms) VALUES (?1,'owner','Plan the release\nand tell me the risks','sent',1)",
            [PROJECT],
        )
        .unwrap();
    let message = fixture.connection.last_insert_rowid();

    let mut team = fixture.runtime();
    let first = scan(&fixture.base).unwrap();
    assert_eq!(first.len(), 1, "{first:#?}");
    assert_eq!(first[0].agent, master);
    assert_eq!(first[0].kind, AgentRole::Master);
    assert_eq!(first[0].at, HEARTBEAT);
    assert_eq!(
        first[0].last_turn,
        LastTurn {
            number: 5,
            task: None,
            message: Some(message),
            asked: Some("Plan the release".into()),
        }
    );
    assert_eq!(first[0].record, None);
    assert_eq!(first[0].cwd, None);
    assert_eq!(first[0].record_file, None);
    assert_eq!(scan(&fixture.base).unwrap(), first);
    team.shutdown().unwrap();

    let mut again = fixture.runtime();
    assert_eq!(
        scan(&fixture.base).unwrap(),
        first,
        "a later engine start records nothing new"
    );
    assert_eq!(fixture.runs(), 1);
    again.shutdown().unwrap();
}

/// macOS only: by DEC-094 no team agent or agent pane runs on Linux, where the launch is
/// refused with `launch_unavailable_on_linux` (asserted in `harnesses::terminal_session::tests`).
#[cfg(target_os = "macos")]
#[test]
fn a_clean_stop_mid_turn_records_the_run_when_its_pane_is_gone_at_the_next_start() {
    let fixture = Fixture::new();
    let host = fixture.host();
    let sub = fixture.agent(AgentRole::Sub, AgentStatus::Idle, 0);
    fixture.pane(&host, sub);
    let mut team = fixture.runtime().with_terminal(host.clone()).unwrap();
    fixture.until(&mut team, "the adopted agent records its pane", |f| {
        f.status(sub) == "idle"
            && f.connection
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM team_panes WHERE agent=?1)",
                    [sub],
                    |row| row.get::<_, bool>(0),
                )
                .unwrap()
    });
    let record: String = fixture
        .connection
        .query_row(
            "SELECT record FROM team_panes WHERE agent=?1",
            [sub],
            |row| row.get(0),
        )
        .unwrap();
    let task = fixture.running(sub, "Rename the module");

    let before = now();
    assert_eq!(
        team.shutdown().unwrap(),
        vec![AgentId::team(sub).unwrap()],
        "the agent's pane outlives the engine"
    );
    let after = now();
    assert_eq!(fixture.status(sub), "stopped");
    host.kill(&AgentId::team(sub).unwrap()).unwrap();

    let mut next = fixture.runtime().with_terminal(host.clone()).unwrap();
    let found = scan(&fixture.base).unwrap();
    assert_eq!(found.len(), 1, "{found:#?}");
    assert_eq!(found[0].agent, sub);
    assert!(
        (before..=after).contains(&found[0].at),
        "the run was cut at the stop"
    );
    assert_eq!(found[0].last_turn.task, Some(task));
    assert_eq!(
        found[0].last_turn.asked.as_deref(),
        Some("Rename the module")
    );
    assert_eq!(found[0].record, PaneRecord::decode(&record));
    assert_eq!(
        found[0].record_file,
        Some(fixture.base.join(format!("session-{sub}.jsonl")))
    );
    assert!(!found[0].paused);
    next.shutdown().unwrap();
}

fn rollout(day: &Path, name: &str, cwd: &Path) -> PathBuf {
    let file = day.join(name);
    let meta = serde_json::json!({
        "type": "session_meta",
        "payload": {"id": name.trim_end_matches(".jsonl"), "cwd": cwd.to_str().unwrap()}
    });
    let started = serde_json::json!({"type": "event_msg", "payload": {"type": "task_started"}});
    std::fs::write(&file, format!("{meta}\n{started}\n")).unwrap();
    file
}

#[test]
fn a_codex_agent_interrupted_mid_turn_names_its_own_rollout_file() {
    let fixture = Fixture::new();
    let host = fixture.host();
    let sub = fixture.agent(AgentRole::Sub, AgentStatus::Idle, 0);
    let task = fixture.running(sub, "Split the parser");
    let worktree = fixture.base.join("worktree-sub");
    let other = fixture.base.join("worktree-other");
    std::fs::create_dir(&worktree).unwrap();
    std::fs::create_dir(&other).unwrap();
    let sessions = fixture.base.join("codex-home").join("sessions");
    let day = sessions.join("2026").join("10").join("27");
    std::fs::create_dir_all(&day).unwrap();

    let since = SystemTime::now();
    let earlier = rollout(&day, "rollout-2026-10-27T00-00-00-earlier.jsonl", &worktree);
    std::fs::File::options()
        .write(true)
        .open(&earlier)
        .unwrap()
        .set_modified(since - Duration::from_secs(3600))
        .unwrap();
    rollout(&day, "rollout-2026-10-27T00-00-01-other.jsonl", &other);
    let own = rollout(&day, "rollout-2026-10-27T00-00-02-own.jsonl", &worktree);
    let command = InteractiveCommand {
        argv: vec!["/bin/sh".into(), "-c".into(), "exec cat".into()],
        cwd: worktree.clone(),
        env: Vec::new(),
        transcript: Transcript::CodexRollouts {
            sessions: sessions.clone(),
        },
    };
    let pane = host.spawn(&AgentId::team(sub).unwrap(), &command).unwrap();
    let record = PaneRecord::new(&command.transcript, &worktree, since, pane.pid, None).unwrap();
    fixture
        .connection
        .execute(
            "INSERT INTO team_panes(agent,record) VALUES (?1,?2)",
            params![sub, record.encode().unwrap()],
        )
        .unwrap();
    host.kill(&AgentId::team(sub).unwrap()).unwrap();

    let mut team = fixture.runtime().with_terminal(host.clone()).unwrap();
    let found = scan(&fixture.base).unwrap();
    assert_eq!(found.len(), 1, "{found:#?}");
    assert_eq!(found[0].agent, sub);
    assert_eq!(found[0].last_turn.task, Some(task));
    assert_eq!(found[0].record, Some(record));
    assert_eq!(found[0].cwd, Some(worktree));
    assert_eq!(
        found[0].record_file,
        Some(own),
        "neither the older rollout of the same folder nor another folder's rollout"
    );
    team.shutdown().unwrap();
}
