use crate::engine::EngineError;
use crate::engine::owner::Admission;
use crate::project_setup::SetupAction;
use crate::protocol::{Command, ProjectId};
use crate::state_log::fence::Fence;
use crate::state_log::replay::Clock;
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

/// The removal an owner request would append, with the agent worktrees a Delete everything
/// forgets (rv-sec3 #415 F1), read before the request deletes the project's worktree rows;
/// Remove from dashboard keeps them, as it keeps the change sets naming them. Inputs: the
/// request's transaction, its command and whether it carries Delete everything's effect.
/// Output: the removal and its worktrees, none for any other request, or the read's refusal.
pub(crate) fn removing(
    tx: &Transaction<'_>,
    command: &Command,
    deleting: bool,
) -> Result<Option<(ProjectRemoved, Vec<String>)>, EngineError> {
    let Some(removed) = removal(command, deleting) else {
        return Ok(None);
    };
    assert_eq!(removed.kept, !deleting);
    let worktrees = if removed.kept {
        Vec::new()
    } else {
        crate::landing_log::project_worktrees(tx, removed.project.as_str())?
    };
    Ok(Some((removed, worktrees)))
}

/// Appends a removal and tombstones its project's earlier events, keeping its own: its
/// admissions, ide and settings events and, for a Delete everything, the landing events of
/// its agent worktrees, whose rows go in this transaction too. Inputs: the request's
/// transaction, the moved revision, the admission, the removal and its worktrees. Output:
/// how many events were tombstoned, or the first refusal.
pub(crate) fn settle(
    tx: &Transaction<'_>,
    revision: u64,
    admission: &Admission,
    removed: &ProjectRemoved,
    worktrees: &[String],
) -> Result<usize, EngineError> {
    assert!(!removed.kept || worktrees.is_empty());
    let seq = append(tx, revision, admission, removed)?;
    let fence = Fence::Owner(admission.clone());
    let project = removed.project.as_str();
    let mut redacted = crate::worktrees::log::forget_project(tx, project, Some(seq), &fence)?;
    redacted += crate::ide::log::forget_project(tx, project, Some(seq), &fence)?;
    redacted += crate::settings_log::forget_project(tx, project, Some(seq), &fence)?;
    redacted += crate::landing_log::forget_worktrees(tx, worktrees, &fence)?;
    Ok(redacted)
}

/// Appends a removal at the request's moved revision behind the owner's admission and
/// applies its projection through the appender, which checks the fence again; the project's
/// agent texts are marked first, so the request's sessions close after it logs them dropped
/// and lets their texts go, with the sessions the removal closed (v2.5 L13). Inputs: the
/// request's transaction, the revision it moved to, the admission and the event. Output:
/// the event's seq, or the fence's, the revision's or a bound's refusal.
pub(crate) fn append(
    tx: &Transaction<'_>,
    revision: u64,
    admission: &Admission,
    removed: &ProjectRemoved,
) -> Result<u64, EngineError> {
    let body = removed.body()?;
    let mut log = Appender::owner(tx, revision, Fence::Owner(admission.clone()))?;
    let seq = log.append(&Draft {
        aggregate: Aggregate::Projects,
        kind: KIND,
        schema: 1,
        at_ms: Clock::System.now_ms(),
        body: &body,
    })?;
    crate::sessions_log::mark_project_texts(tx, removed.project.as_str())?;
    log.project(|tx| apply(tx, &body))?;
    crate::sessions_log::close(&mut log)?;
    Ok(seq)
}

/// The projection of a removal, live and in replay (v2.5 L14b, L7, L9): first the project's
/// rule proposals, a Delete's publish prompt answer, schedule override and version,
/// coordination with its stopped and failed agents' counts, activity, IDE rows and usage (to
/// `<removed>`); then its agent texts, questions, hidden folders, access, grants, stopped and
/// failed agents and [`TABLES`] rows. Inputs: the transaction and the body. Output: Ok,
/// `project_removed_body`, or a store failure.
pub(crate) fn apply(tx: &Transaction<'_>, body: &[u8]) -> Result<(), EngineError> {
    let removed: ProjectRemoved =
        serde_json::from_slice(body).map_err(|_| EngineError("project_removed_body".into()))?;
    let failed = |error: rusqlite::Error| EngineError(error.to_string());
    let project = removed.project.as_str();
    crate::github::proposals::forget_project(tx, &removed.project)?;
    if !removed.kept {
        crate::github::forget_prompt(tx, &removed.project)?;
    }
    crate::scheduling::forget_project(tx, project)?;
    crate::coord::forget_project(tx, &removed.project)?;
    crate::activity::forget_project(tx, &removed.project)?;
    crate::usage::forget_project(tx, project)?;
    crate::ide::log::forget_rows(tx, &removed.project)?;
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
