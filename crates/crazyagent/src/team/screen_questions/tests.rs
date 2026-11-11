use super::*;
use crate::harnesses::interactive::{InteractiveCommand, Transcript};
use crate::team::events::question;
use crate::terminal::tmux::{SOCKET, Tmux, installed};
use crate::terminal::{FOLDER, type_prompt};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

const PROJECT: &str = "screen-questions";
const WAIT: Duration = Duration::from_secs(20);
const CLAUDE_ASK: &str = " \u{2610} Sign-in\n\n Which sign-in should the app use?\n\n \u{276f} 1. OAuth\n      The provider's own sign-in page\n   2. API key\n      A key kept in project settings\n   3. Type something.\n\n Enter to select \u{b7} \u{2191}/\u{2193} to navigate \u{b7} Esc to cancel\n";
const CLAUDE_LINE: &str =
    "Which sign-in should the app use? | 1. OAuth | 2. API key | 3. Type something.";
const ASK: &str = r#"printf ' \342\230\220 Sign-in\n\n Which sign-in should the app use?\n\n \342\235\257 1. OAuth\n      The provider sign-in page\n   2. API key\n      A key kept in project settings\n   3. Type something.\n\n Enter to select \302\267 Tab/Arrow keys to navigate \302\267 Esc to cancel\n'
IFS= read -r answer
printf '\033[2J\033[H'
printf 'answer:%s\n' "$answer" > "$1"
printf 'thanks\n'
exec sleep 60
"#;
const PERMISSION: &str = r#"printf ' Bash command\n   cargo test\n Do you want to proceed?\n \342\235\257 1. Yes\n   2. Yes, and don'"'"'t ask again for cargo test commands\n   3. No, and tell Claude what to do differently (esc)\n'
exec sleep 60
"#;

struct Fixture {
    _root: tempfile::TempDir,
    base: PathBuf,
    tmux: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::Builder::new()
            .prefix("ca-screenq-")
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let base = root.path().canonicalize().unwrap();
        let tmux = installed(&base).expect("tmux must be installed on this machine");
        Self {
            _root: root,
            base,
            tmux,
        }
    }

    fn host(&self) -> Arc<dyn Host> {
        Arc::new(Tmux::open(Some(self.tmux.clone()), &self.base.join(FOLDER)))
    }

    /// Starts one master's terminal running a shell script with one argument.
    /// Inputs: the host, the team agent id, the script text and its argument.
    fn pane(&self, host: &Arc<dyn Host>, agent: i64, script: &str, argument: &str) {
        let path = self.base.join(format!("master-{agent}.sh"));
        std::fs::write(&path, script).unwrap();
        let command = InteractiveCommand {
            argv: vec![
                "/bin/sh".to_owned(),
                path.to_str().unwrap().to_owned(),
                argument.to_owned(),
            ],
            cwd: self.base.clone(),
            env: Vec::new(),
            transcript: Transcript::Unavailable,
        };
        host.spawn(&AgentId::team(agent).unwrap(), &command)
            .unwrap();
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

fn project() -> ProjectId {
    ProjectId::new(PROJECT).unwrap()
}

fn master(agent: i64) -> Watched {
    Watched {
        agent,
        project: project(),
        task: None,
    }
}

fn pending(id: u64, agent: i64) -> Pending {
    Pending {
        id,
        project: project(),
        agent,
        text: CLAUDE_LINE.to_owned(),
    }
}

fn screen(host: &Arc<dyn Host>, agent: i64) -> Vec<u8> {
    host.screen(&AgentId::team(agent).unwrap()).unwrap()
}

fn until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + WAIT;
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn only_a_harness_s_own_question_box_is_an_owner_question() {
    let codex_ask = "  Which database should the service use?\r\n\r\n\x1b[1m\u{203a} 1. Postgres\x1b[22m\r\n  2. SQLite\r\n  3. None of the above\r\n     Optionally, add details in notes (tab)\r\n\r\n  tab to add notes \u{b7} enter to submit answer \u{b7} esc to interrupt\r\n";
    assert_eq!(
        owner_question(CLAUDE_ASK.as_bytes()).as_deref(),
        Some(CLAUDE_LINE)
    );
    assert_eq!(
        owner_question(codex_ask.as_bytes()).as_deref(),
        Some(
            "Which database should the service use? | 1. Postgres | 2. SQLite | 3. None of the above"
        )
    );

    let named = " Should I Type something into the form?\n \u{276f} 1. Yes\n   2. No\n";
    assert_eq!(owner_question(named.as_bytes()), None);
    let unmarked = " Which sign-in?\n   1. OAuth\n   2. API key\n   3. Type something.\n";
    assert_eq!(owner_question(unmarked.as_bytes()), None);

    let other_marker = CLAUDE_ASK.replace('\u{276f}', ">");
    let printed: [(&str, &str); 4] = [
        (
            "a quoted list above an idle Claude input prompt",
            " Here is what the page asks:\n\n > 1. Yes\n   2. None of the above\n\n\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\n > \n\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\n  ? for shortcuts\n",
        ),
        (
            "a printed box with its footer above an idle Claude input prompt",
            " Which sign-in should the app use?\n \u{276f} 1. OAuth\n   2. Type something.\n Enter to select \u{b7} \u{2191}/\u{2193} to navigate \u{b7} Esc to cancel\n\n\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\n > \n\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\n  ? for shortcuts\n",
        ),
        (
            "a printed box with its footer above an idle Codex composer",
            "  Which database?\r\n\u{203a} 1. Postgres\r\n  2. None of the above\r\n  tab to add notes \u{b7} enter to submit answer\r\n\r\n\u{203a} Ask Codex to do anything\r\n  \u{23ce} send   \u{2303}J newline   \u{2303}C quit\r\n",
        ),
        (
            "a box drawn with another app's marker",
            other_marker.as_str(),
        ),
    ];
    for (case, shown) in printed {
        assert!(
            question(shown.as_bytes()).is_some(),
            "{case} reads as a choice"
        );
        assert_eq!(owner_question(shown.as_bytes()), None, "{case}");
    }
}

#[test]
fn a_permission_prompt_is_not_logged() {
    let prompts = [
        "\u{2502} Bash command                    \u{2502}\r\n\u{2502}   cargo test                    \u{2502}\r\n\u{2502} Do you want to proceed?         \u{2502}\r\n\u{2502} \x1b[36m\u{276f} 1. Yes\x1b[0m                      \u{2502}\r\n\u{2502}   2. Yes, and don't ask again for cargo test commands \u{2502}\r\n\u{2502}   3. No, and tell Claude what to do differently (esc) \u{2502}\r\n",
        "Would you like to run the following command?\r\n\r\n  $ cargo test -p crazyagent\r\n\r\n\x1b[1m\u{203a} 1. Yes, proceed (y)\x1b[22m\r\n  2. Yes, and don't ask again for this command in this session (a)\r\n  3. No, and tell Codex what to do differently (esc)\r\n",
        " Would you like to proceed?\n \u{276f} 1. Yes, auto-accept edits\n   2. Yes, manually approve edits\n   3. No, keep planning\n",
        " New MCP server found in .mcp.json: docs\n \u{276f} 1. Use this and all future MCP servers in this project\n   2. Use this MCP server\n   3. Continue without using this MCP server\n",
        " Allow access to the host api.example.com?\n\u{203a} 1. Yes, just this once\n  2. Yes, and allow this host for this conversation\n  3. No, and block this host in the future\n",
    ];
    for prompt in prompts {
        assert!(
            question(prompt.as_bytes()).is_some(),
            "the box is read as a choice: {prompt:?}"
        );
        assert_eq!(
            owner_question(prompt.as_bytes()),
            None,
            "a permission or approval prompt is not an owner question: {prompt:?}"
        );
    }

    let fixture = Fixture::new();
    let host = fixture.host();
    fixture.pane(&host, 42, PERMISSION, "unused");
    until("the permission prompt on screen", || {
        question(&screen(&host, 42)).is_some()
    });
    let masters = [master(42)];
    let mut watch = MasterWatch::default();
    for _ in 0..3 {
        assert_eq!(
            watch.scan(host.as_ref(), &masters, &[]),
            vec![],
            "a permission prompt waiting on a master's screen is never asked"
        );
    }
}

#[test]
#[should_panic]
fn only_a_master_the_last_scan_read_can_report_a_logged_question() {
    MasterWatch::default().logged(1, CLAUDE_LINE);
}

#[test]
fn a_question_box_on_a_master_screen_is_logged_once() {
    let fixture = Fixture::new();
    let host = fixture.host();
    let answered = fixture.base.join("answered.txt");
    fixture.pane(&host, 41, ASK, answered.to_str().unwrap());
    fixture.pane(&host, 42, PERMISSION, "unused");
    until("both boxes on screen", || {
        owner_question(&screen(&host, 41)).is_some() && question(&screen(&host, 42)).is_some()
    });
    let masters = [master(41), master(42)];
    let mut watch = MasterWatch::default();
    let mut seen = Vec::new();
    until("the master's question to be asked", || {
        seen.extend(watch.scan(host.as_ref(), &masters, &[]));
        !seen.is_empty()
    });
    let asked = Change::Asked {
        project: project(),
        agent: 41,
        text: CLAUDE_LINE.to_owned(),
    };
    assert_eq!(
        seen,
        vec![asked.clone()],
        "the permission prompt on the other master is never asked"
    );
    assert_eq!(
        watch.scan(host.as_ref(), &masters, &[]),
        vec![asked],
        "a question the log does not hold yet, such as one it refused, is asked again"
    );
    let mut dismissed = MasterWatch::default();
    until("the restarted watch to ask the box", || {
        !dismissed.scan(host.as_ref(), &masters, &[]).is_empty()
    });
    dismissed.logged(41, CLAUDE_LINE);
    for _ in 0..3 {
        assert_eq!(
            dismissed.scan(host.as_ref(), &masters, &[]),
            vec![],
            "a box logged and dismissed before the next scan is not asked again"
        );
    }

    let logged = [pending(7, 41)];
    for _ in 0..3 {
        assert_eq!(watch.scan(host.as_ref(), &masters, &logged), vec![]);
    }
    let mut restarted = MasterWatch::default();
    for _ in 0..3 {
        assert_eq!(
            restarted.scan(host.as_ref(), &masters, &logged),
            vec![],
            "a box the log already holds is not asked again after a restart"
        );
    }
    for _ in 0..3 {
        assert_eq!(
            restarted.scan(host.as_ref(), &masters, &[]),
            vec![],
            "a box the owner dismissed is not asked again while it stays on screen"
        );
    }
}

#[test]
fn the_box_leaving_after_owner_input_reports_it_left() {
    let fixture = Fixture::new();
    let host = fixture.host();
    let answered = fixture.base.join("answered.txt");
    fixture.pane(&host, 41, ASK, answered.to_str().unwrap());
    until("the box on screen", || {
        owner_question(&screen(&host, 41)).is_some()
    });
    let masters = [master(41)];
    let logged = [pending(7, 41)];
    let mut watch = MasterWatch::default();
    for _ in 0..3 {
        assert_eq!(
            watch.scan(host.as_ref(), &masters, &logged),
            vec![],
            "a logged box still on screen is neither asked again nor closed"
        );
    }

    assert!(type_prompt(host.as_ref(), &AgentId::team(41).unwrap(), "3").unwrap());
    until("the owner's answer in the terminal", || {
        std::fs::read_to_string(&answered).is_ok_and(|text| text == "answer:3\n")
    });
    until("the box to leave the screen", || {
        let shown = screen(&host, 41);
        String::from_utf8_lossy(&shown).contains("thanks") && owner_question(&shown).is_none()
    });
    assert_eq!(
        watch.scan(host.as_ref(), &masters, &logged),
        vec![],
        "one scan without the box does not yet close it"
    );
    assert_eq!(
        watch.scan(host.as_ref(), &masters, &logged),
        vec![Change::Left { id: 7, agent: 41 }]
    );
    for _ in 0..3 {
        assert_eq!(watch.scan(host.as_ref(), &masters, &[]), vec![]);
    }
}

#[test]
fn a_logged_question_whose_master_left_or_whose_terminal_ended_has_left() {
    let fixture = Fixture::new();
    let host = fixture.host();
    let answered = fixture.base.join("answered.txt");
    fixture.pane(&host, 44, ASK, answered.to_str().unwrap());
    until("the box on screen", || {
        owner_question(&screen(&host, 44)).is_some()
    });
    let masters = [master(44)];
    let logged = [pending(8, 43), pending(9, 44)];
    let mut watch = MasterWatch::default();
    assert!(watch.due());
    assert!(!watch.due(), "a second scan waits for SCAN_EVERY");
    assert_eq!(watch.askers(&masters), vec![(project(), 44)]);
    assert_eq!(watch.scan(host.as_ref(), &masters, &logged), vec![]);
    assert_eq!(
        watch.askers(&masters),
        vec![(project(), 43), (project(), 44)],
        "a master that left is read while the log holds a question of its"
    );
    assert_eq!(
        watch.scan(host.as_ref(), &masters, &logged),
        vec![Change::Left { id: 8, agent: 43 }],
        "a master no longer live shows none of its questions"
    );
    let logged = [pending(9, 44)];
    assert_eq!(watch.scan(host.as_ref(), &masters, &logged), vec![]);
    assert_eq!(
        watch.askers(&masters),
        vec![(project(), 44)],
        "a master that left is forgotten once its questions are closed"
    );

    host.kill(&AgentId::team(44).unwrap()).unwrap();
    until("the terminal to end", || {
        host.screen(&AgentId::team(44).unwrap()).is_err()
    });
    assert_eq!(watch.scan(host.as_ref(), &masters, &logged), vec![]);
    assert_eq!(
        watch.scan(host.as_ref(), &masters, &logged),
        vec![Change::Left { id: 9, agent: 44 }],
        "a question whose terminal ended is closed"
    );
    assert!(
        watch.screens.is_empty(),
        "an ended terminal's screen is forgotten"
    );
}
