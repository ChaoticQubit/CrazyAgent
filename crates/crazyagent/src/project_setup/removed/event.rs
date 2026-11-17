use crate::engine::EngineError;
use crate::engine::owner::Admission;
use crate::project_setup::SetupAction;
use crate::protocol::{Command, ProjectId, WireRequest};
use crate::state_log::fence::Fence;
use crate::state_log::replay::{Clock, redact_matching};
use crate::state_log::{Aggregate, Appender, Draft};
use rusqlite::Transaction;
use serde::{Deserialize, Serialize};

/// The kind of the event a project removal appends to the state log (v2.5 L14a).
pub(crate) const KIND: &str = "ProjectRemoved";

/// The tables whose rows of the removed project the removal's projection deletes whole, so
/// the removal's own loop over the project's tables skips them; each stays listed for the
/// open-time sweep.
pub(crate) const TABLES: [&str; 2] = ["onboarding", "repository_admissions"];

/// The body of a project removal: the project, and whether its notes are kept (Remove from
/// dashboard) or forgotten (Delete everything). It has no list, so its size is fixed by the
/// longest project id and a removal never fails at append.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ProjectRemoved {
    pub(crate) project: ProjectId,
    pub(crate) kept: bool,
}

impl ProjectRemoved {
    /// The bytes the event stores as its body. Output: the JSON body, or the encoder's
    /// failure.
    pub(crate) fn body(&self) -> Result<Vec<u8>, EngineError> {
        serde_json::to_vec(self).map_err(|error| EngineError(error.to_string()))
    }
}

/// The removal an owner request appends once it applied: Remove from dashboard keeps the
/// project's notes, and Delete everything, its disk effect done, forgets them. Inputs: the
/// request's command and whether it carries Delete everything's effect. Output: the event,
/// or none for any other request.
pub(crate) fn removal(command: &Command, deleting: bool) -> Option<ProjectRemoved> {
    let Command::ProjectSetup { action } = command else {
        return None;
    };
    let (project, kept) = match action {
        SetupAction::Remove { project_id } if !deleting => (project_id, true),
        SetupAction::Delete { project_id, .. } if deleting => (project_id, false),
        _ => return None,
    };
    Some(ProjectRemoved {
        project: project.clone(),
        kept,
    })
}

/// Logs the removal an applied owner request carries through [`append`], and nothing for
/// any other request. Inputs: the request's transaction, the request as stored, whether it
/// carries Delete everything's effect, the revision it moved to and the admission. Output:
/// how many events the removal redacted, 0 for any other request, or the first refusal.
pub(crate) fn record(
    tx: &Transaction<'_>,
    wire: &WireRequest,
    deleting: bool,
    revision: u64,
    admission: &Admission,
) -> Result<usize, EngineError> {
    match removal(&wire.command, deleting) {
        Some(removed) => append(tx, revision, admission, &removed),
        None => Ok(0),
    }
}

/// Appends a removal at the request's moved revision behind the owner's admission, applies
/// its projection through the appender, then redacts every earlier ide event naming the
/// project (rv-sec2 #309 N1); the caller checkpoints the WAL after commit. Inputs: the
/// transaction, the moved revision, the admission and the event. Output: how many events
/// were redacted, or the fence's, the revision's, a bound's or the redaction's refusal.
fn append(
    tx: &Transaction<'_>,
    revision: u64,
    admission: &Admission,
    removed: &ProjectRemoved,
) -> Result<usize, EngineError> {
    let body = removed.body()?;
    let fence = Fence::Owner(admission.clone());
    let mut log = Appender::owner(tx, revision, fence.clone())?;
    let seq = log.append(&Draft {
        aggregate: Aggregate::Projects,
        kind: KIND,
        schema: 1,
        at_ms: Clock::System.now_ms(),
        body: &body,
    })?;
    assert!(seq > 0, "an appended removal has a seq");
    log.project(|tx| apply(tx, &body))?;
    redact_matching(
        tx,
        &[Aggregate::Ide],
        Some(crate::ide::log::KINDS.as_slice()),
        "project",
        removed.project.as_str(),
        seq,
        &fence,
    )
}

/// The projection of a removal, live and in replay: forgets the removed project's GitHub
/// rule proposals, schedule override (moving the schedule's version), coordination with its
/// stopped and failed agents' counts, activity and IDE rows with its agents' unlocked
/// files, and moves its usage to `<removed>` (v2.5 L14b, L7); then deletes its agent texts
/// and their instruction versions,
/// questions, hidden folders and the agents that can see them, agent access, grants and
/// access requests, its stopped and failed agents with their panes, and its rows in
/// [`TABLES`]. Inputs: the transaction and the event body. Output: Ok,
/// `project_removed_body` for a body that is not a removal, or a store failure.
pub(crate) fn apply(tx: &Transaction<'_>, body: &[u8]) -> Result<(), EngineError> {
    let removed: ProjectRemoved =
        serde_json::from_slice(body).map_err(|_| EngineError("project_removed_body".into()))?;
    let failed = |error: rusqlite::Error| EngineError(error.to_string());
    let project = removed.project.as_str();
    crate::github::proposals::forget_project(tx, &removed.project)?;
    crate::scheduling::forget_project(tx, project)?;
    crate::coord::forget_project(tx, &removed.project)?;
    crate::activity::forget_project(tx, &removed.project)?;
    crate::usage::forget_project(tx, project)?;
    crate::ide::log::forget_project(tx, &removed.project)?;
    crate::storage::agent_text::forget_project(tx, &removed.project)?;
    crate::questions::forget_project(tx, &removed.project)?;
    crate::sandbox::hidden_paths::save(tx, project, &[]).map_err(failed)?;
    crate::sandbox::hidden_paths::forget_seers(tx, project).map_err(failed)?;
    crate::sandbox::policy::remove(tx, project).map_err(failed)?;
    crate::sandbox::grants::forget_project(tx, project).map_err(failed)?;
    tx.execute(
        "DELETE FROM team_panes WHERE agent IN
         (SELECT id FROM team_agents WHERE project=?1 AND status IN ('stopped','failed'))",
        [project],
    )
    .map_err(failed)?;
    tx.execute(
        "DELETE FROM team_agents WHERE project=?1 AND status IN ('stopped','failed')",
        [project],
    )
    .map_err(failed)?;
    for table in TABLES {
        tx.execute(&format!("DELETE FROM {table} WHERE project=?1"), [project])
            .map_err(failed)?;
    }
    Ok(())
}
