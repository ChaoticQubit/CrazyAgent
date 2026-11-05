#![cfg(target_os = "macos")]

use crazyagent::coord::delivery::codex::{self, Bridged, CONTROL_SOCKET};
use crazyagent::harnesses::interactive::{Run, command};
use crazyagent::harnesses::launch::{AccountBinding, LaunchLimits, LaunchRequest};
use crazyagent::harnesses::{HarnessStatus, Search, catalog};
use crazyagent::sandbox::{ProjectSandbox, SandboxNetwork, SandboxRemote};
use crazyagent::terminal::tmux::{SOCKET, Tmux, installed};
use crazyagent::terminal::{AgentId, FOLDER, Host};
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const OLLAMA_OPENAI: &str = "http://127.0.0.1:11434/v1";
const LOCAL_MODEL: &str = "qwen2.5:0.5b";
const SESSION: &str = "0199a0b0-0000-4000-8000-000000000001";
const GREETING: &str = "Reply with one short greeting.";
const LONG_TURN: &str =
    "Write the numbers from 1 to 3000 in words, one per line, with no other text.";
const STEER: &str = "Stop counting now and reply with the single word steered.";
const START: Duration = Duration::from_secs(60);
const TURN: Duration = Duration::from_secs(400);

struct Server {
    tmux: PathBuf,
    socket: PathBuf,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = crazyagent::spawn_gate::status(
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

/// Makes a private folder for one part of the agent's world.
/// Input: the path. Output: the canonical path, created with mode 0700.
fn private(path: &Path) -> PathBuf {
    std::fs::create_dir_all(path).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
    path.canonicalize().unwrap()
}

/// Finds the installed Codex and checks it is a release the bridge was checked against.
/// Input: nothing. Output: the canonical executable; panics when Codex is absent or is
/// not one of `codex::VERSIONS`.
fn installed_codex() -> PathBuf {
    let entries = catalog(&Search::from_environment());
    let entry = entries.iter().find(|entry| entry.id == "codex").unwrap();
    assert_eq!(
        entry.status,
        HarnessStatus::Ready,
        "UNAVAILABLE: codex must be installed for the live bridge check"
    );
    let executable = std::fs::canonicalize(entry.executable.as_ref().unwrap()).unwrap();
    let output = crazyagent::spawn_gate::output(
        Command::new(&executable)
            .arg("--version")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
    )
    .unwrap();
    let version = String::from_utf8(output.stdout).unwrap();
    assert!(
        codex::VERSIONS
            .iter()
            .any(|(major, minor)| version.contains(&format!(" {major}.{minor}."))),
        "UNAVAILABLE: the bridge was not checked against {version}"
    );
    executable
}

/// Tells whether any Codex record under the agent home holds a text.
/// Input: the agent's `CODEX_HOME` and the text. Output: whether a rollout contains it.
fn recorded(home: &Path, text: &str) -> bool {
    let mut folders = vec![home.join("sessions")];
    while let Some(folder) = folders.pop() {
        let Ok(entries) = std::fs::read_dir(&folder) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let kind = entry.file_type().unwrap();
            if kind.is_dir() {
                folders.push(path);
            } else if kind.is_file()
                && std::fs::read_to_string(&path).is_ok_and(|record| record.contains(text))
            {
                return true;
            }
        }
    }
    false
}

/// Waits for a condition on the agent home, failing with what was awaited.
/// Input: how long, what is awaited and the check. Output: nothing; panics on timeout.
fn until(limit: Duration, what: &str, done: impl Fn() -> bool) {
    let deadline = Instant::now() + limit;
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// K2b live: a real tools-on Codex app launched by the engine's own command, inside the real
/// project sandbox with only its control socket granted, starts its app server through the
/// bridge and attaches to it (a tools-off app runs without the shared app server and is
/// reached by typing). `codex::deliver` then starts a turn while the app is idle and steers
/// the running turn while it works. Needs Codex 0.164 and a local Ollama model.
#[test]
fn a_real_codex_app_takes_coord_messages_through_its_app_server_idle_and_busy() {
    let root = tempfile::Builder::new()
        .prefix("ca-bridge-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    let base = private(root.path());
    let project = private(&base.join("project"));
    let home = private(&base.join("home"));
    let tmp = private(&base.join("tmp"));
    let executable = installed_codex();
    let socket = codex::socket(&home).unwrap();
    codex::prepare_folder().unwrap();
    let sandbox = ProjectSandbox::new(
        &project,
        &home,
        &tmp,
        SandboxNetwork::Only(vec![SandboxRemote::for_endpoint(OLLAMA_OPENAI).unwrap()]),
    )
    .unwrap()
    .allow_read(executable.parent().unwrap())
    .unwrap()
    .allow_unix_socket(&socket)
    .unwrap();
    let profile = base.join("agent.sb");
    sandbox.install(&profile).unwrap();
    let request = LaunchRequest {
        harness: "codex".into(),
        executable,
        working_dir: project,
        account: AccountBinding::Url {
            home: home.clone(),
            base_url: OLLAMA_OPENAI.into(),
            key: None,
        },
        model: Some(LOCAL_MODEL.into()),
        tools: true,
        sandbox_profile: Some(profile),
        unconfined: false,
        git_identity: None,
        limits: LaunchLimits::default(),
    };
    let built = command(
        &request,
        None,
        &Run {
            session_id: SESSION,
            key_file: None,
            resume: false,
        },
    )
    .unwrap();

    let tmux = installed(&base).expect("tmux must be installed on this machine");
    let folder = base.join(FOLDER);
    let _server = Server {
        tmux: tmux.clone(),
        socket: folder.join(SOCKET),
    };
    let host = Tmux::open(Some(tmux), &folder);
    let agent = AgentId::team(1).unwrap();
    host.spawn(&agent, &built).unwrap();
    until(START, "the app server's control socket", || {
        std::fs::metadata(home.join(CONTROL_SOCKET))
            .is_ok_and(|socket| socket.file_type().is_socket())
    });

    let deadline = Instant::now() + START;
    loop {
        match codex::deliver(&home, GREETING) {
            Bridged::Delivered => break,
            Bridged::Unavailable(reason) => {
                assert!(
                    Instant::now() < deadline,
                    "the idle app never took a turn through its app server: {reason}"
                );
                std::thread::sleep(Duration::from_millis(500));
            }
        }
    }
    until(TURN, "the delivered greeting and its reply", || {
        recorded(&home, GREETING) && recorded(&home, "\"role\":\"assistant\"")
    });

    assert_eq!(codex::deliver(&home, LONG_TURN), Bridged::Delivered);
    until(TURN, "the long turn to start", || {
        recorded(&home, LONG_TURN)
    });
    assert_eq!(codex::deliver(&home, STEER), Bridged::Delivered);
    until(TURN, "the steer to reach the running turn", || {
        recorded(&home, STEER)
    });
    host.kill(&agent).unwrap();
}
