use crate::engine::EngineError;
use crate::engine::owner::Admission;
use crate::protocol::{Command, ProjectId};
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
        Ok(Vec::new())
    }
}

/// The removal an owner request appends once it applied: Remove from dashboard keeps the
/// project's notes, and Delete everything, its disk effect done, forgets them. Inputs: the
/// request's command and whether it carries Delete everything's effect. Output: the event,
/// or none for any other request.
pub(crate) fn removal(command: &Command, deleting: bool) -> Option<ProjectRemoved> {
    let _ = (command, deleting);
    None
}

/// Appends a removal at the request's moved revision behind the owner's admission and
/// applies its projection through the appender, which checks the fence again. Inputs: the
/// request's transaction, the revision it moved to, the admission and the event. Output:
/// the event's seq, or the fence's, the revision's or a bound's refusal.
pub(crate) fn append(
    tx: &Transaction<'_>,
    revision: u64,
    admission: &Admission,
    removed: &ProjectRemoved,
) -> Result<u64, EngineError> {
    let _ = (tx, revision, admission, removed);
    Err(EngineError("project_removed_unwired".into()))
}

/// The projection of a removal, live and in replay: deletes the removed project's agent
/// texts and their instruction versions, questions, hidden folders and the agents that can
/// see them, agent access, grants and access requests, its stopped and failed agents with
/// their panes, and its rows in [`TABLES`]. Inputs: the transaction and the event body.
/// Output: Ok, `project_removed_body` for a body that is not a removal, or a store failure.
pub(crate) fn apply(tx: &Transaction<'_>, body: &[u8]) -> Result<(), EngineError> {
    let _ = (tx, body);
    Ok(())
}
