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
        ("no_forbid", "plugin_forbid_missing"),
        ("bad_manifest", "plugin_manifest_invalid"),
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
            line: 5,
            code: "plugin_std_not_allowed",
        }]
    );
}

#[test]
fn the_bundled_plugins_pass() {
    let plugins = bundled_plugins();
    assert!(!plugins.is_empty());
    for plugin in plugins {
        assert_eq!(check_crate(&plugin), Ok(()), "{}", plugin.display());
    }
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

#[test]
fn a_root_import_inside_macro_arguments_is_refused() {
    assert_eq!(
        codes("macro_use_root"),
        BTreeSet::from(["plugin_std_not_allowed"])
    );
}

#[test]
fn a_path_attribute_inside_cfg_attr_is_refused() {
    assert_eq!(codes("cfg_attr_path"), BTreeSet::from(["plugin_path_attr"]));
}

#[test]
fn a_raw_crate_root_is_refused_like_the_plain_one() {
    let violations = check_crate(&fixture("raw_std")).expect_err("refused");
    assert_eq!(
        violations
            .iter()
            .map(|violation| (violation.line, violation.code))
            .collect::<Vec<_>>(),
        vec![(5, "plugin_std_not_allowed"), (10, "plugin_std_not_allowed")]
    );
}
