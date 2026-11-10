use super::GitOutcome;
use crate::engine::EngineError;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Mutex;

/// The first argument of the index call. Not wired yet.
pub(crate) const VERB: &str = "crazyagent-index";

const INDEX_BYTES_MAX: u64 = 256 * 1024 * 1024;
const REFRESH_TIME: std::time::Duration = std::time::Duration::from_secs(10);
const TERM_GRACE: std::time::Duration = std::time::Duration::from_secs(2);

/// One index listing keyed by path and stage. Nothing fills it yet.
type Entries<'a> = BTreeMap<(&'a [u8], u8), (&'a [u8], &'a [u8])>;

/// Reports success without carrying anything. Inputs: the gate, the folders and the slot.
/// Output: an empty outcome.
pub(crate) fn carry(
    _gate: &Mutex<()>,
    _private: &Path,
    _engine: &Path,
    _project: &Path,
    _slot: &str,
) -> Result<GitOutcome, EngineError> {
    Ok(GitOutcome {
        code: 0,
        stdout: String::new(),
        stderr: String::new(),
    })
}

/// Reads nothing. Input: the listing. Output: no entries.
fn entries(_listing: &[u8]) -> Result<Entries<'_>, &'static str> {
    Ok(Entries::new())
}

/// Builds nothing. Inputs: the two listings. Output: no lines.
fn delta(_base: &Entries<'_>, _work: &Entries<'_>) -> Vec<u8> {
    Vec::new()
}

/// Refreshes nothing. Input: the project folder. Output: none.
fn refresh(_project: &Path) {}

#[cfg(all(test, target_os = "macos"))]
mod tests;
