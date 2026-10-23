use crazyagent::harnesses::{AccountKind, HarnessStatus, Search, ToolControl, catalog, specs};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

fn private_dir(prefix: &str) -> tempfile::TempDir {
    let directory = tempfile::Builder::new().prefix(prefix).tempdir().unwrap();
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    directory
}

fn install(directory: &Path, name: &str, mode: u32) -> PathBuf {
    std::fs::create_dir_all(directory).unwrap();
    let path = directory.join(name);
    std::fs::write(&path, "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
    path
}

fn entry<'a>(
    entries: &'a [crazyagent::harnesses::HarnessEntry],
    id: &str,
) -> &'a crazyagent::harnesses::HarnessEntry {
    entries.iter().find(|entry| entry.id == id).unwrap()
}

#[test]
fn catalog_lists_owner_named_harnesses_first_in_priority_order() {
    let ids: Vec<_> = specs().iter().map(|spec| spec.id).collect();
    assert_eq!(
        ids[..10],
        [
            "codex",
            "claude-code",
            "opencode",
            "gemini-cli",
            "cursor-agent",
            "fx",
            "antigravity",
            "openclaw",
            "pi",
            "rcli"
        ]
    );
    for other in [
        "amp",
        "goose",
        "aider",
        "qwen-code",
        "crush",
        "droid",
        "kiro",
        "copilot",
    ] {
        assert!(ids.contains(&other), "{other}");
    }
    let mut unique = ids.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(unique.len(), ids.len());
}

#[test]
fn empty_search_reports_every_harness_not_installed() {
    let entries = catalog(&Search::new(Vec::new(), None, Vec::new()));
    assert_eq!(entries.len(), specs().len());
    for entry in &entries {
        assert_eq!(entry.status, HarnessStatus::NotInstalled, "{}", entry.id);
        assert_eq!(entry.status_label, "Not installed");
        assert!(entry.executable.is_none());
        assert!(!entry.name.is_empty());
        assert!(!entry.headless.is_empty());
        assert!(!entry.account_kinds.is_empty(), "{}", entry.id);
    }
}

#[test]
fn search_finds_executable_files_on_path_and_skips_unusable_candidates() {
    let root = private_dir("ca-harness-path-");
    let bin = root.path().join("bin");
    let opencode = install(&bin, "opencode", 0o755);
    install(&bin, "gemini", 0o644);
    std::fs::create_dir_all(bin.join("cursor-agent")).unwrap();
    let relative = install(&root.path().join("relative"), "goose", 0o755);
    assert!(relative.is_file());
    let entries = catalog(&Search::new(
        vec![PathBuf::from("relative"), bin.clone()],
        None,
        Vec::new(),
    ));
    let found = entry(&entries, "opencode");
    assert_eq!(found.executable.as_deref(), Some(opencode.as_path()));
    assert_ne!(found.status, HarnessStatus::NotInstalled);
    assert!(
        found.status_label.starts_with("Limited control: "),
        "{}",
        found.status_label
    );
    assert_eq!(
        entry(&entries, "gemini-cli").status,
        HarnessStatus::NotInstalled
    );
    assert_eq!(
        entry(&entries, "cursor-agent").status,
        HarnessStatus::NotInstalled
    );
    assert_eq!(entry(&entries, "goose").status, HarnessStatus::NotInstalled);
}

#[test]
fn search_finds_known_install_locations_under_home_without_path() {
    let home = private_dir("ca-harness-home-");
    let agy = install(&home.path().join(".local/bin"), "agy", 0o755);
    let opencode = install(&home.path().join(".opencode/bin"), "opencode", 0o755);
    let entries = catalog(&Search::new(
        Vec::new(),
        Some(home.path().to_path_buf()),
        Vec::new(),
    ));
    assert_eq!(
        entry(&entries, "antigravity").executable.as_deref(),
        Some(agy.as_path())
    );
    assert_eq!(
        entry(&entries, "opencode").executable.as_deref(),
        Some(opencode.as_path())
    );
    assert_eq!(entry(&entries, "codex").status, HarnessStatus::NotInstalled);
}

#[test]
fn registered_engine_executable_takes_precedence_over_path() {
    let root = private_dir("ca-harness-registered-");
    let on_path = install(&root.path().join("path"), "codex", 0o755);
    let registered = install(&root.path().join("registered"), "codex", 0o755);
    let entries = catalog(&Search::new(
        vec![on_path.parent().unwrap().to_path_buf()],
        None,
        vec![("codex".into(), registered.clone())],
    ));
    let codex = entry(&entries, "codex");
    assert_eq!(codex.executable.as_deref(), Some(registered.as_path()));
    assert_eq!(codex.status, HarnessStatus::Ready);
    assert_eq!(codex.status_label, "Ready");
}

#[test]
fn codex_and_claude_report_switchable_tools_models_and_account_kinds() {
    let entries = catalog(&Search::new(Vec::new(), None, Vec::new()));
    let codex = entry(&entries, "codex");
    assert_eq!(codex.name, "Codex");
    assert_eq!(codex.tools, ToolControl::Switchable);
    assert!(codex.model_selection);
    assert_eq!(
        codex.account_kinds,
        [
            AccountKind::Subscription,
            AccountKind::ApiKey,
            AccountKind::OpenAiCompatibleUrl,
            AccountKind::Local
        ]
    );
    let claude = entry(&entries, "claude-code");
    assert_eq!(claude.name, "Claude Code");
    assert_eq!(claude.tools, ToolControl::Switchable);
    assert!(claude.model_selection);
    assert!(claude.account_kinds.contains(&AccountKind::Subscription));
    assert!(claude.account_kinds.contains(&AccountKind::ApiKey));
    let antigravity = entry(&entries, "antigravity");
    assert!(matches!(antigravity.tools, ToolControl::Limited { .. }));
}

#[test]
fn installed_codex_and_claude_are_ready_from_the_real_environment() {
    let entries = catalog(&Search::from_environment());
    for id in ["codex", "claude-code"] {
        let found = entry(&entries, id);
        assert_eq!(found.status, HarnessStatus::Ready, "{id}: {found:?}");
        assert!(found.executable.as_ref().unwrap().is_absolute());
    }
}

#[test]
fn entries_serialize_with_plain_wire_names() {
    let entries = catalog(&Search::new(Vec::new(), None, Vec::new()));
    let value = serde_json::to_value(entry(&entries, "antigravity")).unwrap();
    assert_eq!(value["id"], "antigravity");
    assert_eq!(value["status"]["kind"], "not_installed");
    assert_eq!(value["tools"]["kind"], "limited");
    assert!(value["tools"]["reason"].as_str().unwrap().len() > 10);
    assert_eq!(value["account_kinds"][0], "subscription");
}
