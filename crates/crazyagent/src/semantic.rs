use crate::protocol::ProjectId;
use rusqlite::Connection;
use std::path::Path;

/// The search order after fusing full text with vectors: chunk ids best first, and a note
/// the answer carries when the vector part could not be used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hybrid {
    pub ids: Vec<i64>,
    pub note: Option<&'static str>,
}

/// Embeds the project's chunks that have no vector for the chosen plugin yet, on the
/// indexer thread outside any transaction. Inputs: the indexer's connection, the engine
/// root and the project. Output: how many chunks were embedded; this stub embeds none
/// until the w-v3-plugins vectors land.
pub fn embed_pending(connection: &mut Connection, root: &Path, project: &ProjectId) -> usize {
    let _ = (connection, root, project);
    0
}

/// Fuses the full-text order with the chosen plugin's vector order, on the tick and with
/// no network. Inputs: the connection, the engine root, the project, the source, the query
/// and the full-text chunk ids best first. Output: the fused ids and an optional note; this
/// stub keeps the full-text order.
pub fn hybrid(
    connection: &Connection,
    root: &Path,
    project: &ProjectId,
    source: crate::search_index::Source,
    query: &str,
    full_text: &[i64],
) -> Hybrid {
    let _ = (connection, root, project, source, query);
    Hybrid {
        ids: full_text.to_vec(),
        note: None,
    }
}
