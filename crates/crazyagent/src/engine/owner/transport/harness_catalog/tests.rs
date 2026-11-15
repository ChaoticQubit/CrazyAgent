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
    let outside = crate::harnesses::finding::owner_folder("hc-claude-");
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
    assert_eq!(
        after.executable,
        Some(link),
        "still listed by the entry once it leads to versions/2.1.300"
    );
}

/// security-audit vid 1 with rv-sa241 f-10: when the registered Codex cannot be used, here
/// because its pinned file was rewritten in place, the Versions route lists what the engine
/// search finds instead. A Codex an agent planted in a project's `node_modules/.bin` that the
/// search finds there is still never run for its version: the row names the resolver's
/// refusal, and neither the planted file nor the owner's changed one starts.
#[tokio::test]
async fn a_versions_read_that_falls_back_from_a_broken_pin_never_runs_a_planted_codex() {
    let root = tempfile::Builder::new()
        .prefix("ca-hc-fallback-")
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
    let outside = crate::harnesses::finding::owner_folder("hc-fallback-");
    std::fs::set_permissions(outside.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let host = root.path().join("crazyagent-pin-only");
    let owner_ran = outside.path().join("owner-ran");
    let planted_ran = outside.path().join("planted-ran");
    let owned = outside.path().join("owner/codex-0.167.1");
    let work = outside.path().join("proj");
    let planted = work.join("node_modules/@openai/codex/bin/codex.js");
    let link = work.join("node_modules/.bin/codex");
    let version = |ran: &Path| {
        format!(
            "#!/bin/sh\necho ran >> {}\necho 'codex-cli 0.167.1'\n",
            ran.display()
        )
    };
    program(&host, "host", 0o700);
    program(&owned, &version(&owner_ran), 0o700);
    program(&planted, &version(&planted_ran), 0o755);
    std::fs::create_dir_all(work.join(".crazyagent")).unwrap();
    std::fs::create_dir_all(link.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&planted, &link).unwrap();
    service.register_native_auth_host(&host).unwrap();
    service.register_codex_executable(&owned).unwrap();
    service
        .set_harness_search(crate::harnesses::Search::new(
            vec![link.parent().unwrap().to_path_buf()],
            None,
            Vec::new(),
        ))
        .unwrap();
    service.ensure_ready(&stopped).unwrap();
    let client = OwnerClient::discover(root.path(), OwnerClientKind::Tui, Duration::from_secs(20))
        .await
        .unwrap()
        .0;

    std::fs::write(&owned, version(&owner_ran).replace("0.167.1", "0.167.2")).unwrap();
    let versions = client.harness_versions("codex", false).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), service.shutdown())
        .await
        .unwrap()
        .unwrap();

    assert_eq!(versions.installed, None);
    assert!(
        versions.needs.is_some(),
        "the fallback lists the planted Codex and names the resolver's refusal"
    );
    assert!(!planted_ran.exists(), "a planted Codex is never run");
    assert!(!owner_ran.exists(), "a pinned file rewritten in place is never run");
}

/// security-audit vid 1 with rv-sa241 f-11: a Versions request re-pins a registered Codex
/// whose entry now leads to another file only once that file passes the checks of engine
/// start. An entry relinked to a Codex in a project's `.crazyagent` folder, in a folder
/// anyone can write, or to a version-manager shim is refused there: the pin is not replaced,
/// and neither that file nor the owner's first one is run for its version.
#[tokio::test]
async fn a_versions_read_never_repins_an_entry_relinked_to_a_file_engine_start_refuses() {
    let root = tempfile::Builder::new()
        .prefix("ca-hc-repin-")
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
    let outside = crate::harnesses::finding::owner_folder("hc-repin-");
    std::fs::set_permissions(outside.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let host = root.path().join("crazyagent-pin-only");
    let owner_ran = outside.path().join("owner-ran");
    let owned = outside.path().join("owner/codex-0.167.1");
    let entry = outside.path().join("bin/codex");
    let crazyagent = outside.path().join("proj/.crazyagent/worktrees/a/codex");
    let open_folder = outside.path().join("open");
    let open = open_folder.join("codex");
    let shim = outside.path().join("shims/mise");
    let refused = [
        ("agent space", &crazyagent),
        ("open folder", &open),
        ("shim", &shim),
    ];
    let version = |ran: &Path| {
        format!(
            "#!/bin/sh\necho ran >> {}\necho 'codex-cli 0.167.1'\n",
            ran.display()
        )
    };
    program(&host, "host", 0o700);
    program(&owned, &version(&owner_ran), 0o700);
    for (_, file) in &refused {
        program(file, &version(&file.with_extension("ran")), 0o700);
    }
    std::fs::set_permissions(&open_folder, std::fs::Permissions::from_mode(0o777)).unwrap();
    std::fs::create_dir_all(entry.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&owned, &entry).unwrap();
    service.register_native_auth_host(&host).unwrap();
    service.register_codex_executable(&entry).unwrap();
    service
        .set_harness_search(crate::harnesses::Search::new(
            vec![entry.parent().unwrap().to_path_buf()],
            None,
            Vec::new(),
        ))
        .unwrap();
    service.ensure_ready(&stopped).unwrap();
    let runtime = service.owner_runtime().unwrap().clone();
    let client = OwnerClient::discover(root.path(), OwnerClientKind::Tui, Duration::from_secs(20))
        .await
        .unwrap()
        .0;

    let mut seen = Vec::new();
    for (name, file) in &refused {
        std::fs::remove_file(&entry).unwrap();
        std::os::unix::fs::symlink(file, &entry).unwrap();
        let versions = client.harness_versions("codex", false).await.unwrap();
        let registered = runtime.registered_codex_executable();
        let repinned = matches!(registered, Ok(Some(_)));
        let ran = file.with_extension("ran").exists();
        seen.push((*name, versions.installed, repinned, ran));
    }
    tokio::time::timeout(Duration::from_secs(5), service.shutdown())
        .await
        .unwrap()
        .unwrap();

    for (name, installed, repinned, ran) in seen {
        assert_eq!(installed, None, "{name}");
        assert!(!repinned, "{name}: a refused file never replaces the pin");
        assert!(!ran, "{name}: a refused file is never run for its version");
    }
    assert!(
        !owner_ran.exists(),
        "the entry no longer leads to the owner's first file"
    );
}
