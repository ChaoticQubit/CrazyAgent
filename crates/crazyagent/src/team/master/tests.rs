use super::*;
use crate::harnesses::interactive::{InteractiveCommand, Transcript};
use crate::project_setup::{HarnessSlot, TeamPlan};
use crate::terminal::FOLDER;
use crate::terminal::tmux::{SOCKET, Tmux, installed};
use serde_json::{Value, json};
use std::io::Cursor;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Instant;

const PROJECT: &str = "live-master";
const MASTER: i64 = 41;
const NOW: i64 = 1_800_000_000_000;

struct Fixture {
    _root: tempfile::TempDir,
    base: PathBuf,
    connection: Connection,
    setup: StoredSetup,
    tmux: Option<PathBuf>,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::Builder::new()
            .prefix("ca-live-")
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let base = root.path().canonicalize().unwrap();
        let connection = Connection::open(base.join("engine.db")).unwrap();
        crate::storage::initialize_authority_schemas(&connection).unwrap();
        super::super::initialize(&connection).unwrap();
        crate::project_setup::initialize(&connection).unwrap();
        let folder = base.join("project");
        std::fs::create_dir(&folder).unwrap();
        let setup = StoredSetup {
            project_id: ProjectId::new(PROJECT).unwrap(),
            name: "Live".into(),
            about: String::new(),
            tech_stack: vec![],
            platforms: vec![],
            research: vec![],
            pool: None,
            team: TeamPlan {
                master: "claude-code".into(),
                master_tools: true,
                sub_harnesses: vec![HarnessSlot {
                    harness: "codex".into(),
                    agents: 2,
                    tools: true,
                }],
                min_agents: 1,
                max_agents: 3,
            },
            documents: vec![],
            folder: folder.to_str().unwrap().into(),
            created_ms: 1,
            revision: 1,
        };
        Self {
            _root: root,
            base,
            connection,
            setup,
            tmux: None,
        }
    }

    fn live(&self, agent: Option<i64>) {
        self.connection
            .execute(
                "INSERT OR REPLACE INTO team_live_masters(project,live,agent) VALUES (?1,1,?2)",
                params![PROJECT, agent],
            )
            .unwrap();
    }

    fn set(&self, sql: &str) {
        self.connection
            .execute(
                &format!("UPDATE team_live_masters SET {sql} WHERE project=?1"),
                [PROJECT],
            )
            .unwrap();
    }

    fn draft(&self) -> (Option<i64>, bool) {
        self.connection
            .query_row(
                "SELECT typed_ms,draft FROM team_live_masters WHERE project=?1",
                [PROJECT],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap()
    }

    fn host(&mut self) -> Arc<dyn Host> {
        let tmux = installed(&self.base).expect("tmux must be installed on this machine");
        self.tmux = Some(tmux.clone());
        Arc::new(Tmux::open(Some(tmux), &self.base.join(FOLDER)))
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(tmux) = &self.tmux {
            let _ = crate::spawn_gate::status(
                Command::new(tmux)
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
}

fn typed_until(file: &Path, wanted: &[u8]) -> Vec<u8> {
    let until = Instant::now() + Duration::from_secs(15);
    loop {
        let seen = std::fs::read(file).unwrap_or_default();
        if seen.windows(wanted.len()).any(|window| window == wanted) || Instant::now() >= until {
            return seen;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn engine_input_reaches_an_idle_live_master_only_while_the_owner_is_not_typing() {
    let mut fixture = Fixture::new();
    let host = fixture.host();
    let project = fixture.setup.project_id.clone();
    let typed = fixture.base.join("typed");
    let command = InteractiveCommand {
        argv: vec![
            "/bin/sh".into(),
            "-c".into(),
            format!(
                "/bin/stty raw -echo; printf '\\033[?2004hready\\n'; exec /bin/cat > '{}'",
                typed.display()
            ),
        ],
        cwd: fixture.base.clone(),
        env: Vec::new(),
        transcript: Transcript::Unavailable,
    };
    host.spawn(&AgentId::team(MASTER).unwrap(), &command)
        .unwrap();
    let input = |text: &str, now: i64| {
        master_input_at(
            &fixture.connection,
            host.as_ref(),
            None,
            &project,
            text,
            now,
        )
    };

    assert_eq!(input("hello", NOW), Err(InputRefused::NoMaster));
    fixture.live(None);
    assert_eq!(input("hello", NOW), Err(InputRefused::NoMaster));
    fixture.live(Some(MASTER));
    assert_eq!(input("   ", NOW), Err(InputRefused::Invalid));
    assert_eq!(input("bell \u{7}", NOW), Err(InputRefused::Invalid));
    assert_eq!(
        input(&"x".repeat(INPUT_TEXT_MAX + 1), NOW),
        Err(InputRefused::Invalid)
    );
    fixture.set("turn_open=1");
    assert_eq!(input("hello", NOW), Err(InputRefused::Busy));
    fixture.set("turn_open=0");

    owner_typed(&fixture.connection, MASTER, b"x", NOW - 5_000).unwrap();
    assert_eq!(input("hello", NOW), Err(InputRefused::OwnerTyping));
    assert_eq!(
        input("hello", NOW + 60_000),
        Err(InputRefused::OwnerTyping),
        "an unsent draft holds engine input back"
    );
    owner_typed(&fixture.connection, MASTER, b"\r", NOW).unwrap();
    assert_eq!(input("hello", NOW + 5_000), Err(InputRefused::OwnerTyping));
    assert_eq!(fixture.draft(), (Some(NOW), false));

    assert_eq!(
        input("[team] Ada (@page) asks:\nwhich header?", NOW + 11_000),
        Ok(())
    );
    let pasted = b"\x1b[200~[team] Ada (@page) asks:\nwhich header?\x1b[201~\r";
    let seen = typed_until(&typed, pasted);
    assert!(seen.windows(pasted.len()).any(|w| w == pasted), "{seen:?}");
    assert_eq!(
        input("second", NOW + 12_000),
        Err(InputRefused::Busy),
        "the first input has not started its turn yet"
    );
    assert_eq!(input("second", NOW + 11_000 + INPUT_SETTLE_MS), Ok(()));
    let seen = typed_until(&typed, b"second\x1b[201~\r");
    assert!(seen.ends_with(b"second\x1b[201~\r"), "{seen:?}");

    owner_typed(&fixture.connection, MASTER, b"draft", NOW).unwrap();
    assert_eq!(
        input("third", NOW + DRAFT_ABANDONED_MS + 1),
        Ok(()),
        "a draft untouched for two minutes counts as abandoned"
    );

    host.kill(&AgentId::team(MASTER).unwrap()).unwrap();
    let gone = input("fourth", NOW + DRAFT_ABANDONED_MS + 1 + INPUT_SETTLE_MS);
    assert!(matches!(gone, Err(InputRefused::Failed(_))), "{gone:?}");
    let input_ms: Option<i64> = fixture
        .connection
        .query_row(
            "SELECT input_ms FROM team_live_masters WHERE project=?1",
            [PROJECT],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        input_ms, None,
        "a failed input frees the master for the next one"
    );
}

#[test]
fn owner_keys_leave_a_draft_until_enter_ctrl_c_or_ctrl_u_and_arrows_change_nothing() {
    assert_eq!(keys(b"abc"), (false, true));
    assert_eq!(keys(b"abc\r"), (true, false));
    assert_eq!(keys(b"\rabc"), (true, true));
    assert_eq!(keys(b"\x03"), (true, false));
    assert_eq!(keys(b"\x15"), (true, false));
    assert_eq!(keys(b"\x1b[A\x1b[B\x1bOC"), (false, false));
    assert_eq!(keys(b"\x7f"), (false, false));
    assert_eq!(keys(b"\n"), (false, true));
    assert_eq!(keys(b"\x1b[200~pasted\x1b[201~"), (false, true));
    assert_eq!(keys("é".as_bytes()), (false, true));

    let fixture = Fixture::new();
    fixture.live(Some(MASTER));
    owner_typed(&fixture.connection, MASTER, b"plan", NOW).unwrap();
    assert_eq!(fixture.draft(), (Some(NOW), true));
    owner_typed(&fixture.connection, MASTER, b"\x1b[D", NOW + 1).unwrap();
    assert_eq!(fixture.draft(), (Some(NOW + 1), true));
    owner_typed(&fixture.connection, MASTER, b"\r", NOW + 2).unwrap();
    assert_eq!(fixture.draft(), (Some(NOW + 2), false));
    owner_typed(&fixture.connection, MASTER + 1, b"other", NOW + 3).unwrap();
    assert_eq!(
        fixture.draft(),
        (Some(NOW + 2), false),
        "only the master's keys count"
    );
    fixture.set("live=0");
    owner_typed(&fixture.connection, MASTER, b"off", NOW + 4).unwrap();
    assert_eq!(fixture.draft(), (Some(NOW + 2), false));
}

#[test]
fn owner_typing_kept_in_memory_holds_engine_input_back_before_any_write_lands() {
    let mut fixture = Fixture::new();
    let host = fixture.host();
    let project = fixture.setup.project_id.clone();
    let typed = fixture.base.join("typed");
    let command = InteractiveCommand {
        argv: vec![
            "/bin/sh".into(),
            "-c".into(),
            format!(
                "/bin/stty raw -echo; printf '\\033[?2004hready\\n'; exec /bin/cat > '{}'",
                typed.display()
            ),
        ],
        cwd: fixture.base.clone(),
        env: Vec::new(),
        transcript: Transcript::Unavailable,
    };
    host.spawn(&AgentId::team(MASTER).unwrap(), &command)
        .unwrap();
    fixture.live(Some(MASTER));
    let typing = OwnerTyping::new(fixture.base.join("never-written.db"));
    let input = |text: &str, now: i64| {
        master_input_at(
            &fixture.connection,
            host.as_ref(),
            Some(&typing),
            &project,
            text,
            now,
        )
    };

    typing.record(MASTER, b"x", NOW - 5_000);
    assert_eq!(input("hello", NOW), Err(InputRefused::OwnerTyping));
    assert_eq!(
        input("hello", NOW + 60_000),
        Err(InputRefused::OwnerTyping),
        "an unsent draft kept in memory holds engine input back"
    );
    typing.record(MASTER, b"\x1b[D", NOW - 1);
    assert_eq!(input("hello", NOW + 60_000), Err(InputRefused::OwnerTyping));
    typing.record(MASTER, b"\r", NOW);
    assert_eq!(input("hello", NOW + 5_000), Err(InputRefused::OwnerTyping));
    assert_eq!(
        fixture.draft(),
        (None, false),
        "the refusals came from memory alone"
    );
    typing.record(MASTER + 1, b"other", NOW + 10_000);
    assert_eq!(
        input("hello", NOW + 11_000),
        Ok(()),
        "keys typed into another agent do not hold the master back"
    );
    let seen = typed_until(&typed, b"hello\x1b[201~\r");
    assert!(seen.ends_with(b"hello\x1b[201~\r"), "{seen:?}");
}

#[test]
fn owner_typing_reaches_the_database_for_the_live_master_at_most_once_a_second() {
    let fixture = Fixture::new();
    fixture.live(Some(MASTER));
    fixture
        .connection
        .execute_batch(
            "CREATE TABLE typing_writes(at INTEGER);
             CREATE TRIGGER typing_written AFTER UPDATE OF typed_ms ON team_live_masters
             BEGIN INSERT INTO typing_writes(at) VALUES (NEW.typed_ms); END;",
        )
        .unwrap();
    let writes = || -> i64 {
        fixture
            .connection
            .query_row("SELECT COUNT(*) FROM typing_writes", [], |row| row.get(0))
            .unwrap()
    };
    let landed = |wanted: (Option<i64>, bool)| {
        let until = Instant::now() + Duration::from_secs(10);
        while fixture.draft() != wanted && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(20));
        }
        fixture.draft() == wanted
    };
    let typing = OwnerTyping::new(fixture.base.join("engine.db"));

    fixture.connection.execute_batch("BEGIN IMMEDIATE").unwrap();
    let recording = Instant::now();
    typing.record(MASTER, b"plan", NOW);
    assert!(
        recording.elapsed() < Duration::from_millis(100),
        "recording a key never waits on the database"
    );
    std::thread::sleep(Duration::from_millis(300));
    fixture.connection.execute_batch("COMMIT").unwrap();
    assert!(
        landed((Some(NOW), true)),
        "the signal is written once the lock is free"
    );

    let typing_started = Instant::now();
    for step in 1..=60 {
        typing.record(MASTER, b"\x1b[D", NOW + step);
        std::thread::sleep(Duration::from_millis(25));
    }
    typing.record(MASTER, b"\r", NOW + 61);
    assert!(landed((Some(NOW + 61), false)));
    let elapsed = typing_started.elapsed();
    let written = writes();
    assert!(
        written >= 2 && written - 1 <= elapsed.as_secs() as i64 + 2,
        "62 keys over {elapsed:?} made {written} writes"
    );

    fixture.connection.execute_batch("BEGIN IMMEDIATE").unwrap();
    typing.record(MASTER + 1, b"other", NOW + 70);
    std::thread::sleep(Duration::from_millis(1_500));
    let closing = Instant::now();
    drop(typing);
    assert!(
        closing.elapsed() < Duration::from_secs(2),
        "keys for an agent that is not the live master never wait on the database lock"
    );
    fixture.connection.execute_batch("COMMIT").unwrap();
    assert_eq!(fixture.draft(), (Some(NOW + 61), false));
}

fn serve_while(fixture: &mut Fixture, mailbox: &Path, requests: Vec<Value>) -> Vec<Value> {
    let mut input = Vec::new();
    for request in requests {
        input.extend(serde_json::to_vec(&request).unwrap());
        input.push(b'\n');
    }
    let served = mailbox.to_path_buf();
    let server = std::thread::spawn(move || {
        let mut output = Vec::new();
        tool::serve_live(Cursor::new(input), &mut output, &served, &[], None).unwrap();
        output
    });
    let access = ModelAccess::default();
    let setup = fixture.setup.clone();
    let landing = fixture.base.clone();
    let answering = Answering {
        access: &access,
        stored: &setup,
        landing: &landing,
        agents: &landing,
        events: None,
        host: None,
    };
    let deadline = Instant::now() + Duration::from_secs(30);
    while !server.is_finished() {
        assert!(Instant::now() < deadline, "the server never finished");
        serve_calls(&mut fixture.connection, &answering, MASTER, mailbox).unwrap();
        std::thread::sleep(Duration::from_millis(20));
    }
    server
        .join()
        .unwrap()
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_slice(line).unwrap())
        .collect()
}

fn tool_call(id: u64, name: &str, arguments: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "method": "tools/call",
        "params": {"name": name, "arguments": arguments}})
}

fn text(answer: &Value) -> (&str, bool) {
    (
        answer["result"]["content"][0]["text"].as_str().unwrap(),
        answer["result"]["isError"].as_bool().unwrap(),
    )
}

#[test]
fn a_live_master_s_team_calls_take_effect_while_its_turn_still_runs() {
    let mut fixture = Fixture::new();
    let mailbox = fixture.base.join(tool::MAILBOX_FOLDER);
    std::fs::create_dir(&mailbox).unwrap();
    let answers = serve_while(
        &mut fixture,
        &mailbox,
        vec![
            tool_call(
                1,
                tool::SPAWN_TOOL,
                json!({"task": "Write page.md", "harness": "codex", "role": "@page-writer"}),
            ),
            tool_call(
                2,
                tool::SPAWN_TOOL,
                json!({"task": "Run the tests", "harness": "pi"}),
            ),
            tool_call(
                3,
                tool::UPDATE_WORK_TOOL,
                json!({"to": "@page-writer", "text": "Use the old header."}),
            ),
            tool_call(4, tool::INSPECT_WORK_TOOL, json!({})),
            tool_call(5, tool::INSPECT_WORK_TOOL, json!({})),
        ],
    );
    assert_eq!(answers.len(), 5, "{answers:#?}");
    let task: i64 = fixture
        .connection
        .query_row(
            "SELECT id FROM team_tasks WHERE project=?1 AND prompt='Write page.md' AND source='master' AND harness='codex' AND status='needs_pool'",
            [PROJECT],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        text(&answers[0]),
        (
            format!("Task {task} is queued for a codex sub-agent. It waits until the pool has a ready account for that harness. The engine tells you when it ends.").as_str(),
            false
        )
    );
    let role: String = fixture
        .connection
        .query_row(
            "SELECT role FROM team_task_roles WHERE task=?1 AND project=?2",
            params![task, PROJECT],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(role, "page-writer");
    assert_eq!(
        text(&answers[1]),
        (
            "Not handed off: that harness is not in the team plan.",
            true
        )
    );
    assert!(
        text(&answers[2])
            .0
            .starts_with("Not sent: no agent of this project is called @page-writer"),
        "{answers:#?}"
    );
    assert!(!text(&answers[3]).1, "{answers:#?}");
    assert_eq!(
        text(&answers[4]),
        ("No change set is waiting for the owner.", false)
    );
    let tasks: i64 = fixture
        .connection
        .query_row("SELECT COUNT(*) FROM team_tasks", [], |row| row.get(0))
        .unwrap();
    assert_eq!(tasks, 1);
    assert_eq!(std::fs::read_dir(&mailbox).unwrap().count(), 0);
}

#[test]
fn a_planted_mailbox_call_is_checked_again_before_it_takes_effect() {
    let mut fixture = Fixture::new();
    let mailbox = fixture.base.join(tool::MAILBOX_FOLDER);
    std::fs::create_dir(&mailbox).unwrap();
    let planted = "a".repeat(32);
    std::fs::write(
        mailbox.join(format!("{planted}.call")),
        br#"{"tool":"spawn","task":"Write page.md","harness":"CODEX","role":null}"#,
    )
    .unwrap();
    let unreadable = "b".repeat(32);
    std::fs::write(mailbox.join(format!("{unreadable}.call")), b"{").unwrap();
    let access = ModelAccess::default();
    let setup = fixture.setup.clone();
    let landing = fixture.base.clone();
    let answering = Answering {
        access: &access,
        stored: &setup,
        landing: &landing,
        agents: &landing,
        events: None,
        host: None,
    };
    assert_eq!(
        serve_calls(&mut fixture.connection, &answering, MASTER, &mailbox).unwrap(),
        2
    );
    let reply = |id: &str| -> Value {
        serde_json::from_slice(&std::fs::read(mailbox.join(format!("{id}.reply"))).unwrap())
            .unwrap()
    };
    assert_eq!(reply(&planted)["error"], true);
    assert!(
        reply(&planted)["text"]
            .as_str()
            .unwrap()
            .starts_with("Not handed off")
    );
    assert_eq!(reply(&unreadable)["text"], UNREAD);
    let tasks: i64 = fixture
        .connection
        .query_row("SELECT COUNT(*) FROM team_tasks", [], |row| row.get(0))
        .unwrap();
    assert_eq!(tasks, 0);
}

/// Adds a sub-agent of the fixture's project with a role handle and, for a finished one, a
/// kept worktree, as a delegated agent leaves them. Takes the fixture, the role, the status
/// and when it stopped. Returns the agent id.
#[cfg(target_os = "macos")]
fn named_sub(fixture: &Fixture, role: &str, status: &str) -> i64 {
    fixture
        .connection
        .execute(
            "INSERT INTO team_agents(project,role,harness,tools,status,created_ms,heartbeat_ms) VALUES (?1,'sub','codex',0,'working',?2,?2)",
            params![PROJECT, NOW],
        )
        .unwrap();
    let agent = fixture.connection.last_insert_rowid();
    let project = ProjectId::new(PROJECT).unwrap();
    assert!(
        super::super::names::assign_role(&fixture.connection, &project, agent, role)
            .unwrap()
            .is_ok()
    );
    fixture
        .connection
        .execute(
            "UPDATE team_agents SET status=?2, stopped_ms=?3 WHERE id=?1",
            params![agent, status, NOW + agent],
        )
        .unwrap();
    fixture
        .connection
        .execute(
            "INSERT INTO team_worktrees(agent,project,repository,worktree,git_dir) VALUES (?1,?2,'.',?3,'.git')",
            params![agent, PROJECT, format!("worktrees/agent-{agent}")],
        )
        .unwrap();
    agent
}

/// The task ids pinned to an agent, as a wake leaves them. Takes the fixture and agent.
#[cfg(target_os = "macos")]
fn pinned(fixture: &Fixture, agent: i64) -> Vec<i64> {
    let mut statement = fixture
        .connection
        .prepare("SELECT task FROM team_task_pins WHERE agent=?1 ORDER BY task")
        .unwrap();
    statement
        .query_map([agent], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

/// A handle a finished sub-agent and a running one share wakes the finished one (WK-R2):
/// the master's `wake` looks among finished sub-agents first, so a live namesake never
/// answers "running", and the running agent is still reached by its number. The woken
/// agent's handle is settled when the wake is admitted and the reply names it (WKC-2): the
/// old handle still reaches only the running namesake, the new one reaches nobody else
/// while it waits and is not given away, and the agent starts under it.
#[cfg(target_os = "macos")]
#[test]
fn wake_by_a_shared_handle_reaches_the_finished_agent_not_its_live_namesake() {
    let mut fixture = Fixture::new();
    let mailbox = fixture.base.join(tool::MAILBOX_FOLDER);
    std::fs::create_dir(&mailbox).unwrap();
    let finished = named_sub(&fixture, "onboarding-fix", "stopped");
    let live = named_sub(&fixture, "onboarding-fix", "working");
    let label = super::super::events::label(&fixture.connection, finished).unwrap();
    assert!(label.ends_with("(@onboarding-fix)"), "{label}");
    let project = ProjectId::new(PROJECT).unwrap();
    let answers = serve_while(
        &mut fixture,
        &mailbox,
        vec![
            tool_call(
                1,
                tool::UPDATE_WORK_TOOL,
                json!({"to": "@onboarding-fix", "text": "Fix the onboarding copy."}),
            ),
            tool_call(
                2,
                tool::UPDATE_WORK_TOOL,
                json!({"to": format!("Sub-agent {live}"), "text": "Stop."}),
            ),
        ],
    );
    assert_eq!(answers.len(), 2, "{answers:#?}");
    let settled = super::super::events::label(&fixture.connection, finished).unwrap();
    assert!(settled.ends_with("(@onboarding-fix-2)"), "{settled}");
    assert_eq!(
        text(&answers[0]),
        (
            format!("Waking {settled}: it starts again in its own worktree and gets your text as its next task. The engine tells you when it ends.").as_str(),
            false
        )
    );
    assert_eq!(pinned(&fixture, finished).len(), 1);
    assert!(pinned(&fixture, live).is_empty());
    let (said, refused) = text(&answers[1]);
    assert!(refused && said.contains("is running"), "{said}");
    let resolve =
        |name: &str| super::super::names::resolve(&fixture.connection, &project, name).unwrap();
    assert_eq!(resolve("@onboarding-fix"), Some(live));
    assert_eq!(
        resolve("@onboarding-fix-2"),
        None,
        "a message by the woken agent's handle reaches no other agent while it waits"
    );
    let later = named_sub(&fixture, "onboarding-fix", "working");
    let handle = |agent: i64| -> String {
        fixture
            .connection
            .query_row(
                "SELECT handle FROM team_names WHERE agent=?1",
                [agent],
                |row| row.get(0),
            )
            .unwrap()
    };
    assert_eq!(
        handle(later),
        "onboarding-fix-3",
        "the waiting agent's handle is not given away"
    );
    fixture
        .connection
        .execute(
            "UPDATE team_agents SET status='starting' WHERE id=?1",
            [finished],
        )
        .unwrap();
    super::super::names::reclaim(&fixture.connection, &project, finished).unwrap();
    assert_eq!(handle(finished), "onboarding-fix-2");
    assert_eq!(resolve("@onboarding-fix-2"), Some(finished));
    assert_eq!(resolve("@onboarding-fix"), Some(live));
}

/// A sub-agent that finishes after the live master's session started is in the master's
/// status read, named with its Sub-agent number, and wakes by that number (WK-R3), so a
/// master whose briefing predates it can still find it.
#[cfg(target_os = "macos")]
#[test]
fn an_agent_that_finishes_after_the_briefing_is_listed_by_status_and_wakes() {
    let mut fixture = Fixture::new();
    let mailbox = fixture.base.join(tool::MAILBOX_FOLDER);
    std::fs::create_dir(&mailbox).unwrap();
    let project = ProjectId::new(PROJECT).unwrap();
    let before = status_text(&fixture.connection, &project).unwrap();
    assert!(!before.contains("Finished sub-agents"), "{before}");

    let agent = named_sub(&fixture, "page-writer", "stopped");
    let label = super::super::events::label(&fixture.connection, agent).unwrap();
    let told = status_text(&fixture.connection, &project).unwrap();
    assert!(
        told.contains(&format!(
            "Finished sub-agents you can wake with {}(to, text), newest first:\n- {label}, Sub-agent {agent} (",
            tool::UPDATE_WORK_TOOL
        )),
        "{told}"
    );
    let answers = serve_while(
        &mut fixture,
        &mailbox,
        vec![tool_call(
            1,
            tool::UPDATE_WORK_TOOL,
            json!({"to": format!("Sub-agent {agent}"), "text": "Add the footer."}),
        )],
    );
    assert_eq!(answers.len(), 1, "{answers:#?}");
    let (said, refused) = text(&answers[0]);
    assert!(
        !refused && said.starts_with(&format!("Waking {label}:")),
        "{said}"
    );
    assert_eq!(pinned(&fixture, agent).len(), 1);
}

/// A master's wake of a sub-agent the owner stopped is refused in plain words and pins
/// nothing (AW-1); only the owner's Wake or restart starts it again.
#[cfg(target_os = "macos")]
#[test]
fn the_master_cannot_wake_an_agent_the_owner_stopped() {
    let mut fixture = Fixture::new();
    let mailbox = fixture.base.join(tool::MAILBOX_FOLDER);
    std::fs::create_dir(&mailbox).unwrap();
    let agent = named_sub(&fixture, "page-writer", "stopped");
    fixture
        .connection
        .execute(
            "INSERT INTO team_owner_stops(agent,project,stopped_ms) VALUES (?1,?2,?3)",
            params![agent, PROJECT, NOW],
        )
        .unwrap();
    let label = super::super::events::label(&fixture.connection, agent).unwrap();
    let answers = serve_while(
        &mut fixture,
        &mailbox,
        vec![tool_call(
            1,
            tool::UPDATE_WORK_TOOL,
            json!({"to": "@page-writer", "text": "You were interrupted, finish it."}),
        )],
    );
    assert_eq!(answers.len(), 1, "{answers:#?}");
    assert_eq!(
        text(&answers[0]),
        (
            format!("The owner stopped {label}. Only the owner can wake it.").as_str(),
            true
        )
    );
    assert!(pinned(&fixture, agent).is_empty());
    let tasks: i64 = fixture
        .connection
        .query_row("SELECT COUNT(*) FROM team_tasks", [], |row| row.get(0))
        .unwrap();
    assert_eq!(tasks, 0);
}

/// D6: a project's live master answers a turn only with the live switch on, an agent and an
/// open turn; a database without the live master table has none.
#[test]
fn a_live_master_turn_runs_only_while_live_with_an_agent_and_an_open_turn() {
    let directory = tempfile::tempdir().unwrap();
    let connection = Connection::open(directory.path().join("engine.db")).unwrap();
    let project = ProjectId::new("live-turn").unwrap();
    assert!(!turn_running(&connection, &project).unwrap());
    initialize(&connection).unwrap();
    assert!(!turn_running(&connection, &project).unwrap());
    let set = |live: i64, agent: Option<i64>, open: i64| {
        connection
            .execute(
                "INSERT INTO team_live_masters(project,live,agent,turn_open) VALUES (?1,?2,?3,?4)
                 ON CONFLICT(project) DO UPDATE SET live=?2, agent=?3, turn_open=?4",
                params![project.as_str(), live, agent, open],
            )
            .unwrap();
    };
    set(1, Some(4), 0);
    assert!(!turn_running(&connection, &project).unwrap());
    set(1, None, 1);
    assert!(!turn_running(&connection, &project).unwrap());
    set(0, Some(4), 1);
    assert!(!turn_running(&connection, &project).unwrap());
    set(1, Some(4), 1);
    assert!(turn_running(&connection, &project).unwrap());
    assert!(!turn_running(&connection, &ProjectId::new("other").unwrap()).unwrap());
}

/// Inserts a working app-spawned sub-agent with team tools, started by `lead` or by the
/// master when none. Takes the fixture, its role and the lead. Returns its id.
fn worker(fixture: &Fixture, role: &str, lead: Option<i64>) -> i64 {
    let agent = named_sub(fixture, role, "working");
    fixture
        .connection
        .execute(
            "UPDATE team_agents SET parent=?2, tools=1, mode='app_spawned', stopped_ms=NULL WHERE id=?1",
            params![agent, lead],
        )
        .unwrap();
    agent
}

/// Stores the fixture's setup as the engine does, for the team changes that read it.
fn store_setup(fixture: &Fixture) {
    fixture
        .connection
        .execute(
            "INSERT INTO project_setups VALUES (?1,?2)",
            params![PROJECT, serde_json::to_string(&fixture.setup).unwrap()],
        )
        .unwrap();
}

/// A lead starts a worker only while its depth under the master is below the owner's
/// Sub-agent depth setting (N1): under each setting 1, 2 and 3 a lead at that depth or
/// deeper is refused in plain words naming the setting, a shallower one passes that check,
/// and no refused start leaves a task queued.
#[test]
fn delegate_refuses_past_sub_agent_depth() {
    let mut fixture = Fixture::new();
    store_setup(&fixture);
    let first = worker(&fixture, "notes-lead", None);
    let second = worker(&fixture, "notes-worker", Some(first));
    let third = worker(&fixture, "notes-helper", Some(second));
    let access = ModelAccess::default();
    let setup = fixture.setup.clone();
    let landing = fixture.base.clone();
    let answering = Answering {
        access: &access,
        stored: &setup,
        landing: &landing,
        agents: &landing,
        events: None,
        host: None,
    };
    let project = ProjectId::new(PROJECT).unwrap();
    for limit in crate::team::DEPTH_CHOICES {
        let tx = fixture.connection.transaction().unwrap();
        let outcome = crate::team::execute(
            &tx,
            &access,
            &crate::team::TeamAction::SubAgentDepth {
                project_id: project.clone(),
                depth: limit,
            },
        )
        .unwrap();
        assert_eq!(outcome, crate::protocol::Outcome::Applied);
        tx.commit().unwrap();
        assert_eq!(
            crate::team::nesting(&fixture.connection, &project)
                .unwrap()
                .depth,
            limit
        );
        let reason = if limit == 1 {
            "only the master starts sub-agents"
        } else {
            "workers this deep cannot start more of their own"
        };
        for (depth, lead) in [(1, first), (2, second), (3, third)] {
            assert_eq!(
                crate::team::agent_depth(&fixture.connection, lead).unwrap(),
                depth
            );
            let (text, refused) = answer_sub(
                &mut fixture.connection,
                &answering,
                lead,
                Call::Spawn {
                    task: "Write the notes page.".into(),
                    harness: None,
                    role: None,
                    mode: None,
                },
            )
            .unwrap()
            .unwrap();
            if depth >= limit {
                assert!(refused, "depth {depth} under setting {limit}");
                assert_eq!(
                    text,
                    format!(
                        "Not started: the owner's Sub-agent depth setting is {limit}, so {reason}. Do the work yourself or tell the master."
                    )
                );
            } else {
                assert!(
                    !text.contains("Sub-agent depth setting"),
                    "depth {depth} under setting {limit}: {text}"
                );
            }
        }
    }
    let queued: i64 = fixture
        .connection
        .query_row(
            "SELECT COUNT(*) FROM team_tasks WHERE parent IS NOT NULL",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(queued, 0, "a refused start queues nothing");
}

/// A lead's `stop` reaches only the workers it started (N2): a sibling lead's worker and
/// the sibling itself are refused by name and keep working, while its own worker stops,
/// and its status lists only its own workers.
#[test]
fn lead_tools_reach_only_its_workers() {
    let mut fixture = Fixture::new();
    store_setup(&fixture);
    let lead = worker(&fixture, "notes-lead", None);
    let mine = worker(&fixture, "notes-page", Some(lead));
    let sibling = worker(&fixture, "search-lead", None);
    let theirs = worker(&fixture, "search-index", Some(sibling));
    let access = ModelAccess::default();
    let setup = fixture.setup.clone();
    let landing = fixture.base.clone();
    let answering = Answering {
        access: &access,
        stored: &setup,
        landing: &landing,
        agents: &landing,
        events: None,
        host: None,
    };
    let status_of = |connection: &Connection, agent: i64| -> String {
        connection
            .query_row(
                "SELECT status FROM team_agents WHERE id=?1",
                [agent],
                |row| row.get(0),
            )
            .unwrap()
    };
    for name in ["@search-index", "@search-lead"] {
        let (text, refused) = answer_sub(
            &mut fixture.connection,
            &answering,
            lead,
            Call::Stop { agent: name.into() },
        )
        .unwrap()
        .unwrap();
        assert!(refused, "{name}: {text}");
        assert_eq!(
            text,
            format!(
                "Not stopped: no agent you lead is called {name}. Call {}() for their names.",
                tool::INSPECT_WORK_TOOL
            )
        );
    }
    assert_eq!(status_of(&fixture.connection, theirs), "working");
    assert_eq!(status_of(&fixture.connection, sibling), "working");

    let (listed, _) =
        lead_status(&fixture.connection, &ProjectId::new(PROJECT).unwrap(), lead).unwrap();
    assert!(listed.starts_with("Your workers:\n"), "{listed}");
    assert!(listed.contains("notes-page"), "{listed}");
    for other in ["search-index", "search-lead", "notes-lead"] {
        assert!(!listed.contains(other), "{other}: {listed}");
    }

    let (text, refused) = answer_sub(
        &mut fixture.connection,
        &answering,
        lead,
        Call::Stop {
            agent: "@notes-page".into(),
        },
    )
    .unwrap()
    .unwrap();
    assert!(!refused, "{text}");
    assert!(text.starts_with("Stopping "), "{text}");
    assert!(crate::team::owner_stopped(&fixture.connection, mine).unwrap());
    for other in [theirs, sibling, lead] {
        assert!(!crate::team::owner_stopped(&fixture.connection, other).unwrap());
    }
}

const LEAD: i64 = 51;
const WORKER: i64 = 52;

/// Runs git as the owner would in a test: an isolated home, no system or global config
/// and a fixed identity. Takes the home, the folder and the arguments. Returns its
/// trimmed standard output; a failing git fails the test.
fn git_in(home: &Path, folder: &Path, arguments: &[&str]) -> String {
    let output = crate::spawn_gate::output(
        Command::new("/usr/bin/git")
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("LC_ALL", "C")
            .args(["-c", "user.email=agent@local", "-c", "user.name=agent"])
            .arg("-C")
            .arg(folder)
            .args(arguments)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
    )
    .unwrap();
    assert!(
        output.status.success(),
        "git {arguments:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

/// A managed native Codex account in the project's pool, signed in only when its sign-in
/// operations say so. Takes nothing. Returns the access a lead's delegation is admitted
/// against.
fn codex_pool() -> ModelAccess {
    use crate::config::{
        AccountPool, ModelAuthentication, ModelConnection, ModelEndpoint, ModelReference,
    };
    use crate::protocol::CredentialRef;
    let reference = |value: &str| CredentialRef::new(value).unwrap();
    ModelAccess {
        accounts: vec![reference("local-account")],
        connections: vec![ModelConnection {
            id: reference("codex-managed"),
            provider: reference("openai"),
            harness: reference("codex"),
            protocol: reference("native"),
            account_ref: reference("local-account"),
            endpoint: ModelEndpoint::Native {},
            model: ModelReference {
                id: "gpt-5.5".into(),
                author: reference("openai"),
            },
            auth: ModelAuthentication::NativeManaged {
                profile_ref: reference(CODEX_PROFILE),
            },
        }],
        pools: vec![AccountPool {
            id: reference("team-pool"),
            name: None,
            members: vec![reference("codex-managed")],
            projects: vec![reference(PROJECT)],
            roles: vec![reference("project_leader")],
            fallback_allowed: false,
            provider_quota_policy: Default::default(),
        }],
    }
}

const CODEX_PROFILE: &str = "codex-managed-profile";

/// Records one sign-in operation of the pool's Codex profile as the native sign-in flow
/// stores it. Takes the fixture, the operation's generation and its phase.
fn codex_sign_in(fixture: &Fixture, generation: i64, phase: &str) {
    fixture
        .connection
        .execute(
            "INSERT INTO native_auth_operations(id,profile_key,generation,phase,request,fact)
             VALUES (?1,?2,?3,?4,X'01',X'01')",
            params![
                format!("operation-{generation}"),
                crate::memory::hash(CODEX_PROFILE.as_bytes()),
                generation,
                phase
            ],
        )
        .unwrap();
}

/// Records a team agent of the project as the runtime does. Takes the fixture, the id,
/// the harness, the status and its lead, None for a lead of the master.
fn team_agent(fixture: &Fixture, id: i64, harness: &str, status: &str, parent: Option<i64>) {
    fixture
        .connection
        .execute(
            "INSERT INTO team_agents(id,project,role,harness,tools,status,created_ms,heartbeat_ms,parent)
             VALUES (?1,?2,'sub',?3,1,?4,?5,?5,?6)",
            params![id, PROJECT, harness, status, NOW, parent],
        )
        .unwrap();
}

/// Answers one call of a sub-agent as the runtime's mailbox serving does. Takes the
/// fixture, the access, the folder of agents' own folders, the caller and the call.
/// Returns the text and whether it is a refusal.
fn lead_call(
    fixture: &mut Fixture,
    access: &ModelAccess,
    agents: &Path,
    caller: i64,
    call: Call,
) -> (String, bool) {
    let answering = Answering {
        access,
        stored: &fixture.setup,
        landing: &fixture.base,
        agents,
        events: None,
        host: None,
    };
    answer_sub(&mut fixture.connection, &answering, caller, call)
        .unwrap()
        .expect("a lead's work calls are answered here")
}

fn delegate(task: &str) -> Call {
    Call::Spawn {
        task: task.into(),
        harness: None,
        role: None,
        mode: None,
    }
}

fn tasks(fixture: &Fixture) -> Vec<(i64, Option<i64>)> {
    let mut statement = fixture
        .connection
        .prepare("SELECT id,parent FROM team_tasks ORDER BY id")
        .unwrap();
    statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

/// A lead's delegation starts a worker only of a harness kind with a signed-in account
/// (feat/coord-spawn 8a85b292): a pool Codex profile with no sign-in, or whose latest
/// sign-in observed no account, is refused with no task queued; once its latest sign-in
/// observed an account the same delegation is queued under the lead.
#[test]
fn a_lead_delegate_is_refused_for_a_kind_with_no_signed_in_account() {
    let mut fixture = Fixture::new();
    let access = codex_pool();
    let agents = fixture.base.join("team/agents");
    team_agent(&fixture, LEAD, "claude-code", "working", None);
    let refused = "Not started: no account in this project's pool that runs codex is signed in.";
    let (answer, error) = lead_call(
        &mut fixture,
        &access,
        &agents,
        LEAD,
        delegate("Fix the parser"),
    );
    assert!(error && answer.starts_with(refused), "{answer}");
    assert_eq!(tasks(&fixture), vec![]);
    codex_sign_in(&fixture, 1, "unauthenticated_observed");
    let (answer, error) = lead_call(
        &mut fixture,
        &access,
        &agents,
        LEAD,
        delegate("Fix the parser"),
    );
    assert!(error && answer.starts_with(refused), "{answer}");
    assert_eq!(tasks(&fixture), vec![]);
    codex_sign_in(&fixture, 2, "authenticated_observed");
    let (answer, error) = lead_call(
        &mut fixture,
        &access,
        &agents,
        LEAD,
        delegate("Fix the parser"),
    );
    assert!(!error, "{answer}");
    let queued = tasks(&fixture);
    assert_eq!(queued.len(), 1);
    assert_eq!(queued[0].1, Some(LEAD));
    assert!(
        answer.starts_with(&format!(
            "Task {} is queued for a codex worker of yours",
            queued[0].0
        )),
        "{answer}"
    );
    codex_sign_in(&fixture, 3, "starting");
    let (answer, error) = lead_call(
        &mut fixture,
        &access,
        &agents,
        LEAD,
        delegate("Fix the lexer"),
    );
    assert!(
        error && answer.starts_with(refused),
        "the latest sign-in decides: {answer}"
    );
    assert_eq!(tasks(&fixture).len(), 1);
}

/// A lead whose harness kind the owner set to Native sub-agents is told to start them in
/// its own agent team, and no task is queued (feat/coord-spawn 372ee630).
#[test]
fn native_mode_lead_is_told_to_use_its_agent_team() {
    use crate::coord::settings::{HarnessKind, SubagentMode, set_mode};
    let mut fixture = Fixture::new();
    let access = codex_pool();
    let agents = fixture.base.join("team/agents");
    codex_sign_in(&fixture, 1, "authenticated_observed");
    fixture
        .connection
        .execute("INSERT INTO projects VALUES (?1,'{}')", [PROJECT])
        .unwrap();
    set_mode(
        &fixture.connection,
        &ProjectId::new(PROJECT).unwrap(),
        HarnessKind::ClaudeCode,
        SubagentMode::Native,
    )
    .unwrap();
    team_agent(&fixture, LEAD, "claude-code", "working", None);
    let answer = lead_call(
        &mut fixture,
        &access,
        &agents,
        LEAD,
        delegate("Fix the parser"),
    );
    assert_eq!(answer, (NATIVE_LEAD_REFUSED.to_owned(), true));
    assert_eq!(tasks(&fixture), vec![]);
    team_agent(&fixture, 61, "codex", "working", None);
    let (answer, error) = lead_call(
        &mut fixture,
        &access,
        &agents,
        61,
        delegate("Fix the parser"),
    );
    assert!(!error, "a Codex lead keeps app-spawned workers: {answer}");
    assert_eq!(tasks(&fixture).len(), 1);
}

/// A lead already running the owner's Max teammates per harness reads the cap sentence and
/// no task is queued (feat/coord-spawn 90657f80, DEC-104 K8).
#[test]
fn lead_delegate_is_refused_at_max_teammates() {
    let mut fixture = Fixture::new();
    let access = codex_pool();
    let agents = fixture.base.join("team/agents");
    codex_sign_in(&fixture, 1, "authenticated_observed");
    fixture
        .connection
        .execute(
            "INSERT INTO team_caps(project,teammates) VALUES (?1,1)",
            [PROJECT],
        )
        .unwrap();
    team_agent(&fixture, LEAD, "claude-code", "working", None);
    team_agent(&fixture, WORKER, "codex", "working", Some(LEAD));
    let (answer, error) = lead_call(
        &mut fixture,
        &access,
        &agents,
        LEAD,
        delegate("Fix the parser"),
    );
    assert!(error, "{answer}");
    assert!(
        answer.starts_with("Not started: The owner allows at most 1 sub-agents per harness, and this harness already runs 1."),
        "{answer}"
    );
    assert_eq!(tasks(&fixture), vec![]);
    fixture
        .connection
        .execute(
            "UPDATE team_agents SET status='stopped' WHERE id=?1",
            [WORKER],
        )
        .unwrap();
    let (answer, error) = lead_call(
        &mut fixture,
        &access,
        &agents,
        LEAD,
        delegate("Fix the parser"),
    );
    assert!(!error, "a finished worker frees its place: {answer}");
    assert_eq!(tasks(&fixture).len(), 1);
}

/// A lead's `merge` reaches only its own workers: a worker of another lead is not one of
/// its sub-agents, whatever it is called (feat/coord-spawn 372ee630).
#[test]
fn merge_refuses_a_worker_of_another_lead() {
    let mut fixture = Fixture::new();
    let access = codex_pool();
    let agents = fixture.base.join("team/agents");
    team_agent(&fixture, LEAD, "claude-code", "working", None);
    team_agent(&fixture, 61, "claude-code", "working", None);
    team_agent(&fixture, WORKER, "codex", "idle", Some(LEAD));
    let answer = lead_call(
        &mut fixture,
        &access,
        &agents,
        61,
        Call::Merge {
            agent: WORKER.to_string(),
        },
    );
    assert_eq!(
        answer,
        (
            format!(
                "Not merged: none of your own sub-agents is called {WORKER}. Call {}() for their names.",
                tool::INSPECT_WORK_TOOL
            ),
            true
        )
    );
}

/// Makes the fixture's project folder a real git repository with one commit on `main`, the
/// engine's state folder excluded and its worktrees folder made. Takes the fixture.
/// Returns the git home and the project folder.
fn owner_repository(fixture: &Fixture) -> (PathBuf, PathBuf) {
    let (home, project) = (fixture.base.join("git-home"), fixture.base.join("project"));
    std::fs::create_dir(&home).unwrap();
    git_in(&home, &project, &["init", "-q", "-b", "main"]);
    git_in(&home, &project, &["config", "user.name", "Repo Owner"]);
    git_in(
        &home,
        &project,
        &["config", "user.email", "owner@example.invalid"],
    );
    std::fs::write(project.join("README"), "one\n").unwrap();
    git_in(&home, &project, &["add", "README"]);
    git_in(&home, &project, &["commit", "-q", "-m", "Start"]);
    std::fs::write(project.join(".git/info/exclude"), ".crazyagent/\n").unwrap();
    std::fs::create_dir_all(project.join(".crazyagent/worktrees")).unwrap();
    (home, project)
}

/// A project that is a real git repository with a lead of the master in its own clone on
/// `crazyagent/lead` and one worker of it in its own clone on `crazyagent/lead--fix`, both
/// recorded as the runtime records a team agent's worktree.
struct Clones {
    fixture: Fixture,
    home: PathBuf,
    project: PathBuf,
    agents: PathBuf,
    lead: PathBuf,
    worker: PathBuf,
    worker_branch: String,
}

impl Clones {
    fn new() -> Self {
        let fixture = Fixture::new();
        let (home, project) = owner_repository(&fixture);
        let db = crate::landing::open(&fixture.base).unwrap();
        let worktrees = project.join(".crazyagent/worktrees");
        let lead = worktrees.join(format!("agent-{LEAD}"));
        let branch = crate::landing::claim_task_branch(&db, &project, &lead, "lead").unwrap();
        crate::landing::add_agent_clone_on(&project, &lead, None, &branch).unwrap();
        team_agent(&fixture, LEAD, "claude-code", "working", None);
        let clones = Self {
            agents: fixture.base.join("team/agents"),
            worker: worktrees.join(format!("agent-{WORKER}")),
            worker_branch: String::new(),
            fixture,
            home,
            project,
            lead,
        };
        clones.record(&db, LEAD, &clones.lead);
        let base = crate::landing::agent_tip(
            &db,
            &clones.project,
            &clones.lead,
            &crate::agent_git::git_dir(&clones.lead),
        )
        .unwrap();
        let worker_branch = crate::landing::claim_sub_agent_branch(
            &db,
            &clones.project,
            &clones.worker,
            &branch,
            Some(&clones.lead),
            "fix",
        )
        .unwrap()
        .expect("the lead's task branch carries sub-agent branches");
        assert_eq!(worker_branch, "crazyagent/lead--fix");
        crate::landing::add_agent_clone_on(
            &clones.project,
            &clones.worker,
            Some(&base),
            &worker_branch,
        )
        .unwrap();
        team_agent(&clones.fixture, WORKER, "codex", "working", Some(LEAD));
        clones.record(&db, WORKER, &clones.worker);
        Self {
            worker_branch,
            ..clones
        }
    }

    fn record(&self, db: &Connection, agent: i64, worktree: &Path) {
        let git_dir = crate::agent_git::git_dir(worktree);
        self.fixture
            .connection
            .execute(
                "INSERT INTO team_worktrees(agent,project,repository,worktree,git_dir) VALUES (?1,?2,?3,?4,?5)",
                params![
                    agent,
                    PROJECT,
                    self.project.to_str().unwrap(),
                    worktree.to_str().unwrap(),
                    git_dir.to_str().unwrap()
                ],
            )
            .unwrap();
        crate::landing::record_worktree_start(db, &self.project, worktree, &git_dir).unwrap();
    }

    fn git(&self, folder: &Path, arguments: &[&str]) -> String {
        git_in(&self.home, folder, arguments)
    }

    fn commit(&self, folder: &Path, path: &str, text: &str) {
        std::fs::write(folder.join(path), text).unwrap();
        self.git(folder, &["add", path]);
        self.git(folder, &["commit", "-q", "-m", path]);
    }

    fn call(&mut self, caller: i64, call: Call) -> (String, bool) {
        let access = ModelAccess::default();
        lead_call(&mut self.fixture, &access, &self.agents, caller, call)
    }

    fn finish(&mut self, work: i64, version: i64) -> (String, bool) {
        self.call(
            LEAD,
            Call::FinishWork {
                work,
                version,
                outcome: crate::coord::Finish::Complete,
                evidence: "Merged and checked.".into(),
            },
        )
    }

    fn started(&mut self) -> i64 {
        let (answer, error) = self.call(
            LEAD,
            Call::StartWork {
                title: "Lead the fix".into(),
                claims: vec!["fix.txt".into()],
            },
        );
        assert!(!error, "{answer}");
        answer
            .strip_prefix("Started work #")
            .and_then(|rest| rest.split_once(' '))
            .and_then(|(id, _)| id.parse().ok())
            .unwrap_or_else(|| panic!("no work id in {answer}"))
    }

    fn worker_set(&self) -> crate::landing::ChangeSetId {
        let db = crate::landing::open(&self.fixture.base).unwrap();
        crate::landing::record_turn_end(
            &db,
            &crate::landing::Workspace {
                project: ProjectId::new(PROJECT).unwrap(),
                agent: crate::landing::ChangeAgent {
                    kind: crate::landing::AgentKind::Sub,
                    id: WORKER.to_string(),
                    label: format!("Agent {WORKER}"),
                },
                repository: self.project.clone(),
                worktree: self.worker.clone(),
                git_dir: crate::agent_git::git_dir(&self.worker),
            },
            &crate::landing::TurnEnd {
                outcome: crate::landing::TurnOutcome::Succeeded,
                turn: "turn-1".into(),
                prompt_line: "Fix it".into(),
            },
        )
        .unwrap()
        .expect("the worker's commit is work for review")
    }

    fn set_state(&self, set: &crate::landing::ChangeSetId) -> (String, Option<String>) {
        self.fixture
            .connection
            .query_row(
                "SELECT state,landed FROM landing_change_sets WHERE id=?1",
                [set.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap()
    }
}

/// A lead cannot report its work Complete while a worker's branch holds a commit its task
/// branch does not: finish_work names the worker's branch until the lead merges it, then
/// completes (feat/coord-spawn 4f372a7e).
#[test]
fn finish_work_waits_for_an_unmerged_sub_agent() {
    let mut clones = Clones::new();
    let work = clones.started();
    clones.commit(&clones.worker.clone(), "fix.txt", "fixed\n");
    let (answer, error) = clones.finish(work, 1);
    assert_eq!(
        (answer.as_str(), error),
        (
            "Not finished: merge these into your branch first, then finish again: crazyagent/lead--fix.",
            true
        )
    );
    let (lead, worker, branch) = (
        clones.lead.clone(),
        clones.worker.clone(),
        clones.worker_branch.clone(),
    );
    clones.git(&lead, &["fetch", "-q", worker.to_str().unwrap(), &branch]);
    clones.git(
        &lead,
        &["merge", "--no-ff", "-q", "--no-edit", "FETCH_HEAD"],
    );
    let (answer, error) = clones.finish(work, 1);
    assert_eq!(
        (answer.as_str(), error),
        (
            format!("Work #{work} is complete at version 2.").as_str(),
            false
        )
    );
}

/// A lead's `merge` of its finished worker in its own clone runs git with the lead's own
/// Seatbelt profile, makes a merge commit on the lead's task branch and closes the
/// worker's change set as Accepted at that commit (feat/coord-spawn 372ee630, 4f372a7e).
#[cfg(target_os = "macos")]
#[test]
fn merge_from_a_lead_merges_its_finished_worker_and_closes_its_change_set() {
    let mut clones = Clones::new();
    let state = clones.fixture.base.join("team");
    let home = super::super::runtime::agent_home(&state, LEAD);
    let tmp = clones.agents.join(format!("agent-{LEAD}")).join("tmp");
    let profiles = state.join("profiles");
    for folder in [&home, &tmp, &profiles] {
        std::fs::create_dir_all(folder).unwrap();
    }
    crate::sandbox::ProjectSandbox::new(
        &clones.project,
        &home,
        &tmp,
        crate::sandbox::SandboxNetwork::Only(Vec::new()),
    )
    .unwrap()
    .confine_to_clone(&clones.lead)
    .unwrap()
    .install(&profiles.join(format!("agent-{LEAD}.sb")))
    .unwrap();
    clones.commit(&clones.worker.clone(), "fix.txt", "fixed\n");
    let set = clones.worker_set();
    let merge = || Call::Merge {
        agent: WORKER.to_string(),
    };
    let (answer, error) = clones.call(LEAD, merge());
    assert_eq!(
        (answer.as_str(), error),
        (
            format!(
                "Not merged: Sub-agent {WORKER} is still working. Merge it once it calls {}().",
                tool::FINISH_WORK_TOOL
            )
            .as_str(),
            true
        )
    );
    clones
        .fixture
        .connection
        .execute("UPDATE team_agents SET status='idle' WHERE id=?1", [WORKER])
        .unwrap();
    let (answer, error) = clones.call(LEAD, merge());
    assert!(!error, "{answer}");
    let lead = clones.lead.clone();
    let head = clones.git(&lead, &["rev-parse", "HEAD"]);
    assert_eq!(
        answer,
        format!(
            "Merged Sub-agent {WORKER}'s branch crazyagent/lead--fix into your task branch as {}. Nothing was pushed.",
            &head[..12]
        )
    );
    assert_eq!(
        clones.git(&lead, &["symbolic-ref", "--short", "HEAD"]),
        "crazyagent/lead"
    );
    let parents = clones.git(&lead, &["rev-list", "--parents", "-n", "1", "HEAD"]);
    assert_eq!(parents.split(' ').count(), 3, "a merge commit: {parents}");
    assert_eq!(clones.git(&lead, &["show", "HEAD:fix.txt"]), "fixed");
    assert_eq!(clones.set_state(&set), ("accepted".to_owned(), Some(head)));
    let (answer, error) = clones.call(LEAD, merge());
    assert_eq!(
        (answer.as_str(), error),
        (
            format!("Nothing to merge: Sub-agent {WORKER}'s branch crazyagent/lead--fix is already in your task branch.").as_str(),
            false
        )
    );
}

/// A lead in a linked worktree of a Sandboxed project lands its finished worker's waiting
/// change set on its own task branch as one commit, as the owner's Accept does, and the
/// set is Accepted at that commit (feat/coord-spawn 4f372a7e).
#[test]
fn lead_merge_lands_a_linked_sub_agent_change_set() {
    let mut fixture = Fixture::new();
    let (home, project) = owner_repository(&fixture);
    let db = crate::landing::open(&fixture.base).unwrap();
    let worktrees = project.join(".crazyagent/worktrees");
    let (lead, worker) = (
        worktrees.join(format!("agent-{LEAD}")),
        worktrees.join(format!("agent-{WORKER}")),
    );
    let branch = crate::landing::claim_task_branch(&db, &project, &lead, "lead").unwrap();
    assert_eq!(branch, "crazyagent/lead");
    git_in(
        &home,
        &project,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            &branch,
            lead.to_str().unwrap(),
            "HEAD",
        ],
    );
    git_in(
        &home,
        &project,
        &[
            "worktree",
            "add",
            "-q",
            "--detach",
            worker.to_str().unwrap(),
            "HEAD",
        ],
    );
    team_agent(&fixture, LEAD, "claude-code", "working", None);
    team_agent(&fixture, WORKER, "codex", "idle", Some(LEAD));
    for (agent, worktree) in [(LEAD, &lead), (WORKER, &worker)] {
        let git_dir = project
            .join(".git/worktrees")
            .join(format!("agent-{agent}"));
        fixture
            .connection
            .execute(
                "INSERT INTO team_worktrees(agent,project,repository,worktree,git_dir) VALUES (?1,?2,?3,?4,?5)",
                params![
                    agent,
                    PROJECT,
                    project.to_str().unwrap(),
                    worktree.to_str().unwrap(),
                    git_dir.to_str().unwrap()
                ],
            )
            .unwrap();
        crate::landing::record_worktree_start(&db, &project, worktree, &git_dir).unwrap();
    }
    std::fs::write(worker.join("fix.txt"), "fixed\n").unwrap();
    let set = crate::landing::record_turn_end(
        &db,
        &crate::landing::Workspace {
            project: ProjectId::new(PROJECT).unwrap(),
            agent: crate::landing::ChangeAgent {
                kind: crate::landing::AgentKind::Sub,
                id: WORKER.to_string(),
                label: format!("Agent {WORKER}"),
            },
            repository: project.clone(),
            worktree: worker.clone(),
            git_dir: project
                .join(".git/worktrees")
                .join(format!("agent-{WORKER}")),
        },
        &crate::landing::TurnEnd {
            outcome: crate::landing::TurnOutcome::Succeeded,
            turn: "turn-1".into(),
            prompt_line: "Fix it".into(),
        },
    )
    .unwrap()
    .expect("the worker's file is work for review");
    let access = ModelAccess::default();
    let agents = fixture.base.join("team/agents");
    let (answer, error) = lead_call(
        &mut fixture,
        &access,
        &agents,
        LEAD,
        Call::Merge {
            agent: WORKER.to_string(),
        },
    );
    assert!(!error, "{answer}");
    let landed = git_in(&home, &project, &["rev-parse", &branch]);
    assert_eq!(
        answer,
        format!(
            "Landed on your branch crazyagent/lead as {}.",
            &landed[..12]
        )
    );
    assert_eq!(
        git_in(&home, &project, &["show", &format!("{branch}:fix.txt")]),
        "fixed"
    );
    assert_eq!(
        git_in(&home, &project, &["rev-parse", "main"]),
        git_in(&home, &project, &["rev-parse", &format!("{landed}^")])
    );
    let state: (String, Option<String>) = fixture
        .connection
        .query_row(
            "SELECT state,landed FROM landing_change_sets WHERE id=?1",
            [set.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(state, ("accepted".to_owned(), Some(landed)));
}
