use super::Tab;
use std::path::Path;

/// The most bytes of the evidence one writing run is shown.
pub const EVIDENCE_BYTES_MAX: usize = 96 * 1024;
/// The most bytes of competitor research text inside the evidence.
pub const RESEARCH_BYTES_MAX: usize = 24 * 1024;
/// The most file paths the evidence lists.
pub const TREE_PATHS_MAX: usize = 400;

/// One linked competitor research file as the run reads it: its path below the project
/// folder and its text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Research {
    pub path: String,
    pub text: String,
}

/// The evidence a writing run is shown. Inputs: the project folder, the research read and
/// the project's file paths. Output: the text, within [`EVIDENCE_BYTES_MAX`].
pub fn evidence(_folder: &Path, _research: &[Research], _tree: &[String]) -> String {
    String::new()
}

/// The prompt of a writing run. Inputs: the evidence, each current tab with its text and
/// whether the owner edited it, and whether competitor research exists. Output: the prompt.
pub fn prompt(
    _evidence: &str,
    _current: &[(Tab, String, bool)],
    _with_competitors: bool,
) -> String {
    String::new()
}

/// The tabs an agent's reply holds. Input: the reply. Output: each tab once with its text.
pub fn parse_reply(_reply: &str) -> Vec<(Tab, String)> {
    Vec::new()
}
