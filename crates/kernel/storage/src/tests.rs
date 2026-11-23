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

/// The places a manifest text turns storage's `test-support` on outside dev-dependencies.
/// Input: the text. Output: the places.
fn refused_in(text: &str) -> Vec<String> {
    refused_places(std::path::Path::new("other/Cargo.toml"), text)
}

#[test]
fn another_crates_own_test_support_definition_passes() {
    let text = "[package]\nname = \"other\"\n\n[features]\ntest-support = [\"dep:tempfile\"]\n\n[dependencies]\ncrazyagent-kernel-storage = { path = \"../storage\" }\ntempfile = { version = \"1\", optional = true }\n";
    assert_eq!(refused_in(text), Vec::<String>::new());
}

#[test]
fn a_dev_dependency_turning_test_support_on_passes() {
    let text = "[dev-dependencies]\ncrazyagent-kernel-storage = { path = \"../storage\", features = [\"test-support\"] }\n\n[target.'cfg(unix)'.dev-dependencies.store]\npackage = \"crazyagent-kernel-storage\"\npath = \"../storage\"\nfeatures = [\"test-support\"]\n";
    assert_eq!(refused_in(text), Vec::<String>::new());
}

#[test]
fn a_dependency_table_of_its_own_with_a_wrapped_features_list_is_caught() {
    let text = "[dependencies.crazyagent-kernel-storage]\npath = \"../storage\"\nfeatures = [\n    \"test-support\",\n]\n";
    assert_eq!(refused_in(text), ["dependencies"]);
}

#[test]
fn a_target_or_build_dependency_turning_test_support_on_is_caught() {
    let text = "[target.'cfg(unix)'.dependencies]\nstore = { package = \"crazyagent-kernel-storage\", path = \"../storage\", features = [\"test-support\"] }\n\n[build-dependencies]\ncrazyagent-kernel-storage = { path = \"../storage\", features = [\"test-support\"] }\n";
    assert_eq!(
        refused_in(text),
        ["build-dependencies", "target.cfg(unix).dependencies"]
    );
}

#[test]
fn a_feature_that_turns_test_support_on_through_a_dependency_is_caught() {
    let text = "[dependencies]\nstore = { package = \"crazyagent-kernel-storage\", path = \"../storage\", optional = true }\n\n[features]\nextra = [\"store?/test-support\"]\nother = [\"crazyagent-kernel-storage/test-support\"]\nplain = [\"dep:store\"]\n";
    assert_eq!(refused_in(text), ["features.extra"]);
}

#[cfg(unix)]
#[test]
fn files_leaves_out_a_folder_link() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("real")).unwrap();
    std::fs::write(root.path().join("real/Cargo.toml"), "").unwrap();
    std::os::unix::fs::symlink(root.path().join("real"), root.path().join("linked")).unwrap();

    let found = files(root.path(), "Cargo.toml");

    assert_eq!(found, [root.path().join("real/Cargo.toml")]);
}

#[cfg(unix)]
#[test]
fn files_panics_naming_a_folder_it_cannot_read() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let shut = root.path().join("shut");
    std::fs::create_dir(&shut).unwrap();
    std::fs::set_permissions(&shut, std::fs::Permissions::from_mode(0o000)).unwrap();
    let readable = std::fs::read_dir(&shut).is_ok();

    let walked = std::panic::catch_unwind(|| files(root.path(), "Cargo.toml"));

    std::fs::set_permissions(&shut, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(
        !readable,
        "this user reads a 000 folder, so the row proves nothing"
    );
    let cause = walked.unwrap_err();
    let said = cause.downcast_ref::<String>().cloned().unwrap_or_default();
    assert!(said.contains(&format!("{shut:?}")), "{said}");
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
