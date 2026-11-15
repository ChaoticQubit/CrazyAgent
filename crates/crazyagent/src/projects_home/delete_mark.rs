use crate::engine::EngineError;
use crate::ledger::{self, Action, ActionKind, Outcome};
use crate::protocol::{ProjectId, RequestId};
use rusqlite::Connection;

/// The fields of a mark the engine's Delete everything job needs to show it again after an
/// engine start: its request, the project's name and how many folders it moves.
const JOB_FIELDS: [&str; 3] = ["request", "name", "total"];

/// A Delete everything an engine stop, or a record that never came after its Trash moves, cut
/// short, as its mark names it: the project, the job's request, the project's name and how
/// many folders the delete moves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CutShort {
    pub project: ProjectId,
    pub request: RequestId,
    pub name: String,
    pub total: u64,
}

/// Marks one Delete everything in the action ledger before it moves anything, so an engine
/// stop that cuts it short is shown to the owner on the next start, never forgotten. A mark
/// of the same project the owner was shown is closed first, since this delete replaces it, and
/// the new mark carries that it was cut short, so this delete keeps the notice if it moves
/// nothing either (RV-DS241-141 r2); a live mark means another Delete everything of the project
/// still runs, which refuses this one. The mark names the delete's request, the project's name
/// and how many folders it moves, so an engine start can show it as the engine's job (DEC-177
/// addendum 1). Call it inside the plan's write, after every other refusal. Inputs: the
/// plan's transaction, the project, the plan's exchange, the request, the project's name and
/// the folder count. Output: Ok(Ok), Ok(Err) with `delete_in_progress`, or the ledger's
/// failure.
pub(crate) fn begin(
    tx: &Connection,
    project: &ProjectId,
    exchange: i64,
    request: &RequestId,
    name: &str,
    total: u64,
) -> Result<Result<(), &'static str>, EngineError> {
    assert!(exchange > 0);
    assert!(!name.is_empty());
    ledger::initialize(tx)?;
    let mut payload = serde_json::json!({
        "project": project.as_str(),
        "exchange": exchange,
        "request": request.as_str(),
        "name": name,
        "total": total,
    });
    if let Some(earlier) = ledger::pending_key(tx, ActionKind::ProjectDelete, project.as_str())? {
        assert_eq!(earlier.key, project.as_str());
        if ledger::notice(&earlier).is_none() {
            return Ok(Err("delete_in_progress"));
        }
        ledger::finish(tx, earlier.id, Outcome::Unknown)?;
        payload["cut_short"] = serde_json::Value::Bool(true);
    }
    ledger::begin(
        tx,
        ActionKind::ProjectDelete,
        project.as_str(),
        &payload.to_string(),
    )?;
    let marked = ledger::pending_key(tx, ActionKind::ProjectDelete, project.as_str())?;
    assert!(marked.is_some_and(|action| exchange_of(&action) == Some(exchange)));
    Ok(Ok(()))
}

/// Ends the mark of one Delete everything once its exchange settled: done when the project's
/// records went, failed when they stayed with nothing moved. When they stayed after its
/// folders reached the Trash (a record refused, cut off or failed), or after a delete it
/// replaced was cut short, the mark is kept as "A delete was cut short" on the project's
/// banner instead, its exchange dropped, as an engine start shows it. A mark the owner is
/// shown is left alone, since an exchange number is unique only within one engine process.
/// Inputs: the transaction, the exchange and whether its folders reached the Trash. Output:
/// how many marks ended or were shown.
pub(crate) fn settle(tx: &Connection, exchange: i64, trashed: bool) -> Result<usize, EngineError> {
    assert!(exchange > 0);
    ledger::initialize(tx)?;
    let marks = ledger::pending(tx, ActionKind::ProjectDelete)?;
    assert!(marks.len() <= ledger::PENDING_MAX);
    let mut ended = 0;
    for action in marks {
        if ledger::notice(&action).is_some() || exchange_of(&action) != Some(exchange) {
            continue;
        }
        ended += 1;
        if !exists(tx, &action.key)? {
            ledger::finish(tx, action.id, Outcome::Done)?;
        } else if trashed || replaced_cut_short(&action) {
            ledger::note(tx, action.id, &shown(&action).to_string())?;
        } else {
            ledger::finish(tx, action.id, Outcome::Failed)?;
        }
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
            ledger::note(tx, action.id, &self::shown(&action).to_string())?;
        }
    }
    Ok(shown)
}

/// The newest delete the owner is shown as cut short whose mark names its job, for an engine
/// start to show as the engine's job. A mark of a project whose records went, or one a 2.4.1
/// delete left without the job's fields, gives none. Input: the connection. Output: the
/// delete, or None.
pub(crate) fn cut_short(tx: &Connection) -> Result<Option<CutShort>, EngineError> {
    ledger::initialize(tx)?;
    let marks = ledger::pending(tx, ActionKind::ProjectDelete)?;
    assert!(marks.len() <= ledger::PENDING_MAX);
    let mut newest: Option<((i64, i64), CutShort)> = None;
    for action in marks {
        if ledger::notice(&action).is_none() || !exists(tx, &action.key)? {
            continue;
        }
        let Some(found) = job_of(&action) else {
            continue;
        };
        let order = (action.started_ms, action.id.get());
        if newest.as_ref().is_none_or(|(seen, _)| order > *seen) {
            newest = Some((order, found));
        }
    }
    Ok(newest.map(|(_, found)| found))
}

/// Keeps the project of a delete the owner is shown as cut short, named by its job's request:
/// ends its mark with the outcome it showed, as the banner's Dismiss does. Inputs: the
/// transaction and the request. Output: whether a mark ended; false for a live delete or none.
pub(crate) fn keep(tx: &Connection, request: &RequestId) -> Result<bool, EngineError> {
    ledger::initialize(tx)?;
    let marks = ledger::pending(tx, ActionKind::ProjectDelete)?;
    assert!(marks.len() <= ledger::PENDING_MAX);
    let named = marks.into_iter().find(|action| {
        ledger::notice(action).is_some()
            && job_of(action).is_some_and(|job| &job.request == request)
    });
    let Some(action) = named else {
        return Ok(false);
    };
    let shown = ledger::dismiss(tx, ActionKind::ProjectDelete, action.id.get())?;
    assert_eq!(shown, Outcome::Unknown);
    Ok(true)
}

/// The payload a mark gets when the owner is shown it as cut short: its project, the unknown
/// outcome and the job's fields it carried, with its exchange dropped. Input: the mark.
/// Output: the payload.
fn shown(action: &Action) -> serde_json::Value {
    assert_eq!(action.kind, ActionKind::ProjectDelete);
    let mut payload = serde_json::json!({ "project": action.key, "unknown": true });
    let old = serde_json::from_str::<serde_json::Value>(&action.payload).unwrap_or_default();
    for field in JOB_FIELDS {
        if let Some(value) = old.get(field) {
            payload[field] = value.clone();
        }
    }
    assert!(payload.get("exchange").is_none());
    payload
}

/// The job a mark names, read back from its payload. Input: the mark. Output: the delete, or
/// None for a payload that names no request, name or count.
fn job_of(action: &Action) -> Option<CutShort> {
    assert_eq!(action.kind, ActionKind::ProjectDelete);
    let payload: serde_json::Value = serde_json::from_str(&action.payload).ok()?;
    let request = RequestId::new(payload.get("request")?.as_str()?).ok()?;
    let name = payload.get("name")?.as_str()?.to_owned();
    let total = payload.get("total")?.as_u64()?;
    let project = ProjectId::new(&action.key).ok()?;
    (!name.is_empty()).then_some(CutShort {
        project,
        request,
        name,
        total,
    })
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

/// Whether a live mark replaced a delete of its project that was cut short, so what that one
/// moved can still be in the Trash. Input: the mark. Output: the answer, false for a payload
/// that does not parse.
fn replaced_cut_short(action: &Action) -> bool {
    assert_eq!(action.kind, ActionKind::ProjectDelete);
    serde_json::from_str::<serde_json::Value>(&action.payload)
        .ok()
        .and_then(|payload| payload.get("cut_short")?.as_bool())
        .unwrap_or(false)
}

/// The exchange a live mark names. Input: the mark. Output: the exchange, none for a mark
/// the owner is shown or a payload that names none.
fn exchange_of(action: &Action) -> Option<i64> {
    assert_eq!(action.kind, ActionKind::ProjectDelete);
    let payload: serde_json::Value = serde_json::from_str(&action.payload).ok()?;
    payload.get("exchange")?.as_i64()
}
