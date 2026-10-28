#![cfg(target_os = "macos")]

use crazyagent::harnesses::interactive::KEY_FILE;
use crazyagent::harnesses::launch::{
    AccountBinding, LaunchEvent, LaunchLimits, LaunchRequest, Secret,
};
use crazyagent::harnesses::leftovers::Unstopped;
use crazyagent::harnesses::terminal_session::TerminalSession;
use crazyagent::terminal::tmux::{SOCKET, Tmux, installed};
use crazyagent::terminal::{AgentId, FOLDER, Host};
use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

const KEY: &str = "sk-stand-in-5521";
const STAND_IN: &str = "#!/bin/sh\n/bin/stty raw -echo\nprintf '\\033[?2004hstand-in ready\\n'\nexec /bin/cat > \"$HOME/typed\"\n";
const SLOW_STAND_IN: &str = "#!/bin/sh\nprintf 'stand-in starting\\n'\n/bin/sleep 3\n/bin/stty raw -echo\nprintf '\\033[?2004hstand-in ready\\n'\nexec /bin/cat > \"$HOME/typed\"\n";

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

fn private(path: &Path) {
    std::fs::create_dir_all(path).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
}

fn typed_until(file: &Path, wanted: &[u8]) -> Vec<u8> {
    let until = Instant::now() + Duration::from_secs(15);
    loop {
        let seen = std::fs::read(file).unwrap_or_default();
        if seen.windows(wanted.len()).any(|window| window == wanted) || Instant::now() >= until {
            return seen;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Wiring test: a shell script stands in for Claude Code in a real tmux pane, so this
/// checks TerminalSession's paste, key file and stop only. A real harness in a terminal:
/// e2e_tui.rs the_tui_types_into_a_live_agent_terminal_from_the_team_tree, which needs
/// a live team and reports UNAVAILABLE otherwise.
#[test]
fn wiring_a_shell_stand_in_in_a_real_tmux_pane_gets_the_prompt_as_one_paste_and_stops_with_its_key_file()
 {
    let root = tempfile::Builder::new()
        .prefix("ca-pane-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    let root_path = root.path().canonicalize().unwrap();
    private(&root_path);
    let tmux = installed(&root_path).expect("tmux must be installed on this machine");
    let folder = root_path.join(FOLDER);
    let _server = Server {
        tmux: tmux.clone(),
        socket: folder.join(SOCKET),
    };
    let host: Arc<dyn Host> = Arc::new(Tmux::open(Some(tmux), &folder));
    let home = root_path.join("home");
    let work = root_path.join("work");
    let bin = root_path.join("bin");
    for path in [&home, &work, &bin] {
        private(path);
    }
    let executable = bin.join("claude");
    std::fs::write(&executable, STAND_IN).unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    let profile = root_path.join("agent.sb");
    std::fs::write(&profile, "(version 1)\n(allow default)\n").unwrap();
    let request = LaunchRequest {
        harness: "claude-code".into(),
        executable,
        working_dir: work,
        account: AccountBinding::ApiKey {
            home: home.clone(),
            key: Secret::new(KEY.into()).unwrap(),
            base_url: Some("http://127.0.0.1:9/v1".into()),
        },
        model: None,
        tools: true,
        sandbox_profile: Some(profile),
        unconfined: false,
        git_identity: None,
        limits: LaunchLimits::default(),
    };
    let agent = AgentId::new("stand-in-1").unwrap();
    let mut session = TerminalSession::start(host.clone(), agent.clone(), request, None).unwrap();
    assert!(session.process_group() > 1);
    assert!(host.alive().unwrap().contains(&agent));
    match session.next_event(Duration::from_secs(1)).unwrap() {
        Some(LaunchEvent::Ready {
            native_session: Some(id),
        }) => assert_eq!(id.len(), 36),
        other => panic!("expected Ready with the session id, got {other:?}"),
    }
    let key_file = home.join(KEY_FILE);
    let placeholder = std::fs::read_to_string(&key_file).unwrap();
    assert!(
        crazyagent::model_gateway::relay::is_placeholder(&placeholder),
        "the pane's key file holds the relay token, never the key"
    );
    assert!(!placeholder.contains(KEY));
    assert_eq!(
        std::fs::metadata(&key_file).unwrap().permissions().mode() & 0o777,
        0o600
    );

    session.send("hello\x1b world\nsecond line").unwrap();
    let pasted = b"\x1b[200~hello world\nsecond line\x1b[201~\r";
    let seen = typed_until(&home.join("typed"), pasted);
    assert!(
        seen.windows(pasted.len()).any(|window| window == pasted),
        "{seen:?}"
    );
    assert_eq!(session.send("again").unwrap_err().0, "launch_busy");
    session.interrupt().unwrap();
    let seen = typed_until(&home.join("typed"), b"\r\x1b");
    assert!(seen.ends_with(b"\r\x1b"), "{seen:?}");
    assert_eq!(
        session.next_event(Duration::from_millis(300)).unwrap(),
        None
    );

    assert_eq!(session.stop().unwrap(), None);
    assert!(!host.alive().unwrap().contains(&agent));
    assert!(!key_file.exists());
    let config = std::fs::read_to_string(home.join(".claude.json")).unwrap();
    assert!(config.contains("hasCompletedOnboarding"));
}

/// Wiring test: a shell script stands in for Claude Code in a real tmux pane, so this
/// checks TerminalSession's first-prompt timing only. A real harness in a terminal:
/// e2e_tui.rs the_tui_types_into_a_live_agent_terminal_from_the_team_tree, which needs
/// a live team and reports UNAVAILABLE otherwise.
#[test]
fn wiring_the_first_prompt_waits_until_the_app_reads_keys_itself() {
    let root = tempfile::Builder::new()
        .prefix("ca-pane-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    let root_path = root.path().canonicalize().unwrap();
    private(&root_path);
    let tmux = installed(&root_path).expect("tmux must be installed on this machine");
    let folder = root_path.join(FOLDER);
    let _server = Server {
        tmux: tmux.clone(),
        socket: folder.join(SOCKET),
    };
    let host: Arc<dyn Host> = Arc::new(Tmux::open(Some(tmux), &folder));
    let home = root_path.join("home");
    let work = root_path.join("work");
    let bin = root_path.join("bin");
    for path in [&home, &work, &bin] {
        private(path);
    }
    let executable = bin.join("claude");
    std::fs::write(&executable, SLOW_STAND_IN).unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    let profile = root_path.join("agent.sb");
    std::fs::write(&profile, "(version 1)\n(allow default)\n").unwrap();
    let request = LaunchRequest {
        harness: "claude-code".into(),
        executable,
        working_dir: work,
        account: AccountBinding::ApiKey {
            home: home.clone(),
            key: Secret::new(KEY.into()).unwrap(),
            base_url: Some("http://127.0.0.1:9/v1".into()),
        },
        model: None,
        tools: true,
        sandbox_profile: Some(profile),
        unconfined: false,
        git_identity: None,
        limits: LaunchLimits::default(),
    };
    let agent = AgentId::new("stand-in-2").unwrap();
    let mut session = TerminalSession::start(host.clone(), agent, request, None).unwrap();
    let started = Instant::now();
    session.send("first prompt").unwrap();
    assert!(
        started.elapsed() < Duration::from_secs(20),
        "the prompt goes in once the app reads keys, not after the whole ready wait"
    );
    let pasted = b"\x1b[200~first prompt\x1b[201~\r";
    let seen = typed_until(&home.join("typed"), pasted);
    assert!(
        seen.ends_with(pasted),
        "a prompt typed before the app reads keys loses its Enter: {seen:?}"
    );
    assert_eq!(session.stop().unwrap(), None);
}

const LEAVING_STAND_IN: &str = "#!/bin/sh\n/usr/bin/perl -mPOSIX -e 'POSIX::setsid() > 0 or exit 1; open(my $file, \">\", \"$ENV{HOME}/descendant.pid\") or exit 1; print $file \"$$\\n\"; close($file); sleep 30' < /dev/null > /dev/null 2>&1 &\n(/usr/bin/nohup /bin/sleep 31 < /dev/null > /dev/null 2>&1 & echo $! > \"$HOME/member.pid\")\n/bin/stty raw -echo\nprintf '\\033[?2004hstand-in ready\\n'\nexec /bin/cat > \"$HOME/typed\"\n";

/// Kills, by id, the processes a test's stand-in agent left, however the test ends.
struct Leftover(Vec<rustix::process::Pid>);

impl Drop for Leftover {
    fn drop(&mut self) {
        for process in &self.0 {
            let _ = rustix::process::kill_process(*process, rustix::process::Signal::KILL);
        }
    }
}

fn written_pid(folder: &Path, name: &str) -> Option<rustix::process::Pid> {
    std::fs::read_to_string(folder.join(name))
        .ok()
        .and_then(|text| text.trim().parse::<i32>().ok())
        .and_then(rustix::process::Pid::from_raw)
}

/// Reads a process's parent id with `ps`. Input: the process. Output: the parent, or none
/// once it has ended.
fn parent_of(process: rustix::process::Pid) -> Option<i32> {
    let listing = crazyagent::spawn_gate::output(
        Command::new("/bin/ps")
            .args(["-o", "ppid=", "-p", &process.as_raw_nonzero().to_string()])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null()),
    )
    .unwrap();
    String::from_utf8_lossy(&listing.stdout).trim().parse().ok()
}

/// The ids and programs a leftovers report names. Input: the report. Output: them.
fn named_in(report: &str) -> BTreeMap<i32, String> {
    let (_, named) = report.split_once("still running: ").unwrap();
    let (named, _) = named.split_once(". ").unwrap();
    named
        .split(", ")
        .map(|entry| {
            let (pid, program) = entry.split_once(' ').unwrap();
            (pid.parse().unwrap(), program.to_owned())
        })
        .collect()
}

/// A shell script stands in for Claude Code at Full access in a real tmux pane and leaves
/// a process in its own session and one in the pane's session that ignores the hangup.
/// Ending the pane reaches neither, so the stop names both instead of reporting a clean
/// stop (CB-2).
#[test]
fn stopping_a_full_access_pane_names_what_it_left_running() {
    let root = tempfile::Builder::new()
        .prefix("ca-pane-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    let root_path = root.path().canonicalize().unwrap();
    private(&root_path);
    let tmux = installed(&root_path).expect("tmux must be installed on this machine");
    let folder = root_path.join(FOLDER);
    let _server = Server {
        tmux: tmux.clone(),
        socket: folder.join(SOCKET),
    };
    let host: Arc<dyn Host> = Arc::new(Tmux::open(Some(tmux), &folder));
    let home = root_path.join("home");
    let work = root_path.join("work");
    let bin = root_path.join("bin");
    for path in [&home, &work, &bin] {
        private(path);
    }
    let executable = bin.join("claude");
    std::fs::write(&executable, LEAVING_STAND_IN).unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    let request = LaunchRequest {
        harness: "claude-code".into(),
        executable,
        working_dir: work,
        account: AccountBinding::Subscription { home: home.clone() },
        model: None,
        tools: false,
        sandbox_profile: None,
        unconfined: true,
        git_identity: None,
        limits: LaunchLimits::default(),
    };
    let agent = AgentId::new("stand-in-3").unwrap();
    let session = TerminalSession::start(host.clone(), agent.clone(), request, None).unwrap();
    let leader =
        rustix::process::Pid::from_raw(i32::try_from(session.process_group()).unwrap()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    let (descendant, member) = loop {
        if let (Some(descendant), Some(member)) = (
            written_pid(&home, "descendant.pid"),
            written_pid(&home, "member.pid"),
        ) && parent_of(member) == Some(1)
        {
            break (descendant, member);
        }
        assert!(Instant::now() < deadline, "stand-in agent never started");
        std::thread::sleep(Duration::from_millis(10));
    };
    let _leftover = Leftover(vec![descendant, member]);
    assert_eq!(rustix::process::getsid(Some(member)), Ok(leader));
    assert_ne!(rustix::process::getsid(Some(descendant)), Ok(leader));

    let left = match session.stop() {
        Err(Unstopped::Left(left)) => left,
        other => panic!(
            "the stop of a Full access pane was reported {other:?} while {descendant:?} and {member:?} kept running"
        ),
    };
    assert!(!host.alive().unwrap().contains(&agent));
    let report = left.plain();
    assert_eq!(
        named_in(&report),
        BTreeMap::from([
            (descendant.as_raw_nonzero().get(), "perl".to_owned()),
            (member.as_raw_nonzero().get(), "sleep".to_owned()),
        ]),
        "{report}"
    );
    assert!(
        report.ends_with("Its account stays held until they end or you sign it out"),
        "{report}"
    );
}
