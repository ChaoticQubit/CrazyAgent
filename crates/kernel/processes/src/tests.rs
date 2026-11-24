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
    let entries =
        std::fs::read_dir(folder).unwrap_or_else(|error| panic!("read {folder:?}: {error}"));
    for entry in entries {
        let entry = entry.unwrap_or_else(|error| panic!("entry in {folder:?}: {error}"));
        let path = entry.path();
        let kind = entry
            .file_type()
            .unwrap_or_else(|error| panic!("type of {path:?}: {error}"));
        let name = entry.file_name().to_string_lossy().into_owned();
        if kind.is_symlink() {
            continue;
        }
        if kind.is_dir() {
            if !skipped.contains(&name.as_str()) {
                files_named(&path, keep, found);
            }
        } else if keep(&path) {
            found.push(path);
        }
    }
}

/// The text of a file the guards read. Panics naming the file when it cannot be read.
/// Input: the file. Output: its text.
fn text_of(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|error| panic!("read {path:?}: {error}"))
}

/// The real path of a file the guards compare. Panics naming the file when it has none.
/// Input: the file. Output: its real path.
fn real_of(path: &Path) -> PathBuf {
    path.canonicalize()
        .unwrap_or_else(|error| panic!("real path of {path:?}: {error}"))
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
        let text = text_of(path);
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

/// The tables of one Cargo.toml that turn on this crate's `test-support` feature: its
/// dependency tables, `workspace.dependencies`, and `features` when one forwards the
/// feature. Read as TOML so a dependency table of its own or a wrapped features line counts
/// as an inline one. Panics naming the manifest when it does not parse. Inputs: the
/// manifest's path and text. Output: each such table's name.
fn test_support_tables(path: &Path, text: &str) -> Vec<String> {
    let manifest: toml::Table = toml::from_str(text)
        .unwrap_or_else(|error| panic!("{path:?} does not parse as a manifest: {error}"));
    let kinds = ["dependencies", "dev-dependencies", "build-dependencies"];
    let mut tables: Vec<(String, &toml::Value)> = kinds
        .iter()
        .filter_map(|kind| Some(((*kind).to_owned(), manifest.get(*kind)?)))
        .collect();
    if let Some(targets) = manifest.get("target").and_then(toml::Value::as_table) {
        for (target, inside) in targets {
            for kind in kinds {
                if let Some(table) = inside.get(kind) {
                    tables.push((format!("target.{target}.{kind}"), table));
                }
            }
        }
    }
    if let Some(shared) = manifest
        .get("workspace")
        .and_then(|workspace| workspace.get("dependencies"))
    {
        tables.push(("workspace.dependencies".to_owned(), shared));
    }
    let name = "crazyagent-kernel-processes";
    let keys: Vec<&str> = tables
        .iter()
        .filter_map(|(_, table)| table.as_table())
        .flat_map(|table| table.iter())
        .filter(|(key, entry)| {
            key.as_str() == name || entry.get("package").and_then(toml::Value::as_str) == Some(name)
        })
        .map(|(key, _)| key.as_str())
        .collect();
    let forwards = manifest
        .get("features")
        .and_then(toml::Value::as_table)
        .is_some_and(|features| {
            features
                .values()
                .filter_map(toml::Value::as_array)
                .flatten()
                .filter_map(toml::Value::as_str)
                .any(|feature| {
                    std::iter::once(name)
                        .chain(keys.iter().copied())
                        .any(|key| {
                            feature == format!("{key}/test-support")
                                || feature == format!("{key}?/test-support")
                        })
                })
        });
    let mut found: Vec<String> = tables
        .into_iter()
        .filter(|(_, table)| {
            table.as_table().is_some_and(|table| {
                table.iter().any(|(key, entry)| {
                    let entry = entry.as_table();
                    let named = key == name
                        || entry
                            .and_then(|entry| entry.get("package"))
                            .and_then(toml::Value::as_str)
                            == Some(name);
                    let features = entry
                        .and_then(|entry| entry.get("features"))
                        .and_then(toml::Value::as_array);
                    named
                        && features.is_some_and(|features| {
                            features
                                .iter()
                                .any(|feature| feature.as_str() == Some("test-support"))
                        })
                })
            })
        })
        .map(|(table, _)| table)
        .collect();
    if forwards {
        found.push("features".to_owned());
    }
    found
}

/// A folder that holds a link to a sibling folder gives each file once, never through the
/// link, so a linked folder never counts a file twice.
#[cfg(unix)]
#[test]
fn files_named_skips_folder_links() -> crate::update::test_support::TestResult {
    use crate::update::test_support::at;
    let temporary = tempfile::tempdir()?;
    let real = at(temporary.path(), temporary.path().canonicalize())?;
    let held = real.join("held");
    at(&held, std::fs::create_dir(&held))?;
    let one = held.join("one.rs");
    at(&one, std::fs::write(&one, "fn one() {}\n"))?;
    let linked = real.join("linked");
    at(&linked, std::os::unix::fs::symlink(&held, &linked))?;
    let mut found = Vec::new();
    files_named(&real, &|_| true, &mut found);
    assert_eq!(found, vec![one]);
    Ok(())
}

/// A manifest that turns `test-support` on through a dependency table of its own, with the
/// features on the next line, is caught as plainly as an inline one.
#[test]
fn a_dependency_table_turning_test_support_on_is_caught() {
    let text = "[package]\nname = \"other\"\n\n[dev-dependencies.crazyagent-kernel-processes]\npath = \"../kernel/processes\"\nfeatures = [\"test-support\"]\n";
    assert_eq!(
        test_support_tables(Path::new("other/Cargo.toml"), text),
        ["dev-dependencies"]
    );
}

/// A manifest whose own feature forwards this crate's `test-support` is caught, since a
/// forward in `default` reaches a release build.
#[test]
fn a_feature_forwarding_test_support_is_caught() {
    let text = "[package]\nname = \"other\"\n\n[features]\ndefault = [\"crazyagent-kernel-processes/test-support\"]\n\n[dependencies]\ncrazyagent-kernel-processes = { path = \"../kernel/processes\" }\n";
    assert_eq!(
        test_support_tables(Path::new("other/Cargo.toml"), text),
        ["features"]
    );
}

/// A workspace dependency row that turns `test-support` on is caught, since every member
/// that inherits it with `workspace = true` gets the feature.
#[test]
fn a_workspace_dependency_turning_test_support_on_is_caught() {
    let text = "[workspace]\nmembers = [\"crates/other\"]\n\n[workspace.dependencies]\ncrazyagent-kernel-processes = { path = \"crates/kernel/processes\", features = [\"test-support\"] }\n";
    assert_eq!(
        test_support_tables(Path::new("Cargo.toml"), text),
        ["workspace.dependencies"]
    );
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
        let text = text_of(manifest);
        for table in test_support_tables(manifest, &text) {
            let place = manifest
                .strip_prefix(&root)
                .unwrap_or_else(|_| panic!("{manifest:?} is outside {root:?}"))
                .to_string_lossy()
                .replace('\\', "/");
            enabling.push(format!("{place} {table}"));
        }
    }
    assert_eq!(enabling, ["crates/crazyagent/Cargo.toml dev-dependencies"]);
}

/// The stub hooks are named only in the kernel's own sources, so no crate outside it can
/// build places on hooks that answer nothing. The name is spelled in parts here, so this row
/// is not its own hit; at least one hit inside the sources shows the search reads them.
#[test]
fn the_stub_hooks_are_named_only_inside_the_kernel_sources() {
    let name = ["STUB", "_HOOKS"].concat();
    let source = real_of(&Path::new(env!("CARGO_MANIFEST_DIR")).join("src"));
    let mut files = Vec::new();
    files_named(
        &workspace_root(),
        &|path| path.extension().is_some_and(|e| e == "rs"),
        &mut files,
    );
    assert!(files.len() >= 100, "{}", files.len());
    let (inside, outside): (Vec<&PathBuf>, Vec<&PathBuf>) = files
        .iter()
        .filter(|path| text_of(path).contains(&name))
        .partition(|path| real_of(path).starts_with(&source));
    assert!(!inside.is_empty(), "the search found no stub hooks at all");
    assert!(outside.is_empty(), "named outside the kernel: {outside:?}");
}

/// `finding::test_support` holds exactly the items crazyagent's rows need and nothing else:
/// a new export fails this row until it is named here.
#[test]
fn finding_test_support_exports_exactly_the_named_items() {
    let text = text_of(&Path::new(env!("CARGO_MANIFEST_DIR")).join("src/finding/test_support.rs"));
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
        "forget_last_good",
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
