use super::ProjectSandbox;
use kernel_sandbox::shared_folders::{NEEDS_MACOS, REFUSED, SharedFolders, plan};
use std::path::Path;

/// Gives one confined launch's sandbox the shared folders choice for the project folder
/// `root`, which it must already read: the secret names kept closed there outside its own
/// writable roots with the owner's shared-back secrets open again, then [`plan`]'s keep-outs
/// and writable folders. A Full access launch runs with no profile, so its caller skips this.
/// Takes the sandbox, the canonical project folder and the choice. Returns the sandbox, or a
/// plain refusal code: [`NEEDS_MACOS`] for a wider choice off macOS, [`plan`]'s codes, or
/// [`REFUSED`] for a grant the sandbox would not hold.
pub fn apply(
    sandbox: ProjectSandbox,
    root: &Path,
    shared: &SharedFolders,
) -> Result<ProjectSandbox, &'static str> {
    if cfg!(not(target_os = "macos")) && !shared.plain_reach() {
        return Err(NEEDS_MACOS);
    }
    let plan = plan(&super::ACCESS_HOOKS, root, shared)?;
    let mut sandbox = sandbox
        .keep_project_secrets(root, &plan.revealed)
        .map_err(|_| REFUSED)?;
    for path in &plan.kept_out {
        sandbox = sandbox.keep_out(path).map_err(|_| REFUSED)?;
    }
    if let Some(open) = &plan.open_tops {
        sandbox = sandbox.open_only_tops(open).map_err(|_| REFUSED)?;
    }
    for folder in &plan.writes {
        sandbox = sandbox.allow_shared_write(folder).map_err(|_| REFUSED)?;
    }
    Ok(sandbox)
}

#[cfg(test)]
mod tests;
