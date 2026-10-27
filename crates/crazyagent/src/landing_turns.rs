use crate::engine::EngineError;
use crate::landing::{self, AgentKind, ChangeSetId, ChangeState, TurnEnd, Workspace};
use crate::protocol::ProjectId;
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior};

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
/// added edits or finished an update, by comparing its change set's head and state before
/// and after, so the owner is told once for each turn that changed something rather than
/// at every turn while a set waits. Inputs: the change set store, the workspace and the
/// turn end. Output: Nothing when no set is pending, Added or Waiting, or the refusal.
pub(crate) fn record(
    db: &Connection,
    workspace: &Workspace,
    end: &TurnEnd,
) -> Result<Landed, EngineError> {
    let head = |id: Option<&ChangeSetId>| -> Result<Option<(String, ChangeState)>, EngineError> {
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
            .map(|set| (set.head, set.state)))
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

const RESOLVE_PROMPT_BYTES_MAX: usize = crate::team::TASK_PROMPT_BYTES_MAX;

/// Starts the work that resolves an update, right after `landing::update` merged the
/// owner's branch into the agent's worktree: a team agent gets it through
/// `team::admit_resolve`, and a conversation master gets the prompt with its next
/// tools-on turn. Inputs: the store and the change set. Output: Ok once started or left
/// for the master's next turn, or a plain refusal.
pub(crate) fn start_resolve(db: &Connection, id: &ChangeSetId) -> Result<(), EngineError> {
    let project: Option<String> = db
        .query_row(
            "SELECT project FROM landing_change_sets WHERE id=?1",
            [id.as_str()],
            |row| row.get(0),
        )
        .optional()
        .map_err(storage)?;
    let project = project
        .and_then(|project| ProjectId::new(project).ok())
        .ok_or_else(|| refused("changes_not_found"))?;
    let set = landing::list(db, &project)?
        .into_iter()
        .find(|set| &set.id == id)
        .ok_or_else(|| refused("changes_not_found"))?;
    if set.state != ChangeState::Updating {
        return Err(refused("changes_stale"));
    }
    if set.agent.kind == AgentKind::Master {
        return Ok(());
    }
    let agent: i64 = set
        .agent
        .id
        .parse()
        .map_err(|_| refused("team_agent_not_found"))?;
    let prompt = fitted(&landing::resolve_task_prompt(db, id)?);
    let tx = Transaction::new_unchecked(db, TransactionBehavior::Immediate).map_err(storage)?;
    crate::team::admit_resolve(&tx, &project, agent, &prompt, crate::team::now_ms())?
        .map_err(refused)?;
    tx.commit().map_err(storage)
}

/// The resolve prompt for an agent whose change set an update left waiting for it, so a
/// conversation master's next tools-on turn carries it. Inputs: the store, project, agent
/// kind and id. Output: the prompt within `RESOLVE_PROMPT_BYTES_MAX`, or None when no set
/// of that agent is updating or the store has no change sets yet.
pub(crate) fn resolve_prompt_for(
    db: &Connection,
    project: &ProjectId,
    kind: AgentKind,
    agent: &str,
) -> Result<Option<String>, EngineError> {
    if cfg!(not(unix)) {
        return Ok(None);
    }
    let stored: Option<i64> = db
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type='table' AND name='landing_change_sets'",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(storage)?;
    if stored.is_none() {
        return Ok(None);
    }
    let Some(set) = landing::list(db, project)?.into_iter().find(|set| {
        set.agent.kind == kind && set.agent.id == agent && set.state == ChangeState::Updating
    }) else {
        return Ok(None);
    };
    Ok(Some(fitted(&landing::resolve_task_prompt(db, &set.id)?)))
}

/// Fits a resolve prompt within `RESOLVE_PROMPT_BYTES_MAX` with no control characters
/// but newlines and tabs: listed files are dropped from the end, with one line saying to
/// search the worktree for the rest. Input: the prompt. Output: the fitted prompt.
fn fitted(prompt: &str) -> String {
    const MORE: &str = "- and more; search the worktree for <<<<<<<";
    let clean: String = prompt
        .chars()
        .map(|c| match c {
            '\n' | '\t' => c,
            c if c.is_control() => '\u{fffd}',
            c => c,
        })
        .collect();
    let mut lines: Vec<&str> = clean.lines().collect();
    let fits = |lines: &[&str], more: bool| {
        let added = if more && !lines.contains(&MORE) { MORE.len() + 1 } else { 0 };
        lines.iter().map(|line| line.len() + 1).sum::<usize>() + added <= RESOLVE_PROMPT_BYTES_MAX
    };
    let mut cut = false;
    while !fits(&lines, cut) {
        let Some(last) = lines
            .iter()
            .rposition(|line| line.starts_with("- ") && *line != MORE)
        else {
            break;
        };
        lines.remove(last);
        cut = true;
    }
    if cut && !lines.contains(&MORE) {
        let at = lines
            .iter()
            .rposition(|line| line.starts_with("- "))
            .map_or(lines.len().min(2), |last| last + 1);
        lines.insert(at, MORE);
    }
    let mut fitted = lines.join("\n");
    fitted.push('\n');
    assert!(fitted.len() <= RESOLVE_PROMPT_BYTES_MAX, "resolve_prompt_bounds");
    fitted
}

fn refused(code: &str) -> EngineError {
    EngineError(code.into())
}

fn storage(_: rusqlite::Error) -> EngineError {
    refused("changes_unavailable")
}
