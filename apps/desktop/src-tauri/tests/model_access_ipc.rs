use tauri::ipc::Origin;

#[test]
fn metadata_commands_use_the_actual_local_main_window_acl() {
    let mut context: tauri::Context<tauri::Wry> = tauri::generate_context!();
    let authority = context.runtime_authority_mut();
    assert!(
        authority
            .resolve_access("desktop_baselines", "main", "main", &Origin::Local)
            .is_some()
    );
    for command in [
        "desktop_model_access_page",
        "desktop_inspect_model_access",
        "desktop_edit_model_access",
    ] {
        assert!(
            authority
                .resolve_access(command, "main", "main", &Origin::Local)
                .is_some(),
            "accepted metadata command must reach its native owner checks: {command}"
        );
        assert!(
            authority
                .resolve_access(command, "other", "other", &Origin::Local)
                .is_none()
        );
        assert!(
            authority
                .resolve_access(
                    command,
                    "main",
                    "main",
                    &Origin::Remote {
                        url: "https://example.test".parse().unwrap(),
                    },
                )
                .is_none()
        );
    }
    assert!(
        authority
            .resolve_access("desktop_unregistered", "main", "main", &Origin::Local)
            .is_none()
    );
}
