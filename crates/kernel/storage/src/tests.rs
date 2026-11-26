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

/// Every file under a folder whose name ends with the given suffix. Build output, local
/// state and dependencies are left out by name first; any other link panics naming it, since
/// cargo reads through links. Panics naming every folder, entry or type it cannot read.
/// Inputs: the folder and the suffix. Output: the paths, at most 100,000.
pub(crate) fn files(folder: &std::path::Path, suffix: &str) -> Vec<std::path::PathBuf> {
    let mut found = Vec::new();
    let mut pending = vec![folder.to_path_buf()];
    while let Some(next) = pending.pop() {
        let entries =
            std::fs::read_dir(&next).unwrap_or_else(|error| panic!("read {next:?}: {error}"));
        for entry in entries {
            let entry = entry.unwrap_or_else(|error| panic!("entry in {next:?}: {error}"));
            let name = entry.file_name().into_string().unwrap_or_default();
            let path = entry.path();
            let kind = entry
                .file_type()
                .unwrap_or_else(|error| panic!("type of {path:?}: {error}"));
            if matches!(
                name.as_str(),
                "target" | ".local-state" | "node_modules" | ".git" | ".worktrees"
            ) {
                continue;
            }
            assert!(!kind.is_symlink(), "a link under the walk: {path:?}");
            if kind.is_dir() {
                pending.push(path);
            } else if kind.is_file() && name.ends_with(suffix) {
                found.push(path);
            }
        }
        assert!(found.len() <= 100_000);
    }
    found
}

/// This crate's package name, as other manifests name it.
const STORAGE: &str = "crazyagent-kernel-storage";

/// The places one Cargo.toml turns this crate's `test-support` feature on outside a
/// dev-dependency, read as TOML so a dependency table of its own, a wrapped features list,
/// a target table or a feature naming the dependency counts. Panics naming the manifest
/// when it does not parse. Inputs: the manifest's path and text. Output: the places, sorted.
fn refused_places(path: &std::path::Path, text: &str) -> Vec<String> {
    let manifest: toml::Table = toml::from_str(text)
        .unwrap_or_else(|error| panic!("{path:?} does not parse as a manifest: {error}"));
    let kinds = ["dependencies", "dev-dependencies", "build-dependencies"];
    let mut tables: Vec<(String, &toml::Table)> = kinds
        .iter()
        .filter_map(|kind| Some(((*kind).to_owned(), manifest.get(*kind)?.as_table()?)))
        .collect();
    if let Some(targets) = manifest.get("target").and_then(toml::Value::as_table) {
        for (target, inside) in targets {
            for kind in kinds {
                if let Some(table) = inside.get(kind).and_then(toml::Value::as_table) {
                    tables.push((format!("target.{target}.{kind}"), table));
                }
            }
        }
    }
    let mut keys = vec![STORAGE.to_owned()];
    let mut refused = Vec::new();
    for (place, table) in &tables {
        for (key, entry) in *table {
            let entry = entry.as_table();
            let package = entry
                .and_then(|entry| entry.get("package"))
                .and_then(toml::Value::as_str);
            if key != STORAGE && package != Some(STORAGE) {
                continue;
            }
            keys.push(key.clone());
            let turned_on = entry
                .and_then(|entry| entry.get("features"))
                .and_then(toml::Value::as_array)
                .is_some_and(|features| {
                    features
                        .iter()
                        .any(|feature| feature.as_str() == Some("test-support"))
                });
            if turned_on && !place.ends_with("dev-dependencies") {
                refused.push(place.clone());
            }
        }
    }
    let features = manifest.get("features").and_then(toml::Value::as_table);
    for (feature, turns_on) in features.into_iter().flatten() {
        let named = turns_on.as_array().into_iter().flatten().any(|item| {
            keys.iter().any(|key| {
                let item = item.as_str().unwrap_or_default();
                item == format!("{key}/test-support") || item == format!("{key}?/test-support")
            })
        });
        if named {
            refused.push(format!("features.{feature}"));
        }
    }
    refused.sort();
    refused.dedup();
    refused
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
        let text = std::fs::read_to_string(manifest)
            .unwrap_or_else(|error| panic!("read {manifest:?}: {error}"));
        for place in refused_places(manifest, &text) {
            refused.push(format!("{} {place}", manifest.display()));
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

/// The places a member manifest turns storage's `test-support` on outside dev-dependencies,
/// with its `workspace = true` entries read through the root's `[workspace.dependencies]`.
/// Inputs: the root text and the member text. Output: the places.
fn refused_with_root(root: &str, text: &str) -> Vec<String> {
    let root: toml::Table = toml::from_str(root).expect("the root text parses");
    assert!(
        root.get("workspace")
            .and_then(|workspace| workspace.get("dependencies"))
            .is_some(),
        "the root text has workspace dependencies"
    );
    refused_in(text)
}

/// The root text these rows plant: storage under an alias, once with test-support on.
const ROOT_WITH_STORAGE: &str = "[workspace.dependencies]\nstore = { package = \"crazyagent-kernel-storage\", path = \"crates/kernel/storage\", features = [\"test-support\"] }\nplain = { package = \"crazyagent-kernel-storage\", path = \"crates/kernel/storage\" }\n";

#[test]
fn an_inherited_workspace_dependency_turning_test_support_on_is_caught() {
    let text = "[dependencies]\nstore = { workspace = true }\n";
    assert_eq!(refused_with_root(ROOT_WITH_STORAGE, text), ["dependencies"]);
}

#[test]
fn an_inherited_dependency_whose_member_adds_test_support_is_caught() {
    let text = "[dependencies]\nplain = { workspace = true, features = [\"test-support\"] }\n\n[build-dependencies]\nplain = { workspace = true, optional = true }\n\n[features]\nextra = [\"plain?/test-support\"]\n";
    assert_eq!(
        refused_with_root(ROOT_WITH_STORAGE, text),
        ["dependencies", "features.extra"]
    );
}

#[test]
fn an_inherited_dev_dependency_turning_test_support_on_passes() {
    let text = "[dev-dependencies]\nstore = { workspace = true }\nplain = { workspace = true, features = [\"test-support\"] }\n";
    assert_eq!(refused_with_root(ROOT_WITH_STORAGE, text), Vec::<String>::new());
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
    assert_eq!(refused_in(text), ["features.extra", "features.other"]);
}

/// The panic text of a walk of `folder` for manifests, which must panic. Input: the folder.
/// Output: the text.
#[cfg(unix)]
fn files_panic(folder: &std::path::Path) -> String {
    let cause = std::panic::catch_unwind(|| files(folder, "Cargo.toml")).expect_err("files panics");
    cause
        .downcast_ref::<String>()
        .cloned()
        .unwrap_or_default()
}

#[cfg(unix)]
#[test]
fn files_panics_naming_a_folder_link() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("real")).unwrap();
    std::fs::write(root.path().join("real/Cargo.toml"), "").unwrap();
    std::os::unix::fs::symlink(root.path().join("real"), root.path().join("linked")).unwrap();

    let said = files_panic(root.path());

    assert!(said.contains(&format!("{:?}", root.path().join("linked"))), "{said}");
}

#[cfg(unix)]
#[test]
fn files_panics_naming_a_file_link() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("real")).unwrap();
    std::fs::write(root.path().join("real/Cargo.toml"), "").unwrap();
    std::os::unix::fs::symlink(
        root.path().join("real/Cargo.toml"),
        root.path().join("Cargo.toml"),
    )
    .unwrap();

    let said = files_panic(root.path());

    assert!(said.contains(&format!("{:?}", root.path().join("Cargo.toml"))), "{said}");
}

#[cfg(unix)]
#[test]
fn files_leaves_out_a_linked_excluded_folder_before_the_link_check() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("real")).unwrap();
    std::fs::write(root.path().join("real/Cargo.toml"), "").unwrap();
    for name in ["node_modules", "target", ".local-state", ".worktrees", ".git"] {
        std::os::unix::fs::symlink(root.path().join("real"), root.path().join(name)).unwrap();
    }

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
    if readable && rustix::process::geteuid().is_root() {
        return;
    }
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
            || shown.contains("/perf_tests/")
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
