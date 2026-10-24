#![cfg(unix)]

use crazyagent::harnesses::interactive::{InteractiveCommand, Transcript};
use crazyagent::terminal::buffer::skipped_marker;
use crazyagent::terminal::tmux::{SOCKET, SOCKET_PATH_BYTES_MAX, Tmux, installed};
use crazyagent::terminal::{
    AgentId, FOLDER, Host, INPUT_BYTES_MAX, SCROLLBACK_LINES, TerminalStream,
    VIEWER_BUFFER_BYTES_MAX, plain_reason,
};
use std::collections::BTreeMap;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

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

#[test]
fn a_socket_path_too_long_for_the_system_is_refused() {
    let root = root();
    let tmux = tmux(root.path());
    let deep = root.path().join("d".repeat(SOCKET_PATH_BYTES_MAX));
    let host = Tmux::open(Some(tmux), &deep);
    let agent = AgentId::team(1).unwrap();
    let command = shell(root.path(), Vec::new(), &["/bin/sh"]);
    assert_eq!(
        host.spawn(&agent, &command).unwrap_err().0,
        "terminal_socket_path_long"
    );
    assert!(
        host.alive().unwrap().is_empty(),
        "no server runs, so no agent does"
    );
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
    let output = Command::new(tmux)
        .arg("-S")
        .arg(socket)
        .args(["show-options", "-gv", name])
        .env_clear()
        .stdin(Stdio::null())
        .output()
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
fn a_viewer_that_falls_behind_is_told_how_much_output_it_missed() {
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
        format!("head -c {flood} /dev/zero | tr '\\0' a; echo; echo flood-done\r").as_bytes(),
    )
    .unwrap();
    std::thread::sleep(Duration::from_secs(5));
    let mut seen = Vec::new();
    assert!(read_until(&mut stream, &mut seen, b"\r\nflood-done\r\n"));
    assert!(
        contains(&seen, b" bytes of output skipped]"),
        "the viewer reads a skipped marker instead of being dropped"
    );
    assert!(seen.len() < flood, "the oldest output was dropped");
    assert!(skipped_marker(1).starts_with(b"\r\n"));
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
    std::thread::sleep(Duration::from_millis(500));
    let listed = Command::new(&tmux)
        .arg("-S")
        .arg(&socket)
        .arg("list-sessions")
        .env_clear()
        .stdin(Stdio::null())
        .output()
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

fn tmux_reply(tmux: &Path, socket: &Path, arguments: &[&str]) -> String {
    let output = Command::new(tmux)
        .arg("-S")
        .arg(socket)
        .args(arguments)
        .env_clear()
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(output.status.success(), "{arguments:?}");
    String::from_utf8_lossy(&output.stdout).into_owned()
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
    let planted_server = Command::new(&tmux)
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
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(planted_server.success());
    assert!(
        tmux_reply(&tmux, &socket, &["show-environment", "-g"])
            .contains("GH_TOKEN=planted-gh-token"),
        "the server starts out holding the planted variables"
    );

    let engine = Command::new(std::env::current_exe().unwrap())
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
        .output()
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
