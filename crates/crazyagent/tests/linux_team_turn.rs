#![cfg(target_os = "linux")]
use crazyagent::client_controller::OwnerController;
use crazyagent::engine::owner::OwnerClientKind;
use crazyagent::landing::{AcceptOutcome, ChangeSetView, ChangeState};
use crazyagent::project_setup::{HarnessSlot, StoredSetup, TeamPlan};
use crazyagent::protocol::{Outcome, ProjectId, ProjectSpec, StoredProject};
use crazyagent::team::{
    AgentRole, MessageAuthor, MessageState, TaskSource, TaskStatus, TeamAction, TeamStatus,
};
use std::fs;
use std::io::{BufRead, BufReader, Read};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

const PROJECT: &str = "project-linux-turn";
const WAIT: Duration = Duration::from_secs(120);
const READY_TIME: Duration = Duration::from_secs(60);
const ATTACH_TIME: Duration = Duration::from_secs(10);
const STDERR_BYTES_MAX: u64 = 262_144;
const PAGE: &str = "Written by the sub-agent.\n";

const CATALOG: &str = "version = 1\ntimeout_ms = 2000\nallowed_destinations = []\n\n[model_access]\naccounts = [\"local-account\"]\n\n[[model_access.connections]]\nid = \"local-connection\"\nprovider = \"anthropic-compatible\"\nharness = \"claude\"\nprotocol = \"anthropic-messages\"\naccount_ref = \"local-account\"\nendpoint = { kind = \"http\", url = \"http://127.0.0.1:11434\" }\nmodel = { id = \"qwen2.5:7b\", author = \"anthropic-compatible\" }\nauth = { method = \"none\" }\n\n[[model_access.pools]]\nid = \"local-pool\"\nmembers = [\"local-connection\"]\nprojects = [\"project-linux-turn\"]\nroles = [\"project_leader\"]\nfallback_allowed = false\n";

/// A headless Claude Code stand-in for one sub-agent turn. On its first turn, in its
/// worktree, it writes `page.md`, then tries the writes the call supervisor must refuse: an
/// append to the worktree admin folder's `HEAD`, a `config.worktree` beside it and a nested
/// `.git`. It answers with one line naming how each went and its seccomp mode.
const PROBING_CLAUDE: &str = r#"#!/bin/sh
case "$1" in --version) printf '2.1.309 (Claude Code)\n'; exit 0 ;; esac
judge() {
  if [ "$1" = 0 ]; then printf 'wrote'; return; fi
  case "$2" in
    *"Permission denied"*|*"Operation not permitted"*|*"Read-only file system"*) printf 'denied' ;;
    *"No such file"*) printf 'missing' ;;
    *) printf 'other' ;;
  esac
}
n=0
while IFS= read -r turn; do
  n=$((n+1))
  text="Done."
  if [ "$n" = 1 ]; then
    e=$( { printf 'Written by the sub-agent.\n' > page.md; } 2>&1 ); page=$(judge $? "$e")
    admin=$(/bin/sed -n 's/^gitdir: //p' .git)
    e=$( { printf 'x' >> "$admin/HEAD"; } 2>&1 ); head=$(judge $? "$e")
    e=$( { printf '[core]\n' > "$admin/config.worktree"; } 2>&1 ); worktree_config=$(judge $? "$e")
    e=$(/bin/mkdir -p nested/.git 2>&1); nested=$(judge $? "$e")
    seccomp=$(/bin/sed -n 's/^Seccomp:[[:space:]]*//p' /proc/self/status)
    text="PROBES page=$page head=$head config_worktree=$worktree_config nested_git=$nested seccomp=$seccomp"
  fi
  printf '{"type":"system","subtype":"init","uuid":"init-%s","session_id":"probe-session","model":"observed-model","capabilities":[]}\n' "$n"
  printf '{"type":"assistant","uuid":"step-%s","session_id":"probe-session","parent_tool_use_id":null,"message":{"id":"message-%s","type":"message","role":"assistant","model":"observed-model","content":[{"type":"text","text":"%s"}]}}\n' "$n" "$n" "$text"
  printf '{"type":"result","subtype":"success","uuid":"result-%s","session_id":"probe-session","is_error":false,"result":"%s"}\n' "$n" "$text"
done
"#;

/// The real `crazyagent engine --home <root>` process, the form the service starts, so it
/// records its engine root and runs Linux agents under the call supervisor.
struct Engine {
    child: Child,
    stderr: Option<std::thread::JoinHandle<String>>,
}

impl Engine {
    /// Starts the engine with `claude` as its Claude Code executable, `home` as HOME and the
    /// system PATH, and waits for its `ready` line. Inputs: the root, the home and the
    /// executable. Output: the running engine, or a panic with its stderr.
    fn start(root: &Path, home: &Path, claude: &Path) -> Self {
        let mut child = crazyagent::spawn_gate::spawn(
            Command::new(env!("CARGO_BIN_EXE_crazyagent"))
                .arg("engine")
                .arg("--home")
                .arg(root)
                .arg("--claude-executable")
                .arg(claude)
                .env_clear()
                .env("HOME", home)
                .env("PATH", "/usr/bin:/bin")
                .env("LANG", "C")
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
            let _ = std::io::copy(&mut reader, &mut std::io::sink());
        });
        let mut stderr = child.stderr.take().unwrap();
        let stderr = std::thread::spawn(move || {
            let mut kept = Vec::new();
            let _ = (&mut stderr).take(STDERR_BYTES_MAX).read_to_end(&mut kept);
            let _ = std::io::copy(&mut stderr, &mut std::io::sink());
            String::from_utf8_lossy(&kept).into_owned()
        });
        let line = ready.recv_timeout(READY_TIME).unwrap_or_default();
        let mut engine = Self {
            child,
            stderr: Some(stderr),
        };
        if line != "ready\n" {
            panic!("engine not ready: {line:?} {}", engine.stop());
        }
        engine
    }

    /// Kills the engine and returns what it wrote on stderr. Output: the text.
    fn stop(&mut self) -> String {
        let _ = self.child.kill();
        let _ = self.child.wait();
        self.stderr
            .take()
            .map(|handle| handle.join().unwrap_or_default())
            .unwrap_or_default()
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Runs the owner's git in `folder` with HOME set to `home`, no system config and the
/// system PATH, and checks it succeeded. Inputs: the folder, the home and the arguments.
/// Output: git's standard output and error, joined.
fn owner_git(folder: &Path, home: &Path, arguments: &[&str]) -> String {
    let output = crazyagent::spawn_gate::output(
        Command::new("git")
            .env_clear()
            .env("HOME", home)
            .env("PATH", "/usr/bin:/bin")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .arg("-C")
            .arg(folder)
            .args(arguments),
    )
    .unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.status.success(), "git {arguments:?}: {text}");
    text
}

/// The project's setup: one Claude Code sub-agent slot under a Claude Code master. Input:
/// the project folder. Output: the setup.
fn setup(folder: &Path) -> StoredSetup {
    StoredSetup {
        project_id: ProjectId::new(PROJECT).unwrap(),
        name: "Linux turn".into(),
        about: "One page written by a confined Linux sub-agent.".into(),
        tech_stack: vec![],
        platforms: vec![],
        research: vec![],
        pool: None,
        team: TeamPlan {
            master: "claude-code".into(),
            master_tools: false,
            sub_harnesses: vec![HarnessSlot {
                harness: "claude-code".into(),
                agents: 1,
                tools: false,
            }],
            min_agents: 1,
            max_agents: 2,
        },
        documents: vec![],
        folder: folder.to_str().unwrap().into(),
        created_ms: 1,
        revision: 1,
    }
}

/// Polls the team status until `done` holds. Inputs: the database, the setup, what is
/// awaited, the engine for its stderr on a timeout and the condition. Output: the status.
fn until(
    database: &rusqlite::Connection,
    setup: &StoredSetup,
    what: &str,
    engine: &mut Engine,
    done: impl Fn(&TeamStatus) -> bool,
) -> TeamStatus {
    let deadline = Instant::now() + WAIT;
    loop {
        let status = crazyagent::team::status(database, setup).unwrap();
        if done(&status) {
            return status;
        }
        if Instant::now() >= deadline {
            panic!("{what}: {status:#?}\nengine stderr:\n{}", engine.stop());
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

/// One engine and project for a row: a private root under /tmp holding the local pool as
/// global settings, an owner home with a git identity, the project checked in on `main`, the
/// engine started on it and the project's rows written. Fields are what the rows act on.
struct Rig {
    _temp: tempfile::TempDir,
    root: PathBuf,
    home: PathBuf,
    folder: PathBuf,
    engine: Engine,
    database: rusqlite::Connection,
    setup: StoredSetup,
    access: crazyagent::config::ModelAccess,
}

/// Starts the rig. Output: the rig, with the engine ready.
fn rig() -> Rig {
    let temp = tempfile::Builder::new()
        .prefix("ca-lt-")
        .tempdir_in("/tmp")
        .unwrap();
    let root = temp.path().canonicalize().unwrap().join("engine");
    let home = temp.path().canonicalize().unwrap().join("home");
    let bin = temp.path().canonicalize().unwrap().join("bin");
    for folder in [&root, &home, &bin] {
        fs::create_dir(folder).unwrap();
        fs::set_permissions(folder, fs::Permissions::from_mode(0o700)).unwrap();
    }
    fs::write(
        home.join(".gitconfig"),
        "[user]\n\tname = Owner\n\temail = owner@example.invalid\n",
    )
    .unwrap();
    for (name, text) in [("global.toml", CATALOG), ("project.toml", "")] {
        fs::write(root.join(name), text).unwrap();
        fs::set_permissions(root.join(name), fs::Permissions::from_mode(0o600)).unwrap();
    }
    let claude = bin.join("claude");
    fs::write(&claude, PROBING_CLAUDE).unwrap();
    fs::set_permissions(&claude, fs::Permissions::from_mode(0o700)).unwrap();
    let folder = root.join("projects").join(PROJECT);
    fs::create_dir_all(folder.join("context")).unwrap();
    fs::write(folder.join("context").join("brief.md"), "# Linux turn\n").unwrap();
    let folder = folder.canonicalize().unwrap();
    owner_git(&folder, &home, &["init", "-q", "-b", "main"]);
    owner_git(&folder, &home, &["add", "-A"]);
    owner_git(&folder, &home, &["commit", "-q", "-m", "Start the project"]);

    let engine = Engine::start(&root, &home, &claude);
    let database = rusqlite::Connection::open(root.join("engine.db")).unwrap();
    database.busy_timeout(Duration::from_secs(5)).unwrap();
    let setup = setup(&folder);
    let stored = StoredProject {
        project_id: setup.project_id.clone(),
        spec: ProjectSpec {
            name: setup.name.clone(),
            repositories: vec![],
        },
    };
    database
        .execute(
            "INSERT INTO projects VALUES (?1, ?2)",
            rusqlite::params![PROJECT, serde_json::to_string(&stored).unwrap()],
        )
        .unwrap();
    database
        .execute(
            "INSERT INTO project_setups(project, body) VALUES (?1, ?2)",
            rusqlite::params![PROJECT, serde_json::to_string(&setup).unwrap()],
        )
        .unwrap();
    let access =
        crazyagent::config::resolve_config(&root.join("global.toml"), &root.join("project.toml"))
            .unwrap()
            .settings
            .model_access
            .unwrap();
    Rig {
        _temp: temp,
        root,
        home,
        folder,
        engine,
        database,
        setup,
        access,
    }
}

/// Runs one owner action in its own immediate transaction. Inputs: the rig and the
/// action. Output: the outcome.
fn act(rig: &mut Rig, action: &TeamAction) -> Outcome {
    let tx = rig
        .database
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .unwrap();
    let outcome = crazyagent::team::execute(&tx, &rig.access, action).unwrap();
    tx.commit().unwrap();
    outcome
}

/// A Linux team sub-agent delegated by the owner runs in its linked worktree under the call
/// supervisor, headless, as the engine the service starts runs it (DEC-205 S5b): it writes
/// its page, each write into the worktree's git admin folder or a nested `.git` is refused
/// and leaves the owner's bytes as they were, its turn ends with a change set the owner
/// accepts onto the checked-out branch, and `git worktree list` and `prune` stay clean.
#[test]
fn a_supervised_linux_sub_agent_turn_lands_its_page_and_is_refused_every_git_metadata_write() {
    let mut rig = rig();
    let project = rig.setup.project_id.clone();
    let delegated = act(
        &mut rig,
        &TeamAction::Delegate {
            project_id: project.clone(),
            harness: Some("claude-code".into()),
            prompt: "Write page.md".into(),
        },
    );
    assert_eq!(delegated, Outcome::Applied);
    let Rig {
        _temp,
        root,
        home,
        folder,
        mut engine,
        database,
        setup,
        ..
    } = rig;

    let team = until(
        &database,
        &setup,
        "the owner's task ends",
        &mut engine,
        |team| {
            team.tasks.iter().any(|task| {
                task.source == TaskSource::Owner
                    && matches!(
                        task.status,
                        TaskStatus::Done | TaskStatus::Failed | TaskStatus::Cancelled
                    )
            })
        },
    );
    let task = team
        .tasks
        .iter()
        .find(|task| task.source == TaskSource::Owner)
        .unwrap();
    if task.status != TaskStatus::Done {
        panic!("{team:#?}\nengine stderr:\n{}", engine.stop());
    }
    let result = task.result.clone().unwrap_or_default();
    let agent = team
        .agents
        .iter()
        .find(|agent| Some(agent.id) == task.agent)
        .unwrap();
    assert!(
        agent.pane.is_none(),
        "this row pins the headless path; the engine ran the sub-agent in a pane: {team:#?}"
    );
    let worktree = folder.join(agent.worktree.as_deref().unwrap());
    for probe in [
        "page=wrote",
        "head=denied",
        "config_worktree=denied",
        "nested_git=denied",
    ] {
        assert!(result.contains(probe), "{probe} in {result:?}");
    }
    assert!(!result.contains("seccomp=0"), "{result:?}");
    assert_eq!(fs::read_to_string(worktree.join("page.md")).unwrap(), PAGE);
    let link = fs::read_to_string(worktree.join(".git")).unwrap();
    let admin = PathBuf::from(link.trim().strip_prefix("gitdir: ").unwrap());
    assert!(
        admin.starts_with(folder.join(".git").join("worktrees")),
        "{admin:?}"
    );
    let head = fs::read_to_string(admin.join("HEAD")).unwrap();
    let named = head.trim_end_matches('\n');
    assert!(
        head.ends_with('\n')
            && (named.starts_with("ref: refs/heads/")
                || (named.len() >= 40 && named.bytes().all(|byte| byte.is_ascii_hexdigit()))),
        "{head:?}"
    );
    match fs::read(admin.join("config.worktree")) {
        Ok(config) => assert_eq!(
            config,
            crazyagent::worktrees::REMOTE_LOCK,
            "only the engine's remote lock may sit in config.worktree"
        ),
        Err(error) => assert_eq!(error.kind(), std::io::ErrorKind::NotFound, "{error}"),
    }
    assert!(!worktree.join("nested").join(".git").exists());

    let landed = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let owner = OwnerController::attach(&root, OwnerClientKind::Tui, ATTACH_TIME)
                .await
                .unwrap();
            let deadline = Instant::now() + WAIT;
            let open: ChangeSetView = loop {
                let sets = owner.change_sets(setup.project_id.clone()).await.unwrap();
                if let Some(set) = sets
                    .into_iter()
                    .find(|set| set.state == ChangeState::Pending && set.files >= 1)
                {
                    break set;
                }
                assert!(Instant::now() < deadline, "no pending change set");
                tokio::time::sleep(Duration::from_millis(250)).await;
            };
            owner
                .accept_changes(open.id.clone(), open.head.clone(), None)
                .await
        });
    match landed {
        Ok(AcceptOutcome::Landed { .. }) => {}
        other => panic!(
            "Accept answered {other:?}\nengine stderr:\n{}",
            engine.stop()
        ),
    }
    assert_eq!(owner_git(&folder, &home, &["show", "HEAD:page.md"]), PAGE);
    let listed = owner_git(&folder, &home, &["worktree", "list", "--porcelain"]);
    for line in listed.lines() {
        if let Some(path) = line.strip_prefix("worktree ") {
            assert!(Path::new(path).is_dir(), "{listed}");
        }
        assert_ne!(line, "prunable", "{listed}");
    }
    let pruned = owner_git(&folder, &home, &["worktree", "prune", "--dry-run", "-v"]);
    assert!(pruned.trim().is_empty(), "{pruned}");
    let stderr = engine.stop();
    for refusal in ["linux_supervisor_not_started", "linux_helper_missing"] {
        assert!(!stderr.contains(refusal), "{stderr}");
    }
}

/// A Linux master whose live switch is on still answers the owner, headless, since agent
/// panes run only on macOS and the engine gives Linux team runtimes no terminal host
/// (DEC-205 S5b, QUESTION-1544): the owner's line gets the master's reply, the master runs
/// with no pane, and its live problem line is recorded for the row's report.
#[test]
fn a_linux_master_with_its_live_switch_on_still_answers_the_owner_headless() {
    let mut rig = rig();
    let project = rig.setup.project_id.clone();
    let tx = rig
        .database
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .unwrap();
    crazyagent::team::master::set_live(&tx, &project, true).unwrap();
    tx.commit().unwrap();
    let asked = act(
        &mut rig,
        &TeamAction::Message {
            project_id: project.clone(),
            text: "Say hello.".into(),
        },
    );
    assert_eq!(asked, Outcome::Applied);
    let Rig {
        _temp,
        mut engine,
        database,
        setup,
        ..
    } = rig;
    let team = until(
        &database,
        &setup,
        "the master answers the owner",
        &mut engine,
        |team| {
            team.messages.iter().any(|message| {
                message.author == MessageAuthor::Master && message.state == MessageState::Reply
            })
        },
    );
    let master = team
        .agents
        .iter()
        .find(|agent| agent.role == AgentRole::Master)
        .unwrap();
    assert!(master.pane.is_none(), "{team:#?}");
    println!("live problem: {:?}", team.live_problem);
    let stderr = engine.stop();
    assert!(
        stderr.contains("agent panes run only on macOS; Linux agents run headless"),
        "{stderr}"
    );
}
