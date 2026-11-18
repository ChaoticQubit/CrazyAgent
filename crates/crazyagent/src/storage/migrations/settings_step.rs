use crate::engine::EngineError;
use rusqlite::Transaction;

/// Step 14 (v2.5 L10): the settings aggregate goes on the state log. Settings rows written
/// before the log reach replay through one snapshot run of every settings table, appended
/// through the open's proof; the step makes any settings table missing first, and a store
/// with no settings row gets no run. Input: the open upgrade transaction. Output: Ok or the
/// log's refusal.
pub(super) fn settings_on_the_log(tx: &Transaction<'_>) -> Result<(), EngineError> {
    let at_ms = crate::state_log::replay::Clock::System.now_ms();
    let opening = crate::storage::opening::Opening::new(at_ms);
    crate::settings_log::first_run(tx, opening)?;
    assert!(
        !tx.is_autocommit(),
        "a step leaves the upgrade transaction open"
    );
    Ok(())
}

/// Step 16 (v2.5 L13): master sessions, conversation entries and agent texts go on the state
/// log. Their rows written before the log reach replay through one snapshot run of the three
/// tables, appended through the open's proof, their texts kept as digests; a store with no
/// row in them gets no run. Input: the open upgrade transaction. Output: Ok or the log's
/// refusal.
pub(super) fn sessions_on_the_log(tx: &Transaction<'_>) -> Result<(), EngineError> {
    let at_ms = crate::state_log::replay::Clock::System.now_ms();
    let opening = crate::storage::opening::Opening::new(at_ms);
    crate::sessions_log::first_run(tx, opening)?;
    assert!(
        !tx.is_autocommit(),
        "a step leaves the upgrade transaction open"
    );
    Ok(())
}
