use super::*;
use crate::harnesses::launch::AccountBinding;
use crate::harnesses::manifest;
use crate::terminal::FOLDER;
use crate::terminal::tmux::{SOCKET, Tmux, installed};
use serde_json::{Value, json};
use std::os::unix::fs::PermissionsExt;
use std::process::{Command, Stdio};

const STANDIN_SH: &str = include_str!("../../../tests/fixtures/harness/standin.sh");
const APPEND: &str = include_str!("../../../tests/fixtures/harness/standin-jsonl-append.yaml");
const REWRITE: &str = include_str!("../../../tests/fixtures/harness/standin-json-rewrite.yaml");
const FIRST_SCREEN: &str = "Trust this folder?\n@standin wait\n";
const PROMPT_MARK: &str = "\x1b[?2004h> ";
/// How long a row waits for a turn to end: well below the stand-in's 30 s pause, so a turn
/// ends in time only when the interrupt key reached the stand-in.
const TURN_WAIT: Duration = Duration::from_secs(20);
const DONE: LaunchEvent = LaunchEvent::TurnFinished { success: true };

/// One row's tmux server, ended when the row ends.
struct Server {
    tmux: PathBuf,
    socket: PathBuf,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = crate::spawn_gate::status(
            Command::new(&self.tmux)
                .arg("-S")
                .arg(&self.socket)
                .arg("kill-server")
                .env_clear()
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null()),
        );
    }
}

/// The stand-in harness of one record kind, run from its manifest in real tmux panes of a
/// server of its own under the always-allow sandbox profile, with its private home, its
/// working folder and the stand-in's state folder, where a row puts its fixtures.
struct Standin {
    host: Arc<dyn Host>,
    kind: &'static str,
    settings: TerminalSettings,
    home: PathBuf,
    work: PathBuf,
    executable: PathBuf,
    profile: PathBuf,
    _server: Server,
    _root: tempfile::TempDir,
}

fn private(path: &Path) {
    std::fs::create_dir_all(path).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
}

/// Sets up a stand-in of one kind: its program, its manifest's settings, its prompt mark
/// that turns bracketed paste on, and its first screen when `first_screen`. Inputs: the
/// record kind and whether it shows a first screen. Output: the stand-in, not started.
fn standin(kind: &'static str, first_screen: bool) -> Standin {
    let root = tempfile::Builder::new()
        .prefix("ca-manifest-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    let base = root.path().canonicalize().unwrap();
    private(&base);
    let tmux = installed(&base).expect("tmux must be installed on this machine");
    let folder = base.join(FOLDER);
    let server = Server {
        tmux: tmux.clone(),
        socket: folder.join(SOCKET),
    };
    let host: Arc<dyn Host> = Arc::new(Tmux::open(Some(tmux), &folder));
    let (home, work, bin) = (base.join("home"), base.join("work"), base.join("bin"));
    for path in [&home, &work, &bin, &home.join("standin")] {
        private(path);
    }
    let executable = bin.join(format!("standin-{kind}"));
    std::fs::write(&executable, STANDIN_SH).unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    let profile = base.join("agent.sb");
    std::fs::write(&profile, "(version 1)\n(allow default)\n").unwrap();
    let text = if kind == "jsonl-append" {
        APPEND
    } else {
        REWRITE
    };
    let settings = TerminalSettings::read(&manifest::parse(text.as_bytes()).unwrap()).unwrap();
    let made = Standin {
        host,
        kind,
        settings,
        home,
        work,
        executable,
        profile,
        _server: server,
        _root: root,
    };
    made.put("prompt-mark", PROMPT_MARK);
    if first_screen {
        made.put("first-screen.out", FIRST_SCREEN);
    }
    made
}

impl Standin {
    /// Starts the stand-in in a new pane from its manifest's settings, resuming the
    /// conversation when given. Inputs: the pane name and the conversation. Output: the
    /// running session.
    fn start(&self, agent: &str, resume: Option<&Resume>) -> TerminalSession {
        let request = LaunchRequest {
            harness: format!("standin-{}", self.kind),
            executable: self.executable.clone(),
            working_dir: self.work.clone(),
            account: AccountBinding::Url {
                home: self.home.clone(),
                base_url: "http://127.0.0.1:9/v1".into(),
                key: None,
            },
            model: None,
            tools: true,
            sandbox_profile: Some(self.profile.clone()),
            unconfined: false,
            git_identity: None,
            limits: LaunchLimits::default(),
        };
        let (host, agent) = (self.host.clone(), AgentId::new(agent).unwrap());
        let settings = self.settings.clone();
        let started = TerminalSession::start_with(host, agent, request, settings, resume);
        assert!(started.is_ok(), "{:?}", started.as_ref().err());
        started.unwrap()
    }

    fn put(&self, name: &str, text: &str) {
        std::fs::write(self.home.join("standin").join(name), text).unwrap();
    }

    fn log(&self, name: &str) -> String {
        std::fs::read_to_string(self.home.join("standin").join(name)).unwrap_or_default()
    }

    /// The arguments the stand-in's run `run` was started with, as it logged them. Input:
    /// the run's number, from 1. Output: the arguments.
    fn argv(&self, run: usize) -> Vec<String> {
        let logged = self.log(&format!("argv-{run}"));
        let mut arguments: Vec<String> = logged.split('\0').map(str::to_owned).collect();
        assert_eq!(arguments.pop().as_deref(), Some(""));
        arguments
    }

    fn home_text(&self) -> String {
        self.home.to_str().unwrap().to_owned()
    }

    /// Waits until the stand-in logged a typed line holding `text`, so its turn runs.
    /// Inputs: the run's number and the text. Output: none.
    fn typed(&self, run: usize, text: &str) {
        let read = crate::poll_support::poll_until(Instant::now() + TURN_WAIT, || {
            self.log(&format!("stdin-{run}")).contains(text)
        });
        assert!(read, "the stand-in never read {text}");
    }
}

fn jsonl(objects: &[Value]) -> String {
    objects.iter().map(|object| format!("{object}\n")).collect()
}

fn said(text: &str) -> LaunchEvent {
    LaunchEvent::Text {
        text: text.to_owned(),
    }
}

/// Reads a session's events until a turn ends, for at most `TURN_WAIT`. Input: the
/// session. Output: the events, the turn's end last.
fn turn(session: &mut TerminalSession) -> Vec<LaunchEvent> {
    let until = Instant::now() + TURN_WAIT;
    let mut events = Vec::new();
    while !matches!(events.last(), Some(LaunchEvent::TurnFinished { .. })) {
        assert!(Instant::now() < until, "no turn end in {events:?}");
        let next = session.next_event(Duration::from_millis(200)).unwrap();
        events.extend(next);
    }
    events
}

/// Reads the `Ready` event a session starts with. Input: the session. Output: the id the
/// harness resumes the conversation by.
fn ready(session: &mut TerminalSession) -> String {
    let first = session.next_event(Duration::from_secs(1)).unwrap();
    let Some(LaunchEvent::Ready {
        native_session: Some(id),
    }) = first
    else {
        panic!("expected Ready with the session id, got {first:?}");
    };
    id
}

/// DEC-180 Task 16: a stand-in run from its manifest gets its arguments, answers its first
/// screen once with the manifest's key, gets each prompt pasted and submitted, and its
/// turns come from its record by the manifest's rules.
#[test]
fn a_stand_in_s_first_screen_is_answered_once_and_its_turns_come_from_its_record() {
    let standin = standin("jsonl-append", true);
    let turn_record = [
        json!({"type": "started"}),
        json!({"type": "text", "delta": "hello back"}),
        json!({"type": "done", "ok": true}),
    ];
    standin.put("turn.jsonl", &jsonl(&turn_record));
    let mut session = standin.start("manifest-append", None);
    let id = ready(&mut session);
    let home = standin.home_text();
    session.send("hello").unwrap();
    assert_eq!(turn(&mut session), [said("hello back"), DONE]);
    let arguments = ["--home", home.as_str(), "--session", id.as_str()];
    assert_eq!(standin.argv(1), arguments);
    session.send("again").unwrap();
    assert_eq!(turn(&mut session), [said("hello back"), DONE]);
    let typed = standin.log("stdin-1");
    let lines: Vec<&str> = typed.lines().collect();
    assert_eq!(lines.len(), 3, "{typed:?}");
    assert_eq!(lines[0], "", "{typed:?}");
    assert!(lines[1].contains("hello"), "{typed:?}");
    assert!(lines[2].contains("again"), "{typed:?}");
}

/// DEC-180 Task 16: a first screen the manifest names that never shows fails the first
/// prompt with its own plain reason once the wait is over, and nothing is typed.
#[test]
fn a_first_screen_never_shown_fails_the_first_prompt_with_a_plain_reason() {
    let standin = standin("jsonl-append", false);
    let mut session = standin.start("manifest-missing", None);
    let asked = Instant::now();
    let missing = Err(failure("launch_first_screen_missing"));
    assert_eq!(session.send("hello"), missing);
    assert!(asked.elapsed() >= READY_WAIT);
    assert_eq!(standin.log("stdin-1"), "");
    let plain = first_screen_missing("Stand-in JSON lines record", "Trust this folder?");
    assert_eq!(session.first_screen_reason(), Some(plain));
}

/// Sets up the jsonl-append stand-in with one piece of its manifest changed. Inputs: the
/// piece, which the manifest holds once, its replacement and whether the stand-in shows
/// its first screen. Output: the stand-in, not started.
fn changed(piece: &str, with: &str, first_screen: bool) -> Standin {
    assert_eq!(APPEND.matches(piece).count(), 1, "{piece}");
    let text = APPEND.replace(piece, with);
    let read = TerminalSettings::read(&manifest::parse(text.as_bytes()).unwrap());
    assert!(read.is_ok(), "{read:?}");
    Standin {
        settings: read.unwrap(),
        ..standin("jsonl-append", first_screen)
    }
}

/// DEC-180 Task 16 r2 (T16-1): a stand-in whose arguments name no session picks its own
/// id. Its run reports none at start and offers no resume until its record shows; the
/// record's file name then names the conversation, and a resume carries that id. Its pane
/// keeps no pane record, so an engine that quits ends it rather than leave a pane no later
/// engine can adopt.
#[test]
fn a_stand_in_that_picks_its_own_session_id_is_named_by_its_record_and_resumed_by_it() {
    let standin = changed("    - \"--session\"\n    - \"{session}\"\n", "", true);
    for (turn, text) in [(1, "one"), (2, "two")] {
        let record = [
            json!({"type": "started"}),
            json!({"type": "text", "delta": text}),
            json!({"type": "done"}),
        ];
        standin.put(&format!("turn-{turn}.jsonl"), &jsonl(&record));
    }
    let mut first = standin.start("manifest-own-id", None);
    let started = first.next_event(Duration::from_secs(1)).unwrap();
    let unnamed = LaunchEvent::Ready {
        native_session: None,
    };
    assert_eq!(started, Some(unnamed));
    assert_eq!(first.conversation(), None);
    assert!(first.record().is_none());
    first.send("one").unwrap();
    assert_eq!(turn(&mut first), [said("one"), DONE]);
    let record = standin.home.join("standin/records/standin-1.jsonl");
    let conversation = Resume {
        id: "standin-1".to_owned(),
        record,
    };
    assert_eq!(first.conversation().as_ref(), Some(&conversation));
    first.leave();
    let agent = AgentId::new("manifest-own-id").unwrap();
    let left = crate::poll_support::poll_until(Instant::now() + TURN_WAIT, || {
        !standin.host.alive().unwrap_or_default().contains(&agent)
    });
    assert!(left, "a pane with no record was left running");
    let mut second = standin.start("manifest-own-id-again", Some(&conversation));
    assert_eq!(ready(&mut second), conversation.id);
    second.send("two").unwrap();
    assert_eq!(turn(&mut second), [said("two"), DONE]);
    let home = standin.home_text();
    assert_eq!(standin.argv(1), ["--home", home.as_str()]);
    let resumed = ["--home", home.as_str(), "--resume", "standin-1"];
    assert_eq!(standin.argv(2), resumed);
}

/// DEC-180 Task 16 r2 (T16-5): a first screen that keeps changing, as a spinner does, is
/// answered once its text shows, without waiting for the screen to go quiet.
#[test]
fn a_first_screen_that_keeps_changing_is_answered_once_its_text_shows() {
    let standin = standin("jsonl-append", false);
    standin.put("first-screen.out", "Trust this folder?\n@standin tick\n");
    let record = [
        json!({"type": "started"}),
        json!({"type": "text", "delta": "hello back"}),
        json!({"type": "done"}),
    ];
    standin.put("turn.jsonl", &jsonl(&record));
    let mut session = standin.start("manifest-ticking", None);
    ready(&mut session);
    let asked = Instant::now();
    assert_eq!(session.send("hello"), Ok(()));
    assert!(asked.elapsed() < READY_WAIT);
    assert_eq!(turn(&mut session), [said("hello back"), DONE]);
    let typed = standin.log("stdin-1");
    let lines: Vec<&str> = typed.lines().collect();
    assert_eq!(lines.len(), 2, "{typed:?}");
    assert_eq!(lines[0], "", "{typed:?}");
    assert!(lines[1].contains("hello"), "{typed:?}");
}

/// DEC-180 Task 16 r2 (T16-Q1): an optional first-screen step whose text never shows is
/// passed over, so the needed step after it is answered once, and its key is never pressed.
#[test]
fn an_optional_step_never_shown_is_passed_over_for_the_needed_step_after_it() {
    let theme = concat!(
        "  first_screen:\n",
        "    theme:\n",
        "      seen: \"Pick a theme\"\n",
        "      press: \"tab\"\n",
        "      optional: true\n",
        "    trust:\n",
    );
    let standin = changed("  first_screen:\n    trust:\n", theme, true);
    let record = [
        json!({"type": "started"}),
        json!({"type": "text", "delta": "hello back"}),
        json!({"type": "done"}),
    ];
    standin.put("turn.jsonl", &jsonl(&record));
    let mut session = standin.start("manifest-theme", None);
    ready(&mut session);
    assert_eq!(session.send("hello"), Ok(()));
    assert_eq!(turn(&mut session), [said("hello back"), DONE]);
    let typed = standin.log("stdin-1");
    let lines: Vec<&str> = typed.lines().collect();
    assert_eq!(lines.len(), 2, "{typed:?}");
    assert_eq!(lines[0], "", "{typed:?}");
    assert!(lines[1].contains("hello"), "{typed:?}");
}

/// DEC-180 Task 16 r2 (T16-Q1): an optional first-screen step is pressed once when its
/// text shows, and a launch whose optional step never shows is never held for it.
#[test]
fn an_optional_step_is_pressed_when_shown_and_never_waited_for() {
    let optional = "      press: \"enter\"\n      optional: true\n";
    let record = [
        json!({"type": "started"}),
        json!({"type": "text", "delta": "hello back"}),
        json!({"type": "done"}),
    ];
    for shown in [true, false] {
        let standin = changed("      press: \"enter\"\n", optional, shown);
        standin.put("turn.jsonl", &jsonl(&record));
        let mut session = standin.start("manifest-optional", None);
        ready(&mut session);
        let asked = Instant::now();
        assert_eq!(session.send("hello"), Ok(()), "shown {shown}");
        assert!(asked.elapsed() < READY_WAIT, "shown {shown}");
        assert_eq!(turn(&mut session), [said("hello back"), DONE]);
        let typed = standin.log("stdin-1");
        let lines: Vec<&str> = typed.lines().collect();
        let pressed = usize::from(shown);
        assert_eq!(lines.len(), pressed + 1, "{typed:?}");
        let presses = &lines[..pressed];
        assert!(presses.iter().all(|line| line.is_empty()), "{typed:?}");
        assert!(lines[pressed].contains("hello"), "{typed:?}");
    }
}

/// DEC-180 Task 16: the interrupt key the manifest names, Ctrl-C for the stand-in, stops
/// its running turn, and the record's facts end it interrupted.
#[test]
fn a_json_rewrite_stand_in_stops_its_turn_on_the_manifest_s_interrupt_key() {
    let standin = standin("json-rewrite", true);
    standin.put("turn.out", "working\n@standin sleep 30\n");
    let stopped = [
        json!({"type": "started"}),
        json!({"type": "stopped"}),
        json!({"type": "done", "ok": false}),
    ];
    standin.put("turn.json", &Value::Array(stopped.to_vec()).to_string());
    let mut session = standin.start("manifest-stop", None);
    ready(&mut session);
    session.send("long job").unwrap();
    standin.typed(1, "long job");
    session.interrupt().unwrap();
    let ended = [
        LaunchEvent::Interrupted,
        LaunchEvent::TurnFinished { success: false },
    ];
    assert_eq!(turn(&mut session), ended);
}

/// DEC-180 Task 16: a resumed stand-in starts with the manifest's resume arguments for its
/// conversation and reports only the turns added after it.
#[test]
fn a_resumed_stand_in_gets_its_resume_arguments_and_reports_only_later_turns() {
    let standin = standin("jsonl-append", true);
    for (turn, text) in [(1, "one"), (2, "two")] {
        let record = [
            json!({"type": "started"}),
            json!({"type": "text", "delta": text}),
            json!({"type": "done"}),
        ];
        standin.put(&format!("turn-{turn}.jsonl"), &jsonl(&record));
    }
    let mut first = standin.start("manifest-first", None);
    ready(&mut first);
    first.send("one").unwrap();
    assert_eq!(turn(&mut first), [said("one"), DONE]);
    let conversation = first.conversation();
    assert!(conversation.is_some());
    let conversation = conversation.unwrap();
    drop(first);
    let mut second = standin.start("manifest-second", Some(&conversation));
    assert_eq!(ready(&mut second), conversation.id);
    second.send("two").unwrap();
    assert_eq!(turn(&mut second), [said("two"), DONE]);
    let home = standin.home_text();
    let id = conversation.id.as_str();
    let arguments = ["--home", home.as_str(), "--resume", id];
    assert_eq!(standin.argv(2), arguments);
}

/// DEC-180 Task 16: a stand-in a quitting engine left running is taken over from its pane
/// record, its record found again through the manifest's record path: no earlier turn is
/// reported again, and the manifest's interrupt key stops a later one.
#[test]
fn a_running_stand_in_is_adopted_from_its_pane_record_and_keeps_its_manifest_s_keys() {
    let standin = standin("jsonl-append", true);
    let one = [
        json!({"type": "started"}),
        json!({"type": "text", "delta": "one"}),
        json!({"type": "done"}),
    ];
    standin.put("turn-1.jsonl", &jsonl(&one));
    standin.put("turn-2.out", "working\n@standin sleep 30\n");
    let stopped = [
        json!({"type": "started"}),
        json!({"type": "stopped"}),
        json!({"type": "done", "ok": false}),
    ];
    standin.put("turn-2.jsonl", &jsonl(&stopped));
    let mut first = standin.start("manifest-adopt", None);
    ready(&mut first);
    first.send("one").unwrap();
    assert_eq!(turn(&mut first), [said("one"), DONE]);
    let record = first.record().cloned();
    assert!(record.is_some());
    let record = record.unwrap();
    let stored = record.encode().and_then(|text| PaneRecord::decode(&text));
    assert_eq!(stored.as_ref(), Some(&record));
    first.leave();
    let agent = AgentId::new("manifest-adopt").unwrap();
    let host = standin.host.clone();
    let (limits, settings) = (LaunchLimits::default(), standin.settings.clone());
    let adopted = TerminalSession::adopt_with(host, agent, &record, limits, settings);
    assert!(adopted.is_ok(), "{:?}", adopted.as_ref().err());
    let mut adopted = adopted.unwrap();
    adopted.send("long job").unwrap();
    standin.typed(1, "long job");
    adopted.interrupt().unwrap();
    let ended = [
        LaunchEvent::Interrupted,
        LaunchEvent::TurnFinished { success: false },
    ];
    assert_eq!(turn(&mut adopted), ended);
}
