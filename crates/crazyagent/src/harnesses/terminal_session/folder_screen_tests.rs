use super::TerminalSession;
use crate::harnesses::codex_folder::SCREEN_WAIT;
use crate::harnesses::launch::{AccountBinding, LaunchError, LaunchLimits, LaunchRequest};
use crate::terminal::tmux::{SOCKET, Tmux, installed};
use crate::terminal::{AgentId, FOLDER, Host};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
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
struct Pane {
    session: TerminalSession,
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

fn start(name: &str, first: &str, then: Option<&str>, codex_folder: bool) -> Pane {
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

    fn close(self) {
        assert!(self.session.stop().is_ok());
    }
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
