use super::super::launch::{LaunchError, failure};
use super::super::transcript::{TranscriptEvent, open_plain};
use rusqlite::types::ValueRef;
use rusqlite::{Connection, ErrorCode, OpenFlags};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

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
        let Some(mode) = mode(&self.path)? else {
            return Ok(Vec::new());
        };
        match self.read_rows(mode) {
            Ok(events) => {
                self.busy = 0;
                Ok(events)
            }
            Err(Stop::Busy) => {
                self.busy += 1;
                if self.busy > BUSY_READS_MAX {
                    return Err(failure(BUSY));
                }
                Ok(Vec::new())
            }
            Err(Stop::Refused(code)) => Err(failure(code)),
        }
    }

    fn read_rows(&mut self, mode: Mode) -> Result<Vec<TranscriptEvent>, Stop> {
        let immutable = mode == Mode::Immutable;
        let sort = |error: rusqlite::Error| stop(&error, immutable);
        let connection = open(&self.path, mode).map_err(sort)?;
        for table in self.schema.tables {
            let kind: String = connection
                .query_row(
                    "SELECT type FROM sqlite_master WHERE name = ?1",
                    [table],
                    |row| row.get(0),
                )
                .map_err(sort)?;
            if kind != "table" {
                return Err(Stop::Refused(VERSION_UNKNOWN));
            }
        }
        let version: i64 = connection
            .query_row(self.schema.version, [], |row| row.get(0))
            .map_err(sort)?;
        if !self.schema.versions.contains(&version) {
            return Err(Stop::Refused(VERSION_UNKNOWN));
        }
        let mut statement = connection.prepare(self.schema.facts).map_err(sort)?;
        let limit = i64::try_from(ROWS_PER_READ_MAX).unwrap_or(i64::MAX);
        let mut rows = statement
            .query(rusqlite::params![self.after, limit])
            .map_err(sort)?;
        let mut events = Vec::new();
        let (mut last, mut taken, mut text) = (self.after, 0, 0);
        while let Some(row) = rows.next().map_err(sort)? {
            taken += 1;
            assert!(taken <= ROWS_PER_READ_MAX);
            let sequence: i64 = row.get(0).map_err(sort)?;
            if sequence <= last {
                continue;
            }
            let body = match row.get_ref(2).map_err(sort)? {
                ValueRef::Text(bytes) => Some(bounded(bytes)),
                _ => None,
            };
            let event = match row.get_ref(1).map_err(sort)? {
                ValueRef::Text(kind) => fact(kind, body),
                _ => None,
            };
            let size = match &event {
                Some(TranscriptEvent::Text { text }) => text.len(),
                Some(TranscriptEvent::ToolUse { name }) => name.len(),
                _ => 0,
            };
            if text + size > READ_TEXT_BYTES_MAX {
                break;
            }
            text += size;
            last = sequence;
            events.extend(event);
        }
        assert!(last >= self.after && text <= READ_TEXT_BYTES_MAX);
        assert!(events.len() <= ROWS_PER_READ_MAX);
        self.after = last;
        Ok(events)
    }
}

/// How one read opens the database: read-only through the WAL index the harness keeps, or,
/// for a WAL database no process has open, as immutable, so the read makes no WAL or index
/// file beside it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    ReadOnly,
    Immutable,
}

/// Why one read gave no facts: the database was busy or locked, or a refusal code.
enum Stop {
    Busy,
    Refused(&'static str),
}

/// Checks the record path before SQLite opens it: a plain file, within the size bound, that
/// starts with the SQLite header, and any WAL or index file beside it a plain file of its
/// own. Input: the database path. Output: how to open it, none while nothing or an empty
/// file is there, or a refusal code.
fn mode(path: &Path) -> Result<Option<Mode>, LaunchError> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(failure(UNREADABLE)),
    };
    if !metadata.file_type().is_file() {
        return Err(failure(UNSAFE));
    }
    if metadata.len() > DATABASE_BYTES_MAX {
        return Err(failure(TOO_LARGE));
    }
    let file = open_plain(path).ok_or_else(|| failure(UNSAFE))?;
    let mut header = Vec::with_capacity(100);
    file.take(100)
        .read_to_end(&mut header)
        .map_err(|_| failure(UNREADABLE))?;
    if header.is_empty() {
        return Ok(None);
    }
    if header.len() < 100 || !header.starts_with(b"SQLite format 3\0") {
        return Err(failure(UNREADABLE));
    }
    let mut beside = 0;
    for suffix in ["-wal", "-shm"] {
        let mut name = path.as_os_str().to_owned();
        name.push(suffix);
        match std::fs::symlink_metadata(&name) {
            Ok(metadata) if plain(&metadata) => beside += 1,
            Ok(_) => return Err(failure(UNSAFE)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(failure(UNREADABLE)),
        }
    }
    let wal = header[18] == 2 || header[19] == 2;
    Ok(Some(if wal && beside == 0 {
        Mode::Immutable
    } else {
        Mode::ReadOnly
    }))
}

#[cfg(unix)]
fn plain(metadata: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    metadata.file_type().is_file() && metadata.nlink() == 1
}

#[cfg(not(unix))]
fn plain(metadata: &std::fs::Metadata) -> bool {
    metadata.file_type().is_file()
}

/// Opens the database for one read with every write and schema code path off: read-only,
/// no link followed, defensive, no busy wait, query only, no schema functions trusted, cell
/// sizes checked and no memory map. Inputs: the path and the mode. Output: the connection.
fn open(path: &Path, mode: Mode) -> Result<Connection, rusqlite::Error> {
    let mut uri = String::from("file:");
    for byte in path.to_string_lossy().bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~/".contains(&byte) {
            uri.push(char::from(byte));
        } else {
            uri.push_str(&format!("%{byte:02X}"));
        }
    }
    uri.push_str(match mode {
        Mode::ReadOnly => "?mode=ro",
        Mode::Immutable => "?mode=ro&immutable=1",
    });
    let connection = Connection::open_with_flags(
        uri,
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_URI
            | OpenFlags::SQLITE_OPEN_NOFOLLOW
            | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    connection.set_db_config(rusqlite::config::DbConfig::SQLITE_DBCONFIG_DEFENSIVE, true)?;
    connection.busy_timeout(Duration::ZERO)?;
    connection.execute_batch(
        "PRAGMA query_only = 1; PRAGMA trusted_schema = 0; PRAGMA cell_size_check = 1; PRAGMA mmap_size = 0;",
    )?;
    Ok(connection)
}

/// Sorts one SQLite failure: busy or locked, and under `immutable` a page a writer changed
/// mid-read, is busy; a query the database's layout does not fit is an unknown version; the
/// rest is unreadable. Inputs: the failure and the mode. Output: the stop.
fn stop(error: &rusqlite::Error, immutable: bool) -> Stop {
    match error {
        rusqlite::Error::SqliteFailure(sqlite, _) => match sqlite.code {
            ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked => Stop::Busy,
            ErrorCode::DatabaseCorrupt | ErrorCode::NotADatabase if immutable => Stop::Busy,
            ErrorCode::Unknown => Stop::Refused(VERSION_UNKNOWN),
            _ => Stop::Refused(UNREADABLE),
        },
        rusqlite::Error::QueryReturnedNoRows
        | rusqlite::Error::InvalidColumnType(..)
        | rusqlite::Error::InvalidColumnIndex(_)
        | rusqlite::Error::InvalidParameterCount(..)
        | rusqlite::Error::IntegralValueOutOfRange(..)
        | rusqlite::Error::FromSqlConversionFailure(..) => Stop::Refused(VERSION_UNKNOWN),
        _ => Stop::Refused(UNREADABLE),
    }
}

/// The fact one row names. Inputs: the row's fact word and its bounded text. Output: the
/// fact, or none for a word this reader does not know or a text fact with no text.
fn fact(kind: &[u8], body: Option<String>) -> Option<TranscriptEvent> {
    let body = body.filter(|text| !text.is_empty());
    Some(match kind {
        b"turn_started" => TranscriptEvent::TurnStarted,
        b"text" => TranscriptEvent::Text { text: body? },
        b"tool_use" => TranscriptEvent::ToolUse { name: body? },
        b"waiting" => TranscriptEvent::Waiting,
        b"interrupted" => TranscriptEvent::Interrupted,
        b"turn_finished" => TranscriptEvent::TurnFinished { success: true },
        b"turn_failed" => TranscriptEvent::TurnFinished { success: false },
        b"turn_error" => TranscriptEvent::TurnError {
            rate_limited: false,
        },
        b"rate_limited" => TranscriptEvent::TurnError { rate_limited: true },
        _ => return None,
    })
}

/// One row's text, at most [`ROW_TEXT_BYTES_MAX`] bytes, cut at a character boundary, with
/// bytes that are not UTF-8 replaced. Input: the row's bytes. Output: the text.
fn bounded(bytes: &[u8]) -> String {
    let taken = &bytes[..bytes.len().min(ROW_TEXT_BYTES_MAX + 3)];
    let mut text = String::from_utf8_lossy(taken).into_owned();
    if text.len() > ROW_TEXT_BYTES_MAX {
        let mut end = ROW_TEXT_BYTES_MAX;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
    }
    assert!(text.len() <= ROW_TEXT_BYTES_MAX);
    text
}

/// The plain sentence the apps show for one refusal code of this reader. Input: the code.
/// Output: the sentence, or none for a code this reader does not give.
pub fn sentence(code: &str) -> Option<&'static str> {
    Some(match code {
        UNSAFE => {
            "The harness's session database is not a plain file, so CrazyAgent will not read it."
        }
        BUSY => "The harness's session database stayed locked by another program for too long.",
        VERSION_UNKNOWN => {
            "This harness version keeps its sessions in a layout CrazyAgent does not know yet. Update CrazyAgent or use a harness version it supports."
        }
        UNREADABLE => "The harness's session database could not be read.",
        TOO_LARGE => "The harness's session database is too large for CrazyAgent to read.",
        _ => return None,
    })
}

#[cfg(test)]
#[path = "sqlite_poll_tests.rs"]
mod tests;
