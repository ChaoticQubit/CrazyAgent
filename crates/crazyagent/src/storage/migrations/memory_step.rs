use crate::engine::EngineError;
use rusqlite::Transaction;

/// Step 15 (v2.5 L8): the memory aggregate goes on the state log. Memory rows written before
/// the log reach replay through one snapshot run of the 14 memory tables, appended through
/// the open's proof; a store with no memory row, or whose memory is already on the log, gets
/// no run. Input: the open upgrade transaction. Output: Ok or the log's refusal.
pub(super) fn memory_on_the_log(tx: &Transaction<'_>) -> Result<(), EngineError> {
    let at_ms = crate::state_log::replay::Clock::System.now_ms();
    let opening = crate::storage::opening::Opening::new(at_ms);
    crate::memory::log::first_run(tx, opening)?;
    assert!(
        !tx.is_autocommit(),
        "a step leaves the upgrade transaction open"
    );
    Ok(())
}
