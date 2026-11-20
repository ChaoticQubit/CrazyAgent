use std::ffi::OsString;
use std::path::Path;
use std::process::Command;

/// Gives a sign-in login child its one shape, so every login child command is built the same
/// way. Not built yet (N4 red rows): it only clears the environment. Inputs: the command, the
/// profile home and the harness's config variable. Output: none.
pub(super) fn login_child(command: &mut Command, home: &Path, config: &'static str) {
    let _ = (home, config);
    command.env_clear();
}

/// The environment a sign-in login child starts with. Not built yet (N4 red rows): none.
/// Inputs: the profile home and the harness's config variable. Output: the names and values.
pub(super) fn login_env(home: &Path, config: &'static str) -> Vec<(&'static str, OsString)> {
    let _ = (home, config);
    Vec::new()
}
