#![cfg(unix)]

use crazyagent::engine::bootstrap::EngineService;
use crazyagent::plugins::{Scope, ServerSpec, ServerStatus};
use crazyagent_desktop::bridge::NativeBridge;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::Duration;
use tauri::ipc::Origin;

const FIXTURE: &str = include_str!("../../../../crates/crazyagent/tests/fixtures/mcp_fixture.py");

async fn flow(root: &Path) -> Result<(), String> {
    let code = |e: crazyagent_desktop::bridge::BridgeError| e.code.to_owned();
    let mut bridge = NativeBridge::new(Some(root.join("e.sock")));
    assert!(bridge.connect().await.map_err(code)?.connected);
    let folder = root.join("picked/notes");
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(
        folder.join("SKILL.md"),
        "---\nname: release-notes\ndescription: Writes release notes.\n---\n",
    )
    .unwrap();
    let skill = bridge
        .add_skill(Scope::Global, folder.display().to_string())
        .await
        .map_err(code)?;
    assert_eq!(skill.name, "release-notes");
    assert_eq!(
        bridge
            .add_skill(Scope::Global, root.join("picked").display().to_string())
            .await
            .unwrap_err()
            .code,
        "skill_missing_skill_md"
    );
    assert_eq!(bridge.plugin_skills(None).await.map_err(code)?.len(), 1);
    assert!(
        !bridge
            .set_skill_enabled(skill.id.clone(), false)
            .await
            .map_err(code)?
            .enabled
    );

    let server_path = root.join("mcp_fixture.py");
    std::fs::write(&server_path, FIXTURE).unwrap();
    std::fs::set_permissions(&server_path, std::fs::Permissions::from_mode(0o700)).unwrap();
    let spec = ServerSpec::Stdio {
        command: server_path.display().to_string(),
        args: vec![],
    };
    let server = bridge
        .add_mcp_server(
            Scope::Global,
            "fixture".into(),
            spec.clone(),
            vec![("FIXTURE_TOKEN".into(), "dummy-value".into())],
        )
        .await
        .map_err(code)?;
    assert_eq!(server.status_label, "Waiting for approval");
    assert!(server.secrets[0].saved);
    assert_eq!(
        bridge
            .set_mcp_secret(server.id.clone(), "BAD NAME".into(), "x".into())
            .await
            .unwrap_err()
            .code,
        "server_secret_name_invalid"
    );
    let approved = bridge
        .approve_mcp_server(server.id.clone(), spec)
        .await
        .map_err(code)?;
    assert_eq!(approved.status, ServerStatus::Approved);
    let checked = bridge
        .check_mcp_server(server.id.clone())
        .map_err(code)?
        .await
        .map_err(code)?;
    assert_eq!(checked.tools, vec!["echo", "authorized"]);
    let listed = bridge.plugin_servers(None).await.map_err(code)?;
    assert_eq!(listed.len(), 1);
    assert!(!format!("{listed:?}").contains("dummy-value"));
    bridge.remove_mcp_server(server.id).await.map_err(code)?;
    bridge.remove_skill(skill.id).await.map_err(code)?;
    assert!(bridge.plugin_servers(None).await.map_err(code)?.is_empty());
    assert!(bridge.plugin_skills(None).await.map_err(code)?.is_empty());
    Ok(())
}

#[tokio::test]
async fn the_desktop_bridge_adds_checks_and_removes_skills_and_mcp_servers() {
    let root = tempfile::Builder::new()
        .prefix("ca-desk-plugins-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let global = root.path().join("g.toml");
    std::fs::write(&global, "version = 1\ntimeout_ms = 2000\n").unwrap();
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
    let result = tokio::time::timeout(Duration::from_secs(40), &mut body).await;
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
fn plugins_commands_use_the_local_main_window_acl() {
    let mut context: tauri::Context<tauri::Wry> = tauri::generate_context!();
    let authority = context.runtime_authority_mut();
    for command in [
        "desktop_plugin_skills",
        "desktop_plugin_servers",
        "desktop_add_skill",
        "desktop_set_skill_enabled",
        "desktop_remove_skill",
        "desktop_add_mcp_server",
        "desktop_update_mcp_server",
        "desktop_approve_mcp_server",
        "desktop_set_mcp_server_enabled",
        "desktop_remove_mcp_server",
        "desktop_switch_plugin",
        "desktop_set_mcp_secret",
        "desktop_remove_mcp_secret",
        "desktop_check_mcp_server",
        "desktop_project_plugins",
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
