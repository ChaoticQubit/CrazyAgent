use crate::engine::EngineError;
use crate::landing::{self, ChangeSetId, ChangeState, TurnEnd, Workspace};
use rusqlite::Connection;

/// What one turn end did to its worktree's change set: nothing is pending, this turn
/// added edits to the set, or a set waits that this turn added nothing to. Fields: the
/// change set, when there is one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Landed {
    Nothing,
    Added(ChangeSetId),
    Waiting(ChangeSetId),
}

/// Records one ended turn through `landing::record_turn_end` and tells whether this turn
/// added edits, by comparing its change set's head before and after, so the owner is told
/// once for each turn that changed something rather than at every turn while a set waits.
/// Inputs: the change set store, the workspace and the turn end. Output: Nothing when no
/// set is pending, Added or Waiting with the set, or the landing refusal.
pub(crate) fn record(
    db: &Connection,
    workspace: &Workspace,
    end: &TurnEnd,
) -> Result<Landed, EngineError> {
    let head = |id: Option<&ChangeSetId>| -> Result<Option<String>, EngineError> {
        Ok(landing::list(db, &workspace.project)?
            .into_iter()
            .find(|set| match id {
                Some(id) => &set.id == id,
                None => {
                    set.agent.kind == workspace.agent.kind
                        && set.agent.id == workspace.agent.id
                        && matches!(
                            set.state,
                            ChangeState::Pending | ChangeState::Conflict | ChangeState::Updating
                        )
                }
            })
            .map(|set| set.head))
    };
    let before = head(None)?;
    let Some(id) = landing::record_turn_end(db, workspace, end)? else {
        return Ok(Landed::Nothing);
    };
    let after = head(Some(&id))?;
    if after.is_some() && after == before {
        return Ok(Landed::Waiting(id));
    }
    Ok(Landed::Added(id))
}
