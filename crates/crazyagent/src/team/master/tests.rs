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
        tool::serve_live(Cursor::new(input), &mut output, &served).unwrap();
        output
    });
    let access = ModelAccess::default();
    let setup = fixture.setup.clone();
    let landing = fixture.base.clone();
    let answering = Answering {
        access: &access,
        stored: &setup,
        landing: &landing,
        events: None,
        host: None,
    };
    let deadline = Instant::now() + Duration::from_secs(30);
    while !server.is_finished() {
        assert!(Instant::now() < deadline, "the server never finished");
        serve_calls(&mut fixture.connection, &answering, mailbox).unwrap();
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
                tool::TOOL_NAME,
                json!({"task": "Write page.md", "harness": "codex", "role": "@page-writer"}),
            ),
            tool_call(
                2,
                tool::TOOL_NAME,
                json!({"task": "Run the tests", "harness": "pi"}),
            ),
            tool_call(
                3,
                tool::MESSAGE_TOOL,
                json!({"agent": "@page-writer", "text": "Use the old header."}),
            ),
            tool_call(4, tool::STATUS_TOOL, json!({})),
            tool_call(5, tool::CHANGES_TOOL, json!({})),
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
    assert_eq!(text(&answers[2]), (NOT_WIRED, true));
    assert_eq!(text(&answers[3]), (NOT_WIRED, true));
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
        br#"{"tool":"delegate","task":"Write page.md","harness":"CODEX","role":null}"#,
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
        events: None,
        host: None,
    };
    assert_eq!(
        serve_calls(&mut fixture.connection, &answering, &mailbox).unwrap(),
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

#[test]
fn a_live_master_s_escalations_stop_at_the_hourly_cap() {
    let mut fixture = Fixture::new();
    let mailbox = fixture.base.join(tool::MAILBOX_FOLDER);
    std::fs::create_dir(&mailbox).unwrap();
    let project = fixture.setup.project_id.clone();
    for n in 1..ESCALATIONS_PER_HOUR_MAX {
        super::super::events::escalate(
            &fixture.connection,
            &project,
            &format!("Earlier question {n}"),
            now_ms(),
        )
        .unwrap()
        .unwrap();
    }
    let answers = serve_while(
        &mut fixture,
        &mailbox,
        vec![
            tool_call(
                1,
                tool::ESCALATE_TOOL,
                json!({"text": "May Ada delete the release branch?"}),
            ),
            tool_call(
                2,
                tool::ESCALATE_TOOL,
                json!({"text": "May Ada push to main?"}),
            ),
        ],
    );
    assert_eq!(text(&answers[0]), (ESCALATED, false));
    assert_eq!(text(&answers[1]), (ESCALATIONS_FULL, true));
    let escalations: i64 = fixture
        .connection
        .query_row(
            "SELECT COUNT(*) FROM team_notifications WHERE project=?1 AND kind='escalation'",
            [PROJECT],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(escalations, ESCALATIONS_PER_HOUR_MAX);
    assert_eq!(std::fs::read_dir(&mailbox).unwrap().count(), 0);
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
/// answers "running", and the running agent is still reached by its number.
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
    let answers = serve_while(
        &mut fixture,
        &mailbox,
        vec![
            tool_call(
                1,
                tool::WAKE_TOOL,
                json!({"agent": "@onboarding-fix", "text": "Fix the onboarding copy."}),
            ),
            tool_call(
                2,
                tool::WAKE_TOOL,
                json!({"agent": format!("Sub-agent {live}"), "text": "Stop."}),
            ),
        ],
    );
    assert_eq!(answers.len(), 2, "{answers:#?}");
    assert_eq!(
        text(&answers[0]),
        (
            format!("Waking {label}: it starts again in its own worktree and gets your text as its next task. The engine tells you when it ends.").as_str(),
            false
        )
    );
    assert_eq!(pinned(&fixture, finished).len(), 1);
    assert!(pinned(&fixture, live).is_empty());
    let (said, refused) = text(&answers[1]);
    assert!(refused && said.contains("is running"), "{said}");
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
            "Finished sub-agents you can wake with {}(agent, text), newest first:\n- {label}, Sub-agent {agent} (",
            tool::WAKE_TOOL
        )),
        "{told}"
    );
    let answers = serve_while(
        &mut fixture,
        &mailbox,
        vec![tool_call(
            1,
            tool::WAKE_TOOL,
            json!({"agent": format!("Sub-agent {agent}"), "text": "Add the footer."}),
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
            tool::WAKE_TOOL,
            json!({"agent": "@page-writer", "text": "You were interrupted, finish it."}),
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
