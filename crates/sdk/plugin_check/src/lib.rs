#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};

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

/// A plugin's `plugin.toml`: its id, tier word, kind, manifest version and the
/// permissions it asks the kernel for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    pub plugin: String,
    pub tier: String,
    pub kind: String,
    pub version: i64,
    pub permissions: Vec<String>,
}

/// Checks one plugin crate against every plugin rule. Input: the crate folder.
/// Output: nothing, or every violation sorted by file, line and code.
pub fn check_crate(dir: &Path) -> Result<(), Vec<Violation>> {
    let mut out = Vec::new();
    manifest::cargo(dir, &mut out);
    manifest::build_script(dir, &mut out);
    if let Err(violation) = manifest::plugin(dir) {
        out.push(violation);
    }
    source::sources(dir, &mut out);
    out.sort();
    out.dedup();
    if out.is_empty() { Ok(()) } else { Err(out) }
}

/// Checks every bundled plugin: each entry of the plugins folder is a tier folder `t1`,
/// `t2` or `t3`, and each entry of a tier is a crate passing `check_crate`. Input: the
/// plugins folder. Output: nothing, or every violation, files relative to that folder.
pub fn check_plugins(root: &Path) -> Result<(), Vec<Violation>> {
    let mut out = Vec::new();
    let mut seen = 0usize;
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
        }
    }
    out.sort();
    out.dedup();
    if out.is_empty() { Ok(()) } else { Err(out) }
}

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
    manifest::plugin(dir)
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

/// What a bounded read of one file found: its UTF-8 text, more bytes than the bound, or a
/// file that cannot be opened, read or decoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Bounded {
    Text(String),
    TooLarge,
    Unreadable,
}

/// Reads one file, taking at most `max` + 1 bytes, so a large file is refused without
/// holding it whole in memory. Inputs: the path and the bound. Output: the text,
/// `TooLarge` past the bound, or `Unreadable`.
pub(crate) fn read_bounded(path: &Path, max: usize) -> Bounded {
    use std::io::Read;
    let Ok(file) = std::fs::File::open(path) else {
        return Bounded::Unreadable;
    };
    let limit = u64::try_from(max)
        .expect("a bound fits u64")
        .saturating_add(1);
    let mut bytes = Vec::new();
    if file.take(limit).read_to_end(&mut bytes).is_err() {
        return Bounded::Unreadable;
    }
    assert!(bytes.len() <= max.saturating_add(1));
    if bytes.len() > max {
        return Bounded::TooLarge;
    }
    String::from_utf8(bytes).map_or(Bounded::Unreadable, Bounded::Text)
}

mod manifest;
mod source;
#[cfg(test)]
mod tests;

pub use source::STD_ALLOWED;
