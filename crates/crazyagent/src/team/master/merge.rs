use super::{Answering, state};
use crate::engine::EngineError;
use crate::landing::{AcceptOutcome, AgentKind, ChangeSetView, ChangeState};
use crate::protocol::ProjectId;
use crate::team::{MessageAuthor, MessageState, bound_text, now_ms};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use std::path::Path;

const SHOWN_MAX: usize = 64;
const FILES_SHOWN_MAX: usize = 400;
const COMMIT_SHOWN: usize = 12;

/// Merges a finished harness's task branch into the project's Land on branch, the Master's
/// branch unless the owner named another (DEC-111 Q1), for the live master's `merge` tool
/// (DEC-100): the harness's waiting change set lands through
/// landing's `accept_into` as one squash commit with the owner's git identity, locally and
/// never pushed, with Accept's own refusals for a dirty, busy or detached checkout and a
/// conflict. When the owner's "Master accepts change sets" setting is Only when I click or
/// say accept all (DEC-147.7), nothing lands unless the owner sent accept all to the team
/// since the set last changed: the change set stays in Changes and the owner gets a
/// notification instead. Each outcome is
/// also noted in the team conversation for the owner. Takes the engine database, what the
/// answers need and the harness's name. Returns the sentence the master reads and whether
/// it is a refusal, or a SQLite failure.
pub(super) fn merge(
    connection: &mut Connection,
    answering: &Answering<'_>,
    agent: &str,
) -> Result<(String, bool), EngineError> {
    let project = &answering.stored.project_id;
    let Found {
        db,
        label,
        task,
        set,
    } = match found(connection, answering, agent, &MERGE_WORDS)? {
        Ok(found) => found,
        Err(refused) => return Ok((refused, true)),
    };
    match set.state {
        ChangeState::Conflict => {
            return Ok((
                format!(
                    "Not merged: {label}'s work conflicts with the owner's branch in {}. Ask {label} to bring {task} up to date, then merge again.",
                    bound_text(&set.conflicts.join(", "), FILES_SHOWN_MAX)
                ),
                true,
            ));
        }
        ChangeState::Updating => {
            return Ok((
                format!(
                    "Not merged: {label}'s change set is being brought up to date. Merge again once it is ready."
                ),
                true,
            ));
        }
        _ => {}
    }
    let land_on = crate::team::land_on(connection, project)?;
    let (choice, setting) = match land_on {
        Some(branch) => (Some(branch), "Land on branch"),
        None => (
            crate::team::master_branch_choice(connection, project)?,
            "Master's branch",
        ),
    };
    let target = answering
        .stored
        .project_folder()
        .canonicalize()
        .map_err(|_| EngineError("changes_workspace_invalid".into()))
        .and_then(|repository| crate::landing::land_on(connection, &repository, project));
    let target = match target {
        Ok(Some((branch, _))) => branch,
        Ok(None) => {
            return Ok((
                "Not merged: the project has no Master's branch. Its folder has no default branch and its checkout is detached; ask the owner to set the Master's branch in the team settings.".to_owned(),
                true,
            ));
        }
        Err(problem) => {
            return Ok((
                format!(
                    "Not merged: the {setting} {} is not a local branch of the project ({}). Ask the owner to check the {setting} setting.",
                    bound_text(choice.as_deref().unwrap_or("default"), SHOWN_MAX),
                    bound_text(&problem.0, 120)
                ),
                true,
            ));
        }
    };
    let target = bound_text(&target, crate::landing::BRANCH_BYTES_MAX);
    let said = if crate::team::ask_before_merge(connection, project)? {
        let since = i64::try_from(set.updated_ms).unwrap_or(i64::MAX);
        Some(crate::team::owner_said_accept_all(
            connection, project, since,
        )?)
    } else {
        None
    };
    if said == Some(false) {
        let text = format!(
            "{label} finished {task}. Its change set waits in Changes for your Accept into {target}, or send accept all to the team."
        );
        note(connection, project, &text)?;
        let notified = crate::team::events::escalate(connection, project, &text, now_ms())?;
        let told = if notified.is_ok() {
            "The owner got a notification."
        } else {
            "The owner sees it in the team conversation."
        };
        return Ok((
            format!(
                "Waiting for the owner: the owner's setting lets the master merge into {target} only after the owner clicks Accept or sends accept all to the team. {label}'s change set waits in Changes. {told}"
            ),
            false,
        ));
    }
    let landed = crate::projects_home::owner_home().and_then(|home| {
        crate::landing::accept_into_as(
            &db,
            &home,
            &set.id,
            &set.head,
            None,
            &target,
            crate::activity::MASTER_ACTOR,
        )
    });
    Ok(match landed {
        Ok(AcceptOutcome::Landed { commit, branch }) => {
            let branch = branch.strip_prefix("refs/heads/").unwrap_or(&branch);
            let text = format!(
                "The master merged {label}'s work from {task} into {} as {}{}. Nothing was pushed.",
                bound_text(branch, crate::landing::BRANCH_BYTES_MAX),
                bound_text(&commit, COMMIT_SHOWN),
                if said == Some(true) {
                    " because you said accept all"
                } else {
                    ""
                }
            );
            note(connection, project, &text)?;
            (
                format!(
                    "Merged {label}'s work from {task} into {} as {}. Nothing was pushed; the owner was told.",
                    bound_text(branch, crate::landing::BRANCH_BYTES_MAX),
                    bound_text(&commit, COMMIT_SHOWN)
                ),
                false,
            )
        }
        Ok(AcceptOutcome::Conflict { files }) => (
            format!(
                "Not merged: {label}'s work conflicts with {target} in {}. Ask {label} to merge {target} into {task}, resolve it and report done again.",
                bound_text(&files.join(", "), FILES_SHOWN_MAX)
            ),
            true,
        ),
        Err(problem) => {
            let moved = match problem.0.as_str() {
                "changes_owner_dirty" => {
                    crate::instruction_files::uncommitted(&answering.stored.project_folder())
                        .unwrap_or_default()
                }
                _ => Vec::new(),
            };
            (
                format!("Not merged into {target}: {}", refusal(&problem.0, &moved)),
                true,
            )
        }
    })
}

/// The words a refusal of the master's merge or discard starts with, and the one asking to
/// try again, so both tools read the same.
struct Words {
    not: &'static str,
    again: &'static str,
    nothing: &'static str,
}

const MERGE_WORDS: Words = Words {
    not: "Not merged",
    again: "Merge",
    nothing: "Nothing to merge",
};

const DISCARD_WORDS: Words = Words {
    not: "Not discarded",
    again: "Discard",
    nothing: "Nothing to discard",
};

/// A finished harness's waiting change set, with the landing store it was read from, the
/// harness's label and its task branch in words.
struct Found {
    db: Connection,
    label: String,
    task: String,
    set: ChangeSetView,
}

/// Finds the change set a finished harness the master started has waiting, for the master's
/// merge and discard: the harness by name, live or finished, not still working, and its
/// pending, conflicting or updating set. Takes the engine database, what the answers need,
/// the harness's name and the tool's words. Returns the set, the sentence the master reads
/// when there is none, or a SQLite failure.
fn found(
    connection: &Connection,
    answering: &Answering<'_>,
    agent: &str,
    words: &Words,
) -> Result<Result<Found, String>, EngineError> {
    let project = &answering.stored.project_id;
    let shown = bound_text(agent, SHOWN_MAX);
    let not = words.not;
    let found = match crate::team::events::resolve(connection, project, agent)? {
        Some(id) => Some(id),
        None => crate::team::names::resolve_finished(connection, project, agent)?,
    };
    let Some(id) = found else {
        return Ok(Err(format!(
            "{not}: no harness of this project is called {shown}. Call inspect_work() for their names."
        )));
    };
    let row: Option<(String, Option<i64>, String)> = connection
        .query_row(
            "SELECT role,parent,status FROM team_agents WHERE id=?1 AND project=?2",
            params![id, project.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()
        .map_err(state)?;
    let label = crate::team::events::label(connection, id)?;
    match row
        .as_ref()
        .map(|(role, parent, status)| (role.as_str(), *parent, status.as_str()))
    {
        Some(("sub", None, "starting" | "working")) => {
            return Ok(Err(format!(
                "{not}: {label} is still working. {} it once it reports done.",
                words.again
            )));
        }
        Some(("sub", None, _)) => {}
        _ => {
            return Ok(Err(format!("{not}: {label} is not a harness you started.")));
        }
    }
    let worktree: Option<(String, String)> = connection
        .query_row(
            "SELECT worktree,git_dir FROM team_worktrees WHERE agent=?1 AND project=?2",
            params![id, project.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(state)?;
    let opened = crate::landing::open(answering.landing).and_then(|db| {
        let sets = crate::landing::list(&db, project)?;
        let branch = match &worktree {
            Some((worktree, git_dir)) => {
                crate::landing::agent_branch(&db, Path::new(worktree), Path::new(git_dir))?
            }
            None => None,
        };
        Ok((db, sets, branch))
    });
    let (db, sets, task) = match opened {
        Ok(opened) => opened,
        Err(problem) => {
            return Ok(Err(format!(
                "{not}: the change sets could not be read ({}).",
                bound_text(&problem.0, 120)
            )));
        }
    };
    let task = task.map_or_else(
        || "its branch".to_owned(),
        |task| format!("its branch {task}"),
    );
    let Some(set) = waiting(sets, id) else {
        return Ok(Err(format!(
            "{}: {label} has no change set waiting. Its work becomes one when its turn ends; call inspect_work() to see what waits.",
            words.nothing
        )));
    };
    Ok(Ok(Found {
        db,
        label,
        task,
        set,
    }))
}

/// Discards a finished harness's waiting change set for the master's `discard` tool
/// (DEC-147 S3.6), restorable for 7 days, logged as the master's and told to the owner with
/// the reason. Takes the engine database, what the answers need, the harness's name and the
/// reason. Returns the sentence the master reads and whether it is a refusal, or a SQLite
/// failure.
pub(super) fn discard(
    connection: &mut Connection,
    answering: &Answering<'_>,
    agent: &str,
    reason: &str,
) -> Result<(String, bool), EngineError> {
    let project = &answering.stored.project_id;
    let Found {
        db,
        label,
        task,
        set,
    } = match found(connection, answering, agent, &DISCARD_WORDS)? {
        Ok(found) => found,
        Err(refused) => return Ok((refused, true)),
    };
    let reason = bound_text(reason.trim(), crate::team::tool::DISCARD_REASON_BYTES_MAX);
    assert!(!reason.is_empty());
    let now = u64::try_from(now_ms()).unwrap_or(0);
    let discarded =
        crate::landing::discard_as(&db, &set.id, &set.head, now, crate::activity::MASTER_ACTOR);
    Ok(match discarded {
        Ok(_) => {
            let text = format!(
                "The master discarded {label}'s change set from {task}: {reason}. You can restore it from Changes for 7 days."
            );
            note(connection, project, &text)?;
            (
                format!(
                    "Discarded {label}'s change set from {task}. The owner was told and can restore it from Changes for 7 days."
                ),
                false,
            )
        }
        Err(problem) => (
            format!(
                "Not discarded: {}",
                match problem.0.as_str() {
                    "changes_stale" =>
                        "the change set moved meanwhile. Call inspect_work(), then discard again."
                            .to_owned(),
                    crate::landing::WORKER_OPEN => crate::landing::WORKER_OPEN_TEXT.to_owned(),
                    other => format!(
                        "the change set could not be discarded ({}).",
                        bound_text(other, 120)
                    ),
                }
            ),
            true,
        ),
    })
}

/// The change set of one harness that waits to land, if any. Takes the project's change
/// sets and the harness. Returns its pending, conflicting or updating set.
pub(super) fn waiting(sets: Vec<ChangeSetView>, agent: i64) -> Option<ChangeSetView> {
    let id = agent.to_string();
    sets.into_iter().find(|set| {
        set.agent.kind == AgentKind::Sub
            && set.agent.id == id
            && matches!(
                set.state,
                ChangeState::Pending | ChangeState::Conflict | ChangeState::Updating
            )
    })
}

/// The plain sentence for an Accept refusal the merge met, naming for a dirty checkout
/// the instruction files whose import move is not committed and the owner's commit for
/// it. Takes the code and those files. Returns the sentence.
fn refusal(code: &str, moved: &[String]) -> String {
    match code {
        "changes_owner_dirty" if !moved.is_empty() => format!(
            "the owner's checkout of that branch has uncommitted changes, among them the instruction import's move of {} into .crazyagent/imported/, which is not committed yet. Ask the owner to choose Commit the move for the instruction import, which commits only those files, and to commit or stash any other change, then merge again.",
            bound_text(&moved.join(", "), FILES_SHOWN_MAX)
        ),
        "changes_owner_dirty" => "the owner's checkout of that branch has uncommitted changes. Ask the owner to commit or stash them, then merge again.".to_owned(),
        "changes_git_busy" => "a git operation is in progress in the project folder. Merge again once it ends.".to_owned(),
        "changes_detached_head" => "the project folder's checkout is detached.".to_owned(),
        "changes_target_checked_out" => "that branch is checked out in another worktree the engine does not manage.".to_owned(),
        "changes_target_invalid" => "that branch is not a local branch of the project.".to_owned(),
        "changes_branch_moved" | "changes_stale" => "the branch moved meanwhile. Merge again.".to_owned(),
        "changes_agent_busy" => "the harness is still writing. Merge again once it reports done.".to_owned(),
        crate::landing::WORKER_OPEN => crate::landing::WORKER_OPEN_TEXT.to_owned(),
        other => format!("the change set could not land ({}).", bound_text(other, 120)),
    }
}

/// Notes one merge outcome in the project's team conversation, where the owner reads what
/// the engine did. Takes the engine database, the project and the text. Returns any SQLite
/// failure.
fn note(connection: &mut Connection, project: &ProjectId, text: &str) -> Result<(), EngineError> {
    let tx = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(state)?;
    let text = bound_text(text, crate::team::NOTE_BYTES_MAX);
    let _ = crate::team::append_message(
        &tx,
        project,
        MessageAuthor::Engine,
        &text,
        MessageState::Note,
        now_ms(),
    )?;
    tx.commit().map_err(state)
}

#[cfg(test)]
mod tests {
    use super::refusal;

    #[test]
    fn a_dirty_checkout_refusal_names_the_uncommitted_import_move_and_its_commit() {
        let moved = ["CLAUDE.md".to_owned(), ".claude/CLAUDE.md".to_owned()];
        let named = refusal("changes_owner_dirty", &moved);
        assert!(named.contains("CLAUDE.md, .claude/CLAUDE.md"), "{named}");
        assert!(named.contains("Commit the move"), "{named}");
        assert!(
            named.contains("commit or stash any other change"),
            "{named}"
        );
        assert_eq!(
            refusal("changes_owner_dirty", &[]),
            "the owner's checkout of that branch has uncommitted changes. Ask the owner to commit or stash them, then merge again."
        );
        assert!(!refusal("changes_git_busy", &moved).contains("CLAUDE.md"));
    }
}
