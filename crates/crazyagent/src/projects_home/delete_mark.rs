use crate::engine::EngineError;
use crate::ledger::{self, Action, ActionKind, Outcome};
use crate::protocol::ProjectId;
use rusqlite::Connection;

/// Marks one Delete everything in the action ledger before it moves anything, so an engine
/// stop that cuts it short is shown to the owner on the next start, never forgotten. A mark
/// of the same project the owner was shown is closed first, since this delete replaces it.
/// Call it inside the plan's write, after every refusal. Inputs: the plan's transaction, the
/// project and the plan's exchange. Output: Ok, or the ledger's code, which refuses the
/// delete with nothing written.
pub(crate) fn begin(
    tx: &Connection,
    project: &ProjectId,
    exchange: i64,
) -> Result<(), EngineError> {
    assert!(exchange > 0);
    ledger::initialize(tx)?;
    if let Some(earlier) = ledger::pending_key(tx, ActionKind::ProjectDelete, project.as_str())? {
        assert_eq!(earlier.key, project.as_str());
        ledger::finish(tx, earlier.id, Outcome::Unknown)?;
    }
    let payload = serde_json::json!({ "project": project.as_str(), "exchange": exchange });
    ledger::begin(
        tx,
        ActionKind::ProjectDelete,
        project.as_str(),
        &payload.to_string(),
    )?;
    let marked = ledger::pending_key(tx, ActionKind::ProjectDelete, project.as_str())?;
    assert!(marked.is_some_and(|action| exchange_of(&action) == Some(exchange)));
    Ok(())
}

/// Ends the mark of one Delete everything once its exchange settled: done when the project's
/// records went, failed when they stayed. A mark the owner is shown is left alone, since an
/// exchange number is unique only within one engine process. Inputs: the settle's
/// transaction and the exchange. Output: how many marks ended.
pub(crate) fn settle(tx: &Connection, exchange: i64) -> Result<usize, EngineError> {
    assert!(exchange > 0);
    ledger::initialize(tx)?;
    let marks = ledger::pending(tx, ActionKind::ProjectDelete)?;
    assert!(marks.len() <= ledger::PENDING_MAX);
    let mut ended = 0;
    for action in marks {
        if ledger::notice(&action).is_some() || exchange_of(&action) != Some(exchange) {
            continue;
        }
        let outcome = if exists(tx, &action.key)? {
            Outcome::Failed
        } else {
            Outcome::Done
        };
        ledger::finish(tx, action.id, outcome)?;
        ended += 1;
    }
    Ok(ended)
}

/// Turns the marks an engine stop left into what the owner sees, as the engine starts: the
/// mark of a project whose records went is ended, and the mark of a project still there
/// becomes a notice on its banner, "A delete was cut short", with its exchange dropped, since
/// a new engine process numbers exchanges again. Nothing is moved or continued. Input: the
/// start's transaction. Output: how many notices the owner is shown.
pub(crate) fn start(tx: &Connection) -> Result<usize, EngineError> {
    ledger::initialize(tx)?;
    let marks = ledger::pending(tx, ActionKind::ProjectDelete)?;
    assert!(marks.len() <= ledger::PENDING_MAX);
    let mut shown = 0;
    for action in marks {
        assert!(ledger::notice(&action).is_none_or(|outcome| outcome == Outcome::Unknown));
        if !exists(tx, &action.key)? {
            let outcome = ledger::notice(&action).unwrap_or(Outcome::Done);
            ledger::finish(tx, action.id, outcome)?;
            continue;
        }
        shown += 1;
        if ledger::notice(&action).is_none() {
            let payload = serde_json::json!({ "project": action.key, "unknown": true });
            ledger::note(tx, action.id, &payload.to_string())?;
        }
    }
    Ok(shown)
}

/// Ends the mark of a project whose records are removed, by Remove from dashboard or by the
/// Delete everything it marks, so a project added again under the same id never inherits it.
/// Inputs: the removal's transaction and the project. Output: whether a mark ended.
pub(crate) fn forget(tx: &Connection, project: &ProjectId) -> Result<bool, EngineError> {
    ledger::initialize(tx)?;
    let marked = ledger::pending_key(tx, ActionKind::ProjectDelete, project.as_str())?;
    let Some(action) = marked else {
        return Ok(false);
    };
    assert_eq!(action.key, project.as_str());
    let outcome = ledger::notice(&action).unwrap_or(Outcome::Done);
    ledger::finish(tx, action.id, outcome)?;
    let left = ledger::pending_key(tx, ActionKind::ProjectDelete, project.as_str())?;
    assert!(left.is_none());
    Ok(true)
}

/// Whether the engine still holds a project's records. Inputs: the connection and the
/// project id. Output: true while its row is there.
fn exists(tx: &Connection, project: &str) -> Result<bool, EngineError> {
    assert!(!project.is_empty());
    tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM projects WHERE id=?1)",
        [project],
        |row| row.get(0),
    )
    .map_err(|e| EngineError(e.to_string()))
}

/// The exchange a live mark names. Input: the mark. Output: the exchange, none for a mark
/// the owner is shown or a payload that names none.
fn exchange_of(action: &Action) -> Option<i64> {
    assert_eq!(action.kind, ActionKind::ProjectDelete);
    let payload: serde_json::Value = serde_json::from_str(&action.payload).ok()?;
    payload.get("exchange")?.as_i64()
}
