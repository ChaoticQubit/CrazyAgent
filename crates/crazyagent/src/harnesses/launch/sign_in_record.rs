use super::{LaunchError, failure};
use std::path::Path;

/// Refuses a folder that holds a native sign-in's own record, Codex's or Claude Code's,
/// as a file, a link or anything else, so a listed harness never runs where it could read
/// a sign-in's tokens. Input: the folder. Output: Ok, or
/// `launch_account_home_holds_sign_in`, also when the folder cannot be looked into.
pub(crate) fn holds_no_sign_in(folder: &Path) -> Result<(), LaunchError> {
    use crate::engine::bootstrap::{CLAUDE_RECORD, CODEX_RECORD};
    for record in [CODEX_RECORD, CLAUDE_RECORD] {
        match std::fs::symlink_metadata(folder.join(record)) {
            Err(problem) if problem.kind() == std::io::ErrorKind::NotFound => {}
            _ => return Err(failure("launch_account_home_holds_sign_in")),
        }
    }
    Ok(())
}
