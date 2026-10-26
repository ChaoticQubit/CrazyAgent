use super::super::Root;
use super::*;
use crate::adapters::events::{EventKind, TurnOutcome};
use crate::adapters::{NativeAction, NativeHarness};
use crate::config::{ModelAuthentication, ModelReference};
use crate::engine::bootstrap::{NativeProfileCustody, NativeProfileUsePurpose};
use crate::engine::claude_host::ClaudeHost;
use crate::engine::owner::{HostStamp, NativeProfileBinding};
use crate::protocol::{ConfigId, CredentialRef, Cursor, SessionId};
use crate::routing::ResourceBudget;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::os::unix::fs::PermissionsExt;

const PROFILE: &str = "CRAZYAGENT_MANAGED_CLAUDE_PROFILE";

/// A signed-in managed Claude profile of a real engine, named by its slot folder in
/// `CRAZYAGENT_MANAGED_CLAUDE_PROFILE`, and the engine database its custody belongs to.
struct SignedIn {
    database: PathBuf,
    instance: String,
    binding: Value,
    custody_id: String,
}

/// Reads the profile slot's identity manifest, which holds the stable binding and the
/// engine database instance and no credential. Input: the environment. Output: the
/// profile, or a panic with the plain BLOCKED message when no signed-in profile is named.
fn signed_in() -> SignedIn {
    let slot = std::env::var_os(PROFILE).unwrap_or_else(|| {
        panic!(
            "BLOCKED: no signed-in managed Claude profile. Sign in to a Claude account in CrazyAgent, then run with {PROFILE}=<engine folder>/native-profiles/<profile key>"
        )
    });
    let slot = PathBuf::from(slot)
        .canonicalize()
        .unwrap_or_else(|_| panic!("BLOCKED: {PROFILE} does not name a profile slot"));
    let manifest = std::fs::read(slot.join("identity.json"))
        .unwrap_or_else(|_| panic!("BLOCKED: {PROFILE} has no identity manifest"));
    let identity: Value = serde_json::from_slice(&manifest).unwrap();
    let engine = slot.parent().and_then(Path::parent).unwrap();
    SignedIn {
        database: engine.join("engine.db"),
        instance: identity["root"]["database_instance"]
            .as_str()
            .unwrap()
            .to_owned(),
        binding: identity["binding"].clone(),
        custody_id: format!("{:x}", Sha256::digest(&manifest)),
    }
}

fn private_directory(path: &Path) {
    std::fs::create_dir_all(path).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
}

#[tokio::test]
async fn a_real_claude_master_turn_is_signed_in_inside_the_master_sandbox() {
    let signed_in = signed_in();
    let reference =
        |key: &str| CredentialRef::new(signed_in.binding[key].as_str().unwrap()).unwrap();
    let connection = ModelConnection {
        id: CredentialRef::new("claude-live-master-connection").unwrap(),
        provider: reference("provider"),
        harness: reference("harness"),
        protocol: reference("protocol"),
        account_ref: reference("account_ref"),
        endpoint: ModelEndpoint::Native {},
        model: ModelReference {
            id: "native-default".into(),
            author: reference("provider"),
        },
        auth: ModelAuthentication::NativeManaged {
            profile_ref: reference("profile_ref"),
        },
    };
    let mut custody = NativeProfileCustody::capture(
        &signed_in.database,
        &std::fs::metadata(&signed_in.database).unwrap(),
    )
    .unwrap();
    let host = HostStamp {
        db_instance: signed_in.instance.clone(),
        engine_epoch: "b".repeat(64),
    };
    let binding = NativeProfileBinding {
        database_instance: signed_in.instance.clone(),
        engine_epoch: host.engine_epoch.clone(),
        config_id: ConfigId::new("c".repeat(64)).unwrap(),
        source_hash: "d".repeat(64),
        at_revision: Cursor::new(1),
        connection: connection.clone(),
        custody_id: signed_in.custody_id.clone(),
    };
    let turn = RequestId::new("claude-live-master-turn").unwrap();
    let profile = custody
        .acquire_use(
            &host,
            &binding,
            &turn,
            1,
            NativeProfileUsePurpose::Execution,
        )
        .unwrap();
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
    let writable: Vec<PathBuf> = sandbox
        .sandbox()
        .writable_roots()
        .cloned()
        .collect();
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
            profile_ref: reference("profile_ref"),
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
