use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use super::*;

/// Names a fixture crate. Input: its folder name. Output: its folder.
fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

/// The set of refusal codes a fixture gets. Input: its folder name. Output: the codes.
fn codes(name: &str) -> BTreeSet<&'static str> {
    match check_crate(&fixture(name)) {
        Ok(()) => BTreeSet::new(),
        Err(violations) => violations.iter().map(|violation| violation.code).collect(),
    }
}

/// Every bundled plugin crate folder below `plugins/t*/`. Output: the folders, sorted.
fn bundled_plugins() -> Vec<PathBuf> {
    let plugins = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../plugins");
    let mut crates = Vec::new();
    for tier in list(&plugins) {
        crates.extend(list(&tier));
    }
    crates.sort();
    crates
}

#[test]
fn the_good_fixture_passes() {
    assert_eq!(check_crate(&fixture("good")), Ok(()));
}

#[test]
fn a_no_std_plugin_building_the_new_type_values_passes() {
    assert_eq!(check_crate(&fixture("uses_new_types")), Ok(()));
}

#[test]
fn each_bad_fixture_gets_exactly_its_code() {
    for (name, code) in [
        ("uses_fs", "plugin_std_not_allowed"),
        ("uses_net", "plugin_std_not_allowed"),
        ("uses_process", "plugin_std_not_allowed"),
        ("renamed_std", "plugin_std_not_allowed"),
        ("std_os", "plugin_std_not_allowed"),
        ("std_in_macro", "plugin_std_not_allowed"),
        ("uses_unsafe", "plugin_unsafe"),
        ("other_plugin_dep", "plugin_dep_not_allowed"),
        ("renamed_sdk_dep", "plugin_dep_not_allowed"),
        ("include_str", "plugin_macro_not_allowed"),
        ("macro_rules", "plugin_macro_not_allowed"),
        ("prints", "plugin_macro_not_allowed"),
        ("path_attr", "plugin_path_attr"),
        ("build_rs_changed", "plugin_build_rs_changed"),
        ("extern_crate", "plugin_extern_crate"),
        ("extra_target", "plugin_manifest_not_allowed"),
        ("tests_folder", "plugin_manifest_not_allowed"),
        ("no_forbid", "plugin_header_missing"),
        ("bad_manifest", "plugin_manifest_invalid"),
        ("use_outside_root", "plugin_use_not_allowed"),
    ] {
        assert_eq!(codes(name), BTreeSet::from([code]), "{name}");
    }
}

#[test]
fn a_violation_reports_its_file_and_real_line() {
    let violations = check_crate(&fixture("uses_fs")).expect_err("refused");
    assert_eq!(
        violations,
        vec![Violation {
            file: PathBuf::from("src/lib.rs"),
            line: 6,
            code: "plugin_std_not_allowed",
        }]
    );
}

#[test]
fn the_bundled_plugins_pass() {
    let plugins = bundled_plugins();
    assert!(!plugins.is_empty());
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../plugins");
    assert_eq!(check_plugins(&root), Ok(()));
}

#[test]
fn the_canonical_build_rs_is_what_the_plugins_carry() {
    for plugin in bundled_plugins() {
        assert_eq!(
            read_text(plugin.join("build.rs")),
            CANONICAL_BUILD_RS,
            "{}",
            plugin.display()
        );
    }
}

#[test]
fn a_manifest_permission_list_is_read() {
    let plugin = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../plugins/t2/agent-quit");
    assert_eq!(
        read_manifest(&plugin),
        Ok(Manifest {
            plugin: "agent-quit".to_owned(),
            tier: "t2".to_owned(),
            kind: "setting".to_owned(),
            version: 1,
            permissions: vec!["settings.write".to_owned()],
        })
    );
    assert_eq!(
        read_manifest(&fixture("bad_manifest")).map_err(|violation| violation.code),
        Err("plugin_manifest_invalid")
    );
}

#[test]
fn enforce_here_prints_no_rerun_directive() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    for file in ["lib.rs", "manifest.rs", "source.rs"] {
        let text = read_text(src.join(file));
        for banned in ["rerun-if", "print!", "println!", "cargo:"] {
            assert!(!text.contains(banned), "{file} has {banned}");
        }
    }
}

/// Copies a fixture crate into a scratch folder below a fresh root. Inputs: the fixture
/// name and the path below the root. Output: the root and the copied crate folder.
fn scratch(name: &str, below: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!(
        "plugin-check-{name}-{}-{}",
        std::process::id(),
        below.replace('/', "-")
    ));
    let _ = fs::remove_dir_all(&root);
    let dir = root.join(below);
    copy_tree(&fixture(name), &dir);
    (root, dir)
}

/// Copies a folder tree. Inputs: the source and the target folder. Output: none.
fn copy_tree(from: &Path, to: &Path) {
    make_dir(to);
    for path in list(from) {
        let target = to.join(path.file_name().expect("a listed entry has a name"));
        if path.is_dir() {
            copy_tree(&path, &target);
        } else {
            copy_file(&path, &target);
        }
    }
}

/// Panics with the path and the error when a file operation fails. Inputs: the path and
/// the operation's result. Output: the value.
fn at_path<T>(path: &Path, result: std::io::Result<T>) -> T {
    result.unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

/// Reads a file as text. Input: the path. Output: the text; panics naming the path.
fn read_text(path: impl AsRef<Path>) -> String {
    at_path(path.as_ref(), fs::read_to_string(path.as_ref()))
}

/// Writes a file. Inputs: the path and the bytes. Output: none; panics naming the path.
fn write_file(path: impl AsRef<Path>, bytes: impl AsRef<[u8]>) {
    at_path(path.as_ref(), fs::write(path.as_ref(), bytes));
}

/// Makes a folder and its parents. Input: the path. Output: none; panics naming the path.
fn make_dir(path: impl AsRef<Path>) {
    at_path(path.as_ref(), fs::create_dir_all(path.as_ref()));
}

/// Removes a folder tree. Input: the path. Output: none; panics naming the path.
fn remove_tree(path: impl AsRef<Path>) {
    at_path(path.as_ref(), fs::remove_dir_all(path.as_ref()));
}

/// Removes one file. Input: the path. Output: none; panics naming the path.
fn remove(path: impl AsRef<Path>) {
    at_path(path.as_ref(), fs::remove_file(path.as_ref()));
}

/// Copies one file. Inputs: the source and the target. Output: none; panics naming the
/// source.
fn copy_file(from: impl AsRef<Path>, to: impl AsRef<Path>) {
    at_path(from.as_ref(), fs::copy(from.as_ref(), to.as_ref()));
}

/// Lists a folder's entries. Input: the folder. Output: the entry paths, sorted; panics
/// naming the folder when it or an entry cannot be read.
fn list(folder: impl AsRef<Path>) -> Vec<PathBuf> {
    let folder = folder.as_ref();
    let mut paths: Vec<PathBuf> = at_path(folder, fs::read_dir(folder))
        .map(|entry| at_path(folder, entry).path())
        .collect();
    paths.sort();
    paths
}

#[test]
fn cr_h1_a_root_import_inside_macro_arguments_is_refused() {
    assert_eq!(
        codes("macro_use_root"),
        BTreeSet::from(["plugin_macro_item_not_allowed", "plugin_std_not_allowed"])
    );
}

#[test]
fn cr_h2_a_path_attribute_inside_cfg_attr_is_refused() {
    assert_eq!(
        codes("cfg_attr_path"),
        BTreeSet::from(["plugin_attr_not_allowed", "plugin_tree_not_allowed"])
    );
}

#[test]
fn cr_h3_a_file_over_its_bound_is_refused_from_a_bounded_read() {
    let dir = std::env::temp_dir().join(format!("plugin-check-bounded-{}", std::process::id()));
    make_dir(&dir);
    let path = dir.join("big.txt");
    write_file(&path, vec![b'a'; 100]);
    assert_eq!(read_bounded(&path, 99), Bounded::TooLarge);
    assert_eq!(read_bounded(&path, 100), Bounded::Text("a".repeat(100)));
    write_file(&path, [0xff, 0xfe]);
    assert_eq!(read_bounded(&path, 10), Bounded::Unreadable);
    assert_eq!(read_bounded(&dir.join("missing"), 10), Bounded::Unreadable);
    remove_tree(&dir);
    let (root, big) = scratch("good", "big_file");
    let mut text = read_text(big.join("src/lib.rs"));
    text.push_str(&"/// a\n".repeat(source::SOURCE_BYTES_MAX / 6));
    write_file(big.join("src/lib.rs"), text);
    assert_eq!(
        check_crate(&big).expect_err("refused"),
        vec![Violation {
            file: PathBuf::from("src/lib.rs"),
            line: 1,
            code: "plugin_source_too_large",
        }]
    );
    remove_tree(&root);
}

#[test]
fn cr_h4_a_raw_crate_root_is_refused() {
    let violations = check_crate(&fixture("raw_std")).expect_err("refused");
    assert_eq!(
        violations
            .iter()
            .map(|violation| (violation.line, violation.code))
            .collect::<Vec<_>>(),
        vec![(6, "plugin_raw_ident"), (11, "plugin_raw_ident")]
    );
}

#[test]
fn sec_h1_a_raw_macro_name_is_refused() {
    assert_eq!(codes("raw_ident"), BTreeSet::from(["plugin_raw_ident"]));
}

#[test]
fn sec_h2_a_use_syn_keeps_as_verbatim_is_still_read() {
    assert_eq!(
        codes("verbatim_use"),
        BTreeSet::from(["plugin_std_not_allowed"])
    );
}

#[test]
fn sec_h3_items_inside_macro_arguments_are_refused() {
    assert_eq!(
        codes("block_in_macro"),
        BTreeSet::from(["plugin_macro_item_not_allowed", "plugin_tree_not_allowed"])
    );
}

#[test]
fn sec_h4_attributes_outside_the_allowlist_are_refused() {
    assert_eq!(
        codes("unsafe_attr"),
        BTreeSet::from(["plugin_attr_not_allowed", "plugin_unsafe"])
    );
}

#[test]
fn sec_h5_only_the_exact_header_passes() {
    assert_eq!(
        codes("forbid_tool_lint"),
        BTreeSet::from(["plugin_header_missing"])
    );
}

#[test]
fn sec_h6_the_central_check_refuses_an_unchecked_plugin_folder() {
    let (root, plugin) = scratch("good", "plugins/t2/no-build-rs");
    set_id(&plugin, "\"no-build-rs\"");
    remove(plugin.join("build.rs"));
    let plugins = root.join("plugins");
    write_file(plugins.join("README"), "x");
    make_dir(plugins.join("t9"));
    let violations = check_plugins(&plugins).expect_err("refused");
    let found: BTreeSet<(PathBuf, &str)> = violations
        .iter()
        .map(|violation| (violation.file.clone(), violation.code))
        .collect();
    assert_eq!(
        found,
        BTreeSet::from([
            (PathBuf::from("README"), "plugin_tree_not_allowed"),
            (PathBuf::from("t9"), "plugin_tree_not_allowed"),
            (
                Path::new("t2").join("no-build-rs").join("build.rs"),
                "plugin_build_rs_changed"
            ),
        ])
    );
    remove_tree(&root);
}

#[test]
fn sec_h7_the_edition_is_pinned_to_2024() {
    assert_eq!(
        codes("edition_2015"),
        BTreeSet::from(["plugin_manifest_not_allowed"])
    );
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../Cargo.toml");
    let table: toml::Table = read_text(&path)
        .parse()
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    assert_eq!(
        table["workspace"]["package"]["edition"].as_str(),
        Some("2024")
    );
}

#[test]
fn a_no_std_crate_cannot_name_std_back_in() {
    assert_eq!(
        codes("extern_std"),
        BTreeSet::from(["plugin_extern_crate", "plugin_std_not_allowed"])
    );
}

#[test]
fn an_aliased_file_reading_macro_is_refused() {
    assert_eq!(
        codes("macro_alias"),
        BTreeSet::from(["plugin_macro_not_allowed"])
    );
}

#[test]
fn a_crate_root_renamed_through_crate_or_self_is_refused() {
    assert_eq!(
        codes("alias_alloc"),
        BTreeSet::from(["plugin_std_not_allowed"])
    );
}

#[test]
fn an_attribute_named_by_a_path_is_refused() {
    assert_eq!(
        codes("attr_path_head"),
        BTreeSet::from(["plugin_attr_not_allowed", "plugin_std_not_allowed"])
    );
}

#[test]
fn every_manifest_table_outside_the_allowlist_is_refused() {
    let cases = [
        (
            "target",
            "\n[target.'cfg(unix)'.dependencies]\nlibc = \"1\"\n",
            "plugin_manifest_not_allowed",
        ),
        (
            "patch",
            "\n[patch.crates-io]\nserde = { path = \"x\" }\n",
            "plugin_manifest_not_allowed",
        ),
        (
            "replace",
            "\n[replace]\n\"serde:1.0.0\" = { path = \"x\" }\n",
            "plugin_manifest_not_allowed",
        ),
        (
            "features",
            "\n[features]\nextra = []\n",
            "plugin_manifest_not_allowed",
        ),
        (
            "dev",
            "\n[dev-dependencies]\nserde = \"1\"\n",
            "plugin_manifest_not_allowed",
        ),
    ];
    for (name, extra, code) in cases {
        let (root, dir) = scratch("good", &format!("manifest-{name}"));
        let mut manifest = read_text(dir.join("Cargo.toml"));
        manifest.push_str(extra);
        write_file(dir.join("Cargo.toml"), manifest);
        assert_eq!(
            check_crate(&dir)
                .expect_err(name)
                .iter()
                .map(|violation| violation.code)
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([code]),
            "{name}"
        );
        remove_tree(&root);
    }
    let edits = [
        (
            "inherited",
            "crazyagent-sdk = { path = \"../../../crates/sdk/rust\" }",
            "crazyagent-sdk = { workspace = true }",
            "plugin_dep_not_allowed",
        ),
        (
            "no-edition",
            "edition = \"2024\"\n",
            "",
            "plugin_manifest_not_allowed",
        ),
    ];
    for (name, from, to, code) in edits {
        let (root, dir) = scratch("good", &format!("manifest-{name}"));
        let manifest = read_text(dir.join("Cargo.toml"));
        assert!(manifest.contains(from), "{name}");
        write_file(dir.join("Cargo.toml"), manifest.replace(from, to));
        assert_eq!(
            check_crate(&dir)
                .expect_err(name)
                .iter()
                .map(|violation| violation.code)
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([code]),
            "{name}"
        );
        remove_tree(&root);
    }
}

#[test]
fn a_source_file_not_named_exactly_rs_is_refused() {
    for name in ["upper_ext", "upper_mod"] {
        assert_eq!(
            codes(name),
            BTreeSet::from(["plugin_manifest_not_allowed"]),
            "{name}"
        );
    }
}

#[test]
fn a_use_group_with_a_leading_colon_is_refused() {
    assert_eq!(
        codes("use_leading_group"),
        BTreeSet::from(["plugin_use_not_allowed"])
    );
}

#[test]
fn a_finder_ds_store_file_is_the_one_named_exception() {
    let (root, plugin) = scratch("good", "plugins/t2/finder");
    set_id(&plugin, "\"finder\"");
    let plugins = root.join("plugins");
    for folder in [plugins.clone(), plugins.join("t2"), plugin.join("src")] {
        write_file(folder.join(".DS_Store"), [0u8; 8]);
    }
    assert_eq!(check_plugins(&plugins), Ok(()));
    write_file(plugin.join("src").join("._lib.rs"), [0u8; 8]);
    assert!(
        check_plugins(&plugins).is_err(),
        "only .DS_Store is excepted"
    );
    remove_tree(&root);
}

/// Sets the `plugin` value of a scratch plugin's `plugin.toml`. Inputs: the plugin folder
/// and the new value as TOML text. Output: none; panics when the fixture id is missing.
fn set_id(plugin: &Path, value: &str) {
    let path = plugin.join("plugin.toml");
    let text = read_text(&path);
    assert!(text.contains("plugin = \"fixture\""), "the fixture id line");
    let text = text.replace("plugin = \"fixture\"", &format!("plugin = {value}"));
    write_file(&path, text);
}

/// Collects the (file, code) pairs of a refusal. Input: the violations. Output: the pairs.
fn pairs(violations: &[Violation]) -> BTreeSet<(PathBuf, &'static str)> {
    violations
        .iter()
        .map(|violation| (violation.file.clone(), violation.code))
        .collect()
}

#[test]
fn a_plugin_id_not_equal_to_its_folder_is_refused() {
    let (root, plugin) = scratch("good", "plugins/t2/other-name");
    let plugins = root.join("plugins");
    let violations = check_plugins(&plugins).expect_err("refused");
    assert_eq!(
        pairs(&violations),
        BTreeSet::from([(
            PathBuf::from("t2/other-name/plugin.toml"),
            "plugin_id_not_folder"
        )])
    );
    set_id(&plugin, "\"other-name\"");
    assert_eq!(check_plugins(&plugins), Ok(()));
    remove_tree(&root);
}

#[test]
fn a_plugin_id_outside_its_charset_or_over_64_bytes_is_refused() {
    let long = "a".repeat(65);
    let cases = [
        ("newline", "\"a\\nb\""),
        ("escape", "\"a\\u001bb\""),
        ("upper", "\"Agent-Quit\""),
        ("long", &format!("\"{long}\"")),
        ("underscore", "\"agent_quit\""),
    ];
    for (name, value) in cases {
        let (root, plugin) = scratch("good", &format!("plugins/t2/id-{name}"));
        set_id(&plugin, value);
        let violations = check_crate(&plugin).expect_err(name);
        assert!(
            pairs(&violations).contains(&(PathBuf::from("plugin.toml"), "plugin_id_invalid")),
            "{name}: {violations:?}"
        );
        remove_tree(&root);
    }
    let edge = "a".repeat(64);
    let (root, plugin) = scratch("good", &format!("plugins/t2/{edge}"));
    set_id(&plugin, &format!("\"{edge}\""));
    assert_eq!(
        check_plugins(&root.join("plugins")),
        Ok(()),
        "64 bytes is allowed"
    );
    remove_tree(&root);
}

#[test]
fn a_duplicate_plugin_id_across_tiers_is_refused() {
    let (root, first) = scratch("good", "plugins/t1/twin");
    set_id(&first, "\"twin\"");
    copy_tree(&first, &root.join("plugins/t2/twin"));
    let violations = check_plugins(&root.join("plugins")).expect_err("refused");
    assert_eq!(
        pairs(&violations),
        BTreeSet::from([(PathBuf::from("t2/twin/plugin.toml"), "plugin_id_duplicate")])
    );
    remove_tree(&root);
}

/// Sets a folder's permission bits. Inputs: the folder and the mode. Output: none; panics
/// naming the folder.
#[cfg(unix)]
fn set_mode(folder: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    at_path(
        folder,
        fs::set_permissions(folder, fs::Permissions::from_mode(mode)),
    );
}

#[cfg(unix)]
#[test]
fn an_unreadable_entry_is_a_violation() {
    let (root, plugin) = scratch("good", "plugins/t2/locked");
    set_id(&plugin, "\"locked\"");
    set_mode(&plugin, 0o000);
    let refused = check_plugins(&root.join("plugins"));
    set_mode(&plugin, 0o755);
    let violations = refused.expect_err("an unreadable crate folder is refused");
    assert!(
        pairs(&violations).contains(&(PathBuf::from("t2/locked"), "plugin_tree_not_allowed")),
        "{violations:?}"
    );
    remove_tree(&root);
}

#[test]
fn a_folder_named_ds_store_is_checked() {
    let (root, _) = scratch("good", "plugins/t2/.DS_Store");
    let violations = check_plugins(&root.join("plugins")).expect_err("the folder is checked");
    assert_eq!(
        pairs(&violations),
        BTreeSet::from([(
            PathBuf::from("t2/.DS_Store/plugin.toml"),
            "plugin_id_not_folder"
        )])
    );
    remove_tree(&root);
    let (root, dir) = scratch("good", "ds-store-folder");
    make_dir(dir.join(".DS_Store/payload"));
    assert_eq!(
        check_crate(&dir).expect_err("refused"),
        vec![Violation {
            file: PathBuf::from(".DS_Store"),
            line: 1,
            code: "plugin_tree_not_allowed",
        }]
    );
    remove_tree(&root);
}

#[test]
fn a_crate_root_entry_outside_the_allowlist_is_refused() {
    let (root, plugin) = scratch("good", "root-allowlist");
    make_dir(plugin.join(".cargo"));
    write_file(
        plugin.join(".cargo/config.toml"),
        "[build]\nrustflags = []\n",
    );
    write_file(
        plugin.join("rust-toolchain.toml"),
        "[toolchain]\nchannel = \"nightly\"\n",
    );
    write_file(plugin.join(".DS_Store"), [0u8; 8]);
    assert_eq!(
        check_crate(&plugin).expect_err("refused"),
        vec![
            Violation {
                file: PathBuf::from(".cargo"),
                line: 1,
                code: "plugin_tree_not_allowed",
            },
            Violation {
                file: PathBuf::from("rust-toolchain.toml"),
                line: 1,
                code: "plugin_tree_not_allowed",
            },
        ]
    );
    assert_eq!(
        ROOT_ALLOWED,
        ["Cargo.toml", "build.rs", "plugin.toml", "src"]
    );
    assert_eq!(
        codes("tests_folder"),
        BTreeSet::from(["plugin_manifest_not_allowed"])
    );
    remove_tree(&root);
}

#[cfg(unix)]
#[test]
fn an_unreadable_source_folder_is_a_violation() {
    let (root, plugin) = scratch("good", "locked-source");
    let inner = plugin.join("src/inner");
    make_dir(&inner);
    write_file(inner.join("hidden.rs"), "pub fn f() {}\n");
    set_mode(&inner, 0o000);
    let refused = check_crate(&plugin);
    set_mode(&inner, 0o755);
    assert_eq!(
        refused.expect_err("refused"),
        vec![Violation {
            file: PathBuf::from("src/inner"),
            line: 1,
            code: "plugin_source_unparsed",
        }]
    );
    remove_tree(&root);
}
