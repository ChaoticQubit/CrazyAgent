use std::fs;
use std::path::{Path, PathBuf};

use toml::{Table, Value};

use kernel_plugin_host::manifest::{Bounded, MANIFEST_BYTES_MAX, line_of, read_bounded};

use crate::{CANONICAL_BUILD_RS, Violation};

/// The only dependency a plugin may have: the SDK, by this exact path.
const SDK_DEPENDENCY: (&str, &str) = ("crazyagent-sdk", "../../../crates/sdk/rust");
/// The only build dependency a plugin may have: this check, by this exact path.
const CHECK_DEPENDENCY: (&str, &str) = (
    "crazyagent-plugin-check",
    "../../../crates/sdk/plugin_check",
);
/// The one edition a plugin is built with, literal or inherited from the workspace.
const EDITION: &str = "2024";
/// The tables a plugin `Cargo.toml` may hold.
const TOP_KEYS: [&str; 4] = ["package", "lib", "dependencies", "build-dependencies"];
/// The keys a plugin `[package]` may hold.
const PACKAGE_KEYS: [&str; 6] = ["name", "version", "edition", "license", "publish", "build"];
/// The keys a plugin `[lib]` may hold.
const LIB_KEYS: [&str; 2] = ["name", "path"];
/// Folders and files cargo would build as other targets, whose code the scan never reads.
pub(crate) const TARGETS_NOT_ALLOWED: [&str; 5] =
    ["tests", "examples", "benches", "src/bin", "src/main.rs"];

/// Checks a plugin's `Cargo.toml` and the target folders next to it. Inputs: the crate
/// folder and the violation list. Output: the list grows.
pub(crate) fn cargo(dir: &Path, out: &mut Vec<Violation>) {
    let Some(text) = read(dir, "Cargo.toml", out, "plugin_manifest_not_allowed") else {
        return;
    };
    let file = PathBuf::from("Cargo.toml");
    let refuse = |out: &mut Vec<Violation>, needle: &str, code: &'static str| {
        out.push(Violation {
            file: file.clone(),
            line: line_of(&text, needle),
            code,
        });
    };
    let Ok(table) = text.parse::<Table>() else {
        refuse(out, "", "plugin_manifest_not_allowed");
        return;
    };
    for key in table.keys() {
        if !TOP_KEYS.contains(&key.as_str()) {
            refuse(out, key, "plugin_manifest_not_allowed");
        }
    }
    for (section, allowed) in [("package", &PACKAGE_KEYS[..]), ("lib", &LIB_KEYS[..])] {
        let Some(keys) = table.get(section) else {
            continue;
        };
        let Some(keys) = keys.as_table() else {
            refuse(out, section, "plugin_manifest_not_allowed");
            continue;
        };
        for key in keys.keys() {
            if !allowed.contains(&key.as_str()) {
                refuse(out, key, "plugin_manifest_not_allowed");
            }
        }
    }
    let fixed = [
        ("package", "build", "build.rs"),
        ("lib", "path", "src/lib.rs"),
    ];
    for (section, key, wanted) in fixed {
        let value = table.get(section).and_then(|keys| keys.get(key));
        if value.is_some_and(|value| value.as_str() != Some(wanted)) {
            refuse(out, key, "plugin_manifest_not_allowed");
        }
    }
    let edition = table
        .get("package")
        .and_then(|package| package.get("edition"));
    let pinned = match edition {
        Some(Value::String(edition)) => edition == EDITION,
        Some(Value::Table(inherited)) => {
            inherited.len() == 1
                && inherited.get("workspace").and_then(Value::as_bool) == Some(true)
        }
        _ => false,
    };
    if !pinned {
        refuse(out, "edition", "plugin_manifest_not_allowed");
    }
    for (section, (name, path)) in [
        ("dependencies", SDK_DEPENDENCY),
        ("build-dependencies", CHECK_DEPENDENCY),
    ] {
        let only = table
            .get(section)
            .and_then(Value::as_table)
            .is_some_and(|deps| {
                deps.len() == 1
                    && deps.get(name).and_then(Value::as_table).is_some_and(|dep| {
                        dep.len() == 1 && dep.get("path").and_then(Value::as_str) == Some(path)
                    })
            });
        if !only {
            refuse(out, &format!("[{section}]"), "plugin_dep_not_allowed");
        }
    }
    for target in TARGETS_NOT_ALLOWED {
        if fs::symlink_metadata(dir.join(target)).is_ok() {
            out.push(Violation {
                file: PathBuf::from(target),
                line: 1,
                code: "plugin_manifest_not_allowed",
            });
        }
    }
}

/// Checks that `build.rs` is the canonical one. Inputs: the crate folder and the
/// violation list. Output: the list grows.
pub(crate) fn build_script(dir: &Path, out: &mut Vec<Violation>) {
    let text = read(dir, "build.rs", out, "plugin_build_rs_changed");
    if text.is_some_and(|text| text != CANONICAL_BUILD_RS) {
        out.push(Violation {
            file: PathBuf::from("build.rs"),
            line: 1,
            code: "plugin_build_rs_changed",
        });
    }
}

/// Reads one file of the crate folder, bounded by `MANIFEST_BYTES_MAX`. Inputs: the
/// folder, the file name, the violation list and the code for a missing or large file.
/// Output: the text, or `None` with one violation added.
fn read(dir: &Path, name: &str, out: &mut Vec<Violation>, code: &'static str) -> Option<String> {
    let text = match read_bounded(&dir.join(name), MANIFEST_BYTES_MAX) {
        Bounded::Text(text) => Some(text),
        Bounded::TooLarge | Bounded::Unreadable => None,
    };
    if text.is_none() {
        out.push(Violation {
            file: PathBuf::from(name),
            line: 1,
            code,
        });
    }
    text
}
