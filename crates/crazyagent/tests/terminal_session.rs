#![cfg(target_os = "macos")]

use crazyagent::harnesses::interactive::KEY_FILE;
use crazyagent::harnesses::launch::{
    AccountBinding, LaunchEvent, LaunchLimits, LaunchRequest, Secret,
};
use crazyagent::harnesses::leftovers::Unstopped;
use crazyagent::harnesses::terminal_session::TerminalSession;
use crazyagent::research::runner::{ResearchLimits, ResearchRunner};
use crazyagent::terminal::tmux::{SOCKET, Tmux, installed};
use crazyagent::terminal::{AgentId, FOLDER, Host};
use rustix::process::{
    Pid, Signal, getpgid, kill_process, test_kill_process, test_kill_process_group,
};
use std::collections::BTreeMap;
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
    root: PathBuf,
    _server: Server,
    _root: tempfile::TempDir,
}

/// Starts `script` as a stand-in Claude Code in a pane named `name` of a fresh tmux server,
/// under the sandbox profile text `profile_text`. Output: the running stand-in.
fn stand_in(script: &str, name: &str, profile_text: &str) -> StandIn {
    stand_in_on(script, name, Some(profile_text), false)
}

/// Starts `script` as a stand-in Claude Code in a pane named `name` of a fresh tmux server:
/// under the sandbox profile text `profile_text`, or with Full access when there is none,
/// and on the host the engine runs agents on, which keeps each ended terminal's last
/// screen, when `keeps` is set. Output: the running stand-in.
fn stand_in_on(script: &str, name: &str, profile_text: Option<&str>, keeps: bool) -> StandIn {
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
    let host: Arc<dyn Host> = if keeps {
        crazyagent::terminal::tmux_host(&root_path, Some(tmux))
    } else {
        Arc::new(Tmux::open(Some(tmux), &folder))
    };
    let home = root_path.join("home");
    let work = root_path.join("work");
    let bin = root_path.join("bin");
    for path in [&home, &work, &bin] {
        private(path);
    }
    let executable = bin.join("claude");
    std::fs::write(&executable, script).unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    let profile = profile_text.map(|text| {
        let profile = root_path.join("agent.sb");
        std::fs::write(&profile, text).unwrap();
        profile
    });
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
        unconfined: profile.is_none(),
        sandbox_profile: profile,
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
        root: root_path,
        _server: server,
        _root: root,
    }
}

/// Whether `seen` holds `wanted`. Inputs: the bytes and the text. Output: whether.
fn contains(seen: &[u8], wanted: &[u8]) -> bool {
    seen.windows(wanted.len()).any(|window| window == wanted)
}

/// Stops a stand-in on the engine's host once its app drew its ready line, then checks the
/// host kept the pane's last screen: the pane is gone, `kept` names it, and attaching opens
/// that screen, the ready line under the finished label. Input: the stand-in.
fn stops_keeping_its_last_screen(pane: StandIn) {
    assert!(
        within(Duration::from_secs(15), || pane
            .host
            .screen(&pane.agent)
            .is_ok_and(|screen| contains(&screen, b"stand-in ready"))),
        "the stand-in never drew its ready line"
    );
    assert_eq!(pane.session.stop().unwrap(), None);
    assert!(!pane.host.alive().unwrap().contains(&pane.agent));
    assert!(
        pane.host.kept(&pane.agent),
        "the host keeps the stopped pane's last screen"
    );
    let kept = pane.host.attach(&pane.agent).unwrap();
    assert!(
        contains(&kept.screen, b"stand-in ready") && contains(&kept.screen, b" Finished at "),
        "the kept screen is the pane's last one under its finished label: {}",
        String::from_utf8_lossy(&kept.screen)
    );
}

/// B19-KEPT: a Sandboxed pane's stop ends it through its keeper, which empties the sandbox
/// and ends the pane before the host's kill, and the engine's host still keeps the pane's
/// last screen for its finished card (N-D21, OP-39).
#[test]
fn a_sandboxed_pane_stopped_through_its_keeper_keeps_its_last_screen() {
    stops_keeping_its_last_screen(stand_in_on(STAND_IN, "stand-in-11", Some(ALLOW_ALL), true));
}

/// B19-KEPT: a Full access pane, which stops without a keeper, keeps its last screen on the
/// engine's host as well, so every confinement shows how its run finished.
#[test]
fn a_full_access_pane_stopped_without_a_keeper_keeps_its_last_screen() {
    stops_keeping_its_last_screen(stand_in_on(STAND_IN, "stand-in-12", None, true));
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

    assert_eq!(pane.session.stop().unwrap(), None);
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

/// CB-2 one signal: a Full access pane has no keeper, so the host's kill still finds it
/// running and its stop holds the pane's group across that kill. The stop alone ends what
/// the pane left in its group and confirms only once the group is gone.
#[test]
fn stopping_a_full_access_pane_session_ends_every_process_left_in_its_group() {
    let pane = stand_in_on(SERVER_STAND_IN, "stand-in-13", None, false);
    let group = Pid::from_raw(i32::try_from(pane.session.process_group()).unwrap()).unwrap();
    let server = background(&pane.home, group);

    let stopped = pane.session.stop();
    let ended = within(Duration::from_secs(10), || {
        test_kill_process_group(group) == Err(rustix::io::Errno::SRCH)
    });
    if !ended {
        let _ = kill_process(server, Signal::KILL);
    }
    assert_eq!(stopped.map_err(|problem| problem.code()), Ok(None));
    assert!(!pane.host.alive().unwrap().contains(&pane.agent));
    assert!(ended, "the Full access pane's group outlived the stop");
    assert_eq!(test_kill_process(server), Err(rustix::io::Errno::SRCH));
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
        stopped.map_err(|problem| problem.code()),
        Err("harness_cleanup_uncertain"),
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
    assert_eq!(stopped.map_err(|problem| problem.code()), Ok(None));
}

/// PR-2 (rv-code pre-review of RV-DS-75): a research pane an earlier engine left running,
/// whose harness keeps a background process that ignores hangup, has that process ended when
/// the next engine's research runner sweeps the pane, not only its window. The profile
/// leaves signals unconfined, so the pane's keeper cannot empty the sandbox and only the
/// sweep's own group kill ends the process.
#[test]
fn a_research_pane_an_earlier_engine_left_is_swept_with_what_it_started() {
    let pane = stand_in(SERVER_STAND_IN, "research-left-1", ALLOW_ALL);
    let group = Pid::from_raw(i32::try_from(pane.session.process_group()).unwrap()).unwrap();
    let server = background(&pane.home, group);
    pane.session.leave();
    let engine = pane.root.join("engine");
    private(&engine);
    let state = engine.join("research");
    private(&state);
    let runner = ResearchRunner::new(
        &engine.join("engine.db"),
        &state,
        Path::new("/usr/bin/true"),
        ResearchLimits::default(),
    )
    .unwrap()
    .with_terminal(pane.host.clone())
    .unwrap();
    assert!(!pane.host.alive().unwrap().contains(&pane.agent));
    let ended = within(Duration::from_secs(10), || {
        test_kill_process(server).is_err()
    });
    if !ended {
        let _ = kill_process(server, Signal::KILL);
    }
    drop(runner);
    assert!(
        ended,
        "a research pane an earlier engine left kept its background process after the sweep"
    );
}

const LEAVING_STAND_IN: &str = "#!/bin/sh\n/usr/bin/perl -mPOSIX -e 'POSIX::setsid() > 0 or exit 1; open(my $file, \">\", \"$ENV{HOME}/descendant.pid\") or exit 1; print $file \"$$\\n\"; close($file); sleep 30' < /dev/null > /dev/null 2>&1 &\n(/usr/bin/perl -e '$SIG{HUP} = \"IGNORE\"; setpgrp(0, 0) or exit 1; open(my $file, \">\", \"$ENV{HOME}/member.pid\") or exit 1; print $file \"$$\\n\"; close($file); exec \"/bin/sleep\", \"31\"' < /dev/null > /dev/null 2>&1 &)\n(/usr/bin/nohup /bin/sleep 32 < /dev/null > /dev/null 2>&1 & echo $! > \"$HOME/grouped.pid\")\n/bin/stty raw -echo\nprintf '\\033[?2004hstand-in ready\\n'\nexec /bin/cat > \"$HOME/typed\"\n";

/// Kills, by id, the processes a test's stand-in agent left, however the test ends.
struct Leftover(Vec<rustix::process::Pid>);

impl Drop for Leftover {
    fn drop(&mut self) {
        for process in &self.0 {
            let _ = rustix::process::kill_process(*process, rustix::process::Signal::KILL);
        }
    }
}

fn pid_in(folder: &Path, name: &str) -> Option<rustix::process::Pid> {
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
/// a process in its own session, one in the pane's session but a process group of its own
/// that ignores the hangup, and one in the pane's process group. Ending the pane and its
/// group reaches neither of the first two, so the stop names both instead of reporting a
/// clean stop (CB-2), and it ends the third.
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
    let (descendant, member, grouped) = loop {
        if let (Some(descendant), Some(member), Some(grouped)) = (
            pid_in(&home, "descendant.pid"),
            pid_in(&home, "member.pid"),
            pid_in(&home, "grouped.pid"),
        ) && parent_of(member) == Some(1)
            && parent_of(grouped) == Some(1)
        {
            break (descendant, member, grouped);
        }
        assert!(Instant::now() < deadline, "stand-in agent never started");
        std::thread::sleep(Duration::from_millis(10));
    };
    let _leftover = Leftover(vec![descendant, member, grouped]);
    assert_eq!(rustix::process::getsid(Some(member)), Ok(leader));
    assert_ne!(getpgid(Some(member)), Ok(leader));
    assert_eq!(getpgid(Some(grouped)), Ok(leader));
    assert_ne!(rustix::process::getsid(Some(descendant)), Ok(leader));

    let left = match session.stop() {
        Err(Unstopped::Left(left)) => left,
        other => panic!(
            "the stop of a Full access pane was reported {other:?} while {descendant:?} and {member:?} kept running"
        ),
    };
    assert!(!host.alive().unwrap().contains(&agent));
    assert!(
        within(Duration::from_secs(10), || test_kill_process(grouped)
            .is_err()),
        "the stop of a Full access pane left {grouped:?} of its process group running"
    );
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

/// The longest a pane's keeper is given to end its sandbox before a stop counts as
/// uncertain; a pane with no keeper never waits for it.
const KEEPER_WAIT: Duration = Duration::from_secs(6);

/// A stand-in that stays the pane's first process while its reader runs, and says so once
/// it is ready.
const FIRST_PROCESS_STAND_IN: &str = "#!/bin/sh\n/bin/stty raw -echo\nprintf '\\033[?2004hstand-in ready\\n'\n: > \"$HOME/ready\"\n/bin/cat > \"$HOME/typed\"\n";

/// The same stand-in ignoring hangup and terminate, which its reader inherits.
const DEAF_STAND_IN: &str = "#!/bin/sh\ntrap '' HUP TERM\n/bin/stty raw -echo\nprintf '\\033[?2004hstand-in ready\\n'\n: > \"$HOME/ready\"\n/bin/cat > \"$HOME/typed\"\n";

/// A stand-in that records its parent and whether it may signal it, then reads its input.
const PARENT_STAND_IN: &str = "#!/bin/sh\nif kill -0 \"$PPID\" 2>/dev/null; then may=open; else may=confined; fi\necho \"$PPID $may\" > \"$HOME/parent.new\"\n/bin/mv \"$HOME/parent.new\" \"$HOME/parent\"\n/bin/stty raw -echo\nprintf '\\033[?2004hstand-in ready\\n'\nexec /bin/cat > \"$HOME/typed\"\n";

/// The command line `ps` shows for one process. Input: the pid. Output: the line.
fn command_of(pid: Pid) -> String {
    let listed = crazyagent::spawn_gate::output(
        Command::new("/bin/ps")
            .args(["-o", "command=", "-p"])
            .arg(pid.as_raw_nonzero().get().to_string())
            .stdout(Stdio::piped()),
    )
    .unwrap();
    assert!(listed.status.success(), "ps lists the pane's first process");
    String::from_utf8_lossy(&listed.stdout).trim().to_owned()
}

/// Waits for a stand-in's ready mark in its home. Takes the stand-in.
fn ready(pane: &StandIn) {
    assert!(
        within(Duration::from_secs(15), || pane.home.join("ready").exists()),
        "the stand-in never became ready"
    );
}

/// fix/pane-keeper-unconfined@76d15a92: a Full access (unconfined) pane starts with its
/// harness argv as before build 19, with no keeper and no `launch_sandbox_required`: the
/// pane's first process is the harness itself, and the stop is clean without the keeper's
/// terminate wait.
#[test]
fn a_full_access_pane_runs_its_harness_as_its_first_process_and_stops_without_a_keeper() {
    let pane = stand_in_on(FIRST_PROCESS_STAND_IN, "stand-in-8", None, false);
    ready(&pane);
    let group = Pid::from_raw(i32::try_from(pane.session.process_group()).unwrap()).unwrap();
    let first = command_of(group);
    let harness = pane.root.join("bin/claude");
    assert!(
        first.contains(harness.to_str().unwrap()),
        "the harness is the pane's first process: {first}"
    );
    assert!(
        !first.starts_with(crazyagent::sandbox::KEEPER[0]),
        "no keeper runs first: {first}"
    );

    let stopping = Instant::now();
    let stopped = pane.session.stop();
    let took = stopping.elapsed();
    assert!(
        matches!(stopped, Ok(None)),
        "the stop of a Full access pane was {stopped:?}"
    );
    assert!(took < KEEPER_WAIT, "the stop took {took:?}");
    assert!(!pane.host.alive().unwrap().contains(&pane.agent));
    assert!(test_kill_process_group(group).is_err());
}

/// fix/pane-keeper-unconfined@76d15a92: a Full access harness that ignores terminate still
/// stops: the window and group kills end it, with no keeper wait and no false Uncertain.
#[test]
fn a_full_access_harness_that_ignores_terminate_stops_clean_without_the_keeper_wait() {
    let pane = stand_in_on(DEAF_STAND_IN, "stand-in-9", None, false);
    ready(&pane);
    let group = Pid::from_raw(i32::try_from(pane.session.process_group()).unwrap()).unwrap();

    let stopping = Instant::now();
    let stopped = pane.session.stop();
    let took = stopping.elapsed();
    assert!(
        matches!(stopped, Ok(None)),
        "the stop of a Full access pane that ignores terminate was {stopped:?}"
    );
    assert!(took < KEEPER_WAIT, "the stop took {took:?}");
    assert!(!pane.host.alive().unwrap().contains(&pane.agent));
    assert!(test_kill_process_group(group).is_err());
}

/// fix/pane-keeper-unconfined@76d15a92: a Sandboxed pane still starts as `KEEPER --
/// sandbox-exec -f <profile> PANE_SHIM -- <harness>`: its first process is the keeper, and
/// the harness it runs is the keeper's child inside the confining profile, which keeps it
/// from signalling the keeper.
#[test]
fn a_sandboxed_pane_runs_its_harness_under_the_keeper_inside_the_profile() {
    let pane = stand_in(PARENT_STAND_IN, "stand-in-10", CONFINED);
    let written = pane.home.join("parent");
    assert!(
        within(Duration::from_secs(15), || written.exists()),
        "the stand-in never wrote its parent"
    );
    let group = Pid::from_raw(i32::try_from(pane.session.process_group()).unwrap()).unwrap();
    let first = command_of(group);
    let keeper = crazyagent::sandbox::KEEPER[..4].join(" ");
    assert!(
        first.starts_with(&keeper),
        "the keeper is the pane's first process: {first}"
    );
    let parent = std::fs::read_to_string(&written).unwrap();
    assert_eq!(
        parent.trim(),
        format!("{} confined", group.as_raw_nonzero().get()),
        "the harness runs under the keeper and sandbox-exec's profile"
    );

    assert_eq!(pane.session.stop().unwrap(), None);
    assert!(!pane.host.alive().unwrap().contains(&pane.agent));
}
