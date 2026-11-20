use super::*;

use std::os::unix::fs::PermissionsExt;

#[cfg(target_os = "macos")]
use std::os::unix::process::CommandExt;

const CHILD: &str = include_str!("../../tests/fixtures/agent_child.py");

const CRASH_ROOT: &str = "CRAZYAGENT_AGENTS_CRASH_ROOT";

const CRASH_PROMPT: &str = "CRAZYAGENT_AGENTS_CRASH_PROMPT";

const OWNER_WRITER_LOG: &str = "CRAZYAGENT_AGENTS_OWNER_WRITER_LOG";

const OWNER_WRITER_MARK: &str = "CRAZYAGENT_AGENTS_OWNER_WRITER_MARK";

const SLEEPER: &str = "CRAZYAGENT_AGENTS_SLEEPER";

const WAIT: Duration = Duration::from_secs(30);

struct Case {
    root: tempfile::TempDir,
}

impl Case {
    fn new() -> Self {
        let root = tempfile::Builder::new()
            .prefix("ca-agents-")
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        install_fixture(root.path());
        Self { root }
    }

    fn path(&self) -> PathBuf {
        self.root.path().canonicalize().unwrap()
    }

    fn agents(&self) -> Agents {
        Agents::open(&self.path()).unwrap()
    }
}

/// Writes the fixture agent child as `agent_child.py` in `root`, executable by its owner,
/// and runs it once through the spawn gate, so macOS has assessed the fresh script before
/// any test bounds how long the child takes to start.
pub(crate) fn install_fixture(root: &Path) {
    let child = root.join("agent_child.py");
    std::fs::write(&child, CHILD).unwrap();
    std::fs::set_permissions(&child, std::fs::Permissions::from_mode(0o700)).unwrap();
    let first = crate::spawn_gate::first_run(
        std::process::Command::new(&child)
            .arg("first-exec")
            .current_dir(root),
    );
    assert_eq!(first.code(), Some(3));
}

fn private(path: &Path) -> PathBuf {
    std::fs::create_dir_all(path).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
    path.canonicalize().unwrap()
}

/// Builds a spec that runs the fixture child installed in `root` for project "controls",
/// under its own sandbox profile that lets it write only outside `/Users`.
/// Inputs: the root, a short unique name and the first task. Output: the spec.
pub(crate) fn spec(root: &Path, name: &str, prompt: Option<&str>) -> AgentSpec {
    let profile = root.join(format!("{name}.sb"));
    std::fs::write(
        &profile,
        format!(
            "(version 1)\n(allow default)\n(deny file-write* (subpath \"/Users\"))\n(allow file-write* (subpath \"{}\"))\n",
            root.display()
        ),
    )
    .unwrap();
    AgentSpec {
        project: ProjectId::new("controls").unwrap(),
        role: AgentRole::SubAgent,
        label: format!("Sub-agent {name}"),
        harness: "claude-code".into(),
        executable: root.join("agent_child.py"),
        working_dir: private(&root.join("work").join(name)),
        home: private(&root.join("homes").join(name)),
        account: AgentAccount::Endpoint {
            base_url: Some("http://127.0.0.1:9".into()),
            keyed: false,
        },
        model: None,
        tools: false,
        sandbox_profile: Some(profile),
        run_time_secs: 300,
        context: false,
        prompt: prompt.map(str::to_owned),
    }
}

pub(crate) fn until(what: &str, mut check: impl FnMut() -> bool) {
    let deadline = Instant::now() + WAIT;
    while !check() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(25));
    }
}

#[cfg(target_os = "macos")]
pub(crate) fn pids(root: &Path, name: &str) -> (i32, i32) {
    let path = root.join("work").join(name).join("pids.json");
    until("the child to write its pids", || path.is_file());
    let value: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    (
        value["pid"].as_i64().unwrap() as i32,
        value["grandchild"].as_i64().unwrap() as i32,
    )
}

#[cfg(target_os = "macos")]
pub(crate) fn alive(pid: i32) -> bool {
    rustix::process::test_kill_process(rustix::process::Pid::from_raw(pid).unwrap()).is_ok()
}

fn replied(agents: &Agents, id: &str, text: &str) -> bool {
    let view = agents.view(id).unwrap();
    view.last_text.as_deref() == Some(text) && !view.busy
}

#[test]
fn a_launch_without_its_sandbox_profile_is_refused_before_it_is_listed() {
    let case = Case::new();
    let root = case.path();
    let agents = case.agents();
    let mut bare = spec(&root, "bare", Some("echo bare"));
    bare.sandbox_profile = None;
    assert_eq!(
        agents.launch(bare, None).unwrap_err().0,
        "launch_sandbox_required"
    );
    assert!(agents.list(None, 0, LIST_MAX).unwrap().is_empty());
    assert!(!root.join("work").join("bare").join("pids.json").exists());
}

#[test]
#[ignore = "run only by the engine crash test as its crashing engine process"]
fn crashing_engine_process() {
    let Some(root) = std::env::var_os(CRASH_ROOT).map(PathBuf::from) else {
        return;
    };
    let agents = Agents::open(&root).unwrap();
    let mut ids = Vec::new();
    for name in ["kept", "killed", "stopped"] {
        let agent = agents
            .launch(spec(&root, name, Some(&format!("echo {name}"))), None)
            .unwrap();
        ids.push(agent.id.clone());
        until("the reply", || replied(&agents, &agent.id, name));
    }
    std::fs::write(root.join("ids.json"), serde_json::to_vec(&ids).unwrap()).unwrap();
    std::process::exit(0);
}

#[test]
#[ignore = "run only by the escaped holder tests as their crashing engine process"]
fn escaping_engine_process() {
    let Some(root) = std::env::var_os(CRASH_ROOT).map(PathBuf::from) else {
        return;
    };
    let prompt = std::env::var(CRASH_PROMPT).unwrap_or_else(|_| "escape".to_owned());
    let agents = Agents::open(&root).unwrap();
    let agent = agents
        .launch(spec(&root, "escaping", Some(&prompt)), None)
        .unwrap();
    until("the reply", || replied(&agents, &agent.id, "escaped"));
    std::fs::write(
        root.join("ids.json"),
        serde_json::to_vec(&[&agent.id]).unwrap(),
    )
    .unwrap();
    std::process::exit(0);
}

#[cfg(target_os = "macos")]
fn crash(root: &Path, test: &str, envs: &[(&str, &str)]) -> Vec<String> {
    let status = crate::spawn_gate::status(
        std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", test, "--ignored", "--test-threads", "1"])
            .env(CRASH_ROOT, root)
            .envs(envs.iter().copied())
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null()),
    )
    .unwrap();
    assert!(status.success());
    serde_json::from_slice(&std::fs::read(root.join("ids.json")).unwrap()).unwrap()
}

/// Crashes an engine whose one agent started a child in a new session that keeps the
/// agent's output log, then kills the agent's own group so only that child holds it.
/// Inputs: the case root and the fixture prompt that starts the child. Output: the agent
/// id and the escaped child's pid.
#[cfg(target_os = "macos")]
fn escaped_after_crash(root: &Path, prompt: &str) -> (String, i32) {
    let ids = crash(
        root,
        "agents::tests::escaping_engine_process",
        &[(CRASH_PROMPT, prompt)],
    );
    let (pid, grandchild) = pids(root, "escaping");
    let escaped = escaped(root, "escaping");
    rustix::process::kill_process_group(
        rustix::process::Pid::from_raw(pid).unwrap(),
        rustix::process::Signal::KILL,
    )
    .unwrap();
    until("the agent's own group to be gone", || {
        !alive(pid) && !alive(grandchild)
    });
    assert!(alive(escaped));
    (ids[0].clone(), escaped)
}

#[cfg(target_os = "macos")]
fn escaped(root: &Path, name: &str) -> i32 {
    let path = root.join("work").join(name).join("escaped.json");
    let value: serde_json::Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    value["pid"].as_i64().unwrap() as i32
}

#[cfg(target_os = "macos")]
fn gone_within(pid: i32, wait: Duration) -> bool {
    let deadline = Instant::now() + wait;
    while alive(pid) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(25));
    }
    !alive(pid)
}

#[cfg(target_os = "macos")]
fn end_escaped(pid: i32) {
    if let Some(pid) = rustix::process::Pid::from_raw(pid) {
        let _ = rustix::process::kill_process(pid, rustix::process::Signal::KILL);
    }
}

/// Starts a process of the owner's own, as `tee -a` or an editor is, that holds `log` open
/// for appending in a process group it leads and carries no lineage token, and waits until
/// it holds the log. Inputs: the log and a mark path the process writes once it does.
/// Output: the process.
#[cfg(target_os = "macos")]
fn owner_writer(log: &Path, mark: &Path) -> std::process::Child {
    let writer = crate::spawn_gate::spawn(
        std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "agents::tests::owner_writer_process",
                "--ignored",
                "--test-threads",
                "1",
            ])
            .env(OWNER_WRITER_LOG, log)
            .env(OWNER_WRITER_MARK, mark)
            .env_remove(crate::harnesses::leftovers::LINEAGE_VARIABLE)
            .process_group(0)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null()),
    )
    .unwrap();
    until("the owner's writer to hold the log", || mark.is_file());
    writer
}

#[test]
#[ignore = "run only by the log holder rows as the owner's own writer of an agent log"]
fn owner_writer_process() {
    let (Some(log), Some(mark)) = (
        std::env::var_os(OWNER_WRITER_LOG),
        std::env::var_os(OWNER_WRITER_MARK),
    ) else {
        return;
    };
    let _held = std::fs::OpenOptions::new().append(true).open(log).unwrap();
    std::fs::write(mark, b"").unwrap();
    std::thread::sleep(Duration::from_secs(120));
}

#[test]
#[ignore = "run only by the carrier row as a process that sleeps with the environment it got"]
fn sleeping_process() {
    if std::env::var_os(SLEEPER).is_some() {
        std::thread::sleep(Duration::from_secs(60));
    }
}

/// Tells whether a process still runs, a child of this test reaped once it has ended,
/// after `wait`. Inputs: the child and the wait. Output: true while it runs.
#[cfg(target_os = "macos")]
fn kept_for(child: &mut std::process::Child, wait: Duration) -> bool {
    let deadline = Instant::now() + wait;
    while Instant::now() < deadline {
        if child.try_wait().unwrap().is_some() {
            return false;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    child.try_wait().unwrap().is_none()
}

/// Whether a stop can tell `pid` is agent `id`'s: its environment, read as a stop reads
/// it on this host, shows the launch token the agent register keeps for the run. Inputs:
/// the case root, the agent id and the process. Output: the answer.
#[cfg(target_os = "macos")]
fn told(root: &Path, id: &str, pid: i32) -> bool {
    let stored: Option<String> = Connection::open(root.join("engine.db"))
        .unwrap()
        .query_row(
            "SELECT lineage FROM agent_runs WHERE id=?1",
            params![id],
            |row| row.get(0),
        )
        .unwrap();
    let token = crate::harnesses::leftovers::LineageToken::try_from(
        stored.expect("the run keeps its launch token"),
    )
    .unwrap();
    crate::harnesses::leftovers::carriers_among(&token, &[pid])
        .is_some_and(|found| found.contains(&pid))
}

/// Checks a stop against what it could tell about an escaped log holder: one it could tell
/// is the agent's has ended and the stop is clean; one it could not is still running, and
/// the stop ends Failed with `agent_stop_incomplete` and names it by its process id.
/// Inputs: the stopped view, the holder, whether the stop could tell and whether the
/// holder ended. Output: none, or a panic naming the broken rule.
#[cfg(target_os = "macos")]
fn ended_only_when_told(stopped: &AgentView, escaped: i32, told: bool, ended: bool) {
    if told {
        assert!(
            ended,
            "stop left an escaped child whose environment shows the launch token running: {}",
            stopped.status
        );
        assert_eq!(stopped.state, AgentState::Stopped);
        assert_eq!(stopped.reason, None);
        return;
    }
    assert!(
        !ended,
        "stop signalled a log holder it could not tell was the agent's: {}",
        stopped.status
    );
    assert_eq!(stopped.state, AgentState::Failed);
    assert_eq!(stopped.reason.as_deref(), Some("agent_stop_incomplete"));
    let id = escaped.to_string();
    assert!(
        stopped
            .status
            .split(|c: char| !c.is_ascii_digit())
            .any(|word| word == id),
        "{}",
        stopped.status
    );
}

/// macOS only: an agent launch needs Seatbelt, so every other system refuses it with
/// `launch_sandbox_unavailable`, which `harnesses::launch::tests` asserts in its own row.
#[cfg(target_os = "macos")]
#[test]
fn stopping_an_agent_never_ends_the_owners_own_writer_of_its_log() {
    let case = Case::new();
    let root = case.path();
    let agents = case.agents();
    let agent = agents
        .launch(spec(&root, "carrying", Some("escape-carrier")), None)
        .unwrap();
    until("the reply", || replied(&agents, &agent.id, "escaped"));
    let escaped = escaped(&root, "carrying");
    let told = told(&root, &agent.id, escaped);
    let log = agents.folder(&agent.id).join(launch::DURABLE_OUTPUT);
    let mut writer = owner_writer(&log, &root.join("writer.held"));

    let stopped = agents.stop(&agent.id).unwrap();

    let writer_kept = kept_for(&mut writer, Duration::from_secs(1));
    let escaped_ended = gone_within(escaped, Duration::from_secs(5));
    let _ = writer.kill();
    let _ = writer.wait();
    end_escaped(escaped);
    assert!(
        writer_kept,
        "stop killed the owner's own writer of the agent log"
    );
    ended_only_when_told(&stopped, escaped, told, escaped_ended);
}

/// macOS only: an agent launch needs Seatbelt, so every other system refuses it with
/// `launch_sandbox_unavailable`, which `harnesses::launch::tests` asserts in its own row.
#[cfg(target_os = "macos")]
#[test]
fn stopping_an_agent_ends_an_escaped_log_holder_only_when_it_can_tell_it_is_the_agents() {
    let case = Case::new();
    let root = case.path();
    let agents = case.agents();
    let agent = agents
        .launch(spec(&root, "leaving", Some("escape")), None)
        .unwrap();
    until("the reply", || replied(&agents, &agent.id, "escaped"));
    let (pid, grandchild) = pids(&root, "leaving");
    let escaped = escaped(&root, "leaving");
    assert!(alive(escaped));
    let told = told(&root, &agent.id, escaped);

    let stopped = agents.stop(&agent.id).unwrap();

    let escaped_ended = gone_within(escaped, Duration::from_secs(5));
    end_escaped(escaped);
    ended_only_when_told(&stopped, escaped, told, escaped_ended);
    until("the agent's own group to be gone", || {
        !alive(pid) && !alive(grandchild)
    });
}

/// macOS only: an agent launch needs Seatbelt, so every other system refuses it with
/// `launch_sandbox_unavailable`, which `harnesses::launch::tests` asserts in its own row.
#[cfg(target_os = "macos")]
#[test]
fn stopping_a_reattached_agent_ends_its_escaped_child_only_when_it_can_tell_it_is_the_agents() {
    let case = Case::new();
    let root = case.path();
    let (id, escaped) = escaped_after_crash(&root, "escape-carrier");
    let told = told(&root, &id, escaped);
    let agents = Agents::open(&root).unwrap();
    assert!(agents.view(&id).unwrap().reattached);

    let stopped = agents.stop(&id).unwrap();

    let escaped_ended = gone_within(escaped, Duration::from_secs(5));
    end_escaped(escaped);
    ended_only_when_told(&stopped, escaped, told, escaped_ended);
}

/// macOS only: an agent launch needs Seatbelt, so every other system refuses it with
/// `launch_sandbox_unavailable`, which `harnesses::launch::tests` asserts in its own row.
#[cfg(target_os = "macos")]
#[test]
fn stopping_a_reattached_agent_never_signals_a_foreign_group() {
    let case = Case::new();
    let root = case.path();
    let (id, escaped) = escaped_after_crash(&root, "escape");
    let mut foreign = crate::spawn_gate::spawn(
        std::process::Command::new("/bin/sleep")
            .arg("300")
            .process_group(0)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null()),
    )
    .unwrap();
    let recycled = Connection::open(root.join("engine.db"))
        .unwrap()
        .execute(
            "UPDATE agent_runs SET process_group=?2 WHERE id=?1",
            params![id, foreign.id()],
        )
        .unwrap();
    assert_eq!(recycled, 1);
    let told = told(&root, &id, escaped);
    let agents = Agents::open(&root).unwrap();
    assert!(agents.view(&id).unwrap().reattached);

    let stopped = agents.stop(&id).unwrap();

    let foreign_kept = kept_for(&mut foreign, Duration::from_secs(1));
    let escaped_ended = gone_within(escaped, Duration::from_secs(5));
    let _ = foreign.kill();
    let _ = foreign.wait();
    end_escaped(escaped);
    assert!(foreign_kept, "stop signalled a foreign process group");
    ended_only_when_told(&stopped, escaped, told, escaped_ended);
}

/// macOS only: an agent launch needs Seatbelt, so every other system refuses it with
/// `launch_sandbox_unavailable`, which `harnesses::launch::tests` asserts in its own row.
#[cfg(target_os = "macos")]
#[test]
fn a_reattached_agent_whose_log_was_replaced_is_not_signalled_and_not_called_stopped() {
    let case = Case::new();
    let root = case.path();
    let (id, escaped) = escaped_after_crash(&root, "escape");
    let agents = Agents::open(&root).unwrap();
    assert!(agents.view(&id).unwrap().reattached);
    let log = root.join("agents").join(&id).join(launch::DURABLE_OUTPUT);
    std::fs::rename(&log, log.with_extension("moved")).unwrap();
    std::fs::write(&log, b"").unwrap();

    let stopped = agents.stop(&id).unwrap();

    let escaped_kept = alive(escaped);
    end_escaped(escaped);
    assert!(
        escaped_kept,
        "stop signalled through a log path it could not verify"
    );
    assert_eq!(stopped.state, AgentState::Failed);
    assert_eq!(stopped.reason.as_deref(), Some("agent_stop_incomplete"));
}

#[cfg(target_os = "macos")]
#[test]
fn only_other_write_holders_are_read_from_lsof_and_a_listing_without_this_engine_is_refused() {
    let own = rustix::process::getpid().as_raw_nonzero().get();
    let listed = format!("p{own}\nf7\nar\np41\nf1\naw\nf2\nau\np42\nf3\nar\np43\nf4\nau\n");
    let writers: Vec<i32> = listed_writers(&listed)
        .unwrap()
        .into_iter()
        .map(|pid| pid.as_raw_nonzero().get())
        .collect();
    assert_eq!(writers, [41, 43]);
    assert!(listed_writers("p41\nf1\naw\n").is_none());
    assert!(listed_writers(&format!("p{own}\nf7\nar\nn/elsewhere\n")).is_none());
}

/// macOS only: an agent launch needs Seatbelt, so every other system refuses it with
/// `launch_sandbox_unavailable`, which `harnesses::launch::tests` asserts in its own row.
#[cfg(target_os = "macos")]
#[test]
fn after_an_engine_crash_live_agents_are_reattached_and_dead_ones_are_marked_closed() {
    let case = Case::new();
    let root = case.path();
    let ids = crash(&root, "agents::tests::crashing_engine_process", &[]);
    let (kept, killed, stopped) = (&ids[0], &ids[1], &ids[2]);
    let (kept_pid, _) = pids(&root, "kept");
    let (killed_pid, killed_grandchild) = pids(&root, "killed");
    let (stopped_pid, stopped_grandchild) = pids(&root, "stopped");
    assert!(alive(kept_pid) && alive(killed_pid) && alive(stopped_pid));
    rustix::process::kill_process_group(
        rustix::process::Pid::from_raw(killed_pid).unwrap(),
        rustix::process::Signal::KILL,
    )
    .unwrap();
    until("the killed group to be gone", || {
        !alive(killed_pid) && !alive(killed_grandchild)
    });

    let agents = Agents::open(&root).unwrap();

    let view = agents.view(kept).unwrap();
    assert_eq!(view.state, AgentState::Running);
    assert!(view.reattached && view.can_stop && view.can_restart && !view.can_interrupt);
    assert_eq!(
        view.status,
        "Running, reconnected after CrazyAgent restarted"
    );
    assert_eq!(view.last_text.as_deref(), Some("kept"));
    assert_eq!(
        agents.send(kept, "echo more").unwrap_err().0,
        "agent_reconnected_read_only"
    );
    let closed = agents.view(killed).unwrap();
    assert_eq!(closed.state, AgentState::Closed);
    assert_eq!(closed.status, "Stopped when CrazyAgent closed");
    assert!(closed.can_restart && !closed.can_stop);

    let ended = agents.stop(stopped).unwrap();
    assert_eq!(ended.state, AgentState::Stopped);
    until("the stopped group to be gone", || {
        !alive(stopped_pid) && !alive(stopped_grandchild)
    });

    rustix::process::kill_process_group(
        rustix::process::Pid::from_raw(kept_pid).unwrap(),
        rustix::process::Signal::KILL,
    )
    .unwrap();
    until("the reattached agent's exit to be noticed", || {
        agents.view(kept).unwrap().state == AgentState::Exited
    });

    std::fs::remove_file(root.join("work/killed/pids.json")).unwrap();
    let restarted = agents.restart(killed).unwrap();
    assert_eq!(
        (restarted.state, restarted.attempt),
        (AgentState::Running, 2)
    );
    until("the restarted reply", || replied(&agents, killed, "killed"));
    agents.stop(killed).unwrap();
}

#[cfg(target_os = "macos")]
fn serve(
    hosted: Hosted,
    request: LaunchRequest,
    prompts: Receiver<String>,
) -> Option<HostedRequest> {
    let mut session = Session::start(request).unwrap();
    hosted.started(session.process_group());
    let interrupt = hosted.interrupt_flag();
    let mut text = String::new();
    loop {
        if let Some(asked) = hosted.requested() {
            let _ = session.stop();
            hosted.finish(AgentState::Stopped, None);
            return Some(asked);
        }
        if let Ok(prompt) = prompts.try_recv() {
            interrupt.store(false, Ordering::Release);
            session.send(&prompt).unwrap();
            text.clear();
            hosted.set_busy(true);
        }
        if interrupt.swap(false, Ordering::AcqRel) {
            let _ = session.interrupt();
        }
        match session.next_event(POLL) {
            Ok(Some(LaunchEvent::Text { text: part })) => text.push_str(&part),
            Ok(Some(LaunchEvent::TurnFinished { .. })) => {
                hosted.text(&text);
                hosted.set_busy(false);
            }
            Ok(Some(LaunchEvent::Exited { .. })) | Err(_) => {
                hosted.finish(AgentState::Exited, None);
                return None;
            }
            Ok(_) => {}
        }
    }
}

/// Wiring test: tests/fixtures/agent_child.py stands in for Claude Code and invents its
/// replies, so this checks the supervisor's stop, process groups and listing only.
/// Real codex and Claude Code: agents_live.rs and the harness_launch.rs live tests.
/// macOS only: an agent launch needs Seatbelt, so every other system refuses it with
/// `launch_sandbox_unavailable`, which `harnesses::launch::tests` asserts in its own row.
#[cfg(target_os = "macos")]
#[test]
fn wiring_stopping_one_agent_ends_its_whole_process_group_and_leaves_the_others_running() {
    let case = Case::new();
    let root = case.path();
    let agents = case.agents();
    let first = agents
        .launch(spec(&root, "first", Some("echo one")), None)
        .unwrap();
    let second = agents
        .launch(spec(&root, "second", Some("echo two")), None)
        .unwrap();
    assert_eq!(first.state, AgentState::Running);
    assert!(first.can_stop);
    until("both replies", || {
        replied(&agents, &first.id, "one") && replied(&agents, &second.id, "two")
    });
    let (first_pid, first_grandchild) = pids(&root, "first");
    let (second_pid, second_grandchild) = pids(&root, "second");
    assert!(alive(first_grandchild) && alive(second_grandchild));

    let stopped = agents.stop(&first.id).unwrap();

    assert_eq!(stopped.state, AgentState::Stopped);
    assert_eq!(stopped.status, "Stopped");
    assert!(!stopped.can_stop && stopped.can_restart && !stopped.can_interrupt);
    until("the stopped group to be gone", || {
        !alive(first_pid) && !alive(first_grandchild)
    });
    assert!(alive(second_pid) && alive(second_grandchild));
    assert_eq!(agents.view(&second.id).unwrap().state, AgentState::Running);
    assert_eq!(agents.stop(&first.id).unwrap_err().0, "agent_not_running");
    let listed = agents
        .list(Some(&ProjectId::new("controls").unwrap()), 0, LIST_MAX)
        .unwrap();
    assert_eq!(listed.len(), 2);
    let page = agents
        .list(Some(&ProjectId::new("controls").unwrap()), 1, 1)
        .unwrap();
    assert_eq!(page.len(), 1);
    assert_eq!(page[0].id, listed[1].id);
    assert!(
        agents
            .list(Some(&ProjectId::new("other").unwrap()), 0, LIST_MAX)
            .unwrap()
            .is_empty()
    );
    agents.stop(&second.id).unwrap();
    until("the second group to be gone", || {
        !alive(second_pid) && !alive(second_grandchild)
    });
}

/// Wiring test: tests/fixtures/agent_child.py stands in for Claude Code and invents its
/// replies, including the turn end after an interrupt, so this checks only that the
/// owner's interrupt reaches one agent and its turn bookkeeping.
/// Real codex and Claude Code: agents_live.rs and the harness_launch.rs live tests.
/// macOS only: an agent launch needs Seatbelt, so every other system refuses it with
/// `launch_sandbox_unavailable`, which `harnesses::launch::tests` asserts in its own row.
#[cfg(target_os = "macos")]
#[test]
fn wiring_interrupt_ends_only_the_current_turn_of_one_agent() {
    let case = Case::new();
    let root = case.path();
    let agents = case.agents();
    let waiting = agents
        .launch(spec(&root, "waiting", Some("wait")), None)
        .unwrap();
    let other = agents
        .launch(spec(&root, "other", Some("wait")), None)
        .unwrap();
    pids(&root, "waiting");
    pids(&root, "other");

    until("the interrupt to be accepted", || {
        agents.interrupt(&waiting.id).is_ok()
    });

    until("the interrupted turn to end", || {
        !agents.view(&waiting.id).unwrap().busy
    });
    let view = agents.view(&waiting.id).unwrap();
    assert_eq!(view.state, AgentState::Running);
    assert!(!view.can_interrupt);
    assert!(agents.view(&other.id).unwrap().busy);
    assert_eq!(
        agents.interrupt(&waiting.id).unwrap_err().0,
        "agent_not_busy"
    );
    agents.send(&waiting.id, "echo again").unwrap();
    until("the next reply", || replied(&agents, &waiting.id, "again"));
    agents.stop(&waiting.id).unwrap();
    agents.stop(&other.id).unwrap();
}

/// Wiring test: tests/fixtures/agent_child.py stands in for Claude Code and invents its
/// replies, so this checks the supervisor's restart bookkeeping and fresh process only.
/// Real codex and Claude Code: agents_live.rs and the harness_launch.rs live tests.
/// macOS only: an agent launch needs Seatbelt, so every other system refuses it with
/// `launch_sandbox_unavailable`, which `harnesses::launch::tests` asserts in its own row.
#[cfg(target_os = "macos")]
#[test]
fn wiring_restart_keeps_the_agent_and_starts_a_fresh_process_with_its_first_task() {
    let case = Case::new();
    let root = case.path();
    let agents = case.agents();
    let agent = agents
        .launch(spec(&root, "again", Some("echo first task")), None)
        .unwrap();
    until("the first reply", || {
        replied(&agents, &agent.id, "first task")
    });
    let (old_pid, old_grandchild) = pids(&root, "again");
    std::fs::remove_file(root.join("work/again/pids.json")).unwrap();

    let restarted = agents.restart(&agent.id).unwrap();

    assert_eq!(restarted.id, agent.id);
    assert_eq!(restarted.attempt, 2);
    assert_eq!(restarted.state, AgentState::Running);
    until("the old group to be gone", || {
        !alive(old_pid) && !alive(old_grandchild)
    });
    let (new_pid, _) = pids(&root, "again");
    assert_ne!(new_pid, old_pid);
    until("the task to run again", || {
        replied(&agents, &agent.id, "first task")
    });
    agents.stop(&agent.id).unwrap();
    assert_eq!(agents.restart(&agent.id).unwrap().attempt, 3);
    agents.stop(&agent.id).unwrap();
    assert_eq!(
        agents.restart("not-an-agent").unwrap_err().0,
        "agent_not_found"
    );
}

/// macOS only: an agent launch needs Seatbelt, so every other system refuses it with
/// `launch_sandbox_unavailable`, which `harnesses::launch::tests` asserts in its own row.
#[cfg(target_os = "macos")]
#[test]
fn an_agent_that_ends_leaves_no_runtime_folder_behind() {
    let case = Case::new();
    let root = case.path();
    let agents = case.agents();
    let leaving = agents
        .launch(spec(&root, "gone", Some("exit 3")), None)
        .unwrap();
    let halted = agents
        .launch(spec(&root, "halted", Some("echo hi")), None)
        .unwrap();
    until("the reply", || replied(&agents, &halted.id, "hi"));
    assert!(root.join(AGENTS).join(&halted.id).is_dir());
    until("the exit to be recorded", || {
        agents.view(&leaving.id).unwrap().state == AgentState::Exited
    });

    agents.stop(&halted.id).unwrap();

    for id in [&leaving.id, &halted.id] {
        assert!(!root.join(AGENTS).join(id).exists(), "{id} kept its folder");
    }
    assert_eq!(
        agents.restart(&halted.id).unwrap().state,
        AgentState::Running
    );
    until("the restarted reply", || replied(&agents, &halted.id, "hi"));
    assert!(root.join(AGENTS).join(&halted.id).is_dir());
    agents.stop(&halted.id).unwrap();
    assert!(!root.join(AGENTS).join(&halted.id).exists());
}

/// macOS only: an agent launch needs Seatbelt, so every other system refuses it with
/// `launch_sandbox_unavailable`, which `harnesses::launch::tests` asserts in its own row.
#[cfg(target_os = "macos")]
#[test]
fn opening_the_register_removes_runtime_folders_of_agents_that_no_longer_run() {
    let case = Case::new();
    let root = case.path();
    drop(case.agents());
    let stale: Vec<PathBuf> = (0..1030_u32)
        .map(|n| root.join(AGENTS).join(format!("{n:032x}")))
        .collect();
    for folder in &stale {
        std::fs::create_dir(folder).unwrap();
        std::fs::write(folder.join(launch::DURABLE_OUTPUT), "old run").unwrap();
    }
    let foreign = root.join(AGENTS).join("not-an-agent");
    std::fs::create_dir(&foreign).unwrap();

    let agents = case.agents();

    let left: Vec<&PathBuf> = stale.iter().filter(|folder| folder.exists()).collect();
    assert!(left.is_empty(), "{} stale folders left", left.len());
    assert!(foreign.is_dir(), "only agent folders are pruned");
    let agent = agents
        .launch(spec(&root, "after", Some("echo hi")), None)
        .unwrap();
    until("the reply", || replied(&agents, &agent.id, "hi"));
    agents.stop(&agent.id).unwrap();
}

/// Wiring test: tests/fixtures/agent_child.py stands in for Claude Code and invents its
/// replies and exit, so this checks only how the supervisor records an exit.
/// Real codex and Claude Code: agents_live.rs and the harness_launch.rs live tests.
/// macOS only: an agent launch needs Seatbelt, so every other system refuses it with
/// `launch_sandbox_unavailable`, which `harnesses::launch::tests` asserts in its own row.
#[cfg(target_os = "macos")]
#[test]
fn wiring_an_agent_that_exits_by_itself_is_shown_as_exited_never_running() {
    let case = Case::new();
    let root = case.path();
    let agents = case.agents();
    let agent = agents
        .launch(spec(&root, "leaving", Some("exit 3")), None)
        .unwrap();
    let (_, grandchild) = pids(&root, "leaving");

    until("the exit to be recorded", || {
        agents.view(&agent.id).unwrap().state == AgentState::Exited
    });

    let view = agents.view(&agent.id).unwrap();
    assert_eq!(view.exit_code, Some(3));
    assert_eq!(view.status, "Exited with code 3");
    assert!(!view.can_stop && view.can_restart);
    until("the leftover grandchild to be stopped", || {
        !alive(grandchild)
    });
}

/// A Linux run whose call supervisor stopped because an entry read as a held name could
/// not be taken back shows that reason, not a bare exit code; any other code, 125 among
/// them, still shows the code (DEC-185 S2a, rv-sec3 F1). The stored row is built directly,
/// so nothing is launched.
#[cfg(target_os = "linux")]
#[test]
fn a_run_the_sandbox_stopped_for_a_held_name_shows_why() {
    let ended = |code: i32| {
        Stored {
            id: "held".into(),
            project: ProjectId::new("controls").unwrap(),
            role: AgentRole::SubAgent,
            label: "held".into(),
            harness: "claude".into(),
            state: AgentState::Exited,
            reattached: false,
            attempt: 1,
            exit_code: Some(code),
            reason: None,
            last_text: None,
            started_ms: 1,
            ended_ms: Some(2),
            hosted: false,
            stop_left: None,
        }
        .view(None, false)
        .status
    };
    let stopped = ended(crate::sandbox::HELD_NOT_PUT_BACK_EXIT);
    assert_eq!(stopped, crate::sandbox::HELD_NOT_PUT_BACK);
    assert_eq!(ended(125), "Exited with code 125");
    assert_eq!(ended(3), "Exited with code 3");
}

/// Wiring test: tests/fixtures/agent_child.py stands in for Claude Code and invents its
/// replies, so this checks the supervisor's close and next start only.
/// Real codex and Claude Code: agents_live.rs and the harness_launch.rs live tests.
/// macOS only: an agent launch needs Seatbelt, so every other system refuses it with
/// `launch_sandbox_unavailable`, which `harnesses::launch::tests` asserts in its own row.
#[cfg(target_os = "macos")]
#[test]
fn wiring_closing_the_engine_stops_its_agents_and_says_so_after_the_next_start() {
    let case = Case::new();
    let root = case.path();
    let agents = case.agents();
    let agent = agents
        .launch(spec(&root, "closing", Some("echo hi")), None)
        .unwrap();
    until("the reply", || replied(&agents, &agent.id, "hi"));
    let (pid, grandchild) = pids(&root, "closing");

    drop(agents);

    until("the closed group to be gone", || {
        !alive(pid) && !alive(grandchild)
    });
    let reopened = case.agents();
    let view = reopened.view(&agent.id).unwrap();
    assert_eq!(view.state, AgentState::Closed);
    assert_eq!(view.status, "Stopped when CrazyAgent closed");
    assert!(view.can_restart && !view.can_stop);
    assert_eq!(
        reopened.restart(&agent.id).unwrap().state,
        AgentState::Running
    );
    until("the restarted reply", || {
        replied(&reopened, &agent.id, "hi")
    });
    reopened.stop(&agent.id).unwrap();
}

/// Wiring test: tests/fixtures/agent_child.py stands in for Claude Code and invents its
/// replies, so this checks the supervisor's key handling across restarts only.
/// Real codex and Claude Code: agents_live.rs and the harness_launch.rs live tests.
/// macOS only: an agent launch needs Seatbelt, so every other system refuses it with
/// `launch_sandbox_unavailable`, which `harnesses::launch::tests` asserts in its own row.
#[cfg(target_os = "macos")]
#[test]
fn wiring_a_keyed_agent_cannot_restart_once_its_key_is_gone() {
    let case = Case::new();
    let root = case.path();
    let agents = case.agents();
    let mut keyed = spec(&root, "keyed", Some("echo keyed"));
    keyed.account = AgentAccount::Endpoint {
        base_url: Some("http://127.0.0.1:9".into()),
        keyed: true,
    };
    assert_eq!(
        agents.launch(keyed.clone(), None).unwrap_err().0,
        "agent_account_invalid"
    );
    let key = Secret::new("dummy-agent-key-7781".into()).unwrap();
    let agent = agents.launch(keyed, Some(key)).unwrap();
    until("the reply", || replied(&agents, &agent.id, "keyed"));
    assert_eq!(agents.restart(&agent.id).unwrap().attempt, 2);
    agents.stop(&agent.id).unwrap();
    assert_eq!(agents.restart(&agent.id).unwrap().attempt, 3);
    let encoded: String = agents
        .db()
        .unwrap()
        .query_row(
            "SELECT spec FROM agent_runs WHERE id=?1",
            [&agent.id],
            |row| row.get(0),
        )
        .unwrap();
    assert!(!encoded.contains("dummy-agent-key-7781"));
    drop(agents);

    let reopened = case.agents();
    assert_eq!(
        reopened.restart(&agent.id).unwrap_err().0,
        "agent_restart_needs_key"
    );
    assert_eq!(reopened.view(&agent.id).unwrap().state, AgentState::Closed);
}

/// Wiring test: tests/fixtures/agent_child.py stands in for Claude Code and invents its
/// replies, so this checks the supervisor's reattachment after an engine crash only.
/// Real codex and Claude Code: agents_live.rs and the harness_launch.rs live tests.
/// macOS only: an agent launch needs Seatbelt, so every other system refuses it with
/// `launch_sandbox_unavailable`, which `harnesses::launch::tests` asserts in its own row.
#[cfg(target_os = "macos")]
#[test]
fn wiring_after_an_engine_crash_live_agents_are_reattached_and_dead_ones_are_marked_closed() {
    let case = Case::new();
    let root = case.path();
    let status = crate::spawn_gate::status(
        std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "agents::tests::crashing_engine_process",
                "--ignored",
                "--test-threads",
                "1",
            ])
            .env(CRASH_ROOT, &root)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null()),
    )
    .unwrap();
    assert!(status.success());
    let ids: Vec<String> =
        serde_json::from_slice(&std::fs::read(root.join("ids.json")).unwrap()).unwrap();
    let (kept, killed, stopped) = (&ids[0], &ids[1], &ids[2]);
    let (kept_pid, _) = pids(&root, "kept");
    let (killed_pid, killed_grandchild) = pids(&root, "killed");
    let (stopped_pid, stopped_grandchild) = pids(&root, "stopped");
    assert!(alive(kept_pid) && alive(killed_pid) && alive(stopped_pid));
    rustix::process::kill_process_group(
        rustix::process::Pid::from_raw(killed_pid).unwrap(),
        rustix::process::Signal::KILL,
    )
    .unwrap();
    until("the killed group to be gone", || {
        !alive(killed_pid) && !alive(killed_grandchild)
    });

    let agents = Agents::open(&root).unwrap();

    let view = agents.view(kept).unwrap();
    assert_eq!(view.state, AgentState::Running);
    assert!(view.reattached && view.can_stop && view.can_restart && !view.can_interrupt);
    assert_eq!(
        view.status,
        "Running, reconnected after CrazyAgent restarted"
    );
    assert_eq!(view.last_text.as_deref(), Some("kept"));
    assert_eq!(
        agents.send(kept, "echo more").unwrap_err().0,
        "agent_reconnected_read_only"
    );
    let closed = agents.view(killed).unwrap();
    assert_eq!(closed.state, AgentState::Closed);
    assert_eq!(closed.status, "Stopped when CrazyAgent closed");
    assert!(closed.can_restart && !closed.can_stop);

    let ended = agents.stop(stopped).unwrap();
    assert_eq!(ended.state, AgentState::Stopped);
    until("the stopped group to be gone", || {
        !alive(stopped_pid) && !alive(stopped_grandchild)
    });

    rustix::process::kill_process_group(
        rustix::process::Pid::from_raw(kept_pid).unwrap(),
        rustix::process::Signal::KILL,
    )
    .unwrap();
    until("the reattached agent's exit to be noticed", || {
        agents.view(kept).unwrap().state == AgentState::Exited
    });

    std::fs::remove_file(root.join("work/killed/pids.json")).unwrap();
    let restarted = agents.restart(killed).unwrap();
    assert_eq!(
        (restarted.state, restarted.attempt),
        (AgentState::Running, 2)
    );
    until("the restarted reply", || replied(&agents, killed, "killed"));
    agents.stop(killed).unwrap();
}

/// Wiring test: tests/fixtures/agent_child.py stands in for Claude Code and invents its
/// replies, so this checks the hosted-agent controls of the supervisor only.
/// Real codex and Claude Code: agents_live.rs and the harness_launch.rs live tests.
/// macOS only: an agent launch needs Seatbelt, so every other system refuses it with
/// `launch_sandbox_unavailable`, which `harnesses::launch::tests` asserts in its own row.
#[cfg(target_os = "macos")]
#[test]
fn wiring_a_hosted_agent_is_listed_and_obeys_the_owners_stop_interrupt_and_restart() {
    let case = Case::new();
    let root = case.path();
    let agents = Arc::new(case.agents());
    let base = spec(&root, "hosted", None);
    let agent = HostedAgent {
        project: base.project.clone(),
        role: AgentRole::SubAgent,
        label: "Team sub-agent 7".into(),
        harness: "claude-code".into(),
        host: "team",
        reference: "7".into(),
        account: None,
    };
    let start = |hosted: Hosted| {
        let request = base.request(None).unwrap();
        let (prompts, inbox) = std::sync::mpsc::channel();
        let thread = std::thread::spawn(move || serve(hosted, request, inbox));
        (prompts, thread)
    };

    let hosted = agents.host(agent.clone(), None).unwrap();
    let id = hosted.id().to_owned();
    let (prompts, thread) = start(hosted);
    let (pid, grandchild) = pids(&root, "hosted");
    let listed = agents.list(Some(&base.project), 0, LIST_MAX).unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, id);
    assert_eq!(listed[0].label, "Team sub-agent 7");
    assert_eq!(listed[0].state, AgentState::Running);
    assert!(listed[0].can_stop && listed[0].can_restart && !listed[0].can_interrupt);
    assert_eq!(agents.interrupt(&id).unwrap_err().0, "agent_not_busy");
    assert_eq!(agents.send(&id, "echo no").unwrap_err().0, "agent_hosted");

    prompts.send("wait".into()).unwrap();
    until("the hosted turn to start", || {
        agents.view(&id).unwrap().busy
    });
    assert!(agents.view(&id).unwrap().can_interrupt);
    agents.interrupt(&id).unwrap();
    until("the interrupted turn to end", || {
        !agents.view(&id).unwrap().busy
    });
    prompts.send("echo after".into()).unwrap();
    until("the next reply", || replied(&agents, &id, "after"));
    assert!(alive(pid));

    let stopped = agents.stop(&id).unwrap();
    assert_eq!(stopped.state, AgentState::Stopped);
    assert!(!stopped.can_stop);
    assert_eq!(thread.join().unwrap(), Some(HostedRequest::Stop));
    until("the hosted group to be gone", || {
        !alive(pid) && !alive(grandchild)
    });

    assert_eq!(
        agents.restart(&id).unwrap_err().0,
        "agent_restart_unavailable"
    );
    assert!(agents.take_restarts("team").is_empty());
    assert!(agents.view(&id).unwrap().can_restart);
    assert_eq!(agents.restart(&id).unwrap().state, AgentState::Stopped);
    assert!(agents.take_restarts("research").is_empty());
    assert_eq!(
        agents.take_restarts("team"),
        vec![HostedRestart {
            id: id.clone(),
            host: "team".into(),
            reference: "7".into(),
        }]
    );

    std::fs::remove_file(root.join("work").join("hosted").join("pids.json")).unwrap();
    let again = agents.host(agent.clone(), Some(&id)).unwrap();
    assert_eq!(again.id(), id);
    let (_prompts, thread) = start(again);
    let (second, _) = pids(&root, "hosted");
    assert_ne!(second, pid);
    let view = agents.view(&id).unwrap();
    assert_eq!((view.state, view.attempt), (AgentState::Running, 2));
    assert_eq!(
        agents.restart(&id).unwrap().state,
        AgentState::Stopped,
        "a live hosted agent stops first"
    );
    assert_eq!(thread.join().unwrap(), Some(HostedRequest::Restart));
    assert_eq!(agents.take_restarts("team").len(), 1);
    until("the second process to be gone", || !alive(second));

    let third = agents.host(agent, Some(&id)).unwrap();
    assert_eq!(agents.view(&id).unwrap().attempt, 3);
    drop(third);
    let closed = agents.view(&id).unwrap();
    assert_eq!(closed.state, AgentState::Closed);
    assert_eq!(closed.status, "Stopped when CrazyAgent closed");
    assert!(closed.can_restart);
    drop(prompts);
}

/// The owner's stop and restart reach a hosted agent whose host ends it later. Until the
/// host has ended it the agent reads "Stopping" or "Restarting" and offers no second
/// stop; once ended it reads "Stopped".
#[test]
fn a_hosted_agent_reads_stopping_until_its_host_ends_it() {
    let case = Case::new();
    let agents = Arc::new(case.agents());
    let agent = HostedAgent {
        project: ProjectId::new("controls").unwrap(),
        role: AgentRole::SubAgent,
        label: "Team sub-agent 3".into(),
        harness: "codex".into(),
        host: "team",
        reference: "3".into(),
        account: None,
    };
    let ask = |action: fn(&Agents, &str) -> Result<AgentView, LaunchError>, id: &str| {
        let (agents, id) = (agents.clone(), id.to_owned());
        std::thread::spawn(move || action(&agents, &id))
    };

    let hosted = agents.host(agent.clone(), None).unwrap();
    let id = hosted.id().to_owned();
    let stop = ask(Agents::stop, &id);
    until("the stop to reach the host", || {
        hosted.requested() == Some(HostedRequest::Stop)
    });
    let stopping = agents.view(&id).unwrap();
    assert_eq!(
        (stopping.state, stopping.status.as_str()),
        (AgentState::Running, "Stopping")
    );
    assert!(!stopping.can_stop);
    hosted.finish(AgentState::Stopped, None);
    let stopped = stop.join().unwrap().unwrap();
    assert_eq!(
        (stopped.state, stopped.status.as_str()),
        (AgentState::Stopped, "Stopped")
    );
    assert!(!stopped.can_stop);

    let again = agents.host(agent, Some(&id)).unwrap();
    let restart = ask(Agents::restart, &id);
    until("the restart to reach the host", || {
        again.requested() == Some(HostedRequest::Restart)
    });
    let restarting = agents.view(&id).unwrap();
    assert_eq!(
        (restarting.state, restarting.status.as_str()),
        (AgentState::Running, "Restarting")
    );
    assert!(!restarting.can_stop);
    again.finish(AgentState::Stopped, None);
    assert_eq!(restart.join().unwrap().unwrap().state, AgentState::Stopped);
}

/// A refused sign-out names the agents that use the account: the running hosted agents
/// recorded with it, from every project, and none of another account's. Stopping them asks
/// each of their hosts at once and returns once all have ended.
#[test]
fn the_agents_using_an_account_are_named_and_stopped_together() {
    let case = Case::new();
    let agents = Arc::new(case.agents());
    let account = |name: &str| CredentialRef::new(name).unwrap();
    let agent = |project: &str, reference: &str, used: Option<CredentialRef>| HostedAgent {
        project: ProjectId::new(project).unwrap(),
        role: AgentRole::SubAgent,
        label: format!("Team sub-agent {reference}"),
        harness: "claude-code".into(),
        host: "team",
        reference: reference.into(),
        account: used,
    };
    let claude_c = account("claude-account-c");
    let alpha = agents
        .host(agent("alpha", "1", Some(claude_c.clone())), None)
        .unwrap();
    let beta = agents
        .host(agent("beta", "2", Some(claude_c.clone())), None)
        .unwrap();
    let other = agents
        .host(agent("alpha", "3", Some(account("claude-account-d"))), None)
        .unwrap();
    let keyless = agents.host(agent("alpha", "4", None), None).unwrap();

    let mut named: Vec<(String, String)> = agents
        .using(&claude_c)
        .unwrap()
        .into_iter()
        .map(|view| (view.project.as_str().to_owned(), view.label))
        .collect();
    named.sort();
    assert_eq!(
        named,
        vec![
            ("alpha".to_owned(), "Team sub-agent 1".to_owned()),
            ("beta".to_owned(), "Team sub-agent 2".to_owned())
        ]
    );
    assert!(
        agents
            .using(&account("claude-account-e"))
            .unwrap()
            .is_empty()
    );

    let stop = {
        let (agents, account) = (agents.clone(), claude_c.clone());
        std::thread::spawn(move || agents.stop_using(&account))
    };
    until("both stops to reach their hosts", || {
        alpha.requested() == Some(HostedRequest::Stop)
            && beta.requested() == Some(HostedRequest::Stop)
    });
    assert_eq!(other.requested(), None);
    assert_eq!(keyless.requested(), None);
    alpha.finish(AgentState::Stopped, None);
    beta.finish(AgentState::Stopped, None);
    let stopped = stop.join().unwrap().unwrap();
    assert_eq!(stopped.len(), 2);
    assert!(
        stopped
            .iter()
            .all(|view| view.state == AgentState::Stopped && !view.can_stop)
    );
    assert!(agents.using(&claude_c).unwrap().is_empty());
    assert_eq!(agents.using(&account("claude-account-d")).unwrap().len(), 1);
    drop((other, keyless));
}

/// A master turn or research job that holds an account's profile is named for that account
/// while its hold lives, newest first, and no longer once the hold is dropped.
#[test]
fn holders_of_an_account_are_named_until_their_hold_ends() {
    let case = Case::new();
    let agents = case.agents();
    let account = |name: &str| CredentialRef::new(name).unwrap();
    let claude_c = account("claude-account-c");
    let master = Holder::Master {
        project: ProjectId::new("alpha").unwrap(),
        session: SessionId::new("master-1").unwrap(),
    };
    let research = Holder::Research {
        project: ProjectId::new("beta").unwrap(),
        job: 7,
        title: "Market research".into(),
    };
    let held_master = agents.hold(claude_c.clone(), master.clone());
    let held_research = agents.hold(claude_c.clone(), research.clone());
    let elsewhere = agents.hold(account("claude-account-d"), research.clone());

    assert_eq!(
        agents.holders(&claude_c),
        vec![research.clone(), master.clone()]
    );
    assert_eq!(
        agents.holders(&account("claude-account-d")),
        vec![research.clone()]
    );
    assert!(agents.holders(&account("claude-account-e")).is_empty());

    drop(held_research);
    assert_eq!(agents.holders(&claude_c), vec![master]);
    drop(held_master);
    assert!(agents.holders(&claude_c).is_empty());
    assert_eq!(agents.holders(&account("claude-account-d")).len(), 1);
    drop(elsewhere);
    assert!(agents.holders(&account("claude-account-d")).is_empty());
}

#[test]
fn an_output_log_that_is_a_fifo_is_no_probe_and_is_not_waited_on() {
    use std::os::unix::fs::OpenOptionsExt;
    let folder = tempfile::tempdir().unwrap();
    let plain = folder.path().join("output.log");
    std::fs::write(&plain, "line\n").unwrap();
    assert!(log_probe(&plain).is_some());
    let fifo = folder.path().join("planted.log");
    let made = crate::spawn_gate::status(std::process::Command::new("/usr/bin/mkfifo").arg(&fifo))
        .unwrap();
    assert!(made.success());
    let (sent, probed) = std::sync::mpsc::channel();
    let path = fifo.clone();
    std::thread::spawn(move || {
        let _ = sent.send(log_probe(&path).is_some());
    });
    let answer = probed.recv_timeout(std::time::Duration::from_secs(5));
    let _writer = std::fs::OpenOptions::new()
        .write(true)
        .custom_flags(rustix::fs::OFlags::NONBLOCK.bits() as i32)
        .open(&fifo);
    assert_eq!(answer, Ok(false), "the probe waited on the FIFO");
}

/// A stop never signals process 1 or group 1 for a holder of an agent's output log:
/// `kill_process_group` sends `kill(-id)`, so group 1 would be every process the user may
/// signal. A holder that leads its group, or sits in the recorded group, gets its group; a
/// holder in the engine's group or another group gets itself alone; a gone one, nothing.
#[cfg(unix)]
#[test]
fn a_log_holder_stop_never_signals_process_or_group_one() {
    let pid = |raw: i32| rustix::process::Pid::from_raw(raw).unwrap();
    let own = pid(500);
    let cases = [
        (pid(1), Ok(pid(1)), 1, None),
        (pid(1), Ok(pid(1)), 4321, None),
        (pid(1234), Ok(pid(1)), 1, Some(Signalled::One(pid(1234)))),
        (
            pid(1234),
            Ok(pid(1234)),
            4321,
            Some(Signalled::Group(pid(1234))),
        ),
        (
            pid(1234),
            Ok(pid(777)),
            777,
            Some(Signalled::Group(pid(777))),
        ),
        (pid(1234), Ok(own), 500, Some(Signalled::One(pid(1234)))),
        (
            pid(1234),
            Ok(pid(888)),
            4321,
            Some(Signalled::One(pid(1234))),
        ),
        (pid(1234), Err(rustix::io::Errno::SRCH), 4321, None),
    ];
    for (writer, leader, group, wanted) in cases {
        assert_eq!(
            holder_target(writer, leader, own, group, true),
            wanted,
            "{writer:?} {leader:?} {group}"
        );
    }
}

/// A stop signals a holder of an agent's output log outside the agent's recorded group only
/// when the holder carries the launch's lineage token, so the owner's own `tee -a` or editor
/// on the log is never ended; a holder in the recorded group keeps its group.
#[cfg(unix)]
#[test]
fn a_log_holder_outside_the_agents_group_is_signalled_only_when_it_carries_the_launch_token() {
    let pid = |raw: i32| rustix::process::Pid::from_raw(raw).unwrap();
    let own = pid(500);
    let cases = [
        (
            pid(1234),
            Ok(pid(1234)),
            4321,
            true,
            Some(Signalled::Group(pid(1234))),
        ),
        (pid(1234), Ok(pid(1234)), 4321, false, None),
        (
            pid(1234),
            Ok(pid(777)),
            777,
            false,
            Some(Signalled::Group(pid(777))),
        ),
        (
            pid(1234),
            Ok(own),
            500,
            true,
            Some(Signalled::One(pid(1234))),
        ),
        (pid(1234), Ok(own), 500, false, None),
        (
            pid(1234),
            Ok(pid(888)),
            4321,
            true,
            Some(Signalled::One(pid(1234))),
        ),
        (pid(1234), Ok(pid(888)), 4321, false, None),
        (
            pid(1234),
            Err(rustix::io::Errno::PERM),
            4321,
            true,
            Some(Signalled::One(pid(1234))),
        ),
        (pid(1234), Err(rustix::io::Errno::PERM), 4321, false, None),
    ];
    for (writer, leader, group, carries, wanted) in cases {
        assert_eq!(
            holder_target(writer, leader, own, group, carries),
            wanted,
            "{writer:?} {leader:?} {group} {carries}"
        );
    }
}

/// A stop that leaves holders of an agent's output log running names each by id and
/// program, at most `LEFT_NAMED_MAX`, counts the rest, and logs the same names for the
/// agent.
#[cfg(unix)]
#[test]
fn a_stop_names_each_log_holder_it_left_by_id_and_program_and_counts_the_rest() {
    assert_eq!(left_named(&[]), "");
    assert_eq!(
        left_named(&[(1234, "sleep".to_owned()), (77, String::new())]),
        "1234 sleep, 77"
    );
    let many: Vec<(i32, String)> = (2..12).map(|pid| (pid, "tee".to_owned())).collect();
    assert_eq!(
        left_named(&many),
        "2 tee, 3 tee, 4 tee, 5 tee, 6 tee, 7 tee, 8 tee, 9 tee, and 2 more"
    );
    assert_eq!(
        left_line("ab", "1234 sleep"),
        "agent_stop_left agent=ab holders=1234 sleep"
    );
    assert_eq!(
        left_line("ab", ""),
        "agent_stop_left agent=ab holders=unlisted"
    );
}

/// The agent register as it was before runs kept their lineage token and the holders their
/// stop left, so the row can open a register made by an older engine.
#[cfg(unix)]
const SCHEMA_BEFORE_LINEAGE: &str = "CREATE TABLE agent_runs(
    id TEXT PRIMARY KEY CHECK(length(id) = 32),
    project TEXT NOT NULL,
    role TEXT NOT NULL CHECK(role IN ('master','sub_agent','research')),
    label TEXT NOT NULL,
    harness TEXT NOT NULL,
    spec TEXT NOT NULL,
    state TEXT NOT NULL CHECK(state IN ('running','stopped','exited','failed','closed')),
    reattached INTEGER NOT NULL CHECK(reattached IN (0,1)),
    process_group INTEGER CHECK(process_group > 1),
    attempt INTEGER NOT NULL CHECK(attempt >= 1),
    epoch TEXT NOT NULL,
    exit_code INTEGER,
    reason TEXT,
    last_text TEXT,
    started_ms INTEGER NOT NULL,
    ended_ms INTEGER
);";

#[cfg(unix)]
#[test]
fn an_agent_register_from_before_lineage_gets_the_lineage_and_stop_left_columns() {
    let case = Case::new();
    let root = case.path();
    Connection::open(root.join("engine.db"))
        .unwrap()
        .execute_batch(SCHEMA_BEFORE_LINEAGE)
        .unwrap();
    drop(case.agents());
    let columns: Vec<String> = Connection::open(root.join("engine.db"))
        .unwrap()
        .prepare("SELECT name FROM pragma_table_info('agent_runs')")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert!(
        columns.iter().any(|column| column == "lineage")
            && columns.iter().any(|column| column == "stop_left"),
        "{columns:?}"
    );
}

/// macOS only: an agent launch needs Seatbelt, so every other system refuses it with
/// `launch_sandbox_unavailable`, which `harnesses::launch::tests` asserts in its own row.
#[cfg(target_os = "macos")]
#[test]
fn a_launched_agent_keeps_its_launch_token_with_its_run() {
    let case = Case::new();
    let root = case.path();
    let agents = case.agents();
    let agent = agents
        .launch(spec(&root, "token", Some("echo token")), None)
        .unwrap();
    until("the reply", || replied(&agents, &agent.id, "token"));
    let kept: Option<String> = Connection::open(root.join("engine.db"))
        .unwrap()
        .query_row(
            "SELECT lineage FROM agent_runs WHERE id=?1",
            [&agent.id],
            |row| row.get(0),
        )
        .unwrap();
    let _ = agents.stop(&agent.id);
    assert!(
        kept.as_deref()
            .is_some_and(crate::harnesses::leftovers::LineageToken::valid),
        "{kept:?}"
    );
}

/// Starts this test binary as a process that sleeps, with the lineage token in its
/// environment or with the variable removed. Inputs: the token and whether it carries it.
/// Output: the process.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn sleeper(
    token: &crate::harnesses::leftovers::LineageToken,
    carries: bool,
) -> std::process::Child {
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "agents::tests::sleeping_process",
            "--ignored",
            "--test-threads",
            "1",
        ])
        .env(SLEEPER, "1")
        .env_remove(crate::harnesses::leftovers::LINEAGE_VARIABLE)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    if carries {
        command.env(
            crate::harnesses::leftovers::LINEAGE_VARIABLE,
            token.as_str(),
        );
    }
    crate::spawn_gate::spawn(&mut command).unwrap()
}

/// A stop's carriers are only processes started with the launch's token: one without it
/// never is, whatever this host shows of environments, and on Linux, which shows every
/// process of the user, both that carry it are, once each one's environment is set up after
/// its exec. Each process is named by its program for the stop's report.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn only_processes_whose_environment_shows_the_launch_token_are_carriers() {
    let token = crate::harnesses::leftovers::LineageToken::new().unwrap();
    let mut carrier = sleeper(&token, true);
    let mut plain = sleeper(&token, false);
    let mut platform = crate::spawn_gate::spawn(
        std::process::Command::new("/bin/sleep")
            .arg("60")
            .env(
                crate::harnesses::leftovers::LINEAGE_VARIABLE,
                token.as_str(),
            )
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null()),
    )
    .unwrap();
    let id = |child: &std::process::Child| i32::try_from(child.id()).unwrap();
    let asked = [id(&carrier), id(&plain), id(&platform)];
    let deadline = Instant::now() + WAIT;
    let mut found = Vec::new();
    let shown = |found: &[i32]| {
        found.contains(&asked[0]) && (cfg!(not(target_os = "linux")) || found.contains(&asked[2]))
    };
    while Instant::now() < deadline && !shown(&found) {
        found = crate::harnesses::leftovers::carriers_among(&token, &asked).unwrap_or_default();
        std::thread::sleep(Duration::from_millis(25));
    }
    let named = crate::harnesses::leftovers::programs_of(&asked[2..]);
    for child in [&mut carrier, &mut plain, &mut platform] {
        let _ = child.kill();
        let _ = child.wait();
    }
    assert!(!found.contains(&asked[1]), "{found:?}");
    assert!(found.iter().all(|pid| asked.contains(pid)), "{found:?}");
    if cfg!(target_os = "linux") {
        assert!(
            found.contains(&asked[0]) && found.contains(&asked[2]),
            "{found:?}"
        );
    }
    assert_eq!(named.get(&asked[2]).map(String::as_str), Some("sleep"));
}
