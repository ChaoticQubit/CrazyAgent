//! The engine state behind each Action kind (DEC-136a decision 1): open questions, accounts
//! that need a sign-in, research waiting for a pool, memories to review and change sets to
//! review. Each source reads only its own tables and reports at most `SOURCE_ROWS_MAX`
//! conditions; one whose tables are missing reports none and resolves nothing, and one a cap
//! stopped before the end of its state resolves nothing either.

use super::reconcile::{Condition, SOURCE_ROWS_MAX};
use super::{NoticeKind, NoticeTarget, error};
use crate::config::{ModelAccess, ModelAuthentication};
use crate::engine::EngineError;
use crate::landing::ChangeSetId;
use crate::protocol::ProjectId;
use rusqlite::{Connection, params};
use std::collections::BTreeSet;

/// The most projects with open team inbox state whose inbox one tick reads.
const TEAM_PROJECTS_MAX: i64 = 64;

const _: () = assert!(TEAM_PROJECTS_MAX * 64 <= 8 * SOURCE_ROWS_MAX as i64);

type Source = fn(&Connection, &ModelAccess) -> Result<Option<Found>, EngineError>;

/// What one source read: its conditions, whether a cap stopped it before the end of its
/// state, and the projects whose own part a cap cut short, so items it did not reach may
/// still hold and stay open.
pub(super) struct Found {
    conditions: Vec<Condition>,
    capped: bool,
    capped_projects: Vec<ProjectId>,
}

impl Found {
    /// One source's read. Inputs: the conditions, whether a row or project cap was reached
    /// and the projects an inbox cap cut short. Output: the read, capped as well when the
    /// list itself is full.
    fn read(conditions: Vec<Condition>, capped: bool, capped_projects: Vec<ProjectId>) -> Self {
        assert!(conditions.len() <= SOURCE_ROWS_MAX);
        assert!(capped_projects.len() <= SOURCE_ROWS_MAX);
        let full = conditions.len() == SOURCE_ROWS_MAX;
        Self {
            conditions,
            capped: capped || full,
            capped_projects,
        }
    }
}

/// The conditions that hold now: the kinds whose sources were read, the parts a cap stopped
/// before the end (a whole kind, or one project of it), their conditions, and the first
/// failure of a source that could not be read.
#[derive(Default)]
pub(super) struct Live {
    pub kinds: Vec<NoticeKind>,
    pub partial: Vec<(NoticeKind, Option<ProjectId>)>,
    pub conditions: Vec<Condition>,
    pub problem: Option<EngineError>,
}

/// Reads every Action source. Inputs: the engine database and the model access. Output:
/// what holds now.
pub(super) fn live(connection: &Connection, access: &ModelAccess) -> Live {
    let sources: [(NoticeKind, Source); 5] = [
        (NoticeKind::Question, questions),
        (NoticeKind::AccountSignIn, accounts),
        (NoticeKind::ResearchPool, pools),
        (NoticeKind::MemoryReview, memories),
        (NoticeKind::ChangeSetReview, change_sets),
    ];
    let mut live = Live::default();
    for (kind, read) in sources {
        match read(connection, access) {
            Ok(Some(found)) => {
                assert!(found.conditions.len() <= SOURCE_ROWS_MAX);
                assert!(
                    found
                        .conditions
                        .iter()
                        .all(|condition| condition.kind == kind)
                );
                live.kinds.push(kind);
                if found.capped {
                    live.partial.push((kind, None));
                }
                for project_id in found.capped_projects {
                    live.partial.push((kind, Some(project_id)));
                }
                live.conditions.extend(found.conditions);
            }
            Ok(None) => {}
            Err(problem) => {
                live.problem.get_or_insert(problem);
            }
        }
    }
    assert!(
        live.partial
            .iter()
            .all(|(kind, _)| live.kinds.contains(kind))
    );
    live
}

fn tables_exist(connection: &Connection, names: &[&str]) -> Result<bool, EngineError> {
    assert!(!names.is_empty());
    let mut statement = connection
        .prepare("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)")
        .map_err(error)?;
    for name in names {
        let found: bool = statement
            .query_row([name], |row| row.get(0))
            .map_err(error)?;
        if !found {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Adds one condition unless its key is already listed or the list is full. Inputs: the
/// list, the keys seen and the condition. Output: none.
fn push(found: &mut Vec<Condition>, keys: &mut BTreeSet<String>, condition: Condition) {
    assert_eq!(condition.target.project(), condition.project_id.as_ref());
    if found.len() < SOURCE_ROWS_MAX && keys.insert(condition.subject.clone()) {
        found.push(condition);
    }
}

/// Open questions to the owner (decision 11): every pending row of the questions log,
/// master and team alike, keyed by its team message when it has one so linking it never
/// opens a second item, then each team project's Questions inbox: team questions not yet
/// linked and agents waiting on the owner in their own terminal. Inputs: the database and
/// the model access (unused). Output: the read, capped when the log or the team projects
/// with open inbox state held more than it reads, with each project whose inbox held more
/// than it shows, or none without the questions log.
fn questions(connection: &Connection, _: &ModelAccess) -> Result<Option<Found>, EngineError> {
    if !tables_exist(connection, &["questions_log"])? {
        return Ok(None);
    }
    let mut found = Vec::new();
    let mut keys = BTreeSet::new();
    let rows: Vec<(i64, String, String, String, Option<i64>)> = connection
        .prepare(
            "SELECT id, project, asker, question, coord_message FROM questions_log
             WHERE status='pending' ORDER BY id LIMIT ?1",
        )
        .map_err(error)?
        .query_map([SOURCE_ROWS_MAX as i64], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        })
        .map_err(error)?
        .collect::<Result<_, _>>()
        .map_err(error)?;
    let mut capped = rows.len() >= SOURCE_ROWS_MAX;
    for (id, project, asker, question, coord) in rows {
        let Ok(project_id) = ProjectId::new(project) else {
            continue;
        };
        let subject = coord.map_or_else(
            || format!("question:q{id}"),
            |coord| format!("question:c{coord}"),
        );
        let target = NoticeTarget::Questions {
            project_id: project_id.clone(),
            question: Some(id),
            agent: None,
        };
        let text = format!("{asker} asks: {question}");
        push(
            &mut found,
            &mut keys,
            Condition {
                kind: NoticeKind::Question,
                project_id: Some(project_id),
                subject,
                text,
                target,
            },
        );
    }
    let team = [
        "team_agents",
        "coord_messages",
        "coord_activity",
        "team_owner_alerts",
    ];
    let mut capped_projects = Vec::new();
    if tables_exist(connection, &team)? {
        let (projects, more) = team_projects(connection)?;
        capped |= more;
        for project_id in projects {
            if inbox(connection, &project_id, &mut found, &mut keys)? {
                capped_projects.push(project_id);
            }
        }
    }
    assert!(found.len() <= SOURCE_ROWS_MAX);
    Ok(Some(Found::read(found, capped, capped_projects)))
}

/// The team projects whose inbox one tick reads: those with an open question to the owner
/// or an agent waiting on the owner. Input: the database. Output: at most
/// `TEAM_PROJECTS_MAX` projects, and whether more were left out.
fn team_projects(connection: &Connection) -> Result<(Vec<ProjectId>, bool), EngineError> {
    let mut projects = crate::team::questions::inbox_projects(connection, TEAM_PROJECTS_MAX + 1)?;
    let cap = usize::try_from(TEAM_PROJECTS_MAX).expect("the project cap is small");
    let more = projects.len() > cap;
    projects.truncate(cap);
    assert!(projects.len() <= cap);
    let read = projects
        .into_iter()
        .filter_map(|id| ProjectId::new(id).ok())
        .collect();
    Ok((read, more))
}

/// Adds one project's team Questions inbox: open team questions not yet in the questions
/// log, and agents waiting on the owner in their own terminal. Inputs: the database, the
/// project, the list and its keys. Output: whether the inbox held more than it shows, or
/// the inbox failure.
fn inbox(
    connection: &Connection,
    project_id: &ProjectId,
    found: &mut Vec<Condition>,
    keys: &mut BTreeSet<String>,
) -> Result<bool, EngineError> {
    let inbox = crate::team::questions::inbox(connection, project_id)?;
    let capped = inbox.more > 0 || inbox.waiting.len() >= crate::team::questions::WAITING_SHOWN_MAX;
    let target = |agent: i64| NoticeTarget::Questions {
        project_id: project_id.clone(),
        question: None,
        agent: Some(agent),
    };
    for question in inbox.questions {
        let condition = Condition {
            kind: NoticeKind::Question,
            project_id: Some(project_id.clone()),
            subject: format!("question:c{}", question.id),
            text: format!("{} asks: {}", question.label, question.text),
            target: target(question.agent),
        };
        push(found, keys, condition);
    }
    for waiting in inbox.waiting {
        let what = match waiting.waiting {
            crate::coord::Waiting::Permission => "a permission prompt",
            crate::coord::Waiting::Question => "a question",
        };
        let condition = Condition {
            kind: NoticeKind::Question,
            project_id: Some(project_id.clone()),
            subject: format!("waiting:{}:{}", waiting.agent, waiting.since_ms),
            text: format!("{} waits on {what} in its terminal.", waiting.label),
            target: target(waiting.agent),
        };
        push(found, keys, condition);
    }
    Ok(capped)
}

/// Accounts that need their owner to sign in (decision 1): each model access connection
/// the engine reads as needing a sign-in, one item each. Inputs: the database and the model
/// access. Output: the read, or none without the account tables.
fn accounts(connection: &Connection, access: &ModelAccess) -> Result<Option<Found>, EngineError> {
    if !tables_exist(connection, &["accounts", "native_auth_operations"])? {
        return Ok(None);
    }
    let mut found = Vec::new();
    let mut keys = BTreeSet::new();
    for model in access.connections.iter().take(SOURCE_ROWS_MAX) {
        if matches!(model.auth, ModelAuthentication::Oauth { .. }) {
            continue;
        }
        if !crate::storage::accounts::needs_sign_in(connection, model)? {
            continue;
        }
        let name = crate::storage::accounts::account_name(connection, model)?;
        let condition = Condition {
            kind: NoticeKind::AccountSignIn,
            project_id: None,
            subject: format!("account:{}", model.id.as_str()),
            text: format!("{name} isn't signed in."),
            target: NoticeTarget::AccountSignIn {
                account: model.id.clone(),
            },
        };
        push(&mut found, &mut keys, condition);
    }
    let capped = access.connections.len() > SOURCE_ROWS_MAX;
    Ok(Some(Found::read(found, capped, Vec::new())))
}

/// Projects with work waiting for a pool (decision 1): research jobs and team tasks in
/// needs_pool, one item per project that counts them. Inputs: the database and the model
/// access (unused). Output: the conditions, or none without the job tables.
fn pools(connection: &Connection, _: &ModelAccess) -> Result<Option<Found>, EngineError> {
    if !tables_exist(connection, &["research_jobs", "team_tasks"])? {
        return Ok(None);
    }
    let rows: Vec<(String, i64, i64)> = connection
        .prepare(
            "SELECT project, SUM(jobs), SUM(tasks) FROM (
                 SELECT project, COUNT(*) AS jobs, 0 AS tasks FROM research_jobs
                 WHERE status='needs_pool' GROUP BY project
                 UNION ALL
                 SELECT project, 0, COUNT(*) FROM team_tasks WHERE status='needs_pool' GROUP BY project
             ) GROUP BY project ORDER BY project LIMIT ?1",
        )
        .map_err(error)?
        .query_map([SOURCE_ROWS_MAX as i64], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .map_err(error)?
        .collect::<Result<_, _>>()
        .map_err(error)?;
    let capped = rows.len() >= SOURCE_ROWS_MAX;
    let mut found = Vec::new();
    let mut keys = BTreeSet::new();
    for (project, jobs, tasks) in rows {
        let Ok(project_id) = ProjectId::new(project) else {
            continue;
        };
        assert!(jobs >= 0 && tasks >= 0 && jobs + tasks > 0);
        let condition = Condition {
            kind: NoticeKind::ResearchPool,
            subject: format!("pool:{}", project_id.as_str()),
            text: waiting_words(jobs, tasks),
            target: NoticeTarget::ProjectPool {
                project_id: project_id.clone(),
            },
            project_id: Some(project_id),
        };
        push(&mut found, &mut keys, condition);
    }
    Ok(Some(Found::read(found, capped, Vec::new())))
}

/// The words of a pool item. Inputs: the research jobs and team tasks waiting. Output:
/// "2 research jobs wait for a pool.", "1 team task waits for a pool." or both joined.
fn waiting_words(jobs: i64, tasks: i64) -> String {
    let jobs_words = match jobs {
        0 => None,
        1 => Some("1 research job".to_owned()),
        many => Some(format!("{many} research jobs")),
    };
    let tasks_words = match tasks {
        0 => None,
        1 => Some("1 team task".to_owned()),
        many => Some(format!("{many} team tasks")),
    };
    let verb = if jobs + tasks == 1 { "waits" } else { "wait" };
    match (jobs_words, tasks_words) {
        (Some(jobs), Some(tasks)) => format!("{jobs} and {tasks} {verb} for a pool."),
        (Some(one), None) | (None, Some(one)) => format!("{one} {verb} for a pool."),
        (None, None) => unreachable!("a pool item counts at least one job or task"),
    }
}

/// Projects with research memories nobody reviewed yet (decision 1), one item per project
/// that counts them. Inputs: the database and the model access (unused). Output: the
/// conditions, or none without the memory tables.
fn memories(connection: &Connection, _: &ModelAccess) -> Result<Option<Found>, EngineError> {
    if !tables_exist(connection, &["knowledge_heads", "knowledge_entries"])? {
        return Ok(None);
    }
    let rows: Vec<(String, i64)> = connection
        .prepare(
            "SELECT h.project, COUNT(*) FROM knowledge_heads h
             JOIN knowledge_entries e ON e.id=h.id AND e.version=h.version
             WHERE json_extract(e.body,'$.status')='Provisional'
               AND json_extract(e.body,'$.human_reviewed')=0
             GROUP BY h.project ORDER BY h.project LIMIT ?1",
        )
        .map_err(error)?
        .query_map([SOURCE_ROWS_MAX as i64], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .map_err(error)?
        .collect::<Result<_, _>>()
        .map_err(error)?;
    let capped = rows.len() >= SOURCE_ROWS_MAX;
    let mut found = Vec::new();
    let mut keys = BTreeSet::new();
    for (project, count) in rows {
        let Ok(project_id) = ProjectId::new(project) else {
            continue;
        };
        assert!(count > 0);
        let text = if count == 1 {
            "1 suggested memory to review.".to_owned()
        } else {
            format!("{count} suggested memories to review.")
        };
        let condition = Condition {
            kind: NoticeKind::MemoryReview,
            subject: format!("memories:{}", project_id.as_str()),
            text,
            target: NoticeTarget::Memories {
                project_id: project_id.clone(),
            },
            project_id: Some(project_id),
        };
        push(&mut found, &mut keys, condition);
    }
    Ok(Some(Found::read(found, capped, Vec::new())))
}

/// Change sets waiting for the owner (decision 1): pending, in conflict or updating, one
/// item each, oldest first. Inputs: the database and the model access (unused). Output: the
/// conditions, or none without the change set tables.
fn change_sets(connection: &Connection, _: &ModelAccess) -> Result<Option<Found>, EngineError> {
    if !tables_exist(
        connection,
        &["landing_change_sets", "landing_change_set_numbers"],
    )? {
        return Ok(None);
    }
    let rows: Vec<(String, String, String, String, Option<i64>)> = connection
        .prepare(
            "SELECT c.id, c.project, c.label, c.prompt_line, n.number FROM landing_change_sets c
             LEFT JOIN landing_change_set_numbers n ON n.change_set=c.id
             WHERE c.state IN ('pending','conflict','updating')
             ORDER BY c.created_ms, c.id LIMIT ?1",
        )
        .map_err(error)?
        .query_map(params![SOURCE_ROWS_MAX as i64], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        })
        .map_err(error)?
        .collect::<Result<_, _>>()
        .map_err(error)?;
    let capped = rows.len() >= SOURCE_ROWS_MAX;
    let mut found = Vec::new();
    let mut keys = BTreeSet::new();
    for (id, project, label, prompt, number) in rows {
        let (Ok(project_id), Ok(change_set)) = (ProjectId::new(project), ChangeSetId::new(id))
        else {
            continue;
        };
        let named = number.map_or_else(
            || "A change set".to_owned(),
            |number| format!("Change set #{number}"),
        );
        let condition = Condition {
            kind: NoticeKind::ChangeSetReview,
            subject: format!("change_set:{}", change_set.as_str()),
            text: format!("{named} from {label} is ready to review: {prompt}"),
            target: NoticeTarget::ChangeSet {
                project_id: project_id.clone(),
                change_set: Some(change_set),
            },
            project_id: Some(project_id),
        };
        push(&mut found, &mut keys, condition);
    }
    Ok(Some(Found::read(found, capped, Vec::new())))
}
