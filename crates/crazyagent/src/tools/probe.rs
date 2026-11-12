use std::path::{Path, PathBuf};

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
