use super::installed;
use kernel_processes::terminal::tmux::Tmux;
use kernel_processes::terminal::{AgentId, Host, PaneCommand};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

#[path = "../../../tests/pgid_support/mod.rs"]
mod pgid_support;

struct Server {
    tmux: PathBuf,
    socket: PathBuf,
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
    let mut child = None;
    let started_child = crate::poll_support::poll_until(deadline, || {
        child = std::fs::read_to_string(started)
            .ok()
            .and_then(|text| text.trim().parse::<i32>().ok())
            .and_then(rustix::process::Pid::from_raw);
        child.is_some()
    });
    assert!(started_child, "the pane started its child");
    let child = child.expect("the poll held");
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
    let ended = crate::poll_support::poll_until(deadline, || {
        rustix::process::test_kill_process_group(group.0) == Err(rustix::io::Errno::SRCH)
            && rustix::process::test_kill_process(child) == Err(rustix::io::Errno::SRCH)
    });
    if ended {
        group.1.set(true);
    }
    ended
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
    let host = Tmux::open(
        Some(tmux.clone()),
        &folder,
        crate::sandbox::confinement(),
        super::tmux_hooks(),
    );
    let _server = Server {
        tmux,
        socket: host.socket(),
    };
    let started = home.path().join("child.pid");
    let agent = AgentId::team(1).unwrap();
    host.spawn(&agent, &hangup_ignored_agent(home.path(), &started))
        .unwrap();
    let pane = host.panes(&agent).unwrap().remove(0);
    let (group, child) = left_in_group(pane.pid, &started);
    let held = kernel_processes::group::Ending::claim(pane.pid)
        .expect("no path holds the new pane's group");

    host.kill(&agent).unwrap();
    kernel_processes::terminal::tmux::test_seams::join_reapers(&host);
    let survived = rustix::process::test_kill_process(child).is_ok();
    let ended = crate::harnesses::launch::end_pane_group(pane.pid, Some(held));
    assert!(
        survived,
        "the host signalled a pane group another path holds"
    );
    assert!(ended, "the holder ends the group");
    assert!(gone(&group, child), "the pane's group is gone");
}

/// A tmux program answering as the stand-in server a lead swapped in for its own would: it
/// reports the lowest version the host takes ([`kernel_processes::terminal::tmux::VERSION_MIN`]), its session lists `agent` and
/// every pane it reports has `pid` as its first process. Takes the folder it goes in, the
/// agent and the pid. Returns the program.
#[cfg(target_os = "macos")]
fn forging_tmux(folder: &Path, agent: &AgentId, pid: impl std::fmt::Display) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let program = folder.join("forging-tmux");
    let (major, minor) = kernel_processes::terminal::tmux::VERSION_MIN;
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
    _home: tempfile::TempDir,
    _listener: std::os::unix::net::UnixListener,
    _address: PathBuf,
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
        let host = Tmux::open(
            Some(forging_tmux(home.path(), &agent, pid)),
            &folder,
            crate::sandbox::confinement(),
            super::tmux_hooks(),
        );
        let address = kernel_processes::socket_address::address(&host.socket()).unwrap();
        let listener = kernel_processes::spawn_gate::bind_unix_std(&address).unwrap();
        Self {
            _home: home,
            _listener: listener,
            _address: address,
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

impl Drop for Server {
    fn drop(&mut self) {
        let _ = kernel_processes::spawn_gate::status(
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

#[cfg(target_os = "macos")]
impl Drop for Group {
    fn drop(&mut self) {
        if !self.1.get() {
            let _ = rustix::process::kill_process_group(self.0, rustix::process::Signal::KILL);
        }
    }
}
