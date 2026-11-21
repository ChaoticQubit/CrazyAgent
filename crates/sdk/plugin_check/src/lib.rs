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

mod manifest;
mod source;
#[cfg(test)]
mod tests;

pub use source::STD_ALLOWED;
