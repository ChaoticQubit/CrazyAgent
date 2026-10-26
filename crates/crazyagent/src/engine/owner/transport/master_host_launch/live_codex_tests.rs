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
    let command = master_command(&codex, &managed, &worktree, &connection.model.id).unwrap();
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
