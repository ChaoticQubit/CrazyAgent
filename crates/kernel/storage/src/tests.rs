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
/// a target table or a feature naming the dependency counts. A `workspace = true` entry reads
/// its package and features through the root's `[workspace.dependencies]`, with its own
/// features added. Panics naming the manifest when it does not parse. Inputs: the manifest's
/// path, text and the root's workspace dependencies. Output: the places, sorted.
fn refused_places(
    path: &std::path::Path,
    text: &str,
    inherited: Option<&toml::Table>,
) -> Vec<String> {
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
            let from_workspace = entry
                .and_then(|entry| entry.get("workspace"))
                .and_then(toml::Value::as_bool)
                == Some(true);
            let base = inherited
                .filter(|_| from_workspace)
                .and_then(|inherited| inherited.get(key))
                .and_then(toml::Value::as_table);
            let package = [entry, base]
                .into_iter()
                .flatten()
                .find_map(|table| table.get("package").and_then(toml::Value::as_str));
            if key != STORAGE && package != Some(STORAGE) {
                continue;
            }
            keys.push(key.clone());
            let turned_on = [entry, base]
                .into_iter()
                .flatten()
                .filter_map(|table| table.get("features").and_then(toml::Value::as_array))
                .flatten()
                .any(|feature| feature.as_str() == Some("test-support"));
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

/// An I/O step's result with the step and its path named in the error, for row fixtures.
/// Inputs: the result, the step and the path. Output: the value, or the named error.
fn named<T>(result: std::io::Result<T>, step: &str, path: &std::path::Path) -> Result<T, String> {
    result.map_err(|error| format!("{step} {path:?}: {error}"))
}

/// The manifests the test-support guard reads under a workspace root: every Cargo.toml
/// under `crates` and `apps`, the root's own and each member's the root lists. A glob member
/// panics naming it, so no member drops out unread. Input: the root. Output: the paths,
/// sorted and without repeats.
fn manifests(root: &std::path::Path) -> Vec<std::path::PathBuf> {
    let path = root.join("Cargo.toml");
    let text =
        std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("read {path:?}: {error}"));
    let manifest: toml::Table = toml::from_str(&text)
        .unwrap_or_else(|error| panic!("{path:?} does not parse as a manifest: {error}"));
    let members = manifest
        .get("workspace")
        .and_then(|workspace| workspace.get("members"))
        .and_then(toml::Value::as_array)
        .into_iter()
        .flatten()
        .map(|member| {
            let member = member
                .as_str()
                .unwrap_or_else(|| panic!("a member that is not text: {member}"));
            assert!(
                !member.contains(['*', '?', '[']),
                "a glob member the guard cannot list: {member}"
            );
            root.join(member).join("Cargo.toml")
        });
    let mut found: Vec<_> = files(&root.join("crates"), "Cargo.toml")
        .into_iter()
        .chain(files(&root.join("apps"), "Cargo.toml"))
        .chain([path.clone()])
        .chain(members)
        .collect();
    found.sort();
    found.dedup();
    found
}

/// A workspace root with empty `crates` and `apps` folders and the given root manifest.
/// Input: the root manifest text. Output: the folder, or the step and path that failed.
fn planted_root(text: &str) -> Result<tempfile::TempDir, String> {
    let root = tempfile::tempdir().map_err(|error| format!("make a temporary folder: {error}"))?;
    named(
        std::fs::create_dir(root.path().join("crates")),
        "create",
        &root.path().join("crates"),
    )?;
    named(
        std::fs::create_dir(root.path().join("apps")),
        "create",
        &root.path().join("apps"),
    )?;
    named(
        std::fs::write(root.path().join("Cargo.toml"), text),
        "write",
        &root.path().join("Cargo.toml"),
    )?;
    Ok(root)
}

#[test]
fn a_member_outside_crates_and_apps_is_read() -> Result<(), String> {
    let root = planted_root("[workspace]\nmembers = [\"plugins/p\"]\n")?;
    named(
        std::fs::create_dir_all(root.path().join("plugins/p")),
        "create",
        &root.path().join("plugins/p"),
    )?;
    named(
        std::fs::write(root.path().join("plugins/p/Cargo.toml"), ""),
        "write",
        &root.path().join("plugins/p/Cargo.toml"),
    )?;

    let found = manifests(root.path());

    assert!(
        found.contains(&root.path().join("plugins/p/Cargo.toml")),
        "{found:?}"
    );
    Ok(())
}

#[test]
fn a_glob_member_panics_naming_it() -> Result<(), String> {
    let root = planted_root("[workspace]\nmembers = [\"plugins/*\"]\n")?;

    let cause = std::panic::catch_unwind(|| manifests(root.path())).expect_err("a glob panics");
    let said = cause.downcast_ref::<String>().cloned().unwrap_or_default();

    assert!(said.contains("plugins/*"), "{said}");
    Ok(())
}

#[test]
fn test_support_is_turned_on_only_by_dev_dependencies() {
    let root = workspace();
    let root_text = std::fs::read_to_string(root.join("Cargo.toml"))
        .unwrap_or_else(|error| panic!("read the root manifest: {error}"));
    let root_manifest: toml::Table = toml::from_str(&root_text)
        .unwrap_or_else(|error| panic!("the root manifest does not parse: {error}"));
    let inherited = root_manifest
        .get("workspace")
        .and_then(|workspace| workspace.get("dependencies"))
        .and_then(toml::Value::as_table);
    let manifests = manifests(&root);
    assert!(manifests.len() > 3, "{manifests:?}");
    let mut refused = Vec::new();
    for manifest in &manifests {
        let text = std::fs::read_to_string(manifest)
            .unwrap_or_else(|error| panic!("read {manifest:?}: {error}"));
        for place in refused_places(manifest, &text, inherited) {
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
    refused_places(std::path::Path::new("other/Cargo.toml"), text, None)
}

/// The places a member manifest turns storage's `test-support` on outside dev-dependencies,
/// with its `workspace = true` entries read through the root's `[workspace.dependencies]`.
/// Inputs: the root text and the member text. Output: the places.
fn refused_with_root(root: &str, text: &str) -> Vec<String> {
    let root: toml::Table = toml::from_str(root).expect("the root text parses");
    let inherited = root
        .get("workspace")
        .and_then(|workspace| workspace.get("dependencies"))
        .and_then(toml::Value::as_table);
    assert!(
        inherited.is_some(),
        "the root text has workspace dependencies"
    );
    refused_places(std::path::Path::new("member/Cargo.toml"), text, inherited)
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
    assert_eq!(
        refused_with_root(ROOT_WITH_STORAGE, text),
        Vec::<String>::new()
    );
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
    cause.downcast_ref::<String>().cloned().unwrap_or_default()
}

#[cfg(unix)]
#[test]
fn files_panics_naming_a_folder_link() -> Result<(), String> {
    let root = tempfile::tempdir().map_err(|error| format!("make a temporary folder: {error}"))?;
    named(
        std::fs::create_dir(root.path().join("real")),
        "create",
        &root.path().join("real"),
    )?;
    named(
        std::fs::write(root.path().join("real/Cargo.toml"), ""),
        "write",
        &root.path().join("real/Cargo.toml"),
    )?;
    named(
        std::os::unix::fs::symlink(root.path().join("real"), root.path().join("linked")),
        "link",
        &root.path().join("linked"),
    )?;

    let said = files_panic(root.path());

    assert!(
        said.contains(&format!("{:?}", root.path().join("linked"))),
        "{said}"
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn files_panics_naming_a_file_link() -> Result<(), String> {
    let root = tempfile::tempdir().map_err(|error| format!("make a temporary folder: {error}"))?;
    named(
        std::fs::create_dir(root.path().join("real")),
        "create",
        &root.path().join("real"),
    )?;
    named(
        std::fs::write(root.path().join("real/Cargo.toml"), ""),
        "write",
        &root.path().join("real/Cargo.toml"),
    )?;
    let link = root.path().join("Cargo.toml");
    named(
        std::os::unix::fs::symlink(root.path().join("real/Cargo.toml"), &link),
        "link",
        &link,
    )?;

    let said = files_panic(root.path());

    assert!(
        said.contains(&format!("{:?}", root.path().join("Cargo.toml"))),
        "{said}"
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn files_leaves_out_a_linked_excluded_folder_before_the_link_check() -> Result<(), String> {
    let root = tempfile::tempdir().map_err(|error| format!("make a temporary folder: {error}"))?;
    named(
        std::fs::create_dir(root.path().join("real")),
        "create",
        &root.path().join("real"),
    )?;
    named(
        std::fs::write(root.path().join("real/Cargo.toml"), ""),
        "write",
        &root.path().join("real/Cargo.toml"),
    )?;
    for name in [
        "node_modules",
        "target",
        ".local-state",
        ".worktrees",
        ".git",
    ] {
        named(
            std::os::unix::fs::symlink(root.path().join("real"), root.path().join(name)),
            "link",
            &root.path().join(name),
        )?;
    }

    let found = files(root.path(), "Cargo.toml");

    assert_eq!(found, [root.path().join("real/Cargo.toml")]);
    Ok(())
}

#[cfg(unix)]
#[test]
fn files_panics_naming_a_folder_it_cannot_read() -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().map_err(|error| format!("make a temporary folder: {error}"))?;
    let shut = root.path().join("shut");
    named(std::fs::create_dir(&shut), "create", &shut)?;
    named(
        std::fs::set_permissions(&shut, std::fs::Permissions::from_mode(0o000)),
        "set the mode of",
        &shut,
    )?;
    let readable = std::fs::read_dir(&shut).is_ok();

    let walked = std::panic::catch_unwind(|| files(root.path(), "Cargo.toml"));

    named(
        std::fs::set_permissions(&shut, std::fs::Permissions::from_mode(0o700)),
        "set the mode of",
        &shut,
    )?;
    if readable && rustix::process::geteuid().is_root() {
        return Ok(());
    }
    assert!(
        !readable,
        "this user reads a 000 folder, so the row proves nothing"
    );
    let cause = walked.unwrap_err();
    let said = cause.downcast_ref::<String>().cloned().unwrap_or_default();
    assert!(said.contains(&format!("{shut:?}")), "{said}");
    Ok(())
}

#[test]
fn for_test_names_stay_in_test_files() {
    let root = workspace();
    let own = root.join("crates/kernel/storage/src");
    let mut names = Vec::new();
    for file in files(&own, ".rs") {
        for line in std::fs::read_to_string(&file)
            .unwrap_or_else(|error| panic!("read {file:?}: {error}"))
            .lines()
        {
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
