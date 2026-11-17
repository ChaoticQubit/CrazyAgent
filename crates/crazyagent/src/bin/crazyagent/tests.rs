use super::*;

fn engine_arguments() -> Vec<OsString> {
    [
        "engine",
        "/private/tmp/runtime",
        "/global.toml",
        "/project.toml",
        "e.sock",
    ]
    .into_iter()
    .map(OsString::from)
    .collect()
}

#[test]
fn engine_options_preserve_writable_settings_with_codex_in_both_orders() {
    for options in [
        vec![
            "--writable-settings",
            "--codex-executable",
            "/official/codex",
        ],
        vec![
            "--codex-executable",
            "/official/codex",
            "--writable-settings",
        ],
    ] {
        let mut arguments = engine_arguments();
        arguments.extend(options.into_iter().map(OsString::from));
        let (writable, executable) = engine_options(&arguments).unwrap();
        assert!(writable);
        assert_eq!(executable, Some(Path::new("/official/codex")));
    }
}

#[test]
fn engine_options_preserve_existing_no_codex_shapes() {
    let mut arguments = engine_arguments();
    assert_eq!(engine_options(&arguments).unwrap(), (false, None));
    arguments.push("--writable-settings".into());
    assert_eq!(engine_options(&arguments).unwrap(), (true, None));
}

#[test]
fn engine_options_reject_duplicate_unknown_and_missing_options() {
    for options in [
        vec!["--writable-settings", "--writable-settings"],
        vec!["--codex-executable"],
        vec!["--unknown"],
        vec![
            "--codex-executable",
            "/official/codex",
            "--codex-executable",
        ],
    ] {
        let mut arguments = engine_arguments();
        arguments.extend(options.into_iter().map(OsString::from));
        assert!(engine_options(&arguments).is_err());
    }
}

/// RV-DS241-140 at engine start: a harness the owner chose or the search found is
/// checked well before it is pinned, so a link in its chain can move in between. Such a
/// harness is pinned only once the real file pinned passes the start checks, while a
/// flag's file stays as the caller placed it.
#[cfg(unix)]
#[tokio::test]
async fn engine_start_pins_a_chosen_or_found_harness_only_when_its_real_file_passes() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let made = tempfile::Builder::new()
        .prefix("ca-start-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    let root = made.path();
    std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o700)).unwrap();
    let global = root.join("global.toml");
    std::fs::write(&global, b"version = 1\ntimeout_ms = 2000\n").unwrap();
    std::fs::set_permissions(&global, std::fs::Permissions::from_mode(0o600)).unwrap();
    let (_stop, stopped) = tokio::sync::watch::channel(false);
    let mut service = crazyagent::engine::bootstrap::EngineService::start_with_settings_sources(
        root,
        "e.sock",
        &global,
        &root.join("project.toml"),
        &stopped,
    )
    .await
    .unwrap();
    let owner = root.join("owner");
    let planted = root.join("work/.crazyagent/agents/a1/bin");
    std::fs::create_dir_all(&owner).unwrap();
    std::fs::create_dir_all(&planted).unwrap();
    let entries = ["codex", "claude"].map(|name| {
        let real = planted.join(name);
        std::fs::write(&real, b"agent-harness").unwrap();
        std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o700)).unwrap();
        let middle = planted.join(format!("{name}-middle"));
        symlink(&real, &middle).unwrap();
        let entry = owner.join(name);
        symlink(&middle, &entry).unwrap();
        entry
    });
    let mut harnesses = crazyagent::paths::Harnesses {
        codex: Some(entries[0].clone()),
        claude: Some(entries[1].clone()),
        rcli: None,
    };
    register_harnesses(&mut service, &mut harnesses, [false, true, true]).unwrap();
    assert_eq!(harnesses.codex.as_deref(), Some(entries[0].as_path()));
    assert_eq!(harnesses.claude, None);
    service.shutdown().await.unwrap();
}

/// DeepSec -191 at engine start: the RCLI host found beside the engine or by the tool
/// resolver is checked well before it is pinned, as a found harness is (RV-DS241-140), so
/// it is pinned only once its real file passes the start checks. A found host whose link
/// leads into an agent's folder is dropped, and the engine runs without it.
#[cfg(unix)]
#[tokio::test]
async fn engine_start_pins_a_found_rcli_host_only_when_its_real_file_passes() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let made = tempfile::Builder::new()
        .prefix("ca-start-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    let root = made.path();
    std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o700)).unwrap();
    let global = root.join("global.toml");
    std::fs::write(&global, b"version = 1\ntimeout_ms = 2000\n").unwrap();
    std::fs::set_permissions(&global, std::fs::Permissions::from_mode(0o600)).unwrap();
    let (_stop, stopped) = tokio::sync::watch::channel(false);
    let mut service = crazyagent::engine::bootstrap::EngineService::start_with_settings_sources(
        root,
        "e.sock",
        &global,
        &root.join("project.toml"),
        &stopped,
    )
    .await
    .unwrap();
    let owner = root.join("owner");
    let planted = root.join("work/.crazyagent/agents/a1/bin");
    std::fs::create_dir_all(&owner).unwrap();
    std::fs::create_dir_all(&planted).unwrap();
    let real = planted.join(crazyagent::paths::RCLI_EXECUTABLE);
    std::fs::write(&real, b"agent-rcli").unwrap();
    std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o700)).unwrap();
    let entry = owner.join(crazyagent::paths::RCLI_EXECUTABLE);
    symlink(&real, &entry).unwrap();
    let mut harnesses = crazyagent::paths::Harnesses {
        codex: None,
        claude: None,
        rcli: Some(entry),
    };
    register_harnesses(&mut service, &mut harnesses, [true, true, true]).unwrap();
    assert_eq!(harnesses.rcli, None);
    service.shutdown().await.unwrap();
}
