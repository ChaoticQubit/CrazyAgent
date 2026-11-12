use crate::engine::EngineError;
use crate::protocol::ProjectId;
use rusqlite::{Connection, params};

pub const CHUNK_BYTES_MAX: usize = 2000;
pub const BEST_MAX: usize = 50;
pub const KEY_BYTES_MAX: usize = 192;
const WORDS_MAX: usize = 8;
const WORD_BYTES_MAX: usize = 64;
const TRIGGERS: [&str; 3] = ["search_chunks_ai", "search_chunks_ad", "search_chunks_au"];

const TABLES: &str = "CREATE TABLE IF NOT EXISTS search_chunks (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    project TEXT CHECK(project IS NULL OR length(CAST(project AS BLOB)) BETWEEN 1 AND 256),
    source TEXT NOT NULL CHECK(source IN ('session','memory')),
    key TEXT NOT NULL CHECK(length(CAST(key AS BLOB)) BETWEEN 1 AND 192),
    ordinal INTEGER NOT NULL CHECK(ordinal >= 0),
    text TEXT NOT NULL CHECK(length(CAST(text AS BLOB)) BETWEEN 1 AND 2000),
    at_ms INTEGER NOT NULL,
    CHECK(source='memory' OR project IS NOT NULL)
);
CREATE UNIQUE INDEX IF NOT EXISTS search_chunks_message ON search_chunks(project, key, ordinal) WHERE source='session';
CREATE UNIQUE INDEX IF NOT EXISTS search_chunks_memory ON search_chunks(key) WHERE source='memory';
CREATE INDEX IF NOT EXISTS search_chunks_project ON search_chunks(project, source);
CREATE TABLE IF NOT EXISTS search_sources (
    project TEXT NOT NULL,
    key TEXT NOT NULL CHECK(length(CAST(key AS BLOB)) BETWEEN 1 AND 192),
    position INTEGER NOT NULL DEFAULT 0 CHECK(position >= 0),
    position_ms INTEGER NOT NULL DEFAULT 0,
    next_ordinal INTEGER NOT NULL DEFAULT 0 CHECK(next_ordinal >= 0),
    inode INTEGER,
    checked_ms INTEGER NOT NULL,
    PRIMARY KEY(project, key)
);
CREATE TABLE IF NOT EXISTS search_projects (
    project TEXT PRIMARY KEY,
    caught_up_ms INTEGER
);";

const FULL_TEXT_TABLE: &str = "CREATE VIRTUAL TABLE IF NOT EXISTS search_chunks_fts USING fts5(
    text, content='search_chunks', content_rowid='id', tokenize='unicode61 remove_diacritics 2'
)";

const FULL_TEXT_TRIGGERS: &str = "CREATE TRIGGER IF NOT EXISTS search_chunks_ai AFTER INSERT ON search_chunks BEGIN
    INSERT INTO search_chunks_fts(rowid, text) VALUES (new.id, new.text);
END;
CREATE TRIGGER IF NOT EXISTS search_chunks_ad AFTER DELETE ON search_chunks BEGIN
    INSERT INTO search_chunks_fts(search_chunks_fts, rowid, text) VALUES ('delete', old.id, old.text);
END;
CREATE TRIGGER IF NOT EXISTS search_chunks_au AFTER UPDATE OF text ON search_chunks BEGIN
    INSERT INTO search_chunks_fts(search_chunks_fts, rowid, text) VALUES ('delete', old.id, old.text);
    INSERT INTO search_chunks_fts(rowid, text) VALUES (new.id, new.text);
END;";

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
        match self {
            Self::Session => "session",
            Self::Memory => "memory",
        }
    }
}

fn storage(_: impl std::fmt::Display) -> EngineError {
    EngineError("search_index_unavailable".to_owned())
}

/// Creates the index tables and, when this SQLite was built with FTS5, the full-text table
/// and its triggers; without FTS5 the index stays empty and every search scans as before.
/// Input: the engine database. Output: done, or the database failure.
pub(crate) fn initialize(connection: &Connection) -> Result<(), EngineError> {
    let built: bool = connection
        .query_row(
            "SELECT sqlite_compileoption_used('ENABLE_FTS5')",
            [],
            |row| row.get(0),
        )
        .map_err(storage)?;
    prepare(connection, built)
}

/// Makes the schema for a known FTS5 answer: with it, the full-text table and triggers,
/// rebuilt when the triggers were missing so no stale entry stays; without it, no trigger
/// and no chunk, watermark or pass mark. Inputs: the database and whether FTS5 works.
/// Output: done, or the database failure.
fn prepare(connection: &Connection, full_text: bool) -> Result<(), EngineError> {
    connection.execute_batch(TABLES).map_err(storage)?;
    if !full_text {
        for trigger in TRIGGERS {
            connection
                .execute_batch(&format!("DROP TRIGGER IF EXISTS {trigger}"))
                .map_err(storage)?;
        }
        connection
            .execute_batch("DELETE FROM search_chunks; DELETE FROM search_sources; DELETE FROM search_projects;")
            .map_err(storage)?;
        assert!(!triggers_present(connection)?);
        return Ok(());
    }
    connection.execute_batch(FULL_TEXT_TABLE).map_err(storage)?;
    if !triggers_present(connection)? {
        connection
            .execute_batch(FULL_TEXT_TRIGGERS)
            .map_err(storage)?;
        connection
            .execute(
                "INSERT INTO search_chunks_fts(search_chunks_fts) VALUES ('rebuild')",
                [],
            )
            .map_err(storage)?;
    }
    assert!(triggers_present(connection)?);
    Ok(())
}

fn triggers_present(connection: &Connection) -> Result<bool, EngineError> {
    let found: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM sqlite_schema WHERE type='trigger' AND name IN (?1,?2,?3)",
            TRIGGERS,
            |row| row.get(0),
        )
        .map_err(storage)?;
    assert!((0..=3).contains(&found));
    Ok(found == 3)
}

/// Tells whether the full-text index can answer: its triggers are in place and this SQLite
/// reads its table. Input: the database. Output: the answer.
pub(crate) fn full_text(connection: &Connection) -> bool {
    triggers_present(connection).unwrap_or(false)
        && connection
            .prepare("SELECT rowid FROM search_chunks_fts LIMIT 0")
            .is_ok()
}

/// The text a chunk stores: the message with secrets masked, trimmed and cut to
/// `CHUNK_BYTES_MAX` bytes on a character, masked before the cut so no secret is split
/// past the masking. Input: the raw text. Output: the text, or None when nothing is left.
fn chunk_text(raw: &str) -> Option<String> {
    let masked = crate::project_setup::redact(raw);
    let trimmed = masked.trim();
    let mut end = trimmed.len().min(CHUNK_BYTES_MAX);
    while !trimmed.is_char_boundary(end) {
        end -= 1;
    }
    let text = trimmed[..end].trim_end();
    assert!(text.len() <= CHUNK_BYTES_MAX);
    (!text.is_empty()).then(|| text.to_owned())
}

/// Stores the chunks of one conversation or memory not stored yet, each masked and cut
/// first; nothing is stored for a project that is gone. Inputs: the database, the project
/// (None for an app-wide memory), the source, the key, each chunk's ordinal and raw text,
/// and the time. Output: the chunks stored, or the database failure.
pub(crate) fn append(
    connection: &Connection,
    project: Option<&ProjectId>,
    source: Source,
    key: &str,
    chunks: &[(i64, &str)],
    at_ms: i64,
) -> Result<usize, EngineError> {
    assert!(!key.is_empty() && key.len() <= KEY_BYTES_MAX);
    assert!(source == Source::Memory || project.is_some());
    if let Some(project) = project {
        let exists: bool = connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM projects WHERE id=?1)",
                [project.as_str()],
                |row| row.get(0),
            )
            .map_err(storage)?;
        if !exists {
            return Ok(0);
        }
    }
    let mut statement = connection
        .prepare_cached(
            "INSERT OR IGNORE INTO search_chunks(project,source,key,ordinal,text,at_ms)
             VALUES (?1,?2,?3,?4,?5,?6)",
        )
        .map_err(storage)?;
    let mut stored = 0;
    for (ordinal, raw) in chunks {
        assert!(*ordinal >= 0);
        let Some(text) = chunk_text(raw) else {
            continue;
        };
        stored += statement
            .execute(params![
                project.map(ProjectId::as_str),
                source.as_str(),
                key,
                ordinal,
                text,
                at_ms
            ])
            .map_err(storage)?;
    }
    assert!(stored <= chunks.len());
    Ok(stored)
}

/// Finds the chunks of a project and source that hold every word of a query, ranked by
/// BM25: a project's own conversations, or its memories and the app-wide ones. Inputs: the
/// database, the project, the source and the untrusted query. Output: at most `BEST_MAX`
/// chunk ids best first, none for a query with no word, or the database failure.
pub(crate) fn best(
    connection: &Connection,
    project: &ProjectId,
    source: Source,
    query: &str,
) -> Result<Vec<i64>, EngineError> {
    let Some(expression) = match_expression(query) else {
        return Ok(Vec::new());
    };
    let scope = match source {
        Source::Session => "c.project=?2",
        Source::Memory => "(c.project=?2 OR c.project IS NULL)",
    };
    let sql = format!(
        "SELECT c.id FROM search_chunks_fts JOIN search_chunks c ON c.id=search_chunks_fts.rowid
         WHERE search_chunks_fts MATCH ?1 AND c.source=?3 AND {scope}
         ORDER BY bm25(search_chunks_fts), c.id DESC LIMIT {BEST_MAX}"
    );
    let mut statement = connection.prepare(&sql).map_err(storage)?;
    let ids = statement
        .query_map(
            params![expression, project.as_str(), source.as_str()],
            |row| row.get(0),
        )
        .map_err(storage)?
        .collect::<Result<Vec<i64>, _>>()
        .map_err(storage)?;
    assert!(ids.len() <= BEST_MAX);
    Ok(ids)
}

/// Removes every chunk, watermark and pass mark of a project, its memories' chunks
/// included. Inputs: the database and the project. Output: done, or the database failure.
pub(crate) fn forget_project(
    connection: &Connection,
    project: &ProjectId,
) -> Result<(), EngineError> {
    if !made(connection)? {
        return Ok(());
    }
    for table in ["search_chunks", "search_sources", "search_projects"] {
        connection
            .execute(
                &format!("DELETE FROM {table} WHERE project=?1"),
                [project.as_str()],
            )
            .map_err(storage)?;
    }
    Ok(())
}

/// Removes one conversation's chunks and watermark. Inputs: the database, the project and
/// the conversation key. Output: done, or the database failure.
pub(crate) fn forget_key(
    connection: &Connection,
    project: &ProjectId,
    key: &str,
) -> Result<(), EngineError> {
    connection
        .execute(
            "DELETE FROM search_chunks WHERE project=?1 AND source='session' AND key=?2",
            params![project.as_str(), key],
        )
        .map_err(storage)?;
    connection
        .execute(
            "DELETE FROM search_sources WHERE project=?1 AND key=?2",
            params![project.as_str(), key],
        )
        .map_err(storage)?;
    Ok(())
}

/// Removes a deleted master session's engine turns from the index, in whichever project
/// they were. Inputs: the database and the session id. Output: done, or the database
/// failure.
pub(crate) fn forget_session(connection: &Connection, session: &str) -> Result<(), EngineError> {
    if !made(connection)? {
        return Ok(());
    }
    let key = format!("session/{session}");
    connection
        .execute(
            "DELETE FROM search_chunks WHERE source='session' AND key=?1",
            [key.as_str()],
        )
        .map_err(storage)?;
    connection
        .execute("DELETE FROM search_sources WHERE key=?1", [key.as_str()])
        .map_err(storage)?;
    Ok(())
}

fn made(connection: &Connection) -> Result<bool, EngineError> {
    connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name='search_chunks')",
            [],
            |row| row.get(0),
        )
        .map_err(storage)
}

/// Turns an untrusted query into an FTS5 expression of plain words: at most `WORDS_MAX`
/// words of at most `WORD_BYTES_MAX` bytes that hold a letter or digit, each one quoted
/// phrase with its quotes doubled and a prefix star, so no word is read as FTS5 syntax.
/// Input: the query. Output: the expression, or None when no word is left.
fn match_expression(query: &str) -> Option<String> {
    let words: Vec<String> = query
        .split_whitespace()
        .filter(|word| word.len() <= WORD_BYTES_MAX && word.chars().any(char::is_alphanumeric))
        .take(WORDS_MAX)
        .map(|word| format!("\"{}\"*", word.replace('"', "\"\"")))
        .collect();
    assert!(words.len() <= WORDS_MAX);
    (!words.is_empty()).then(|| words.join(" "))
}

/// Tells whether sessions(query) of a project may read the index: full text works and the
/// project's first full pass is done. Inputs: the database and the project. Output: the
/// answer.
pub(crate) fn sessions_ready(_connection: &Connection, _project: &ProjectId) -> bool {
    unimplemented!("search index slice 2")
}

/// Tells whether recall(query) may read the index: full text works and every memory change
/// is synced. Input: the database. Output: the answer.
pub(crate) fn memories_ready(_connection: &Connection) -> bool {
    unimplemented!("search index slice 2")
}

/// One stored chunk as a search shows it: its conversation or memory key, its message
/// number and its text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Chunk {
    pub(crate) key: String,
    pub(crate) ordinal: i64,
    pub(crate) text: String,
}

/// Ranks a project's chunks of one source for a query: BM25 first, then the semantic
/// ranking when a plugin gives one. Inputs: the database, the project, the source and the
/// untrusted query. Output: the chunk ids best first and the ranking's note, or the failure.
pub(crate) fn ranked(
    _connection: &Connection,
    _project: &ProjectId,
    _source: Source,
    _query: &str,
) -> Result<(Vec<i64>, Option<&'static str>), EngineError> {
    unimplemented!("search index slice 2")
}

/// Reloads one ranked chunk with the project and source it must belong to, so an id from
/// any other place is never shown. Inputs: the database, the id, the project and the source.
/// Output: the chunk, None for an id of another place, or the failure.
pub(crate) fn chunk(
    _connection: &Connection,
    _id: i64,
    _project: &ProjectId,
    _source: Source,
) -> Result<Option<Chunk>, EngineError> {
    unimplemented!("search index slice 2")
}

pub(crate) mod indexer;

#[cfg(test)]
mod tests;
