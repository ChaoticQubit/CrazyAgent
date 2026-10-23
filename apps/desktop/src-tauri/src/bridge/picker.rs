use super::{BridgeError, failure};
use std::process::Command;

const OSASCRIPT: &str = "/usr/bin/osascript";
const REPLY_BYTES_MAX: usize = 64 * 1024;
const PATHS_MAX: usize = 64;
const CANCELED: &str = "(-128)";

/// The macOS folder chooser for a skill folder. Fixed text: nothing the owner or the web view
/// sends is ever placed into a script.
pub const FOLDER_SCRIPT: &str = "POSIX path of (choose folder with prompt \"Choose a skill folder. It holds a SKILL.md file.\")";

/// The macOS file chooser for project documents, one POSIX path per line. Fixed text.
pub const DOCUMENTS_SCRIPT: &str = "set picked to choose file with prompt \"Choose documents to add\" with multiple selections allowed
set reply to \"\"
repeat with chosen in picked
set reply to reply & POSIX path of chosen & linefeed
end repeat
return reply";

/// What the owner did in a native chooser: closed it, or chose absolute paths.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pick {
    Canceled,
    Paths(Vec<String>),
}

/// Reads an osascript chooser reply. Inputs: whether osascript exited successfully, its
/// standard output and standard error. Output: Canceled, the chosen absolute paths in order
/// (folders without a trailing slash), or `picker_failed` for anything unexpected.
pub fn read_reply(success: bool, stdout: &[u8], stderr: &[u8]) -> Result<Pick, BridgeError> {
    if !success {
        let canceled = String::from_utf8_lossy(stderr).contains(CANCELED);
        return if canceled {
            Ok(Pick::Canceled)
        } else {
            Err(failure("picker_failed"))
        };
    }
    if stdout.len() > REPLY_BYTES_MAX {
        return Err(failure("picker_failed"));
    }
    let text = std::str::from_utf8(stdout).map_err(|_| failure("picker_failed"))?;
    let mut paths = Vec::new();
    for line in text.split('\n').filter(|line| !line.is_empty()) {
        if !line.starts_with('/') || line.contains('\0') || paths.len() == PATHS_MAX {
            return Err(failure("picker_failed"));
        }
        let trimmed = line.trim_end_matches('/');
        paths.push(if trimmed.is_empty() {
            "/".to_owned()
        } else {
            trimmed.to_owned()
        });
    }
    assert!(paths.len() <= PATHS_MAX);
    Ok(if paths.is_empty() {
        Pick::Canceled
    } else {
        Pick::Paths(paths)
    })
}

/// Shows one fixed macOS chooser and waits for the owner without blocking the async runtime.
/// Input: one of the fixed scripts. Output: the owner's pick or `picker_failed`.
pub async fn choose(script: &'static str) -> Result<Pick, BridgeError> {
    assert!(script == FOLDER_SCRIPT || script == DOCUMENTS_SCRIPT);
    let output =
        tokio::task::spawn_blocking(move || Command::new(OSASCRIPT).arg("-e").arg(script).output())
            .await
            .map_err(|_| failure("picker_failed"))?
            .map_err(|_| failure("picker_failed"))?;
    read_reply(output.status.success(), &output.stdout, &output.stderr)
}
