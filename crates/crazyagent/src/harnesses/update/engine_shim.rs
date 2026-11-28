use super::Roots;
use crazyagent_win_sandbox::private;
use kernel_processes::batch;
use std::path::{Path, PathBuf};

/// The script npm's cmd-shim in the engine prefix starts, the real file its install method
/// is read from. Only a batch file right in the engine prefix is read, while the prefix is
/// held as a private folder and the shim is open as a private file with no link followed,
/// and no further than a cmd-shim can reach; its target must be its own real file below the
/// prefix's `node_modules`, reached through no link, and no batch file. Inputs: the PATH
/// entry's real file and the roots. Output: the script, none for anything else.
pub(super) fn target(real: &Path, roots: &Roots) -> Option<PathBuf> {
    let prefix = &roots.npm.as_ref()?.prefix;
    if !batch::batch_file(real) || real.parent() != Some(prefix.as_path()) {
        return None;
    }
    let _held = private::open_dir(prefix).ok()?;
    let shim = private::read_only(real).ok()?;
    let named = batch::held_shim_target(prefix, &shim)?;
    let script = kernel_sandbox::paths::canonical(&named).ok()?;
    let modules = prefix.join("node_modules");
    let inside = script == named && script.starts_with(&modules) && script != modules;
    (inside && !batch::batch_file(&script)).then_some(script)
}
