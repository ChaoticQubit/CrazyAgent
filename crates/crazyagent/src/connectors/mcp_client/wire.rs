use std::process::Command;

/// Builds the curl command without running it. Input: the one scheme it may speak.
/// Output: the command.
pub(super) fn command(_scheme: &str) -> Command {
    Command::new("/usr/bin/false")
}
