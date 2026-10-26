use super::super::Root;
use super::*;
use crate::adapters::events::{EventKind, TurnOutcome};
use crate::adapters::{NativeAction, NativeHarness};
use crate::engine::claude_host::ClaudeHost;
use crate::protocol::SessionId;
use crate::routing::ResourceBudget;
use crate::team::runtime::managed_agent::SignedInClaude;
use std::os::unix::fs::PermissionsExt;

fn private_directory(path: &Path) {
    std::fs::create_dir_all(path).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
}

#[tokio::test]
#[ignore = "live qualification: needs CRAZYAGENT_MANAGED_CLAUDE_PROFILE"]
async fn a_real_claude_master_turn_is_signed_in_inside_the_master_sandbox() {
    let mut signed_in = SignedInClaude::open();
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
    )
    .unwrap();
    let MasterTurnLaunch {
        command,
        sandbox,
        home,
        credential,
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
