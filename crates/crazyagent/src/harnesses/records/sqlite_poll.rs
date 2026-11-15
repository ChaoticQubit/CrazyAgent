use super::super::launch::{LaunchError, failure};
use super::super::transcript::TranscriptEvent;
use std::path::PathBuf;

/// The most reads in a row a busy or locked database may answer with no events before the
/// reader refuses it with [`BUSY`].
pub const BUSY_READS_MAX: usize = 50;
/// The most rows one read takes; the rest wait for the next read.
pub const ROWS_PER_READ_MAX: usize = 4096;
/// The most bytes of text one row gives; longer text is cut at a character boundary.
pub const ROW_TEXT_BYTES_MAX: usize = 64 * 1024;
/// The most bytes of text one read gives; the row that would pass it waits for the next read.
pub const READ_TEXT_BYTES_MAX: usize = 1024 * 1024;
/// The largest database the reader opens.
pub const DATABASE_BYTES_MAX: u64 = 1024 * 1024 * 1024;
/// The code of a record path that is a link, a folder or anything but a plain file.
pub const UNSAFE: &str = "record_database_unsafe";
/// The code of a database that stayed busy or locked for [`BUSY_READS_MAX`] reads.
pub const BUSY: &str = "record_database_busy";
/// The code of a database whose layout or schema version no query set here knows.
pub const VERSION_UNKNOWN: &str = "record_version_unknown";
/// The code of a file that is not a database SQLite can read.
pub const UNREADABLE: &str = "record_database_unreadable";
/// The code of a database larger than [`DATABASE_BYTES_MAX`].
pub const TOO_LARGE: &str = "record_database_too_large";

/// A read-only query set for one known harness database, written as engine code and never
/// read from a manifest. `tables` must all be real tables before any query runs, `version`
/// gives one integer that must be in `versions`, and `facts` takes the last sequence read as
/// `?1` and a row limit as `?2` and gives `(sequence, fact, text)` rows in sequence order.
pub struct Schema {
    pub tables: &'static [&'static str],
    pub version: &'static str,
    pub versions: &'static [i64],
    pub facts: &'static str,
}

/// Follows one harness session database by polling it read-only. Each read gives the facts
/// of the rows past the last sequence read. Holds the database path, its query set, the last
/// sequence read and how many reads in a row found it busy.
pub struct SqlitePoll {
    path: PathBuf,
    schema: &'static Schema,
    after: i64,
    busy: usize,
}

impl SqlitePoll {
    /// A reader that starts past sequence `after`. Inputs: the database path, its query set
    /// and the last sequence already read, 0 for none. Output: the reader.
    pub fn new(path: PathBuf, schema: &'static Schema, after: i64) -> Self {
        Self {
            path,
            schema,
            after,
            busy: 0,
        }
    }

    /// The last sequence read, the position a resumed reader starts past. Output: it.
    pub fn after(&self) -> i64 {
        self.after
    }

    /// Reads the facts of the rows written since the last read. Output: the facts, none while
    /// the database does not exist yet or is busy, or a refusal code from this module.
    pub fn read(&mut self) -> Result<Vec<TranscriptEvent>, LaunchError> {
        let _ = (&self.path, self.schema, self.busy);
        Err(failure("launch_harness_unsupported"))
    }
}

/// The plain sentence the apps show for one refusal code of this reader. Input: the code.
/// Output: the sentence, or none for a code this reader does not give.
pub fn sentence(code: &str) -> Option<&'static str> {
    let _ = code;
    None
}

#[cfg(test)]
#[path = "sqlite_poll_tests.rs"]
mod tests;
