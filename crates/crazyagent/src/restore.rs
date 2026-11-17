use std::path::Path;
use std::time::SystemTime;

/// The code a client gives for an engine it does not start while a restore runs.
pub const ENGINE_RESTORING: &str = "engine_restoring";
/// The code a client gives for an engine it does not start after a restore brought the 2.4
/// data back.
pub const ENGINE_RESTORED: &str = "engine_restored";

/// How the owner's last restore of the pinned 2.4 backup stands, as the record beside
/// engine.db says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RestoreState {
    Pending,
    Restored { copy: String },
    Failed { code: String },
}

/// How the last restore of the engine at `root` stands, read without an engine. Input: the
/// engine root. Output: the state, or none.
pub fn state(root: &Path) -> Option<RestoreState> {
    let _ = root;
    None
}

/// Why a client must not start the engine at `root` by itself. Input: the engine root.
/// Output: the code, or none.
pub fn blocks_start(root: &Path) -> Option<&'static str> {
    let _ = root;
    None
}

/// Until when the owner can restore the pinned 2.4 backup. Input: the engine root.
/// Output: the pin's end, or none.
pub fn restorable_until(root: &Path) -> Option<SystemTime> {
    let _ = root;
    None
}

/// [`after_stop`] with the request given. Inputs: whether a restore was admitted, the engine
/// root and the settings files. Output: Ok, or the failure.
fn after_stop_when(
    requested: bool,
    root: &Path,
    global: &Path,
    project: &Path,
) -> Result<(), String> {
    let _ = (requested, root, global, project);
    Ok(())
}

#[cfg(all(test, unix))]
mod tests;
