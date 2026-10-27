use super::super::Root;
use super::*;
use crate::adapters::events::{EventKind, TurnOutcome};
use crate::adapters::{NativeAction, NativeHarness};
use crate::engine::codex_host::CodexHost;
use crate::protocol::SessionId;
use crate::routing::ResourceBudget;
use crate::team::runtime::managed_agent::SignedIn;
use std::os::unix::fs::PermissionsExt;

fn private_directory(path: &Path) {
    std::fs::create_dir_all(path).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
}

#[tokio::test]
#[ignore = "live qualification: needs CRAZYAGENT_MANAGED_CODEX_PROFILE"]
async fn a_real_codex_master_turn_is_signed_in_inside_the_master_sandbox() {
    let mut signed_in = SignedIn::codex();
    let connection = signed_in.connection.clone();
    let turn = RequestId::new("codex-live-master-turn").unwrap();
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
    let codex = crate::team::runtime::managed_agent::codex();
    let (runtime_ref, _, _) = allocate_master_capture(&root).unwrap();
    let folder = MasterRuntimeFolder::new(&root, &runtime_ref);
    let command = master_command(&codex, &managed, &worktree, &connection.model.id, None).unwrap();
    let sandbox = master_sandbox(
        &root,
        &runtime_ref,
        &repository,
        &worktree,
        &managed,
        &codex,
        &connection.endpoint,
    )
    .unwrap();
    let mut master = CodexHost::spawn(
        command,
        sandbox,
        profile,
        managed.clone(),
        NativeAction::Launch {
            session_id: SessionId::new("codex-live-master-session").unwrap(),
            harness: NativeHarness::Codex,
            profile_ref: signed_in.profile_ref(),
            worktree_lease: RequestId::new("codex-live-master-lease").unwrap(),
        },
        connection.account_ref.clone(),
        ResourceBudget::unit(),
        "You are a test. Follow the user's instruction exactly.",
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
            EventKind::AssistantDelta { text: part, .. }
            | EventKind::AssistantText { text: part, .. } => text.push_str(&part),
            EventKind::TurnFinished { outcome, .. } => break outcome,
            _ => {}
        }
    };
    master.close().await.unwrap();
    println!("real codex master turn: outcome={outcome:?} errors={errors:?} reply={text:?}");
    assert_eq!(errors, Vec::<String>::new());
    assert_eq!(outcome, TurnOutcome::Success);
    assert!(text.contains("OK"), "{text}");
    drop(folder);
    assert!(
        !base
            .join(format!("master-runtime-{}", runtime_ref.as_str()))
            .exists()
    );
}

/// Qualifies a real Codex master turn with its tools on (DEC-093) inside the tools-on master
/// sandbox, in its own home with a copy of the account's credential: it writes a file in its
/// worktree with its own tools, calls the engine's real `delegate` tool served by the built
/// engine executable, and replies; the engine then takes the recorded task. The reply is built
/// as the master runtime builds it. The turn is bounded by a 240 s wall time.
#[tokio::test]
#[ignore = "live qualification: needs CRAZYAGENT_MANAGED_CODEX_PROFILE"]
async fn a_real_codex_master_with_tools_on_works_in_its_worktree_and_delegates() {
    use super::super::master_runtime::ReplyText;

    let engine = super::live_claude_tests::engine_executable();
    let mut signed_in = SignedIn::codex();
    let connection = signed_in.connection.clone();
    let turn = RequestId::new("codex-live-tools-turn").unwrap();
    let profile = signed_in.acquire(turn.as_str());
    let managed = profile.profile_path().unwrap();
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
    let codex = crate::team::runtime::managed_agent::codex();
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
    } = codex_master_turn(
        &place,
        &profile,
        &codex,
        &connection,
        Duration::from_millis(budget.wall_time_ms),
        Some(&engine),
    )
    .unwrap();
    assert_ne!(home, managed);
    let record = tool_record.unwrap();
    require_master_tool_policy(
        &command,
        Some(crate::engine::claude_host::MasterTools {
            engine: &engine,
            drop: &record,
        }),
    )
    .unwrap();
    let mut master = CodexHost::spawn(
        command,
        sandbox,
        profile,
        home,
        NativeAction::Launch {
            session_id: SessionId::new("codex-live-tools-session").unwrap(),
            harness: NativeHarness::Codex,
            profile_ref: signed_in.profile_ref(),
            worktree_lease: RequestId::new("codex-live-tools-lease").unwrap(),
        },
        connection.account_ref.clone(),
        budget,
        "You are a test. Follow the user's steps exactly and use your tools for them.",
    )
    .await
    .unwrap();
    master
        .start_turn(
            turn.clone(),
            "Do these steps in order. 1. With your shell, create the file note.txt in your working folder containing exactly the word HERON. 2. Call the delegate tool of the crazyagent-team MCP server once with the task \"Write the README\". 3. Reply with exactly the word DONE.",
        )
        .await
        .unwrap();
    let mut errors = Vec::new();
    let mut steps = Vec::new();
    let mut reply = ReplyText::default();
    let outcome = loop {
        let event = master.next_event().await.unwrap();
        match event.kind {
            EventKind::AssistantError { code } => errors.push(code),
            EventKind::TurnError {
                message,
                will_retry: false,
            } => errors.push(message),
            EventKind::AssistantDelta { item_id, text } => reply.push(Some(&item_id), &text, true),
            EventKind::ApprovalRequired { approval, .. } => steps.push(format!("{approval:?}")),
            EventKind::UnsupportedRequest { method, .. } => steps.push(method),
            EventKind::Unsupported {
                subtype: Some(method),
                ..
            } if method == "item/started" || method == "item/completed" => steps.push(method),
            EventKind::TurnFinished { outcome, .. } => break outcome,
            _ => {}
        }
    };
    master.close().await.unwrap();
    let delegations = crate::team::tool::take(&record);
    let note = std::fs::read_to_string(worktree.join("note.txt")).unwrap_or_default();
    println!(
        "real codex tools-on master turn: outcome={outcome:?} errors={errors:?} steps={steps:?} delegations={delegations:?} note={note:?} reply={:?}",
        reply.text
    );
    assert_eq!(errors, Vec::<String>::new());
    assert_eq!(outcome, TurnOutcome::Success);
    assert!(reply.text.contains("DONE"), "{}", reply.text);
    assert_eq!(note.trim(), "HERON");
    assert_eq!(delegations.len(), 1, "{delegations:?}");
    assert_eq!(delegations[0].prompt, "Write the README");
    assert_eq!(std::fs::read_dir(&repository).unwrap().count(), 0);
    drop(credential);
    drop(folder);
}

/// Reproduces the owner's failed turn (LB-48): a real tools-on Codex master with the startup
/// tools section of a project whose team plan has ten Codex sub-agents with tools on is asked
/// "Hey can you start 2 sub agents". The turn must complete without a protocol fault and hand
/// one or two tasks to the engine's delegate tool, even when the model first starts Codex's
/// own sub-agent, which the host interrupts; it prints how many it interrupted and, on a fault,
/// the host's stream diagnostic. With `CRAZYAGENT_LIVE_REPOSITORY` set
/// the master works in a git worktree of a shallow clone of that repository, as an owner's
/// master works in a worktree of the project. Bounded by a 240 s wall time.
#[tokio::test]
#[ignore = "live qualification: needs CRAZYAGENT_MANAGED_CODEX_PROFILE"]
async fn a_real_codex_master_asked_to_start_two_sub_agents_completes_its_turn() {
    let turn = browsky_master_turn("Hey can you start 2 sub agents", true).await;
    assert_eq!(turn.errors, Vec::<String>::new());
    assert_eq!(turn.outcome, TurnOutcome::Success);
    assert!((1..=2).contains(&turn.delegations), "{turn:?}");
}

/// Proves against real Codex that a sub-agent Codex starts on its own is interrupted and the
/// master's turn still completes: a Browsky master without the tools section, which names
/// Codex's own sub-agent tools as not allowed, is asked to use `spawn_agent`. Bounded by a
/// 240 s wall time; prints the host's stdout tail so the child's end is visible.
#[tokio::test]
#[ignore = "live qualification: needs CRAZYAGENT_MANAGED_CODEX_PROFILE"]
async fn a_real_codex_master_s_own_sub_agent_is_interrupted_and_its_turn_completes() {
    let turn = browsky_master_turn(
        "Use your collaboration spawn_agent tool once to start one sub-agent whose task is to reply OK, then tell me in one line what happened to it.",
        false,
    )
    .await;
    assert_eq!(turn.errors, Vec::<String>::new());
    assert_eq!(turn.outcome, TurnOutcome::Success);
    assert!(turn.native >= 1, "{turn:?}");
}

#[derive(Debug)]
struct BrowskyTurn {
    outcome: TurnOutcome,
    native: usize,
    errors: Vec<String>,
    delegations: usize,
}

/// Runs one real tools-on Codex master turn with Browsky's setup in a private engine root, as
/// the owner's master runs. Inputs: the prompt and whether the instructions carry the tools
/// section. Output: the turn's outcome, the Codex sub-agents the host interrupted, the turn's
/// errors and how many tasks reached the delegate tool; a protocol fault panics after
/// printing the host's stream diagnostic.
async fn browsky_master_turn(prompt: &str, tools_section: bool) -> BrowskyTurn {
    use super::super::master_runtime::ReplyText;

    let setup: crate::project_setup::StoredSetup = serde_json::from_value(serde_json::json!({
        "project_id":"browsky","name":"Browsky",
        "about":"It is the world model brain for my Personal Humanoid Robot.",
        "tech_stack":["Python"],"platforms":["other"],"research":[],"pool":null,
        "team":{"master":"codex","master_tools":true,
            "sub_harnesses":[{"harness":"codex","agents":10,"tools":true}],
            "min_agents":1,"max_agents":11},
        "documents":[],"folder":"/tmp/browsky/.crazyagent","created_ms":1,"revision":1
    }))
    .unwrap();
    let instructions = format!(
        "# Browsky\n\n{}\n{}",
        setup.about,
        if tools_section {
            crate::launch_context::master_tools_section(&setup, &[])
        } else {
            String::new()
        }
    );
    let engine = super::live_claude_tests::engine_executable();
    let mut signed_in = SignedIn::codex();
    let connection = signed_in.connection.clone();
    let turn = RequestId::new("codex-live-sub-agents-turn").unwrap();
    let profile = signed_in.acquire(turn.as_str());
    let temporary = tempfile::Builder::new()
        .prefix("ca-live-sub-agents-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    std::fs::set_permissions(temporary.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let base = temporary.path().canonicalize().unwrap();
    let root = Root::open(&base).unwrap();
    let repository = base.join("repository");
    let worktree = base.join("worktrees").join("lease");
    match std::env::var_os("CRAZYAGENT_LIVE_REPOSITORY") {
        Some(source) => {
            let mut url = std::ffi::OsString::from("file://");
            url.push(&source);
            let git = |arguments: &[&std::ffi::OsStr]| {
                let status = crate::spawn_gate::status(
                    std::process::Command::new("/usr/bin/git").args(arguments),
                )
                .unwrap();
                assert!(status.success(), "git {arguments:?}");
            };
            git(&[
                "clone".as_ref(),
                "--quiet".as_ref(),
                "--depth".as_ref(),
                "1".as_ref(),
                &url,
                repository.as_os_str(),
            ]);
            private_directory(&base.join("worktrees"));
            git(&[
                "-C".as_ref(),
                repository.as_os_str(),
                "worktree".as_ref(),
                "add".as_ref(),
                "--quiet".as_ref(),
                "--detach".as_ref(),
                worktree.as_os_str(),
            ]);
        }
        None => {
            private_directory(&repository);
            private_directory(&worktree);
        }
    }
    let codex = crate::team::runtime::managed_agent::codex();
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
    } = codex_master_turn(
        &place,
        &profile,
        &codex,
        &connection,
        Duration::from_millis(budget.wall_time_ms),
        Some(&engine),
    )
    .unwrap();
    let record = tool_record.unwrap();
    let mut master = CodexHost::spawn(
        command,
        sandbox,
        profile,
        home,
        NativeAction::Launch {
            session_id: SessionId::new("codex-live-sub-agents-session").unwrap(),
            harness: NativeHarness::Codex,
            profile_ref: signed_in.profile_ref(),
            worktree_lease: RequestId::new("codex-live-sub-agents-lease").unwrap(),
        },
        connection.account_ref.clone(),
        budget,
        &instructions,
    )
    .await
    .unwrap();
    let account = master.observe_account().await;
    println!(
        "real codex sub-agents master account read: {:?}",
        account.as_ref().map(|_| "read").map_err(|error| &error.0)
    );
    let account = account.unwrap();
    super::super::master_runtime::account_failure(&account).unwrap();
    master.start_turn(turn.clone(), prompt).await.unwrap();
    let mut errors = Vec::new();
    let mut steps = Vec::new();
    let mut reply = ReplyText::default();
    let outcome = loop {
        let event = match master.next_event().await {
            Ok(event) => event,
            Err(error) => {
                let diagnostic = master.diagnostic();
                println!(
                    "real codex sub-agents master turn failed: {} fault={:?} message={:?} steps={steps:?} frames={:?}",
                    error.0,
                    diagnostic.fault,
                    diagnostic.message,
                    diagnostic.frames()
                );
                let _ = master.close().await;
                panic!("{}", error.0);
            }
        };
        match event.kind {
            EventKind::AssistantError { code } => errors.push(code),
            EventKind::TurnError {
                message,
                will_retry: false,
            } => errors.push(message),
            EventKind::AssistantDelta { item_id, text } => reply.push(Some(&item_id), &text, true),
            EventKind::ApprovalRequired { approval, .. } => steps.push(format!("{approval:?}")),
            EventKind::UnsupportedRequest { method, .. } => steps.push(method),
            EventKind::Unsupported {
                subtype: Some(method),
                ..
            } => steps.push(method),
            EventKind::TurnFinished { outcome, .. } => break outcome,
            _ => {}
        }
    };
    master.close().await.unwrap();
    let delegations = crate::team::tool::take(&record);
    let native = steps
        .iter()
        .filter(|step| *step == "native_sub_agent")
        .count();
    println!(
        "real codex sub-agents master turn: outcome={outcome:?} native_sub_agents={native} errors={errors:?} steps={steps:?} delegations={delegations:?} reply={:?}",
        reply.text
    );
    if native > 0 {
        println!("frames={:?}", master.diagnostic().frames());
    }
    drop(credential);
    drop(folder);
    BrowskyTurn {
        outcome,
        native,
        errors,
        delegations: delegations.len(),
    }
}
