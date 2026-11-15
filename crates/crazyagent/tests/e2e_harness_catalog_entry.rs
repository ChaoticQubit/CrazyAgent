#![cfg(unix)]

mod e2e_support;

use crazyagent::harnesses::HarnessStatus;
use crazyagent::harnesses::update::NOT_INSTALLED;
use e2e_support::{Engine, Owner, scratch};
use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::Path;

/// RV-DS241-139: Codex registered at engine start through a PATH link stays installed after
/// that link is removed while another PATH link still leads to the same pinned file. The
/// catalog lists it by that other link, the launcher a launch now uses, never by the removed
/// link or the pinned versioned file, its versions row is an installed one with no install
/// line, and Update is never refused as not installed.
#[tokio::test(flavor = "multi_thread")]
async fn a_registered_harness_whose_path_link_is_gone_is_listed_by_the_path_link_a_launch_uses() {
    let disk = scratch("ca-e2e-entry-");
    let base = disk.path().canonicalize().unwrap();
    let (first, second, kept, owner_home) = (
        base.join("a"),
        base.join("b"),
        base.join("real"),
        base.join("owner"),
    );
    for folder in [&first, &second, &kept, &owner_home] {
        fs::create_dir(folder).unwrap();
        fs::set_permissions(folder, fs::Permissions::from_mode(0o700)).unwrap();
    }
    let real = kept.join("codex");
    fs::write(&real, "#!/bin/sh\necho 'codex-cli 0.200.0'\n").unwrap();
    fs::set_permissions(&real, fs::Permissions::from_mode(0o700)).unwrap();
    let (entry, other) = (first.join("codex"), second.join("codex"));
    symlink(&real, &entry).unwrap();
    symlink(&real, &other).unwrap();
    let mut engine = Engine::new("ca-e2e-entry-engine-");
    engine.home = Some(owner_home.clone());
    engine.harness = Some(("--codex-executable", entry.clone()));
    engine.env.push((
        "PATH",
        std::env::join_paths([
            first.as_path(),
            second.as_path(),
            Path::new("/usr/bin"),
            Path::new("/bin"),
        ])
        .unwrap(),
    ));
    engine.launch();
    fs::remove_file(&entry).unwrap();
    assert!(fs::symlink_metadata(&entry).is_err());
    assert_eq!(fs::canonicalize(&other).unwrap(), real);
    let owner = Owner::attach(&engine).await;

    let entries = owner.controller.harness_catalog(false).await.unwrap();
    let codex = entries.iter().find(|entry| entry.id == "codex").unwrap();
    assert_ne!(codex.status, HarnessStatus::NotInstalled, "{codex:?}");
    assert_eq!(
        codex.executable.as_deref(),
        Some(other.as_path()),
        "listed by the PATH link a launch uses, never the removed link or the real file: {codex:?}"
    );

    let versions = owner
        .controller
        .harness_versions("codex", false)
        .await
        .unwrap();
    assert_eq!(versions.install, None, "{versions:?}");
    assert!(
        versions
            .refusal
            .as_deref()
            .is_none_or(|code| code != NOT_INSTALLED && !code.starts_with("harness_install_")),
        "an installed row, never the not-installed one: {versions:?}"
    );

    let update = owner.controller.harness_update("codex").await;
    assert!(
        update
            .as_ref()
            .err()
            .is_none_or(|error| !error.0.contains(NOT_INSTALLED)),
        "{update:?}"
    );
}
