use super::{Host, engine_error, key_of};
use crate::engine::EngineError;
use crate::protocol::ProjectId;
use rusqlite::{OptionalExtension, Transaction, params};
use std::path::Path;
use trackwright_core::criteria::NOTIFY_TICKETS_MAX;
use trackwright_core::host::HostEvent;
use trackwright_core::model::{Actor, ProjectKey, ShortId, codes};
use trackwright_core::store::StoreError;

/// The engine identity the links an engine write adds in its own transaction run as, so a
/// ticket's history tells them from the links an agent or the owner made.
pub const HOOK_ACTOR: &str = "engine:hooks";

/// The most tickets one task's engine writes link; a task normally carries the one ticket
/// its `spawn` named.
pub const TICKETS_PER_TASK_MAX: usize = 16;

const TICKETS_OF_TASK: &str = "SELECT t.number FROM tk_links l
    JOIN tk_tickets t ON t.id = l.ticket JOIN tk_projects p ON p.id = t.project
    WHERE p.key = ?1 AND l.kind = 'task' AND l.target = ?2 ORDER BY t.id LIMIT ?3";
const RUNNING_TASK: &str = "SELECT id FROM team_tasks
    WHERE project = ?1 AND agent = ?2 AND status = 'running' ORDER BY id DESC LIMIT 1";
const WORKTREE_OF: &str =
    "SELECT worktree, git_dir FROM team_worktrees WHERE agent = ?1 AND project = ?2";
const TASK_TURN_PREFIX: &str = "task-";

fn unavailable(_: impl std::fmt::Display) -> EngineError {
    EngineError(super::TRACKER_UNAVAILABLE.into())
}

/// The tracker key of a project the hooks link for. `tracker::initialize` is unix-only,
/// so elsewhere the engine database holds no tracker tables and every hook links nothing
/// without querying them. Inputs: the transaction and the project. Output: the key, None
/// off unix or for a project with no tickets, or `tracker_unavailable`.
fn attached(tx: &Transaction<'_>, project: &ProjectId) -> Result<Option<ProjectKey>, EngineError> {
    if cfg!(unix) {
        key_of(tx, project)
    } else {
        Ok(None)
    }
}

/// Reads the task a turn ran from the name the team runtime gives a task's turns,
/// `task-<id>`, the id in decimal with no sign or leading zero. Input: the turn. Output:
/// the task, None for any other turn.
pub(crate) fn turn_task(turn: &str) -> Option<i64> {
    let digits = turn.strip_prefix(TASK_TURN_PREFIX)?;
    let task = digits.parse::<i64>().ok().filter(|task| *task >= 1)?;
    (task.to_string() == digits).then_some(task)
}

/// The project's tickets linked to one task, in creation order, at most
/// `TICKETS_PER_TASK_MAX`. Inputs: the transaction, the project's key and the task.
/// Output: the tickets, or `tracker_unavailable`.
fn tickets_of(
    tx: &Transaction<'_>,
    key: &ProjectKey,
    task: i64,
) -> Result<Vec<ShortId>, EngineError> {
    assert!(task >= 1, "a task id is above 0");
    let bound = i64::try_from(TICKETS_PER_TASK_MAX + 1).expect("the bound fits an i64");
    let mut statement = tx.prepare(TICKETS_OF_TASK).map_err(unavailable)?;
    let numbers = statement
        .query_map(params![key.as_str(), task.to_string(), bound], |row| {
            row.get::<_, i64>(0)
        })
        .map_err(unavailable)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(unavailable)?;
    if numbers.len() > TICKETS_PER_TASK_MAX {
        eprintln!("tracker_hook_tickets_skipped task={task}");
    }
    let tickets = numbers
        .into_iter()
        .take(TICKETS_PER_TASK_MAX)
        .map(|number| {
            let number = u32::try_from(number).map_err(unavailable)?;
            ShortId::new(key.clone(), number).map_err(|refusal| EngineError(refusal.code.into()))
        })
        .collect::<Result<Vec<_>, _>>()?;
    assert!(tickets.len() <= TICKETS_PER_TASK_MAX);
    Ok(tickets)
}

/// The task an agent of the project runs now, the newest when the store holds more than
/// one. Inputs: the transaction, the project and the agent. Output: the task, or None.
fn running_task(
    tx: &Transaction<'_>,
    project: &ProjectId,
    agent: i64,
) -> Result<Option<i64>, EngineError> {
    assert!(agent >= 1, "a team agent id is above 0");
    let task: Option<i64> = tx
        .query_row(RUNNING_TASK, params![project.as_str(), agent], |row| {
            row.get(0)
        })
        .optional()
        .map_err(unavailable)?;
    assert!(task.is_none_or(|task| task >= 1));
    Ok(task)
}

/// Links every ticket of a task to each target as `HOOK_ACTOR`. A link that exists is
/// kept as it is, and a link the tracker refuses is logged by its code and skipped, so
/// the engine write it rides on still lands; a SQLite failure fails that write. Inputs:
/// the transaction, the project's key, the task, the kinds and targets and the time.
/// Output: how many links were added, or `tracker_unavailable`.
fn link_tickets(
    tx: &Transaction<'_>,
    key: &ProjectKey,
    task: i64,
    targets: &[(&'static str, String)],
    now_ms: i64,
) -> Result<usize, EngineError> {
    assert!(!targets.is_empty());
    let tickets = tickets_of(tx, key, task)?;
    let actor = Actor::new(HOOK_ACTOR).expect("the hook actor is a valid actor");
    let host = Host;
    let mut added = 0;
    for ticket in &tickets {
        for (kind, target) in targets {
            match trackwright_core::host::link(tx, &host, &actor, ticket, kind, target, now_ms) {
                Ok(_) => added += 1,
                Err(StoreError::Refused(refusal)) if refusal.code == codes::LINK_EXISTS => {}
                Err(StoreError::Refused(refusal)) => {
                    eprintln!("tracker_link_skipped kind={kind} cause={}", refusal.code);
                }
                Err(other) => return Err(engine_error(other)),
            }
        }
    }
    assert!(added <= tickets.len() * targets.len());
    Ok(added)
}

/// Links the tickets a task carries to the agent that takes it, the agent's worktree and
/// the branch that worktree has checked out, in the transaction that marks the task
/// running. A take replayed after a restart adds nothing. Inputs: the transaction, the
/// project, the task, the agent and the time. Output: Ok, or `tracker_unavailable`.
pub(crate) fn task_taken(
    tx: &Transaction<'_>,
    project: &ProjectId,
    task: i64,
    agent: i64,
    now_ms: i64,
) -> Result<(), EngineError> {
    assert!(task >= 1, "a task id is above 0");
    assert!(agent >= 1, "a team agent id is above 0");
    let Some(key) = attached(tx, project)? else {
        return Ok(());
    };
    let place: Option<(String, String)> = tx
        .query_row(WORKTREE_OF, params![agent, project.as_str()], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .optional()
        .map_err(unavailable)?;
    let mut targets = vec![("agent", agent.to_string())];
    if let Some((worktree, git_dir)) = place {
        let branch = super::host::checked_out(Path::new(&git_dir));
        targets.push(("worktree", worktree));
        targets.extend(branch.map(|branch| ("branch", branch)));
    }
    assert!((1..=3).contains(&targets.len()));
    link_tickets(tx, &key, task, &targets, now_ms).map(|_| ())
}

/// Links the tickets of the task a sub-agent's turn ran to the change set that turn's
/// work went into, in the transaction that stores the change set. Inputs: the
/// transaction, the project, the change set, the turn and the time. Output: Ok, or
/// `tracker_unavailable`.
pub(crate) fn change_set_saved(
    tx: &Transaction<'_>,
    project: &ProjectId,
    change_set: &str,
    turn: &str,
    now_ms: i64,
) -> Result<(), EngineError> {
    assert!(!change_set.is_empty());
    let Some(task) = turn_task(turn) else {
        return Ok(());
    };
    let Some(key) = attached(tx, project)? else {
        return Ok(());
    };
    let target = [("change_set", change_set.to_owned())];
    link_tickets(tx, &key, task, &target, now_ms).map(|_| ())
}

/// Tells the tracker a change set was accepted or discarded, in the transaction that
/// stores that state, so a ticket whose last check that change set proves closes on
/// evidence in the same transaction, or not at all. A refusal is logged by its code and
/// leaves the tickets as they are. Inputs: the transaction, the project, the change set and
/// the time. Output: Ok, or `tracker_unavailable`.
pub(crate) fn change_set_closed(
    tx: &Transaction<'_>,
    project: &ProjectId,
    change_set: &str,
    now_ms: i64,
) -> Result<(), EngineError> {
    assert!(!change_set.is_empty());
    if attached(tx, project)?.is_none() {
        return Ok(());
    }
    let event = HostEvent {
        kind: "change_set".to_owned(),
        target: change_set.to_owned(),
    };
    match trackwright_core::criteria::notify(tx, &Host, &event, now_ms) {
        Ok(touched) => {
            assert!(touched.len() <= NOTIFY_TICKETS_MAX);
            Ok(())
        }
        Err(StoreError::Refused(refusal)) => {
            eprintln!("tracker_notify_skipped cause={}", refusal.code);
            Ok(())
        }
        Err(other) => Err(engine_error(other)),
    }
}

/// Links the tickets of the task an agent runs now to one of its records, in the
/// transaction that stores it; an agent between tasks links nothing. Inputs: the
/// transaction, the project, the agent, the link kind, the record and the time. Output:
/// Ok, or `tracker_unavailable`.
fn agent_record(
    tx: &Transaction<'_>,
    project: &ProjectId,
    agent: i64,
    (kind, target): (&'static str, String),
    now_ms: i64,
) -> Result<(), EngineError> {
    assert!(!target.is_empty());
    let Some(key) = attached(tx, project)? else {
        return Ok(());
    };
    let Some(task) = running_task(tx, project, agent)? else {
        return Ok(());
    };
    link_tickets(tx, &key, task, &[(kind, target)], now_ms).map(|_| ())
}

/// Links the tickets of the task an agent runs now to a memory it saved, in the
/// transaction that saves it. Inputs: the transaction, the project, the agent, the memory
/// and the time. Output: Ok, or `tracker_unavailable`.
pub(crate) fn memory_saved(
    tx: &Transaction<'_>,
    project: &ProjectId,
    agent: i64,
    memory: &str,
    now_ms: i64,
) -> Result<(), EngineError> {
    agent_record(tx, project, agent, ("memory", memory.to_owned()), now_ms)
}

/// Links the tickets of the task an agent runs now to a question it asked the owner, in
/// the transaction that logs it. Inputs: the transaction, the project, the agent, the
/// question and the time. Output: Ok, or `tracker_unavailable`.
pub(crate) fn question_asked(
    tx: &Transaction<'_>,
    project: &ProjectId,
    agent: i64,
    question: u64,
    now_ms: i64,
) -> Result<(), EngineError> {
    assert!(question >= 1, "a question id is above 0");
    agent_record(
        tx,
        project,
        agent,
        ("question", question.to_string()),
        now_ms,
    )
}

#[cfg(all(test, unix))]
#[path = "hooks_tests.rs"]
mod tests;

#[cfg(all(test, not(unix)))]
mod off_unix {
    use super::*;

    /// An engine database without the unix-only tracker schema takes every hook as a
    /// no-op, so a task take, a change set write, a memory and a question still store.
    #[test]
    fn every_hook_is_a_no_op_where_the_engine_holds_no_tracker_schema() {
        let mut db = rusqlite::Connection::open_in_memory().unwrap();
        let tx = db.transaction().unwrap();
        let project = ProjectId::new("ticket-hooks").unwrap();
        let ran = [
            task_taken(&tx, &project, 1, 7, 1),
            change_set_saved(&tx, &project, "cs-1", "task-1", 1),
            change_set_closed(&tx, &project, "cs-1", 1),
            memory_saved(&tx, &project, 7, "memory-1", 1),
            question_asked(&tx, &project, 7, 1, 1),
        ];
        let errors: Vec<String> = ran
            .into_iter()
            .filter_map(|run| run.err())
            .map(|e| e.0)
            .collect();
        assert_eq!(errors, Vec::<String>::new());
        tx.commit().unwrap();
    }
}
