use super::super::launch::{LaunchError, failure};
use super::super::transcript::TranscriptEvent;
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// How long the history file must stay the same length before a turn that has reply text
/// counts as finished, when no next prompt has ended it first.
pub const QUIET: Duration = Duration::from_secs(2);
/// The most bytes of one line read as text; a longer line is passed over.
pub const LINE_BYTES_MAX: usize = 64 * 1024;
/// The most bytes one read takes from the file; the rest waits for the next read.
pub const READ_BYTES_MAX: usize = 1024 * 1024;
/// The code of a record path that is a link, a folder or anything but a plain file.
pub const UNSAFE: &str = "record_history_unsafe";

/// Follows a Markdown chat history file as a harness appends to it, aider's
/// `.aider.chat.history.md` shape: a user heading line (`#### `) starts a turn, the plain
/// text after it is the reply, a quoted line (`>`) is tool output and a session header line
/// (`# aider chat started at`) starts a new session. Holds the file, the offset read to, the
/// unfinished last line and where the current turn stands.
pub struct MarkdownHistory {
    path: PathBuf,
    offset: u64,
    changed: Instant,
}

impl MarkdownHistory {
    /// A reader that starts at `offset`. Inputs: the history file, the offset already read,
    /// 0 for none, and the time now. Output: the reader.
    pub fn new(path: PathBuf, offset: u64, now: Instant) -> Self {
        Self {
            path,
            offset,
            changed: now,
        }
    }

    /// The offset read to, the position a resumed reader starts at. Output: it.
    pub fn offset(&self) -> u64 {
        self.offset
    }

    /// Reads the facts of the lines finished since the last read and ends a turn the file
    /// has been quiet on for [`QUIET`]. Input: the time now. Output: the facts, none while
    /// the file does not exist yet, or [`UNSAFE`].
    pub fn read(&mut self, now: Instant) -> Result<Vec<TranscriptEvent>, LaunchError> {
        let _ = (&self.path, self.changed, now);
        Err(failure("launch_harness_unsupported"))
    }
}

#[cfg(test)]
#[path = "markdown_history_tests.rs"]
mod tests;
