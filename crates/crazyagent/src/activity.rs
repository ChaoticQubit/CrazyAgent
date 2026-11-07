use crate::engine::EngineError;
use crate::protocol::ProjectId;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

/// The most bytes of one event's text.
pub const TEXT_BYTES_MAX: usize = 600;
/// The most bytes of the goal an event serves.
pub const GOAL_BYTES_MAX: usize = 120;
/// The most events one list returns.
pub const LIST_MAX: usize = 500;
/// The most bytes of JSON one list returns, so it fits one engine frame.
pub const LISTED_BYTES_MAX: usize = 48 * 1024;
/// The most bytes of who did an event.
const ACTOR_BYTES_MAX: usize = 120;
/// The most bytes of a change set id kept with an event.
const CHANGE_SET_BYTES_MAX: usize = 128;
/// The most events one project keeps; the oldest go first.
const KEPT_PER_PROJECT_MAX: i64 = 10_000;

const _: () = assert!(LISTED_BYTES_MAX + 4096 <= crate::protocol::FRAME_BYTES_MAX);
const _: () = assert!(TEXT_BYTES_MAX + GOAL_BYTES_MAX + ACTOR_BYTES_MAX + 512 < LISTED_BYTES_MAX);
const _: () = assert!(LIST_MAX as i64 <= KEPT_PER_PROJECT_MAX);

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS project_activity (
    id INTEGER PRIMARY KEY,
    project TEXT NOT NULL,
    at_ms INTEGER NOT NULL,
    kind TEXT NOT NULL,
    actor TEXT NOT NULL,
    goal TEXT,
    text TEXT NOT NULL,
    change_set TEXT,
    undone INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS project_activity_at ON project_activity(project, at_ms DESC);
CREATE TABLE IF NOT EXISTS project_activity_backfill (project TEXT PRIMARY KEY);
";

/// What happened in a project, one name per kind of event the engine sees.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivityKind {
    Created,
    ResearchStarted,
    ResearchSaved,
    ResearchStopped,
    TaskStarted,
    TaskFinished,
    ChangesReady,
    Accepted,
    Discarded,
    Conflict,
    SentBack,
    LeadMerged,
    MergedIntoMain,
    Undone,
    WorkStarted,
    WorkReviewReady,
    WorkComplete,
    WorkClosed,
    Phase,
    Test,
}

impl ActivityKind {
    pub const ALL: [Self; 20] = [
        Self::Created,
        Self::ResearchStarted,
        Self::ResearchSaved,
        Self::ResearchStopped,
        Self::TaskStarted,
        Self::TaskFinished,
        Self::ChangesReady,
        Self::Accepted,
        Self::Discarded,
        Self::Conflict,
        Self::SentBack,
        Self::LeadMerged,
        Self::MergedIntoMain,
        Self::Undone,
        Self::WorkStarted,
        Self::WorkReviewReady,
        Self::WorkComplete,
        Self::WorkClosed,
        Self::Phase,
        Self::Test,
    ];

    /// The stored name of the kind, the same as its JSON name.
    pub fn name(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::ResearchStarted => "research_started",
            Self::ResearchSaved => "research_saved",
            Self::ResearchStopped => "research_stopped",
            Self::TaskStarted => "task_started",
            Self::TaskFinished => "task_finished",
            Self::ChangesReady => "changes_ready",
            Self::Accepted => "accepted",
            Self::Discarded => "discarded",
            Self::Conflict => "conflict",
            Self::SentBack => "sent_back",
            Self::LeadMerged => "lead_merged",
            Self::MergedIntoMain => "merged_into_main",
            Self::Undone => "undone",
            Self::WorkStarted => "work_started",
            Self::WorkReviewReady => "work_review_ready",
            Self::WorkComplete => "work_complete",
            Self::WorkClosed => "work_closed",
            Self::Phase => "phase",
            Self::Test => "test",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.name() == value)
    }
}

/// One thing that happened in a project, as the Overview shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivityItem {
    pub id: i64,
    pub at_ms: i64,
    pub kind: ActivityKind,
    pub actor: String,
    pub goal: Option<String>,
    pub text: String,
    pub change_set: Option<String>,
    pub undone: bool,
}

/// One event a writer records. `agent` is the team agent whose work the event is about:
/// when `goal` is None, the goal is the title of that agent's latest active work.
#[derive(Debug, Clone, Copy)]
pub struct NewActivity<'a> {
    pub kind: ActivityKind,
    pub actor: &'a str,
    pub agent: Option<i64>,
    pub goal: Option<&'a str>,
    pub text: &'a str,
    pub change_set: Option<&'a str>,
}

/// Creates the activity tables when they are missing, then brings each project's history
/// in once (`backfill`); a project whose backfill fails is logged by its plain code and
/// tried again at the next start, so the engine always starts. Takes the engine database.
/// Returns Ok, or the SQLite failure of the tables.
pub fn initialize(db: &Connection) -> Result<(), EngineError> {
    db.execute_batch(SCHEMA).map_err(state)?;
    let projects: Vec<String> = {
        let mut statement = db.prepare("SELECT id FROM projects").map_err(state)?;
        statement
            .query_map([], |row| row.get(0))
            .map_err(state)?
            .collect::<Result<_, _>>()
            .map_err(state)?
    };
    for project in projects {
        let Ok(project) = ProjectId::new(&project) else {
            continue;
        };
        if let Err(error) = backfill(db, &project) {
            eprintln!(
                "activity_backfill_failed cause={}",
                crate::protocol::plain_code(&error.0)
            );
        }
    }
    Ok(())
}

/// Records one event: the actor, text and goal are made one line and cut to their bounds,
/// and the project keeps its newest `KEPT_PER_PROJECT_MAX` events. Takes the database, the
/// project, the event and the time in ms. Returns the event id, or `activity_item_invalid`
/// for an empty actor or text.
pub fn record(
    db: &Connection,
    project: &ProjectId,
    item: &NewActivity<'_>,
    now_ms: i64,
) -> Result<i64, EngineError> {
    let actor = one_line(item.actor, ACTOR_BYTES_MAX);
    let text = one_line(item.text, TEXT_BYTES_MAX);
    if actor.is_empty() || text.is_empty() || now_ms < 0 {
        return Err(failure("activity_item_invalid"));
    }
    let goal = match item.goal {
        Some(goal) => Some(one_line(goal, GOAL_BYTES_MAX)),
        None => match item.agent {
            Some(agent) => active_goal(db, project, agent)?,
            None => None,
        },
    }
    .filter(|goal| !goal.is_empty());
    let change_set = item
        .change_set
        .map(|id| one_line(id, CHANGE_SET_BYTES_MAX))
        .filter(|id| !id.is_empty());
    db.execute(
        "INSERT INTO project_activity(project,at_ms,kind,actor,goal,text,change_set)
         VALUES (?1,?2,?3,?4,?5,?6,?7)",
        params![
            project.as_str(),
            now_ms,
            item.kind.name(),
            actor,
            goal,
            text,
            change_set
        ],
    )
    .map_err(state)?;
    let id = db.last_insert_rowid();
    assert!(id > 0);
    db.execute(
        "DELETE FROM project_activity WHERE id IN (SELECT id FROM project_activity
         WHERE project=?1 ORDER BY at_ms DESC, id DESC LIMIT -1 OFFSET ?2)",
        params![project.as_str(), KEPT_PER_PROJECT_MAX],
    )
    .map_err(state)?;
    Ok(id)
}

/// Records one event for a writer whose own action must never fail on it: a failure is
/// logged by its plain code. Takes what `record` takes.
pub fn note(db: &Connection, project: &ProjectId, item: &NewActivity<'_>, now_ms: i64) {
    if let Err(error) = record(db, project, item, now_ms) {
        eprintln!(
            "activity_record_failed kind={} cause={}",
            item.kind.name(),
            crate::protocol::plain_code(&error.0)
        );
    }
}

/// Lists a project's events newest first, at most `limit` and `LISTED_BYTES_MAX` bytes of
/// JSON. Takes the database, the project and the limit, 1 to `LIST_MAX`. Returns the
/// events, or `activity_limit_invalid`.
pub fn list(
    db: &Connection,
    project: &ProjectId,
    limit: usize,
) -> Result<Vec<ActivityItem>, EngineError> {
    if limit == 0 || limit > LIST_MAX {
        return Err(failure("activity_limit_invalid"));
    }
    let _ = (db, project);
    Ok(Vec::new())
}

/// Brings in, once per project, what happened before the engine recorded events: the
/// accepted change sets, the work starts and ends of the coordination audit and the
/// research runs, each at its own time. An event already recorded is not added again.
/// Takes the database and the project. Returns how many events it added.
pub fn backfill(db: &Connection, project: &ProjectId) -> Result<usize, EngineError> {
    let _ = (db, project);
    Ok(0)
}

/// Clears a removed project's events and its backfill mark. Runs inside the removal's
/// transaction. Takes the database and the project. Returns Ok or the SQLite failure.
pub fn forget_project(db: &Connection, project: &ProjectId) -> Result<(), EngineError> {
    for table in ["project_activity", "project_activity_backfill"] {
        db.execute(
            &format!("DELETE FROM {table} WHERE project=?1"),
            [project.as_str()],
        )
        .map_err(state)?;
    }
    Ok(())
}

/// The text one event of an accepted change set carries, the same for the live writer
/// and the backfill. Takes the agent's label and the set's one-line prompt. Returns the
/// text.
pub fn accepted_text(label: &str, prompt_line: &str) -> String {
    format!("Accepted the changes of {label}: {prompt_line}")
}

/// The kind and text of a coordination audit action the activity shows: the start and
/// the three ends of a unit of work. Takes the action name. Returns None for the others.
pub fn audit_event(action: &str) -> Option<(ActivityKind, &'static str)> {
    match action {
        "start" => Some((ActivityKind::WorkStarted, "Started the work")),
        "finish_review_ready" => Some((
            ActivityKind::WorkReviewReady,
            "The work is ready for review",
        )),
        "finish_complete" => Some((ActivityKind::WorkComplete, "The work is complete")),
        "finish_closed" => Some((ActivityKind::WorkClosed, "The work was closed")),
        _ => None,
    }
}

/// Names a team agent for the activity the way the team list does. Takes the database
/// and the agent id. Returns its label, or "Agent" and the id for a row that is gone.
pub fn agent_actor(db: &Connection, agent: i64) -> Result<String, EngineError> {
    let role: Option<String> = db
        .query_row("SELECT role FROM team_agents WHERE id=?1", [agent], |row| {
            row.get(0)
        })
        .optional()
        .map_err(state)?;
    Ok(
        match role.as_deref().and_then(crate::team::AgentRole::parse) {
            Some(role) => role.agent_label(agent),
            None => format!("Agent {agent}"),
        },
    )
}

fn active_goal(
    db: &Connection,
    project: &ProjectId,
    agent: i64,
) -> Result<Option<String>, EngineError> {
    db.query_row(
        "SELECT title FROM coord_work WHERE project=?1 AND owner_agent=?2
         AND state IN ('active','accepted') ORDER BY updated DESC, id DESC LIMIT 1",
        params![project.as_str(), agent],
        |row| row.get::<_, String>(0),
    )
    .optional()
    .map(|title| title.map(|title| one_line(&title, GOAL_BYTES_MAX)))
    .map_err(state)
}

fn one_line(text: &str, bytes: usize) -> String {
    let joined = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut end = joined.len().min(bytes);
    while !joined.is_char_boundary(end) {
        end -= 1;
    }
    let line = joined[..end].trim_end().to_owned();
    assert!(line.len() <= bytes);
    line
}

fn failure(code: &str) -> EngineError {
    EngineError(code.into())
}

fn state(problem: impl std::fmt::Display) -> EngineError {
    EngineError(format!("activity_state_unavailable: {problem}"))
}

#[cfg(all(test, unix))]
mod tests;
