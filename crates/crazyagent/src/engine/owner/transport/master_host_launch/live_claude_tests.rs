use super::super::Root;
use super::*;
use crate::adapters::events::{EventKind, TurnOutcome};
use crate::adapters::{NativeAction, NativeHarness};
use crate::engine::claude_host::ClaudeHost;
use crate::protocol::SessionId;
use crate::routing::ResourceBudget;
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
/// sandbox: it writes a file in its worktree with its own tools, calls the engine's real
/// `delegate` tool served by the built engine executable, and replies; the engine then takes
/// the recorded task. The turn is bounded by a 240 s wall time.
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
            "Do these steps in order. 1. With your file tools, create the file note.txt in your working folder containing exactly the word HERON. 2. Call the delegate tool once with the task \"Write the README\". 3. Reply with exactly the word DONE.",
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
    assert_eq!(delegations.len(), 1, "{delegations:?}");
    assert_eq!(delegations[0].prompt, "Write the README");
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
