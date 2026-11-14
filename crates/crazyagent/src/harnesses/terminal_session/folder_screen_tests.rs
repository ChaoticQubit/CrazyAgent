use super::TerminalSession;
use crate::harnesses::codex_folder::{SCREEN_WAIT, Screen, read_screen};
use crate::harnesses::interactive::InteractiveCommand;
use crate::harnesses::launch::{AccountBinding, LaunchError, LaunchLimits, LaunchRequest};
use crate::terminal::tmux::{SOCKET, Tmux, installed};
use crate::terminal::{AgentId, FOLDER, Host, HostError, PaneRef, TerminalStream};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const MENU_QUIT: &str = "Folder access\r\n/some/folder\r\n\r\n\u{203a} 1. Open restricted\r\n  2. Quit\r\n\r\nenter continue \u{b7} esc quit\r\n";
const MENU_BACK: &str = "Folder access\r\n/some/folder\r\n\r\n\u{203a} 1. Open restricted\r\n  2. Back to Agent Command Center\r\n\r\nenter continue \u{b7} esc back\r\n";
const MENU_EXISTING: &str = "Folder access\r\n/some/folder\r\n\r\n\u{203a} 1. Open existing task\r\n  2. Back to Agent Command Center\r\n\r\nenter continue \u{b7} esc back\r\n";
const INPUT_BOX: &str = "\u{203a} Ask Codex to do anything\r\n\r\n  ? for shortcuts\r\n";
const ENTER_KEY: &str = "0d";
const WATCH: Duration = Duration::from_millis(2500);
const READY_WITHIN: Duration = Duration::from_secs(15);

/// One test's tmux server, ended when the test ends.
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

/// A shell stand-in for Codex in a real tmux pane: it draws `first`, records every key it
/// reads, and redraws `then` after a key once that file exists.
pub(crate) struct Pane {
    pub(crate) session: TerminalSession,
    keys: PathBuf,
    _server: Server,
    _root: tempfile::TempDir,
}

fn private(path: &Path) {
    std::fs::create_dir_all(path).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
}

fn stand_in(first: &Path, then: &Path, keys: &Path) -> String {
    format!(
        "#!/bin/sh\n/bin/stty -echo -icanon -icrnl min 1 time 0\nprintf '\\033[?2004h'\n/bin/cat '{first}'\nwhile :; do\n  c=$(/bin/dd bs=1 count=1 2>/dev/null | /usr/bin/od -An -tx1)\n  printf '%s\\n' $c >> '{keys}'\n  if [ -e '{then}' ]; then printf '\\033[2J\\033[H'; /bin/cat '{then}'; fi\ndone\n",
        first = first.display(),
        keys = keys.display(),
        then = then.display(),
    )
}

pub(crate) fn start(name: &str, first: &str, then: Option<&str>, codex_folder: bool) -> Pane {
    let root = tempfile::Builder::new()
        .prefix("ca-fold-")
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
    for path in [&home, &work, &bin] {
        private(path);
    }
    private(&work.join(".git"));
    if codex_folder {
        private(&work.join(".codex"));
    }
    let (first_file, then_file, keys) = (base.join("first"), base.join("then"), base.join("keys"));
    std::fs::write(&first_file, first).unwrap();
    if let Some(then) = then {
        std::fs::write(&then_file, then).unwrap();
    }
    let executable = bin.join("codex");
    std::fs::write(&executable, stand_in(&first_file, &then_file, &keys)).unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    let profile = base.join("agent.sb");
    std::fs::write(&profile, "(version 1)\n(allow default)\n").unwrap();
    let request = LaunchRequest {
        harness: "codex".into(),
        executable,
        working_dir: work,
        account: AccountBinding::Url {
            home,
            base_url: "http://127.0.0.1:9/v1".into(),
            key: None,
        },
        model: None,
        tools: false,
        sandbox_profile: Some(profile),
        unconfined: false,
        git_identity: None,
        limits: LaunchLimits::default(),
    };
    let session = TerminalSession::start(host, AgentId::new(name).unwrap(), request, None).unwrap();
    Pane {
        session,
        keys,
        _server: server,
        _root: root,
    }
}

impl Pane {
    fn keys(&self) -> Vec<String> {
        std::fs::read_to_string(&self.keys)
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    fn ready_within(&mut self, limit: Duration) -> bool {
        let until = Instant::now() + limit;
        while Instant::now() < until {
            if self.session.poll_ready().unwrap() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        false
    }

    fn run_out(&mut self) -> LaunchError {
        let first = self.session.first.as_mut().unwrap();
        first.started = Instant::now() - SCREEN_WAIT - Duration::from_secs(1);
        self.session.poll_ready().unwrap_err()
    }

    /// Uses up a launch's wait for its folder access screen, so the next look at it ends
    /// the launch. Takes the pane, which has not been looked at yet.
    pub(crate) fn spend_the_wait(&mut self) {
        assert!(!self.session.poll_ready().unwrap());
        let first = self.session.first.as_mut().unwrap();
        first.started = Instant::now() - SCREEN_WAIT - Duration::from_secs(1);
    }

    /// Waits until the pane's screen reads as `wanted`. Panics when it never does.
    fn shows(&self, wanted: Screen) {
        let until = Instant::now() + READY_WITHIN;
        while read_screen(&self.session.host.screen(&self.session.agent).unwrap()) != wanted {
            assert!(Instant::now() < until, "the pane never showed {wanted:?}");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// Hands the pane to a later engine, as a restart does: this engine lets go of it and a
    /// new session adopts it from its record. Takes the pane. Returns the adopted pane.
    fn adopted(self) -> Self {
        let Self {
            session,
            keys,
            _server,
            _root,
        } = self;
        let record = session.record().cloned().unwrap();
        let (host, agent) = (session.host.clone(), session.agent.clone());
        session.leave();
        let session =
            TerminalSession::adopt(host, agent, &record, LaunchLimits::default()).unwrap();
        Self {
            session,
            keys,
            _server,
            _root,
        }
    }

    fn close(self) {
        assert!(self.session.stop().is_ok());
    }
}

/// SECAUDIT-10-L2: a Codex pane a restart finds still on its folder access screen takes the
/// one Enter its launch owed it and is ready only at the input box, so no prompt is pasted
/// into the menu.
#[test]
fn an_adopted_pane_still_on_the_folder_screen_takes_its_enter_before_it_is_ready() {
    let pane = start("adopt-menu", MENU_QUIT, Some(INPUT_BOX), true);
    pane.shows(Screen::Menu);
    let mut pane = pane.adopted();
    assert!(pane.ready_within(READY_WITHIN));
    assert_eq!(pane.keys(), [ENTER_KEY]);
    pane.close();
}

/// SECAUDIT-10-L2: an adopted Codex pane at its input box is ready at the first look and gets
/// no key, as before.
#[test]
fn an_adopted_pane_at_its_input_box_is_ready_at_once_and_gets_no_key() {
    let pane = start("adopt-box", INPUT_BOX, None, false);
    pane.shows(Screen::InputBox);
    let mut pane = pane.adopted();
    assert!(pane.session.poll_ready().unwrap());
    std::thread::sleep(WATCH);
    assert_eq!(pane.keys(), Vec::<String>::new());
    pane.close();
}

/// A real pane's host whose first look at a screen fails, as one failed tmux capture does.
/// Holds the real host and whether that look has happened. Every other call goes through.
struct FirstLookFails {
    real: Arc<dyn Host>,
    looked: AtomicBool,
}

impl Host for FirstLookFails {
    fn spawn(&self, agent: &AgentId, command: &InteractiveCommand) -> Result<PaneRef, HostError> {
        self.real.spawn(agent, command)
    }

    fn attach(&self, agent: &AgentId) -> Result<TerminalStream, HostError> {
        self.real.attach(agent)
    }

    fn screen(&self, agent: &AgentId) -> Result<Vec<u8>, HostError> {
        if self.looked.swap(true, Ordering::SeqCst) {
            return self.real.screen(agent);
        }
        Err(HostError("terminal_capture_failed".into()))
    }

    fn snapshot(&self, agent: &AgentId) -> Result<TerminalStream, HostError> {
        self.real.snapshot(agent)
    }

    fn kept(&self, agent: &AgentId) -> bool {
        self.real.kept(agent)
    }

    fn keep_last(&self, agent: &AgentId) {
        self.real.keep_last(agent);
    }

    fn input(&self, agent: &AgentId, bytes: &[u8]) -> Result<(), HostError> {
        self.real.input(agent, bytes)
    }

    fn resize(&self, agent: &AgentId, cols: u16, rows: u16) -> Result<(), HostError> {
        self.real.resize(agent, cols, rows)
    }

    fn kill(&self, agent: &AgentId) -> Result<(), HostError> {
        self.real.kill(agent)
    }

    fn unkept(&self, agent: &AgentId) -> bool {
        self.real.unkept(agent)
    }

    fn end_kept_server(&self, agent: &AgentId) -> Option<bool> {
        self.real.end_kept_server(agent)
    }

    fn alive(&self) -> Result<Vec<AgentId>, HostError> {
        self.real.alive()
    }

    fn pane_pid(&self, agent: &AgentId) -> Result<Option<u32>, HostError> {
        self.real.pane_pid(agent)
    }
}

/// A failed look at an adopted pane's screen reads as not on the folder screen, the way
/// `poll_ready` keeps its wait on one, so one failed tmux capture at a restart cannot end
/// a healthy Codex worker.
#[test]
fn an_adopted_pane_whose_first_screen_look_fails_stays_up_and_is_ready() {
    let pane = start("adopt-blind", INPUT_BOX, None, false);
    pane.shows(Screen::InputBox);
    let Pane {
        session,
        keys,
        _server,
        _root,
    } = pane;
    let record = session.record().cloned().unwrap();
    let (real, agent) = (session.host.clone(), session.agent.clone());
    session.leave();
    let failing = FirstLookFails {
        real: real.clone(),
        looked: AtomicBool::new(false),
    };
    let session = TerminalSession::adopt(
        Arc::new(failing),
        agent.clone(),
        &record,
        LaunchLimits::default(),
    )
    .expect("one failed look at the screen must not end the adoption");
    assert!(real.alive().unwrap().contains(&agent));
    let mut pane = Pane {
        session,
        keys,
        _server,
        _root,
    };
    assert!(pane.session.poll_ready().unwrap());
    std::thread::sleep(WATCH);
    assert_eq!(pane.keys(), Vec::<String>::new());
    pane.close();
}

#[test]
fn each_folder_screen_takes_one_enter_and_then_the_input_box_is_ready() {
    for (at, menu) in [MENU_QUIT, MENU_BACK, MENU_EXISTING]
        .into_iter()
        .enumerate()
    {
        let mut pane = start(&format!("fold-a{at}"), menu, Some(INPUT_BOX), true);
        assert!(pane.ready_within(READY_WITHIN), "{menu:?}");
        assert_eq!(pane.keys(), [ENTER_KEY], "{menu:?}");
        pane.close();
    }
}

#[test]
fn the_folder_screen_is_not_ready_and_a_box_that_never_comes_fails_the_launch() {
    let mut pane = start("fold-b", MENU_QUIT, None, true);
    assert!(!pane.ready_within(WATCH));
    assert_eq!(pane.keys(), [ENTER_KEY]);
    assert!(!pane.ready_within(WATCH));
    assert_eq!(pane.keys(), [ENTER_KEY], "never a second key");
    assert_eq!(pane.run_out().0, "launch_folder_box_missing");
    let refused = pane.session.send("the first task").unwrap_err();
    assert_eq!(refused.0, "launch_folder_box_missing");
    assert_eq!(pane.keys(), [ENTER_KEY], "the task is not typed");
    pane.close();
}

#[test]
fn a_screen_that_is_not_exactly_the_folder_menu_gets_no_key_and_fails_the_launch() {
    let cases = [
        MENU_QUIT
            .replace("\u{203a} 1. Open", "  1. Open")
            .replace("  2. Quit", "\u{203a} 2. Quit"),
        MENU_QUIT.replace("Open restricted", "Open anyway"),
        "Trust this folder?\r\n\r\n\u{203a} 1. Trust and continue\r\n  2. Quit\r\n".to_owned(),
        INPUT_BOX.to_owned(),
    ];
    for (at, screen) in cases.iter().enumerate() {
        let mut pane = start(&format!("fold-c{at}"), screen, Some(INPUT_BOX), true);
        assert!(!pane.ready_within(WATCH), "{screen:?}");
        assert_eq!(pane.keys(), Vec::<String>::new(), "{screen:?}");
        assert_eq!(
            pane.run_out().0,
            "launch_folder_screen_missing",
            "{screen:?}"
        );
        pane.close();
    }
}

#[test]
fn a_launch_with_no_codex_folder_presses_nothing_and_is_ready_at_the_box() {
    let mut pane = start("fold-d", INPUT_BOX, None, false);
    assert!(pane.ready_within(READY_WITHIN));
    assert_eq!(pane.keys(), Vec::<String>::new());
    pane.close();
}

/// Writes a Codex rollout for the pane's working folder into its home's sessions, as Codex
/// 0.164 does, and returns its path. Takes the pane and the lines after the session meta.
fn codex_rollout(pane: &Pane, lines: &[&str]) -> PathBuf {
    let base = pane._root.path().canonicalize().unwrap();
    let folder = base.join("home").join("sessions").join("2026/11/11");
    std::fs::create_dir_all(&folder).unwrap();
    let path = folder.join("rollout-2026-11-11T13-00-00-background.jsonl");
    let meta = serde_json::json!({
        "type": "session_meta",
        "payload": {"id": "s", "cwd": base.join("work").to_string_lossy(), "history_mode": "paginated"}
    });
    let mut body = format!("{meta}\n");
    for line in lines {
        body.push_str(line);
        body.push('\n');
    }
    std::fs::write(&path, body).unwrap();
    path
}

/// Appends lines to a rollout. Takes the path and the lines.
fn append_rollout(path: &Path, lines: &[&str]) {
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new().append(true).open(path).unwrap();
    for line in lines {
        writeln!(file, "{line}").unwrap();
    }
}

/// Reads the session's events until none comes for half a second. Takes the pane.
/// Returns the events.
fn pane_events(pane: &mut Pane) -> Vec<crate::harnesses::launch::LaunchEvent> {
    let mut events = Vec::new();
    while let Some(event) = pane.session.next_event(Duration::from_millis(500)).unwrap() {
        events.push(event);
    }
    events
}

const CODEX_TURN: &str = r#"{"type":"event_msg","payload":{"type":"task_started"}}"#;
const CODEX_EXEC: &str = r#"{"type":"response_item","payload":{"type":"function_call","name":"exec_command","call_id":"c1","arguments":"{\"cmd\":\"sleep 40\"}"}}"#;
const CODEX_RUNNING: &str = r#"{"type":"response_item","payload":{"type":"function_call_output","call_id":"c1","output":"Chunk ID: 1\nWall time: 10.0 seconds\nProcess running with session ID 4821\nOutput:\n"}}"#;
const CODEX_REPLY: &str = r#"{"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"Waiting for the check."}]}}"#;
const CODEX_DONE: &str = r#"{"type":"event_msg","payload":{"type":"task_complete","last_agent_message":"Waiting for the check."}}"#;
const CODEX_ENDED: &str = r#"{"type":"event_msg","payload":{"type":"item_completed","thread_id":"t","turn_id":"u","item":{"type":"CommandExecution","id":"c1","process_id":"4821","command":["sleep","40"],"cwd":"/w","parsed_cmd":[],"source":"agent","status":"completed","exit_code":0}}}"#;

/// A Codex pane holds its engine-typed turn for a command it left running in the background
/// as a Claude Code pane does (PA-10): the turn's end is not given while the command runs,
/// and since Codex starts no turn when the command ends, the session ends the held turn
/// itself once Codex records that end. A command whose end is read before its start holds
/// nothing.
#[test]
fn a_codex_pane_holds_its_turn_for_a_background_command_until_codex_records_its_end() {
    use crate::harnesses::launch::LaunchEvent;
    let mut pane = start("codex-background", INPUT_BOX, None, false);
    assert!(pane.ready_within(READY_WITHIN));
    assert!(pane.session.reader.reports_background());
    pane.session.send("Run the check.").unwrap();
    let path = codex_rollout(
        &pane,
        &[
            CODEX_TURN,
            CODEX_EXEC,
            CODEX_RUNNING,
            CODEX_REPLY,
            CODEX_DONE,
        ],
    );
    let first = pane_events(&mut pane);
    assert!(
        !first
            .iter()
            .any(|event| matches!(event, LaunchEvent::TurnFinished { .. })),
        "{first:?}"
    );
    assert!(pane.session.held(), "{first:?}");
    assert_eq!(pane.session.background_running(), 1);

    append_rollout(&path, &[CODEX_ENDED]);
    let last = pane_events(&mut pane);
    assert_eq!(last, vec![LaunchEvent::TurnFinished { success: true }]);
    assert!(!pane.session.held());
    assert_eq!(pane.session.background_running(), 0);

    pane.session.send("Run it again.").unwrap();
    let early = CODEX_ENDED.replace("4821", "4822");
    let late = CODEX_RUNNING.replace("4821", "4822");
    append_rollout(
        &path,
        &[
            CODEX_TURN,
            CODEX_EXEC,
            &early,
            &late,
            CODEX_REPLY,
            CODEX_DONE,
        ],
    );
    let again = pane_events(&mut pane);
    assert!(
        again.contains(&LaunchEvent::TurnFinished { success: true }),
        "{again:?}"
    );
    assert!(!pane.session.held());
    assert_eq!(pane.session.background_running(), 0);
    pane.close();
}
