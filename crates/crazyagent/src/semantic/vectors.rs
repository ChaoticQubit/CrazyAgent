use super::{Hybrid, Progress};
use crate::app_plugins::manifest;
use crate::engine::EngineError;
use crate::protocol::ProjectId;
use crate::search_index::{CHUNK_BYTES_MAX, Source};
use rusqlite::{Connection, OptionalExtension, params};

#[cfg(test)]
mod tests;

/// The most bytes of vectors one project keeps, over every plugin key (DEC-156 addendum 2).
pub(crate) const PROJECT_BYTES_MAX: u64 = 256 * 1024 * 1024;
/// The most bytes of vectors the app-wide memories keep, apart from any project (leader QC).
pub(crate) const MEMORY_BYTES_MAX: u64 = 32 * 1024 * 1024;
/// The most bytes of vectors one query scores, newest first (leader QA).
pub(crate) const SCAN_BYTES_MAX: u64 = 32 * 1024 * 1024;
/// The most chunks one project's round embeds.
pub(crate) const ROUND_CHUNKS_MAX: usize = 2_000;
/// The most vector hits one query fuses, as many as the full-text side gives.
pub(crate) const NEAREST_MAX: usize = crate::search_index::BEST_MAX;
/// The reciprocal rank fusion constant.
pub(crate) const FUSION_K: f64 = 60.0;
/// The most query vectors kept per plugin key.
pub(crate) const QUERY_ROWS_MAX: usize = 256;
/// The most queries one round embeds.
pub(crate) const QUERY_ROUND_MAX: usize = 16;
/// The most stale vectors one round deletes.
pub(crate) const SWEEP_ROUND_MAX: usize = 10_000;
/// The fewest and most dimensions a plugin's vectors may have (manifest bounds).
pub(crate) const DIMENSIONS_MIN: usize = manifest::DIMENSIONS_MIN as usize;
pub(crate) const DIMENSIONS_MAX: usize = manifest::DIMENSIONS_MAX as usize;
/// The most bytes of a plugin key: a plugin name, a colon and a SHA-256.
pub(crate) const KEY_BYTES_MAX: usize = 128;
const TABLES: &str = "CREATE TABLE IF NOT EXISTS search_vectors (
    chunk INTEGER NOT NULL,
    plugin_key TEXT NOT NULL CHECK (length(plugin_key) BETWEEN 1 AND 128),
    project TEXT,
    source TEXT NOT NULL CHECK (source IN ('session', 'memory')),
    at_ms INTEGER NOT NULL,
    vector BLOB NOT NULL CHECK (length(vector) BETWEEN 256 AND 4096 AND length(vector) % 4 = 0),
    PRIMARY KEY (chunk, plugin_key)
);
CREATE INDEX IF NOT EXISTS search_vectors_scan ON search_vectors(plugin_key, project, source, at_ms, chunk);
CREATE INDEX IF NOT EXISTS search_vectors_room ON search_vectors(plugin_key, project, at_ms, chunk);
CREATE TABLE IF NOT EXISTS search_vector_pending (
    chunk INTEGER PRIMARY KEY,
    project TEXT,
    source TEXT NOT NULL CHECK (source IN ('session', 'memory')),
    at_ms INTEGER NOT NULL,
    dropped INTEGER NOT NULL DEFAULT 0 CHECK (dropped IN (0, 1))
);
CREATE INDEX IF NOT EXISTS search_vector_pending_next ON search_vector_pending(project, dropped, at_ms);
CREATE INDEX IF NOT EXISTS search_vector_pending_scope ON search_vector_pending(project, source, dropped);
CREATE TABLE IF NOT EXISTS search_vector_state (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    plugin_key TEXT NOT NULL CHECK (length(plugin_key) BETWEEN 1 AND 128)
);
CREATE TABLE IF NOT EXISTS search_query_vectors (
    plugin_key TEXT NOT NULL CHECK (length(plugin_key) BETWEEN 1 AND 128),
    query_sha TEXT NOT NULL CHECK (length(query_sha) = 64),
    query TEXT CHECK (query IS NULL OR length(CAST(query AS BLOB)) BETWEEN 1 AND 2000),
    asked_ms INTEGER NOT NULL,
    vector BLOB CHECK (vector IS NULL OR (length(vector) BETWEEN 256 AND 4096 AND length(vector) % 4 = 0)),
    PRIMARY KEY (plugin_key, query_sha),
    CHECK ((vector IS NULL) = (query IS NOT NULL))
);
CREATE INDEX IF NOT EXISTS search_query_vectors_asked ON search_query_vectors(plugin_key, asked_ms);";
const TRIGGERS: [&str; 3] = [
    "search_vectors_added",
    "search_vectors_gone",
    "search_vectors_stale",
];
const TRIGGER_SQL: &str = "CREATE TRIGGER IF NOT EXISTS search_vectors_added AFTER INSERT ON search_chunks BEGIN
    DELETE FROM search_vector_pending WHERE chunk = new.id;
    INSERT INTO search_vector_pending(chunk, project, source, at_ms) VALUES (new.id, new.project, new.source, new.at_ms);
END;
CREATE TRIGGER IF NOT EXISTS search_vectors_gone AFTER DELETE ON search_chunks BEGIN
    DELETE FROM search_vectors WHERE chunk = old.id;
    DELETE FROM search_vector_pending WHERE chunk = old.id;
END;
CREATE TRIGGER IF NOT EXISTS search_vectors_stale AFTER UPDATE OF text, project, source, at_ms ON search_chunks BEGIN
    DELETE FROM search_vectors WHERE chunk = old.id;
    DELETE FROM search_vector_pending WHERE chunk = old.id;
    INSERT INTO search_vector_pending(chunk, project, source, at_ms) VALUES (new.id, new.project, new.source, new.at_ms);
END;";
const SWEEP: &str = "DELETE FROM search_vectors WHERE NOT EXISTS (SELECT 1 FROM search_chunks WHERE id = search_vectors.chunk);
DELETE FROM search_vector_pending WHERE NOT EXISTS (SELECT 1 FROM search_chunks WHERE id = search_vector_pending.chunk);
DELETE FROM search_vector_state;";

/// The ranking is words only: the chosen plugin can't be used now.
pub(crate) const NOTE_UNAVAILABLE: &str =
    "Ranked by words only: the chosen search plugin can't be used right now.";
/// The ranking is words only: the local search model is still loading.
pub(crate) const NOTE_LOADING: &str =
    "Ranked by words only this time: the search model is still loading.";
/// The ranking is words only: an HTTP plugin is still turning this query into a vector.
pub(crate) const NOTE_QUERY_PENDING: &str = "Ranked by words only this time: the search plugin is still reading this query. Ask again in a few seconds to rank by meaning too.";
/// The ranking is words only: the plugin has read none of this history yet.
pub(crate) const NOTE_NO_VECTORS: &str =
    "Ranked by words only: the search plugin has not read this history yet.";
/// Part of the history has no vector yet and ranks by words only.
pub(crate) const NOTE_PARTLY: &str = "Ranked by words and meaning; part of this history is not read by the search plugin yet and ranks by words only.";
/// Only the newest part of the history was scored by meaning.
pub(crate) const NOTE_NEWEST: &str = "Ranked by words and by meaning over the newest part of this history; older parts rank by words only.";
/// Both of the above.
pub(crate) const NOTE_PARTLY_NEWEST: &str = "Ranked by words and by meaning over the newest part of this history; older parts and parts not read by the search plugin yet rank by words only.";

/// The limits one round or query works within; the engine uses [`BOUNDS`], tests smaller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Bounds {
    pub(crate) project_bytes: u64,
    pub(crate) memory_bytes: u64,
    pub(crate) round_chunks: usize,
    pub(crate) scan_bytes: u64,
}

/// The engine's limits.
pub(crate) const BOUNDS: Bounds = Bounds {
    project_bytes: PROJECT_BYTES_MAX,
    memory_bytes: MEMORY_BYTES_MAX,
    round_chunks: ROUND_CHUNKS_MAX,
    scan_bytes: SCAN_BYTES_MAX,
};

/// The chosen embedding plugin as vectors see it: the key its vectors are stored under, the
/// dimensions every vector has and the most texts one call takes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Plugin {
    pub(crate) key: String,
    pub(crate) dimensions: usize,
    pub(crate) batch: usize,
}

impl Plugin {
    /// Names a plugin's vectors by its manifest name and SHA-256, so an edited or other
    /// plugin re-embeds. Inputs: the name, the SHA-256 hex, the dimensions and the batch.
    /// Output: the plugin.
    pub(crate) fn new(name: &str, sha256: &str, dimensions: usize, batch: usize) -> Self {
        assert!(
            manifest::valid_name(name),
            "a loaded plugin has a valid name"
        );
        assert!(sha256.len() == 64 && sha256.bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert!((DIMENSIONS_MIN..=DIMENSIONS_MAX).contains(&dimensions));
        assert!((1..=usize::from(manifest::BATCH_MAX)).contains(&batch));
        let key = format!("{name}:{sha256}");
        assert!(key.len() <= KEY_BYTES_MAX);
        Self {
            key,
            dimensions,
            batch,
        }
    }

    /// The bytes one stored vector of this plugin takes. Output: the dimensions times four.
    fn vector_bytes(&self) -> u64 {
        let bytes = self.dimensions as u64 * 4;
        assert!((DIMENSIONS_MIN as u64 * 4..=DIMENSIONS_MAX as u64 * 4).contains(&bytes));
        bytes
    }
}

fn storage(_: impl std::fmt::Display) -> EngineError {
    EngineError("search_index_unavailable".to_owned())
}

/// How many of the three vector triggers exist on the chunk table. Input: the database.
/// Output: the count, 0 to 3.
fn triggers_found(connection: &Connection) -> Result<usize, EngineError> {
    let found: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM sqlite_schema WHERE type='trigger' AND name IN (?1,?2,?3)",
            TRIGGERS,
            |row| row.get(0),
        )
        .map_err(storage)?;
    assert!((0..=3).contains(&found));
    Ok(found as usize)
}

/// Brings the vectors in line with the plugin key before a round: when the stored key is
/// another, deletes at most [`SWEEP_ROUND_MAX`] of the other keys' vectors, and once none
/// is left drops their query vectors, queues every chunk with no vector of this key and
/// stores the key. Inputs: the connection and the plugin key. Output: whether the vectors
/// are this key's alone, so the round may embed.
fn settle(connection: &mut Connection, key: &str) -> Result<bool, EngineError> {
    assert!(!key.is_empty() && key.len() <= KEY_BYTES_MAX);
    let tx = immediate(connection)?;
    let state: Option<String> = tx
        .query_row(
            "SELECT plugin_key FROM search_vector_state WHERE id=1",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(storage)?;
    if state.as_deref() == Some(key) {
        return Ok(true);
    }
    let mut deleted = 0;
    for side in ["<", ">"] {
        let left = SWEEP_ROUND_MAX - deleted;
        deleted += tx
            .execute(
                &format!(
                    "DELETE FROM search_vectors WHERE rowid IN
                     (SELECT rowid FROM search_vectors WHERE plugin_key {side} ?1 LIMIT ?2)"
                ),
                params![key, left as i64],
            )
            .map_err(storage)?;
        assert!(deleted <= SWEEP_ROUND_MAX);
    }
    let others: bool = tx
        .query_row(
            "SELECT EXISTS (SELECT 1 FROM search_vectors WHERE plugin_key < ?1)
                 OR EXISTS (SELECT 1 FROM search_vectors WHERE plugin_key > ?1)",
            [key],
            |row| row.get(0),
        )
        .map_err(storage)?;
    if others {
        tx.commit().map_err(storage)?;
        return Ok(false);
    }
    tx.execute(
        "DELETE FROM search_query_vectors WHERE plugin_key <> ?1",
        [key],
    )
    .map_err(storage)?;
    tx.execute_batch("DELETE FROM search_vector_pending;")
        .map_err(storage)?;
    tx.execute(
        "INSERT INTO search_vector_pending(chunk, project, source, at_ms)
         SELECT id, project, source, at_ms FROM search_chunks
         WHERE NOT EXISTS (SELECT 1 FROM search_vectors
                           WHERE chunk = search_chunks.id AND plugin_key = ?1)",
        [key],
    )
    .map_err(storage)?;
    tx.execute(
        "INSERT INTO search_vector_state(id, plugin_key) VALUES (1, ?1)
         ON CONFLICT(id) DO UPDATE SET plugin_key = excluded.plugin_key",
        [key],
    )
    .map_err(storage)?;
    tx.commit().map_err(storage)?;
    Ok(true)
}

/// One call that turns texts into vectors, one per text in order.
pub(crate) type Embed<'a> = dyn FnMut(&[&str]) -> Result<Vec<Vec<f32>>, EngineError> + 'a;

/// Creates the vector tables and the triggers that drop a chunk's vectors in the same
/// transaction that deletes or rewrites it, and sweeps vectors left by chunks deleted before.
/// Input: the engine database, after the search index tables. Output: done, or the failure.
pub(crate) fn initialize(connection: &Connection) -> Result<(), EngineError> {
    connection.execute_batch(TABLES).map_err(storage)?;
    if triggers_found(connection)? < TRIGGERS.len() {
        connection.execute_batch(SWEEP).map_err(storage)?;
        connection.execute_batch(TRIGGER_SQL).map_err(storage)?;
    }
    assert_eq!(triggers_found(connection)?, TRIGGERS.len());
    Ok(())
}

/// Embeds the newest chunks of a project, then app-wide memories, that have no vector for
/// the plugin, within the round and storage bounds. Inputs: the connection, the project, the
/// plugin, the bounds and the embed call. Output: how many chunks were embedded.
pub(crate) fn embed_round(
    connection: &mut Connection,
    project: &ProjectId,
    plugin: &Plugin,
    bounds: &Bounds,
    embed: &mut Embed<'_>,
) -> usize {
    assert!((1..=ROUND_CHUNKS_MAX).contains(&bounds.round_chunks));
    if !matches!(settle(connection, &plugin.key), Ok(true)) {
        return 0;
    }
    let Ok(plan) = plan(connection, project, plugin, bounds) else {
        return 0;
    };
    assert!(plan.items.len() <= bounds.round_chunks);
    if !plan.drops.is_empty() && drop_older(connection, &plan.drops).is_err() {
        return 0;
    }
    let mut embedded = 0;
    for batch in plan.items.chunks(plugin.batch) {
        let texts: Vec<&str> = batch.iter().map(|item| item.text.as_str()).collect();
        let Some(vectors) = embed(texts.as_slice())
            .ok()
            .and_then(|vectors| checked(vectors, texts.len(), plugin.dimensions))
        else {
            break;
        };
        match store(connection, plugin, batch, &vectors) {
            Ok(stored) => embedded += stored,
            Err(_) => break,
        }
    }
    assert!(embedded <= plan.items.len());
    embedded
}

/// One queued chunk a round sends: its id, its text as read, and the older vector of its
/// bucket it replaces when the bucket is full.
struct Item {
    chunk: i64,
    text: String,
    evict: Option<i64>,
}

/// What one round does: the chunks to send, newest first with the project's before the
/// app-wide memories, and for each bucket out of room the newest queued chunk from which
/// every older one ranks by words only: its bucket, time and id.
#[derive(Default)]
struct Plan {
    items: Vec<Item>,
    drops: Vec<(Option<String>, i64, i64)>,
}

/// Plans a round over the project's bucket, then the app-wide memories' bucket, each with
/// its own cap: a queued chunk takes free room, else replaces the oldest kept vector when
/// that is older, else it and every older queued chunk of the bucket are dropped. Inputs:
/// the connection, the project, the plugin and the bounds. Output: the plan.
fn plan(
    connection: &Connection,
    project: &ProjectId,
    plugin: &Plugin,
    bounds: &Bounds,
) -> Result<Plan, EngineError> {
    let mut plan = Plan::default();
    let buckets = [
        (Some(project.as_str()), bounds.project_bytes),
        (None, bounds.memory_bytes),
    ];
    for (bucket, bytes) in buckets {
        let left = bounds.round_chunks - plan.items.len();
        if left == 0 {
            break;
        }
        let candidates = queued(connection, bucket, left)?;
        if candidates.is_empty() {
            continue;
        }
        let count: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM search_vectors WHERE plugin_key = ?1 AND project IS ?2",
                params![plugin.key, bucket],
                |row| row.get(0),
            )
            .map_err(storage)?;
        let mut room = (bytes / plugin.vector_bytes()).saturating_sub(count as u64);
        let mut oldest = kept_oldest(connection, &plugin.key, bucket, candidates.len())?
            .into_iter()
            .peekable();
        for (chunk, at_ms, text) in candidates {
            if room > 0 {
                room -= 1;
                plan.items.push(Item {
                    chunk,
                    text,
                    evict: None,
                });
            } else if let Some((old, _)) =
                oldest.next_if(|&(old, old_at)| (old_at, old) < (at_ms, chunk))
            {
                plan.items.push(Item {
                    chunk,
                    text,
                    evict: Some(old),
                });
            } else {
                plan.drops.push((bucket.map(str::to_owned), at_ms, chunk));
                break;
            }
        }
    }
    Ok(plan)
}

/// The newest queued chunks of a bucket with their text. Inputs: the connection, the
/// bucket's project (none for app-wide memories) and the most to read. Output: each chunk's
/// id, time and text, newest first.
fn queued(
    connection: &Connection,
    bucket: Option<&str>,
    most: usize,
) -> Result<Vec<(i64, i64, String)>, EngineError> {
    let mut statement = connection
        .prepare(
            "SELECT p.chunk, p.at_ms, c.text FROM search_vector_pending p
             JOIN search_chunks c ON c.id = p.chunk
             WHERE p.project IS ?1 AND p.dropped = 0
             ORDER BY p.at_ms DESC, p.chunk DESC LIMIT ?2",
        )
        .map_err(storage)?;
    let rows = statement
        .query_map(params![bucket, most as i64], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })
        .map_err(storage)?;
    let found = rows.collect::<Result<Vec<_>, _>>().map_err(storage)?;
    assert!(found.len() <= most);
    Ok(found)
}

/// The oldest kept vectors of a bucket. Inputs: the connection, the plugin key, the
/// bucket's project and the most to read. Output: each chunk's id and time, oldest first.
fn kept_oldest(
    connection: &Connection,
    key: &str,
    bucket: Option<&str>,
    most: usize,
) -> Result<Vec<(i64, i64)>, EngineError> {
    let mut statement = connection
        .prepare(
            "SELECT chunk, at_ms FROM search_vectors WHERE plugin_key = ?1 AND project IS ?2
             ORDER BY at_ms ASC, chunk ASC LIMIT ?3",
        )
        .map_err(storage)?;
    let rows = statement
        .query_map(params![key, bucket, most as i64], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .map_err(storage)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(storage)
}

/// Marks each dropped chunk and every older queued chunk of its bucket as ranked by words
/// only, so no later round sends them. Inputs: the connection and the drops. Output: done.
fn drop_older(
    connection: &mut Connection,
    drops: &[(Option<String>, i64, i64)],
) -> Result<(), EngineError> {
    assert!(drops.len() <= 2);
    let tx = immediate(connection)?;
    for (bucket, at_ms, chunk) in drops {
        tx.execute(
            "UPDATE search_vector_pending SET dropped = 1
             WHERE project IS ?1 AND dropped = 0 AND (at_ms < ?2 OR (at_ms = ?2 AND chunk <= ?3))",
            params![bucket, at_ms, chunk],
        )
        .map_err(storage)?;
    }
    tx.commit().map_err(storage)
}

/// Stores one sent batch: a vector only for a chunk still holding the text that was sent,
/// its queue row removed, and the older vector it replaces evicted and marked dropped.
/// Inputs: the connection, the plugin, the batch and its checked vectors. Output: how many
/// vectors were stored.
fn store(
    connection: &mut Connection,
    plugin: &Plugin,
    batch: &[Item],
    vectors: &[Vec<f32>],
) -> Result<usize, EngineError> {
    assert_eq!(batch.len(), vectors.len());
    let tx = immediate(connection)?;
    let mut stored = 0;
    for (item, vector) in batch.iter().zip(vectors) {
        let added = tx
            .execute(
                "INSERT INTO search_vectors(chunk, plugin_key, project, source, at_ms, vector)
                 SELECT id, ?2, project, source, at_ms, ?3 FROM search_chunks
                 WHERE id = ?1 AND text = ?4
                 ON CONFLICT(chunk, plugin_key) DO UPDATE SET vector = excluded.vector,
                     project = excluded.project, source = excluded.source, at_ms = excluded.at_ms",
                params![item.chunk, plugin.key, encode(vector), item.text],
            )
            .map_err(storage)?;
        assert!(added <= 1);
        if added == 0 {
            continue;
        }
        tx.execute(
            "DELETE FROM search_vector_pending WHERE chunk = ?1",
            [item.chunk],
        )
        .map_err(storage)?;
        if let Some(old) = item.evict {
            evict(&tx, &plugin.key, old)?;
        }
        stored += 1;
    }
    tx.commit().map_err(storage)?;
    Ok(stored)
}

/// Deletes one kept vector to make room and queues its chunk as dropped, so ranking says
/// older parts rank by words only. Inputs: the transaction, the plugin key and the chunk.
/// Output: done.
fn evict(tx: &rusqlite::Transaction<'_>, key: &str, chunk: i64) -> Result<(), EngineError> {
    let gone = tx
        .execute(
            "DELETE FROM search_vectors WHERE chunk = ?1 AND plugin_key = ?2",
            params![chunk, key],
        )
        .map_err(storage)?;
    if gone == 1 {
        tx.execute(
            "DELETE FROM search_vector_pending WHERE chunk = ?1",
            [chunk],
        )
        .map_err(storage)?;
        tx.execute(
            "INSERT INTO search_vector_pending(chunk, project, source, at_ms, dropped)
             SELECT id, project, source, at_ms, 1 FROM search_chunks WHERE id = ?1",
            [chunk],
        )
        .map_err(storage)?;
    }
    Ok(())
}

/// Starts a write transaction that takes the write lock at once. Input: the connection.
/// Output: the transaction.
fn immediate(connection: &mut Connection) -> Result<rusqlite::Transaction<'_>, EngineError> {
    connection
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(storage)
}

/// Checks a reply: one vector per text, each of the plugin's dimensions and finite, then
/// L2 normalizes each so ranking can use the dot product. Inputs: the vectors, the text
/// count and the dimensions. Output: the vectors, or none when any is refused.
fn checked(vectors: Vec<Vec<f32>>, count: usize, dimensions: usize) -> Option<Vec<Vec<f32>>> {
    if vectors.len() != count {
        return None;
    }
    vectors
        .into_iter()
        .map(|vector| normalized(vector, dimensions))
        .collect()
}

/// One vector of the right length with every value finite, scaled to length one unless it
/// is all zeros. Inputs: the vector and the dimensions. Output: the vector, or none.
fn normalized(mut vector: Vec<f32>, dimensions: usize) -> Option<Vec<f32>> {
    if vector.len() != dimensions || !vector.iter().all(|value| value.is_finite()) {
        return None;
    }
    let norm = vector.iter().map(|value| value * value).sum::<f32>().sqrt();
    if !norm.is_finite() {
        return None;
    }
    if norm > 0.0 {
        vector.iter_mut().for_each(|value| *value /= norm);
    }
    Some(vector)
}

/// The stored bytes of a vector: each value as f32 little endian. Input: the vector.
/// Output: the bytes.
fn encode(vector: &[f32]) -> Vec<u8> {
    let bytes: Vec<u8> = vector
        .iter()
        .flat_map(|value| value.to_le_bytes())
        .collect();
    assert_eq!(bytes.len(), vector.len() * 4);
    bytes
}

/// Reads stored bytes back as a vector of the plugin's dimensions. Inputs: the bytes and
/// the dimensions. Output: the vector, or none for another length or a value not finite.
fn decode(bytes: &[u8], dimensions: usize) -> Option<Vec<f32>> {
    if bytes.len() != dimensions * 4 {
        return None;
    }
    let vector: Vec<f32> = bytes
        .chunks_exact(4)
        .map(|four| f32::from_le_bytes([four[0], four[1], four[2], four[3]]))
        .collect();
    vector
        .iter()
        .all(|value| value.is_finite())
        .then_some(vector)
}

/// Masks, trims and cuts an untrusted query before it is hashed, stored, embedded or sent,
/// masked before the cut so no secret is split past the masking. Input: the query. Output:
/// the text, or none when nothing is left.
pub(crate) fn query_text(query: &str) -> Option<String> {
    let masked = crate::project_setup::redact(query);
    let trimmed = masked.trim();
    let mut end = trimmed.len().min(CHUNK_BYTES_MAX);
    while !trimmed.is_char_boundary(end) {
        end -= 1;
    }
    let text = trimmed[..end].trim_end();
    assert!(text.len() <= CHUNK_BYTES_MAX);
    (!text.is_empty()).then(|| text.to_owned())
}

/// Records a query an HTTP plugin must embed on the indexer thread, keeping at most
/// [`QUERY_ROWS_MAX`] per plugin key, newest asked first. Inputs: the connection, the plugin
/// key, the masked query and the time. Output: done, or the failure.
pub(crate) fn ask_query(
    connection: &Connection,
    key: &str,
    query: &str,
    now_ms: i64,
) -> Result<(), EngineError> {
    assert!(!key.is_empty() && key.len() <= KEY_BYTES_MAX);
    assert!(!query.is_empty() && query.len() <= CHUNK_BYTES_MAX);
    connection
        .execute(
            "INSERT INTO search_query_vectors(plugin_key, query_sha, query, asked_ms)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(plugin_key, query_sha)
             DO UPDATE SET asked_ms = max(asked_ms, excluded.asked_ms)",
            params![key, super::sha256_hex(query.as_bytes()), query, now_ms],
        )
        .map_err(storage)?;
    connection
        .execute(
            "DELETE FROM search_query_vectors WHERE rowid IN
             (SELECT rowid FROM search_query_vectors WHERE plugin_key = ?1
              ORDER BY asked_ms DESC, rowid DESC LIMIT -1 OFFSET ?2)",
            params![key, QUERY_ROWS_MAX as i64],
        )
        .map_err(storage)?;
    Ok(())
}

/// Embeds the queries waiting for the plugin, newest first, at most [`QUERY_ROUND_MAX`];
/// on a failed or refused reply the round's unanswered queries are dropped, never resent.
/// Inputs: the connection, the plugin and the embed call. Output: how many were embedded.
pub(crate) fn embed_queries(
    connection: &mut Connection,
    plugin: &Plugin,
    embed: &mut Embed<'_>,
) -> usize {
    let Ok(waiting) = waiting_queries(connection, &plugin.key) else {
        return 0;
    };
    let mut embedded = 0;
    for (index, batch) in waiting.chunks(plugin.batch).enumerate() {
        let texts: Vec<&str> = batch.iter().map(|(_, text)| text.as_str()).collect();
        let Some(vectors) = embed(texts.as_slice())
            .ok()
            .and_then(|vectors| checked(vectors, texts.len(), plugin.dimensions))
        else {
            let _ = forget_queries(connection, &plugin.key, &waiting[index * plugin.batch..]);
            break;
        };
        match answer_queries(connection, &plugin.key, batch, &vectors) {
            Ok(answered) => embedded += answered,
            Err(_) => break,
        }
    }
    assert!(embedded <= waiting.len());
    embedded
}

/// The queries waiting for a plugin key, newest asked first, at most [`QUERY_ROUND_MAX`].
/// Inputs: the connection and the key. Output: each query's SHA-256 and text.
fn waiting_queries(
    connection: &Connection,
    key: &str,
) -> Result<Vec<(String, String)>, EngineError> {
    let mut statement = connection
        .prepare(
            "SELECT query_sha, query FROM search_query_vectors
             WHERE plugin_key = ?1 AND vector IS NULL
             ORDER BY asked_ms DESC, rowid DESC LIMIT ?2",
        )
        .map_err(storage)?;
    let rows = statement
        .query_map(params![key, QUERY_ROUND_MAX as i64], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .map_err(storage)?;
    let found = rows.collect::<Result<Vec<_>, _>>().map_err(storage)?;
    assert!(found.len() <= QUERY_ROUND_MAX);
    Ok(found)
}

/// Stores the vectors of answered queries and clears their text. Inputs: the connection,
/// the key, the queries and their checked vectors. Output: how many were stored.
fn answer_queries(
    connection: &mut Connection,
    key: &str,
    batch: &[(String, String)],
    vectors: &[Vec<f32>],
) -> Result<usize, EngineError> {
    assert_eq!(batch.len(), vectors.len());
    let tx = immediate(connection)?;
    let mut answered = 0;
    for ((sha, _), vector) in batch.iter().zip(vectors) {
        answered += tx
            .execute(
                "UPDATE search_query_vectors SET vector = ?3, query = NULL
                 WHERE plugin_key = ?1 AND query_sha = ?2 AND vector IS NULL",
                params![key, sha, encode(vector)],
            )
            .map_err(storage)?;
    }
    tx.commit().map_err(storage)?;
    Ok(answered)
}

/// Drops queries whose send failed. Inputs: the connection, the key and the queries.
/// Output: done.
fn forget_queries(
    connection: &mut Connection,
    key: &str,
    queries: &[(String, String)],
) -> Result<(), EngineError> {
    let tx = immediate(connection)?;
    for (sha, _) in queries {
        tx.execute(
            "DELETE FROM search_query_vectors
             WHERE plugin_key = ?1 AND query_sha = ?2 AND vector IS NULL",
            params![key, sha],
        )
        .map_err(storage)?;
    }
    tx.commit().map_err(storage)
}

/// The stored vector of a masked query, once the indexer embedded it. Inputs: the
/// connection, the plugin and the masked query. Output: the vector, or none.
pub(crate) fn cached_query(
    connection: &Connection,
    plugin: &Plugin,
    query: &str,
) -> Option<Vec<f32>> {
    let bytes: Vec<u8> = connection
        .query_row(
            "SELECT vector FROM search_query_vectors
             WHERE plugin_key = ?1 AND query_sha = ?2 AND vector IS NOT NULL",
            params![plugin.key, super::sha256_hex(query.as_bytes())],
            |row| row.get(0),
        )
        .ok()?;
    decode(&bytes, plugin.dimensions)
}

/// Fuses the full-text order with the nearest vectors of a project's source. Inputs: the
/// connection, the project, the source, the plugin, the query vector, the full-text ids best
/// first and the bounds. Output: the fused ids and the note.
pub(crate) fn rank(
    connection: &Connection,
    project: &ProjectId,
    source: Source,
    plugin: &Plugin,
    query: &[f32],
    full_text: &[i64],
    bounds: &Bounds,
) -> Hybrid {
    let _ = (
        connection, project, source, plugin, query, full_text, bounds,
    );
    unimplemented!("v3 hybrid part 3")
}

/// Reciprocal rank fusion of two orders, k [`FUSION_K`]. Inputs: the full-text ids and the
/// vector ids, best first. Output: the fused ids.
pub(crate) fn fuse(full_text: &[i64], nearest: &[i64]) -> Vec<i64> {
    let _ = (full_text, nearest);
    unimplemented!("v3 hybrid part 3")
}

/// How many chunks have a vector of the plugin, against every chunk. Inputs: the connection
/// and the plugin key. Output: the progress.
pub(crate) fn progress(connection: &Connection, key: &str) -> Progress {
    let _ = (connection, key);
    unimplemented!("v3 hybrid part 3")
}
