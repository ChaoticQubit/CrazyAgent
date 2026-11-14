use super::*;
use crate::engine::bootstrap::EngineService;
use crate::harnesses::HarnessStatus;
use std::os::unix::fs::PermissionsExt;

const NEEDS_LINE: &str = "codex needs ca-owner-runtime, which is missing. Install it, or run \
                          setup again from Settings. Then try again.";

fn program(path: &Path, text: &str, mode: u32) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
}

fn codex(entries: Vec<HarnessEntry>) -> HarnessEntry {
    entries
        .into_iter()
        .find(|entry| entry.id == "codex")
        .expect("codex is in the catalog")
}

/// v241 HR-L1: a found harness the tool resolver can't start is listed Can't run with the
/// resolver's line, never Ready, so the settings groups, both Home counts and the New Project
/// default follow a launch; the next list reads Ready once the program its first line names
/// is there, so the status is read on each list, never kept from an old probe.
#[tokio::test]
async fn a_found_harness_the_resolver_cant_start_is_listed_cant_run_until_its_program_is_there() {
    let root = tempfile::Builder::new()
        .prefix("ca-hc-needs-")
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
    let host = root.path().join("crazyagent-pin-only");
    let executable = root.path().join("bin/codex");
    let runtime = root.path().join("runtime/ca-owner-runtime");
    program(&host, "host", 0o700);
    program(&executable, &format!("#!{}\n", runtime.display()), 0o700);
    service.register_native_auth_host(&host).unwrap();
    service.register_codex_executable(&executable).unwrap();
    service.ensure_ready(&stopped).unwrap();
    let client = OwnerClient::discover(root.path(), OwnerClientKind::Tui, Duration::from_secs(20))
        .await
        .unwrap()
        .0;

    let blocked = codex(client.harness_catalog(false).await.unwrap());
    program(&runtime, "#!/bin/sh\n", 0o755);
    let runs = codex(client.harness_catalog(false).await.unwrap());
    tokio::time::timeout(Duration::from_secs(5), service.shutdown())
        .await
        .unwrap()
        .unwrap();

    assert_eq!(blocked.executable, Some(executable.clone()));
    assert_eq!(
        blocked.status,
        HarnessStatus::CantRun {
            reason: NEEDS_LINE.into()
        },
        "a found Codex whose program is missing is never listed Ready"
    );
    assert_eq!(blocked.status_label, format!("Can't run: {NEEDS_LINE}"));
    assert_eq!(runs.executable, Some(executable));
    assert_eq!(
        runs.status,
        HarnessStatus::Ready,
        "the next list reads the program now in place"
    );
    assert_eq!(runs.status_label, "Ready");
}
