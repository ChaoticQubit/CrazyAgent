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
/// Who a research run event names as its actor.
pub const RESEARCH_ACTOR: &str = "Research";
/// Who an event names when the owner acted.
pub const OWNER_ACTOR: &str = "Owner";
/// Who an event names when the engine acted on its own or for an agent.
pub const ENGINE_ACTOR: &str = "Engine";
/// Who an event names when a project's master acted, as the team list names it.
pub const MASTER_ACTOR: &str = "Master";

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
    logged(item.kind, record(db, project, item, now_ms).map(drop));
}

/// Records an event of one team agent, named the way the team list names it, with the
/// goal of its latest active work; a failure is logged by its plain code. Takes the
/// database, the project, the agent, the kind of event, its text and the time in ms.
pub fn note_agent(
    db: &Connection,
    project: &ProjectId,
    agent: i64,
    kind: ActivityKind,
    text: &str,
    now_ms: i64,
) {
    let recorded = agent_actor(db, agent).and_then(|actor| {
        let item = NewActivity {
            kind,
            actor: &actor,
            agent: Some(agent),
            goal: None,
            text,
            change_set: None,
        };
        record(db, project, &item, now_ms).map(drop)
    });
    logged(kind, recorded);
}

/// Records that a team agent started a task, with the first line of the task's prompt;
/// a failure is logged by its plain code. Takes the database, the project, the agent, the
/// task id and the time in ms.
pub fn note_task(db: &Connection, project: &ProjectId, agent: i64, task: i64, now_ms: i64) {
    let prompt: Result<String, EngineError> = db
        .query_row("SELECT prompt FROM team_tasks WHERE id=?1", [task], |row| {
            row.get(0)
        })
        .map_err(state);
    match prompt {
        Ok(prompt) => {
            let first = prompt.lines().find(|line| !line.trim().is_empty());
            let text = format!("Started a task: {}", first.unwrap_or("(no prompt)"));
            note_agent(db, project, agent, ActivityKind::TaskStarted, &text, now_ms);
        }
        Err(error) => logged(ActivityKind::TaskStarted, Err(error)),
    }
}

/// Records the start or one of the three ends of a unit of work, as the coordination
/// audit stores it, with the work's title as the goal; other actions are not shown and a
/// failure is logged by its plain code. Takes the database, the project, the work, the
/// party kind and agent who acted, the audit action and the time in ms.
pub fn note_audit(
    db: &Connection,
    project: &ProjectId,
    work: i64,
    by: (&str, Option<i64>),
    action: &str,
    now_ms: i64,
) {
    let Some((kind, text)) = audit_event(action) else {
        return;
    };
    logged(
        kind,
        record_audit(db, project, work, by, (kind, text), now_ms),
    );
}

fn record_audit(
    db: &Connection,
    project: &ProjectId,
    work: i64,
    by: (&str, Option<i64>),
    (kind, text): (ActivityKind, &str),
    now_ms: i64,
) -> Result<(), EngineError> {
    let title: Option<String> = db
        .query_row("SELECT title FROM coord_work WHERE id=?1", [work], |row| {
            row.get(0)
        })
        .optional()
        .map_err(state)?;
    let actor = party_actor(db, by.0, by.1)?;
    let item = NewActivity {
        kind,
        actor: &actor,
        agent: None,
        goal: title.as_deref(),
        text,
        change_set: None,
    };
    record(db, project, &item, now_ms).map(drop)
}

/// Records a research run event of one job, read from the job's row; a failure is logged
/// by its plain code, so the run itself never fails on it. Takes the database, the job
/// id, the research kind of event and the time in ms.
pub fn note_research(db: &Connection, job: i64, kind: ActivityKind, now_ms: i64) {
    logged(kind, record_research(db, job, kind, now_ms));
}

fn record_research(
    db: &Connection,
    job: i64,
    kind: ActivityKind,
    now_ms: i64,
) -> Result<(), EngineError> {
    let (project, name): (String, String) = db
        .query_row(
            "SELECT project,kind FROM research_jobs WHERE id=?1",
            [job],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(state)?;
    let project = ProjectId::new(&project).map_err(|_| failure("activity_item_invalid"))?;
    let text = research_text(kind, &name);
    let item = NewActivity {
        kind,
        actor: RESEARCH_ACTOR,
        agent: None,
        goal: None,
        text: &text,
        change_set: None,
    };
    record(db, &project, &item, now_ms).map(drop)
}

/// Records that a lead merged a sub-agent's work into its own branch, named by the lead
/// and with the lead's goal; a failure is logged by its plain code. Takes the database,
/// the project, the lead, the sub-agent, the merged branch and the time in ms.
pub fn note_lead_merge(
    db: &Connection,
    project: &ProjectId,
    lead: i64,
    sub: i64,
    branch: &str,
    now_ms: i64,
) {
    match agent_actor(db, sub) {
        Ok(worker) => {
            let text = format!("Merged the work of {worker} from {branch}");
            note_agent(db, project, lead, ActivityKind::LeadMerged, &text, now_ms);
        }
        Err(error) => logged(ActivityKind::LeadMerged, Err(error)),
    }
}

fn logged(kind: ActivityKind, recorded: Result<(), EngineError>) {
    if let Err(error) = recorded {
        eprintln!(
            "activity_record_failed kind={} cause={}",
            kind.name(),
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
    let mut statement = db
        .prepare(
            "SELECT id,at_ms,kind,actor,goal,text,change_set,undone FROM project_activity
             WHERE project=?1 ORDER BY at_ms DESC, id DESC LIMIT ?2",
        )
        .map_err(state)?;
    let rows = statement
        .query_map(
            params![project.as_str(), i64::try_from(limit).map_err(state)?],
            |row| {
                Ok((
                    row.get::<_, String>(2)?,
                    ActivityItem {
                        id: row.get(0)?,
                        at_ms: row.get(1)?,
                        kind: ActivityKind::Created,
                        actor: row.get(3)?,
                        goal: row.get(4)?,
                        text: row.get(5)?,
                        change_set: row.get(6)?,
                        undone: row.get(7)?,
                    },
                ))
            },
        )
        .map_err(state)?;
    let mut items = Vec::new();
    let mut bytes = 0;
    for row in rows {
        let (kind, mut item) = row.map_err(state)?;
        let Some(kind) = ActivityKind::parse(&kind) else {
            continue;
        };
        item.kind = kind;
        let size = serde_json::to_vec(&item).map_err(state)?.len() + 1;
        if bytes + size > LISTED_BYTES_MAX {
            break;
        }
        bytes += size;
        items.push(item);
    }
    assert!(items.len() <= limit && bytes <= LISTED_BYTES_MAX);
    Ok(items)
}

/// One event found in the project's older records by `backfill`.
struct Past {
    kind: ActivityKind,
    at_ms: i64,
    actor: String,
    goal: Option<String>,
    text: String,
    change_set: Option<String>,
}

/// Brings in, once per project, what happened before the engine recorded events: the
/// accepted change sets, the work starts and ends of the coordination audit and the
/// research runs, each at its own time. An event already recorded is not added again.
/// Takes the database and the project. Returns how many events it added.
pub fn backfill(db: &Connection, project: &ProjectId) -> Result<usize, EngineError> {
    let tx = db.unchecked_transaction().map_err(state)?;
    let marked = tx
        .execute(
            "INSERT OR IGNORE INTO project_activity_backfill(project) VALUES (?1)",
            [project.as_str()],
        )
        .map_err(state)?;
    if marked == 0 {
        return Ok(0);
    }
    let mut found = Vec::new();
    if has_table(&tx, "landing_change_sets")? {
        found.extend(past_accepts(&tx, project)?);
    }
    if has_table(&tx, "coord_audit")? && has_table(&tx, "coord_work")? {
        found.extend(past_work(&tx, project)?);
    }
    if has_table(&tx, "research_jobs")? {
        found.extend(past_research(&tx, project)?);
    }
    let mut added = 0;
    for past in &found {
        if known(&tx, project, past)? {
            continue;
        }
        let item = NewActivity {
            kind: past.kind,
            actor: &past.actor,
            agent: None,
            goal: past.goal.as_deref(),
            text: &past.text,
            change_set: past.change_set.as_deref(),
        };
        record(&tx, project, &item, past.at_ms)?;
        added += 1;
    }
    tx.commit().map_err(state)?;
    assert!(added <= found.len());
    Ok(added)
}

fn has_table(db: &Connection, name: &str) -> Result<bool, EngineError> {
    db.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
        [name],
        |row| row.get(0),
    )
    .map_err(state)
}

fn past_accepts(db: &Connection, project: &ProjectId) -> Result<Vec<Past>, EngineError> {
    let mut statement = db
        .prepare(
            "SELECT id,label,prompt_line,updated_ms FROM landing_change_sets
             WHERE project=?1 AND state='accepted'",
        )
        .map_err(state)?;
    let rows = statement
        .query_map([project.as_str()], |row| {
            Ok(Past {
                kind: ActivityKind::Accepted,
                at_ms: row.get(3)?,
                actor: OWNER_ACTOR.into(),
                goal: None,
                text: accepted_text(&row.get::<_, String>(1)?, &row.get::<_, String>(2)?),
                change_set: Some(row.get(0)?),
            })
        })
        .map_err(state)?;
    rows.collect::<Result<_, _>>().map_err(state)
}

fn past_work(db: &Connection, project: &ProjectId) -> Result<Vec<Past>, EngineError> {
    let mut statement = db
        .prepare(
            "SELECT a.action,a.by_kind,a.by_agent,a.at,w.title FROM coord_audit a
             JOIN coord_work w ON w.id=a.work WHERE a.project=?1",
        )
        .map_err(state)?;
    let rows: Vec<(String, String, Option<i64>, i64, String)> = statement
        .query_map([project.as_str()], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        })
        .map_err(state)?
        .collect::<Result<_, _>>()
        .map_err(state)?;
    let mut found = Vec::new();
    for (action, by_kind, by_agent, at_ms, title) in rows {
        let Some((kind, text)) = audit_event(&action) else {
            continue;
        };
        found.push(Past {
            kind,
            at_ms,
            actor: party_actor(db, &by_kind, by_agent)?,
            goal: Some(title),
            text: text.into(),
            change_set: None,
        });
    }
    Ok(found)
}

fn past_research(db: &Connection, project: &ProjectId) -> Result<Vec<Past>, EngineError> {
    let mut statement = db
        .prepare(
            "SELECT kind,status,started_ms,finished_ms FROM research_jobs
             WHERE project=?1 AND started_ms IS NOT NULL",
        )
        .map_err(state)?;
    let rows: Vec<(String, String, i64, Option<i64>)> = statement
        .query_map([project.as_str()], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })
        .map_err(state)?
        .collect::<Result<_, _>>()
        .map_err(state)?;
    let mut found = Vec::new();
    for (job, status, started, finished) in rows {
        let ended = finished.map(|at_ms| {
            let kind = if status == "done" {
                ActivityKind::ResearchSaved
            } else {
                ActivityKind::ResearchStopped
            };
            (kind, at_ms)
        });
        for (kind, at_ms) in std::iter::once((ActivityKind::ResearchStarted, started)).chain(ended)
        {
            found.push(Past {
                kind,
                at_ms,
                actor: RESEARCH_ACTOR.into(),
                goal: None,
                text: research_text(kind, &job),
                change_set: None,
            });
        }
    }
    Ok(found)
}

fn known(db: &Connection, project: &ProjectId, past: &Past) -> Result<bool, EngineError> {
    let change_set = past
        .change_set
        .as_deref()
        .map(|id| one_line(id, CHANGE_SET_BYTES_MAX));
    db.query_row(
        "SELECT EXISTS(SELECT 1 FROM project_activity WHERE project=?1 AND kind=?2 AND
         ((?3 IS NOT NULL AND change_set=?3)
          OR (?3 IS NULL AND at_ms=?4 AND actor=?5 AND text=?6)))",
        params![
            project.as_str(),
            past.kind.name(),
            change_set,
            past.at_ms,
            one_line(&past.actor, ACTOR_BYTES_MAX),
            one_line(&past.text, TEXT_BYTES_MAX)
        ],
        |row| row.get(0),
    )
    .map_err(state)
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

/// The text one research run event carries, the same for the live writer and the
/// backfill: the job named by its label, or as stored when the kind is unknown. Takes the
/// research kind of event and the job's stored kind. Returns the text.
pub fn research_text(kind: ActivityKind, job: &str) -> String {
    let verb = match kind {
        ActivityKind::ResearchStarted => "started",
        ActivityKind::ResearchSaved => "saved",
        ActivityKind::ResearchStopped => "stopped",
        _ => unreachable!("research_text takes a research event"),
    };
    let shown = match crate::project_setup::ResearchKind::parse(job) {
        Some(known) => known.label(),
        None => job,
    };
    format!("Research {verb}: {shown}")
}

/// Names who acted from a stored party, as the coordination audit keeps it. Takes the
/// database, the party kind and its agent. Returns "Owner", the agent's label or "Engine".
pub fn party_actor(db: &Connection, kind: &str, agent: Option<i64>) -> Result<String, EngineError> {
    Ok(match (kind, agent) {
        ("agent", Some(agent)) => agent_actor(db, agent)?,
        ("owner", _) => OWNER_ACTOR.into(),
        _ => ENGINE_ACTOR.into(),
    })
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
