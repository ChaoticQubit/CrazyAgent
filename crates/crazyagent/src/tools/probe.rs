use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

#[cfg(all(test, unix))]
mod tests;

/// How long the login shell may take to print its PATH.
pub(crate) const SHELL_LIMIT: Duration = Duration::from_secs(10);
/// How long npm may take to print its global prefix.
pub(crate) const NPM_LIMIT: Duration = Duration::from_secs(10);
/// How long a program may take to print its version.
pub(crate) const VERSION_LIMIT: Duration = Duration::from_secs(5);
/// The most folders the probes keep.
pub(crate) const FOLDERS_MAX: usize = 64;
/// The most bytes of one kept version.
pub(crate) const VERSION_BYTES_MAX: usize = 64;
/// The line the login shell prints before its PATH.
pub(crate) const BEGIN: &str = "CRAZYAGENT-PATH-BEGIN";
/// The line the login shell prints after its PATH.
pub(crate) const END: &str = "CRAZYAGENT-PATH-END";

/// The folders the background probes found: npm's global prefix `bin` of the npm the
/// resolver finds and the login shell's PATH folders. Answers at once from what the probes
/// cached, starting them in the background, and is empty until they finish.
/// Output: the absolute folders.
pub fn folders() -> Vec<PathBuf> {
    Vec::new()
}

/// A program's version as it reports it, read once in the background with a time limit and
/// cached. Input: the program's real file. Output: the version, or none while unknown,
/// which never counts as missing.
pub fn version(_program: &Path) -> Option<String> {
    None
}

/// Runs a command with no input, standard error dropped, its own process group and a time
/// limit; at the limit, or once it exits, its whole group is killed, so nothing it started
/// can hold the output open. Inputs: the command and the limit. Output: at most 64 KiB of
/// standard output when it exits with success in time, else none.
pub(crate) fn run_timed(command: &mut Command, limit: Duration) -> Option<Vec<u8>> {
    let _ = (command, limit);
    unimplemented!("probe run_timed")
}

/// The PATH a login shell prints between [`BEGIN`] and [`END`], ignoring whatever its
/// startup files print around it. Input: the shell's output. Output: the PATH, or none.
pub(crate) fn between_markers(output: &[u8]) -> Option<OsString> {
    let _ = output;
    unimplemented!("probe between_markers")
}

/// Runs the owner's login shell, interactive so the files that set up version managers are
/// read, with the environment cleared to HOME, USER, LOGNAME, SHELL, TERM and a system PATH,
/// and reads the PATH it ends with. Inputs: the shell, HOME, the user name and the limit.
/// Output: the PATH, or none.
pub(crate) fn login_path(
    shell: &Path,
    home: &Path,
    user: &str,
    limit: Duration,
) -> Option<OsString> {
    let _ = (shell, home, user, limit);
    unimplemented!("probe login_path")
}

/// The folders of a PATH value a launch may search: absolute plain folders of bounded
/// length that exist, in no `.crazyagent` folder and in no project (a folder at or below one
/// holding `.crazyagent`, HOME and `/` not counted), that only this user and the system can
/// change, each once, at most [`FOLDERS_MAX`]. Inputs: the PATH value and HOME. Output: the
/// folders in order.
pub(crate) fn kept(path: &OsStr, home: Option<&Path>) -> Vec<PathBuf> {
    let _ = (path, home);
    unimplemented!("probe kept")
}

/// The `bin` folder of the npm global prefix npm prints. Input: npm's output. Output: the
/// folder, or none for anything but one absolute path.
pub(crate) fn prefix_bin(output: &[u8]) -> Option<PathBuf> {
    let _ = output;
    unimplemented!("probe prefix_bin")
}

/// Asks a resolved npm for its global prefix with the environment cleared to HOME, USER,
/// LOGNAME and a system PATH, its interpreter named by path. Inputs: npm, HOME, the user
/// name and the limit. Output: the prefix's `bin` folder, or none.
pub(crate) fn npm_bin(
    npm: &super::Resolved,
    home: &Path,
    user: &str,
    limit: Duration,
) -> Option<PathBuf> {
    let _ = (npm, home, user, limit);
    unimplemented!("probe npm_bin")
}

/// A version from a version report such as `tmux 3.4` or `codex-cli v0.164.0`: the first
/// word holding a digit, without a leading `v`, made of digits, letters, `.`, `-` and `+`,
/// at most [`VERSION_BYTES_MAX`] bytes. Input: the report. Output: the version, or none.
pub(crate) fn reported(output: &[u8]) -> Option<String> {
    let _ = output;
    unimplemented!("probe reported")
}

/// Runs a program with `--version`, through its interpreter when it is a script, with the
/// environment cleared to HOME and a system PATH. Inputs: the program's file and the limit.
/// Output: the version, or none.
pub(crate) fn probe_version(program: &Path, limit: Duration) -> Option<String> {
    let _ = (program, limit);
    unimplemented!("probe probe_version")
}
