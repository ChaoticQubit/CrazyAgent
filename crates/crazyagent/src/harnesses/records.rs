use super::launch::{LaunchError, failure};
use super::manifest::RecordKind;
use super::registry::registry;

pub mod json_files;
pub mod json_rewrite;
pub mod jsonl_append;

/// The record readers the engine runs today: the Codex rollouts and the Claude Code session
/// transcript. Each shared reader kind a manifest names joins this list when its task lands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reader {
    CodexRollouts,
    ClaudeSession,
}

/// The one dispatch point of live terminal records: the reader the engine runs for a record
/// kind. Input: the kind a manifest names. Output: the reader, or
/// `launch_harness_unsupported` for a harness with no record and for every kind whose
/// reader has not landed.
pub fn of(kind: RecordKind) -> Result<Reader, LaunchError> {
    match kind {
        RecordKind::CodexRollouts => Ok(Reader::CodexRollouts),
        RecordKind::ClaudeSession => Ok(Reader::ClaudeSession),
        RecordKind::JsonlAppend => Err(failure("launch_harness_unsupported")),
        RecordKind::JsonRewrite => Err(failure("launch_harness_unsupported")),
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
