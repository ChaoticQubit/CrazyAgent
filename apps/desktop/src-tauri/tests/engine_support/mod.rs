use crazyagent::config::{EffectiveConfig, resolve_config};
use crazyagent_desktop::bridge::{ActionOutcome, NativeBridge};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

/// The engine settings every desktop bridge test starts the real engine with: the
/// defaults plus a 5 s owner deadline, as the other engine fixtures use, so New Project's
/// git work on a loaded machine is not cut off. Input: the private engine root.
/// Output: the resolved settings, read from `<root>/g`.
pub fn engine_config(root: &Path) -> EffectiveConfig {
    let global = root.join("g");
    std::fs::write(&global, "version = 1\ntimeout_ms = 5000\n").unwrap();
    std::fs::set_permissions(&global, std::fs::Permissions::from_mode(0o600)).unwrap();
    let config = resolve_config(&global, &root.join("p")).unwrap();
    assert_eq!(config.settings.timeout_ms, 5000);
    config
}

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
