use rusqlite::Transaction;

use crate::engine::EngineError;

/// Clears the three plugin store tables and writes them again from the plugin store
/// events of the log, the way they were written live. Input: the transaction. Output: the
/// last seq replayed, or the log's refusal.
pub fn rebuild(tx: &Transaction<'_>) -> Result<u64, EngineError> {
    let _ = tx;
    todo!()
}
