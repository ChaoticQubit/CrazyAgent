use super::policy::{AgentAccess, ToolCacheScope};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Command;

const FOLDER: &str = "tool-cache";
const UV: &str = "uv";
const PNPM: &str = "pnpm-store";
const MARKER: &str = ".git";
const MARKER_TARGET: &str = "/dev/null";
const BUCKETS_MAX: usize = 16;

/// The names the priming process's environment holds, and no other (T8 C3).
pub const PRIMING_ENV: [&str; 6] = [
    "HOME",
    "UV_CACHE_DIR",
    "UV_NO_CONFIG",
    "UV_OFFLINE",
    "PATH",
    "LC_ALL",
];

/// Where a confined agent writes, which decides who may share its tool cache (T8 C2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheShape {
    ProjectRoot,
    Worktree,
    Clone,
    Teammate,
}

impl CacheShape {
    /// Every shape.
    pub const ALL: [Self; 4] = [
        Self::ProjectRoot,
        Self::Worktree,
        Self::Clone,
        Self::Teammate,
    ];
}

/// A tool cache ready for one launch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prepared {
    pub root: PathBuf,
    pub uv: bool,
    pub primed: bool,
}

/// The folder name of one launch's tool cache (T8, not written yet).
pub fn key(
    _scope: ToolCacheScope,
    _access: AgentAccess,
    _shape: CacheShape,
    _agent: &str,
) -> Option<String> {
    todo!("T8")
}

/// The folder holding one project's tool caches (T8, not written yet).
pub fn project_folder(_runtime: &Path, _project: &str) -> PathBuf {
    todo!("T8")
}

/// Whether a name is a plain folder name (T8, not written yet).
fn plain_name(_name: &str) -> bool {
    todo!("T8")
}

/// Makes or finds a launch's tool cache (T8, not written yet).
pub fn prepare(
    _runtime: &Path,
    _project: &str,
    _key: &str,
    _uv: Option<&Path>,
) -> Result<Prepared, &'static str> {
    todo!("T8")
}

/// Builds the priming process (T8, not written yet).
pub fn priming_command(_folder: &Path, _uv: &Path) -> Command {
    let _ = OsStr::new("");
    todo!("T8")
}

/// Reads the buckets uv made in a fresh cache (T8, not written yet).
fn buckets_in(_cache: &Path) -> Result<Vec<String>, &'static str> {
    todo!("T8")
}

#[cfg(test)]
mod tests;
