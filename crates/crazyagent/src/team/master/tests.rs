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
        master_input_at(&fixture.connection, host.as_ref(), &project, text, now)
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
