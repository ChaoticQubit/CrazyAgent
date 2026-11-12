#![cfg(unix)]

mod pane_support;

use pane_support::{LEAD_PANE, PaneCallers};
use rusqlite::{Connection, params};
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::Command;

const MATE_PANE: &str = "%7";
const LEAD_SESSION: &str = "lead-session";
const MATE_SESSION: &str = "mate-session";

struct Team {
    _root: tempfile::TempDir,
    config: PathBuf,
    mailbox: PathBuf,
    callers: PaneCallers,
    db: Connection,
    lead: i64,
    mate: i64,
}

impl Team {
    fn new() -> Self {
        let root = tempfile::tempdir_in(std::fs::canonicalize("/tmp").unwrap()).unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let config = root.path().join("home");
        let team = config.join("teams").join("session-1");
        std::fs::create_dir_all(&team).unwrap();
        std::fs::write(
            team.join("config.json"),
            serde_json::json!({
                "leadSessionId": LEAD_SESSION,
                "members": [
                    {"name": "team-lead", "tmuxPaneId": "leader"},
                    {"name": "greet", "tmuxPaneId": MATE_PANE, "cwd": root.path()},
                ],
            })
            .to_string(),
        )
        .unwrap();
        let (mailbox, callers) = PaneCallers::new(root.path(), &[(MATE_PANE, "greet")]);
        let part = mailbox.join(crazyagent::coord::relay::LEAD_PART);
        std::fs::create_dir(&part).unwrap();
        std::fs::set_permissions(&part, std::fs::Permissions::from_mode(0o700)).unwrap();
        let db = Connection::open(root.path().join("engine.db")).unwrap();
        db.execute_batch("CREATE TABLE IF NOT EXISTS projects (id TEXT PRIMARY KEY, body TEXT NOT NULL); INSERT INTO projects VALUES ('alpha','{}');")
            .unwrap();
        crazyagent::team::initialize(&db).unwrap();
        crazyagent::project_setup::initialize(&db).unwrap();
        crazyagent::coord::ensure(&db).unwrap();
        db.execute(
            "INSERT INTO team_agents(project,role,harness,tools,status,created_ms,heartbeat_ms)
             VALUES ('alpha','sub_agent','claude-code',1,'working',0,0)",
            [],
        )
        .unwrap();
        let lead = db.last_insert_rowid();
        db.execute(
            "INSERT INTO team_agents(project,role,harness,tools,status,created_ms,heartbeat_ms,parent,pane)
             VALUES ('alpha','teammate','claude-code',1,'working',0,0,?1,?2)",
            params![lead, MATE_PANE],
        )
        .unwrap();
        let mate = db.last_insert_rowid();
        Self {
            _root: root,
            config,
            mailbox,
            callers,
            db,
            lead,
            mate,
        }
    }

    fn hook(&self, event: &str, pane: &str, input: serde_json::Value) -> String {
        let mut child = self.callers.run(
            pane,
            Command::new(env!("CARGO_BIN_EXE_crazyagent"))
                .arg("coord-hook")
                .arg(event)
                .arg(&self.mailbox)
                .env("CLAUDE_CONFIG_DIR", &self.config)
                .env("TMUX_PANE", pane),
        );
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.to_string().as_bytes())
            .unwrap();
        let done = child.wait_with_output().unwrap();
        assert!(
            done.status.success(),
            "{}",
            String::from_utf8_lossy(&done.stderr)
        );
        String::from_utf8(done.stdout).unwrap()
    }

    /// Whether the engine shows the teammate's row Idle.
    fn resting(&self) -> bool {
        let status: String = self
            .db
            .query_row(
                "SELECT status FROM team_agents WHERE id=?1",
                [self.mate],
                |row| row.get(0),
            )
            .unwrap();
        status == "idle"
    }

    /// Sends the teammate one note from the lead. Takes the body. Returns its id.
    fn note(&self, body: &str) -> i64 {
        crazyagent::coord::send(
            &self.db,
            crazyagent::coord::NewMessage {
                project: crazyagent::protocol::ProjectId::new("alpha").unwrap(),
                from: crazyagent::coord::Party::Agent(self.lead),
                to: crazyagent::coord::Party::Agent(self.mate),
                kind: crazyagent::coord::MessageKind::Note,
                question: None,
                work: None,
                reply_to: None,
                body: body.to_owned(),
                idempotency_key: None,
            },
            0,
        )
        .unwrap()
    }

    fn marks(&self) -> Vec<String> {
        [
            self.mailbox.clone(),
            self.mailbox.join(crazyagent::coord::relay::LEAD_PART),
        ]
        .iter()
        .flat_map(|folder| std::fs::read_dir(folder).unwrap())
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .filter(|name| name.ends_with(".idle"))
        .collect()
    }
}

fn session(id: &str) -> serde_json::Value {
    serde_json::json!({"session_id": id})
}

#[test]
fn a_native_teammate_rests_when_its_turn_ends_and_works_again_on_its_next_tool_call_or_prompt() {
    let team = Team::new();
    assert_eq!(team.hook("stop", MATE_PANE, session(MATE_SESSION)), "");
    assert!(
        team.resting(),
        "a teammate that stops with nothing to read rests"
    );
    assert_eq!(team.hook("stop", MATE_PANE, session(MATE_SESSION)), "");
    assert!(team.resting(), "a second stop keeps it resting");

    team.hook(
        "post-tool",
        MATE_PANE,
        serde_json::json!({
            "session_id": MATE_SESSION,
            "tool_name": "Bash",
            "tool_input": {"command": "ls"},
        }),
    );
    assert!(!team.resting(), "a tool call shows it works again");

    team.hook("stop", MATE_PANE, session(MATE_SESSION));
    assert!(team.resting());
    team.hook(
        "prompt",
        MATE_PANE,
        serde_json::json!({"session_id": MATE_SESSION, "prompt": "Next"}),
    );
    assert!(!team.resting(), "a prompt starts its next turn");
}

#[test]
fn a_teammate_handed_a_waiting_message_at_its_stop_does_not_rest_and_the_lead_never_does() {
    let team = Team::new();
    team.note("Also add a docstring.");
    let blocked = team.hook("stop", MATE_PANE, session(MATE_SESSION));
    let answer: serde_json::Value = serde_json::from_str(blocked.trim()).unwrap();
    assert_eq!(answer["decision"], "block", "{blocked}");
    assert!(
        answer["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("Also add a docstring.")),
        "{blocked}"
    );
    assert!(
        !team.resting(),
        "it keeps working on the message it was handed"
    );
    team.hook("stop", MATE_PANE, session(MATE_SESSION));
    assert!(team.resting(), "with nothing left it rests");

    let lead = Team::new();
    lead.hook("stop", "%3", session(LEAD_SESSION));
    lead.hook("stop", LEAD_PANE, session(MATE_SESSION));
    assert!(
        lead.marks().is_empty(),
        "the lead's own stop is the engine's to read, never a resting mark: {:?}",
        lead.marks()
    );
}
