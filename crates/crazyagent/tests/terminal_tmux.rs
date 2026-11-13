#![cfg(unix)]

use crazyagent::harnesses::interactive::{InteractiveCommand, Transcript};
use crazyagent::harnesses::terminal_session::end_left_pane;
use crazyagent::terminal::buffer::Chunk;
use crazyagent::terminal::tmux::{SOCKET, Tmux, installed};
use crazyagent::terminal::{
    AgentId, FOLDER, Host, INPUT_BYTES_MAX, SCROLLBACK_LINES, TerminalStream,
    VIEWER_BUFFER_BYTES_MAX, VIEWERS_MAX, plain_reason, platform, quit, renew_once,
};
use std::collections::BTreeMap;
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime};

const PANE_ENV_ROOT: &str = "PANE_ENV_ROOT";
const PANE_ENV_OUT: &str = "PANE_ENV_OUT";
const PLANTED_TERMINAL: [(&str, &str); 4] = [
    ("COLORTERM", "planted-colorterm"),
    ("TERM_PROGRAM", "planted-term-program"),
    ("SHELL", "/bin/planted-shell"),
    ("TERM", "planted-term"),
];
const PLANTED: [(&str, &str); 7] = [
    ("SSH_AUTH_SOCK", "/private/tmp/planted-agent.sock"),
    ("GH_TOKEN", "planted-gh-token"),
    ("GITHUB_TOKEN", "planted-github-token"),
    ("ANTHROPIC_API_KEY", "sk-ant-planted-key"),
    ("OPENAI_API_KEY", "sk-planted-openai-key"),
    ("AWS_SECRET_ACCESS_KEY", "planted-aws-secret"),
    ("GIT_ASKPASS", "/private/tmp/planted-askpass"),
];

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

fn root() -> tempfile::TempDir {
    let root = tempfile::Builder::new()
        .prefix("ca-tmux-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
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
                    Some(Chunk::Output(chunk)) => seen.extend_from_slice(&chunk),
                    Some(Chunk::Behind) | None => return false,
                }
            }
            true
        })
        .await
        .unwrap_or(false)
    })
}

fn read_behind(stream: &mut TerminalStream, seen: &mut Vec<u8>) -> bool {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap();
    runtime.block_on(async {
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                match stream.next().await {
                    Some(Chunk::Output(chunk)) => seen.extend_from_slice(&chunk),
                    Some(Chunk::Behind) => return true,
                    None => return false,
                }
            }
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
    assert_eq!(host.alive().unwrap(), std::slice::from_ref(&agent));

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
        std::slice::from_ref(&agent),
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
            "printf '%s|%s|%s|' \"$1\" \"$FOO\" \"${TMUX-unset}\"; exec cat",
            "sh",
            tricky,
        ],
    );
    host.spawn(&agent, &command).unwrap();
    let mut stream = host.attach(&agent).unwrap();
    let mut seen = stream.screen.clone();
    assert!(
        read_until(&mut stream, &mut seen, b"line2|bar $baz|unset|"),
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

struct Closing<'a>(&'a Tmux);

impl Drop for Closing<'_> {
    fn drop(&mut self) {
        let _ = self.0.close(&[]);
    }
}

#[test]
fn a_terminal_folder_too_long_for_a_socket_address_still_runs_agents() {
    let root = root();
    let tmux = tmux(root.path());
    let deep = root.path().join("d".repeat(110));
    let socket = deep.join(SOCKET);
    assert!(socket.as_os_str().len() > 120);
    let host = Tmux::open(Some(tmux.clone()), &deep);
    let _closing = Closing(&host);
    let agent = AgentId::team(1).unwrap();
    let command = shell(root.path(), Vec::new(), &["/bin/sh"]);
    host.spawn(&agent, &command).unwrap();
    assert_eq!(host.alive().unwrap(), std::slice::from_ref(&agent));
    let metadata = std::fs::symlink_metadata(&socket).unwrap();
    assert!(metadata.file_type().is_socket());
    assert_eq!(
        metadata.mode() & 0o077,
        0,
        "only the owner can reach the server"
    );
    let again = Tmux::open(Some(tmux), &deep);
    assert_eq!(
        again.alive().unwrap(),
        std::slice::from_ref(&agent),
        "a new client reaches the same server"
    );
    host.close(&[]).unwrap();
    assert!(!socket.exists());
}

#[test]
fn a_shared_terminal_folder_is_refused() {
    let root = root();
    let tmux = tmux(root.path());
    let folder = root.path().join(FOLDER);
    std::fs::create_dir(&folder).unwrap();
    std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o755)).unwrap();
    let host = Tmux::open(Some(tmux), &folder);
    let agent = AgentId::team(1).unwrap();
    let command = shell(root.path(), Vec::new(), &["/bin/sh"]);
    assert_eq!(
        host.spawn(&agent, &command).unwrap_err().0,
        "terminal_folder_unsafe"
    );
}

fn option(tmux: &Path, socket: &Path, name: &str) -> String {
    let output = crazyagent::spawn_gate::output(
        Command::new(tmux)
            .arg("-S")
            .arg(socket)
            .args(["show-options", "-gv", name])
            .env_clear()
            .stdin(Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped()),
    )
    .unwrap();
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

#[test]
fn panes_have_a_scrollback_cap_and_close_when_their_process_ends() {
    let root = root();
    let tmux = tmux(root.path());
    let folder = root.path().join(FOLDER);
    let socket = folder.join(SOCKET);
    let _server = Server {
        tmux: tmux.clone(),
        socket: socket.clone(),
    };
    let host = Tmux::open(Some(tmux.clone()), &folder);
    let agent = AgentId::team(3).unwrap();
    host.spawn(
        &agent,
        &shell(root.path(), Vec::new(), &["/bin/sh", "-c", "exec cat"]),
    )
    .unwrap();
    assert_eq!(
        option(&tmux, &socket, "history-limit"),
        SCROLLBACK_LINES.to_string()
    );
    let mut stream = host.attach(&agent).unwrap();
    host.input(&agent, b"\x04").unwrap();
    assert!(
        ended(&mut stream),
        "the stream ends when the agent's process ends"
    );
    assert!(
        host.alive().unwrap().is_empty(),
        "a dead pane does not stay open"
    );
    let refused = host.attach(&agent).unwrap_err().0;
    assert_eq!(refused, "terminal_agent_unknown");
    assert_eq!(
        plain_reason(&refused),
        "This agent has stopped, so its terminal is closed."
    );
    assert_eq!(
        host.input(&agent, b"x").unwrap_err().0,
        "terminal_agent_unknown"
    );
}

#[test]
fn a_viewer_that_falls_behind_reads_behind_and_a_fresh_view_draws_the_screen_now() {
    let root = root();
    let tmux = tmux(root.path());
    let folder = root.path().join(FOLDER);
    let _server = Server {
        tmux: tmux.clone(),
        socket: folder.join(SOCKET),
    };
    let host = Tmux::open(Some(tmux), &folder);
    let agent = AgentId::team(4).unwrap();
    host.spawn(&agent, &shell(root.path(), Vec::new(), &["/bin/sh"]))
        .unwrap();
    let mut stream = host.attach(&agent).unwrap();
    let flood = VIEWER_BUFFER_BYTES_MAX * 3;
    host.input(
        &agent,
        format!("head -c {flood} /dev/zero | tr '\\0' a; echo; echo flood-$((6*7))-done\r")
            .as_bytes(),
    )
    .unwrap();
    std::thread::sleep(Duration::from_secs(5));
    let mut seen = Vec::new();
    assert!(
        read_behind(&mut stream, &mut seen),
        "a viewer more than its buffer behind reads Behind instead of being dropped"
    );
    assert!(
        !contains(&seen, b"bytes of output skipped"),
        "no viewer ever reads a skipped-output line"
    );
    assert!(seen.len() < flood, "the output the viewer could not keep was not read");
    assert!(ended(&mut stream), "a viewer that read Behind reads its end next");
    let fresh = host.attach(&agent).unwrap();
    assert!(
        contains(&fresh.redraw(), b"flood-42-done"),
        "a fresh view draws the screen as it is now"
    );
    host.kill(&agent).unwrap();
}

#[test]
fn the_screen_on_attach_keeps_the_spaces_the_program_wrote() {
    let root = root();
    let tmux = tmux(root.path());
    let folder = root.path().join(FOLDER);
    let _server = Server {
        tmux: tmux.clone(),
        socket: folder.join(SOCKET),
    };
    let host = Tmux::open(Some(tmux), &folder);
    let agent = AgentId::team(6).unwrap();
    host.spawn(
        &agent,
        &shell(
            root.path(),
            vec![("PS1".into(), "spaced$ ".into())],
            &["/bin/sh"],
        ),
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let screen = loop {
        let stream = host.attach(&agent).unwrap();
        if contains(&stream.screen, b"spaced$") {
            break stream.screen;
        }
        assert!(Instant::now() < deadline, "the shell never drew its prompt");
        std::thread::sleep(Duration::from_millis(100));
    };
    assert!(
        contains(&screen, b"spaced$ \r\n"),
        "the prompt keeps its trailing space and no unused cell follows it: {}",
        String::from_utf8_lossy(&screen)
    );
    host.kill(&agent).unwrap();
}

fn attach_screen(host: &Tmux, agent: &AgentId, drawn: &[u8]) -> Vec<u8> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let stream = host.attach(agent).unwrap();
        if contains(&stream.screen, drawn) {
            return stream.screen;
        }
        assert!(Instant::now() < deadline, "the pane never drew its text");
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn the_screen_on_attach_turns_on_the_input_modes_the_agent_app_set() {
    let root = root();
    let tmux = tmux(root.path());
    let folder = root.path().join(FOLDER);
    let _server = Server {
        tmux: tmux.clone(),
        socket: folder.join(SOCKET),
    };
    let host = Tmux::open(Some(tmux), &folder);
    let moded = AgentId::team(7).unwrap();
    host.spawn(
        &moded,
        &shell(
            root.path(),
            Vec::new(),
            &[
                "/bin/sh",
                "-c",
                "printf '\\033[?2004h\\033[?1hmodes-set'; exec cat",
            ],
        ),
    )
    .unwrap();
    let plain = AgentId::team(8).unwrap();
    host.spawn(
        &plain,
        &shell(
            root.path(),
            Vec::new(),
            &["/bin/sh", "-c", "printf 'modes-off'; exec cat"],
        ),
    )
    .unwrap();

    let screen = attach_screen(&host, &moded, b"modes-set");
    assert!(
        contains(&screen, b"\x1b[?2004h"),
        "a view attaching after the app turned bracketed paste on is told so: {:?}",
        String::from_utf8_lossy(&screen)
    );
    assert!(
        contains(&screen, b"\x1b[?1h"),
        "and gets the app's cursor key mode: {:?}",
        String::from_utf8_lossy(&screen)
    );
    let screen = attach_screen(&host, &plain, b"modes-off");
    assert!(!contains(&screen, b"\x1b[?2004h"));
    assert!(!contains(&screen, b"\x1b[?1h"));
    host.kill(&moded).unwrap();
    host.kill(&plain).unwrap();
}

#[test]
fn closing_the_host_ends_every_terminal_and_the_server_unless_one_is_kept() {
    let root = root();
    let tmux = tmux(root.path());
    let folder = root.path().join(FOLDER);
    let socket = folder.join(SOCKET);
    let _server = Server {
        tmux: tmux.clone(),
        socket: socket.clone(),
    };
    let host = Tmux::open(Some(tmux.clone()), &folder);
    let master = AgentId::team(1).unwrap();
    let sub = AgentId::team(2).unwrap();
    for agent in [&master, &sub] {
        host.spawn(agent, &shell(root.path(), Vec::new(), &["/bin/sh"]))
            .unwrap();
    }
    host.close(std::slice::from_ref(&master)).unwrap();
    assert_eq!(
        host.alive().unwrap(),
        std::slice::from_ref(&master),
        "the kept master stays"
    );
    drop(host);

    let again = Tmux::open(Some(tmux.clone()), &folder);
    assert_eq!(again.alive().unwrap(), std::slice::from_ref(&master));
    again.close(&[]).unwrap();
    assert!(
        !socket.exists(),
        "close returns only once the server is gone and its socket removed"
    );
    assert!(again.alive().unwrap().is_empty());
    assert!(
        !socket.exists(),
        "listing after a clean quit starts no server"
    );
    let listed = crazyagent::spawn_gate::output(
        Command::new(&tmux)
            .arg("-S")
            .arg(&socket)
            .arg("list-sessions")
            .env_clear()
            .stdin(Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped()),
    )
    .unwrap();
    assert!(
        !listed.status.success(),
        "no tmux session is left after a clean quit: {}",
        String::from_utf8_lossy(&listed.stdout)
    );

    let unused = root.path().join("unused");
    let idle = Tmux::open(Some(tmux), &unused);
    assert!(idle.alive().unwrap().is_empty());
    assert_eq!(
        idle.attach(&master).unwrap_err().0,
        "terminal_agent_unknown"
    );
    idle.close(&[]).unwrap();
    assert!(
        !unused.exists(),
        "listing or closing a host that never started starts nothing"
    );
}

struct Orphan {
    pid: u32,
    socket: PathBuf,
}

impl Drop for Orphan {
    fn drop(&mut self) {
        let named = crazyagent::spawn_gate::output(
            Command::new("/bin/ps")
                .args(["-o", "command=", "-p", &self.pid.to_string()])
                .env_clear()
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null()),
        )
        .is_ok_and(|output| {
            String::from_utf8_lossy(&output.stdout).contains(&*self.socket.to_string_lossy())
        });
        if named {
            let _ = crazyagent::spawn_gate::status(
                Command::new("/bin/kill")
                    .args(["-s", "TERM", &self.pid.to_string()])
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null()),
            );
        }
    }
}

fn process_running(pid: u32) -> bool {
    crazyagent::spawn_gate::output(
        Command::new("/bin/ps")
            .args(["-o", "stat=", "-p", &pid.to_string()])
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null()),
    )
    .is_ok_and(|output| {
        output.status.success()
            && !String::from_utf8_lossy(&output.stdout)
                .trim_start()
                .starts_with('Z')
    })
}

fn ends(pids: &[u32]) -> bool {
    let deadline = Instant::now() + Duration::from_secs(10);
    while pids.iter().any(|pid| process_running(*pid)) {
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    true
}

fn server_pid(tmux: &Path, socket: &Path) -> u32 {
    tmux_reply(tmux, socket, &["display-message", "-p", "#{pid}"])
        .trim()
        .parse()
        .unwrap()
}

/// PR-3 (rv-code final review of fix/rv-ds-process): ending a leftover pane whose window is
/// already gone names no process to kill, although tmux answers a display for a missing
/// target with another pane. The engine's holder and another agent's pane keep running, and
/// the call reports the pane unknown.
#[test]
fn ending_a_gone_leftover_pane_kills_no_other_pane() {
    let root = root();
    let tmux = tmux(root.path());
    let folder = root.path().join(FOLDER);
    let socket = folder.join(SOCKET);
    let _server = Server {
        tmux: tmux.clone(),
        socket: socket.clone(),
    };
    let host = Tmux::open(Some(tmux.clone()), &folder);
    let gone = AgentId::team(3).unwrap();
    let other = AgentId::team(4).unwrap();
    let sleeper = shell(
        root.path(),
        Vec::new(),
        &["/bin/sh", "-c", "exec /bin/sleep 300"],
    );
    host.spawn(&gone, &sleeper).unwrap();
    let other_pid = host.spawn(&other, &sleeper).unwrap().pid;
    let holder_pid: u32 = tmux_reply(
        &tmux,
        &socket,
        &[
            "list-panes",
            "-t",
            "crazyagent:=_crazyagent",
            "-F",
            "#{pane_pid}",
        ],
    )
    .trim()
    .parse()
    .unwrap();
    host.kill(&gone).unwrap();

    let ended = end_left_pane(&host, &gone, "test").map_err(|problem| problem.0);

    assert!(
        process_running(holder_pid),
        "ending a gone pane killed the engine's tmux holder"
    );
    assert!(
        process_running(other_pid),
        "ending a gone pane killed another agent's pane"
    );
    assert_eq!(ended, Err("terminal_agent_unknown".to_owned()));
    host.kill(&other).unwrap();
}

/// A process a test started that the code under test should end, killed when the test ends
/// either way while `ps` still names it by its marker, so a red run leaves nothing running.
struct Stray {
    pid: u32,
    marker: &'static str,
}

impl Drop for Stray {
    fn drop(&mut self) {
        let named = crazyagent::spawn_gate::output(
            Command::new("/bin/ps")
                .args(["-o", "command=", "-p", &self.pid.to_string()])
                .env_clear()
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null()),
        )
        .is_ok_and(|output| String::from_utf8_lossy(&output.stdout).contains(self.marker));
        if named {
            let _ = crazyagent::spawn_gate::status(
                Command::new("/bin/kill")
                    .args(["-KILL", &self.pid.to_string()])
                    .env_clear()
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null()),
            );
        }
    }
}

/// B19L-H1 (rv-code r1 of chore/b19-line): the engine's production host names a leftover
/// pane's first process, so ending the pane also ends a background process it started that
/// ignores hangup and terminate, and the cleanup is confirmed.
#[test]
fn ending_a_leftover_pane_through_the_engine_host_ends_what_it_started() {
    let root = root();
    let tmux = tmux(root.path());
    let path = root.path().canonicalize().unwrap();
    let _server = Server {
        tmux: tmux.clone(),
        socket: path.join(FOLDER).join(SOCKET),
    };
    let host = crazyagent::terminal::tmux_host(&path, Some(tmux));
    let agent = AgentId::team(5).unwrap();
    let written = path.join("child.pid");
    let script = format!(
        "(trap '' HUP TERM; exec /bin/sleep 317) < /dev/null > /dev/null 2>&1 & echo $! > {}; exec /bin/sleep 300",
        written.display()
    );
    let first = host
        .spawn(
            &agent,
            &shell(&path, Vec::new(), &["/bin/sh", "-c", &script]),
        )
        .unwrap()
        .pid;
    let child = Stray {
        pid: wait_text(&written).trim().parse().unwrap(),
        marker: "sleep 317",
    };
    assert!(process_running(child.pid));

    let named = host.pane_pid(&agent).map_err(|problem| problem.0);
    let ended = end_left_pane(host.as_ref(), &agent, "test").map_err(|problem| problem.0);

    assert_eq!(ended, Ok(true));
    assert!(
        ends(&[child.pid]),
        "a background process that ignores hangup outlived its ended pane"
    );
    assert_eq!(named, Ok(Some(first)));
    assert!(!host.alive().unwrap().contains(&agent));
}

/// B19L-H1: an agent's window on the engine's server can hold panes its app split off; the
/// engine's host names the window's first pane, the agent's own, rather than refusing.
#[test]
fn the_engine_host_names_the_first_pane_of_a_split_agent_window() {
    let root = root();
    let tmux = tmux(root.path());
    let path = root.path().canonicalize().unwrap();
    let socket = path.join(FOLDER).join(SOCKET);
    let _server = Server {
        tmux: tmux.clone(),
        socket: socket.clone(),
    };
    let host = crazyagent::terminal::tmux_host(&path, Some(tmux.clone()));
    let agent = AgentId::team(6).unwrap();
    let first = host
        .spawn(
            &agent,
            &shell(&path, Vec::new(), &["/bin/sh", "-c", "exec /bin/sleep 300"]),
        )
        .unwrap()
        .pid;
    tmux_reply(
        &tmux,
        &socket,
        &[
            "split-window",
            "-d",
            "-t",
            &format!("crazyagent:={agent}"),
            "exec /bin/sleep 301",
        ],
    );
    let panes = host.panes(&agent).unwrap();
    assert_eq!(panes.len(), 2, "{panes:?}");
    assert!(panes.iter().any(|pane| pane.pid != first), "{panes:?}");

    assert_eq!(
        host.pane_pid(&agent).map_err(|problem| problem.0),
        Ok(Some(first))
    );
    host.kill(&agent).unwrap();
}

#[test]
fn a_server_whose_engine_home_is_deleted_ends_with_its_agents() {
    let root = root();
    let tmux = tmux(root.path());
    let home = root.path().join("home");
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&home)
        .unwrap();
    let folder = home.join(FOLDER);
    let socket = folder.join(SOCKET);
    let host = Tmux::open(Some(tmux.clone()), &folder);
    let agent = AgentId::team(1).unwrap();
    let pane = host
        .spawn(&agent, &shell(root.path(), Vec::new(), &["/bin/sh"]))
        .unwrap();
    let server = server_pid(&tmux, &socket);
    let _orphan = Orphan {
        pid: server,
        socket: socket.clone(),
    };
    drop(host);
    assert!(
        process_running(server) && process_running(pane.pid),
        "an engine that stops without quitting leaves its agents for the next engine"
    );

    std::fs::remove_dir_all(&home).unwrap();
    assert!(
        ends(&[server, pane.pid]),
        "a server no engine can reach ends by itself once its engine home is gone, and its agents with it"
    );
}

#[test]
fn an_engine_stop_keeps_the_server_only_while_a_team_agent_lives_on() {
    let root = root();
    let tmux = tmux(root.path());
    let folder = root.path().join(FOLDER);
    let socket = folder.join(SOCKET);
    let _server = Server {
        tmux: tmux.clone(),
        socket: socket.clone(),
    };
    let host = Tmux::open(Some(tmux.clone()), &folder);
    let team = AgentId::team(1).unwrap();
    let research = AgentId::new("research-7-1").unwrap();
    let mut panes = Vec::new();
    for agent in [&team, &research] {
        let pane = host
            .spawn(agent, &shell(root.path(), Vec::new(), &["/bin/sh"]))
            .unwrap();
        panes.push(pane.pid);
    }
    quit(&host, true).unwrap();
    assert_eq!(
        host.alive().unwrap(),
        std::slice::from_ref(&team),
        "a team agent lives on for the next engine to take over; every other terminal ends"
    );
    assert!(ends(&panes[1..]), "the research agent's process ended");
    drop(host);

    let next = Tmux::open(Some(tmux.clone()), &folder);
    assert_eq!(
        next.alive().unwrap(),
        std::slice::from_ref(&team),
        "the next engine finds the team agent"
    );
    let server = server_pid(&tmux, &socket);
    next.kill(&team).unwrap();
    quit(&next, true).unwrap();
    assert!(
        !socket.exists(),
        "with no team agent left the stop ends the server and removes its socket"
    );
    assert!(ends(&[server, panes[0]]), "the server process is gone");
    assert!(next.alive().unwrap().is_empty());
    assert!(!socket.exists(), "listing after the stop starts no server");
}

/// FBTMUX-P1, Stop: with team agents set to stop when CrazyAgent quits, an engine stop ends
/// every terminal, a team agent's included, and the server.
#[test]
fn an_engine_stop_set_to_stop_team_agents_ends_them_and_the_server() {
    let root = root();
    let tmux = tmux(root.path());
    let folder = root.path().join(FOLDER);
    let socket = folder.join(SOCKET);
    let _server = Server {
        tmux: tmux.clone(),
        socket: socket.clone(),
    };
    let host = Tmux::open(Some(tmux.clone()), &folder);
    let team = AgentId::team(1).unwrap();
    let research = AgentId::new("research-7-1").unwrap();
    let mut pids = Vec::new();
    for agent in [&team, &research] {
        let pane = host
            .spawn(agent, &shell(root.path(), Vec::new(), &["/bin/sh"]))
            .unwrap();
        pids.push(pane.pid);
    }
    pids.push(server_pid(&tmux, &socket));
    quit(&host, false).unwrap();
    assert!(host.alive().unwrap().is_empty());
    assert!(
        !socket.exists(),
        "with no team agent kept the stop ends the server and removes its socket"
    );
    assert!(ends(&pids), "the agents and the server process are gone");
    let next = Tmux::open(Some(tmux.clone()), &folder);
    assert!(
        next.alive().unwrap().is_empty(),
        "the next engine finds none"
    );
}

/// FBTMUX-P1: renewing sets the deadline the server's holder reads, its socket's
/// modification time, to the time given, and keeps the server and its agent; a host with
/// no server renews nothing and starts none.
#[test]
fn renewing_moves_the_server_deadline_to_the_time_given() {
    let root = root();
    let tmux = tmux(root.path());
    let folder = root.path().join(FOLDER);
    let socket = folder.join(SOCKET);
    let _server = Server {
        tmux: tmux.clone(),
        socket: socket.clone(),
    };
    let host = Tmux::open(Some(tmux.clone()), &folder);
    let until = SystemTime::UNIX_EPOCH + Duration::from_secs(4_000_000_000);
    host.renew(until).unwrap();
    assert!(!socket.exists(), "renewing starts no server");
    let team = AgentId::team(1).unwrap();
    host.spawn(&team, &shell(root.path(), Vec::new(), &["/bin/sh"]))
        .unwrap();
    for deadline in [until, until + Duration::from_secs(60 * 60)] {
        host.renew(deadline).unwrap();
        let modified = std::fs::symlink_metadata(&socket)
            .unwrap()
            .modified()
            .unwrap();
        assert_eq!(modified, deadline);
    }
    assert_eq!(host.alive().unwrap(), std::slice::from_ref(&team));
    quit(&host, false).unwrap();
}

/// FBTMUX-P1 r1 L2: a running engine whose saved quitting setting it cannot read still moves
/// its server's deadline, to the default 24 hours ahead, so its own live agents never reach a
/// deadline it set earlier; once the setting reads again its hours are used.
#[test]
fn an_unreadable_quitting_setting_renews_with_the_default_limit() {
    let root = root();
    let tmux = tmux(root.path());
    let folder = root.path().join(FOLDER);
    let socket = folder.join(SOCKET);
    let _server = Server {
        tmux: tmux.clone(),
        socket: socket.clone(),
    };
    let host = Tmux::open(Some(tmux.clone()), &folder);
    let team = AgentId::team(1).unwrap();
    host.spawn(&team, &shell(root.path(), Vec::new(), &["/bin/sh"]))
        .unwrap();
    let engine = root.path().join("engine");
    std::fs::create_dir(&engine).unwrap();
    let db = rusqlite::Connection::open(engine.join("engine.db")).unwrap();
    db.execute_batch(
        "CREATE TABLE agent_quit_settings (singleton INTEGER PRIMARY KEY, on_quit TEXT NOT NULL, left_running TEXT NOT NULL, hours INTEGER NOT NULL);
         INSERT INTO agent_quit_settings VALUES(1,'pause','never',1);",
    )
    .unwrap();
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000);
    let modified = || {
        std::fs::symlink_metadata(&socket)
            .unwrap()
            .modified()
            .unwrap()
    };
    renew_once(&host, &engine, now).unwrap();
    assert_eq!(modified(), now + Duration::from_secs(24 * 60 * 60));
    db.execute(
        "UPDATE agent_quit_settings SET on_quit='stop', left_running='after_hours', hours=2",
        [],
    )
    .unwrap();
    renew_once(&host, &engine, now).unwrap();
    assert_eq!(modified(), now + Duration::from_secs(2 * 60 * 60));
    assert_eq!(host.alive().unwrap(), std::slice::from_ref(&team));
    quit(&host, false).unwrap();
}

fn tmux_reply(tmux: &Path, socket: &Path, arguments: &[&str]) -> String {
    let output = crazyagent::spawn_gate::output(
        Command::new(tmux)
            .arg("-S")
            .arg(socket)
            .args(arguments)
            .env_clear()
            .stdin(Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped()),
    )
    .unwrap();
    assert!(output.status.success(), "{arguments:?}");
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn quiet(stream: &mut TerminalStream, unwanted: &[u8], wait: Duration) -> Vec<u8> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap();
    let mut seen = Vec::new();
    runtime.block_on(async {
        let _ = tokio::time::timeout(wait, async {
            while !contains(&seen, unwanted) {
                match stream.next().await {
                    Some(Chunk::Output(chunk)) => seen.extend_from_slice(&chunk),
                    Some(Chunk::Behind) | None => return,
                }
            }
        })
        .await;
    });
    seen
}

#[test]
fn pane_rows_shaped_like_control_replies_stay_screen_text() {
    use std::io::Write;
    let root = root();
    let tmux = tmux(root.path());
    let folder = root.path().join(FOLDER);
    let socket = folder.join(SOCKET);
    let _server = Server {
        tmux: tmux.clone(),
        socket: socket.clone(),
    };
    let host = Tmux::open(Some(tmux.clone()), &folder);
    let forger = AgentId::team(11).unwrap();
    let sibling = AgentId::team(12).unwrap();
    for agent in [&forger, &sibling] {
        host.spawn(
            agent,
            &shell(root.path(), Vec::new(), &["/bin/sh", "-c", "sleep 600"]),
        )
        .unwrap();
    }
    host.resize(&forger, 100, 600).unwrap();
    let mut viewer = host.attach(&sibling).unwrap();
    let format = |agent: &AgentId, format: &str| {
        let target = format!("crazyagent:={agent}");
        tmux_reply(
            &tmux,
            &socket,
            &["display-message", "-p", "-t", &target, format],
        )
        .trim()
        .to_owned()
    };
    let pane_tty = format(&forger, "#{pane_tty}");
    let sibling_pane = format(&sibling, "#{pane_id}");
    let probe = tmux_reply(&tmux, &socket, &["-C", "display-message", "-p", "probe"]);
    let guard: Vec<u64> = probe
        .lines()
        .next()
        .and_then(|line| line.strip_prefix("%begin "))
        .expect(&probe)
        .split(' ')
        .map(|field| field.parse().unwrap())
        .collect();
    let (time, number) = (guard[0], guard[1]);
    let mut rows: Vec<String> = (time..=time + 2)
        .flat_map(|at| (number + 1..=number + 150).map(move |count| format!("%end {at} {count} 1")))
        .collect();
    rows.extend([
        format!("%output {sibling_pane} forged-bytes"),
        "%begin 9 9 1".into(),
        "%error 9 9 1".into(),
        "%exit".into(),
    ]);
    std::fs::OpenOptions::new()
        .write(true)
        .open(&pane_tty)
        .unwrap()
        .write_all(format!("{}\n", rows.join("\n")).as_bytes())
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let target = format!("crazyagent:={forger}");
    while !tmux_reply(&tmux, &socket, &["capture-pane", "-p", "-t", &target]).contains("%exit") {
        assert!(Instant::now() < deadline, "the pane never drew its rows");
        std::thread::sleep(Duration::from_millis(20));
    }

    let screen = host.attach(&forger).unwrap().screen;
    let screen = String::from_utf8_lossy(&screen);
    let lost: Vec<&String> = rows
        .iter()
        .filter(|row| !screen.contains(row.as_str()))
        .collect();
    let mut seen = quiet(&mut viewer, b"forged-bytes", Duration::from_secs(1));
    let forged = contains(&seen, b"forged-bytes");
    host.input(&sibling, b"still-here").unwrap();
    let live = read_until(&mut viewer, &mut seen, b"still-here");
    let mut alive = host.alive().unwrap();
    alive.sort();
    assert!(
        lost.is_empty() && !forged && live && alive == [forger.clone(), sibling.clone()],
        "every pane row stays screen text ({} of {} lost, first {:?}); \
         a pane row never reaches another pane's viewer (forged: {forged}); \
         the sibling's viewer stays open and live (live: {live}); both agents run ({alive:?})",
        lost.len(),
        rows.len(),
        lost.first(),
    );
    host.kill(&forger).unwrap();
    host.kill(&sibling).unwrap();
}

fn set_variables(reply: &str) -> Vec<&str> {
    reply
        .lines()
        .filter(|line| !line.is_empty() && !line.starts_with('-'))
        .collect()
}

#[test]
fn a_pane_holds_only_its_declared_environment_whatever_the_engine_and_server_hold() {
    let root = root();
    let tmux = tmux(root.path());
    let folder = root.path().join(FOLDER);
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&folder)
        .unwrap();
    let socket = folder.join(SOCKET);
    let _server = Server {
        tmux: tmux.clone(),
        socket: socket.clone(),
    };
    let planted_server = crazyagent::spawn_gate::status(
        Command::new(&tmux)
            .args(["-u", "-f", "/dev/null", "-S"])
            .arg(&socket)
            .args([
                "new-session",
                "-d",
                "-s",
                "crazyagent",
                "-n",
                "_crazyagent",
                "cat",
            ])
            .env_clear()
            .envs(PLANTED)
            .envs(PLANTED_TERMINAL)
            .env("PATH", "/usr/bin:/bin")
            .current_dir(&folder)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null()),
    )
    .unwrap();
    assert!(planted_server.success());
    assert!(
        tmux_reply(&tmux, &socket, &["show-environment", "-g"])
            .contains("GH_TOKEN=planted-gh-token"),
        "the server starts out holding the planted variables"
    );

    let engine = crazyagent::spawn_gate::output(
        Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "engine_process_with_planted_variables_spawns_a_pane",
                "--ignored",
                "--nocapture",
                "--test-threads",
                "1",
            ])
            .envs(PLANTED)
            .envs(PLANTED_TERMINAL)
            .env(PANE_ENV_ROOT, root.path())
            .stdin(Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped()),
    )
    .unwrap();
    assert!(
        engine.status.success(),
        "{}{}",
        String::from_utf8_lossy(&engine.stdout),
        String::from_utf8_lossy(&engine.stderr)
    );

    let out = root.path().join("pane.env");
    let started = Instant::now();
    while !out.exists() {
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "the pane never wrote its environment"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    let text = std::fs::read_to_string(&out).unwrap();
    let seen: BTreeMap<&str, &str> = text
        .lines()
        .filter_map(|line| line.split_once('='))
        .filter(|(name, _)| *name != "__CF_USER_TEXT_ENCODING")
        .collect();
    let out_text = out.to_str().unwrap();
    assert_eq!(
        seen,
        BTreeMap::from([
            ("TERM", "xterm-256color"),
            ("COLORTERM", "truecolor"),
            ("LANG", "C"),
            (PANE_ENV_OUT, out_text),
        ])
    );
    for (name, value) in PLANTED {
        assert!(!text.contains(name) && !text.contains(value), "{name}");
    }
    for (_, value) in PLANTED_TERMINAL {
        assert!(!text.contains(value), "{value}");
    }
    for name in ["TERM_PROGRAM", "TMUX", "TMUX_PANE", "SHELL"] {
        assert!(!seen.contains_key(name), "{name} stays out of the pane");
    }
    assert_eq!(
        set_variables(&tmux_reply(&tmux, &socket, &["show-environment", "-g"])),
        Vec::<&str>::new()
    );
    assert_eq!(
        set_variables(&tmux_reply(
            &tmux,
            &socket,
            &["show-environment", "-t", "crazyagent"]
        )),
        Vec::<&str>::new()
    );
    assert_eq!(option(&tmux, &socket, "update-environment"), "");
}

#[test]
#[ignore = "run only by the pane environment test as an engine process holding planted variables"]
fn engine_process_with_planted_variables_spawns_a_pane() {
    let Some(root) = std::env::var_os(PANE_ENV_ROOT).map(PathBuf::from) else {
        return;
    };
    for (name, value) in PLANTED.iter().chain(&PLANTED_TERMINAL) {
        assert_eq!(std::env::var(name).unwrap(), *value);
    }
    let host = Tmux::open(Some(tmux(&root)), &root.join(FOLDER));
    let program = std::env::current_exe().unwrap();
    let out = root.join("pane.env");
    let command = shell(
        &root,
        vec![
            ("LANG".into(), "C".into()),
            (PANE_ENV_OUT.into(), out.to_str().unwrap().into()),
        ],
        &[
            program.to_str().unwrap(),
            "--exact",
            "pane_process_writes_its_environment",
            "--ignored",
            "--test-threads",
            "1",
        ],
    );
    host.spawn(&AgentId::team(9).unwrap(), &command).unwrap();
}

#[test]
fn an_agent_between_runs_shows_its_last_screen_labeled_finished_until_it_runs_again() {
    let root = root();
    let tmux = tmux(root.path());
    let _server = Server {
        tmux,
        socket: root.path().join(FOLDER).join(SOCKET),
    };
    let (ready, host) = platform(root.path());
    assert!(ready);
    let agent = AgentId::team(7).unwrap();
    let run = |text: &str| {
        shell(
            root.path(),
            Vec::new(),
            &[
                "/bin/sh",
                "-c",
                &format!("printf '{text}\\n'; exec sleep 600"),
            ],
        )
    };

    host.spawn(&agent, &run("first run output")).unwrap();
    let mut live = host.attach(&agent).unwrap();
    let mut seen = live.screen.clone();
    assert!(read_until(&mut live, &mut seen, b"first run output"));
    drop(live);
    host.kill(&agent).unwrap();
    assert!(host.alive().unwrap().is_empty());

    let mut last = host.attach(&agent).unwrap();
    assert_eq!((last.cols, last.rows), (80, 24));
    assert!(
        contains(&last.screen, b"first run output") && contains(&last.screen, b"Finished at "),
        "{}",
        String::from_utf8_lossy(&last.screen)
    );
    assert!(
        ended(&mut last),
        "a finished run's screen takes no more output"
    );
    assert_eq!(
        host.input(&agent, b"x").unwrap_err().0,
        "terminal_agent_unknown"
    );
    assert_eq!(host.kill(&agent).unwrap_err().0, "terminal_agent_unknown");

    host.spawn(&agent, &run("second run output")).unwrap();
    let mut live = host.attach(&agent).unwrap();
    assert_eq!((live.cols, live.rows), (80, 24));
    let mut seen = live.screen.clone();
    assert!(read_until(&mut live, &mut seen, b"second run output"));
    assert!(
        !contains(&seen, b"Finished at ") && !contains(&seen, b"first run output"),
        "{}",
        String::from_utf8_lossy(&seen)
    );
    host.kill(&agent).unwrap();
    assert!(ended(&mut live));
}

#[test]
fn a_screen_read_of_an_ended_agent_is_unknown_though_its_viewers_get_the_last_screen() {
    let root = root();
    let tmux = tmux(root.path());
    let _server = Server {
        tmux,
        socket: root.path().join(FOLDER).join(SOCKET),
    };
    let (ready, host) = platform(root.path());
    assert!(ready);
    let agent = AgentId::team(6).unwrap();
    host.spawn(
        &agent,
        &shell(
            root.path(),
            Vec::new(),
            &[
                "/bin/sh",
                "-c",
                "printf 'Allow this edit? (y/n)\\n'; exec sleep 600",
            ],
        ),
    )
    .unwrap();
    let mut live = host.attach(&agent).unwrap();
    let mut seen = live.screen.clone();
    assert!(read_until(&mut live, &mut seen, b"Allow this edit?"));
    drop(live);
    assert!(contains(&host.screen(&agent).unwrap(), b"Allow this edit?"));
    host.kill(&agent).unwrap();

    let last = host.attach(&agent).unwrap();
    assert!(
        contains(&last.screen, b"Allow this edit?") && contains(&last.screen, b"Finished at "),
        "{}",
        String::from_utf8_lossy(&last.screen)
    );
    assert_eq!(
        host.screen(&agent).unwrap_err().0,
        "terminal_agent_unknown",
        "the question scan never reads an ended agent's kept screen"
    );
}

#[test]
fn a_screen_read_and_an_ending_take_no_viewer_slot() {
    let root = root();
    let tmux = tmux(root.path());
    let _server = Server {
        tmux,
        socket: root.path().join(FOLDER).join(SOCKET),
    };
    let (ready, host) = platform(root.path());
    assert!(ready);
    let agent = AgentId::team(5).unwrap();
    host.spawn(
        &agent,
        &shell(
            root.path(),
            Vec::new(),
            &[
                "/bin/sh",
                "-c",
                "printf 'busy agent output\\n'; exec sleep 600",
            ],
        ),
    )
    .unwrap();
    let mut viewers: Vec<TerminalStream> = (0..VIEWERS_MAX)
        .map(|_| host.attach(&agent).unwrap())
        .collect();
    let mut seen = viewers[0].screen.clone();
    assert!(read_until(&mut viewers[0], &mut seen, b"busy agent output"));
    assert_eq!(host.attach(&agent).unwrap_err().0, "terminal_viewers_full");

    let screen = host
        .screen(&agent)
        .expect("a screen read needs no viewer slot");
    assert!(
        contains(&screen, b"busy agent output"),
        "{}",
        String::from_utf8_lossy(&screen)
    );
    host.kill(&agent).unwrap();
    for viewer in &mut viewers {
        assert!(ended(viewer));
    }
    drop(viewers);

    let last = host
        .attach(&agent)
        .expect("an ending with every viewer slot taken still keeps the last screen");
    assert!(
        contains(&last.screen, b"busy agent output") && contains(&last.screen, b"Finished at "),
        "{}",
        String::from_utf8_lossy(&last.screen)
    );
}

#[test]
fn a_history_past_the_reply_bound_opens_without_it_and_every_other_terminal_keeps_running() {
    let root = root();
    let tmux = tmux(root.path());
    let _server = Server {
        tmux,
        socket: root.path().join(FOLDER).join(SOCKET),
    };
    let (ready, host) = platform(root.path());
    assert!(ready);
    let quiet = AgentId::team(3).unwrap();
    host.spawn(
        &quiet,
        &shell(root.path(), Vec::new(), &["/bin/sh", "-c", "exec /bin/cat"]),
    )
    .unwrap();
    let mut watching = host.attach(&quiet).unwrap();

    let colourful = root.path().join("colourful.pl");
    std::fs::write(
        &colourful,
        "for my $row (1..1900) { print join('', map { sprintf(\"\\e[38;5;%d;48;5;%dm%s\", $_ % 256, ($row + $_) % 256, chr(97 + $_ % 26)) } 1..1000), \"\\r\\n\" }\nopen(my $done, '>', 'printed') or die; close($done);\n",
    )
    .unwrap();
    let loud = AgentId::team(4).unwrap();
    host.spawn(
        &loud,
        &shell(
            root.path(),
            Vec::new(),
            &[
                "/bin/sh",
                "-c",
                "while [ ! -f go ]; do /bin/sleep 0.05; done; /usr/bin/perl colourful.pl; exec /bin/sleep 600",
            ],
        ),
    )
    .unwrap();
    host.resize(&loud, 1000, 50).unwrap();
    std::fs::write(root.path().join("go"), b"").unwrap();
    let printing = Instant::now();
    while !root.path().join("printed").exists() {
        assert!(
            printing.elapsed() < Duration::from_secs(90),
            "the colourful output never finished"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    std::thread::sleep(Duration::from_millis(500));

    let opened = host
        .attach(&loud)
        .expect("a history past the reply bound still opens the screen");
    assert!(opened.history.is_empty());
    assert_eq!((opened.cols, opened.rows), (1000, 50));
    assert!(contains(&opened.screen, b"\x1b["));
    drop(opened);
    assert_eq!(host.alive().unwrap(), vec![quiet.clone(), loud.clone()]);
    host.input(&quiet, b"still-here\r").unwrap();
    let mut seen = watching.screen.clone();
    assert!(
        read_until(&mut watching, &mut seen, b"still-here"),
        "another agent's viewer kept its terminal: {}",
        String::from_utf8_lossy(&seen)
    );
    host.kill(&loud).unwrap();
    host.kill(&quiet).unwrap();
    assert!(ended(&mut watching));
}

#[test]
#[ignore = "run only inside a tmux pane by the pane environment test"]
fn pane_process_writes_its_environment() {
    let Some(out) = std::env::var_os(PANE_ENV_OUT).map(PathBuf::from) else {
        return;
    };
    let text: String = std::env::vars_os()
        .map(|(name, value)| format!("{}={}\n", name.to_string_lossy(), value.to_string_lossy()))
        .collect();
    let partial = out.with_extension("partial");
    std::fs::write(&partial, text).unwrap();
    std::fs::rename(&partial, &out).unwrap();
}

/// One engine root with team agent 1 ready to lead in its own server: its home, its short
/// private temp folder that holds its own and its teammates' sockets and allow-default
/// installed profiles for it and its teammates.
#[cfg(target_os = "macos")]
struct Lead {
    root: tempfile::TempDir,
    tmux: PathBuf,
    home: PathBuf,
    profile: PathBuf,
    socket: PathBuf,
    mates: PathBuf,
}

#[cfg(target_os = "macos")]
impl Lead {
    fn new() -> Self {
        let root = root();
        let tmux = tmux(root.path());
        let path = root.path().canonicalize().unwrap();
        let team = path.join("team");
        let home = crazyagent::team::runtime::agent_home(&team, 1);
        std::fs::create_dir_all(&home).unwrap();
        crazyagent::sandbox::toolchains::make_tmp(&home).unwrap();
        let profile = crazyagent::team::runtime::agent_profile(&team, 1);
        std::fs::create_dir_all(profile.parent().unwrap()).unwrap();
        std::fs::write(&profile, "(version 1)\n(allow default)\n").unwrap();
        std::fs::write(
            crazyagent::team::runtime::agent_teammates_profile(&team, 1),
            "(version 1)\n(allow default)\n",
        )
        .unwrap();
        let socket =
            crazyagent::terminal::harness_socket(&path, &AgentId::team(1).unwrap()).unwrap();
        let mates = crazyagent::terminal::renew_teammates_socket(&path, &AgentId::team(1).unwrap())
            .unwrap();
        Self {
            root,
            tmux,
            home,
            profile,
            socket,
            mates,
        }
    }

    fn path(&self) -> PathBuf {
        self.root.path().canonicalize().unwrap()
    }

    fn shell(&self, prompt: &str, argv: &[&str]) -> InteractiveCommand {
        shell(
            &self.path(),
            vec![
                ("PS1".into(), prompt.into()),
                ("PATH".into(), "/usr/bin:/bin".into()),
            ],
            argv,
        )
    }

    /// The lead's own command: the shell started under its installed profile, as the team
    /// runtime starts a native lead. Takes the prompt and the argv. Returns the command.
    fn confined(&self, prompt: &str, argv: &[&str]) -> InteractiveCommand {
        let profile = self.profile.to_str().unwrap();
        let mut confined = vec![crazyagent::sandbox::SANDBOX_EXEC, "-f", profile];
        confined.extend_from_slice(argv);
        self.shell(prompt, &confined)
    }

    /// Types a tmux command into the lead's pane that prints the new pane's id into a file,
    /// and waits for it. Takes the host, the lead, the tmux arguments before `-P` and the
    /// command the new pane runs. Returns the new pane id.
    fn new_pane(&self, host: &dyn Host, lead: &AgentId, how: &str, run: &str) -> String {
        let out = self
            .path()
            .join(format!("pane-{}.txt", how.replace(' ', "-")));
        let _ = std::fs::remove_file(&out);
        let line = format!(
            "{} {how} -P -F '#{{pane_id}}' '{run}' > {}\n",
            self.tmux.display(),
            out.display()
        );
        host.input(lead, line.as_bytes()).unwrap();
        let id = wait_text(&out).trim().to_owned();
        assert!(crazyagent::terminal::pane_id_valid(&id), "{id:?}");
        id
    }
}

#[cfg(target_os = "macos")]
impl Drop for Lead {
    fn drop(&mut self) {
        for socket in [
            self.socket.clone(),
            self.mates.clone(),
            self.path().join(FOLDER).join(SOCKET),
        ] {
            let _ = crazyagent::spawn_gate::status(
                Command::new(&self.tmux)
                    .arg("-S")
                    .arg(&socket)
                    .arg("kill-server")
                    .env_clear()
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null()),
            );
        }
        let _ = crazyagent::sandbox::toolchains::remove_tmp(&self.home);
    }
}

/// Waits until nothing accepts connections at `socket`, as after kill-server, whose server
/// ends a moment after the command returns. Takes the socket.
#[cfg(target_os = "macos")]
fn until_refused(socket: &Path) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while crazyagent::spawn_gate::connect_unix_std(socket).is_ok() {
        assert!(
            Instant::now() < deadline,
            "{} still accepts connections",
            socket.display()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn wait_text(path: &Path) -> String {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok(text) = std::fs::read_to_string(path)
            && text.ends_with('\n')
        {
            return text;
        }
        assert!(
            Instant::now() < deadline,
            "{} never written",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[cfg(target_os = "macos")]
#[test]
fn a_native_lead_runs_in_its_own_server_and_every_call_reaches_it_by_name() {
    let lead = Lead::new();
    let root = lead.path();
    let host = crazyagent::terminal::tmux_host(&root, Some(lead.tmux.clone()));
    let leader = AgentId::team(1).unwrap();
    let engine_agent = AgentId::team(2).unwrap();
    let command = lead.confined("lead$ ", &["/bin/sh"]);
    host.spawn_own_server(&leader, &command).unwrap();
    assert!(
        std::fs::symlink_metadata(&lead.socket)
            .unwrap()
            .file_type()
            .is_socket()
    );
    host.spawn(&engine_agent, &lead.shell("engine$ ", &["/bin/sh"]))
        .unwrap();
    let engine_only = Tmux::open(Some(lead.tmux.clone()), &root.join(FOLDER));
    assert_eq!(
        engine_only.alive().unwrap(),
        std::slice::from_ref(&engine_agent)
    );
    assert_eq!(
        host.alive().unwrap(),
        [leader.clone(), engine_agent.clone()]
    );
    assert_eq!(
        host.spawn(&leader, &command).unwrap_err().0,
        "terminal_agent_running"
    );
    assert_eq!(
        host.spawn_own_server(&engine_agent, &command)
            .unwrap_err()
            .0,
        "terminal_agent_running"
    );
    let own_pane = host.panes(&leader).unwrap();
    assert_eq!(own_pane.len(), 1);

    let split_out = root.join("split.txt");
    let split = lead.new_pane(
        host.as_ref(),
        &leader,
        "split-window",
        &format!("cat > {}", split_out.display()),
    );
    let routed = root.join("routed.txt");
    host.input(
        &leader,
        format!("echo routed-$((40 + 2)) > {}\n", routed.display()).as_bytes(),
    )
    .unwrap();
    assert_eq!(wait_text(&routed), "routed-42\n");
    assert!(
        !std::fs::read_to_string(&split_out)
            .unwrap_or_default()
            .contains("routed")
    );
    let panes = host.panes(&leader).unwrap();
    assert_eq!(panes.len(), 2, "{panes:?}");
    assert!(panes.iter().any(|pane| pane.id == split));
    let snapshot = host.snapshot(&leader).unwrap();
    assert!(
        contains(&snapshot.screen, b"lead$"),
        "the snapshot shows the lead's own pane, not the active split: {:?}",
        String::from_utf8_lossy(&snapshot.screen)
    );
    drop(host.attach_pane(&leader, &split).unwrap());
    let window = lead.new_pane(host.as_ref(), &leader, "new-window -d", "sleep 600");
    assert_eq!(
        host.attach_pane(&leader, &window).unwrap_err().0,
        "terminal_pane_unknown"
    );
    assert_eq!(
        host.attach_pane(&leader, &own_pane[0].id).unwrap_err().0,
        "terminal_pane_unknown",
        "the anchor standing for the lead on its teammates' server is no pane to attach"
    );
    drop(host.attach(&leader).unwrap());

    host.close(&[]).unwrap();
    assert!(host.alive().unwrap().is_empty());
    assert!(
        !std::fs::symlink_metadata(&lead.socket)
            .is_ok_and(|metadata| metadata.file_type().is_socket())
    );
    assert!(
        std::fs::symlink_metadata(&lead.mates)
            .is_ok_and(|metadata| metadata.file_type().is_socket()),
        "an ended teammates' server keeps its socket, so no teammate can bind one there"
    );
    assert_eq!(
        crazyagent::spawn_gate::connect_unix_std(&lead.mates)
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::ConnectionRefused
    );
    assert_ne!(
        crazyagent::terminal::renew_teammates_socket(&root, &leader).unwrap(),
        lead.mates,
        "the lead's next start names a new teammates' socket"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn an_adopted_teammate_pane_is_reached_and_ended_by_its_own_name_even_after_a_restart() {
    let lead = Lead::new();
    let root = lead.path();
    let host = crazyagent::terminal::tmux_host(&root, Some(lead.tmux.clone()));
    let leader = AgentId::team(1).unwrap();
    let [ann, bob, missing] = [2, 3, 9].map(|id| AgentId::team(id).unwrap());
    host.spawn_own_server(&leader, &lead.confined("lead$ ", &["/bin/sh"]))
        .unwrap();
    let own_pane = host.panes(&leader).unwrap()[0].id.clone();
    let ann_out = root.join("ann.txt");
    let ann_pane = lead.new_pane(
        host.as_ref(),
        &leader,
        "split-window -d",
        &format!("cat > {}", ann_out.display()),
    );
    let bob_pane = lead.new_pane(host.as_ref(), &leader, "split-window -d", "sleep 600");
    let window = lead.new_pane(host.as_ref(), &leader, "new-window -d", "sleep 600");

    for (pane, code) in [
        (&own_pane, "terminal_pane_unknown"),
        (&window, "terminal_pane_unknown"),
    ] {
        assert_eq!(host.adopt_pane(&ann, &leader, pane).unwrap_err().0, code);
    }
    assert_eq!(
        host.adopt_pane(&ann, &missing, &ann_pane).unwrap_err().0,
        "terminal_own_server_unavailable"
    );
    assert_eq!(
        host.adopt_pane(&leader, &leader, &ann_pane).unwrap_err().0,
        "terminal_agent_running"
    );
    host.adopt_pane(&ann, &leader, &ann_pane).unwrap();
    host.adopt_pane(&ann, &leader, &ann_pane).unwrap();
    host.adopt_pane(&bob, &leader, &bob_pane).unwrap();

    host.input(&ann, b"to-ann\n").unwrap();
    assert_eq!(wait_text(&ann_out), "to-ann\n");
    host.kill(&ann).unwrap();
    let left: Vec<String> = host
        .panes(&leader)
        .unwrap()
        .into_iter()
        .map(|pane| pane.id)
        .collect();
    assert_eq!(left, [own_pane.clone(), bob_pane.clone()]);

    let restarted = crazyagent::terminal::tmux_host(&root, Some(lead.tmux.clone()));
    restarted.kill(&bob).unwrap();
    let left: Vec<String> = restarted
        .panes(&leader)
        .unwrap()
        .into_iter()
        .map(|pane| pane.id)
        .collect();
    assert_eq!(left, [own_pane]);
    assert_eq!(restarted.alive().unwrap(), std::slice::from_ref(&leader));
    assert_eq!(
        restarted.kill(&missing).unwrap_err().0,
        "terminal_agent_unknown"
    );
    restarted.close(&[]).unwrap();
}

/// B19L-H1: through the engine's production host a native lead and each teammate adopted
/// into its window are named by their own pane's first process, though the teammates share
/// one window with the lead's anchor, and ending a left teammate pane ends only that pane.
#[cfg(target_os = "macos")]
#[test]
fn a_native_lead_and_each_adopted_teammate_name_their_own_first_process() {
    let lead = Lead::new();
    let root = lead.path();
    let host = crazyagent::terminal::tmux_host(&root, Some(lead.tmux.clone()));
    let leader = AgentId::team(1).unwrap();
    let [ann, bob] = [2, 3].map(|id| AgentId::team(id).unwrap());
    host.spawn_own_server(&leader, &lead.confined("lead$ ", &["/bin/sh"]))
        .unwrap();
    let ann_pane = lead.new_pane(host.as_ref(), &leader, "split-window -d", "sleep 600");
    let bob_pane = lead.new_pane(host.as_ref(), &leader, "split-window -d", "sleep 601");
    host.adopt_pane(&ann, &leader, &ann_pane).unwrap();
    host.adopt_pane(&bob, &leader, &bob_pane).unwrap();
    let panes = host.panes(&leader).unwrap();
    let own_pane = panes[0].id.clone();
    let pids: BTreeMap<String, u32> = panes.into_iter().map(|pane| (pane.id, pane.pid)).collect();
    assert_eq!(pids.len(), 3, "{pids:?}");

    for (agent, pane) in [(&leader, &own_pane), (&ann, &ann_pane), (&bob, &bob_pane)] {
        assert_eq!(
            host.pane_pid(agent).map_err(|problem| problem.0),
            Ok(Some(pids[pane])),
            "{agent}"
        );
    }

    assert_eq!(
        end_left_pane(host.as_ref(), &bob, "test").map_err(|problem| problem.0),
        Ok(true)
    );
    assert!(ends(&[pids[&bob_pane]]));
    let left: Vec<String> = host
        .panes(&leader)
        .unwrap()
        .into_iter()
        .map(|pane| pane.id)
        .collect();
    assert_eq!(left, [own_pane, ann_pane.clone()]);
    assert!(process_running(pids[&ann_pane]));
    host.close(&[]).unwrap();
}
/// PLC (d): a native lead's team panes live only on the teammates' server its start named.
/// Its own server numbers its panes on its own, so a teammate pane's id can name a pane the
/// lead split there; once the teammates' server ends no team pane call goes to the lead's
/// own server, so that pane is neither listed, typed into nor adopted as a teammate under
/// the teammate pane's id.
#[cfg(target_os = "macos")]
#[test]
fn a_native_lead_s_team_pane_calls_never_reach_its_own_server_once_its_teammates_server_ended() {
    let lead = Lead::new();
    let root = lead.path();
    let host = crazyagent::terminal::tmux_host(&root, Some(lead.tmux.clone()));
    let leader = AgentId::team(1).unwrap();
    let [ann, bob] = [2, 3].map(|id| AgentId::team(id).unwrap());
    host.spawn_own_server(&leader, &lead.confined("lead$ ", &["/bin/sh"]))
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !contains(&host.snapshot(&leader).unwrap().screen, b"lead$") {
        assert!(Instant::now() < deadline, "the lead's shell never prompted");
        std::thread::sleep(Duration::from_millis(50));
    }
    let own_out = root.join("own.txt");
    let own_id = root.join("own-id.txt");
    let line = format!(
        "{} -S {} split-window -d -P -F '#{{pane_id}}' 'cat > {}' > {}\n",
        lead.tmux.display(),
        lead.socket.display(),
        own_out.display(),
        own_id.display()
    );
    host.input(&leader, line.as_bytes()).unwrap();
    let own_split = wait_text(&own_id).trim().to_owned();
    let mut ann_pane = lead.new_pane(host.as_ref(), &leader, "split-window -d", "sleep 600");
    for _ in 0..4 {
        if ann_pane == own_split {
            break;
        }
        ann_pane = lead.new_pane(host.as_ref(), &leader, "split-window -d", "sleep 600");
    }
    assert_eq!(
        ann_pane, own_split,
        "a teammate pane on the teammates' server has the id of the lead's split on its own"
    );
    host.adopt_pane(&ann, &leader, &ann_pane).unwrap();

    let killed = crazyagent::spawn_gate::status(
        Command::new(&lead.tmux)
            .arg("-S")
            .arg(&lead.mates)
            .arg("kill-server")
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null()),
    )
    .unwrap();
    assert!(killed.success());
    until_refused(&lead.mates);

    let ended = "terminal_teammates_server_ended";
    assert_eq!(host.panes(&leader).unwrap_err().0, ended);
    assert_eq!(
        host.input_pane(&leader, &ann_pane, b"to-own\n")
            .unwrap_err()
            .0,
        ended
    );
    assert_eq!(
        host.adopt_pane(&bob, &leader, &own_split).unwrap_err().0,
        ended
    );
    let routed = root.join("routed.txt");
    host.input(
        &leader,
        format!("echo lead-$((40 + 2)) > {}\n", routed.display()).as_bytes(),
    )
    .unwrap();
    assert_eq!(wait_text(&routed), "lead-42\n");
    assert!(
        !std::fs::read_to_string(&own_out)
            .unwrap_or_default()
            .contains("to-own")
    );
    host.close(&[]).unwrap();
}

/// CA-SEC-002: an adopted teammate's calls go only to its lead's teammates' server. Once that
/// server ended while the lead's own server still runs, a pane the lead split there and
/// tagged with the teammate's name receives none of them, and the teammate reads as gone
/// whether or not the engine's own server runs. Ending the teammate, before or after an
/// engine restart, ends no such pane.
#[cfg(target_os = "macos")]
#[test]
fn an_adopted_teammate_s_calls_never_reach_its_lead_s_own_server_once_its_teammates_server_ended() {
    let lead = Lead::new();
    let root = lead.path();
    let host = crazyagent::terminal::tmux_host(&root, Some(lead.tmux.clone()));
    let leader = AgentId::team(1).unwrap();
    let ann = AgentId::team(2).unwrap();
    host.spawn_own_server(&leader, &lead.confined("lead$ ", &["/bin/sh"]))
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !contains(&host.snapshot(&leader).unwrap().screen, b"lead$") {
        assert!(Instant::now() < deadline, "the lead's shell never prompted");
        std::thread::sleep(Duration::from_millis(50));
    }
    let ann_pane = lead.new_pane(host.as_ref(), &leader, "split-window -d", "sleep 600");
    host.adopt_pane(&ann, &leader, &ann_pane).unwrap();

    let own_out = root.join("own.txt");
    let own_id = root.join("own-id.txt");
    let line = format!(
        "{} -S {} split-window -d -P -F '#{{pane_id}}' 'cat > {}' > {}\n",
        lead.tmux.display(),
        lead.socket.display(),
        own_out.display(),
        own_id.display()
    );
    host.input(&leader, line.as_bytes()).unwrap();
    let own_split = wait_text(&own_id).trim().to_owned();
    tmux_reply(
        &lead.tmux,
        &lead.socket,
        &[
            "set-option",
            "-p",
            "-t",
            &own_split,
            "@crazyagent",
            ann.as_str(),
        ],
    );

    let killed = crazyagent::spawn_gate::status(
        Command::new(&lead.tmux)
            .arg("-S")
            .arg(&lead.mates)
            .arg("kill-server")
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null()),
    )
    .unwrap();
    assert!(killed.success());
    until_refused(&lead.mates);

    let gone = Some("terminal_agent_unknown".to_owned());
    let reads_as_gone = |host: &dyn Host| {
        assert_eq!(
            host.input(&ann, b"to-own\n").err().map(|problem| problem.0),
            gone
        );
        assert_eq!(host.input_live(&ann).err().map(|problem| problem.0), gone);
        assert_eq!(host.quiet(&ann).err().map(|problem| problem.0), gone);
        assert_eq!(host.snapshot(&ann).err().map(|problem| problem.0), gone);
        assert_eq!(host.pane_pid(&ann).err().map(|problem| problem.0), gone);
        assert_eq!(
            host.resize(&ann, 100, 40).err().map(|problem| problem.0),
            gone
        );
    };
    reads_as_gone(host.as_ref());
    let bob = AgentId::team(3).unwrap();
    host.spawn(&bob, &lead.shell("bob$ ", &["/bin/sh"]))
        .unwrap();
    reads_as_gone(host.as_ref());
    assert_eq!(host.kill(&ann).err().map(|problem| problem.0), gone);
    let restarted = crazyagent::terminal::tmux_host(&root, Some(lead.tmux.clone()));
    assert_eq!(restarted.kill(&ann).err().map(|problem| problem.0), gone);
    let panes = tmux_reply(
        &lead.tmux,
        &lead.socket,
        &["list-panes", "-a", "-F", "#{pane_id}"],
    );
    assert!(panes.lines().any(|pane| pane == own_split), "{panes}");
    let routed = root.join("routed.txt");
    host.input(
        &leader,
        format!("echo lead-$((40 + 2)) > {}\n", routed.display()).as_bytes(),
    )
    .unwrap();
    assert_eq!(wait_text(&routed), "lead-42\n");
    assert!(
        !std::fs::read_to_string(&own_out)
            .unwrap_or_default()
            .contains("to-own")
    );
    host.close(&[]).unwrap();
}

/// CA-SEC-002 (L2): a teammates' server that could not be reached once is not an ended one.
/// While its socket is away the adopted teammate's call is refused and goes to no other
/// server; once it is back the same teammate is reached again, so one failed connect does
/// not forget a live teammate until the engine restarts.
#[cfg(target_os = "macos")]
#[test]
fn an_adopted_teammate_survives_one_failed_connect_to_its_teammates_server() {
    let lead = Lead::new();
    let root = lead.path();
    let host = crazyagent::terminal::tmux_host(&root, Some(lead.tmux.clone()));
    let leader = AgentId::team(1).unwrap();
    let ann = AgentId::team(2).unwrap();
    host.spawn_own_server(&leader, &lead.confined("lead$ ", &["/bin/sh"]))
        .unwrap();
    let ann_out = root.join("ann.txt");
    let ann_pane = lead.new_pane(
        host.as_ref(),
        &leader,
        "split-window -d",
        &format!("cat > {}", ann_out.display()),
    );
    host.adopt_pane(&ann, &leader, &ann_pane).unwrap();

    let aside = root.join("mates-aside.sock");
    std::fs::rename(&lead.mates, &aside).unwrap();
    assert_eq!(
        host.input(&ann, b"lost\n").err().map(|problem| problem.0),
        Some("terminal_agent_unknown".to_owned())
    );
    std::fs::rename(&aside, &lead.mates).unwrap();
    host.input(&ann, b"to-ann\n").unwrap();
    assert_eq!(wait_text(&ann_out), "to-ann\n");
    host.close(&[]).unwrap();
}

/// Starts a stand-in tmux server at one of a native lead's sockets, as another process could
/// once the engine's server there ended, shaped like the engine's: its session and holder
/// window, whose one pane is tagged as the lead's. Takes the tmux program, the socket and
/// the lead.
#[cfg(target_os = "macos")]
fn stand_in(tmux: &Path, socket: &Path, lead: &AgentId) {
    match std::fs::remove_file(socket) {
        Ok(()) => {}
        Err(error) => assert_eq!(error.kind(), std::io::ErrorKind::NotFound),
    }
    let started = crazyagent::spawn_gate::status(
        Command::new(tmux)
            .args(["-u", "-f", "/dev/null", "-S"])
            .arg(socket)
            .args([
                "new-session",
                "-d",
                "-s",
                "crazyagent",
                "-n",
                "_crazyagent",
                "cat",
            ])
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null()),
    )
    .unwrap();
    assert!(started.success());
    tmux_reply(
        tmux,
        socket,
        &[
            "set-option",
            "-p",
            "-t",
            "crazyagent:=_crazyagent",
            "@crazyagent",
            lead.as_str(),
        ],
    );
}

/// PLC (c): the engine pins a native lead's teammates' server to the pid it first answered
/// with. Once that server and the lead's own ended, stand-in servers another process started
/// at both sockets are refused for the lead's team panes: by the same host, though it dropped
/// its teammates' client while nothing listened there, and by a host opened again on the
/// same engine root, as after an engine restart, since the pin is kept on disk (PC-L1).
#[cfg(target_os = "macos")]
#[test]
fn a_server_started_at_a_native_lead_s_teammates_socket_after_its_own_ended_is_refused() {
    let lead = Lead::new();
    let root = lead.path();
    let host = crazyagent::terminal::tmux_host(&root, Some(lead.tmux.clone()));
    let leader = AgentId::team(1).unwrap();
    let command = lead.confined("lead$ ", &["/bin/sh"]);
    host.spawn_own_server(&leader, &command).unwrap();
    assert_eq!(host.panes(&leader).unwrap().len(), 1);
    for socket in [&lead.socket, &lead.mates] {
        let killed = crazyagent::spawn_gate::status(
            Command::new(&lead.tmux)
                .arg("-S")
                .arg(socket)
                .arg("kill-server")
                .env_clear()
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null()),
        )
        .unwrap();
        assert!(killed.success());
        until_refused(socket);
    }
    assert_eq!(
        host.panes(&leader).unwrap_err().0,
        "terminal_teammates_server_ended",
        "no server lists the lead's panes while none runs, and the host drops its teammates' client"
    );
    for socket in [&lead.socket, &lead.mates] {
        stand_in(&lead.tmux, socket, &leader);
    }

    let replaced = "terminal_teammates_server_replaced";
    assert_eq!(host.panes(&leader).unwrap_err().0, replaced);
    assert_eq!(
        host.input_pane(&leader, "%0", b"x\n").unwrap_err().0,
        replaced
    );
    let restarted = crazyagent::terminal::tmux_host(&root, Some(lead.tmux.clone()));
    assert_eq!(restarted.panes(&leader).unwrap_err().0, replaced);
    assert_eq!(
        restarted.input_pane(&leader, "%0", b"x\n").unwrap_err().0,
        replaced
    );
}

/// PLC (c) PC-L1: a native lead's teammates' server pin outlives the engine's kill of that
/// lead, which forgets its clients: stand-in servers another process started at both sockets
/// after the kill are refused for the lead's team panes.
#[cfg(target_os = "macos")]
#[test]
fn a_server_started_at_a_native_lead_s_teammates_socket_after_the_lead_was_killed_is_refused() {
    let lead = Lead::new();
    let root = lead.path();
    let host = crazyagent::terminal::tmux_host(&root, Some(lead.tmux.clone()));
    let leader = AgentId::team(1).unwrap();
    host.spawn_own_server(&leader, &lead.confined("lead$ ", &["/bin/sh"]))
        .unwrap();
    assert_eq!(host.panes(&leader).unwrap().len(), 1);
    host.kill(&leader).unwrap();
    for socket in [&lead.socket, &lead.mates] {
        until_refused(socket);
        stand_in(&lead.tmux, socket, &leader);
    }
    assert_eq!(
        host.panes(&leader).unwrap_err().0,
        "terminal_teammates_server_replaced"
    );
}

/// PLCC-L1: an engine started again on a root whose teammates' server pin record is gone
/// refuses that server for the lead's team panes, where it used to trust whichever answered.
#[cfg(target_os = "macos")]
#[test]
fn a_restarted_engine_refuses_a_teammates_server_whose_pin_record_is_gone() {
    let lead = Lead::new();
    let root = lead.path();
    let host = crazyagent::terminal::tmux_host(&root, Some(lead.tmux.clone()));
    let leader = AgentId::team(1).unwrap();
    host.spawn_own_server(&leader, &lead.confined("lead$ ", &["/bin/sh"]))
        .unwrap();
    assert_eq!(host.panes(&leader).unwrap().len(), 1);
    std::fs::remove_file(crazyagent::team::runtime::agent_teammates_server_pid(
        &root.join("team"),
        1,
    ))
    .unwrap();
    let restarted = crazyagent::terminal::tmux_host(&root, Some(lead.tmux.clone()));
    assert_eq!(
        restarted
            .panes(&leader)
            .map(|panes| panes.len())
            .map_err(|problem| problem.0),
        Err("terminal_own_server_unavailable".to_string())
    );
}

/// PLCC-L1: a lead whose pin is refused does not stop a close. The next lead and the
/// engine's own server still end, and the close reports the refusal.
#[cfg(target_os = "macos")]
#[test]
fn a_refused_lead_does_not_stop_the_close_of_the_next_lead_and_the_engine() {
    let lead = Lead::new();
    let root = lead.path();
    let team = root.join("team");
    let (refused, next) = (AgentId::team(1).unwrap(), AgentId::team(2).unwrap());
    let next_home = crazyagent::team::runtime::agent_home(&team, 2);
    std::fs::create_dir_all(&next_home).unwrap();
    crazyagent::sandbox::toolchains::make_tmp(&next_home).unwrap();
    for profile in [
        crazyagent::team::runtime::agent_profile(&team, 2),
        crazyagent::team::runtime::agent_teammates_profile(&team, 2),
    ] {
        std::fs::create_dir_all(profile.parent().unwrap()).unwrap();
        std::fs::write(&profile, "(version 1)\n(allow default)\n").unwrap();
    }
    let _next_own = Server {
        tmux: lead.tmux.clone(),
        socket: crazyagent::terminal::harness_socket(&root, &next).unwrap(),
    };
    let _next_mates = Server {
        tmux: lead.tmux.clone(),
        socket: crazyagent::terminal::renew_teammates_socket(&root, &next).unwrap(),
    };
    let host = crazyagent::terminal::tmux_host(&root, Some(lead.tmux.clone()));
    host.spawn_own_server(&refused, &lead.confined("one$ ", &["/bin/sh"]))
        .unwrap();
    let next_profile = crazyagent::team::runtime::agent_profile(&team, 2);
    let next_command = lead.shell(
        "two$ ",
        &[
            crazyagent::sandbox::SANDBOX_EXEC,
            "-f",
            next_profile.to_str().unwrap(),
            "/bin/sh",
        ],
    );
    host.spawn_own_server(&next, &next_command).unwrap();
    host.spawn(
        &AgentId::team(3).unwrap(),
        &lead.shell("three$ ", &["/bin/sh"]),
    )
    .unwrap();
    std::fs::remove_file(crazyagent::team::runtime::agent_teammates_server_pid(
        &team, 1,
    ))
    .unwrap();

    let restarted = crazyagent::terminal::tmux_host(&root, Some(lead.tmux.clone()));
    assert_eq!(
        restarted.close(&[]).map_err(|problem| problem.0),
        Err("terminal_own_server_unavailable".to_string())
    );
    let after = crazyagent::terminal::tmux_host(&root, Some(lead.tmux.clone()));
    assert_eq!(after.alive().unwrap(), Vec::<AgentId>::new());
    crazyagent::sandbox::toolchains::remove_tmp(&next_home).unwrap();
}

#[test]
fn quiet_through_the_engine_host_answers_for_an_agent_on_the_engine_server() {
    let root = root();
    let tmux = tmux(root.path());
    let path = root.path().canonicalize().unwrap();
    let _server = Server {
        tmux: tmux.clone(),
        socket: path.join(FOLDER).join(SOCKET),
    };
    let host = crazyagent::terminal::tmux_host(&path, Some(tmux));
    let agent = AgentId::team(4).unwrap();
    host.spawn(&agent, &shell(&path, Vec::new(), &["/bin/sh"]))
        .unwrap();
    let quiet = host.quiet(&agent).unwrap();
    assert!(
        quiet.is_some_and(|quiet| quiet < Duration::from_secs(60)),
        "{quiet:?}"
    );
    host.close(&[]).unwrap();
}
