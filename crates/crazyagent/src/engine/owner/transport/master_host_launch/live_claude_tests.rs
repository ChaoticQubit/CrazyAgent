use super::super::Root;
use super::*;
use crate::adapters::events::{EventKind, TurnOutcome};
use crate::adapters::{NativeAction, NativeHarness};
use crate::engine::claude_host::ClaudeHost;
use crate::protocol::SessionId;
use crate::routing::ResourceBudget;
use crate::sandbox::warden::fixture::{detached_survivors, naming, processes_naming};
use crate::team::runtime::managed_agent::SignedIn;
use std::os::unix::fs::PermissionsExt;

fn private_directory(path: &Path) {
    std::fs::create_dir_all(path).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
}

#[tokio::test]
#[ignore = "live qualification: needs CRAZYAGENT_MANAGED_CLAUDE_PROFILE"]
async fn a_real_claude_master_turn_is_signed_in_inside_the_master_sandbox() {
    let mut signed_in = SignedIn::claude();
    let connection = signed_in.connection.clone();
    let turn = RequestId::new("claude-live-master-turn").unwrap();
    let profile = signed_in.acquire(turn.as_str());
    let managed = profile.profile_path().unwrap();
    let temporary = tempfile::Builder::new()
        .prefix("ca-live-master-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    std::fs::set_permissions(temporary.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let base = temporary.path().canonicalize().unwrap();
    let root = Root::open(&base).unwrap();
    let repository = base.join("repository");
    let worktree = base.join("worktrees").join("lease");
    private_directory(&repository);
    private_directory(&worktree);
    let claude = crate::team::runtime::managed_agent::claude();
    let (runtime_ref, _, _) = allocate_master_capture(&root).unwrap();
    let folder = MasterRuntimeFolder::new(&root, &runtime_ref);
    let place = MasterTurnPlace {
        root: &root,
        runtime_ref: &runtime_ref,
        repository: &repository,
        worktree: &worktree,
    };
    let budget = ResourceBudget::unit();
    let launch = claude_master_turn(
        &place,
        &profile,
        &claude,
        &connection,
        "You are a test. Follow the user's instruction exactly.",
        Duration::from_millis(budget.wall_time_ms),
        None,
    )
    .unwrap();
    let MasterTurnLaunch {
        command,
        sandbox,
        home,
        credential,
        ..
    } = launch;
    let writable: Vec<PathBuf> = sandbox.sandbox().writable_roots().cloned().collect();
    let copy = home.join(".credentials.json");
    let copied_during_turn = copy.is_file();
    let mut master = ClaudeHost::spawn(
        command,
        sandbox,
        profile,
        home.clone(),
        NativeAction::Launch {
            session_id: SessionId::new("claude-live-master-session").unwrap(),
            harness: NativeHarness::Claude,
            profile_ref: signed_in.profile_ref(),
            worktree_lease: RequestId::new("claude-live-master-lease").unwrap(),
        },
        connection.account_ref.clone(),
        budget,
    )
    .await
    .unwrap();
    master
        .start_turn(
            turn.clone(),
            "Reply with exactly the word OK and nothing else.",
        )
        .await
        .unwrap();
    let mut errors = Vec::new();
    let mut text = String::new();
    let outcome = loop {
        let event = master.next_event().await.unwrap();
        match event.kind {
            EventKind::AssistantError { code } => errors.push(code),
            EventKind::AssistantText { text: part, .. } => text.push_str(&part),
            EventKind::TurnFinished { outcome, .. } => break outcome,
            _ => {}
        }
    };
    master.close().await.unwrap();
    println!("real claude master turn: outcome={outcome:?} errors={errors:?} reply={text:?}");
    assert_eq!(errors, Vec::<String>::new());
    assert_eq!(outcome, TurnOutcome::Success);
    assert!(text.contains("OK"), "{text}");
    assert!(copied_during_turn);
    assert!(home.starts_with(&base));
    assert!(
        writable
            .iter()
            .all(|root| !managed.starts_with(root) && !root.starts_with(&managed)),
        "{writable:?}"
    );
    drop(credential);
    assert!(!copy.exists());
    drop(folder);
    assert!(!home.exists());
}

/// The built engine executable that serves the `delegate` tool: `CRAZYAGENT_ENGINE`, or the
/// `crazyagent` binary beside this test's build folder. Output: its canonical path, or a
/// panic with the plain BLOCKED message when it was not built.
pub(super) fn engine_executable() -> PathBuf {
    std::env::var_os("CRAZYAGENT_ENGINE")
        .map(PathBuf::from)
        .or_else(|| {
            let deps = std::env::current_exe().ok()?;
            Some(deps.parent()?.parent()?.join("crazyagent"))
        })
        .and_then(|path| path.canonicalize().ok())
        .filter(|path| path.is_file())
        .unwrap_or_else(|| {
            panic!(
                "BLOCKED: the engine executable is not built; run cargo build -p crazyagent --bin crazyagent"
            )
        })
}

/// Qualifies a real Claude master turn with its tools on (DEC-093) inside the tools-on master
/// sandbox: it writes a file in its worktree with its own tools, starts the sub-agent it is
/// asked for in plain words through the engine's real `delegate` tool served by the built
/// engine executable, never through Claude Code's own sub-agent tools, and replies; the
/// engine then takes the recorded task. The turn is bounded by a 240 s wall time.
#[tokio::test]
#[ignore = "live qualification: needs CRAZYAGENT_MANAGED_CLAUDE_PROFILE"]
async fn a_real_claude_master_with_tools_on_works_in_its_worktree_and_delegates() {
    let engine = engine_executable();
    let mut signed_in = SignedIn::claude();
    let connection = signed_in.connection.clone();
    let turn = RequestId::new("claude-live-tools-turn").unwrap();
    let profile = signed_in.acquire(turn.as_str());
    let temporary = tempfile::Builder::new()
        .prefix("ca-live-tools-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    std::fs::set_permissions(temporary.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let base = temporary.path().canonicalize().unwrap();
    let root = Root::open(&base).unwrap();
    let repository = base.join("repository");
    let worktree = base.join("worktrees").join("lease");
    private_directory(&repository);
    private_directory(&worktree);
    let claude = crate::team::runtime::managed_agent::claude();
    let (runtime_ref, _, _) = allocate_master_capture(&root).unwrap();
    let folder = MasterRuntimeFolder::new(&root, &runtime_ref);
    let place = MasterTurnPlace {
        root: &root,
        runtime_ref: &runtime_ref,
        repository: &repository,
        worktree: &worktree,
    };
    let budget = ResourceBudget {
        wall_time_ms: 240_000,
        ..ResourceBudget::unit()
    };
    let MasterTurnLaunch {
        command,
        sandbox,
        home,
        credential,
        tool_record,
    } = claude_master_turn(
        &place,
        &profile,
        &claude,
        &connection,
        "You are a test. Follow the user's steps exactly and use your tools for them.",
        Duration::from_millis(budget.wall_time_ms),
        Some(&engine),
    )
    .unwrap();
    let record = tool_record.unwrap();
    crate::engine::claude_host::require_delegate(
        &command,
        Some(crate::engine::claude_host::MasterTools {
            engine: &engine,
            drop: &record,
        }),
    )
    .unwrap();
    let mut master = ClaudeHost::spawn(
        command,
        sandbox,
        profile,
        home,
        NativeAction::Launch {
            session_id: SessionId::new("claude-live-tools-session").unwrap(),
            harness: NativeHarness::Claude,
            profile_ref: signed_in.profile_ref(),
            worktree_lease: RequestId::new("claude-live-tools-lease").unwrap(),
        },
        connection.account_ref.clone(),
        budget,
    )
    .await
    .unwrap();
    master
        .start_turn(
            turn.clone(),
            "Do these steps in order. 1. With your file tools, create the file note.txt in your working folder containing exactly the word HERON. 2. Start one sub-agent that writes the README. Do not write the README yourself. 3. Reply with exactly the word DONE.",
        )
        .await
        .unwrap();
    let mut errors = Vec::new();
    let mut tools = Vec::new();
    let mut text = String::new();
    let outcome = loop {
        let event = master.next_event().await.unwrap();
        match event.kind {
            EventKind::AssistantError { code } => errors.push(code),
            EventKind::ToolUse {
                name,
                parent_tool_use_id: None,
                ..
            } => {
                tools.push(name);
                text.clear();
            }
            EventKind::AssistantText {
                text: part,
                parent_tool_use_id: None,
                ..
            } => text.push_str(&part),
            EventKind::TurnFinished { outcome, .. } => break outcome,
            _ => {}
        }
    };
    master.close().await.unwrap();
    let delegations = crate::team::tool::take(&record);
    let note = std::fs::read_to_string(worktree.join("note.txt")).unwrap_or_default();
    println!(
        "real claude tools-on master turn: outcome={outcome:?} errors={errors:?} tools={tools:?} delegations={delegations:?} note={note:?} reply={text:?}"
    );
    assert_eq!(errors, Vec::<String>::new());
    assert_eq!(outcome, TurnOutcome::Success);
    assert!(text.contains("DONE"), "{text}");
    assert_eq!(note.trim(), "HERON");
    assert!(
        tools
            .iter()
            .any(|name| name == "mcp__crazyagent-team__delegate"),
        "{tools:?}"
    );
    for built_in in ["Agent", "Task", "Workflow", "TeamCreate"] {
        assert!(!tools.iter().any(|name| name == built_in), "{tools:?}");
    }
    assert_eq!(delegations.len(), 1, "{delegations:?}");
    assert!(
        delegations[0].prompt.to_lowercase().contains("readme"),
        "{delegations:?}"
    );
    assert_eq!(std::fs::read_dir(&repository).unwrap().count(), 0);
    drop(credential);
    drop(folder);
}

/// Qualifies LB-44 on a real Claude master with its tools on: asked to start a background
/// shell command and reply without waiting, the master's Bash tool has no background option,
/// the turn ends at its result and the host closes cleanly. The turn is bounded by a 240 s
/// wall time.
#[tokio::test]
#[ignore = "live qualification: needs CRAZYAGENT_MANAGED_CLAUDE_PROFILE"]
async fn a_real_claude_master_asked_for_a_background_task_ends_its_turn_at_the_result() {
    let engine = engine_executable();
    let mut signed_in = SignedIn::claude();
    let connection = signed_in.connection.clone();
    let turn = RequestId::new("claude-live-background-turn").unwrap();
    let profile = signed_in.acquire(turn.as_str());
    let temporary = tempfile::Builder::new()
        .prefix("ca-live-background-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    std::fs::set_permissions(temporary.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let base = temporary.path().canonicalize().unwrap();
    let root = Root::open(&base).unwrap();
    let repository = base.join("repository");
    let worktree = base.join("worktrees").join("lease");
    private_directory(&repository);
    private_directory(&worktree);
    let claude = crate::team::runtime::managed_agent::claude();
    let (runtime_ref, _, _) = allocate_master_capture(&root).unwrap();
    let folder = MasterRuntimeFolder::new(&root, &runtime_ref);
    let place = MasterTurnPlace {
        root: &root,
        runtime_ref: &runtime_ref,
        repository: &repository,
        worktree: &worktree,
    };
    let budget = ResourceBudget {
        wall_time_ms: 240_000,
        ..ResourceBudget::unit()
    };
    let MasterTurnLaunch {
        command,
        sandbox,
        home,
        credential,
        tool_record: _,
    } = claude_master_turn(
        &place,
        &profile,
        &claude,
        &connection,
        "You are a test. Follow the user's steps exactly and use your tools for them.",
        Duration::from_millis(budget.wall_time_ms),
        Some(&engine),
    )
    .unwrap();
    let mut master = ClaudeHost::spawn(
        command,
        sandbox,
        profile,
        home,
        NativeAction::Launch {
            session_id: SessionId::new("claude-live-background-session").unwrap(),
            harness: NativeHarness::Claude,
            profile_ref: signed_in.profile_ref(),
            worktree_lease: RequestId::new("claude-live-background-lease").unwrap(),
        },
        connection.account_ref.clone(),
        budget,
    )
    .await
    .unwrap();
    master
        .start_turn(
            turn.clone(),
            "Do these steps in order. 1. Call your Bash tool once with run_in_background set to true and the command: sleep 5; echo LATE > late.txt 2. Do not wait for it and do not check on it. 3. Reply with exactly the word STARTED.",
        )
        .await
        .unwrap();
    let mut errors = Vec::new();
    let mut tools = Vec::new();
    let mut text = String::new();
    let outcome = loop {
        let event = master.next_event().await.unwrap();
        match event.kind {
            EventKind::AssistantError { code } => errors.push(code),
            EventKind::ToolUse {
                name,
                input,
                parent_tool_use_id: None,
                ..
            } => {
                tools.push((name, input.get("run_in_background").cloned()));
                text.clear();
            }
            EventKind::AssistantText {
                text: part,
                parent_tool_use_id: None,
                ..
            } => text.push_str(&part),
            EventKind::TurnFinished { outcome, .. } => break outcome,
            _ => {}
        }
    };
    let late_at_result = worktree.join("late.txt").exists();
    let closed = master.close().await.map_err(|error| error.0);
    let diagnostic = master.diagnostic().unwrap();
    println!(
        "real claude background master turn: outcome={outcome:?} closed={closed:?} errors={errors:?} tools={tools:?} late_at_result={late_at_result} late_lines={} late={:?} fault={:?} reply={text:?}",
        diagnostic.late_lines, diagnostic.late, diagnostic.fault
    );
    assert_eq!(errors, Vec::<String>::new());
    assert_eq!(outcome, TurnOutcome::Success);
    assert_eq!(closed, Ok(()));
    assert!(text.contains("STARTED"), "{text}");
    assert!(tools.iter().any(|(name, _)| name == "Bash"), "{tools:?}");
    assert!(
        tools
            .iter()
            .all(|(_, background)| background != &Some(serde_json::Value::Bool(true))),
        "{tools:?}"
    );
    drop(credential);
    drop(folder);
}

/// The command text the LB-46 live tests have the master's shells sleep with: a length no
/// other process of this user runs, so a process whose command names it is one of theirs.
const DETACHED_SLEEP: &str = "sleep 2917";

/// The LB-46 turn: one Bash call that leaves a `nohup` sleep and a sleep in its own session,
/// then a foreground sleep that is still running when the run is ended.
const DETACHED_TURN: &str = "Do these steps in order. 1. Call your Bash tool once with exactly this command: /usr/bin/nohup /bin/sleep 2917 > /dev/null 2>&1 & /usr/bin/perl -mPOSIX -e 'POSIX::setsid(); sleep 2917' > /dev/null 2>&1 & 2. Then call your Bash tool once with exactly this command: /bin/sleep 2917 3. Reply with exactly the word DONE.";

/// How long one LB-46 live test may run in all, and the wall time its master gets.
const DETACHED_LIVE_TIME: Duration = Duration::from_secs(180);

/// How long an LB-46 live test waits for the master's shells to start their sleeps.
const DETACHED_START_TIME: Duration = Duration::from_secs(140);

/// How long it still waits for the foreground sleep once both detached sleeps run.
const DETACHED_FOREGROUND_TIME: Duration = Duration::from_secs(20);

/// How long a master run's processes may outlive its end.
const DETACHED_GONE_TIME: Duration = Duration::from_secs(10);

/// Names the private folder of the engine the engine-death test starts and kills; set only
/// in that engine.
const DETACHED_ENGINE_BASE: &str = "CRAZYAGENT_LB46_ENGINE_BASE";

/// A real tools-on Claude master on the owner's signed-in profile, given the LB-46 turn,
/// with what must live as long as it: the turn's credential copy, its runtime folder, the
/// held profile and the owner root.
struct DetachingMaster {
    master: ClaudeHost,
    _credential: Option<crate::engine::bootstrap::AgentCredential>,
    _folder: MasterRuntimeFolder,
    _signed_in: SignedIn,
    _root: Root,
}

/// What the LB-46 turn's shells have started, read from this user's processes, with the
/// matching processes themselves.
#[derive(Debug)]
struct DetachedSeen {
    setsid: bool,
    sleeps: usize,
    found: Vec<(i32, String)>,
}

/// A private folder under `/tmp` for one live run. Input: its name prefix. Output: the
/// folder, removed with everything in it when dropped.
fn live_folder(prefix: &str) -> tempfile::TempDir {
    let temporary = tempfile::Builder::new()
        .prefix(prefix)
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    std::fs::set_permissions(temporary.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    temporary
}

/// Starts a real tools-on Claude master in `base` with the engine executable serving its
/// `delegate` tool, and gives it the LB-46 turn. Inputs: the private folder and a name for
/// the turn's references. Output: the master and what it needs alive.
async fn start_detaching_master(base: &Path, name: &str) -> DetachingMaster {
    let engine = engine_executable();
    let mut signed_in = SignedIn::claude();
    let connection = signed_in.connection.clone();
    let turn = RequestId::new(format!("claude-live-{name}-turn").as_str()).unwrap();
    let profile = signed_in.acquire(turn.as_str());
    let root = Root::open(base).unwrap();
    let repository = base.join("repository");
    let worktree = base.join("worktrees").join("lease");
    private_directory(&repository);
    private_directory(&worktree);
    let claude = crate::team::runtime::managed_agent::claude();
    let (runtime_ref, _, _) = allocate_master_capture(&root).unwrap();
    let folder = MasterRuntimeFolder::new(&root, &runtime_ref);
    let place = MasterTurnPlace {
        root: &root,
        runtime_ref: &runtime_ref,
        repository: &repository,
        worktree: &worktree,
    };
    let budget = ResourceBudget {
        wall_time_ms: u64::try_from(DETACHED_LIVE_TIME.as_millis()).unwrap(),
        ..ResourceBudget::unit()
    };
    let MasterTurnLaunch {
        command,
        sandbox,
        home,
        credential,
        tool_record: _,
    } = claude_master_turn(
        &place,
        &profile,
        &claude,
        &connection,
        "You are a test. Follow the user's steps exactly and use your tools for them.",
        DETACHED_LIVE_TIME,
        Some(&engine),
    )
    .unwrap();
    let mut master = ClaudeHost::spawn(
        command,
        sandbox,
        profile,
        home,
        NativeAction::Launch {
            session_id: SessionId::new(format!("claude-live-{name}-session").as_str()).unwrap(),
            harness: NativeHarness::Claude,
            profile_ref: signed_in.profile_ref(),
            worktree_lease: RequestId::new(format!("claude-live-{name}-lease").as_str()).unwrap(),
        },
        connection.account_ref.clone(),
        budget,
    )
    .await
    .unwrap();
    master.start_turn(turn, DETACHED_TURN).await.unwrap();
    DetachingMaster {
        master,
        _credential: credential,
        _folder: folder,
        _signed_in: signed_in,
        _root: root,
    }
}

/// Reads what the LB-46 turn's shells have started: the sleep in its own session, and the
/// plain sleeps, the `nohup` one and then the foreground one. Output: what runs now.
fn detached_seen() -> DetachedSeen {
    detached_in(processes_naming(DETACHED_SLEEP))
}

/// Reads the LB-46 turn's sleeps from the processes that name [`DETACHED_SLEEP`]. Input:
/// those processes. Output: what they show, keeping them.
fn detached_in(found: Vec<(i32, String)>) -> DetachedSeen {
    DetachedSeen {
        setsid: found
            .iter()
            .any(|(_, command)| command.contains("POSIX::setsid")),
        sleeps: found
            .iter()
            .filter(|(_, command)| command.ends_with(DETACHED_SLEEP) && !command.contains("perl"))
            .count(),
        found,
    }
}

/// Tells whether to end the run now: both detached sleeps run and the foreground sleep runs
/// too, or has not started within [`DETACHED_FOREGROUND_TIME`]. Inputs: what runs and when
/// both detached sleeps were first seen, set here. Output: true to end the run.
fn detached_ready(seen: &DetachedSeen, since: &mut Option<std::time::Instant>) -> bool {
    if !seen.setsid || seen.sleeps == 0 {
        return false;
    }
    let since = *since.get_or_insert_with(std::time::Instant::now);
    seen.sleeps >= 2 || since.elapsed() >= DETACHED_FOREGROUND_TIME
}

/// Qualifies LB-46 on a real Claude master with its tools on: its Bash tool leaves a `nohup`
/// sleep and a sleep in its own session and runs a foreground sleep, the engine cancels the
/// turn, and within 10 s no process of this user names the sleep or the run's folder.
/// Bounded by 180 s.
#[tokio::test]
#[ignore = "live qualification: needs CRAZYAGENT_MANAGED_CLAUDE_PROFILE"]
async fn a_real_claude_master_cancelled_mid_turn_leaves_no_detached_shell() {
    let temporary = live_folder("ca-live-detached-");
    let base = temporary.path().canonicalize().unwrap();
    let folder = base.to_str().unwrap().to_owned();
    let started = std::time::Instant::now();
    let run = tokio::time::timeout(DETACHED_LIVE_TIME, async {
        let mut run = start_detaching_master(&base, "detached").await;
        let mut tools = Vec::new();
        let mut since = None;
        let mut finished = None;
        let mut fault = None;
        let seen = loop {
            let seen = detached_seen();
            if detached_ready(&seen, &mut since)
                || finished.is_some()
                || fault.is_some()
                || started.elapsed() >= DETACHED_START_TIME
            {
                break seen;
            }
            match tokio::time::timeout(Duration::from_millis(250), run.master.next_event()).await {
                Ok(Ok(event)) => match event.kind {
                    EventKind::ToolUse {
                        name,
                        input,
                        parent_tool_use_id: None,
                        ..
                    } => tools.push((name, input.get("command").cloned())),
                    EventKind::TurnFinished { outcome, .. } => finished = Some(outcome),
                    _ => {}
                },
                Ok(Err(error)) => fault = Some(error.0),
                Err(_) => {}
            }
        };
        let ended_at = std::time::Instant::now();
        let ended = if finished.is_some() {
            run.master.close().await
        } else {
            run.master.cancel().await
        }
        .map_err(|error| error.0);
        let survivors =
            detached_survivors(ended_at + DETACHED_GONE_TIME, &[DETACHED_SLEEP, &folder]);
        let gone_after = ended_at.elapsed();
        (
            seen, tools, finished, fault, ended, survivors, gone_after, run,
        )
    })
    .await;
    let Ok((seen, tools, finished, fault, ended, survivors, gone_after, run)) = run else {
        let survivors = detached_survivors(std::time::Instant::now(), &[DETACHED_SLEEP, &folder]);
        panic!("the live run passed {DETACHED_LIVE_TIME:?}; survivors={survivors:?}");
    };
    println!(
        "real claude detached master turn: seen={seen:?} finished={finished:?} fault={fault:?} ended={ended:?} gone_after={gone_after:?} survivors={survivors:?} tools={tools:?}"
    );
    assert_eq!(fault, None);
    assert!(seen.setsid && seen.sleeps >= 1, "{seen:?} {tools:?}");
    assert_eq!(ended, Ok(()));
    assert_eq!(survivors, Vec::<(i32, String)>::new());
    drop(run);
}

/// Qualifies LB-46 engine death on a real Claude master with its tools on: the test runs
/// itself again as the engine, that engine's master gets the same turn, the test kills that
/// engine with SIGKILL while the sleeps run, and within 10 s no process of this user names
/// the sleep or the run's folder. Bounded by 180 s.
#[tokio::test]
#[ignore = "live qualification: needs CRAZYAGENT_MANAGED_CLAUDE_PROFILE"]
async fn a_real_claude_master_whose_engine_is_killed_mid_turn_leaves_no_detached_shell() {
    if let Some(base) = std::env::var_os(DETACHED_ENGINE_BASE) {
        let mut run = start_detaching_master(Path::new(&base), "engine-killed").await;
        let deadline = tokio::time::Instant::now() + DETACHED_LIVE_TIME;
        while tokio::time::Instant::now() < deadline {
            match tokio::time::timeout_at(deadline, run.master.next_event()).await {
                Ok(Ok(event)) => {
                    if let EventKind::ToolUse {
                        name,
                        input,
                        parent_tool_use_id: None,
                        ..
                    } = event.kind
                    {
                        println!("engine under test: tool {name} {:?}", input.get("command"));
                    }
                }
                Ok(Err(_)) | Err(_) => tokio::time::sleep_until(deadline).await,
            }
        }
        panic!("the engine under test was not killed within {DETACHED_LIVE_TIME:?}");
    }
    let temporary = live_folder("ca-live-engine-killed-");
    let base = temporary.path().canonicalize().unwrap();
    let folder = base.to_str().unwrap().to_owned();
    let module = module_path!().split_once("::").unwrap().1;
    let test = format!(
        "{module}::a_real_claude_master_whose_engine_is_killed_mid_turn_leaves_no_detached_shell"
    );
    let started = std::time::Instant::now();
    let mut engine = crate::spawn_gate::spawn(
        std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                test.as_str(),
                "--exact",
                "--ignored",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(DETACHED_ENGINE_BASE, &base)
            .stdin(std::process::Stdio::null()),
    )
    .unwrap();
    let mut since = None;
    let (seen, exited) = loop {
        let seen = detached_seen();
        let exited = engine.try_wait().unwrap();
        if detached_ready(&seen, &mut since)
            || exited.is_some()
            || started.elapsed() >= DETACHED_START_TIME
        {
            break (seen, exited);
        }
        std::thread::sleep(Duration::from_millis(250));
    };
    let killed_at = std::time::Instant::now();
    if exited.is_none() {
        engine.kill().unwrap();
        engine.wait().unwrap();
    }
    let survivors = detached_survivors(killed_at + DETACHED_GONE_TIME, &[DETACHED_SLEEP, &folder]);
    let gone_after = killed_at.elapsed();
    println!(
        "real claude master engine killed: seen={seen:?} exited_before_kill={exited:?} gone_after={gone_after:?} survivors={survivors:?} total={:?}",
        started.elapsed()
    );
    assert_eq!(exited, None);
    assert!(seen.setsid && seen.sleeps >= 1, "{seen:?}");
    assert_eq!(survivors, Vec::<(i32, String)>::new());
    assert!(started.elapsed() < DETACHED_LIVE_TIME);
}

#[test]
fn processes_naming_finds_a_running_process_of_this_user() {
    let mut sleeper = crate::spawn_gate::spawn(
        std::process::Command::new("/bin/sleep")
            .arg("3011")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null()),
    )
    .unwrap();
    let pid = i32::try_from(sleeper.id()).unwrap();
    let found = processes_naming("sleep 3011");
    sleeper.kill().unwrap();
    sleeper.wait().unwrap();
    assert_eq!(found, vec![(pid, "/bin/sleep 3011".to_owned())]);
}

#[test]
fn the_sleeps_of_both_live_runs_are_recognized_in_their_ps_lines() {
    let cancelled = "\
60764    -2 /usr/libexec/dhcp6d
98400   501 /bin/sleep 2917
98402   501 /usr/bin/perl -mPOSIX -e POSIX::setsid(); sleep 2917
98543   501 /bin/zsh -c source /private/tmp/ca-live-detached-ylXwej/master-runtime-c415c5e22df7b9b2a79ca3354e9edfc9b10fc8fd1ded4d11192d235602781320/home/shell-snapshots/snapshot-zsh-1793037087892-y5st5s.sh 2>/dev/null || true && setopt NO_EXTENDED_GLOB NO_BARE_GLOB_QUAL 2>/dev/null || true && { \\builtin unalias -- 'unsetenv'; \\builtin unset -f -- 'unsetenv'; } >/dev/null 2>&1 || true && eval '/bin/sleep 2917' < /dev/null && pwd -P >| /private/tmp/ca-live-detached-ylXwej/master-runtime-c415c5e22df7b9b2a79ca3354e9edfc9b10fc8fd1ded4d11192d235602781320/tmp/claude-db52-cwd
98545   501 /bin/sleep 2917
33508   501 /Applications/Brave Browser.app/Contents/MacOS/Brave Browser Helper (Renderer) --metrics-shmem-handle=1752395122,r,7286591663857117628
";
    let seen = detached_in(naming(cancelled, "501", 98000, DETACHED_SLEEP));
    assert!(seen.setsid);
    assert_eq!(seen.sleeps, 2);
    assert_eq!(
        seen.found.iter().map(|(pid, _)| *pid).collect::<Vec<_>>(),
        [98400, 98402, 98543, 98545]
    );
    assert!(detached_ready(&seen, &mut None));
    let killed = "\
 1256   501 /bin/sleep 2917
 1257   501 /usr/bin/perl -mPOSIX -e POSIX::setsid(); sleep 2917
 1320   501 /bin/sleep 2917
";
    let seen = detached_in(naming(killed, "501", 1000, DETACHED_SLEEP));
    assert!(seen.setsid);
    assert_eq!(seen.sleeps, 2);
    assert!(detached_ready(&seen, &mut None));
    let seen = detached_in(naming(killed, "502", 1000, DETACHED_SLEEP));
    assert!(!seen.setsid);
    assert_eq!(seen.sleeps, 0);
}
