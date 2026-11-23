use super::installed;
use kernel_processes::terminal::tmux::{SOCKET, Tmux};
use kernel_processes::terminal::{AgentId, Host, HostError, PaneCommand, TerminalStream};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[path = "../../../tests/pgid_support/mod.rs"]
mod pgid_support;

/// Types `#` straight into each pane by its id, then waits, for at most ten seconds, until
/// each pane's file ends with it, so every byte typed into that pane before it has arrived.
/// Takes the host and the panes with their files. Returns each file's bytes, in order.
#[cfg(target_os = "macos")]
fn marked(host: &Tmux, panes: &[(&str, &Path)]) -> Vec<Vec<u8>> {
    assert!(!panes.is_empty());
    let control = host.live_control().unwrap();
    for (pane, _) in panes {
        assert!(pane_id_valid(pane));
        control.run(&format!("send-keys -t {pane} -H 23")).unwrap();
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    panes
        .iter()
        .map(|(_, file)| {
            loop {
                let bytes = std::fs::read(file).unwrap_or_default();
                if bytes.ends_with(b"#") {
                    break bytes;
                }
                assert!(
                    Instant::now() < deadline,
                    "no mark reached {}",
                    file.display()
                );
                std::thread::sleep(Duration::from_millis(20));
            }
        })
        .collect()
}

struct Server {
    tmux: PathBuf,
    socket: PathBuf,
}

/// Splits a pane of a test's own private tmux server, as a native harness splits its window
/// for a teammate. Takes the server, the pane to split and the new pane's shell command.
/// Panics unless tmux splits it.
#[cfg(target_os = "macos")]
fn split(server: &Server, pane: &str, command: &str) {
    let status = kernel_processes::spawn_gate::status(
        Command::new(&server.tmux)
            .arg("-S")
            .arg(&server.socket)
            .args(["split-window", "-d", "-t", pane, command])
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null()),
    )
    .unwrap();
    assert!(status.success(), "tmux split the pane");
}

/// A process group a test read from a pane, killed when the test ends unless the test saw
/// it gone, so a failing check leaves no stand-in process running and a group number that
/// answered ESRCH is never signalled (r1 SG-L1).
#[cfg(target_os = "macos")]
struct Group(rustix::process::Pid, std::cell::Cell<bool>);

/// The shell line of a pane whose first process keeps in its group a child that ignores the
/// hangup tmux sends a closing pane, as a busy Codex app-server outlives its bridge pane.
/// The child writes its pid to `started` once it ignores the hangup. Takes that file, whose
/// path holds no shell syntax. Returns the line.
#[cfg(target_os = "macos")]
fn hangup_ignored(started: &Path) -> String {
    let path = started.to_str().unwrap();
    assert!(
        path.bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"/._-".contains(&byte)),
        "{path}"
    );
    format!(
        "/bin/sh -c 'trap \"\" HUP; echo $$ > {path}; exec /bin/sleep 120' & exec /bin/sleep 120"
    )
}

/// An agent command that runs the `hangup_ignored` line. Takes its working folder and the
/// child's pid file. Returns the command.
#[cfg(target_os = "macos")]
fn hangup_ignored_agent(cwd: &Path, started: &Path) -> PaneCommand {
    PaneCommand {
        argv: vec!["/bin/sh".into(), "-c".into(), hangup_ignored(started)],
        cwd: cwd.to_path_buf(),
        env: Vec::new(),
    }
}

/// Waits for the child of a `hangup_ignored` pane and checks that it is in the group the
/// pane's first process leads, once `pgid_support::pane_group` sees that process lead it.
/// Takes the pane's first process id and the child's pid file. Returns the group, killed
/// when dropped, and the child.
#[cfg(target_os = "macos")]
fn left_in_group(pane: u32, started: &Path) -> (Group, rustix::process::Pid) {
    let leader = pgid_support::pane_group(pane);
    let group = Group(leader, std::cell::Cell::new(false));
    let deadline = Instant::now() + Duration::from_secs(10);
    let child = loop {
        let written = std::fs::read_to_string(started).ok();
        if let Some(child) = written
            .and_then(|text| text.trim().parse::<i32>().ok())
            .and_then(rustix::process::Pid::from_raw)
        {
            break child;
        }
        assert!(Instant::now() < deadline, "the pane started its child");
        std::thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(
        rustix::process::getpgid(Some(child)),
        Ok(leader),
        "the child stays in the pane's group"
    );
    (group, child)
}

/// Waits for at most ten seconds until neither the group nor its child is left; macOS
/// answers EPERM for a group whose last member it just reaped before it answers ESRCH.
/// A gone group is marked so its guard leaves it alone. Takes both. Returns whether both
/// are gone.
#[cfg(target_os = "macos")]
fn gone(group: &Group, child: rustix::process::Pid) -> bool {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if rustix::process::test_kill_process_group(group.0) == Err(rustix::io::Errno::SRCH)
            && rustix::process::test_kill_process(child) == Err(rustix::io::Errno::SRCH)
        {
            group.1.set(true);
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// CB-2 one signal: a close leaves a pane group another path holds to that path, as the
/// stop that ends the pane holds its group from before the host's kill, so the host never
/// signals that group as well. The holder then ends it.
#[cfg(target_os = "macos")]
#[test]
fn a_close_leaves_a_pane_group_another_path_holds_to_that_path() {
    use std::os::unix::fs::PermissionsExt;
    let home = tempfile::tempdir().unwrap();
    let folder = home.path().join("terminal");
    std::fs::create_dir(&folder).unwrap();
    std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o700)).unwrap();
    let tmux = installed(home.path()).expect("tmux is required for this test");
    let host = Tmux::open(Some(tmux.clone()), &folder, crate::sandbox::confinement(), super::tmux_hooks());
    let _server = Server {
        tmux,
        socket: host.socket(),
    };
    let started = home.path().join("child.pid");
    let agent = AgentId::team(1).unwrap();
    host.spawn(
        &agent,
        &hangup_ignored_agent(
            home.path(),
            &started,
        ),
    )
    .unwrap();
    let pane = host.panes(&agent).unwrap().remove(0);
    let (group, child) = left_in_group(pane.pid, &started);
    let held = kernel_processes::group::Ending::claim(pane.pid)
        .expect("no path holds the new pane's group");

    host.kill(&agent).unwrap();
    host.join_reapers();
    let survived = rustix::process::test_kill_process(child).is_ok();
    let ended = crate::harnesses::launch::end_pane_group(pane.pid, Some(held));
    assert!(
        survived,
        "the host signalled a pane group another path holds"
    );
    assert!(ended, "the holder ends the group");
    assert!(gone(&group, child), "the pane's group is gone");
}

/// Counts the pane listings a host's held control client ran. Input: the host. Output: the
/// count, zero when no control client is held.
#[cfg(target_os = "macos")]
fn listed(host: &Tmux) -> usize {
    host.control.lock().unwrap().as_ref().map_or(0, |control| {
        control.listed.load(std::sync::atomic::Ordering::Relaxed)
    })
}

/// v242 typing C (BRIEF-typing-path): a pane view types its keys straight into the pane it
/// was attached to, with no pane listing per key while the control client that found that
/// pane still serves it; a pane its attach did not find never takes the view's keys, and a
/// pane found by another connection is typed into only after the usual lookup.
#[cfg(target_os = "macos")]
#[test]
fn a_pane_view_types_keys_into_its_attached_pane_with_no_pane_listing() {
    use std::os::unix::fs::PermissionsExt;
    const KEYS: usize = 20;
    let home = tempfile::tempdir().unwrap();
    let folder = home.path().join("terminal");
    std::fs::create_dir(&folder).unwrap();
    std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o700)).unwrap();
    let tmux = installed(home.path()).expect("tmux is required for this test");
    let host = Arc::new(Tmux::open(Some(tmux.clone()), &folder, crate::sandbox::confinement(), super::tmux_hooks()));
    let socket = host.socket();
    let _server = Server {
        tmux: tmux.clone(),
        socket: socket.clone(),
    };
    let shell = |script: &str| PaneCommand {
        argv: vec!["/bin/sh".into(), "-c".into(), script.into()],
        cwd: home.path().to_path_buf(),
        env: Vec::new(),
    };
    let lead = AgentId::team(1).unwrap();
    let other = AgentId::team(2).unwrap();
    host.spawn(
        &lead,
        &shell(
            "echo lead-own-screen; exec /bin/cat",
        ),
    )
    .unwrap();
    host.spawn(
        &other,
        &shell(
            "echo other-lead-screen; exec /bin/cat",
        ),
    )
    .unwrap();
    let client = |args: &[&str]| {
        let output = kernel_processes::spawn_gate::output(
            Command::new(&tmux)
                .arg("-S")
                .arg(&socket)
                .args(args)
                .env_clear()
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped()),
        )
        .unwrap();
        assert!(
            output.status.success(),
            "tmux {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    };
    let teammate = client(&[
        "split-window",
        "-d",
        "-P",
        "-F",
        "#{pane_id}",
        "-t",
        &target(&lead),
        "--",
        "/bin/sh",
        "-c",
        "echo teammate-pane-ready; exec /bin/cat",
    ]);
    let elsewhere = client(&["list-panes", "-t", &target(&other), "-F", "#{pane_id}"]);
    assert!(pane_id_valid(&teammate), "{teammate}");
    assert!(pane_id_valid(&elsewhere), "{elsewhere}");
    assert_ne!(teammate, elsewhere);
    let shared: Arc<dyn Host> = host.clone();
    let shows = |terminal: &dyn Host, agent: &AgentId, wanted: &str| -> String {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            let screen =
                String::from_utf8_lossy(&terminal.snapshot(agent).unwrap().screen).into_owned();
            if screen.contains(wanted) {
                return screen;
            }
            assert!(
                Instant::now() < deadline,
                "the pane showed {wanted} in time: {screen}"
            );
            std::thread::sleep(Duration::from_millis(100));
        }
    };
    let view = kernel_processes::terminal::pane_view(shared.clone(), &teammate).unwrap();
    shows(&*view, &lead, "teammate-pane-ready");
    let attached = view.attach(&lead).unwrap();
    assert!(
        shared.attach(&lead).unwrap().at.is_none(),
        "an own view on the engine's server keeps typing by the agent's name"
    );

    let typed = "q".repeat(KEYS);
    let before = listed(&host);
    for _ in 0..KEYS {
        kernel_processes::terminal::type_into(&*view, attached.at.as_ref(), &lead, b"q").unwrap();
    }
    kernel_processes::terminal::type_into(&*view, attached.at.as_ref(), &lead, b"\r").unwrap();
    assert_eq!(
        listed(&host),
        before,
        "{KEYS} keys typed into an attached pane view listed no panes"
    );
    shows(&*view, &lead, &typed);
    let own = shows(&*shared, &lead, "lead-own-screen");
    assert!(
        !own.contains(&typed),
        "the keys reached the teammate's pane only: {own}"
    );

    let at = attached
        .at
        .clone()
        .expect("a pane view's stream names the pane it was attached to");
    assert_eq!(at.pane, teammate);
    let forged = kernel_processes::terminal::PaneAt {
        pane: elsewhere.clone(),
        serial: at.serial,
    };
    kernel_processes::terminal::type_into(&*view, Some(&forged), &lead, b"forged-pane\r").unwrap();
    shows(&*view, &lead, "forged-pane");
    let stale = kernel_processes::terminal::PaneAt {
        pane: teammate.clone(),
        serial: at.serial.wrapping_add(1),
    };
    let before = listed(&host);
    kernel_processes::terminal::type_into(&*view, Some(&stale), &lead, b"stale-serial\r").unwrap();
    assert!(
        listed(&host) > before,
        "a pane found by another connection is typed into only after the lookup"
    );
    shows(&*view, &lead, "stale-serial");
    let seen = shows(&*shared, &other, "other-lead-screen");
    assert!(
        !seen.contains("forged-pane"),
        "a pane the view was not attached to never took its keys: {seen}"
    );
    assert!(
        !seen.contains("stale-serial") && !seen.contains(&typed),
        "no key of the view reached another agent's pane: {seen}"
    );

    let chain = crate::terminal::tmux_host(home.path(), Some(tmux.clone()));
    let routed = kernel_processes::terminal::pane_view(chain, &teammate).unwrap();
    let through = routed
        .attach(&lead)
        .unwrap()
        .at
        .expect("a pane view on the engine's host chain names the pane it was attached to");
    assert_eq!(through.pane, teammate);
    assert_ne!(
        through.serial, at.serial,
        "each host keeps its own control client"
    );
    assert_eq!(
        routed
            .input_at(&lead, &through, b"chain-typed\r")
            .map_err(|problem| problem.0),
        Ok(true),
        "the engine's host chain types straight into the attached pane"
    );
    shows(&*view, &lead, "chain-typed");
    assert_eq!(
        routed
            .input_at(&lead, &at, b"other-client\r")
            .map_err(|problem| problem.0),
        Ok(false),
        "a pane found by another control client is refused on the fast path"
    );
}

/// A tmux program answering as the stand-in server a lead swapped in for its own would: it
/// reports the lowest version the host takes ([`VERSION_MIN`]), its session lists `agent` and
/// every pane it reports has `pid` as its first process. Takes the folder it goes in, the
/// agent and the pid. Returns the program.
#[cfg(target_os = "macos")]
fn forging_tmux(folder: &Path, agent: &AgentId, pid: impl std::fmt::Display) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let program = folder.join("forging-tmux");
    let (major, minor) = VERSION_MIN;
    let script = format!(
        r#"#!/bin/sh
printf '%%begin 1 0 1\n%%end 1 0 1\n'
number=1
while IFS= read -r line; do
  printf '%%begin 1 %s 1\n' "$number"
  case "$line" in
    *'#{{version}}'*) printf '{major}.{minor} end\n' ;;
    *list-windows*) printf '{agent} end\n' ;;
    *list-panes*) printf '%%1 {pid} 0 end\n' ;;
    *new-window*) printf '%%2 {pid} end\n' ;;
  esac
  printf '%%end 1 %s 1\n' "$number"
  number=$((number + 1))
done
"#
    );
    std::fs::write(&program, script).unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
    program
}

/// A host whose socket has a listener in this test process and whose tmux program forges
/// every reply: the swap a native lead can make at its own socket path.
#[cfg(target_os = "macos")]
struct StandIn {
    home: tempfile::TempDir,
    _listener: std::os::unix::net::UnixListener,
    address: PathBuf,
    agent: AgentId,
    host: Tmux,
}

#[cfg(target_os = "macos")]
impl StandIn {
    /// Opens the host and binds the stand-in listener. Takes the pid its replies name.
    /// Returns the stand-in, which this process listens for.
    fn naming(pid: impl std::fmt::Display) -> Self {
        use std::os::unix::fs::PermissionsExt;
        let home = tempfile::tempdir().unwrap();
        let folder = home.path().join("terminal");
        std::fs::create_dir(&folder).unwrap();
        std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o700)).unwrap();
        let agent = AgentId::team(1).unwrap();
        let host = Tmux::open(Some(forging_tmux(home.path(), &agent, pid)), &folder, crate::sandbox::confinement(), super::tmux_hooks());
        let address = host.address().unwrap();
        let listener = kernel_processes::spawn_gate::bind_unix_std(&address).unwrap();
        Self {
            home,
            _listener: listener,
            address,
            agent,
            host,
        }
    }
}

/// Starts a process leading its own group under a shell that stays, so neither this test nor
/// a stand-in listening in it is its parent. Returns the shell and the group, killed when
/// dropped.
#[cfg(target_os = "macos")]
fn foreign_group() -> (Child, Group) {
    let mut shell = kernel_processes::spawn_gate::spawn(
        Command::new("/bin/sh")
            .args([
                "-c",
                "/usr/bin/perl -e 'setpgrp(0, 0); sleep 120' & echo $!; wait",
            ])
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null()),
    )
    .unwrap();
    let mut line = String::new();
    BufReader::new(shell.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let leader = pgid_support::pane_group(line.trim().parse().unwrap());
    (shell, Group(leader, std::cell::Cell::new(false)))
}

#[cfg(target_os = "macos")]
fn answers(group: &Group) -> bool {
    rustix::process::test_kill_process_group(group.0).is_ok()
}

#[cfg(target_os = "macos")]
#[test]
fn a_left_pane_cleanup_leaves_alone_a_process_a_stand_in_listener_names_but_did_not_start() {
    let (mut shell, group) = foreign_group();
    let stand_in = StandIn::naming(group.0.as_raw_nonzero());

    let ended =
        crate::harnesses::terminal_session::end_left_pane(&stand_in.host, &stand_in.agent, "test")
            .map_err(|problem| problem.0);

    assert!(
        answers(&group),
        "the cleanup signalled a process the stand-in only named"
    );
    assert_eq!(
        ended,
        Ok(false),
        "a pane no server vouched for stays uncertain"
    );
    drop(group);
    let _ = shell.wait();
}

#[cfg(target_os = "macos")]
#[test]
fn a_reply_naming_a_pid_no_process_can_have_names_none() {
    let stand_in = StandIn::naming(u32::MAX);

    assert_eq!(
        stand_in
            .host
            .pane_pid(&stand_in.agent)
            .map_err(|problem| problem.0),
        Ok(None)
    );
    assert_eq!(
        crate::harnesses::terminal_session::end_left_pane(&stand_in.host, &stand_in.agent, "test",)
            .map_err(|problem| problem.0),
        Ok(false)
    );
}

/// Writes an `sh` stand-in for tmux that adds a line to `ran` each time it starts, then does
/// `rest`. Takes the folder, the counting file and the rest of the script. Returns the tmux
/// as the resolver gives it.
fn counted_tmux(
    folder: &std::path::Path,
    ran: &std::path::Path,
    rest: &str,
) -> kernel_processes::finding::Resolved {
    use std::os::unix::fs::PermissionsExt;
    let program = folder.join(kernel_processes::finding::BUNDLED_TMUX);
    let script = format!("#!/bin/sh\necho ran >> '{}'\n{rest}\n", ran.display());
    std::fs::write(&program, script).unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
    kernel_processes::finding::Resolved {
        entry: program.clone(),
        program,
        kind: kernel_processes::finding::Kind::Native,
    }
}

/// How many times a [`counted_tmux`] started. Takes its counting file. Returns the count.
fn started(ran: &std::path::Path) -> usize {
    std::fs::read_to_string(ran).map_or(0, |text| text.lines().count())
}

const MARKER: &str = "5f0c1d2e3a4b59687766554433221100";

/// An attach waiting at the front of the reader's queue. Takes what its located reply read,
/// if it came yet, and whether it opens a view. Returns the request and its answer.
fn attach(
    located: Option<&[u8]>,
    view: bool,
) -> (Pending, Receiver<Result<TerminalStream, HostError>>) {
    let (reply, answer) = sync_channel(1);
    let pending = Pending::Attach {
        agent: AgentId::team(1).unwrap(),
        located: located.map(|line| vec![line.to_vec()]),
        history: 0,
        view,
        reply,
        marker: MARKER.as_bytes().to_vec(),
    };
    (pending, answer)
}

/// What an attach answered: the size of the screen it opened, or the code it failed with,
/// none while it has no answer. Takes its answer.
fn attached(
    answer: &Receiver<Result<TerminalStream, HostError>>,
) -> Option<Result<(u16, u16), String>> {
    answer.try_recv().ok().map(|opened| {
        opened
            .map(|stream| (stream.cols, stream.rows))
            .map_err(|problem| problem.0)
    })
}
