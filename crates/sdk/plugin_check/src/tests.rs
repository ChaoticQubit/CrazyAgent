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
    for tier in fs::read_dir(&plugins).expect("plugins reads") {
        let tier = tier.expect("tier reads").path();
        for plugin in fs::read_dir(&tier).expect("tier folder reads") {
            crates.push(plugin.expect("plugin reads").path());
        }
    }
    crates.sort();
    crates
}

#[test]
fn the_good_fixture_passes() {
    assert_eq!(check_crate(&fixture("good")), Ok(()));
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
            fs::read_to_string(plugin.join("build.rs")).expect("build.rs reads"),
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
        let text = fs::read_to_string(src.join(file)).expect("source reads");
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
    fs::create_dir_all(to).expect("target folder");
    for entry in fs::read_dir(from).expect("source reads") {
        let entry = entry.expect("entry reads");
        let target = to.join(entry.file_name());
        if entry.file_type().expect("type reads").is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), &target).expect("file copies");
        }
    }
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
        BTreeSet::from(["plugin_attr_not_allowed"])
    );
}

#[test]
fn cr_h3_a_file_over_its_bound_is_refused_from_a_bounded_read() {
    let dir = std::env::temp_dir().join(format!("plugin-check-bounded-{}", std::process::id()));
    fs::create_dir_all(&dir).expect("scratch folder");
    let path = dir.join("big.txt");
    fs::write(&path, vec![b'a'; 100]).expect("writes");
    assert_eq!(read_bounded(&path, 99), Bounded::TooLarge);
    assert_eq!(read_bounded(&path, 100), Bounded::Text("a".repeat(100)));
    fs::write(&path, [0xff, 0xfe]).expect("writes");
    assert_eq!(read_bounded(&path, 10), Bounded::Unreadable);
    assert_eq!(read_bounded(&dir.join("missing"), 10), Bounded::Unreadable);
    fs::remove_dir_all(&dir).expect("scratch folder removed");
    let (root, big) = scratch("good", "big_file");
    let mut text = fs::read_to_string(big.join("src/lib.rs")).expect("reads");
    text.push_str(&"/// a\n".repeat(source::SOURCE_BYTES_MAX / 6));
    fs::write(big.join("src/lib.rs"), text).expect("writes");
    assert_eq!(
        check_crate(&big).expect_err("refused"),
        vec![Violation {
            file: PathBuf::from("src/lib.rs"),
            line: 1,
            code: "plugin_source_too_large",
        }]
    );
    fs::remove_dir_all(&root).expect("scratch removed");
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
        BTreeSet::from(["plugin_macro_item_not_allowed"])
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
    fs::remove_file(plugin.join("build.rs")).expect("build.rs removed");
    let plugins = root.join("plugins");
    fs::write(plugins.join("README"), "x").expect("stray file");
    fs::create_dir_all(plugins.join("t9")).expect("unknown tier");
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
    fs::remove_dir_all(&root).expect("scratch removed");
}

#[test]
fn sec_h7_the_edition_is_pinned_to_2024() {
    assert_eq!(
        codes("edition_2015"),
        BTreeSet::from(["plugin_manifest_not_allowed"])
    );
    let workspace =
        fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../Cargo.toml"))
            .expect("workspace manifest reads");
    let table: toml::Table = workspace.parse().expect("workspace manifest parses");
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
        let mut manifest = fs::read_to_string(dir.join("Cargo.toml")).expect("reads");
        manifest.push_str(extra);
        fs::write(dir.join("Cargo.toml"), manifest).expect("writes");
        assert_eq!(
            check_crate(&dir)
                .expect_err(name)
                .iter()
                .map(|violation| violation.code)
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([code]),
            "{name}"
        );
        fs::remove_dir_all(&root).expect("scratch removed");
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
        let manifest = fs::read_to_string(dir.join("Cargo.toml")).expect("reads");
        assert!(manifest.contains(from), "{name}");
        fs::write(dir.join("Cargo.toml"), manifest.replace(from, to)).expect("writes");
        assert_eq!(
            check_crate(&dir)
                .expect_err(name)
                .iter()
                .map(|violation| violation.code)
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([code]),
            "{name}"
        );
        fs::remove_dir_all(&root).expect("scratch removed");
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
    let plugins = root.join("plugins");
    for folder in [plugins.clone(), plugins.join("t2"), plugin.join("src")] {
        fs::write(folder.join(".DS_Store"), [0u8; 8]).expect("finder file");
    }
    assert_eq!(check_plugins(&plugins), Ok(()));
    fs::write(plugin.join("src").join("._lib.rs"), [0u8; 8]).expect("resource fork file");
    assert!(check_plugins(&plugins).is_err(), "only .DS_Store is excepted");
    fs::remove_dir_all(&root).expect("scratch removed");
}
