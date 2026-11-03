mod config;

pub use config::engine_config;
use crazyagent_desktop::bridge::{ActionOutcome, NativeBridge};
use std::os::unix::fs::PermissionsExt;

/// Picks a fresh projects home through the desktop bridge, as the first-run
/// screen does, so New Project has a home to make its folder in. The home is a
/// separate folder beside the engine root, never inside it.
/// Inputs: the connected bridge and the draft generation. Output: the home,
/// removed when dropped.
pub async fn pick_home(bridge: &mut NativeBridge, generation: u64) -> tempfile::TempDir {
    let home = tempfile::Builder::new()
        .prefix("ca-desk-home-")
        .tempdir_in("/tmp")
        .unwrap();
    std::fs::set_permissions(home.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let picked = bridge
        .set_projects_home(home.path().to_str().unwrap().into(), generation)
        .await
        .unwrap();
    assert!(
        matches!(picked.outcome, ActionOutcome::Applied),
        "{picked:?}"
    );
    assert_eq!(
        bridge.projects_home().await.unwrap().path.as_deref(),
        Some(home.path().canonicalize().unwrap().to_str().unwrap())
    );
    home
}
