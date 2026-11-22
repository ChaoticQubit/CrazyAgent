#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};

use kernel_plugin_host::manifest::{self as host_manifest, Manifest};
#[cfg(test)]
use kernel_plugin_host::manifest::{Bounded, read_bounded};

/// The one build script every plugin crate carries, byte for byte.
pub const CANONICAL_BUILD_RS: &str =
    "fn main() {\n    crazyagent_plugin_check::enforce_here();\n}\n";

/// One rule a plugin crate breaks: the file below the crate folder, its 1-based line
/// (1 when the rule names a whole file or folder) and the refusal code.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Violation {
    pub file: PathBuf,
    pub line: usize,
    pub code: &'static str,
}

/// Checks one plugin crate against every plugin rule. Input: the crate folder.
/// Output: nothing, or every violation sorted by file, line and code.
pub fn check_crate(dir: &Path) -> Result<(), Vec<Violation>> {
    let mut out = Vec::new();
    manifest::cargo(dir, &mut out);
    manifest::build_script(dir, &mut out);
    if let Err(violation) = read_manifest(dir) {
        out.push(violation);
    }
    source::sources(dir, &mut out);
    out.sort();
    out.dedup();
    if out.is_empty() { Ok(()) } else { Err(out) }
}

/// Checks every bundled plugin: each entry of the plugins folder is a tier folder `t1`,
/// `t2` or `t3`, each entry of a tier is a crate passing `check_crate`, and its
/// `plugin.toml` id is its folder name, unique across the tiers. Input: the plugins
/// folder. Output: nothing, or every violation, files relative to that folder.
pub fn check_plugins(root: &Path) -> Result<(), Vec<Violation>> {
    let mut out = Vec::new();
    let mut seen = 0usize;
    let mut ids = std::collections::BTreeSet::new();
    for tier in entries(root, Path::new("."), &mut out) {
        let name = tier.to_string_lossy().into_owned();
        if !matches!(name.as_str(), "t1" | "t2" | "t3") || !is_folder(&root.join(&tier)) {
            out.push(tree(tier));
            continue;
        }
        for plugin in entries(&root.join(&tier), &tier, &mut out) {
            let below = tier.join(&plugin);
            seen += 1;
            if seen > PLUGINS_MAX {
                out.push(tree(below));
                break;
            }
            if !is_folder(&root.join(&below)) {
                out.push(tree(below));
                continue;
            }
            if let Err(violations) = check_crate(&root.join(&below)) {
                out.extend(violations.into_iter().map(|violation| Violation {
                    file: below.join(violation.file),
                    ..violation
                }));
            }
            if let Ok(manifest) = read_manifest(&root.join(&below)) {
                let id_at = |code| Violation {
                    file: below.join("plugin.toml"),
                    line: 1,
                    code,
                };
                if Path::new(&manifest.plugin) != plugin {
                    out.push(id_at("plugin_id_not_folder"));
                }
                if !ids.insert(manifest.plugin) {
                    out.push(id_at("plugin_id_duplicate"));
                }
            }
        }
    }
    out.sort();
    out.dedup();
    if out.is_empty() { Ok(()) } else { Err(out) }
}

/// The one file name the checks skip: the folder view file macOS Finder writes into any
/// folder it shows. It is never Rust source and never a crate.
pub const FINDER_FILE: &str = ".DS_Store";

/// The most bundled plugins the central check walks.
pub const PLUGINS_MAX: usize = 1_024;

/// The sorted entry names of one folder; an unreadable folder is one violation.
/// Inputs: the folder, its name below the plugins folder and the violation list.
/// Output: the names.
fn entries(folder: &Path, below: &Path, out: &mut Vec<Violation>) -> Vec<PathBuf> {
    let Ok(read) = std::fs::read_dir(folder) else {
        out.push(tree(below.to_path_buf()));
        return Vec::new();
    };
    let mut names: Vec<PathBuf> = read
        .flatten()
        .filter(|entry| entry.file_name() != FINDER_FILE)
        .map(|entry| PathBuf::from(entry.file_name()))
        .collect();
    names.sort();
    names
}

/// Whether a path is a real folder, not a symlink. Input: the path. Output: true for one.
fn is_folder(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|meta| meta.is_dir())
}

/// A tree violation at one entry. Input: the entry below the plugins folder.
/// Output: the violation.
fn tree(file: PathBuf) -> Violation {
    Violation {
        file,
        line: 1,
        code: "plugin_tree_not_allowed",
    }
}

/// Reads a plugin's `plugin.toml`, refusing a missing key, a wrong type or any other key.
/// Input: the crate folder. Output: the manifest, or `plugin_manifest_invalid`.
pub fn read_manifest(dir: &Path) -> Result<Manifest, Violation> {
    host_manifest::read(dir).map_err(|refused| Violation {
        file: PathBuf::from("plugin.toml"),
        line: refused.line,
        code: refused.code,
    })
}

/// The plugin build script's whole job: checks the crate being built and fails the build
/// with every violation. It writes no directive to cargo, so cargo runs it again whenever
/// any file of the package changes. Input: the crate folder from the build environment.
/// Output: none; panics on a violation.
pub fn enforce_here() {
    let dir = std::env::var_os("CARGO_MANIFEST_DIR").expect("cargo sets the crate folder");
    if let Err(violations) = check_crate(Path::new(&dir)) {
        let lines: Vec<String> = violations
            .iter()
            .map(|violation| {
                format!(
                    "{}:{} {}",
                    violation.file.display(),
                    violation.line,
                    violation.code
                )
            })
            .collect();
        panic!("plugin check refused this crate:\n{}", lines.join("\n"));
    }
}

mod manifest;
mod source;
#[cfg(test)]
mod tests;

pub use source::STD_ALLOWED;
