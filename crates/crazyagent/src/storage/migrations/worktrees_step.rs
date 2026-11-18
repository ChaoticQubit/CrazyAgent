use crate::engine::EngineError;
use rusqlite::Transaction;

/// Step 13 (v2.5 L6): the worktrees aggregate goes on the state log. Worktree rows written
/// before the log reach replay through one snapshot run, appended through the open's proof
/// since no engine host runs during an upgrade; a store with no worktree row gets none.
/// Input: the open upgrade transaction. Output: Ok or the log's refusal.
pub(super) fn worktrees_on_the_log(tx: &Transaction<'_>) -> Result<(), EngineError> {
    let at_ms = crate::state_log::replay::Clock::System.now_ms();
    let opening = crate::storage::opening::Opening::new(at_ms);
    crate::worktrees::log::first_run(tx, opening)?;
    assert!(
        !tx.is_autocommit(),
        "a step leaves the upgrade transaction open"
    );
    Ok(())
}
