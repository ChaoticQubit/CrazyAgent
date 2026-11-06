use super::{Action, ActionId, ActionKind, FINISHED_KEPT, Outcome, PENDING_MAX, failure, storage};
use crate::engine::EngineError;
use rusqlite::{Connection, OptionalExtension, params};

const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS ledger_actions (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    kind TEXT NOT NULL,
    action_key TEXT NOT NULL,
    payload TEXT NOT NULL,
    started_ms INTEGER NOT NULL,
    outcome TEXT CHECK(outcome IN ('done','failed','unknown')),
    finished_ms INTEGER,
    CHECK((outcome IS NULL) = (finished_ms IS NULL))
);
CREATE UNIQUE INDEX IF NOT EXISTS ledger_actions_pending
    ON ledger_actions(kind, action_key) WHERE outcome IS NULL;";

/// Adds the ledger table and its one-pending-action-per-key index when missing.
/// Input: the connection. Output: done, or `ledger_unavailable`.
pub(super) fn initialize(db: &Connection) -> Result<(), EngineError> {
    db.execute_batch(SCHEMA).map_err(storage)
}

/// Stores one intent in a single statement, so the pending bound and the key's unique
/// index hold across connections. Inputs: the ledger, the kind, key, payload and the
/// time. Output: the new id, `ledger_pending`, `ledger_full` or `ledger_unavailable`.
pub(super) fn insert(
    db: &Connection,
    kind: ActionKind,
    key: &str,
    payload: &str,
    now_ms: i64,
) -> Result<ActionId, EngineError> {
    let added = db
        .execute(
            "INSERT INTO ledger_actions(kind,action_key,payload,started_ms)
             SELECT ?1,?2,?3,?4
             WHERE (SELECT COUNT(*) FROM ledger_actions WHERE outcome IS NULL) < ?5",
            params![kind.name(), key, payload, now_ms, PENDING_MAX as i64],
        )
        .map_err(|error| match error.sqlite_error() {
            Some(code) if code.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE => {
                failure("ledger_pending")
            }
            _ => storage(error),
        })?;
    if added == 0 {
        return Err(failure("ledger_full"));
    }
    Ok(ActionId(db.last_insert_rowid()))
}

/// Sets the outcome of a pending action, of the given kind when one is named, then deletes
/// finished actions older than the FINISHED_KEPT newest. Inputs: the ledger, the kind or
/// none, the id, the outcome and the time. Output: Ok, `ledger_finished`,
/// `ledger_missing` (also for an action of another kind) or `ledger_unavailable`.
pub(super) fn finish(
    db: &Connection,
    kind: Option<ActionKind>,
    id: ActionId,
    outcome: Outcome,
    now_ms: i64,
) -> Result<(), EngineError> {
    let changed = db
        .execute(
            "UPDATE ledger_actions SET outcome=?1,finished_ms=?2
             WHERE id=?3 AND outcome IS NULL AND (?4 IS NULL OR kind=?4)",
            params![outcome.name(), now_ms, id.0, kind.map(ActionKind::name)],
        )
        .map_err(storage)?;
    if changed == 0 {
        return Err(ended(db, kind, id)?);
    }
    db.execute(
        "DELETE FROM ledger_actions WHERE outcome IS NOT NULL AND id <=
            (SELECT id FROM ledger_actions WHERE outcome IS NOT NULL
             ORDER BY id DESC LIMIT 1 OFFSET ?1)",
        [FINISHED_KEPT as i64],
    )
    .map_err(storage)?;
    Ok(())
}

/// Replaces the payload of a pending action. Inputs: the ledger, the id and the payload.
/// Output: Ok, `ledger_finished`, `ledger_missing` or `ledger_unavailable`.
pub(super) fn replace(db: &Connection, id: ActionId, payload: &str) -> Result<(), EngineError> {
    let changed = db
        .execute(
            "UPDATE ledger_actions SET payload=?1 WHERE id=?2 AND outcome IS NULL",
            params![payload, id.0],
        )
        .map_err(storage)?;
    if changed == 0 {
        return Err(ended(db, None, id)?);
    }
    assert_eq!(changed, 1);
    Ok(())
}

/// Names why an action is not pending: `ledger_finished` when it ended, `ledger_missing`
/// when no action of the kind, any when none is named, has that id. Inputs: the ledger,
/// the kind or none and the id. Output: that refusal, or `ledger_unavailable`.
pub(super) fn ended(
    db: &Connection,
    kind: Option<ActionKind>,
    id: ActionId,
) -> Result<EngineError, EngineError> {
    let kept: bool = db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM ledger_actions WHERE id=?1 AND (?2 IS NULL OR kind=?2))",
            params![id.0, kind.map(ActionKind::name)],
            |row| row.get(0),
        )
        .map_err(storage)?;
    Ok(failure(if kept {
        "ledger_finished"
    } else {
        "ledger_missing"
    }))
}

/// Reads the pending actions of one kind, oldest first, at most PENDING_MAX of them.
/// Inputs: the ledger and the kind. Output: the actions, or `ledger_unavailable`.
pub(super) fn pending(db: &Connection, kind: ActionKind) -> Result<Vec<Action>, EngineError> {
    let mut statement = db
        .prepare(
            "SELECT id,action_key,payload,started_ms FROM ledger_actions
             WHERE kind=?1 AND outcome IS NULL ORDER BY id LIMIT ?2",
        )
        .map_err(storage)?;
    let rows = statement
        .query_map(params![kind.name(), PENDING_MAX as i64], |row| {
            Ok(Action {
                id: ActionId(row.get(0)?),
                kind,
                key: row.get(1)?,
                payload: row.get(2)?,
                started_ms: row.get(3)?,
            })
        })
        .map_err(storage)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(storage)
}

/// Reads the pending action of one kind and key. Inputs: the ledger, the kind and the
/// key. Output: the action or none, or `ledger_unavailable`.
pub(super) fn pending_key(
    db: &Connection,
    kind: ActionKind,
    key: &str,
) -> Result<Option<Action>, EngineError> {
    db.query_row(
        "SELECT id,payload,started_ms FROM ledger_actions
         WHERE kind=?1 AND action_key=?2 AND outcome IS NULL",
        params![kind.name(), key],
        |row| {
            Ok(Action {
                id: ActionId(row.get(0)?),
                kind,
                key: key.to_owned(),
                payload: row.get(1)?,
                started_ms: row.get(2)?,
            })
        },
    )
    .optional()
    .map_err(storage)
}
