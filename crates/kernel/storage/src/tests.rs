use super::*;

#[test]
fn a_storage_error_displays_its_code_unchanged() {
    let error = StorageError("state_log_redact_missing".to_owned());
    assert_eq!(error.to_string(), "state_log_redact_missing");
    let boxed: Box<dyn std::error::Error> = Box::new(error.clone());
    assert_eq!(boxed.to_string(), error.0);
}

/// The workspace root, two folders above this crate. Output: its path.
fn workspace() -> std::path::PathBuf {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .unwrap();
    assert!(root.join("Cargo.toml").is_file(), "{}", root.display());
    root
}

/// Every file under a folder whose name ends with the given suffix, leaving out build
/// output, local state and dependencies. Inputs: the folder and the suffix. Output: the
/// paths, at most 100,000.
fn files(folder: &std::path::Path, suffix: &str) -> Vec<std::path::PathBuf> {
    let mut found = Vec::new();
    let mut pending = vec![folder.to_path_buf()];
    while let Some(next) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&next) else {
            continue;
        };
        for entry in entries.map(Result::unwrap) {
            let name = entry.file_name().into_string().unwrap_or_default();
            let path = entry.path();
            let kind = entry.file_type().unwrap();
            if kind.is_dir()
                && !matches!(
                    name.as_str(),
                    "target" | ".local-state" | "node_modules" | ".git"
                )
            {
                pending.push(path);
            } else if kind.is_file() && name.ends_with(suffix) {
                found.push(path);
            }
        }
        assert!(found.len() <= 100_000);
    }
    found
}

#[test]
fn test_support_is_turned_on_only_by_dev_dependencies() {
    let root = workspace();
    let manifests: Vec<_> = files(&root.join("crates"), "Cargo.toml")
        .into_iter()
        .chain(files(&root.join("apps"), "Cargo.toml"))
        .chain([root.join("Cargo.toml")])
        .collect();
    assert!(manifests.len() > 3, "{manifests:?}");
    let mut refused = Vec::new();
    for manifest in &manifests {
        let mut section = String::new();
        for line in std::fs::read_to_string(manifest).unwrap().lines() {
            let line = line.trim();
            if line.starts_with('[') {
                section = line.to_owned();
                continue;
            }
            if !line.contains("test-support") {
                continue;
            }
            let dev = section == "[dev-dependencies]" || section.ends_with(".dev-dependencies]");
            let definition = section == "[features]" && line == "test-support = []";
            if !dev && !definition {
                refused.push(format!("{} {section} {line}", manifest.display()));
            }
        }
    }
    assert!(
        refused.is_empty(),
        "test-support outside dev-dependencies: {refused:?}"
    );
}

#[test]
fn for_test_names_stay_in_test_files() {
    let root = workspace();
    let own = root.join("crates/kernel/storage/src");
    let mut names = Vec::new();
    for file in files(&own, ".rs") {
        for line in std::fs::read_to_string(&file).unwrap().lines() {
            if let Some(rest) = line.trim_start().strip_prefix("pub fn ")
                && let Some(name) = rest
                    .split('(')
                    .next()
                    .filter(|name| name.ends_with("_for_test"))
            {
                names.push(name.to_owned());
            }
        }
    }
    assert!(names.contains(&"backup_for_test".to_owned()), "{names:?}");
    let mut refused = Vec::new();
    for file in files(&root.join("crates"), ".rs")
        .into_iter()
        .chain(files(&root.join("apps"), ".rs"))
    {
        if file.starts_with(&own) {
            continue;
        }
        let shown = file.to_string_lossy().replace('\\', "/");
        let test_file = shown.contains("/tests/")
            || shown.ends_with("/tests.rs")
            || shown.ends_with("_tests.rs");
        if test_file {
            continue;
        }
        let text = std::fs::read_to_string(&file).unwrap_or_default();
        for name in &names {
            if text.contains(name.as_str()) {
                refused.push(format!("{} {name}", file.display()));
            }
        }
    }
    assert!(
        refused.is_empty(),
        "kernel test names in normal code: {refused:?}"
    );
}
