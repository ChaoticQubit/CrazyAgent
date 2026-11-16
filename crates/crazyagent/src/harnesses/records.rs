use super::launch::{LaunchError, failure};
use super::manifest::{RecordKind, Rule};
use super::registry::registry;

pub mod json_files;
pub mod json_rewrite;
pub mod jsonl_append;

/// The record readers the engine runs today: the Codex rollouts, the Claude Code session
/// transcript, and the JSON lines and rewritten JSON records a manifest's terminal settings
/// name. Each other shared reader kind a manifest names joins this list when its task lands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reader {
    CodexRollouts,
    ClaudeSession,
    JsonlAppend,
    JsonRewrite,
}

/// The record of one run of a manifest harness as a launch follows it: the harness, its
/// record kind (jsonl-append or json-rewrite), the record path below the run's private
/// home, the rules that turn the record's objects into facts, and the session id the path
/// was filled with, none for a harness that picks its own id.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Records {
    pub harness: String,
    pub kind: RecordKind,
    pub pattern: json_files::Pattern,
    pub rules: Vec<Rule>,
    pub session: Option<String>,
}

impl Records {
    /// Names the conversation a found record holds: the id the record path was filled
    /// with, else the id the harness picked, read back from the record's place (see
    /// [`json_files::Pattern::session_of`]). Input: the record's full path. Output: the id,
    /// or none while the harness's own id cannot be told.
    pub fn session_of(&self, record: &std::path::Path) -> Option<String> {
        self.session
            .clone()
            .or_else(|| self.pattern.session_of(record))
    }
}

/// The one dispatch point of live terminal records: the reader the engine runs for a record
/// kind. Input: the kind a manifest names. Output: the reader, or
/// `launch_harness_unsupported` for a harness with no record and for every kind whose
/// reader has not landed.
pub fn of(kind: RecordKind) -> Result<Reader, LaunchError> {
    match kind {
        RecordKind::CodexRollouts => Ok(Reader::CodexRollouts),
        RecordKind::ClaudeSession => Ok(Reader::ClaudeSession),
        RecordKind::JsonlAppend => Ok(Reader::JsonlAppend),
        RecordKind::JsonRewrite => Ok(Reader::JsonRewrite),
        RecordKind::SqlitePoll => Err(failure("launch_harness_unsupported")),
        RecordKind::MarkdownHistory => Err(failure("launch_harness_unsupported")),
        RecordKind::Screen => Err(failure("launch_harness_unsupported")),
        RecordKind::Unavailable => Err(failure("launch_harness_unsupported")),
    }
}

/// Picks the reader of a live terminal launch of one harness, from the record kind its
/// manifest names. Input: a catalog id or an alias, such as `claude`. Output: the reader,
/// or `launch_harness_unsupported` for a name outside the catalog or a kind [`of`] refuses.
pub fn reader(harness: &str) -> Result<Reader, LaunchError> {
    registry()
        .get(harness)
        .ok_or_else(|| failure("launch_harness_unsupported"))
        .and_then(|found| of(found.record()))
}

/// Picks the reader of a record kept under the harness name stored with it. A stored record
/// names its harness by catalog id; an alias stored there reads as no reader, as it always
/// has. Input: the stored name. Output: the reader, or none.
pub fn stored(harness: &str) -> Option<Reader> {
    let found = registry()
        .get(harness)
        .filter(|found| found.manifest().id == harness)?;
    of(found.record()).ok()
}

#[cfg(test)]
#[path = "records_tests.rs"]
mod tests;
