use super::InstallError;
use std::path::{Path, PathBuf};

/// The Node.js LTS release setup installs when no usable Node is found (DEC-157).
pub const NODE_VERSION: &str = "26.13.1";

/// One pinned Node release archive: where it is downloaded from, the SHA-256 it must have,
/// and the one top folder every file in it sits in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodePin {
    pub url: String,
    pub sha256: String,
    pub top: String,
}

/// The engine's own Node: the `node` and `npm` programs under `<engine root>/setup/node` and
/// the engine-owned npm prefix `<engine root>/harnesses` npm harnesses install into.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeRuntime {
    pub node: PathBuf,
    pub npm: PathBuf,
    pub prefix: PathBuf,
}

impl NodeRuntime {
    /// The folders a harness run or an npm install puts first on its search path: Node's own
    /// bin folder, then the npm prefix's bin folder. Output: the two folders in that order.
    pub fn search_folders(&self) -> Vec<PathBuf> {
        Vec::new()
    }
}

/// The bounds an unpacked archive must stay within: how many entries, how many file bytes
/// and how many folders deep.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Limits {
    entries: usize,
    bytes: u64,
    depth: usize,
}

/// The pinned Node archive for one target triple. Input: the target, such as
/// `aarch64-apple-darwin`. Output: its pin, or none for a target with no Node pin.
pub fn pin(target: &str) -> Option<NodePin> {
    let _ = target;
    None
}

/// Reads the engine's own Node without running anything. Input: the engine root. Output:
/// the runtime when its node, npm and npm prefix are all in place, else none.
pub fn runtime(engine_root: &Path) -> Option<NodeRuntime> {
    let _ = engine_root;
    None
}

/// Installs this target's pinned Node into the engine root. Input: the engine root.
/// Output: the runtime, or the setup code of why it was not installed.
pub fn install(engine_root: &Path) -> Result<NodeRuntime, InstallError> {
    let _ = engine_root;
    Err(InstallError(super::PLATFORM_UNSUPPORTED))
}

/// Installs one Node archive pin into the engine root. Inputs: the engine root and the
/// pin. Output: the runtime, or the setup code of why it was not installed.
pub(crate) fn install_pin(engine_root: &Path, pin: &NodePin) -> Result<NodeRuntime, InstallError> {
    let _ = (engine_root, pin);
    Err(InstallError("setup_install_failed"))
}

/// Checks an unpacked tree before it is kept. Inputs: the tree's top folder and the
/// bounds. Output: Ok, or `setup_archive_unsafe`.
pub(crate) fn check_tree(top: &Path, limits: Limits) -> Result<(), InstallError> {
    let _ = (top, limits);
    Ok(())
}

#[cfg(test)]
#[path = "node_tests.rs"]
mod tests;
