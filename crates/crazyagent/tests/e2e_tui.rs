#![cfg(unix)]

mod e2e_support;

use crazyagent::engine::owner::OwnerResult;
use crazyagent::terminal::tmux::installed;
use e2e_support::{ENGINE, Engine, Owner, TMUX, hex, profile, scratch, stop_tmux};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

const COLUMNS: &str = "120";
const ROWS: &str = "40";
const SESSION: &str = "e2e";
const SCREEN_TIME: Duration = Duration::from_secs(30);
const POLL_TIME: Duration = Duration::from_millis(100);
const STEPS_MAX: usize = 12;
const SHIFT_RIGHT: [&str; 6] = ["1b", "5b", "31", "3b", "32", "43"];
const PROJECT: &str = "Tidepool e2e";
const LIVE_SOCKET: &str = "CRAZYAGENT_E2E_LIVE_SOCKET";
const LIVE_PROJECT: &str = "CRAZYAGENT_E2E_LIVE_PROJECT";

struct Pty {
    tmux: PathBuf,
    socket: PathBuf,
    _folder: tempfile::TempDir,
}

impl Pty {
    fn launch(tmux: PathBuf, engine_socket: &Path) -> Self {
        let engine_socket = engine_socket.to_str().unwrap();
        assert!(!ENGINE.contains('\'') && !engine_socket.contains('\''));
        let folder = scratch("ca-e2e-pty-");
        let pty = Self {
            tmux,
            socket: folder.path().join("pty.sock"),
            _folder: folder,
        };
        let command = format!("'{ENGINE}' tui '{engine_socket}'");
        pty.run(&[
            "-f",
            "/dev/null",
            "new-session",
            "-d",
            "-s",
            SESSION,
            "-x",
            COLUMNS,
            "-y",
            ROWS,
            &command,
        ]);
        pty
    }

    fn run(&self, args: &[&str]) -> Output {
        let output = Command::new(&self.tmux)
            .arg("-S")
            .arg(&self.socket)
            .args(args)
            .env_remove("TMUX")
            .stdin(Stdio::null())
            .output()
            .expect(TMUX);
        assert!(
            output.status.success(),
            "tmux {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }

    fn keys(&self, keys: &[&str]) {
        let mut args = vec!["send-keys", "-t", SESSION];
        args.extend_from_slice(keys);
        self.run(&args);
    }

    fn bytes(&self, hex: &[&str]) {
        let mut args = vec!["send-keys", "-t", SESSION, "-H"];
        args.extend_from_slice(hex);
        self.run(&args);
    }

    fn text(&self, text: &str) {
        assert!(!text.is_empty() && !text.starts_with('-'));
        self.run(&["send-keys", "-t", SESSION, "-l", text]);
    }

    fn screen(&self) -> String {
        String::from_utf8_lossy(&self.run(&["capture-pane", "-p", "-t", SESSION]).stdout)
            .into_owned()
    }

    fn until(&self, what: &str, check: impl Fn(&str) -> bool) -> String {
        let start = Instant::now();
        loop {
            let screen = self.screen();
            if check(&screen) {
                return screen;
            }
            assert!(start.elapsed() < SCREEN_TIME, "{what}:\n{screen}");
            std::thread::sleep(POLL_TIME);
        }
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        stop_tmux(&self.tmux, &self.socket);
    }
}

fn at_home(screen: &str, project: &str) -> bool {
    screen.contains("New project") && screen.contains(project)
}

fn on_agents(screen: &str, project: &str) -> bool {
    screen.contains(&format!("{project}  /  Agents"))
}

fn on_team(screen: &str, project: &str) -> bool {
    screen.contains(&format!("{project}  /  Team")) && screen.contains("Master")
}

fn reach_home(pty: &Pty, project: &str) {
    for step in 0..=STEPS_MAX {
        assert!(
            step < STEPS_MAX,
            "the TUI never reached Home:\n{}",
            pty.screen()
        );
        let screen = pty.until("the TUI shows Home or a first-run step", |screen| {
            at_home(screen, project)
                || screen.contains("Welcome to CrazyAgent")
                || screen.contains("c Continue without")
        });
        if at_home(&screen, project) {
            return;
        }
        assert!(
            !screen.contains("Welcome to CrazyAgent"),
            "a saved profile skips Welcome:\n{screen}"
        );
        pty.keys(&["c"]);
        pty.until("the first-run step moves on", |now| now != screen);
    }
}

fn open_team(pty: &Pty, project: &str) {
    reach_home(pty, project);
    for _ in 0..STEPS_MAX {
        if pty.screen().contains("Open") {
            break;
        }
        pty.keys(&["Tab"]);
        std::thread::sleep(POLL_TIME);
    }
    pty.until("Home offers to open the project", |screen| {
        screen.contains("Open")
    });
    pty.keys(&["Enter"]);
    pty.until("the project screen opens with its tabs", |screen| {
        screen.contains(project) && screen.contains("Tabs")
    });
    for step in 0..=STEPS_MAX {
        let screen = pty.screen();
        if on_agents(&screen, project) {
            break;
        }
        assert!(step < STEPS_MAX, "no project tab is Agents:\n{screen}");
        pty.bytes(&SHIFT_RIGHT);
        pty.until("the next project tab shows", |now| now != screen);
    }
    pty.keys(&["t"]);
    pty.until("t on the Agents tab opens the team tree", |screen| {
        on_team(screen, project)
    });
}

#[test]
fn the_tui_walks_from_home_into_the_project_team_tree() {
    let mut engine = Engine::start("ca-e2e-tui-");
    let tmux = installed(engine.path()).expect(TMUX);
    let disk = scratch("ca-e2e-home-");
    let home = disk.path().join("Projects");
    std::fs::create_dir(&home).unwrap();
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let mut owner = Owner::attach(&engine).await;
            assert_eq!(owner.save_profile(profile()).await, OwnerResult::Applied);
            assert_eq!(owner.set_home(&home).await, OwnerResult::Applied);
            assert_eq!(
                owner.create_project("e2e-tui", PROJECT).await,
                OwnerResult::Applied
            );
            owner.controller.detach();
        });

    let pty = Pty::launch(tmux, &engine.socket());
    open_team(&pty, PROJECT);
    let screen = pty.screen();
    assert!(screen.contains("Master"), "{screen}");
    assert!(!screen.contains("Not ready yet"), "{screen}");
    drop(pty);
    engine.stop();
}

fn live_team_missing() -> ! {
    panic!(
        "UNAVAILABLE: a running engine with a live team agent. Start a project master on a \
         signed-in pool, then set {LIVE_SOCKET}=<its engine.sock> and \
         {LIVE_PROJECT}=<the project name>"
    )
}

#[test]
fn the_tui_types_into_a_live_agent_terminal_from_the_team_tree() {
    let socket = std::env::var_os(LIVE_SOCKET)
        .map(PathBuf::from)
        .unwrap_or_else(|| live_team_missing());
    let project = std::env::var(LIVE_PROJECT).unwrap_or_else(|_| live_team_missing());
    assert!(socket.exists(), "{LIVE_SOCKET} names no socket");
    let probe = scratch("ca-e2e-probe-");
    let tmux = installed(probe.path()).expect(TMUX);
    let pty = Pty::launch(tmux, &socket);
    open_team(&pty, &project);
    pty.keys(&["Enter"]);
    for step in 0..=STEPS_MAX {
        if pty.screen().contains("Back to the team") {
            break;
        }
        assert!(
            step < STEPS_MAX,
            "Enter then Tab never moved the keys into the live master's terminal:\n{}",
            pty.screen()
        );
        pty.keys(&["Tab"]);
        std::thread::sleep(POLL_TIME);
    }
    let marker = format!("e2e{}", hex(4));
    pty.text(&marker);
    pty.until("the typed text shows in the terminal pane", |screen| {
        screen.contains(&marker)
    });
    for _ in 0..marker.len() {
        pty.keys(&["BSpace"]);
    }
    pty.until("the typed text is erased again", |screen| {
        !screen.contains(&marker)
    });
}
