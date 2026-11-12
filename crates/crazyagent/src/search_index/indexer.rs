use super::*;
use std::path::Path;

/// Indexes one project's new messages within the engine's bounds. Inputs: the database, the
/// agents folder, the project and the time. Output: whether the project is caught up.
pub(crate) fn index_project(
    _db: &mut Connection,
    _agents: &Path,
    _project: &ProjectId,
    _now_ms: i64,
) -> Result<bool, EngineError> {
    unimplemented!("search index slice 2")
}

/// Indexes one project's new messages within given bounds. Inputs: the database, the agents
/// folder, the project, the time and the most chunks and bytes. Output: whether the project
/// is caught up.
pub(crate) fn index_within(
    _db: &mut Connection,
    _agents: &Path,
    _project: &ProjectId,
    _now_ms: i64,
    _chunks_max: usize,
    _bytes_max: u64,
) -> Result<bool, EngineError> {
    unimplemented!("search index slice 2")
}

/// Removes the index rows of projects that are gone. Input: the database. Output: done.
pub(crate) fn sweep_projects(_db: &Connection) -> Result<(), EngineError> {
    unimplemented!("search index slice 2")
}

/// Brings the memory chunks in line with the memories agents may read. Inputs: the
/// database and the time. Output: done, or the failure.
pub(crate) fn sync_memories(_db: &mut Connection, _now_ms: i64) -> Result<(), EngineError> {
    unimplemented!("search index slice 2")
}

/// What one round read of a project, kept until it is stored.
pub(crate) struct Round;

/// Reads one project's new messages within bounds, storing nothing. Inputs: the database,
/// the agents folder, the project, the time and the bounds. Output: the round.
pub(crate) fn read_round(
    _db: &Connection,
    _agents: &Path,
    _project: &ProjectId,
    _now_ms: i64,
    _chunks_max: usize,
    _bytes_max: u64,
) -> Result<Round, EngineError> {
    unimplemented!("search index slice 2")
}

/// Stores a round inside one write transaction. Inputs: the database, the round and the
/// time. Output: whether the project is caught up.
pub(crate) fn store_round(
    _db: &mut Connection,
    _round: Round,
    _now_ms: i64,
) -> Result<bool, EngineError> {
    unimplemented!("search index slice 2")
}

/// Runs one engine tick. Inputs: the database, the engine root, the agents folder and the
/// time. Output: whether a project is still behind.
pub(crate) fn tick(
    _db: &mut Connection,
    _root: &Path,
    _agents: &Path,
    _now_ms: i64,
) -> Result<bool, EngineError> {
    unimplemented!("search index slice 2")
}
