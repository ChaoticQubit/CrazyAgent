#![cfg(unix)]

use crazyagent::adapters::codex::{
    AuthControl, CodexProtocol, Event, FRAME_BYTES_MAX, LoginChallenge, State, TurnStatus,
};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// Resolves the real Codex executable used by the live protocol checks.
/// Input: optional `CRAZYAGENT_CODEX_EXECUTABLE`, otherwise `codex` on PATH.
/// Output: the canonical absolute executable path; panics when Codex is absent.
fn codex_executable() -> PathBuf {
    if let Some(path) = std::env::var_os("CRAZYAGENT_CODEX_EXECUTABLE") {
        return std::fs::canonicalize(path).expect("configured Codex executable exists");
    }
    let path = std::env::var_os("PATH").expect("PATH is set");
    std::env::split_paths(&path)
        .map(|directory| directory.join("codex"))
        .find(|candidate| candidate.is_file())
        .map(|candidate| std::fs::canonicalize(candidate).unwrap())
        .expect("real codex executable is required for live protocol checks")
}

struct AppServer {
    child: Child,
    input: ChildStdin,
    lines: mpsc::Receiver<Vec<u8>>,
    _home: tempfile::TempDir,
}

impl AppServer {
    fn start() -> Self {
        Self::start_with(&[])
    }

    /// Starts a real Codex app server in a private temporary home with extra `-c` settings.
    /// Input: the settings, each a `key=value` text. Output: the running server.
    fn start_with(settings: &[&str]) -> Self {
        let home = tempfile::Builder::new()
            .prefix("ca-codex-live")
            .tempdir()
            .unwrap();
        std::fs::set_permissions(home.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut child = crazyagent::spawn_gate::spawn(
            Command::new(codex_executable())
                .args(["-c", "cli_auth_credentials_store=\"file\""])
                .args(settings.iter().flat_map(|setting| ["-c", setting]))
                .args(["app-server", "--listen", "stdio://"])
                .env_clear()
                .env("HOME", home.path())
                .env("CODEX_HOME", home.path())
                .env("TMPDIR", home.path())
                .env("PATH", "/usr/bin:/bin")
                .env("LANG", "C")
                .env("LC_ALL", "C")
                .current_dir(home.path())
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null()),
        )
        .unwrap();
        let input = child.stdin.take().unwrap();
        let output = child.stdout.take().unwrap();
        let (sender, lines) = mpsc::channel();
        std::thread::spawn(move || {
            let mut reader = BufReader::new(output);
            loop {
                let mut line = Vec::new();
                match reader.read_until(b'\n', &mut line) {
                    Ok(0) | Err(_) => return,
                    Ok(_) => {
                        if sender.send(line).is_err() {
                            return;
                        }
                    }
                }
            }
        });
        Self {
            child,
            input,
            lines,
            _home: home,
        }
    }

    fn send(&mut self, frame: Vec<u8>) {
        self.input.write_all(&frame).unwrap();
        self.input.flush().unwrap();
    }

    fn until(&mut self, protocol: &mut CodexProtocol, wanted: impl Fn(&Event) -> bool) -> Event {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let line = self
                .lines
                .recv_timeout(remaining)
                .expect("codex app-server replied in time");
            for event in protocol.receive(&line).expect("real codex frame decodes") {
                if wanted(&event) {
                    return event;
                }
            }
        }
    }
}

impl Drop for AppServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn initialized(server: &mut AppServer) -> CodexProtocol {
    let mut protocol = CodexProtocol::new();
    server.send(protocol.initialize().unwrap());
    server.until(&mut protocol, |event| matches!(event, Event::Initialized));
    server.send(protocol.initialized().unwrap());
    assert_eq!(protocol.state(), State::Ready);
    protocol
}

#[test]
fn real_codex_app_server_frames_decode_through_initialization_and_account_read() {
    let mut server = AppServer::start();
    let mut protocol = initialized(&mut server);
    server.send(protocol.read_account(false).unwrap());
    match server.until(&mut protocol, |event| {
        matches!(event, Event::AuthControl(_))
    }) {
        Event::AuthControl(AuthControl::AccountRead { account, .. }) => assert!(account.is_none()),
        event => panic!("unexpected event {event:?}"),
    }
}

#[test]
fn real_codex_device_code_sign_in_yields_verification_url_and_user_code() {
    let mut server = AppServer::start();
    let mut protocol = initialized(&mut server);
    server.send(protocol.start_device_code_login().unwrap());
    match server.until(&mut protocol, |event| {
        matches!(event, Event::AuthControl(_))
    }) {
        Event::AuthControl(AuthControl::LoginStarted {
            challenge:
                LoginChallenge::DeviceCode {
                    login_id,
                    user_code,
                    verification_url,
                },
            ..
        }) => {
            assert!(!login_id.is_empty());
            assert!(!user_code.expose().is_empty());
            assert!(verification_url.expose().starts_with("https://"));
        }
        event => panic!("unexpected event {event:?}"),
    }
}

#[test]
fn a_real_codex_turn_keeps_running_past_a_command_output_line_over_64_kib() {
    let mut server = AppServer::start_with(&[
        "model_providers.closed={name=\"closed\",base_url=\"http://127.0.0.1:9/v1\",wire_api=\"responses\"}",
        "model_provider=\"closed\"",
    ]);
    let mut protocol = initialized(&mut server);
    let cwd = server._home.path().to_path_buf();
    server.send(protocol.start_thread(&cwd).unwrap());
    let Event::ThreadStarted { thread_id } = server.until(&mut protocol, |event| {
        matches!(event, Event::ThreadStarted { .. })
    }) else {
        unreachable!()
    };
    server.send(
        protocol
            .start_turn("Wait for the owner's shell command.")
            .unwrap(),
    );
    server.until(&mut protocol, |event| {
        matches!(event, Event::TurnStarted { .. })
    });
    let shell = serde_json::json!({
        "id": "large-output",
        "method": "thread/shellCommand",
        "params": {
            "threadId": thread_id,
            "command": "head -c 100000 /dev/zero | tr '\\0' x; echo",
        },
    });
    let mut request = serde_json::to_vec(&shell).unwrap();
    request.push(b'\n');
    server.send(request);
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut largest = 0;
    while largest <= FRAME_BYTES_MAX {
        let line = server
            .lines
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .expect("codex app-server ran the command in time");
        let value: serde_json::Value = serde_json::from_slice(&line).unwrap();
        if value["id"] == "large-output" {
            continue;
        }
        protocol.receive(&line).unwrap_or_else(|error| {
            panic!("a {}-byte codex line failed with {error:?}", line.len())
        });
        largest = largest.max(line.len());
    }
    assert_eq!(protocol.state(), State::Active);
    server.send(protocol.interrupt().unwrap());
    let ended = server.until(&mut protocol, |event| {
        matches!(event, Event::TurnCompleted { .. })
    });
    assert!(
        matches!(
            ended,
            Event::TurnCompleted {
                status: TurnStatus::Interrupted,
                ..
            }
        ),
        "{ended:?}"
    );
}
