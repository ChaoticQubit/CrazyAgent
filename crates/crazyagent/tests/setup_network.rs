#![cfg(target_os = "macos")]

use crazyagent::setup::macos::{GRAPHIFY_VERSION, MacInstaller, UV_VERSION, pin};
use crazyagent::setup::{Dependency, Installer};
use std::os::unix::fs::PermissionsExt;

#[test]
fn network_installs_pinned_uv_and_graphify_into_the_app_data_folder() {
    let root = tempfile::Builder::new()
        .prefix("ca-setup-net-")
        .tempdir_in("/private/tmp")
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let installer = MacInstaller::with_paths(root.path(), vec![], vec![]);
    assert_eq!(installer.find(Dependency::Uv).unwrap(), None);

    let uv = installer
        .install(Dependency::Uv, &pin(Dependency::Uv))
        .unwrap();
    assert_eq!(uv.as_str(), UV_VERSION);
    assert!(root.path().join("setup/bin/uv").is_file());

    let graphify = installer
        .install(Dependency::Graphify, &pin(Dependency::Graphify))
        .unwrap();
    assert_eq!(graphify.as_str(), GRAPHIFY_VERSION);
    assert!(root.path().join("setup/bin/graphify").exists());
    assert!(root.path().join("setup/uv/tools").is_dir());
}
