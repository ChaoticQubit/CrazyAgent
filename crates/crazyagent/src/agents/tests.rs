use super::*;

use std::os::unix::fs::PermissionsExt;

use std::os::unix::process::CommandExt;

const CHILD: &str = include_str!("../../tests/fixtures/agent_child.py");

const CRASH_ROOT: &str = "CRAZYAGENT_AGENTS_CRASH_ROOT";

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

/// Writes the fixture agent child as `agent_child.py` in `root`, executable by its owner.
pub(crate) fn install_fixture(root: &Path) {
    let child = root.join("agent_child.py");
    std::fs::write(&child, CHILD).unwrap();
    std::fs::set_permissions(&child, std::fs::Permissions::from_mode(0o700)).unwrap();
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

pub(crate) fn pids(root: &Path, name: &str) -> (i32, i32) {
    let path = root.join("work").join(name).join("pids.json");
    until("the child to write its pids", || path.is_file());
    let value: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    (
        value["pid"].as_i64().unwrap() as i32,
        value["grandchild"].as_i64().unwrap() as i32,
    )
}

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
    let agents = Agents::open(&root).unwrap();
    let agent = agents
        .launch(spec(&root, "escaping", Some("escape")), None)
        .unwrap();
    until("the reply", || replied(&agents, &agent.id, "escaped"));
    std::fs::write(
        root.join("ids.json"),
        serde_json::to_vec(&[&agent.id]).unwrap(),
    )
    .unwrap();
    std::process::exit(0);
}

fn crash(root: &Path, test: &str) -> Vec<String> {
    let status = crate::spawn_gate::status(
        std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", test, "--ignored", "--test-threads", "1"])
            .env(CRASH_ROOT, root)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null()),
    )
    .unwrap();
    assert!(status.success());
    serde_json::from_slice(&std::fs::read(root.join("ids.json")).unwrap()).unwrap()
}

/// Crashes an engine whose one agent started a child in a new session that keeps the
/// agent's output log, then kills the agent's own group so only that child holds it.
/// Input: the case root. Output: the agent id and the escaped child's pid.
fn escaped_after_crash(root: &Path) -> (String, i32) {
    let ids = crash(root, "agents::tests::escaping_engine_process");
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

fn escaped(root: &Path, name: &str) -> i32 {
    let path = root.join("work").join(name).join("escaped.json");
    let value: serde_json::Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    value["pid"].as_i64().unwrap() as i32
}

fn gone_within(pid: i32, wait: Duration) -> bool {
    let deadline = Instant::now() + wait;
    while alive(pid) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(25));
    }
    !alive(pid)
}

fn end_escaped(pid: i32) {
    if let Some(pid) = rustix::process::Pid::from_raw(pid) {
        let _ = rustix::process::kill_process(pid, rustix::process::Signal::KILL);
    }
}

#[test]
fn stopping_an_agent_also_ends_a_child_that_left_its_group_but_holds_its_log() {
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

    let stopped = agents.stop(&agent.id).unwrap();

    let escaped_ended = gone_within(escaped, Duration::from_secs(5));
    end_escaped(escaped);
    assert!(
        escaped_ended,
        "stop left the child that left the agent's group running"
    );
    assert_eq!(stopped.state, AgentState::Stopped);
    assert_eq!(stopped.reason, None);
    until("the agent's own group to be gone", || {
        !alive(pid) && !alive(grandchild)
    });
}

#[test]
fn stopping_a_reattached_agent_ends_what_holds_its_log_and_never_a_foreign_group() {
    let case = Case::new();
    let root = case.path();
    let (id, escaped) = escaped_after_crash(&root);
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
    let agents = Agents::open(&root).unwrap();
    assert!(agents.view(&id).unwrap().reattached);

    let stopped = agents.stop(&id).unwrap();

    let foreign_kept = foreign.try_wait().unwrap().is_none();
    let escaped_ended = gone_within(escaped, Duration::from_secs(5));
    let _ = foreign.kill();
    let _ = foreign.wait();
    end_escaped(escaped);
    assert!(foreign_kept, "stop signalled a foreign process group");
    assert!(
        escaped_ended,
        "stop left the child that holds the log running"
    );
    assert_eq!(stopped.state, AgentState::Stopped);
    assert_eq!(stopped.reason, None);
}

#[test]
fn a_reattached_agent_whose_log_was_replaced_is_not_signalled_and_not_called_stopped() {
    let case = Case::new();
    let root = case.path();
    let (id, escaped) = escaped_after_crash(&root);
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

#[test]
fn after_an_engine_crash_live_agents_are_reattached_and_dead_ones_are_marked_closed() {
    let case = Case::new();
    let root = case.path();
    let ids = crash(&root, "agents::tests::crashing_engine_process");
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

/// Wiring test: tests/fixtures/agent_child.py stands in for Claude Code and invents its
/// replies and exit, so this checks only how the supervisor records an exit.
/// Real codex and Claude Code: agents_live.rs and the harness_launch.rs live tests.
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

/// Wiring test: tests/fixtures/agent_child.py stands in for Claude Code and invents its
/// replies, so this checks the supervisor's close and next start only.
/// Real codex and Claude Code: agents_live.rs and the harness_launch.rs live tests.
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
