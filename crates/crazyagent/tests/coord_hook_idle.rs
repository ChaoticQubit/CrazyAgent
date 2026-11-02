#![cfg(unix)]

mod pane_support;

use pane_support::PaneCallers;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const MATE_PANE: &str = "%7";
const MATE_RESTS: &str = "00000000000000000000000000000008.idle";
const LEAD_SESSION: &str = "lead-session";
const MATE_SESSION: &str = "mate-session";

struct Team {
    _root: tempfile::TempDir,
    config: PathBuf,
    mailbox: PathBuf,
    callers: PaneCallers,
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
        let (mailbox, callers) = PaneCallers::new(root.path());
        Self {
            _root: root,
            config,
            mailbox,
            callers,
        }
    }

    fn hook(&self, event: &str, pane: &str, input: serde_json::Value) -> String {
        let mut child = self.callers.spawn(
            pane,
            Command::new(env!("CARGO_BIN_EXE_crazyagent"))
                .arg("coord-hook")
                .arg(event)
                .arg(&self.mailbox)
                .env("CLAUDE_CONFIG_DIR", &self.config)
                .env("TMUX_PANE", pane)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped()),
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

    fn resting(&self) -> bool {
        exists(&self.mailbox.join(MATE_RESTS))
    }

    fn marks(&self) -> Vec<String> {
        std::fs::read_dir(&self.mailbox)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .filter(|name| name.ends_with(".idle"))
            .collect()
    }
}

fn exists(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.is_file())
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
    std::fs::write(
        team.mailbox.join("00000000000000000000000000000005.inbox"),
        serde_json::json!({
            "id": 5,
            "to": MATE_PANE,
            "from": "Ada (@lead-a)",
            "kind": "note",
            "question": null,
            "work": null,
            "reply_to": null,
            "body": "Also add a docstring.",
        })
        .to_string(),
    )
    .unwrap();
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
    lead.hook("stop", MATE_PANE, session(LEAD_SESSION));
    assert!(
        lead.marks().is_empty(),
        "the lead's own stop is the engine's to read, never a resting mark: {:?}",
        lead.marks()
    );
}
