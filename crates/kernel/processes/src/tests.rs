use std::path::{Path, PathBuf};

fn workspace_root() -> PathBuf {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    assert!(root.join("Cargo.toml").is_file());
    root
}

fn files_named(folder: &Path, keep: &dyn Fn(&Path) -> bool, found: &mut Vec<PathBuf>) {
    let skipped = [
        "target",
        "node_modules",
        ".git",
        ".local-state",
        ".worktrees",
        "graphify-out",
    ];
    for entry in std::fs::read_dir(folder).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if path.is_dir() {
            if !skipped.contains(&name.as_str()) {
                files_named(&path, keep, found);
            }
        } else if keep(&path) {
            found.push(path);
        }
    }
}

fn names_item_of(text: &str, krate: &str) -> bool {
    let needle = format!("{krate}::");
    text.match_indices(&needle).any(|(at, _)| {
        text[..at]
            .chars()
            .next_back()
            .is_none_or(|before| before != '_' && !before.is_ascii_lowercase())
    })
}

/// The kernel crate names no item of the engine crate or of the sandbox, so it never grows
/// a dependency back on them; the Windows platform crate's `crazyagent_win_sandbox::` paths
/// are allowed, as the boundary check reads the character before the name.
#[test]
fn the_kernel_names_no_crazyagent_or_sandbox_item() {
    let mut sources = Vec::new();
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    files_named(
        &source,
        &|path| path.extension().is_some_and(|e| e == "rs"),
        &mut sources,
    );
    assert!(sources.len() >= 6, "{sources:?}");
    let engine = ["crazy", "agent"].concat();
    for path in &sources {
        let text = std::fs::read_to_string(path).unwrap();
        for krate in [engine.as_str(), "sandbox"] {
            assert!(
                !names_item_of(&text, krate),
                "{} names {krate}::",
                path.display()
            );
        }
    }
    assert!(names_item_of(
        &format!("use crate::x; {}::y", "sandbox"),
        "sandbox"
    ));
    assert!(!names_item_of(
        "crazyagent_win_sandbox::listener_beside",
        "sandbox"
    ));
}

/// Only the engine crate's dev-dependencies turn on this crate's `test-support` feature, so
/// its test helpers never reach a release build. Input: every Cargo.toml in the workspace.
#[test]
fn test_support_is_enabled_only_by_crazyagent_dev_dependencies() {
    let root = workspace_root();
    let mut manifests = Vec::new();
    files_named(
        &root,
        &|path| path.file_name().is_some_and(|n| n == "Cargo.toml"),
        &mut manifests,
    );
    assert!(manifests.len() >= 10, "{manifests:?}");
    let mut enabling = Vec::new();
    for manifest in &manifests {
        let text = std::fs::read_to_string(manifest).unwrap();
        let mut table = String::new();
        for line in text.lines() {
            if line.starts_with('[') {
                table = line.to_owned();
            }
            if line.starts_with("crazyagent-kernel-processes") && line.contains("test-support") {
                let place = manifest
                    .strip_prefix(&root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                enabling.push(format!("{place} {table}"));
            }
        }
    }
    assert_eq!(
        enabling,
        ["crates/crazyagent/Cargo.toml [dev-dependencies]"]
    );
}
