use std::fs;
use std::path::{Path, PathBuf};

use toml::{Table, Value};

use crate::{CANONICAL_BUILD_RS, Manifest, Violation};

/// The most bytes a plugin's `Cargo.toml`, `plugin.toml` or `build.rs` may hold.
pub(crate) const MANIFEST_BYTES_MAX: usize = 65_536;
/// The most permissions one `plugin.toml` may ask for.
pub(crate) const PERMISSIONS_MAX: usize = 64;

/// The only dependency a plugin may have: the SDK, by this exact path.
const SDK_DEPENDENCY: (&str, &str) = ("crazyagent-sdk", "../../../crates/sdk/rust");
/// The only build dependency a plugin may have: this check, by this exact path.
const CHECK_DEPENDENCY: (&str, &str) = (
    "crazyagent-plugin-check",
    "../../../crates/sdk/plugin_check",
);
/// The tables a plugin `Cargo.toml` may hold.
const TOP_KEYS: [&str; 4] = ["package", "lib", "dependencies", "build-dependencies"];
/// The keys a plugin `[package]` may hold.
const PACKAGE_KEYS: [&str; 6] = ["name", "version", "edition", "license", "publish", "build"];
/// The keys a plugin `[lib]` may hold.
const LIB_KEYS: [&str; 2] = ["name", "path"];
/// Folders and files cargo would build as other targets, whose code the scan never reads.
const TARGETS_NOT_ALLOWED: [&str; 5] = ["tests", "examples", "benches", "src/bin", "src/main.rs"];
/// The keys of a `plugin.toml`, each required.
const PLUGIN_KEYS: [&str; 5] = ["plugin", "tier", "kind", "version", "permissions"];

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

/// Reads and checks `plugin.toml`: exactly its five keys, a tier word, a positive
/// version and at most `PERMISSIONS_MAX` permission strings. Input: the crate folder.
/// Output: the manifest, or `plugin_manifest_invalid` at the first refused key.
pub(crate) fn plugin(dir: &Path) -> Result<Manifest, Violation> {
    let mut out = Vec::new();
    let text = read(dir, "plugin.toml", &mut out, "plugin_manifest_invalid");
    let invalid = |text: &str, needle: &str| Violation {
        file: PathBuf::from("plugin.toml"),
        line: line_of(text, needle),
        code: "plugin_manifest_invalid",
    };
    let Some(text) = text else {
        return Err(out.remove(0));
    };
    let table = text.parse::<Table>().map_err(|_| invalid(&text, ""))?;
    if let Some(key) = table
        .keys()
        .find(|key| !PLUGIN_KEYS.contains(&key.as_str()))
    {
        return Err(invalid(&text, key));
    }
    let string = |key: &str| {
        table
            .get(key)
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
            .ok_or_else(|| invalid(&text, key))
    };
    let (plugin, tier, kind) = (string("plugin")?, string("tier")?, string("kind")?);
    if !matches!(tier.as_str(), "t1" | "t2" | "t3") {
        return Err(invalid(&text, "tier"));
    }
    let version = table
        .get("version")
        .and_then(Value::as_integer)
        .filter(|version| *version >= 1)
        .ok_or_else(|| invalid(&text, "version"))?;
    let permissions = table
        .get("permissions")
        .and_then(Value::as_array)
        .filter(|list| list.len() <= PERMISSIONS_MAX)
        .and_then(|list| {
            list.iter()
                .map(|value| value.as_str().map(str::to_owned))
                .collect::<Option<Vec<String>>>()
        })
        .ok_or_else(|| invalid(&text, "permissions"))?;
    Ok(Manifest {
        plugin,
        tier,
        kind,
        version,
        permissions,
    })
}

/// Reads one file of the crate folder, bounded by `MANIFEST_BYTES_MAX`. Inputs: the
/// folder, the file name, the violation list and the code for a missing or large file.
/// Output: the text, or `None` with one violation added.
fn read(dir: &Path, name: &str, out: &mut Vec<Violation>, code: &'static str) -> Option<String> {
    let text = fs::read_to_string(dir.join(name))
        .ok()
        .filter(|text| text.len() <= MANIFEST_BYTES_MAX);
    if text.is_none() {
        out.push(Violation {
            file: PathBuf::from(name),
            line: 1,
            code,
        });
    }
    text
}

/// The 1-based line of the first occurrence of a needle, 1 when absent or empty.
/// Inputs: the text and the needle. Output: the line.
fn line_of(text: &str, needle: &str) -> usize {
    if needle.is_empty() {
        return 1;
    }
    text.lines()
        .position(|line| line.contains(needle))
        .map_or(1, |index| index + 1)
}
