use super::super::launch::LaunchError;
use super::super::transcript::{FileKey, LINE_BYTES_MAX};
use super::json_files::Pattern;
use serde_json::Value;
use std::path::PathBuf;

/// The most bytes of a rewritten record one read takes; a larger record is not read.
pub const DOCUMENT_BYTES_MAX: usize = LINE_BYTES_MAX;

/// Follows a harness record written anew as one JSON list each time, often as a new file
/// moved over the old: the newest file its record path matches, read whole after each
/// change and diffed by index. Each read returns the objects at indexes past the position,
/// in list order.
pub struct JsonRewrite {
    pattern: Pattern,
    earlier: Vec<FileKey>,
    position: usize,
}

impl JsonRewrite {
    /// Starts following the record of a harness about to start. Every file the record path
    /// matches now is remembered and never taken; since each rewrite is a new file, that
    /// is judged only until a record is first found. Input: the record path. Output: the
    /// reader, before its record exists.
    pub fn new(pattern: Pattern) -> Self {
        Self {
            pattern,
            earlier: Vec::new(),
            position: 0,
        }
    }

    /// Follows the record of a session that already ran from the position an earlier
    /// reader reached ([`Self::position`]): the newest file the record path matches,
    /// whatever its age. Inputs: the record path and the position. Output: the reader.
    pub fn resumed(pattern: Pattern, position: usize) -> Self {
        Self {
            pattern,
            earlier: Vec::new(),
            position,
        }
    }

    /// Names the record this reader follows once it found one. Output: the full path, or
    /// none before the record exists.
    pub fn record(&self) -> Option<PathBuf> {
        None
    }

    /// How many entries of the record's list this reader is done with, for a later
    /// [`Self::resumed`]. Output: the position.
    pub fn position(&self) -> usize {
        self.position
    }

    /// Moves past every entry the record holds now, so the next read returns only entries
    /// written after; a record caught half-written is skipped once it is whole. Output:
    /// nothing, or a plain code when the record cannot be read or is no list.
    pub fn skip_to_end(&mut self) -> Result<(), LaunchError> {
        Ok(())
    }

    /// Reads the record again when it changed and returns the entries past the position. A
    /// record that does not parse yet is no news; an entry already returned and changed
    /// since is not returned again; a shorter list moves the position back with a note;
    /// entries that are no JSON object are passed over; past `EVENTS_PER_READ_MAX` the
    /// rest wait. Output: the objects, or a plain code for a record that is no JSON list.
    pub fn read(&mut self) -> Result<Vec<Value>, LaunchError> {
        Ok(Vec::new())
    }
}

#[cfg(all(test, unix))]
#[path = "json_rewrite_tests.rs"]
mod tests;
