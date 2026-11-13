use super::{Places, Refused};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const TOOL_FIX_REFUSED: &str = "tool_fix_refused";
pub const TOOL_FIX_TOO_LARGE: &str = "tool_fix_too_large";
pub const TOOL_FIX_FAILED: &str = "tool_fix_failed";
/// The most entries one fix looks at before it stops.
pub const ENTRIES_MAX: u64 = 200_000;
/// The deepest folder below the target one fix walks into.
pub const DEPTH_MAX: usize = 64;

/// What one permission fix changes: a folder walked as a whole tree, or one folder or file
/// alone.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Target {
    pub path: PathBuf,
    pub recursive: bool,
}

impl Target {
    /// The command the owner can run for the same change, the path quoted for the shell.
    /// Output: the command.
    pub fn command(&self) -> String {
        String::new()
    }
}

/// What a fix did: entries whose group or other write was removed, entries passed over
/// because another user owns them, links left as they are, other entries passed over, and
/// changed files that also have a name outside the target.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub changed: u64,
    pub skipped_foreign: u64,
    pub skipped_links: u64,
    pub skipped_other: u64,
    pub shared: u64,
}

/// How far one walk may go: entries looked at and folders below the target.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Bounds {
    entries: u64,
    depth: usize,
}

impl Default for Bounds {
    fn default() -> Self {
        Self {
            entries: ENTRIES_MAX,
            depth: DEPTH_MAX,
        }
    }
}

/// The target a permission fix would change for a file the resolver passed over.
/// Inputs: the refusal and the places it was found with. Output: the target, or none.
pub fn target(refused: &Refused, places: &Places) -> Option<Target> {
    let _ = (refused, places);
    None
}

/// Whether a permission fix may change a path. Inputs: the path and the places. Output: the
/// answer.
fn allowed(path: &Path, places: &Places) -> bool {
    let _ = (path, places);
    true
}

/// Removes group and other write from a target. Inputs: the target and the places. Output:
/// the report, or the code.
pub fn fix(target: &Target, places: &Places) -> Result<Report, &'static str> {
    let _ = (target, places);
    Err(TOOL_FIX_FAILED)
}

/// Removes group and other write from a tree in the engine's own setup folder. Inputs: the
/// setup folder and the tree. Output: the report, or the code.
pub(crate) fn tighten_setup(setup: &Path, tree: &Path) -> Result<Report, &'static str> {
    let _ = (setup, tree);
    Err(TOOL_FIX_FAILED)
}

/// Walks a target below an anchor folder. Inputs: the anchor, the target, whether it is
/// walked whole, the bounds and who counts as the owner. Output: the report, or the code.
fn walk(
    anchor: &Path,
    target: &Path,
    recursive: bool,
    bounds: Bounds,
    owned: &dyn Fn(u32, u64) -> bool,
) -> Result<Report, &'static str> {
    let _ = (anchor, target, recursive, bounds, owned);
    Err(TOOL_FIX_FAILED)
}

#[cfg(all(test, unix))]
mod tests;
