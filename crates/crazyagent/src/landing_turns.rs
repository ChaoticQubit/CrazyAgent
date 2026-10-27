use crate::engine::EngineError;
use crate::landing::{self, AgentKind, ChangeSetId, ChangeState, TurnEnd, Workspace};
use crate::protocol::{ProjectId, SessionId};
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
/// `team::admit_resolve`, and the set stops showing as waiting for its agent. A
/// conversation master gets the prompt from the owner runtime. Inputs: the store and the
/// change set. Output: Ok once started or left to the owner runtime, or a plain refusal.
pub(crate) fn start_resolve(db: &Connection, id: &ChangeSetId) -> Result<(), EngineError> {
    let project = set_project(db, id)?;
    let set = view(db, id)?.ok_or_else(|| refused("changes_not_found"))?;
    if set.state != ChangeState::Updating {
        return Err(refused("changes_stale"));
    }
    if set.agent.kind == AgentKind::Master {
        return Ok(());
    }
    let agent = team_agent(&set)?;
    let prompt = fitted(&landing::resolve_task_prompt(db, id)?);
    let tx = Transaction::new_unchecked(db, TransactionBehavior::Immediate).map_err(storage)?;
    crate::team::admit_resolve(&tx, &project, agent, &prompt, crate::team::now_ms())?
        .map_err(refused)?;
    tx.commit().map_err(storage)?;
    landing::resolve_handed_over(db, id)
}

/// Ask agent to update: checks that the agent can take the resolve work, then merges the
/// owner's branch into its worktree and starts that work. A refused check changes
/// nothing. Once merged, a resolve that does not start leaves the set merged and waiting
/// for its agent rather than failing the update; asking again for a waiting set only
/// starts the resolve. A master's resolve turn is started by the owner runtime after
/// this returns. Inputs: the store and the change set. Output: true when this call
/// merged, or the refusal.
pub(crate) fn update(db: &Connection, id: &ChangeSetId) -> Result<bool, EngineError> {
    let set = view(db, id)?.ok_or_else(|| refused("changes_not_found"))?;
    let again = set.state == ChangeState::Updating && set.waiting;
    if set.agent.kind == AgentKind::Master {
        let session =
            SessionId::new(set.agent.id.clone()).map_err(|_| refused("changes_master_closed"))?;
        crate::storage::master_resolve_ready(db, &session)?.map_err(refused)?;
        if !again {
            landing::update(db, id)?;
        }
        return Ok(!again);
    }
    if again {
        start_resolve(db, id)?;
        return Ok(false);
    }
    resolve_ready(db, &set_project(db, id)?, &set)?;
    landing::update(db, id)?;
    if let Err(problem) = start_resolve(db, id) {
        eprintln!("resolve_not_started_after_update cause={}", problem.0);
    }
    Ok(true)
}

/// Checks, before any merge, that a team agent can take its resolve work: the admission
/// `start_resolve` makes, run with a stand-in prompt in a transaction that is rolled
/// back. A stopped agent passes, since its pinned resolve task starts it again. Inputs:
/// the store, the project and the change set. Output: Ok, or the plain refusal.
fn resolve_ready(
    db: &Connection,
    project: &ProjectId,
    set: &landing::ChangeSetView,
) -> Result<(), EngineError> {
    let agent = team_agent(set)?;
    let tx = Transaction::new_unchecked(db, TransactionBehavior::Immediate).map_err(storage)?;
    let admitted = crate::team::admit_resolve(
        &tx,
        project,
        agent,
        "Resolve the update.\n",
        crate::team::now_ms(),
    )?;
    tx.rollback().map_err(storage)?;
    admitted.map_err(refused)
}

fn team_agent(set: &landing::ChangeSetView) -> Result<i64, EngineError> {
    set.agent
        .id
        .parse()
        .map_err(|_| refused("team_agent_not_found"))
}

/// The conversation master an updated change set belongs to, so the owner runtime can
/// start its resolve turn. Inputs: the store and the change set. Output: the project and
/// session, or None for a team agent's set or one no longer updating.
pub(crate) fn master_of(
    db: &Connection,
    id: &ChangeSetId,
) -> Result<Option<(ProjectId, SessionId)>, EngineError> {
    let Some(set) = view(db, id)? else {
        return Ok(None);
    };
    if set.agent.kind != AgentKind::Master || set.state != ChangeState::Updating {
        return Ok(None);
    }
    let session =
        SessionId::new(set.agent.id.clone()).map_err(|_| refused("changes_master_closed"))?;
    Ok(Some((set_project(db, id)?, session)))
}

/// What the engine asks a conversation master in its resolve turn: a plain line for the
/// owner naming the conflicting files and the engine's resolve prompt fitted to its bound. Inputs:
/// the store, project, session and change set. Output: the line and text, or None unless
/// the set is that master's own and updating.
pub(crate) fn master_resolve(
    db: &Connection,
    project: &ProjectId,
    session: &SessionId,
    id: &ChangeSetId,
) -> Result<Option<(String, String)>, EngineError> {
    let Some(set) = landing::list(db, project)?
        .into_iter()
        .find(|set| &set.id == id)
    else {
        return Ok(None);
    };
    if set.agent.kind != AgentKind::Master
        || set.agent.id != session.as_str()
        || set.state != ChangeState::Updating
    {
        return Ok(None);
    }
    let text = fitted(&landing::resolve_task_prompt(db, id)?);
    Ok(Some((resolve_line(&set.conflicts), text)))
}

const RESOLVE_LINE_FILES_MAX: usize = 5;

/// The line the owner sees when the engine asks the master to resolve an update: the
/// first `RESOLVE_LINE_FILES_MAX` conflicting files and how many more, or a check of the
/// merge when nothing conflicted. Input: the conflicting files. Output: the line, within
/// `team::NOTE_BYTES_MAX`.
fn resolve_line(conflicts: &[String]) -> String {
    if conflicts.is_empty() {
        return "CrazyAgent merged your branch into the master's work and asked the master to check it still fits.".into();
    }
    let shown: Vec<String> = conflicts
        .iter()
        .take(RESOLVE_LINE_FILES_MAX)
        .map(|file| crate::team::bound_text(&file.replace(char::is_control, "\u{fffd}"), 120))
        .collect();
    let mut line = format!(
        "CrazyAgent asked the master to resolve the conflicts in {}",
        shown.join(", ")
    );
    let more = conflicts.len() - shown.len();
    if more > 0 {
        line.push_str(&format!(" and {more} more"));
    }
    line.push('.');
    assert!(
        line.len() <= crate::team::NOTE_BYTES_MAX,
        "resolve_line_bounds"
    );
    line
}

fn set_project(db: &Connection, id: &ChangeSetId) -> Result<ProjectId, EngineError> {
    let project: Option<String> = db
        .query_row(
            "SELECT project FROM landing_change_sets WHERE id=?1",
            [id.as_str()],
            |row| row.get(0),
        )
        .optional()
        .map_err(storage)?;
    project
        .and_then(|project| ProjectId::new(project).ok())
        .ok_or_else(|| refused("changes_not_found"))
}

fn view(db: &Connection, id: &ChangeSetId) -> Result<Option<landing::ChangeSetView>, EngineError> {
    let project = set_project(db, id)?;
    Ok(landing::list(db, &project)?
        .into_iter()
        .find(|set| &set.id == id))
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
        let added = if more && !lines.contains(&MORE) {
            MORE.len() + 1
        } else {
            0
        };
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
    assert!(
        fitted.len() <= RESOLVE_PROMPT_BYTES_MAX,
        "resolve_prompt_bounds"
    );
    fitted
}

fn refused(code: &str) -> EngineError {
    EngineError(code.into())
}

fn storage(_: rusqlite::Error) -> EngineError {
    refused("changes_unavailable")
}

#[cfg(all(test, unix))]
mod update_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_long_resolve_prompt_keeps_its_instructions_and_fits_the_task_bound() {
        let head = "The owner's branch changed while your work waited for review, so CrazyAgent merged it into your worktree.\nThese files have conflict markers (<<<<<<<, ======= and >>>>>>>):\n";
        let tail = "Resolve every conflict so both sides' intent is kept, remove every marker, and make sure the project still builds and its tests pass.\nDo not run git commit; CrazyAgent records your changes when you finish.\n";
        let mut prompt = String::from(head);
        for n in 0..50 {
            prompt.push_str(&format!("- {}\u{1b}/file-{n}.md\n", "deep/".repeat(30)));
        }
        prompt.push_str(tail);
        assert!(prompt.len() > RESOLVE_PROMPT_BYTES_MAX);
        let fitted = fitted(&prompt);
        assert!(fitted.len() <= RESOLVE_PROMPT_BYTES_MAX, "{}", fitted.len());
        assert!(fitted.starts_with(head), "{fitted}");
        assert!(fitted.ends_with(tail), "{fitted}");
        assert!(fitted.contains("/file-0.md\n"), "{fitted}");
        assert!(!fitted.contains("/file-49.md"), "{fitted}");
        assert_eq!(
            fitted
                .matches("\n- and more; search the worktree for <<<<<<<\nResolve every")
                .count(),
            1,
            "{fitted}"
        );
        assert!(!fitted.chars().any(|c| c.is_control() && c != '\n'));
        let short = format!("{head}- a.md\n{tail}");
        assert_eq!(super::fitted(&short), short);
    }
}
