#![cfg(unix)]

mod e2e_support;

use crazyagent::engine::Client;
use crazyagent::engine::owner::{OwnerResult, TerminalOutput};
use crazyagent::harnesses::interactive::{InteractiveCommand, Transcript};
use crazyagent::projects_home::ProjectsHome;
use crazyagent::protocol::{
    ClientId, Command as Wire, Cursor, Outcome, PROTOCOL_VERSION, ProjectId, RequestId, WireRequest,
};
use crazyagent::setup::{Dependency, DependencyState};
use crazyagent::team::NodeKind;
use crazyagent::terminal::tmux::{SOCKET, installed};
use crazyagent::terminal::{AgentId, FOLDER, TerminalFrame};
use e2e_support::{ENGINE, Engine, Owner, Survivors, TMUX, hex, scratch, stop_tmux};
use std::collections::BTreeSet;
use std::os::unix::fs::FileTypeExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

const DESKTOP: &str = "UNAVAILABLE: the desktop app binary is not built next to crazyagent; \
run `cargo build -p crazyagent-desktop` (or `cargo test --workspace`, which builds it)";
const PS: &str = "UNAVAILABLE: ps is not on PATH";
const SETTLE_TIME: Duration = Duration::from_secs(10);
const FRAME_TIME: Duration = Duration::from_secs(10);
const POLL_TIME: Duration = Duration::from_millis(100);
const QUIT: &str = "C-q";
const HANGUP_EXIT_TIME: Duration = Duration::from_secs(6);

struct Server {
    tmux: PathBuf,
    socket: PathBuf,
}

impl Server {
    fn run(&self, args: &[&str]) -> Output {
        Command::new(&self.tmux)
            .arg("-S")
            .arg(&self.socket)
            .args(args)
            .env_remove("TMUX")
            .stdin(Stdio::null())
            .output()
            .expect(TMUX)
    }

    fn checked(&self, args: &[&str]) -> String {
        let output = self.run(args);
        assert!(
            output.status.success(),
            "tmux {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    fn alive(&self) -> bool {
        self.run(&["list-sessions"]).status.success()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        stop_tmux(&self.tmux, &self.socket);
    }
}

fn processes(root: &Path) -> Vec<String> {
    let output = Command::new("ps")
        .args(["axww", "-o", "pid=,command="])
        .stdin(Stdio::null())
        .output()
        .expect(PS);
    assert!(output.status.success(), "{PS}");
    let root = root.to_str().unwrap();
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| line.contains(root))
        .map(str::to_owned)
        .collect()
}

fn settled(what: &str, check: impl Fn() -> bool) {
    let start = Instant::now();
    while !check() {
        assert!(start.elapsed() < SETTLE_TIME, "{what}");
        std::thread::sleep(POLL_TIME);
    }
}

fn running(pid: u32) -> bool {
    Command::new("/bin/kill")
        .args(["-0", &pid.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap()
        .success()
}

fn generations(root: &Path) -> Vec<String> {
    std::fs::read_dir(root)
        .unwrap()
        .flatten()
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| {
            name.len() == 9
                && name.starts_with('o')
                && name[1..]
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
        .collect()
}

async fn read_until(output: &mut TerminalOutput, wanted: &[u8]) -> Vec<u8> {
    let mut seen = Vec::new();
    let found = tokio::time::timeout(FRAME_TIME, async {
        while !seen.windows(wanted.len()).any(|window| window == wanted) {
            match output.next().await {
                Ok(TerminalFrame::Output { bytes }) => seen.extend_from_slice(&bytes),
                _ => return false,
            }
        }
        true
    })
    .await
    .unwrap_or(false);
    assert!(found, "{}", String::from_utf8_lossy(&seen));
    seen
}

fn request(id: &str, command: Wire) -> WireRequest {
    WireRequest {
        protocol_version: PROTOCOL_VERSION,
        client_id: ClientId::new("smoke").unwrap(),
        request_id: RequestId::new(id).unwrap(),
        expected_revision: Cursor::new(0),
        command,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_engine_starts_answers_a_plain_read_and_stops_with_nothing_left() {
    let mut engine = Engine::start("ca-smoke-up-");
    let root = engine.path().to_path_buf();
    assert!(
        !processes(&root).is_empty(),
        "the running engine is listed with its root"
    );
    assert!(
        std::fs::symlink_metadata(engine.socket())
            .unwrap()
            .file_type()
            .is_socket()
    );

    let ordinary = Client::new(engine.socket(), Duration::from_secs(5)).unwrap();
    let reply = ordinary
        .request(request("smoke-snapshot", Wire::Snapshot))
        .await
        .unwrap();
    let Outcome::Snapshot { projects, .. } = reply.outcome else {
        panic!("a snapshot answers a snapshot: {:?}", reply.outcome);
    };
    assert!(projects.is_empty());
    drop(ordinary);

    let mut owner = Owner::attach(&engine).await;
    assert_eq!(
        owner.controller.projects_home().await.unwrap(),
        ProjectsHome {
            path: None,
            watching: false
        }
    );
    assert_eq!(generations(&root).len(), 1, "one live owner generation");
    owner.controller.detach();

    engine.stop();
    for leftover in ["engine.sock", "owner-current"] {
        assert!(
            std::fs::symlink_metadata(root.join(leftover)).is_err(),
            "{leftover} is gone after stop"
        );
    }
    assert!(generations(&root).is_empty(), "{:?}", generations(&root));
    assert!(UnixStream::connect(root.join("engine.sock")).is_err());
    settled("no process runs from the engine root after stop", || {
        processes(&root).is_empty()
    });
}

#[tokio::test(flavor = "multi_thread")]
async fn the_engine_private_tmux_server_runs_a_pane_and_never_touches_the_default_socket() {
    let mut engine = Engine::start("ca-smoke-tmux-");
    let root = engine.path().to_path_buf();
    let (ready, host) = crazyagent::terminal::platform(&root);
    assert!(ready, "{TMUX}");
    let server = Server {
        tmux: installed(&root).expect(TMUX),
        socket: root.join(FOLDER).join(SOCKET),
    };
    let marker = format!("smoke-{}", hex(4));
    let agent = AgentId::team(1).unwrap();
    let command = InteractiveCommand {
        argv: vec![
            "/bin/sh".into(),
            "-c".into(),
            format!("/bin/echo {marker}; exec /bin/cat"),
        ],
        cwd: root.clone(),
        env: vec![],
        transcript: Transcript::Unavailable,
    };
    let (host, pane) = {
        let agent = agent.clone();
        tokio::task::spawn_blocking(move || {
            let pane = host.spawn(&agent, &command).unwrap();
            (host, pane)
        })
        .await
        .unwrap()
    };
    assert!(pane.pid > 0 && pane.pane.starts_with('%'), "{pane:?}");
    assert!(
        std::fs::symlink_metadata(&server.socket)
            .unwrap()
            .file_type()
            .is_socket(),
        "the server listens inside the engine's private folder"
    );

    let mut owner = Owner::attach(&engine).await;
    let mut session = owner
        .controller
        .terminal_attach(agent.clone())
        .await
        .unwrap();
    read_until(&mut session.output, marker.as_bytes()).await;
    session.control.detach().await.unwrap();

    let private = server.checked(&["list-panes", "-a", "-F", "#{pane_start_command}"]);
    assert!(private.contains(&marker), "{private}");
    let default = Command::new(&server.tmux)
        .args(["list-panes", "-a", "-F", "#{pane_start_command}"])
        .env_remove("TMUX")
        .stdin(Stdio::null())
        .output()
        .expect(TMUX);
    assert!(
        !String::from_utf8_lossy(&default.stdout).contains(&marker),
        "the engine's pane never shows on the default tmux socket"
    );

    owner.controller.detach();
    tokio::task::spawn_blocking(move || drop(host))
        .await
        .unwrap();
    engine.stop();
    assert!(
        server.alive(),
        "G8b: the engine's tmux sessions outlive the engine so agents keep running"
    );
    assert!(running(pane.pid), "the agent pane survives the engine stop");
    server.checked(&["kill-server"]);
    settled("the private tmux server is gone after kill-server", || {
        !server.alive()
    });
    settled("the pane process ends with its server", || {
        !running(pane.pid)
    });
    settled("no process runs from the engine root", || {
        processes(&root).is_empty()
    });
}

#[tokio::test(flavor = "multi_thread")]
async fn setup_status_projects_home_and_an_empty_team_graph_read_on_a_fresh_engine() {
    let mut engine = Engine::start("ca-smoke-reads-");
    let mut owner = Owner::attach(&engine).await;

    let status = owner.controller.setup_status().await.unwrap();
    assert!(!status.running);
    assert_eq!(
        status
            .dependencies
            .iter()
            .map(|dependency| dependency.name)
            .collect::<Vec<_>>(),
        Dependency::ALL
    );
    for dependency in &status.dependencies {
        assert!(
            matches!(
                dependency.state,
                DependencyState::Missing | DependencyState::Found { .. }
            ),
            "a fresh engine finds each tool or reports it missing: {dependency:?}"
        );
    }
    assert!(!status.pack.is_empty());

    assert_eq!(
        owner.controller.projects_home().await.unwrap(),
        ProjectsHome {
            path: None,
            watching: false
        }
    );

    assert_eq!(
        owner.create_project("smoke-empty", "Smoke empty").await,
        OwnerResult::Applied
    );
    let project = ProjectId::new("smoke-empty").unwrap();
    let graph = owner.controller.team_graph(project.clone()).await.unwrap();
    assert_eq!(graph.project_id, project);
    assert_eq!(
        graph.nodes.iter().map(|node| node.kind).collect::<Vec<_>>(),
        [NodeKind::Master]
    );
    assert!(graph.edges.is_empty());
    owner.controller.detach();
    engine.stop();
}

fn desktop_source(path: &str) -> String {
    let folder = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../apps/desktop/src-tauri");
    std::fs::read_to_string(folder.join(path)).unwrap()
}

fn listed(text: &str, open: &str, close: &str) -> Vec<String> {
    let start = text
        .find(open)
        .unwrap_or_else(|| panic!("{open} is missing"))
        + open.len();
    let end = start + text[start..].find(close).unwrap();
    text[start..end]
        .split(',')
        .map(|item| item.trim().trim_matches('"').to_owned())
        .filter(|item| !item.is_empty())
        .collect()
}

fn command_function(lib: &str, command: &str) -> bool {
    lib.match_indices(&format!("fn {command}(")).any(|(at, _)| {
        let before = lib[..at].trim_end();
        let before = before.strip_suffix("async").unwrap_or(before).trim_end();
        before.ends_with("#[tauri::command]")
    })
}

#[test]
fn the_desktop_app_links_and_registers_every_command_its_build_lists() {
    let build = listed(&desktop_source("build.rs"), ".commands(&[", "]");
    assert!(!build.is_empty());
    assert!(build.iter().all(|command| command.starts_with("desktop_")));
    let unique = build.iter().collect::<BTreeSet<_>>();
    assert_eq!(unique.len(), build.len(), "build.rs lists a command twice");

    let lib = desktop_source("src/lib.rs");
    let handler = listed(&lib, "generate_handler![", "]")
        .into_iter()
        .collect::<BTreeSet<_>>();
    assert_eq!(
        handler,
        build.iter().cloned().collect::<BTreeSet<_>>(),
        "the invoke handler and build.rs list the same commands"
    );
    let capabilities: serde_json::Value =
        serde_json::from_str(&desktop_source("capabilities/main-window.json")).unwrap();
    let allowed = capabilities["permissions"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|permission| permission.as_str())
        .collect::<BTreeSet<_>>();
    for command in &build {
        assert!(
            command_function(&lib, command),
            "{command} has no #[tauri::command] function"
        );
        let permission = format!("allow-{}", command.replace('_', "-"));
        assert!(
            allowed.contains(permission.as_str()),
            "{permission} is missing from main-window.json"
        );
    }

    let binary = Path::new(ENGINE).with_file_name("crazyagent-desktop");
    let bytes = std::fs::read(&binary).expect(DESKTOP);
    let mut linked = BTreeSet::new();
    let mut from = 0;
    while let Some(found) = bytes[from..]
        .windows(b"desktop_".len())
        .position(|window| window == b"desktop_")
    {
        let at = from + found;
        for command in &build {
            if bytes[at..].starts_with(command.as_bytes()) {
                linked.insert(command.clone());
            }
        }
        from = at + 1;
    }
    let missing = build
        .iter()
        .filter(|command| !linked.contains(*command))
        .collect::<Vec<_>>();
    assert!(
        missing.is_empty(),
        "the built desktop app does not link {missing:?}"
    );
}

#[test]
fn the_tui_starts_in_a_pty_draws_its_first_screen_and_quits_on_the_quit_key() {
    let mut engine = Engine::start("ca-smoke-tui-");
    let folder = scratch("ca-smoke-pty-");
    let pty = Server {
        tmux: installed(engine.path()).expect(TMUX),
        socket: folder.path().join("pty.sock"),
    };
    let socket = engine.socket();
    let socket = socket.to_str().unwrap();
    assert!(!ENGINE.contains('\'') && !socket.contains('\''));
    let command = format!("'{ENGINE}' tui '{socket}'");
    pty.checked(&[
        "-f",
        "/dev/null",
        "start-server",
        ";",
        "set-option",
        "-gw",
        "remain-on-exit",
        "on",
        ";",
        "new-session",
        "-d",
        "-s",
        "smoke",
        "-x",
        "100",
        "-y",
        "30",
        &command,
    ]);
    let screen = || pty.checked(&["capture-pane", "-p", "-t", "smoke"]);
    let dead = || {
        pty.checked(&[
            "display-message",
            "-p",
            "-t",
            "smoke",
            "#{pane_dead} #{pane_dead_status}",
        ])
        .trim()
        .to_owned()
    };
    settled("the TUI draws its first screen", || {
        screen().contains("Welcome to CrazyAgent")
    });
    let first = screen();
    assert!(!first.to_lowercase().contains("not connected"), "{first}");
    assert_eq!(dead(), "0", "the TUI keeps running until asked to quit");
    pty.checked(&["send-keys", "-t", "smoke", QUIT]);
    settled("the TUI exits on the quit key", || dead().starts_with('1'));
    assert_eq!(dead(), "1 0", "the TUI exits cleanly:\n{}", screen());
    drop(pty);
    engine.stop();
}

#[test]
fn the_tui_exits_when_its_terminal_hangs_up() {
    let mut engine = Engine::start("ca-smoke-hup-");
    let folder = scratch("ca-smoke-hup-pty-");
    let pty = Server {
        tmux: installed(engine.path()).expect(TMUX),
        socket: folder.path().join("pty.sock"),
    };
    let socket = engine.socket();
    let socket = socket.to_str().unwrap();
    assert!(!ENGINE.contains('\'') && !socket.contains('\''));
    let command = format!("'{ENGINE}' tui '{socket}'");
    pty.checked(&[
        "-f",
        "/dev/null",
        "new-session",
        "-d",
        "-s",
        "hangup",
        "-x",
        "100",
        "-y",
        "30",
        &command,
    ]);
    settled("the TUI draws its first screen", || {
        pty.checked(&["capture-pane", "-p", "-t", "hangup"])
            .contains("Welcome to CrazyAgent")
    });
    let pid: u32 = pty
        .checked(&["display-message", "-p", "-t", "hangup", "#{pane_pid}"])
        .trim()
        .parse()
        .unwrap();
    let survivor = Survivors(vec![pid]);
    assert!(running(pid), "the TUI runs before its terminal hangs up");
    pty.run(&["kill-server"]);
    let start = Instant::now();
    while running(pid) {
        assert!(
            start.elapsed() < HANGUP_EXIT_TIME,
            "the TUI {pid} kept running {HANGUP_EXIT_TIME:?} after its terminal hung up"
        );
        std::thread::sleep(POLL_TIME);
    }
    drop(survivor);
    drop(pty);
    engine.stop();
}
