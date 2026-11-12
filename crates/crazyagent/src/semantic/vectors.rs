use super::{Hybrid, Progress};
use crate::engine::EngineError;
use crate::protocol::ProjectId;
use crate::search_index::Source;
use rusqlite::Connection;

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
pub(crate) const DIMENSIONS_MIN: usize = 64;
pub(crate) const DIMENSIONS_MAX: usize = 1024;
/// The most bytes of a plugin key: a plugin name, a colon and a SHA-256.
pub(crate) const KEY_BYTES_MAX: usize = 128;

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
        let _ = (name, sha256, dimensions, batch);
        unimplemented!("v3 hybrid part 1")
    }
}

/// One call that turns texts into vectors, one per text in order.
pub(crate) type Embed<'a> = dyn FnMut(&[&str]) -> Result<Vec<Vec<f32>>, EngineError> + 'a;

/// Creates the vector tables and the triggers that drop a chunk's vectors in the same
/// transaction that deletes or rewrites it, and sweeps vectors left by chunks deleted before.
/// Input: the engine database, after the search index tables. Output: done, or the failure.
pub(crate) fn initialize(connection: &Connection) -> Result<(), EngineError> {
    let _ = connection;
    unimplemented!("v3 hybrid part 1")
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
    let _ = (connection, project, plugin, bounds, embed);
    unimplemented!("v3 hybrid part 2")
}

/// Masks, trims and cuts an untrusted query before it is hashed, stored, embedded or sent.
/// Input: the query. Output: the text, or none when nothing is left.
pub(crate) fn query_text(query: &str) -> Option<String> {
    let _ = query;
    unimplemented!("v3 hybrid part 2")
}

/// Records a query an HTTP plugin must embed on the indexer thread. Inputs: the connection,
/// the plugin key, the masked query and the time. Output: done, or the failure.
pub(crate) fn ask_query(
    connection: &Connection,
    key: &str,
    query: &str,
    now_ms: i64,
) -> Result<(), EngineError> {
    let _ = (connection, key, query, now_ms);
    unimplemented!("v3 hybrid part 2")
}

/// Embeds the queries waiting for the plugin, newest first, at most [`QUERY_ROUND_MAX`].
/// Inputs: the connection, the plugin and the embed call. Output: how many were embedded.
pub(crate) fn embed_queries(
    connection: &mut Connection,
    plugin: &Plugin,
    embed: &mut Embed<'_>,
) -> usize {
    let _ = (connection, plugin, embed);
    unimplemented!("v3 hybrid part 2")
}

/// The stored vector of a masked query, once the indexer embedded it. Inputs: the
/// connection, the plugin and the query. Output: the vector, or none.
pub(crate) fn cached_query(connection: &Connection, plugin: &Plugin, query: &str) -> Option<Vec<f32>> {
    let _ = (connection, plugin, query);
    unimplemented!("v3 hybrid part 2")
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
    let _ = (connection, project, source, plugin, query, full_text, bounds);
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
