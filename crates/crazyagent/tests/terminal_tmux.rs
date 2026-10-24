#![cfg(unix)]

use crazyagent::harnesses::interactive::{InteractiveCommand, Transcript};
use crazyagent::terminal::tmux::{SOCKET, SOCKET_PATH_BYTES_MAX, Tmux, installed};
use crazyagent::terminal::{AgentId, FOLDER, Host, INPUT_BYTES_MAX, TerminalStream};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

struct Server {
    tmux: PathBuf,
    socket: PathBuf,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = Command::new(&self.tmux)
            .arg("-S")
            .arg(&self.socket)
            .arg("kill-server")
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

fn root() -> tempfile::TempDir {
    let root = tempfile::Builder::new()
        .prefix("ca-tmux-")
        .tempdir_in("/private/tmp")
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    root
}

fn tmux(root: &Path) -> PathBuf {
    installed(root).expect("tmux must be installed on this machine for the terminal host")
}

fn shell(cwd: &Path, env: Vec<(String, String)>, argv: &[&str]) -> InteractiveCommand {
    InteractiveCommand {
        argv: argv.iter().map(|arg| (*arg).to_owned()).collect(),
        cwd: cwd.to_path_buf(),
        env,
        transcript: Transcript::Unavailable,
    }
}

fn contains(seen: &[u8], wanted: &[u8]) -> bool {
    seen.windows(wanted.len()).any(|window| window == wanted)
}

fn read_until(stream: &mut TerminalStream, seen: &mut Vec<u8>, wanted: &[u8]) -> bool {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap();
    runtime.block_on(async {
        tokio::time::timeout(Duration::from_secs(10), async {
            while !contains(seen, wanted) {
                match stream.next().await {
                    Some(chunk) => seen.extend_from_slice(&chunk),
                    None => return false,
                }
            }
            true
        })
        .await
        .unwrap_or(false)
    })
}

fn ended(stream: &mut TerminalStream) -> bool {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap();
    runtime.block_on(async {
        tokio::time::timeout(Duration::from_secs(10), async {
            while stream.next().await.is_some() {}
        })
        .await
        .is_ok()
    })
}

#[test]
fn an_agent_terminal_takes_input_resizes_and_survives_a_new_engine_client() {
    let root = root();
    let tmux = tmux(root.path());
    let folder = root.path().join(FOLDER);
    let _server = Server {
        tmux: tmux.clone(),
        socket: folder.join(SOCKET),
    };
    let host = Tmux::open(Some(tmux.clone()), &folder);
    let agent = AgentId::team(1).unwrap();
    let command = shell(
        root.path(),
        vec![("PS1".into(), "ready$ ".into())],
        &["/bin/sh"],
    );

    let pane = host.spawn(&agent, &command).unwrap();
    assert_eq!(pane.agent, agent);
    assert!(pane.pane.starts_with('%') && pane.pid > 0, "{pane:?}");
    assert_eq!(
        host.spawn(&agent, &command).unwrap_err().0,
        "terminal_agent_running"
    );
    assert_eq!(host.alive().unwrap(), [agent.clone()]);

    let folder_metadata = std::fs::symlink_metadata(&folder).unwrap();
    assert_eq!(folder_metadata.mode() & 0o777, 0o700);
    let socket = std::fs::symlink_metadata(folder.join(SOCKET)).unwrap();
    assert_eq!(socket.uid(), folder_metadata.uid());
    assert_eq!(
        socket.mode() & 0o077,
        0,
        "only the owner can reach the server"
    );

    let mut stream = host.attach(&agent).unwrap();
    assert_eq!((stream.cols, stream.rows), (80, 24));
    assert!(stream.screen.starts_with(b"\x1b[0m\x1b[H\x1b[2J"));
    let mut seen = stream.screen.clone();
    assert!(read_until(&mut stream, &mut seen, b"ready$ "), "{seen:?}");
    host.input(&agent, b"echo hi\r").unwrap();
    assert!(
        read_until(&mut stream, &mut seen, b"\r\nhi\r\n"),
        "{}",
        String::from_utf8_lossy(&seen)
    );

    host.resize(&agent, 100, 30).unwrap();
    assert_eq!(
        host.resize(&agent, 1, 30).unwrap_err().0,
        "terminal_size_bounds"
    );
    assert_eq!(
        host.input(&agent, &[b'x'; INPUT_BYTES_MAX + 1])
            .unwrap_err()
            .0,
        "terminal_input_bounds"
    );
    drop(stream);
    drop(host);

    let again = Tmux::open(Some(tmux), &folder);
    assert_eq!(
        again.alive().unwrap(),
        [agent.clone()],
        "a new client finds the agent still running"
    );
    let mut stream = again.attach(&agent).unwrap();
    assert_eq!((stream.cols, stream.rows), (100, 30));
    assert!(
        contains(&stream.screen, b"echo hi\r\nhi"),
        "{}",
        String::from_utf8_lossy(&stream.screen)
    );

    again.kill(&agent).unwrap();
    assert!(
        ended(&mut stream),
        "the viewer's stream ends with the terminal"
    );
    assert!(again.alive().unwrap().is_empty());
    assert_eq!(
        again.attach(&agent).unwrap_err().0,
        "terminal_agent_unknown"
    );
    assert_eq!(again.kill(&agent).unwrap_err().0, "terminal_agent_unknown");
    assert_eq!(
        again.input(&agent, b"x").unwrap_err().0,
        "terminal_agent_unknown"
    );
}

#[test]
fn arguments_and_environment_reach_the_agent_exactly() {
    let root = root();
    let tmux = tmux(root.path());
    let folder = root.path().join(FOLDER);
    let _server = Server {
        tmux: tmux.clone(),
        socket: folder.join(SOCKET),
    };
    let host = Tmux::open(Some(tmux), &folder);
    let agent = AgentId::new("research-7").unwrap();
    let tricky = "a\"b\\c $HOME #x ~y'z;\nline2";
    let command = shell(
        root.path(),
        vec![("FOO".into(), "bar $baz".into())],
        &[
            "/bin/sh",
            "-c",
            "printf '%s|%s|' \"$1\" \"$FOO\"; exec cat",
            "sh",
            tricky,
        ],
    );
    host.spawn(&agent, &command).unwrap();
    let mut stream = host.attach(&agent).unwrap();
    let mut seen = stream.screen.clone();
    assert!(
        read_until(&mut stream, &mut seen, b"line2|bar $baz|"),
        "{}",
        String::from_utf8_lossy(&seen)
    );
    assert!(contains(&seen, b"a\"b\\c $HOME #x ~y'z;"));

    let other = AgentId::new("research-8").unwrap();
    host.spawn(&other, &shell(root.path(), Vec::new(), &["/bin/sh"]))
        .unwrap();
    assert_eq!(host.alive().unwrap(), [agent.clone(), other.clone()]);
    host.input(&other, b"echo only-other\r").unwrap();
    host.input(&agent, b"typed\r").unwrap();
    assert!(read_until(&mut stream, &mut seen, b"typed"));
    assert!(
        !contains(&seen, b"only-other"),
        "a viewer sees only its own agent's output"
    );
    host.kill(&other).unwrap();
    host.kill(&agent).unwrap();
}

#[test]
fn a_host_without_tmux_reports_every_call_unavailable() {
    let root = root();
    let folder = root.path().join(FOLDER);
    let agent = AgentId::team(2).unwrap();
    let command = shell(root.path(), Vec::new(), &["/bin/sh"]);
    for host in [
        Tmux::open(None, &folder),
        Tmux::open(Some(PathBuf::from("tmux")), &folder),
    ] {
        assert_eq!(
            host.spawn(&agent, &command).unwrap_err().0,
            "terminal_unavailable"
        );
        assert_eq!(host.attach(&agent).unwrap_err().0, "terminal_unavailable");
        assert_eq!(
            host.input(&agent, b"x").unwrap_err().0,
            "terminal_unavailable"
        );
        assert_eq!(
            host.resize(&agent, 80, 24).unwrap_err().0,
            "terminal_unavailable"
        );
        assert_eq!(host.kill(&agent).unwrap_err().0, "terminal_unavailable");
        assert_eq!(host.alive().unwrap_err().0, "terminal_unavailable");
    }
    assert!(!folder.exists(), "nothing is created while tmux is missing");
}

#[test]
fn a_socket_path_too_long_for_the_system_is_refused() {
    let root = root();
    let tmux = tmux(root.path());
    let deep = root.path().join("d".repeat(SOCKET_PATH_BYTES_MAX));
    let host = Tmux::open(Some(tmux), &deep);
    assert_eq!(host.alive().unwrap_err().0, "terminal_socket_path_long");
}

#[test]
fn a_shared_terminal_folder_is_refused() {
    let root = root();
    let tmux = tmux(root.path());
    let folder = root.path().join(FOLDER);
    std::fs::create_dir(&folder).unwrap();
    std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o755)).unwrap();
    let host = Tmux::open(Some(tmux), &folder);
    assert_eq!(host.alive().unwrap_err().0, "terminal_folder_unsafe");
}
