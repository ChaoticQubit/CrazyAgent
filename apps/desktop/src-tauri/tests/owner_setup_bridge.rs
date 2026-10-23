use crazyagent::engine::bootstrap::EngineService;
use crazyagent::owner_profile::{AnswerDetail, Initiative, OwnerProfile, WorkingStyle};
use crazyagent_desktop::bridge::{ActionOutcome, NativeBridge};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::Duration;
use tauri::ipc::Origin;

const CATALOG: &str = r#"version = 1
timeout_ms = 2000
[model_access]
accounts = ["codex-1", "claude-1", "local-1"]
connections = [
  { id = "codex-a", provider = "openai", harness = "codex", protocol = "native", account_ref = "codex-1", endpoint = { kind = "native" }, model = { id = "native-default", author = "openai" }, auth = { method = "native_managed", profile_ref = "codex-profile-a" } },
  { id = "claude-a", provider = "anthropic", harness = "claude", protocol = "native", account_ref = "claude-1", endpoint = { kind = "native" }, model = { id = "native-default", author = "anthropic" }, auth = { method = "native_managed", profile_ref = "claude-profile-a" } },
  { id = "local-a", provider = "openai-compatible", harness = "rcli", protocol = "openai-chat", account_ref = "local-1", endpoint = { kind = "http", url = "http://127.0.0.1:11434/v1" }, model = { id = "qwen2.5:0.5b", author = "openai-compatible" }, auth = { method = "none" } },
]
pools = [{ id = "pool-mixed", name = "Team", members = ["codex-a", "claude-a", "local-a"], projects = [], roles = [], fallback_allowed = false }]
"#;

async fn flow(root: &Path) -> Result<(), String> {
    let mut bridge = NativeBridge::new(Some(root.join("e.sock")));
    assert!(
        bridge
            .connect()
            .await
            .map_err(|e| e.code.to_owned())?
            .connected
    );
    assert_eq!(
        bridge
            .owner_profile()
            .await
            .map_err(|e| e.code.to_owned())?,
        None
    );
    let profile = OwnerProfile {
        name: "Ada".into(),
        role: "Founder".into(),
        bio: String::new(),
        preferred_stack: vec!["Rust".into()],
        working_style: WorkingStyle {
            detail: AnswerDetail::Detailed,
            initiative: Initiative::ActFirst,
        },
    };
    let receipt = bridge
        .save_owner_profile(profile.clone(), 1)
        .await
        .map_err(|e| e.code.to_owned())?;
    assert_eq!(receipt.intent.kind, "owner_profile");
    assert!(matches!(receipt.outcome, ActionOutcome::Applied));
    assert!(!receipt.pending);
    assert_eq!(
        bridge
            .owner_profile()
            .await
            .map_err(|e| e.code.to_owned())?,
        Some(profile.clone())
    );
    let mut blank = profile;
    blank.name = String::new();
    assert_eq!(
        bridge.save_owner_profile(blank, 2).await.unwrap_err().code,
        "owner_profile_name"
    );
    let pools = bridge
        .pool_summaries()
        .await
        .map_err(|e| e.code.to_owned())?;
    assert_eq!(pools.len(), 1);
    assert_eq!(pools[0].name, "Team");
    assert_eq!(pools[0].summary, "1 ChatGPT, 1 Claude, 1 Local model");
    let words: Vec<&str> = pools[0]
        .members
        .iter()
        .map(|m| m.status_label.as_str())
        .collect();
    assert_eq!(words, vec!["Not signed in", "Not signed in", "Set up"]);
    Ok(())
}

#[tokio::test]
async fn the_desktop_bridge_reads_and_saves_the_owner_profile_and_lists_pools() {
    let root = tempfile::Builder::new()
        .prefix("ca-desk-owner-")
        .tempdir_in("/private/tmp")
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let global = root.path().join("g.toml");
    std::fs::write(&global, CATALOG).unwrap();
    std::fs::set_permissions(&global, std::fs::Permissions::from_mode(0o600)).unwrap();
    let (_stop, stopped) = tokio::sync::watch::channel(false);
    let mut service = EngineService::start_with_settings_sources(
        root.path(),
        "e.sock",
        &global,
        &root.path().join("p.toml"),
        &stopped,
    )
    .await
    .unwrap();
    let path = root.path().to_path_buf();
    let mut body = tokio::spawn(async move { flow(&path).await });
    let result = tokio::time::timeout(Duration::from_secs(20), &mut body).await;
    if result.is_err() {
        body.abort();
        let _ = body.await;
    }
    let cleanup = tokio::time::timeout(Duration::from_secs(5), service.shutdown()).await;
    assert!(
        matches!(cleanup, Ok(Ok(()))),
        "actual owner service must stop"
    );
    result.unwrap().unwrap().unwrap();
}

#[test]
fn owner_setup_commands_use_the_local_main_window_acl() {
    let mut context: tauri::Context<tauri::Wry> = tauri::generate_context!();
    let authority = context.runtime_authority_mut();
    for command in [
        "desktop_owner_profile",
        "desktop_save_owner_profile",
        "desktop_pool_summaries",
    ] {
        assert!(
            authority
                .resolve_access(command, "main", "main", &Origin::Local)
                .is_some(),
            "{command} must reach the main window"
        );
        assert!(
            authority
                .resolve_access(command, "other", "other", &Origin::Local)
                .is_none()
        );
    }
}
