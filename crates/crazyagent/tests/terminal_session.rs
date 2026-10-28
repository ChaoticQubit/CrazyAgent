#![cfg(target_os = "macos")]

use crazyagent::harnesses::interactive::KEY_FILE;
use crazyagent::harnesses::launch::{
    AccountBinding, LaunchEvent, LaunchLimits, LaunchRequest, Secret,
};
use crazyagent::harnesses::terminal_session::TerminalSession;
use crazyagent::terminal::tmux::{SOCKET, Tmux, installed};
use crazyagent::terminal::{AgentId, FOLDER, Host};
use rustix::process::{
    Pid, Signal, getpgid, kill_process, test_kill_process, test_kill_process_group,
};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

const KEY: &str = "sk-stand-in-5521";
const STAND_IN: &str = "#!/bin/sh\n/bin/stty raw -echo\nprintf '\\033[?2004hstand-in ready\\n'\nexec /bin/cat > \"$HOME/typed\"\n";
const SLOW_STAND_IN: &str = "#!/bin/sh\nprintf 'stand-in starting\\n'\n/bin/sleep 3\n/bin/stty raw -echo\nprintf '\\033[?2004hstand-in ready\\n'\nexec /bin/cat > \"$HOME/typed\"\n";
const SERVER_STAND_IN: &str = "#!/bin/sh\n(trap '' HUP TERM; exec /bin/sleep 300) < /dev/null > /dev/null 2>&1 &\necho $! > \"$HOME/server.pid\"\n/bin/stty raw -echo\nprintf '\\033[?2004hstand-in ready\\n'\nexec /bin/cat > \"$HOME/typed\"\n";
const SESSION_SERVER_STAND_IN: &str = "#!/bin/sh\n/usr/bin/perl -MPOSIX -e 'POSIX::setsid() > 0 or die; $SIG{HUP} = $SIG{TERM} = \"IGNORE\"; open(my $out, \">\", \"$ENV{HOME}/server.pid.new\") or die; print $out \"$$\\n\"; close($out) or die; rename(\"$ENV{HOME}/server.pid.new\", \"$ENV{HOME}/server.pid\") or die; exec \"/bin/sleep\", \"300\"' < /dev/null > /dev/null 2>&1 &\n/bin/stty raw -echo\nprintf '\\033[?2004hstand-in ready\\n'\nexec /bin/cat > \"$HOME/typed\"\n";
const ALLOW_ALL: &str = "(version 1)\n(allow default)\n";
const CONFINED: &str =
    "(version 1)\n(allow default)\n(deny signal)\n(allow signal (target same-sandbox))\n";
const EXITING_SERVER_STAND_IN: &str = "#!/bin/sh\n(trap '' HUP TERM; exec /bin/sleep 300) < /dev/null > /dev/null 2>&1 &\necho $! > \"$HOME/server.pid\"\n/bin/stty raw -echo\nprintf '\\033[?2004hstand-in ready\\n'\n/bin/sleep 3\n";

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

    assert_eq!(session.stop(), Ok(None));
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
    assert_eq!(session.stop(), Ok(None));
}

/// Waits until `wanted` holds, at most `wait`. Input: the condition and the wait.
/// Output: whether it held in time.
fn within(wait: Duration, wanted: impl Fn() -> bool) -> bool {
    let until = Instant::now() + wait;
    loop {
        if wanted() {
            return true;
        }
        if Instant::now() >= until {
            return false;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// A stand-in harness running in a pane of its own tmux server, with the agent's home.
struct StandIn {
    session: TerminalSession,
    host: Arc<dyn Host>,
    agent: AgentId,
    home: PathBuf,
    _server: Server,
    _root: tempfile::TempDir,
}

/// Starts `script` as a stand-in Claude Code in a pane named `name` of a fresh tmux server,
/// under the sandbox profile text `profile_text`. Output: the running stand-in.
fn stand_in(script: &str, name: &str, profile_text: &str) -> StandIn {
    let root = tempfile::Builder::new()
        .prefix("ca-pane-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    let root_path = root.path().canonicalize().unwrap();
    private(&root_path);
    let tmux = installed(&root_path).expect("tmux must be installed on this machine");
    let folder = root_path.join(FOLDER);
    let server = Server {
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
    std::fs::write(&executable, script).unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    let profile = root_path.join("agent.sb");
    std::fs::write(&profile, profile_text).unwrap();
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
        git_identity: None,
        limits: LaunchLimits::default(),
    };
    let agent = AgentId::new(name).unwrap();
    let session = TerminalSession::start(host.clone(), agent.clone(), request, None).unwrap();
    StandIn {
        session,
        host,
        agent,
        home,
        _server: server,
        _root: root,
    }
}

/// Waits for the pid a stand-in wrote for the background process it started and checks
/// that process runs in the pane's process group. Takes the home and the group.
fn background(home: &Path, group: Pid) -> Pid {
    let server = written_pid(home);
    assert_eq!(getpgid(Some(server)).unwrap(), group);
    server
}

/// Waits for the pid a stand-in wrote for the background process it started and checks
/// that process runs. Takes the home. Output: the pid.
fn written_pid(home: &Path) -> Pid {
    let written = home.join("server.pid");
    assert!(
        within(Duration::from_secs(15), || {
            std::fs::read_to_string(&written).is_ok_and(|text| text.ends_with('\n'))
        }),
        "the stand-in never started its background process"
    );
    let server = std::fs::read_to_string(&written)
        .unwrap()
        .trim()
        .parse::<i32>()
        .ok()
        .and_then(Pid::from_raw)
        .unwrap();
    assert!(test_kill_process(server).is_ok());
    server
}

/// finding_6dd90dfadd09b216: a pane agent that leaves a process in its group which ignores
/// hangup and terminate, as a dev server started in the background does, has that process
/// ended when the session stops, so no agent process outlives the run that started it.
#[test]
fn stopping_a_pane_session_ends_every_process_left_in_its_group() {
    let pane = stand_in(SERVER_STAND_IN, "stand-in-3", ALLOW_ALL);
    let group = Pid::from_raw(i32::try_from(pane.session.process_group()).unwrap()).unwrap();
    let server = background(&pane.home, group);

    assert_eq!(pane.session.stop(), Ok(None));
    assert!(!pane.host.alive().unwrap().contains(&pane.agent));
    let ended = within(Duration::from_secs(10), || {
        test_kill_process(server).is_err()
    });
    if !ended {
        let _ = kill_process(server, Signal::KILL);
    }
    assert!(ended, "the pane's background process outlived the stop");
    assert!(test_kill_process_group(group).is_err());
}

/// RV-DS-75: a pane whose harness exited unseen for longer than a pane id stays proven
/// its own leaves its group alone, since the id may name a reused group by then, and its
/// stop reports the cleanup uncertain instead of a confirmed stop while that group runs.
#[test]
fn a_pane_gone_unseen_reports_its_surviving_group_uncertain_without_killing_it() {
    let pane = stand_in(EXITING_SERVER_STAND_IN, "stand-in-4", ALLOW_ALL);
    let group = Pid::from_raw(i32::try_from(pane.session.process_group()).unwrap()).unwrap();
    let server = background(&pane.home, group);
    assert!(
        within(Duration::from_secs(15), || !pane
            .host
            .alive()
            .unwrap()
            .contains(&pane.agent)),
        "the stand-in harness never exited"
    );
    std::thread::sleep(Duration::from_secs(6));

    let stopped = pane.session.stop();
    let survived = test_kill_process(server).is_ok();
    let _ = kill_process(server, Signal::KILL);
    assert_eq!(
        stopped.map_err(|problem| problem.0),
        Err("harness_cleanup_uncertain".to_owned()),
        "a pane stop confirmed a stop while a process of its group still ran"
    );
    assert!(
        survived,
        "a stop killed a group its pane id was no longer proven to name"
    );
}

/// PR-1 (rv-code pre-review of RV-DS-75): a pane agent whose tool starts a process in a
/// session of its own, as Claude Code runs every tool command, has that process ended when
/// the session stops, and the stop confirms only once it is gone.
#[test]
fn stopping_a_pane_session_ends_what_its_tools_started_in_their_own_sessions() {
    let pane = stand_in(SESSION_SERVER_STAND_IN, "stand-in-5", CONFINED);
    let group = Pid::from_raw(i32::try_from(pane.session.process_group()).unwrap()).unwrap();
    let server = written_pid(&pane.home);
    assert_ne!(getpgid(Some(server)).unwrap(), group);

    let stopped = pane.session.stop();
    let ended = within(Duration::from_secs(10), || {
        test_kill_process(server).is_err()
    });
    if !ended {
        let _ = kill_process(server, Signal::KILL);
    }
    assert!(
        ended || stopped.is_err(),
        "a process the pane agent started in its own session outlived a stop reported as confirmed"
    );
    assert!(
        ended,
        "a process the pane agent started in its own session outlived the stop"
    );
    assert_eq!(stopped, Ok(None));
}
