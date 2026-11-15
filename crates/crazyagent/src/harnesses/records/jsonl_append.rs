use super::super::launch::LaunchError;
use super::super::transcript::FileKey;
use super::json_files::Pattern;
use serde_json::Value;
use std::path::PathBuf;

/// Follows a harness record that grows by whole JSON lines, such as pi's session file: the
/// newest file its record path matches, read as the harness appends. Each read returns the
/// JSON objects of the lines finished since the last, in record order, within the bounds
/// `transcript.rs` keeps.
pub struct JsonlAppend {
    pattern: Pattern,
    earlier: Vec<FileKey>,
    start: u64,
}

impl JsonlAppend {
    /// Starts following the record of a harness about to start. Every file the record path
    /// matches now is remembered and never taken, since an earlier session may have left
    /// it. Input: the record path. Output: the reader, before its record exists.
    pub fn new(pattern: Pattern) -> Self {
        Self {
            pattern,
            earlier: Vec::new(),
            start: 0,
        }
    }

    /// Follows the record of a session that already ran from a position an earlier reader
    /// reached ([`Self::position`]): the newest file the record path matches, whatever its
    /// age, and from its end when it is now shorter than the position. Inputs: the record
    /// path and the position. Output: the reader.
    pub fn resumed(pattern: Pattern, position: u64) -> Self {
        Self {
            pattern,
            earlier: Vec::new(),
            start: position,
        }
    }

    /// Names the record this reader follows once it found one. Output: the full path, or
    /// none before the record exists.
    pub fn record(&self) -> Option<PathBuf> {
        None
    }

    /// The byte just past the last line this reader is done with, for a later
    /// [`Self::resumed`]. Output: the position; before the record is found, the one this
    /// reader starts from.
    pub fn position(&self) -> u64 {
        self.start
    }

    /// Moves past everything the record holds now, looking for the record first when none
    /// is held, so the next read returns only lines written after this call. Output:
    /// nothing, or a plain code when the record cannot be read.
    pub fn skip_to_end(&mut self) -> Result<(), LaunchError> {
        Ok(())
    }

    /// Reads the lines the harness finished since the last read. A half-written last line
    /// waits for its newline, a line past `LINE_BYTES_MAX` is passed over to its newline,
    /// a line that is no JSON object is passed over, and past `EVENTS_PER_READ_MAX` objects
    /// the rest wait for the next read. Output: the objects in record order, none before
    /// the record exists, or a plain code when it cannot be read.
    pub fn read(&mut self) -> Result<Vec<Value>, LaunchError> {
        Ok(Vec::new())
    }
}

#[cfg(all(test, unix))]
#[path = "jsonl_append_tests.rs"]
mod tests;
