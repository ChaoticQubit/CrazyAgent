//! The Keep awake notices (DEC-167 addendum 10): a hold that ended by itself and heat a
//! hold runs on through, both Info items the keeper writes inside its own change, and the
//! sleep changes an earlier engine could not put back, an Action kind whose state is the
//! `keep_awake_restore` table the keeper fills after the engine start's undo.

use super::error;
use crate::engine::EngineError;
use crate::keep_awake::{EndReason, HeatEnd, Leftover};
use rusqlite::Connection;

const TABLE: &str = "CREATE TABLE IF NOT EXISTS keep_awake_restore (
    subject TEXT PRIMARY KEY CHECK(length(subject) BETWEEN 1 AND 128),
    unreadable INTEGER NOT NULL CHECK(unreadable IN (0,1))
)";

/// Creates the restore state when missing. Input: the engine database. Output: Ok, or the
/// SQLite failure.
pub(super) fn initialize(connection: &Connection) -> Result<(), EngineError> {
    connection.execute_batch(TABLE).map_err(error)
}

/// The line of a hold that ended by itself. Not written yet: says nothing. Input: the end
/// reason. Output: none.
pub fn ended_text(_ended: EndReason) -> Option<String> {
    None
}

/// The line of heat a hold runs on through. Not written yet. Input: the setting. Output: an
/// empty line.
pub fn hot_text(_heat_end: HeatEnd) -> &'static str {
    ""
}

/// Records one hold that ended by itself. Not written yet: records nothing. Inputs: the
/// connection, the end reason, the hold start and the time. Output: none.
pub(crate) fn hold_ended(_connection: &Connection, _ended: EndReason, _since_ms: i64, _now: i64) {}

/// Records heat a hold runs on through. Not written yet: records nothing. Inputs: the
/// connection, the setting, the hold start and the time. Output: none.
pub(crate) fn running_hot(_connection: &Connection, _heat_end: HeatEnd, _since_ms: i64, _now: i64) {
}

/// Replaces the restore state. Not written yet: changes nothing. Inputs: the engine
/// database and what was left. Output: Ok.
pub(crate) fn left_over(_connection: &Connection, _leftover: &Leftover) -> Result<(), EngineError> {
    Ok(())
}
