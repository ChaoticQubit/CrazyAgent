#![cfg(target_os = "macos")]

use crazyagent::agents::{AgentAccount, AgentRole, AgentSpec, AgentState, AgentView, Agents};
use crazyagent::harnesses::{HarnessStatus, Search, catalog};
use crazyagent::protocol::ProjectId;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const OLLAMA_OPENAI: &str = "http://127.0.0.1:11434/v1";
const OLLAMA_ANTHROPIC: &str = "http://127.0.0.1:11434";
const LOCAL_MODEL: &str = "qwen2.5:0.5b";
const GREETING: &str = "Reply with one short greeting.";
const LONG_TURN: &str =
    "Write the numbers from 1 to 3000 in words, one per line, with no other text.";
const TURN: Duration = Duration::from_secs(400);

fn private_dir(prefix: &str) -> tempfile::TempDir {
    let directory = tempfile::Builder::new()
        .prefix(prefix)
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    directory
}

fn private(path: &Path) -> PathBuf {
    std::fs::create_dir_all(path).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
    path.canonicalize().unwrap()
}

fn installed(id: &str) -> PathBuf {
    let entries = catalog(&Search::from_environment());
    let entry = entries.iter().find(|entry| entry.id == id).unwrap();
    assert_eq!(
        entry.status,
        HarnessStatus::Ready,
        "UNAVAILABLE: {id} must be installed for live agent checks"
    );
    std::fs::canonicalize(entry.executable.as_ref().unwrap()).unwrap()
}

fn profile(base: &Path, work: &Path, home: &Path) -> PathBuf {
    let profile = base.join("agent.sb");
    let rules = format!(
        "(version 1)\n(allow default)\n(deny file-write* (subpath \"/Users\"))\n(allow file-write* (subpath \"{}\") (subpath \"{}\"))\n",
        work.display(),
        home.display()
    );
    std::fs::write(&profile, rules).unwrap();
    profile
}

fn until(agents: &Agents, id: &str, what: &str, done: impl Fn(&AgentView) -> bool) -> AgentView {
    let deadline = Instant::now() + TURN;
    loop {
        let view = agents.view(id).unwrap();
        if done(&view) {
            return view;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {what}: {view:?}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn replied(view: &AgentView) -> bool {
    !view.busy
        && view
            .last_text
            .as_deref()
            .is_some_and(|text| !text.trim().is_empty())
}

/// Real-harness counterpart of the agent supervisor's wiring tests: real codex and Claude
/// Code, sandboxed and pointed at the local Ollama model, are launched through `Agents`,
/// reply to their first task, take an owner interrupt mid-turn and answer again, then
/// stop. That the harness reports the turn as interrupted is checked at the session level
/// by harness_launch.rs live_interrupt_stops_a_running_turn_through_the_harness_protocol.
#[test]
fn live_codex_and_claude_agents_reply_take_an_interrupt_and_stop_through_the_supervisor() {
    for (harness, base_url) in [("codex", OLLAMA_OPENAI), ("claude-code", OLLAMA_ANTHROPIC)] {
        let root = private_dir("ca-agents-live-");
        let base = root.path().canonicalize().unwrap();
        let work = private(&base.join("work"));
        let home = private(&base.join("home"));
        let agents = Agents::open(&private(&base.join("engine"))).unwrap();
        let spec = AgentSpec {
            project: ProjectId::new("live").unwrap(),
            role: AgentRole::SubAgent,
            label: format!("Live {harness}"),
            harness: harness.into(),
            executable: installed(harness),
            working_dir: work.clone(),
            home: home.clone(),
            account: AgentAccount::Endpoint {
                base_url: Some(base_url.into()),
                keyed: false,
            },
            model: Some(LOCAL_MODEL.into()),
            tools: false,
            sandbox_profile: Some(profile(&base, &work, &home)),
            run_time_secs: 900,
            context: false,
            prompt: Some(GREETING.into()),
        };

        let agent = agents.launch(spec, None).unwrap();
        assert_eq!(agent.state, AgentState::Running, "{harness}");
        let first = until(&agents, &agent.id, "the first reply", replied);
        assert_eq!(first.state, AgentState::Running, "{harness}: {first:?}");

        agents.send(&agent.id, LONG_TURN).unwrap();
        let busy = agents.view(&agent.id).unwrap();
        assert!(busy.busy && busy.can_interrupt, "{harness}: {busy:?}");
        agents
            .interrupt(&agent.id)
            .unwrap_or_else(|error| panic!("{harness}: {} {:?}", error.0, agents.view(&agent.id)));
        let ended = until(&agents, &agent.id, "the interrupted turn to end", |view| {
            !view.busy
        });
        assert_eq!(ended.state, AgentState::Running, "{harness}: {ended:?}");
        assert_eq!(
            agents.interrupt(&agent.id).unwrap_err().0,
            "agent_not_busy",
            "{harness}"
        );

        agents.send(&agent.id, GREETING).unwrap();
        assert!(agents.view(&agent.id).unwrap().busy, "{harness}");
        let again = until(&agents, &agent.id, "the reply after the interrupt", replied);
        assert_eq!(again.state, AgentState::Running, "{harness}: {again:?}");

        let stopped = agents.stop(&agent.id).unwrap();
        assert_eq!(stopped.state, AgentState::Stopped, "{harness}");
        assert!(!stopped.can_stop && stopped.can_restart, "{harness}");
    }
}
