#![cfg(unix)]

use crazyagent::project_setup::{HarnessSlot, StoredSetup, TeamPlan};
use crazyagent::protocol::{Outcome, ProjectId, ProjectSpec, StoredProject};
use crazyagent::team::{
    AgentRole, AgentStatus, MessageAuthor, MessageState, TaskSource, TaskStatus, TeamAction,
    TeamStatus,
};
use crazyagent::terminal::FOLDER;
use crazyagent::terminal::tmux::{SOCKET, installed};
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

const PROJECT: &str = "project-team";
const ASKS_MAX: usize = 3;
const LEFT_RUNNING: &str = "Kept running while CrazyAgent was closed";
const LEFT_EXIT_TIME: Duration = Duration::from_secs(10);
const MODEL: &str = "qwen2.5:7b";
const OLLAMA: &str = "127.0.0.1:11434";
const OLLAMA_TIME: Duration = Duration::from_secs(10);
const OLLAMA_TAGS_BYTES_MAX: u64 = 1024 * 1024;

const CATALOG: &str = "version = 1\ntimeout_ms = 2000\nallowed_destinations = []\n\n[model_access]\naccounts = [\"local-account\"]\n\n[[model_access.connections]]\nid = \"local-connection\"\nprovider = \"openai-compatible\"\nharness = \"rcli\"\nprotocol = \"openai-chat\"\naccount_ref = \"local-account\"\nendpoint = { kind = \"http\", url = \"http://127.0.0.1:11434/v1\" }\nmodel = { id = \"qwen2.5:7b\", author = \"openai-compatible\" }\nauth = { method = \"none\" }\n\n[[model_access.pools]]\nid = \"local-pool\"\nmembers = [\"local-connection\"]\nprojects = [\"project-team\"]\nroles = [\"project_leader\"]\nfallback_allowed = false\n";

fn codex() -> PathBuf {
    let path = std::env::var_os("CRAZYAGENT_CODEX")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/opt/homebrew/bin/codex"))
        .canonicalize()
        .expect("real codex binary required");
    assert!(path.is_file(), "real codex binary required");
    path
}

/// The engine's private tmux server. Agent sessions outlive the engine, so dropping
/// this ends the server and every agent pane left in it, also when the test fails.
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

struct Engine {
    child: Child,
    stderr: Option<std::thread::JoinHandle<String>>,
}

impl Engine {
    fn start(runtime: &Path) -> Self {
        let mut child = crazyagent::spawn_gate::spawn(
            Command::new(env!("CARGO_BIN_EXE_crazyagent"))
                .arg("engine")
                .arg(runtime)
                .arg(runtime.join("global.toml"))
                .arg(runtime.join("project.toml"))
                .arg("engine.sock")
                .arg("--writable-settings")
                .arg("--codex-executable")
                .arg(codex())
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped()),
        )
        .unwrap();
        let stdout = child.stdout.take().unwrap();
        let (send, ready) = mpsc::sync_channel(1);
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            let mut line = String::new();
            let _ = reader.read_line(&mut line);
            let _ = send.send(line);
            let mut rest = Vec::new();
            let _ = reader.take(65536).read_to_end(&mut rest);
        });
        let stderr = child.stderr.take().unwrap();
        let stderr = std::thread::spawn(move || {
            let mut text = String::new();
            let _ = stderr.take(65536).read_to_string(&mut text);
            text
        });
        let line = ready
            .recv_timeout(Duration::from_secs(30))
            .unwrap_or_default();
        let mut engine = Self {
            child,
            stderr: Some(stderr),
        };
        if line != "ready\n" {
            let _ = engine.child.kill();
            let _ = engine.child.wait();
            panic!("engine not ready: {line:?} {}", engine.stderr_text());
        }
        engine
    }

    fn stderr_text(&mut self) -> String {
        self.stderr
            .take()
            .map(|handle| handle.join().unwrap_or_default())
            .unwrap_or_default()
    }

    fn stop(mut self) -> String {
        assert!(
            crazyagent::spawn_gate::status(Command::new("/bin/kill").args([
                "-s",
                "TERM",
                &self.child.id().to_string()
            ]))
            .unwrap()
            .success()
        );
        let deadline = Instant::now() + Duration::from_secs(40);
        let status = loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                break status;
            }
            assert!(Instant::now() < deadline, "engine did not stop");
            std::thread::sleep(Duration::from_millis(20));
        };
        let stderr = self.stderr_text();
        assert!(status.success(), "{stderr}");
        stderr
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

/// Fails the test with the command that fixes it unless the local Ollama serves the pool
/// model. Input: none; reads the model list from Ollama's `/api/tags`. Output: none.
fn require_model() {
    assert!(CATALOG.contains(&format!("model = {{ id = \"{MODEL}\"")));
    let listed = crazyagent::spawn_gate::connect_tcp_std(OLLAMA).and_then(|mut stream| {
        stream.set_read_timeout(Some(OLLAMA_TIME))?;
        stream.set_write_timeout(Some(OLLAMA_TIME))?;
        stream.write_all(
            format!("GET /api/tags HTTP/1.1\r\nHost: {OLLAMA}\r\nConnection: close\r\n\r\n")
                .as_bytes(),
        )?;
        let mut reply = String::new();
        stream
            .take(OLLAMA_TAGS_BYTES_MAX)
            .read_to_string(&mut reply)?;
        Ok(reply)
    });
    let reply = listed.unwrap_or_else(|problem| {
        panic!("the local Ollama at {OLLAMA} is required and did not answer: {problem}")
    });
    assert!(
        reply.contains(&format!("\"name\":\"{MODEL}\"")),
        "the local Ollama does not have {MODEL}; run: ollama pull {MODEL}"
    );
}

fn runtime() -> PathBuf {
    let checkout = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    assert!(
        checkout.join("Cargo.lock").is_file(),
        "{}",
        checkout.display()
    );
    let base = checkout.join(".local-state");
    fs::create_dir_all(&base).unwrap();
    let runtime = base.join(format!("t-team-{}", std::process::id()));
    if runtime.exists() {
        fs::remove_dir_all(&runtime).unwrap();
    }
    fs::create_dir(&runtime).unwrap();
    fs::set_permissions(&runtime, fs::Permissions::from_mode(0o700)).unwrap();
    for (name, text) in [("global.toml", CATALOG), ("project.toml", "")] {
        fs::write(runtime.join(name), text).unwrap();
        fs::set_permissions(runtime.join(name), fs::Permissions::from_mode(0o600)).unwrap();
    }
    runtime
}

fn setup(folder: &Path) -> StoredSetup {
    StoredSetup {
        project_id: ProjectId::new(PROJECT).unwrap(),
        name: "Fruit stand".into(),
        about: "A tiny fruit stand page.".into(),
        tech_stack: vec![],
        platforms: vec![],
        research: vec![],
        pool: None,
        team: TeamPlan {
            master: "codex".into(),
            master_tools: false,
            sub_harnesses: vec![HarnessSlot {
                harness: "codex".into(),
                agents: 2,
                tools: false,
            }],
            min_agents: 1,
            max_agents: 3,
        },
        documents: vec![],
        folder: folder.to_str().unwrap().into(),
        created_ms: 1,
        revision: 1,
    }
}

struct Owner {
    database: rusqlite::Connection,
    runtime: PathBuf,
    setup: StoredSetup,
}

impl Owner {
    fn act(&mut self, action: TeamAction) -> Outcome {
        let access = crazyagent::config::resolve_config(
            &self.runtime.join("global.toml"),
            &self.runtime.join("project.toml"),
        )
        .unwrap()
        .settings
        .model_access
        .unwrap();
        let tx = self.database.transaction().unwrap();
        let outcome = crazyagent::team::execute(&tx, &access, &action).unwrap();
        tx.commit().unwrap();
        outcome
    }

    fn status(&self) -> TeamStatus {
        crazyagent::team::status(&self.database, &self.setup).unwrap()
    }

    fn wait(&self, what: &str, minutes: u64, done: impl Fn(&TeamStatus) -> bool) -> TeamStatus {
        let deadline = Instant::now() + Duration::from_secs(minutes * 60);
        loop {
            let status = self.status();
            if done(&status) {
                return status;
            }
            assert!(Instant::now() < deadline, "{what}: {status:#?}");
            std::thread::sleep(Duration::from_millis(250));
        }
    }
}

fn group_alive(group: i64) -> bool {
    let pid = rustix::process::Pid::from_raw(i32::try_from(group).unwrap()).unwrap();
    rustix::process::test_kill_process_group(pid).is_ok()
}

#[test]
fn the_master_hands_two_tasks_to_sandboxed_sub_agents_and_hears_their_results() {
    require_model();
    let runtime = runtime();
    let server = installed(&runtime).map(|tmux| Server {
        tmux,
        socket: runtime.join(FOLDER).join(SOCKET),
    });
    let engine = Engine::start(&runtime);
    let folder = runtime.join("projects").join(PROJECT);
    fs::create_dir_all(folder.join("context")).unwrap();
    fs::write(
        folder.join("context").join("brief.md"),
        "# Fruit stand\n\nA one page site for a fruit stand.\n",
    )
    .unwrap();
    let folder = folder.canonicalize().unwrap();
    let database = rusqlite::Connection::open(runtime.join("engine.db")).unwrap();
    database.busy_timeout(Duration::from_secs(5)).unwrap();
    let setup = setup(&folder);
    let project_row = StoredProject {
        project_id: setup.project_id.clone(),
        spec: ProjectSpec {
            name: setup.name.clone(),
            repositories: vec![],
        },
    };
    database
        .execute(
            "INSERT INTO projects VALUES (?1, ?2)",
            rusqlite::params![PROJECT, serde_json::to_string(&project_row).unwrap()],
        )
        .unwrap();
    database
        .execute(
            "INSERT INTO project_setups(project, body) VALUES (?1, ?2)",
            rusqlite::params![PROJECT, serde_json::to_string(&setup).unwrap()],
        )
        .unwrap();
    let mut owner = Owner {
        database,
        runtime: runtime.clone(),
        setup,
    };
    let project = owner.setup.project_id.clone();
    for attempt in 1..=ASKS_MAX {
        assert_eq!(
            owner.act(TeamAction::Message {
                project_id: project.clone(),
                text: "Reply with exactly these two lines and nothing else:\nDELEGATE: Reply with the single word apple.\nDELEGATE: Reply with the single word pear.".into(),
            }),
            Outcome::Applied
        );
        let asked = owner.status().messages.last().unwrap().id;
        let team = owner.wait("the master answers", 8, |team| {
            team.messages
                .iter()
                .any(|m| m.id > asked && m.author == MessageAuthor::Master)
        });
        let handed = team
            .tasks
            .iter()
            .filter(|t| t.source == TaskSource::Master)
            .count();
        if handed >= 2 {
            break;
        }
        assert!(attempt < ASKS_MAX, "the master never delegated: {team:#?}");
    }

    let team = owner.wait("results reach the master", 20, |team| {
        let delivered = team.messages.iter().position(|m| {
            m.author == MessageAuthor::Engine
                && m.text.starts_with("Results from your sub-agents")
                && m.state == MessageState::Answered
        });
        delivered.is_some_and(|at| {
            team.messages[at..]
                .iter()
                .any(|m| m.author == MessageAuthor::Master && m.state == MessageState::Reply)
        })
    });
    let handed: Vec<_> = team
        .tasks
        .iter()
        .filter(|t| t.source == TaskSource::Master)
        .collect();
    assert!(handed.len() >= 2, "{team:#?}");
    let first_two = &handed[..2];
    for task in first_two {
        assert_eq!(task.status, TaskStatus::Done, "{task:#?}");
        assert!(
            task.result.as_deref().is_some_and(|r| !r.trim().is_empty()),
            "{task:#?}"
        );
        assert!(task.delivered);
        assert_eq!(task.harness, "codex");
    }
    let agents: Vec<_> = first_two
        .iter()
        .map(|task| {
            team.agents
                .iter()
                .find(|a| Some(a.id) == task.agent)
                .unwrap_or_else(|| panic!("task agent recorded: {team:#?}"))
        })
        .collect();
    assert_ne!(agents[0].id, agents[1].id, "two sub-agents ran: {team:#?}");
    for agent in &agents {
        assert_eq!(agent.role, AgentRole::Sub);
        assert_eq!(agent.account.as_deref(), Some("local-connection"));
        let worktree = agent.worktree.as_deref().unwrap();
        assert_eq!(worktree, format!("worktrees/agent-{}", agent.id));
        assert!(
            folder.join(worktree).join(".git").is_file(),
            "{worktree} is a git worktree"
        );
    }
    let master = team
        .agents
        .iter()
        .find(|a| a.role == AgentRole::Master && a.turns >= 2)
        .unwrap_or_else(|| panic!("the master took both turns: {team:#?}"));
    let worktree = master.worktree.as_deref().unwrap();
    assert_eq!(
        worktree,
        format!("worktrees/agent-{}", master.id),
        "the master works in its own worktree too"
    );
    assert!(folder.join(worktree).join(".git").is_file());

    assert_eq!(
        owner.act(TeamAction::Delegate {
            project_id: project.clone(),
            harness: None,
            prompt: "Write a long essay of at least three thousand words about rivers.".into(),
        }),
        Outcome::Applied
    );
    let owner_task_runs = |team: &TeamStatus| {
        team.tasks.iter().any(|t| {
            t.source == TaskSource::Owner
                && t.status == TaskStatus::Running
                && team
                    .agents
                    .iter()
                    .any(|a| Some(a.id) == t.agent && a.process_group.is_some() && a.status.live())
        })
    };
    let first = owner.wait("the owner task starts", 10, owner_task_runs);
    let held_task = first
        .tasks
        .iter()
        .find(|t| t.source == TaskSource::Owner)
        .unwrap()
        .clone();
    let held_agent = first
        .agents
        .iter()
        .find(|a| Some(a.id) == held_task.agent)
        .unwrap()
        .clone();
    let held_group = held_agent.process_group.unwrap();
    assert!(group_alive(held_group));
    assert_eq!(
        owner.act(TeamAction::Pause {
            project_id: project.clone(),
        }),
        Outcome::Applied
    );
    let paused = owner.wait("the paused team stops", 2, |team| {
        team.paused
            && team.agents.iter().all(|a| !a.status.live())
            && team
                .tasks
                .iter()
                .any(|t| t.id == held_task.id && t.status == TaskStatus::Queued)
    });
    assert!(
        !group_alive(held_group),
        "pause stopped the whole process group"
    );
    let waiting = paused.tasks.iter().find(|t| t.id == held_task.id).unwrap();
    assert_eq!(
        waiting.attempts,
        held_task.attempts - 1,
        "a pause does not spend the task's attempt"
    );
    assert_eq!(
        owner.act(TeamAction::Resume {
            project_id: project.clone(),
        }),
        Outcome::Applied
    );
    let running = owner.wait("the resumed task starts again", 10, owner_task_runs);
    assert!(!running.paused);
    let task = running
        .tasks
        .iter()
        .find(|t| t.source == TaskSource::Owner)
        .unwrap()
        .clone();
    let agent = running
        .agents
        .iter()
        .find(|a| Some(a.id) == task.agent)
        .unwrap()
        .clone();
    let group = agent.process_group.unwrap();
    assert!(group_alive(group));
    assert_eq!(
        owner.act(TeamAction::Cancel {
            project_id: project.clone(),
            task: Some(task.id),
        }),
        Outcome::Applied
    );
    let cancelled = owner.wait("the cancelled agent stops", 2, |team| {
        team.tasks
            .iter()
            .any(|t| t.id == task.id && t.status == TaskStatus::Cancelled)
            && team
                .agents
                .iter()
                .any(|a| a.id == agent.id && a.status == AgentStatus::Stopped)
    });
    assert!(
        !group_alive(group),
        "cancel stopped the whole process group"
    );
    let groups: Vec<(i64, i64)> = cancelled
        .agents
        .iter()
        .filter_map(|a| a.process_group.map(|group| (a.id, group)))
        .collect();

    let stderr = engine.stop();
    assert!(!stderr.contains("panicked"), "{stderr}");
    let after = owner.status();
    assert!(after.agents.iter().all(|a| !a.status.live()), "{after:#?}");
    for (id, group) in &groups {
        let agent = after.agents.iter().find(|a| a.id == *id).unwrap();
        assert_eq!(
            group_alive(*group),
            agent.problem.as_deref() == Some(LEFT_RUNNING),
            "only an agent left in its pane runs on after the engine stops: {agent:#?}"
        );
    }
    drop(server);
    let deadline = Instant::now() + LEFT_EXIT_TIME;
    while groups.iter().any(|(_, group)| group_alive(*group)) {
        assert!(
            Instant::now() < deadline,
            "ending the tmux server ended every agent left running"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    fs::remove_dir_all(&runtime).unwrap();
}
