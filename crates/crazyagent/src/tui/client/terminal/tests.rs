use super::*;
use crate::engine::bootstrap::EngineService;
use crate::harnesses::interactive::{InteractiveCommand, Transcript};
use crate::terminal::tmux::{SOCKET, Tmux, installed};
use crate::terminal::{FOLDER, Host};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command as Process, Stdio};
use std::time::Instant;

const WAIT: Duration = Duration::from_secs(10);

struct Server {
    tmux: PathBuf,
    socket: PathBuf,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = crate::spawn_gate::status(
            Process::new(&self.tmux)
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

fn private_root() -> tempfile::TempDir {
    let root = tempfile::Builder::new()
        .prefix("ca-tui-term-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    root
}

fn agent(number: i64) -> AgentId {
    AgentId::team(number).unwrap()
}

fn next(terminals: &mut Terminals) -> Option<(AgentId, TerminalFrame)> {
    let deadline = Instant::now() + WAIT;
    while Instant::now() < deadline {
        if let Some(frame) = terminals.poll() {
            return Some(frame);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    None
}

fn output_until(terminals: &mut Terminals, seen: &mut Vec<u8>, wanted: &[u8]) -> bool {
    let deadline = Instant::now() + WAIT;
    while Instant::now() < deadline {
        if seen.windows(wanted.len()).any(|window| window == wanted) {
            return true;
        }
        match terminals.poll() {
            Some((_, TerminalFrame::Output { bytes })) => seen.extend_from_slice(&bytes),
            Some(_) => return false,
            None => std::thread::sleep(Duration::from_millis(10)),
        }
    }
    false
}

fn bounded(code: &str) -> bool {
    !code.is_empty()
        && code
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

#[test]
fn an_attach_without_an_engine_is_rejected_with_a_plain_code_and_ends_the_session() {
    let root = private_root();
    let mut terminals = Terminals::new(root.path().join("e.sock"), Duration::from_secs(2));
    terminals.handle(Request::TerminalAttach { agent: agent(1) });
    assert!(terminals.live());
    match next(&mut terminals) {
        Some((attached, TerminalFrame::Rejected { code })) => {
            assert_eq!(attached, agent(1));
            assert!(bounded(&code), "{code}");
        }
        other => panic!("expected a rejection, got {other:?}"),
    }
    assert!(!terminals.live());
    assert!(terminals.poll().is_none());
}

#[test]
fn keys_and_sizes_without_an_attached_session_are_dropped() {
    let root = private_root();
    let mut terminals = Terminals::new(root.path().join("e.sock"), Duration::from_secs(2));
    terminals.handle(Request::TerminalInput {
        agent: agent(1),
        bytes: b"ls\r".to_vec(),
    });
    terminals.handle(Request::TerminalResize {
        agent: agent(1),
        cols: 80,
        rows: 24,
    });
    terminals.handle(Request::TerminalDetach { agent: agent(1) });
    assert!(!terminals.live());
    assert!(terminals.poll().is_none());
}

#[test]
#[should_panic(expected = "not a terminal request")]
fn a_request_for_the_engine_connection_is_not_a_terminal_request() {
    let root = private_root();
    let mut terminals = Terminals::new(root.path().join("e.sock"), Duration::from_secs(2));
    terminals.handle(Request::Profile);
}

fn spawn_shell(tmux: PathBuf, folder: &Path, root: &Path, agent: &AgentId) -> Tmux {
    let command = InteractiveCommand {
        argv: vec!["/bin/sh".into()],
        cwd: root.to_path_buf(),
        env: vec![("PS1".into(), "ready$ ".into())],
        transcript: Transcript::Unavailable,
    };
    let launcher = Tmux::open(Some(tmux), folder);
    launcher.spawn(agent, &command).unwrap();
    launcher
}

#[test]
fn the_tui_types_into_resizes_and_detaches_from_a_live_agent_terminal() {
    let root = private_root();
    let global = root.path().join("g.toml");
    std::fs::write(&global, "version = 1\ntimeout_ms = 2000\n").unwrap();
    std::fs::set_permissions(&global, std::fs::Permissions::from_mode(0o600)).unwrap();
    let tmux = installed(root.path()).expect("tmux must be installed on this machine");
    let folder = root.path().join(FOLDER);
    let _server = Server {
        tmux: tmux.clone(),
        socket: folder.join(SOCKET),
    };
    let launcher = spawn_shell(tmux, &folder, root.path(), &agent(1));

    let runtime = tokio::runtime::Runtime::new().unwrap();
    let (_stop, stopped) = tokio::sync::watch::channel(false);
    let mut service = runtime
        .block_on(EngineService::start_with_settings_sources(
            root.path(),
            "e.sock",
            &global,
            &root.path().join("p.toml"),
            &stopped,
        ))
        .unwrap();

    let mut terminals = Terminals::new(root.path().join("e.sock"), Duration::from_secs(5));
    terminals.handle(Request::TerminalAttach { agent: agent(1) });
    match next(&mut terminals) {
        Some((
            attached,
            TerminalFrame::Attached {
                agent_id,
                cols,
                rows,
            },
        )) => {
            assert_eq!((attached, agent_id), (agent(1), agent(1)));
            assert_eq!((cols, rows), (80, 24));
        }
        other => panic!("expected the attach, got {other:?}"),
    }
    let mut seen = Vec::new();
    assert!(output_until(&mut terminals, &mut seen, b"ready$ "));
    terminals.handle(Request::TerminalInput {
        agent: agent(1),
        bytes: b"echo hi\r".to_vec(),
    });
    assert!(
        output_until(&mut terminals, &mut seen, b"\r\nhi\r\n"),
        "{}",
        String::from_utf8_lossy(&seen)
    );
    terminals.handle(Request::TerminalInput {
        agent: agent(2),
        bytes: b"echo other\r".to_vec(),
    });
    terminals.handle(Request::TerminalResize {
        agent: agent(1),
        cols: 100,
        rows: 30,
    });
    terminals.handle(Request::TerminalDetach { agent: agent(1) });
    assert!(!terminals.live());
    assert!(
        terminals.poll().is_none(),
        "a detached session sends nothing"
    );

    terminals.handle(Request::TerminalAttach { agent: agent(1) });
    match next(&mut terminals) {
        Some((_, TerminalFrame::Attached { cols, rows, .. })) => {
            assert_eq!(
                (cols, rows),
                (100, 30),
                "the resize reached the terminal before the detach"
            );
        }
        other => panic!("expected the second attach, got {other:?}"),
    }
    let mut screen = Vec::new();
    assert!(
        output_until(&mut terminals, &mut screen, b"echo hi\r\nhi"),
        "{}",
        String::from_utf8_lossy(&screen)
    );
    assert!(
        !String::from_utf8_lossy(&screen).contains("other"),
        "input for another agent never reached this terminal"
    );

    terminals.handle(Request::TerminalAttach { agent: agent(9) });
    match next(&mut terminals) {
        Some((unknown, TerminalFrame::Rejected { code })) => {
            assert_eq!(unknown, agent(9));
            assert_eq!(code, "terminal_agent_unknown");
        }
        other => panic!("expected an unknown agent, got {other:?}"),
    }

    terminals.handle(Request::TerminalAttach { agent: agent(1) });
    assert!(matches!(
        next(&mut terminals),
        Some((_, TerminalFrame::Attached { .. }))
    ));
    launcher.kill(&agent(1)).unwrap();
    let deadline = Instant::now() + WAIT;
    let reason = loop {
        assert!(Instant::now() < deadline, "no close within ten seconds");
        match terminals.poll() {
            Some((_, TerminalFrame::Closed { reason })) => break reason,
            Some(_) => {}
            None => std::thread::sleep(Duration::from_millis(10)),
        }
    };
    assert_eq!(reason, "terminal_ended");
    assert!(!terminals.live());

    terminals.handle(Request::TerminalInput {
        agent: agent(1),
        bytes: b"echo gone\r".to_vec(),
    });
    assert!(
        !terminals.live(),
        "input to an ended terminal opens nothing"
    );
    assert!(terminals.poll().is_none());

    terminals.handle(Request::TerminalAttach { agent: agent(1) });
    match next(&mut terminals) {
        Some((dead, TerminalFrame::Rejected { code })) => {
            assert_eq!(dead, agent(1));
            assert!(
                matches!(code.as_str(), "terminal_agent_unknown" | "terminal_ended"),
                "{code}"
            );
        }
        other => panic!("expected the dead agent refused, got {other:?}"),
    }
    assert!(!terminals.live());

    drop(terminals);
    runtime
        .block_on(async { tokio::time::timeout(WAIT, service.shutdown()).await })
        .unwrap()
        .unwrap();
}
