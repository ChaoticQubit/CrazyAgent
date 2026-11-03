use crazyagent::config::{EffectiveConfig, resolve_config};
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
