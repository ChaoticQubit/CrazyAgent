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
/// is there, so the status is read on each list, never kept from an old probe. The Codex and
/// its program sit outside the engine's data folder, whose files agents can write.
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
    let outside = tempfile::Builder::new()
        .prefix("hc-codex-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    std::fs::set_permissions(outside.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let host = root.path().join("crazyagent-pin-only");
    let executable = outside.path().join("bin/codex");
    let runtime = outside.path().join("runtime/ca-owner-runtime");
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

fn claude_code(entries: Vec<HarnessEntry>) -> HarnessEntry {
    entries
        .into_iter()
        .find(|entry| entry.id == "claude-code")
        .expect("claude-code is in the catalog")
}

/// v241 DEC-166 gate: a sign-in host registered through its PATH entry, the link a native
/// Claude Code install keeps in `~/.local/bin` and its updater repoints into `versions`, is
/// listed by that entry, never by the versioned file it pinned. The update command and the
/// version read before and after it take the listed path, so a listed versioned file ran the
/// old version and read it again after the update ("Updated from 2.1.200 to 2.1.200").
#[tokio::test]
async fn a_harness_registered_through_its_link_is_listed_by_the_link_its_updater_repoints() {
    let root = tempfile::Builder::new()
        .prefix("ca-hc-entry-")
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
    let outside = tempfile::Builder::new()
        .prefix("hc-claude-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    std::fs::set_permissions(outside.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let host = root.path().join("crazyagent-pin-only");
    let versions = outside.path().join(".local/share/claude/versions");
    let old = versions.join("2.1.200");
    let new = versions.join("2.1.300");
    let link = outside.path().join(".local/bin/claude");
    program(&host, "host", 0o700);
    program(&old, "#!/bin/sh\necho '2.1.200 (Claude Code)'\n", 0o700);
    program(&new, "#!/bin/sh\necho '2.1.300 (Claude Code)'\n", 0o700);
    std::fs::create_dir_all(link.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&old, &link).unwrap();
    service.register_native_auth_host(&host).unwrap();
    service.register_claude_executable(&link).unwrap();
    service.ensure_ready(&stopped).unwrap();
    let client = OwnerClient::discover(root.path(), OwnerClientKind::Tui, Duration::from_secs(20))
        .await
        .unwrap()
        .0;

    let before = claude_code(client.harness_catalog(false).await.unwrap());
    let next = outside.path().join(".local/bin/claude.next");
    std::os::unix::fs::symlink(&new, &next).unwrap();
    std::fs::rename(&next, &link).unwrap();
    let after = claude_code(client.harness_catalog(false).await.unwrap());
    tokio::time::timeout(Duration::from_secs(5), service.shutdown())
        .await
        .unwrap()
        .unwrap();

    assert_eq!(
        before.executable,
        Some(link.clone()),
        "listed by the entry the updater repoints, never by versions/2.1.200"
    );
    assert_eq!(after.executable, Some(link.clone()));
    assert_eq!(
        std::fs::canonicalize(&link).unwrap(),
        std::fs::canonicalize(&new).unwrap(),
        "the listed path leads to the version the update put in place"
    );
}
