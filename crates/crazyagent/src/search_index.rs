use crate::engine::EngineError;
use crate::protocol::ProjectId;
use rusqlite::Connection;

pub const CHUNK_BYTES_MAX: usize = 2000;
pub const BEST_MAX: usize = 50;

/// What a chunk of the search index holds: one message, tool call or tool output of a past
/// conversation, or one saved memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Session,
    Memory,
}

impl Source {
    /// The stored word for the source. Output: `session` or `memory`.
    pub fn as_str(self) -> &'static str {
        unimplemented!("search index slice 1")
    }
}

/// Creates the index tables and, when this SQLite has FTS5, the full-text table and its
/// triggers. Input: the engine database. Output: done, or the database failure.
pub(crate) fn initialize(_connection: &Connection) -> Result<(), EngineError> {
    unimplemented!("search index slice 1")
}

/// Makes the schema for a known FTS5 answer. Inputs: the database and whether FTS5 works.
/// Output: done, or the database failure.
fn prepare(_connection: &Connection, _full_text: bool) -> Result<(), EngineError> {
    unimplemented!("search index slice 1")
}

/// Tells whether the full-text index can answer. Input: the database. Output: the answer.
pub(crate) fn full_text(_connection: &Connection) -> bool {
    unimplemented!("search index slice 1")
}

/// Stores the chunks of one conversation or memory not stored yet. Inputs: the database,
/// the project (None for an app-wide memory), the source, the key, each chunk's ordinal
/// and raw text, and the time. Output: the chunks stored, or the database failure.
pub(crate) fn append(
    _connection: &Connection,
    _project: Option<&ProjectId>,
    _source: Source,
    _key: &str,
    _chunks: &[(i64, &str)],
    _at_ms: i64,
) -> Result<usize, EngineError> {
    unimplemented!("search index slice 1")
}

/// Finds the chunks of a project and source that hold every word of a query, best first.
/// Inputs: the database, the project, the source and the untrusted query. Output: at most
/// `BEST_MAX` chunk ids, or the database failure.
pub(crate) fn best(
    _connection: &Connection,
    _project: &ProjectId,
    _source: Source,
    _query: &str,
) -> Result<Vec<i64>, EngineError> {
    unimplemented!("search index slice 1")
}

/// Removes every chunk, watermark and pass mark of a project. Inputs: the database and the
/// project. Output: done, or the database failure.
pub(crate) fn forget_project(
    _connection: &Connection,
    _project: &ProjectId,
) -> Result<(), EngineError> {
    unimplemented!("search index slice 1")
}

/// Removes one conversation's chunks and watermark. Inputs: the database, the project and
/// the conversation key. Output: done, or the database failure.
pub(crate) fn forget_key(
    _connection: &Connection,
    _project: &ProjectId,
    _key: &str,
) -> Result<(), EngineError> {
    unimplemented!("search index slice 1")
}

/// Turns an untrusted query into an FTS5 expression of plain words. Input: the query.
/// Output: the expression, or None when no word is left.
fn match_expression(_query: &str) -> Option<String> {
    unimplemented!("search index slice 1")
}

#[cfg(test)]
mod tests;
