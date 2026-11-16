use crate::engine::EngineError;
use crate::ledger::{self, Action, ActionKind, Outcome};
use crate::protocol::{ProjectId, RequestId};
use rusqlite::Connection;

/// A New Project an engine stop cut short, as its mark names it: the project, the job's
/// request and the project's name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Interrupted {
    pub project: ProjectId,
    pub request: RequestId,
    pub name: String,
}

/// Marks one New Project in the action ledger before its folder is built, so an engine stop
/// that cuts it short is shown on the next start as the engine's job, failed while building
/// (DEC-177 addendum 2, Q3 A). A mark of the same project still open belongs to an earlier
/// build that never recorded, and ends failed first. Call it after every refusal of the plan.
/// Inputs: the connection, the project, the build's request and the project's name. Output:
/// Ok, or the ledger's failure.
pub(crate) fn begin(
    tx: &Connection,
    project: &ProjectId,
    request: &RequestId,
    name: &str,
) -> Result<(), EngineError> {
    assert!(!name.is_empty());
    ledger::initialize(tx)?;
    if let Some(earlier) = ledger::pending_key(tx, ActionKind::ProjectSetup, project.as_str())? {
        assert_eq!(earlier.key, project.as_str());
        ledger::finish(tx, earlier.id, Outcome::Failed)?;
    }
    let payload = serde_json::json!({
        "project": project.as_str(),
        "request": request.as_str(),
        "name": name,
    });
    ledger::begin(
        tx,
        ActionKind::ProjectSetup,
        project.as_str(),
        &payload.to_string(),
    )?;
    let marked = ledger::pending_key(tx, ActionKind::ProjectSetup, project.as_str())?;
    assert!(marked.is_some_and(|action| interrupted(&action).is_some()));
    Ok(())
}

/// Ends the mark of one New Project with its record: done when the project was recorded,
/// failed when the record refused it. Inputs: the record's transaction, the project and
/// whether it was recorded. Output: whether a mark ended; false for a project built with no
/// mark, such as the demo.
pub(crate) fn end(
    tx: &Connection,
    project: &ProjectId,
    recorded: bool,
) -> Result<bool, EngineError> {
    ledger::initialize(tx)?;
    let Some(action) = ledger::pending_key(tx, ActionKind::ProjectSetup, project.as_str())? else {
        return Ok(false);
    };
    assert_eq!(action.key, project.as_str());
    let outcome = match recorded {
        true => Outcome::Done,
        false => Outcome::Failed,
    };
    ledger::finish(tx, action.id, outcome)?;
    let left = ledger::pending_key(tx, ActionKind::ProjectSetup, project.as_str())?;
    assert!(left.is_none());
    Ok(true)
}

/// Ends every mark an engine stop left, as the engine starts: done when its project was
/// recorded, else failed, nothing built continued. Inputs: the start's transaction. Output:
/// the newest New Project that ended failed and whose mark names its job, for the start to
/// show as the engine's job cut short, or None.
pub(crate) fn start(tx: &Connection) -> Result<Option<Interrupted>, EngineError> {
    ledger::initialize(tx)?;
    let marks = ledger::pending(tx, ActionKind::ProjectSetup)?;
    assert!(marks.len() <= ledger::PENDING_MAX);
    let mut newest: Option<((i64, i64), Interrupted)> = None;
    for action in marks {
        if recorded(tx, &action.key)? {
            ledger::finish(tx, action.id, Outcome::Done)?;
            continue;
        }
        ledger::finish(tx, action.id, Outcome::Failed)?;
        let Some(found) = interrupted(&action) else {
            continue;
        };
        let order = (action.started_ms, action.id.get());
        if newest.as_ref().is_none_or(|(seen, _)| order > *seen) {
            newest = Some((order, found));
        }
    }
    assert!(ledger::pending(tx, ActionKind::ProjectSetup)?.is_empty());
    Ok(newest.map(|(_, found)| found))
}

/// The New Project a mark names, read back from its payload. Input: the mark. Output: the
/// New Project, or None for a payload that names no request or name.
fn interrupted(action: &Action) -> Option<Interrupted> {
    assert_eq!(action.kind, ActionKind::ProjectSetup);
    let payload: serde_json::Value = serde_json::from_str(&action.payload).ok()?;
    let request = RequestId::new(payload.get("request")?.as_str()?).ok()?;
    let name = payload.get("name")?.as_str()?.to_owned();
    let project = ProjectId::new(&action.key).ok()?;
    (!name.is_empty()).then_some(Interrupted {
        project,
        request,
        name,
    })
}

/// Whether the engine holds a project's records. Inputs: the connection and the project id.
/// Output: true while its row is there.
fn recorded(tx: &Connection, project: &str) -> Result<bool, EngineError> {
    assert!(!project.is_empty());
    tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM projects WHERE id=?1)",
        [project],
        |row| row.get(0),
    )
    .map_err(|e| EngineError(e.to_string()))
}
