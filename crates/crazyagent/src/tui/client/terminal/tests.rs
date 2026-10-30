use super::*;
use crate::engine::bootstrap::EngineService;
use crate::harnesses::interactive::{InteractiveCommand, Transcript};
use crate::terminal::tmux::{SESSION, SOCKET, Tmux, installed};
use crate::terminal::{FOLDER, Host};
use crate::tui::terminal_pane::TerminalPane;
use crate::tui::theme::Depth;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
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
    match next(&mut terminals) {
        Some((resized, TerminalFrame::Attached { cols, rows, .. })) => {
            assert_eq!(resized, agent(1));
            assert_eq!(
                (cols, rows),
                (100, 30),
                "a resize starts the view again at the new size"
            );
        }
        other => panic!("expected the view started again, got {other:?}"),
    }
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

fn tmux_text(tmux: &Path, socket: &Path, args: &[&str]) -> String {
    let output = crate::spawn_gate::output(
        Process::new(tmux)
            .arg("-S")
            .arg(socket)
            .args(args)
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
    )
    .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

fn tmux_rows(tmux: &Path, socket: &Path, target: &str, rows: u16) -> Vec<String> {
    let text = tmux_text(tmux, socket, &["capture-pane", "-p", "-t", target]);
    let mut lines: Vec<String> = text
        .lines()
        .map(|line| line.trim_end().to_owned())
        .collect();
    lines.resize(usize::from(rows), String::new());
    lines
}

fn pane_rows(pane: &TerminalPane) -> Vec<String> {
    let (rows, cols) = pane.size();
    let area = Rect::new(0, 0, cols, rows);
    let mut buffer = Buffer::empty(area);
    pane.draw(&mut buffer, area, Depth::NoColor, false);
    (0..rows)
        .map(|y| {
            (0..cols)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
                .trim_end()
                .to_owned()
        })
        .collect()
}

#[test]
fn a_sized_attach_starts_at_the_box_size_and_a_resize_redraws_the_screen_as_tmux_reflowed_it() {
    let root = private_root();
    let global = root.path().join("g.toml");
    std::fs::write(&global, "version = 1\ntimeout_ms = 2000\n").unwrap();
    std::fs::set_permissions(&global, std::fs::Permissions::from_mode(0o600)).unwrap();
    let tmux = installed(root.path()).expect("tmux must be installed on this machine");
    let folder = root.path().join(FOLDER);
    let socket = folder.join(SOCKET);
    let _server = Server {
        tmux: tmux.clone(),
        socket: socket.clone(),
    };
    let _launcher = spawn_shell(tmux.clone(), &folder, root.path(), &agent(1));
    let target = format!("{SESSION}:={}", agent(1));

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
    terminals.handle(Request::TerminalResize {
        agent: agent(1),
        cols: 120,
        rows: 40,
    });
    match next(&mut terminals) {
        Some((attached, TerminalFrame::Attached { cols, rows, .. })) => {
            assert_eq!(attached, agent(1));
            assert_eq!(
                (cols, rows),
                (120, 40),
                "the first screen already has the box size"
            );
        }
        other => panic!("expected the sized attach, got {other:?}"),
    }
    assert_eq!(
        tmux_text(
            &tmux,
            &socket,
            &[
                "display-message",
                "-p",
                "-t",
                &target,
                "#{window_width}x#{window_height}"
            ]
        )
        .trim(),
        "120x40"
    );
    let mut seen = Vec::new();
    assert!(output_until(&mut terminals, &mut seen, b"ready$ "));
    let long = "0123456789".repeat(15);
    terminals.handle(Request::TerminalInput {
        agent: agent(1),
        bytes: format!("echo {long}\r").into_bytes(),
    });
    assert!(
        output_until(
            &mut terminals,
            &mut seen,
            format!("\r\n{long}\r\n").as_bytes()
        ),
        "{}",
        String::from_utf8_lossy(&seen)
    );

    terminals.handle(Request::TerminalResize {
        agent: agent(1),
        cols: 60,
        rows: 20,
    });
    let mut pane = TerminalPane::new(40, 120);
    match next(&mut terminals) {
        Some((_, TerminalFrame::Attached { cols, rows, .. })) => {
            assert_eq!((cols, rows), (60, 20), "a resize starts the view again");
            pane.resize(rows, cols);
            pane.show(&[], None);
        }
        other => panic!("expected the view started again, got {other:?}"),
    }
    let deadline = Instant::now() + WAIT;
    let mut wanted = tmux_rows(&tmux, &socket, &target, 20);
    while Instant::now() < deadline && pane_rows(&pane) != wanted {
        match terminals.poll() {
            Some((_, TerminalFrame::Output { bytes })) => pane.feed(&bytes),
            Some(other) => panic!("expected output, got {other:?}"),
            None => {
                std::thread::sleep(Duration::from_millis(20));
                wanted = tmux_rows(&tmux, &socket, &target, 20);
            }
        }
    }
    let shown = pane_rows(&pane);
    assert_eq!(shown, wanted, "the TUI copy matches tmux after the resize");
    assert!(
        shown.iter().any(|row| *row == long[..60]),
        "the long line wraps at the new width: {shown:#?}"
    );

    let refused = runtime.block_on(open(
        root.path().join("e.sock"),
        Duration::from_secs(5),
        agent(1),
        None,
        Some((1, 24)),
    ));
    assert_eq!(refused.err().as_deref(), Some("terminal_size_bounds"));

    drop(terminals);
    runtime
        .block_on(async { tokio::time::timeout(WAIT, service.shutdown()).await })
        .unwrap()
        .unwrap();
}
