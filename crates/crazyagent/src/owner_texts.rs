use crate::engine::EngineError;
use crate::protocol::ProjectId;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[cfg(test)]
mod bounds_tests;
#[cfg(test)]
mod tests;

/// The most bytes one kept text may hold.
pub const TEXT_BYTES_MAX: usize = 1 << 20;
/// The most bytes a holder key may hold.
pub const HOLDER_BYTES_MAX: usize = 1024;
const KEY_BYTES: usize = 32;
const BLOCK_BYTES: usize = 64;
const _: () = assert!(KEY_BYTES <= BLOCK_BYTES);

const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS kept_texts
    (digest TEXT PRIMARY KEY, text BLOB NOT NULL);
CREATE TABLE IF NOT EXISTS kept_text_refs
    (holder TEXT PRIMARY KEY, digest TEXT NOT NULL, project TEXT);
CREATE INDEX IF NOT EXISTS kept_text_refs_digest ON kept_text_refs(digest);
CREATE INDEX IF NOT EXISTS kept_text_refs_project ON kept_text_refs(project);
CREATE TRIGGER IF NOT EXISTS kept_text_refs_deleted AFTER DELETE ON kept_text_refs
BEGIN DELETE FROM kept_texts WHERE digest=OLD.digest
    AND NOT EXISTS (SELECT 1 FROM kept_text_refs WHERE digest=OLD.digest); END;
CREATE TRIGGER IF NOT EXISTS kept_text_refs_moved AFTER UPDATE OF digest ON kept_text_refs
BEGIN DELETE FROM kept_texts WHERE digest=OLD.digest
    AND NOT EXISTS (SELECT 1 FROM kept_text_refs WHERE digest=OLD.digest); END;
CREATE TABLE IF NOT EXISTS kept_text_key
    (id INTEGER PRIMARY KEY CHECK(id=1), key BLOB NOT NULL);";

/// A kept text's digest as an event body carries it: 64 lowercase hex digits, decoded only
/// through that check, so no free text rides in it (v2.5 L1-N1, DEC 250-OWNER-TEXT).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct TextDigest(String);

impl TextDigest {
    /// Checks a digest's text. Input: the text. Output: the digest, or
    /// `owner_text_digest_invalid` unless it is 64 lowercase hex digits.
    pub fn new(text: String) -> Result<Self, EngineError> {
        let hex = text.len() == 64
            && text
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
        if !hex {
            return Err(error("owner_text_digest_invalid"));
        }
        Ok(Self(text))
    }

    /// The digest as its 64 hex digits. Output: the text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for TextDigest {
    type Error = EngineError;
    fn try_from(text: String) -> Result<Self, Self::Error> {
        Self::new(text)
    }
}

impl From<TextDigest> for String {
    fn from(digest: TextDigest) -> Self {
        digest.0
    }
}

impl std::fmt::Display for TextDigest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

fn error(code: impl std::fmt::Display) -> EngineError {
    EngineError(code.to_string())
}

/// Whether a holder key is one the engine builds from a table, ids and a column name,
/// `table/id/column`: 1 to [`HOLDER_BYTES_MAX`] ASCII graphic bytes, never a path or text.
/// Input: the key. Output: the answer.
fn holder_ok(holder: &str) -> bool {
    !holder.is_empty()
        && holder.len() <= HOLDER_BYTES_MAX
        && holder.bytes().all(|byte| byte.is_ascii_graphic())
}

/// Creates the text, holder and key tables when missing, none on the state log and all named
/// `kept_text*`, outside the owner schema check's `owner_*`. The holder table's `project`
/// column lets a project's generic clear delete its holders, and its triggers delete a text
/// once no holder has its digest. Input: the connection. Output: Ok, or the storage error.
pub fn initialize(db: &Connection) -> Result<(), EngineError> {
    db.execute_batch(SCHEMA).map_err(error)
}

/// The store's digest key: 32 random bytes made on first use, in the caller's transaction.
/// Input: the transaction. Output: the key, or `owner_text_key_unavailable` when no random
/// bytes can be read, or `owner_text_key_invalid` for a stored key of another length.
fn store_key(db: &Connection) -> Result<[u8; KEY_BYTES], EngineError> {
    let stored: Option<Vec<u8>> = db
        .query_row("SELECT key FROM kept_text_key WHERE id=1", [], |row| {
            row.get(0)
        })
        .optional()
        .map_err(error)?;
    let key = match stored {
        Some(key) => key,
        None => {
            let mut fresh = [0u8; KEY_BYTES];
            getrandom::fill(&mut fresh).map_err(|_| error("owner_text_key_unavailable"))?;
            db.execute(
                "INSERT INTO kept_text_key VALUES (1, ?1)",
                params![&fresh[..]],
            )
            .map_err(error)?;
            fresh.to_vec()
        }
    };
    let key: [u8; KEY_BYTES] = key
        .try_into()
        .map_err(|_| error("owner_text_key_invalid"))?;
    Ok(key)
}

/// HMAC-SHA256 (RFC 2104) of a text under a store key, so a short text's digest cannot be
/// found by hashing guesses without the key. Inputs: the key and the text. Output: the
/// digest.
fn keyed(key: &[u8; KEY_BYTES], text: &[u8]) -> TextDigest {
    let mut block = [0u8; BLOCK_BYTES];
    block[..KEY_BYTES].copy_from_slice(key);
    let inner_pad = block.map(|byte| byte ^ 0x36);
    let outer_pad = block.map(|byte| byte ^ 0x5c);
    let inner = Sha256::new()
        .chain_update(inner_pad)
        .chain_update(text)
        .finalize();
    let outer = Sha256::new()
        .chain_update(outer_pad)
        .chain_update(inner)
        .finalize();
    TextDigest(format!("{outer:x}"))
}

/// Keeps a text for one holder in the live writer's transaction, the one its event is
/// appended in, and gives the digest the event carries; never in a projection replay runs.
/// A holder kept again with another text lets its old one go. Inputs: the transaction, the
/// holder key, its project (none for an engine-wide row) and the text. Output: the digest,
/// or `owner_text_holder_invalid` or `owner_text_too_large` with nothing written.
pub fn keep(
    db: &Connection,
    holder: &str,
    project: Option<&ProjectId>,
    text: &[u8],
) -> Result<TextDigest, EngineError> {
    assert!(
        !db.is_autocommit(),
        "a text is kept in its event's transaction"
    );
    if !holder_ok(holder) {
        return Err(error("owner_text_holder_invalid"));
    }
    if text.len() > TEXT_BYTES_MAX {
        return Err(error("owner_text_too_large"));
    }
    initialize(db)?;
    let digest = keyed(&store_key(db)?, text);
    db.execute(
        "INSERT OR IGNORE INTO kept_texts VALUES (?1, ?2)",
        params![digest.as_str(), text],
    )
    .map_err(error)?;
    db.execute(
        "INSERT INTO kept_text_refs VALUES (?1, ?2, ?3) ON CONFLICT(holder)
         DO UPDATE SET digest=excluded.digest, project=excluded.project",
        params![holder, digest.as_str(), project.map(ProjectId::as_str)],
    )
    .map_err(error)?;
    let stored: i64 = db
        .query_row(
            "SELECT length(text) FROM kept_texts WHERE digest=?1",
            [digest.as_str()],
            |row| row.get(0),
        )
        .map_err(error)?;
    assert_eq!(
        usize::try_from(stored).ok(),
        Some(text.len()),
        "one digest holds one text"
    );
    Ok(digest)
}

/// Releases a holder in the live writer's transaction, as a removal or a redaction of its
/// row does, never in a projection replay runs (a rebuild would release a key a later row
/// reuses). Its text goes once no holder has its digest. An unknown holder is left as it
/// is. Inputs: the transaction and the holder key. Output: Ok, or the storage error.
pub fn release(db: &Connection, holder: &str) -> Result<(), EngineError> {
    assert!(
        !db.is_autocommit(),
        "a text is released in its row's transaction"
    );
    if !holder_ok(holder) {
        return Err(error("owner_text_holder_invalid"));
    }
    initialize(db)?;
    let removed = db
        .execute("DELETE FROM kept_text_refs WHERE holder=?1", [holder])
        .map_err(error)?;
    assert!(removed <= 1, "a holder key names one row");
    Ok(())
}

/// The text kept under a digest. Reads only, so a read-only connection works and a read
/// writes no schema: before any text is kept the tables are missing and it gives none.
/// Inputs: the connection and the digest. Output: the text, none when no text is kept under
/// it, or the storage error.
pub fn text(db: &Connection, digest: &TextDigest) -> Result<Option<Vec<u8>>, EngineError> {
    let made: bool = db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name='kept_texts')",
            [],
            |row| row.get(0),
        )
        .map_err(error)?;
    if !made {
        return Ok(None);
    }
    let found: Option<Vec<u8>> = db
        .query_row(
            "SELECT text FROM kept_texts WHERE digest=?1",
            [digest.as_str()],
            |row| row.get(0),
        )
        .optional()
        .map_err(error)?;
    assert!(
        found
            .as_ref()
            .is_none_or(|bytes| bytes.len() <= TEXT_BYTES_MAX),
        "a kept text is in its bound"
    );
    Ok(found)
}

/// The text replay writes for a digest: the kept text, or empty bytes named on stderr
/// (`owner_text_missing <digest>`) and in `missing`. A missing digest is normal for a text
/// replaced or removed later, so a replay row checks final rows, not an empty list.
/// Inputs: the connection, the digest and the missing list. Output: the bytes.
pub fn replayed(
    db: &Connection,
    digest: &TextDigest,
    missing: &mut Vec<TextDigest>,
) -> Result<Vec<u8>, EngineError> {
    let Some(bytes) = text(db, digest)? else {
        eprintln!("owner_text_missing {digest}");
        missing.push(digest.clone());
        return Ok(Vec::new());
    };
    assert!(bytes.len() <= TEXT_BYTES_MAX, "a kept text is in its bound");
    Ok(bytes)
}
