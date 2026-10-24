#![cfg(target_os = "windows")]

use crazyagent::harnesses::interactive::{InteractiveCommand, Transcript};
use crazyagent::terminal::windows::ConptyHost;
use crazyagent::terminal::{AgentId, Host, INPUT_BYTES_MAX, SIZE_MAX, TerminalStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::time::{Duration, Instant};

fn cmd() -> PathBuf {
    PathBuf::from(std::env::var_os("SystemRoot").unwrap())
        .join("System32")
        .join("cmd.exe")
}

fn command(args: &[&str], cwd: &Path) -> InteractiveCommand {
    InteractiveCommand {
        argv: std::iter::once(cmd().display().to_string())
            .chain(args.iter().map(|arg| arg.to_string()))
            .collect(),
        cwd: cwd.to_path_buf(),
        env: vec![("PANE_MARK".into(), "pane-env".into())],
        transcript: Transcript::Unavailable,
    }
}

fn forward(mut stream: TerminalStream) -> Receiver<Vec<u8>> {
    let (sender, receiver) = channel();
    std::thread::spawn(move || {
        while let Some(chunk) = stream.next_blocking() {
            if sender.send(chunk).is_err() {
                break;
            }
        }
    });
    receiver
}

fn read_until(chunks: &Receiver<Vec<u8>>, needle: &str, limit: Duration) -> String {
    let deadline = Instant::now() + limit;
    let mut seen = String::new();
    while !seen.contains(needle) {
        let left = deadline.saturating_duration_since(Instant::now());
        let chunk = chunks
            .recv_timeout(left)
            .unwrap_or_else(|_| panic!("no {needle:?} in {seen:?}"));
        seen.push_str(&String::from_utf8_lossy(&chunk));
    }
    seen
}

#[test]
fn a_conpty_terminal_runs_streams_takes_input_and_ends() {
    let temp = tempfile::tempdir().unwrap();
    let host = ConptyHost::new();
    let agent = AgentId::team(7).unwrap();
    let shell = command(&["/Q", "/K"], temp.path());
    let pane = host.spawn(&agent, &shell).unwrap();
    assert_eq!(pane.agent, agent);
    assert!(pane.pid > 0);
    assert_eq!(host.pane(&agent), Some(pane.pane.clone()));
    assert_eq!(host.alive().unwrap(), vec![agent.clone()]);
    assert_eq!(
        host.spawn(&agent, &shell).unwrap_err().0,
        "terminal_agent_running"
    );
    for (name, value) in [
        ("GH_TOKEN", "planted"),
        ("SSH_AUTH_SOCK", "planted"),
        ("ANTHROPIC_API_KEY", "sk-ant-planted"),
    ] {
        let mut secret = command(&["/Q", "/K"], temp.path());
        secret.env.push((name.into(), value.into()));
        assert_eq!(
            host.spawn(&AgentId::team(8).unwrap(), &secret)
                .unwrap_err()
                .0,
            "terminal_command_invalid",
            "{name}"
        );
    }
    let stream = host.attach(&agent).unwrap();
    assert_eq!((stream.cols, stream.rows), (80, 24));
    let chunks = forward(stream);
    host.input(&agent, b"echo %PANE_MARK%-%USERNAME%-done\r")
        .unwrap();
    read_until(&chunks, "pane-env-%USERNAME%-done", Duration::from_secs(30));
    host.resize(&agent, 100, 30).unwrap();
    let again = host.attach(&agent).unwrap();
    assert_eq!((again.cols, again.rows), (100, 30));
    assert!(String::from_utf8_lossy(&again.screen).contains("pane-env-%USERNAME%-done"));
    for (cols, rows) in [(1, 30), (100, SIZE_MAX + 1)] {
        assert_eq!(
            host.resize(&agent, cols, rows).unwrap_err().0,
            "terminal_size_invalid"
        );
    }
    assert_eq!(
        host.input(&agent, &vec![b'a'; INPUT_BYTES_MAX + 1])
            .unwrap_err()
            .0,
        "terminal_input_too_long"
    );
    host.kill(&agent).unwrap();
    assert!(host.alive().unwrap().is_empty());
    assert_eq!(host.attach(&agent).unwrap_err().0, "terminal_agent_unknown");
    assert_eq!(host.kill(&agent).unwrap_err().0, "terminal_agent_unknown");
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match chunks.recv_timeout(left) {
            Ok(_) => continue,
            Err(RecvTimeoutError::Disconnected) => break,
            Err(RecvTimeoutError::Timeout) => panic!("the viewer stream did not end"),
        }
    }
    let mut relative = command(&[], temp.path());
    relative.argv[0] = "cmd.exe".into();
    assert_eq!(
        host.spawn(&AgentId::team(8).unwrap(), &relative)
            .unwrap_err()
            .0,
        "terminal_command_invalid"
    );
}
