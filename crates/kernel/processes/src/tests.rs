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

/// The stub hooks are named only in the kernel's own sources, so no crate outside it can
/// build places on hooks that answer nothing. The name is spelled in parts here, so this row
/// is not its own hit; at least one hit inside the sources shows the search reads them.
#[test]
fn the_stub_hooks_are_named_only_inside_the_kernel_sources() {
    let name = ["STUB", "_HOOKS"].concat();
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .canonicalize()
        .unwrap();
    let mut files = Vec::new();
    files_named(
        &workspace_root(),
        &|path| path.extension().is_some_and(|e| e == "rs"),
        &mut files,
    );
    assert!(files.len() >= 100, "{}", files.len());
    let (inside, outside): (Vec<&PathBuf>, Vec<&PathBuf>) = files
        .iter()
        .filter(|path| std::fs::read_to_string(path).unwrap().contains(&name))
        .partition(|path| path.canonicalize().unwrap().starts_with(&source));
    assert!(!inside.is_empty(), "the search found no stub hooks at all");
    assert!(outside.is_empty(), "named outside the kernel: {outside:?}");
}

/// `finding::test_support` holds exactly the items crazyagent's rows need and nothing else:
/// a new export fails this row until it is named here.
#[test]
fn finding_test_support_exports_exactly_the_named_items() {
    let text = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("src/finding/test_support.rs"),
    )
    .unwrap();
    let linux_only = "#[cfg(target_os = \"linux\")]";
    let lines: Vec<&str> = text.lines().collect();
    let mut exported: Vec<&str> = lines
        .iter()
        .enumerate()
        .filter(|(at, _)| cfg!(target_os = "linux") || *at == 0 || lines[at - 1] != linux_only)
        .filter_map(|(_, line)| line.strip_prefix("pub "))
        .map(|rest| {
            let rest = rest.split_once(' ').map_or(rest, |(_, name)| name);
            rest.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .next()
                .unwrap()
        })
        .collect();
    exported.sort_unstable();
    let mut named = vec![
        "FOLDERS_MAX",
        "HOMEBREW_FOLDERS",
        "Step",
        "allowed",
        "at_roots",
        "below",
        "entry_at",
        "kept",
        "last_run_finished",
        "log_own_node_passed_over",
        "run_between_program_and_check",
        "with_app_folder",
        "with_projects",
        "with_temps",
    ];
    if cfg!(target_os = "linux") {
        named.push("LINUX_FOLDERS");
    }
    named.sort_unstable();
    assert_eq!(exported, named);
    assert!(text.contains(&format!("{linux_only}\npub const LINUX_FOLDERS:")));
    assert!(!text.contains("pub use") && !text.contains("pub mod") && !text.contains("pub struct"));
    assert!(!text.contains(&["STUB", "_HOOKS"].concat()));
}
