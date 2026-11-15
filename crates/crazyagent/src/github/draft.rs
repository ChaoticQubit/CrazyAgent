use crate::engine::EngineError;
use std::path::Path;

#[cfg(all(test, target_os = "macos"))]
mod draft_tests;

pub(super) const BASELINE: [&str; 4] = [".env*", "*.pem", "*.key", ".DS_Store"];
pub(super) const NOT_ASKED: &str = "github_publish_draft_not_asked";
const LISTED_MAX: usize = 2000;
const DEPTH_MAX: usize = 6;
const DRAFT_LINES_MAX: usize = 200;
const PROMPT_IGNORE_LINES_MAX: usize = 200;
const INSTRUCTION: &str = "";
const LEFT_OUT: &str = "";

/// Lists the names in a project for the agent that drafts its .gitignore. Input: the
/// project folder. Output: the names, and whether some were left out at the bounds.
pub(super) fn listing(_project: &Path) -> Result<(Vec<String>, bool), EngineError> {
    Ok((Vec::new(), false))
}

/// Builds the prompt the drafting agent gets. Inputs: the names, whether some were left
/// out and the project's .gitignore text. Output: the prompt.
pub(super) fn prompt(_listed: &[String], _cut: bool, _existing: Option<&str>) -> String {
    String::new()
}

/// Reads the patterns out of the drafting agent's reply. Input: the reply. Output: the
/// patterns kept.
pub(super) fn reply_lines(_reply: &str) -> Vec<String> {
    Vec::new()
}

/// Joins the baseline and the agent's patterns into the lines a publish proposes.
/// Inputs: the agent's patterns and the project's .gitignore text. Output: the lines.
pub(super) fn merged(_agent: &[String], _existing: Option<&str>) -> Vec<String> {
    BASELINE.map(String::from).to_vec()
}
