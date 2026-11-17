use crate::engine::EngineError;
use rusqlite::Transaction;

/// Step 15 (v2.5 L9): the landing tables go on the state log. Rows written before the log
/// reach replay through one snapshot run of every landing table the store has, appended
/// through the open's proof; a store with no landing row gets no run. Input: the open
/// upgrade transaction. Output: Ok or the log's refusal.
pub(super) fn landing_on_the_log(tx: &Transaction<'_>) -> Result<(), EngineError> {
    assert!(
        !tx.is_autocommit(),
        "a step runs inside the upgrade transaction"
    );
    let at_ms = crate::state_log::replay::Clock::System.now_ms();
    let opening = crate::storage::opening::Opening::new(at_ms);
    crate::landing_log::first_run(tx, opening)?;
    assert!(
        !tx.is_autocommit(),
        "a step leaves the upgrade transaction open"
    );
    Ok(())
}
