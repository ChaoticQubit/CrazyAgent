use crate::client_controller::OwnerController;
use crate::engine::owner::{OwnerResponse, OwnerResult};
use std::os::unix::fs::PermissionsExt;

/// Sets a fresh projects home beside the engine root, never inside it.
/// Input: an attached owner controller and the mutation generation.
/// Output: the home folder, kept alive by the caller; panics unless the
/// engine applied it.
pub(in crate::tui) async fn projects_home(
    controller: &mut OwnerController,
    generation: u64,
) -> tempfile::TempDir {
    let home = tempfile::Builder::new()
        .prefix("ca-tui-home-")
        .tempdir_in("/private/tmp")
        .unwrap();
    std::fs::set_permissions(home.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let path = std::fs::canonicalize(home.path()).unwrap();
    let response = controller
        .set_projects_home(path.to_str().unwrap().into(), generation)
        .await
        .unwrap();
    assert!(
        matches!(
            &response,
            OwnerResponse::Applied { operation } if operation.result == OwnerResult::Applied
        ),
        "{response:?}"
    );
    home
}

/// How long a TUI engine test lets the engine and its client wait for one answer.
/// Parallel test engines share one disk, so their flushed writes queue behind
/// each other and one write can take seconds; the 1 s default then answers
/// OutcomeUnknown although the engine is healthy.
pub(in crate::tui) const ENGINE_WAIT: std::time::Duration = std::time::Duration::from_secs(10);

/// Engine settings for TUI engine tests: the defaults with the owner wait
/// raised to `ENGINE_WAIT`.
/// Input: none. Output: the effective configuration to start the engine with.
pub(in crate::tui) fn engine_config() -> crate::config::EffectiveConfig {
    let config = crate::config::resolve_config_bytes(
        &format!("timeout_ms = {}\n", ENGINE_WAIT.as_millis()),
        "",
    )
    .unwrap();
    assert_eq!(
        u128::from(config.settings.timeout_ms),
        ENGINE_WAIT.as_millis()
    );
    config
}
