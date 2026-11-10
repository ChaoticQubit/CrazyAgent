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
/// The most events one project keeps besides its merges (`MERGES_KEPT_MAX`), split between
/// the engine's and the owner's events and the agents' (`AGENT_LOGGED_KEPT_MAX`); the oldest
/// of each share go first.
const KEPT_PER_PROJECT_MAX: i64 = 10_000;
/// The share of `KEPT_PER_PROJECT_MAX` for the phases and test runs agents log, which
/// only push out their own oldest, never another kind of event.
const AGENT_LOGGED_KEPT_MAX: i64 = 1_000;
/// The newest merges a project keeps beyond `KEPT_PER_PROJECT_MAX`. A lead's merge and a
/// merge into main are the only record the Merges count has, so other events never push
/// them out, and at this many only the oldest merges go.
const MERGES_KEPT_MAX: i64 = 50_000;
/// The most phases and test runs one caller may log in `LOGGED_WINDOW_MS`.
pub const LOGGED_PER_WINDOW_MAX: i64 = 30;
/// The window a caller's logged rows are counted over: one hour.
const LOGGED_WINDOW_MS: i64 = 3_600_000;
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
const _: () = assert!(AGENT_LOGGED_KEPT_MAX > 0 && AGENT_LOGGED_KEPT_MAX < KEPT_PER_PROJECT_MAX);
const _: () = assert!(LOGGED_PER_WINDOW_MAX > 0 && LOGGED_PER_WINDOW_MAX <= AGENT_LOGGED_KEPT_MAX);
const _: () = assert!(MERGES_KEPT_MAX > KEPT_PER_PROJECT_MAX);

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
CREATE TABLE IF NOT EXISTS project_activity_logged (
    project TEXT NOT NULL,
    agent INTEGER NOT NULL,
    master INTEGER NOT NULL,
    at_ms INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS project_activity_logged_by
    ON project_activity_logged(project, agent, master, at_ms);
";

/// What happened in a project, one name per kind of event the engine sees.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivityKind {
    Created,
    ResearchStarted,
    ResearchSaved,
    ResearchStopped,
    ResearchWaiting,
    TaskStarted,
    TaskFinished,
    TaskStopped,
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
    pub const ALL: [Self; 22] = [
        Self::Created,
        Self::ResearchStarted,
        Self::ResearchSaved,
        Self::ResearchStopped,
        Self::ResearchWaiting,
        Self::TaskStarted,
        Self::TaskFinished,
        Self::TaskStopped,
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

    /// Whether agents log the kind themselves with `log_activity`: a phase or a test run.
    pub fn agent_logged(self) -> bool {
        matches!(self, Self::Phase | Self::Test)
    }

    /// Whether the kind is a merge, which `record` keeps in a share of its own.
    pub fn merge(self) -> bool {
        matches!(self, Self::LeadMerged | Self::MergedIntoMain)
    }

    /// The stored name of the kind, the same as its JSON name.
    pub fn name(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::ResearchStarted => "research_started",
            Self::ResearchSaved => "research_saved",
            Self::ResearchStopped => "research_stopped",
            Self::ResearchWaiting => "research_waiting",
            Self::TaskStarted => "task_started",
            Self::TaskFinished => "task_finished",
            Self::TaskStopped => "task_stopped",
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
    /// The branch a master's merge row's change set landed on, the one its Undo reverts on,
    /// read from the stored set rather than the row's words; None for any other row.
    #[serde(default)]
    pub branch: Option<String>,
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
        if let Err(error) = backfill(db, &project).and_then(|_| backfill_created(db, &project)) {
            eprintln!(
                "activity_backfill_failed cause={}",
                crate::protocol::plain_code(&error.0)
            );
        }
    }
    Ok(())
}

/// Records one event: the actor, text and goal go through the change set subject's scrub
/// (`shown`) and are cut to their bounds. The project keeps `KEPT_PER_PROJECT_MAX` events
/// in two shares that never push each other out: its Created event with the newest other
/// events of the engine and the owner, `KEPT_PER_PROJECT_MAX - AGENT_LOGGED_KEPT_MAX` in
/// all, and the newest `AGENT_LOGGED_KEPT_MAX` phases and test runs agents logged. On top
/// of those it keeps its newest `MERGES_KEPT_MAX` merges, which no other event pushes out.
/// Takes the database, the project, the event and the time in ms. Returns the event id, or
/// `activity_item_invalid` for an empty actor or text.
pub fn record(
    db: &Connection,
    project: &ProjectId,
    item: &NewActivity<'_>,
    now_ms: i64,
) -> Result<i64, EngineError> {
    let actor = shown(item.actor, ACTOR_BYTES_MAX);
    let text = shown(item.text, TEXT_BYTES_MAX);
    if actor.is_empty() || text.is_empty() || now_ms < 0 {
        return Err(failure("activity_item_invalid"));
    }
    let goal = match item.goal {
        Some(goal) => Some(goal.to_owned()),
        None => match item.agent {
            Some(agent) => active_goal(db, project, agent)?,
            None => None,
        },
    }
    .map(|goal| shown(&goal, GOAL_BYTES_MAX))
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
    let (phase, test) = (ActivityKind::Phase.name(), ActivityKind::Test.name());
    if item.kind.agent_logged() {
        db.execute(
            "DELETE FROM project_activity WHERE id IN (SELECT id FROM project_activity
             WHERE project=?1 AND kind IN (?3,?4) ORDER BY at_ms DESC, id DESC LIMIT -1 OFFSET ?2)",
            params![project.as_str(), AGENT_LOGGED_KEPT_MAX, phase, test],
        )
        .map_err(state)?;
        return Ok(id);
    }
    let (lead_merged, merged) = (
        ActivityKind::LeadMerged.name(),
        ActivityKind::MergedIntoMain.name(),
    );
    if item.kind.merge() {
        db.execute(
            "DELETE FROM project_activity WHERE id IN (SELECT id FROM project_activity
             WHERE project=?1 AND kind IN (?3,?4) ORDER BY at_ms DESC, id DESC LIMIT -1 OFFSET ?2)",
            params![project.as_str(), MERGES_KEPT_MAX, lead_merged, merged],
        )
        .map_err(state)?;
        return Ok(id);
    }
    let created: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM project_activity WHERE project=?1 AND kind=?2",
            params![project.as_str(), ActivityKind::Created.name()],
            |row| row.get(0),
        )
        .map_err(state)?;
    db.execute(
        "DELETE FROM project_activity WHERE id IN (SELECT id FROM project_activity
         WHERE project=?1 AND kind NOT IN (?3,?4,?5,?6,?7) ORDER BY at_ms DESC, id DESC LIMIT -1 OFFSET ?2)",
        params![
            project.as_str(),
            (KEPT_PER_PROJECT_MAX - AGENT_LOGGED_KEPT_MAX - created).max(0),
            ActivityKind::Created.name(),
            phase,
            test,
            lead_merged,
            merged
        ],
    )
    .map_err(state)?;
    Ok(id)
}

/// Records one phase or test run a caller logged with `log_activity`, unless that caller
/// already logged `LOGGED_PER_WINDOW_MAX` of them in the last hour; the count and the row
/// are written in one transaction, and counts older than the hour are dropped. Takes the
/// database, the project, the caller's id and whether it is the master, the event and the
/// time in ms. Returns the event id, or None when the caller is over its rate.
pub fn record_logged(
    db: &Connection,
    project: &ProjectId,
    (agent, master): (i64, bool),
    item: &NewActivity<'_>,
    now_ms: i64,
) -> Result<Option<i64>, EngineError> {
    assert!(item.kind.agent_logged());
    let tx = db.unchecked_transaction().map_err(state)?;
    tx.execute(
        "DELETE FROM project_activity_logged WHERE at_ms <= ?1",
        [now_ms.saturating_sub(LOGGED_WINDOW_MS)],
    )
    .map_err(state)?;
    let recent: i64 = tx
        .query_row(
            "SELECT COUNT(*) FROM project_activity_logged WHERE project=?1 AND agent=?2 AND master=?3",
            params![project.as_str(), agent, master],
            |row| row.get(0),
        )
        .map_err(state)?;
    if recent >= LOGGED_PER_WINDOW_MAX {
        tx.commit().map_err(state)?;
        return Ok(None);
    }
    let id = record(&tx, project, item, now_ms)?;
    tx.execute(
        "INSERT INTO project_activity_logged(project,agent,master,at_ms) VALUES (?1,?2,?3,?4)",
        params![project.as_str(), agent, master, now_ms],
    )
    .map_err(state)?;
    tx.commit().map_err(state)?;
    assert!(recent < LOGGED_PER_WINDOW_MAX);
    Ok(Some(id))
}

/// Marks the rows of a change set's merge undone, so the project's activity shows the merge
/// was reverted; a failure is logged by its plain code. Inputs: the database, the project
/// and the change set id.
pub fn mark_undone(db: &Connection, project: &ProjectId, change_set: &str) {
    assert!(!change_set.is_empty());
    let marked = db
        .execute(
            "UPDATE project_activity SET undone=1 WHERE project=?1 AND change_set=?2 \
             AND kind IN ('accepted','merged_into_main')",
            params![project.as_str(), change_set],
        )
        .map(drop)
        .map_err(state);
    logged(ActivityKind::Undone, marked);
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

/// Records that a team agent started a task, named by its title (`task_title`); a failure
/// is logged by its plain code. Takes the database, the project, the agent, the task id and
/// the time in ms.
pub fn note_task(db: &Connection, project: &ProjectId, agent: i64, task: i64, now_ms: i64) {
    let kind = ActivityKind::TaskStarted;
    logged(
        kind,
        record_task(
            db,
            project,
            agent,
            Some(task),
            (kind, "started", ""),
            now_ms,
        ),
    );
}

/// Records that a team agent's running task stopped before it ended, named by its title:
/// stopped for a cancelled one, and stopped and back in the queue for one that waits to run
/// again, so no started row is left with no end; a failure is logged by its plain code.
/// Takes the database, the project, the agent, the task, whether it waits again and the
/// time in ms.
pub fn note_task_stop(
    db: &Connection,
    project: &ProjectId,
    agent: i64,
    task: i64,
    requeued: bool,
    now_ms: i64,
) {
    assert!(task > 0);
    let kind = ActivityKind::TaskStopped;
    let tail = if requeued { ", back in the queue" } else { "" };
    logged(
        kind,
        record_task(
            db,
            project,
            agent,
            Some(task),
            (kind, "stopped", tail),
            now_ms,
        ),
    );
}

/// Records that a team agent's task ended, named by its title and never by the agent's
/// reply or failure text: finished, or could not finish for a failed one; a failure is
/// logged by its plain code. Takes the database, the project, the agent, the task when the
/// event names one, whether it failed and the time in ms.
pub fn note_task_end(
    db: &Connection,
    project: &ProjectId,
    agent: i64,
    task: Option<i64>,
    failed: bool,
    now_ms: i64,
) {
    let kind = ActivityKind::TaskFinished;
    let verb = if failed {
        "could not finish"
    } else {
        "finished"
    };
    logged(
        kind,
        record_task(db, project, agent, task, (kind, verb, ""), now_ms),
    );
}

fn record_task(
    db: &Connection,
    project: &ProjectId,
    agent: i64,
    task: Option<i64>,
    (kind, verb, tail): (ActivityKind, &str, &str),
    now_ms: i64,
) -> Result<(), EngineError> {
    let actor = agent_actor(db, agent)?;
    let title = match task {
        Some(task) => task_title(db, project, agent, task)?,
        None => String::new(),
    };
    let text = if title.is_empty() {
        format!("{actor} {verb} a task{tail}")
    } else {
        format!("{actor} {verb} \u{201c}{title}\u{201d}{tail}")
    };
    let item = NewActivity {
        kind,
        actor: &actor,
        agent: Some(agent),
        goal: None,
        text: &text,
        change_set: None,
    };
    record(db, project, &item, now_ms).map(drop)
}

/// Names a team task the way its rows show it: the first line of its prompt through the
/// change set subject's scrub, paths under the agent's worktree and the project's folder
/// made relative, cut to `GOAL_BYTES_MAX` bytes like the metrics title. Takes the database,
/// the project, the agent and the task. Returns the title, empty for a task of another
/// project, gone or with no prompt.
fn task_title(
    db: &Connection,
    project: &ProjectId,
    agent: i64,
    task: i64,
) -> Result<String, EngineError> {
    let prompt: Option<String> = db
        .query_row(
            "SELECT prompt FROM team_tasks WHERE id=?1 AND project=?2",
            params![task, project.as_str()],
            |row| row.get(0),
        )
        .optional()
        .map_err(state)?;
    let Some(first) = prompt
        .as_deref()
        .and_then(|prompt| prompt.lines().find(|line| !line.trim().is_empty()))
    else {
        return Ok(String::new());
    };
    let worktree: Option<String> = if has_table(db, "team_worktrees")? {
        db.query_row(
            "SELECT worktree FROM team_worktrees WHERE agent=?1 AND project=?2",
            params![agent, project.as_str()],
            |row| row.get(0),
        )
        .optional()
        .map_err(state)?
    } else {
        None
    };
    let folder = setup_folder(db, project)?;
    let roots: Vec<&std::path::Path> = [worktree.as_deref(), folder.as_deref()]
        .into_iter()
        .flatten()
        .map(std::path::Path::new)
        .filter(|root| root.is_absolute())
        .collect();
    Ok(one_line(
        &crate::landing::scrubbed(first, &roots),
        GOAL_BYTES_MAX,
    ))
}

/// The folder of a project's saved setup. Takes the database and the project. Returns
/// it, none for a project with no saved setup, or the SQLite failure.
fn setup_folder(db: &Connection, project: &ProjectId) -> Result<Option<String>, EngineError> {
    if !has_table(db, "project_setups")? {
        return Ok(None);
    }
    let body: Option<String> = db
        .query_row(
            "SELECT body FROM project_setups WHERE project=?1",
            [project.as_str()],
            |row| row.get(0),
        )
        .optional()
        .map_err(state)?;
    Ok(body
        .and_then(|body| serde_json::from_str::<serde_json::Value>(&body).ok())
        .and_then(|setup| {
            setup
                .get("folder")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        }))
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
    logged(
        kind,
        record_research(db, job, (kind, research_verb(kind)), now_ms),
    );
}

/// Records that a research run was parked until its project has a pool, under its own kind
/// so it never reads as stopped; a failure is logged by its plain code. Takes the database,
/// the job id and the time in ms.
pub fn note_research_waiting(db: &Connection, job: i64, now_ms: i64) {
    let kind = ActivityKind::ResearchWaiting;
    logged(
        kind,
        record_research(db, job, (kind, research_verb(kind)), now_ms),
    );
}

fn record_research(
    db: &Connection,
    job: i64,
    (kind, verb): (ActivityKind, &str),
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
    let text = research_line(verb, &name);
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
    listed(db, project, limit).map(|read| read.items)
}

/// One read of a project's events: the newest that fit, and whether older ones were left
/// out by the limit or the byte bound.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Listed {
    pub items: Vec<ActivityItem>,
    pub more: bool,
}

/// Lists a project's events as `list` does and says whether older ones were left out.
/// Takes the database, the project and the limit, 1 to `LIST_MAX`. Returns the read, or
/// `activity_limit_invalid`.
pub fn listed(db: &Connection, project: &ProjectId, limit: usize) -> Result<Listed, EngineError> {
    if limit == 0 || limit > LIST_MAX {
        return Err(failure("activity_limit_invalid"));
    }
    let landed_on = if has_table(db, "landing_change_sets")? {
        "CASE WHEN kind='merged_into_main' THEN \
         (SELECT landed_on FROM landing_change_sets WHERE id=project_activity.change_set) END"
    } else {
        "NULL"
    };
    let mut statement = db
        .prepare(&format!(
            "SELECT id,at_ms,kind,actor,goal,text,change_set,undone,{landed_on} \
             FROM project_activity WHERE project=?1 ORDER BY at_ms DESC, id DESC LIMIT ?2"
        ))
        .map_err(state)?;
    let rows = statement
        .query_map(
            params![project.as_str(), i64::try_from(limit + 1).map_err(state)?],
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
                        branch: row.get::<_, Option<String>>(8)?.and_then(|reference| {
                            reference.strip_prefix("refs/heads/").map(str::to_owned)
                        }),
                    },
                ))
            },
        )
        .map_err(state)?;
    let mut items = Vec::new();
    let mut bytes = 0;
    let mut more = false;
    for row in rows {
        let (kind, mut item) = row.map_err(state)?;
        let Some(kind) = ActivityKind::parse(&kind) else {
            continue;
        };
        if items.len() == limit {
            more = true;
            break;
        }
        item.kind = kind;
        let size = serde_json::to_vec(&item).map_err(state)?.len() + 1;
        if bytes + size > LISTED_BYTES_MAX {
            more = true;
            break;
        }
        bytes += size;
        items.push(item);
    }
    assert!(items.len() <= limit && bytes <= LISTED_BYTES_MAX);
    Ok(Listed { items, more })
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

/// The line of a project's Created event. Takes the project's name. Returns the line.
pub fn created_text(name: &str) -> String {
    format!("You created {name}")
}

/// Records that the owner created a project, in the transaction that creates it; a
/// failure is logged by its plain code, so creating never fails on it. Takes the database,
/// the project, its name and the time in ms.
pub fn note_created(db: &Connection, project: &ProjectId, name: &str, now_ms: i64) {
    let text = created_text(name);
    let item = NewActivity {
        kind: ActivityKind::Created,
        actor: OWNER_ACTOR,
        agent: None,
        goal: None,
        text: &text,
        change_set: None,
    };
    note(db, project, &item, now_ms);
}

/// Brings in the Created event of a project that has none, at its saved setup's creation
/// time and with its name, so a project made before the engine recorded events still shows
/// when it began; a project that has one, or has no saved setup, is left alone. Takes the
/// database and the project. Returns whether it added one, or the SQLite failure.
pub fn backfill_created(db: &Connection, project: &ProjectId) -> Result<bool, EngineError> {
    let created: bool = db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM project_activity WHERE project=?1 AND kind=?2)",
            params![project.as_str(), ActivityKind::Created.name()],
            |row| row.get(0),
        )
        .map_err(state)?;
    if created || !has_table(db, "project_setups")? {
        return Ok(false);
    }
    let body: Option<String> = db
        .query_row(
            "SELECT body FROM project_setups WHERE project=?1",
            [project.as_str()],
            |row| row.get(0),
        )
        .optional()
        .map_err(state)?;
    let setup = body.and_then(|body| serde_json::from_str::<serde_json::Value>(&body).ok());
    let field = |key: &str| setup.as_ref().and_then(|setup| setup.get(key));
    let name = field("name").and_then(serde_json::Value::as_str);
    let at_ms = field("created_ms")
        .and_then(serde_json::Value::as_i64)
        .filter(|at_ms| *at_ms >= 0);
    let (Some(name), Some(at_ms)) = (name, at_ms) else {
        return Ok(false);
    };
    let text = created_text(name);
    let item = NewActivity {
        kind: ActivityKind::Created,
        actor: OWNER_ACTOR,
        agent: None,
        goal: None,
        text: &text,
        change_set: None,
    };
    record(db, project, &item, at_ms)?;
    Ok(true)
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

/// Tells whether a table has a column, false when the table is missing too. Inputs: the
/// store, the table and the column. Output: the answer.
fn has_column(db: &Connection, table: &str, column: &str) -> Result<bool, EngineError> {
    db.query_row(
        "SELECT COUNT(*) FROM pragma_table_info(?1) WHERE name=?2",
        params![table, column],
        |row| row.get::<_, i64>(0),
    )
    .map(|count| count > 0)
    .map_err(state)
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
    let number = if has_table(db, "landing_change_set_numbers")? {
        "(SELECT number FROM landing_change_set_numbers WHERE change_set=landing_change_sets.id)"
    } else {
        "NULL"
    };
    let message = if has_column(db, "landing_change_sets", "message")? {
        "message"
    } else {
        "''"
    };
    let mut statement = db
        .prepare(&format!(
            "SELECT id,label,prompt_line,updated_ms,agent_kind,files,{number},{message} \
             FROM landing_change_sets WHERE project=?1 AND state='accepted'"
        ))
        .map_err(state)?;
    let rows = statement
        .query_map([project.as_str()], |row| {
            let kind = match row.get::<_, String>(4)?.as_str() {
                "master" => crate::landing::AgentKind::Master,
                _ => crate::landing::AgentKind::Sub,
            };
            let label = row.get::<_, String>(1)?;
            let agent = crate::landing::agent_title(kind, &label);
            let number = row
                .get::<_, Option<i64>>(6)?
                .and_then(|number| u64::try_from(number).ok());
            let message = crate::landing::change_set_message(
                &row.get::<_, String>(7)?,
                &row.get::<_, String>(2)?,
                &label,
            );
            let named = crate::landing::change_set_words(number, &agent, &message, row.get(5)?);
            Ok(Past {
                kind: ActivityKind::Accepted,
                at_ms: row.get(3)?,
                actor: OWNER_ACTOR.into(),
                goal: None,
                text: accepted_text(OWNER_ACTOR, &named),
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
        let end = match status.as_str() {
            "done" => Some(ActivityKind::ResearchSaved),
            "failed" | "cancelled" => Some(ActivityKind::ResearchStopped),
            _ => None,
        };
        let ended = end.zip(finished);
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

/// Says whether an event `backfill` found is recorded already: by its kind and change set
/// for one that names a set, else by its kind, time, text and goal, not by who acted, since
/// an agent's name can change after the event. Takes the database, the project and the
/// event. Returns the answer, or the SQLite failure.
fn known(db: &Connection, project: &ProjectId, past: &Past) -> Result<bool, EngineError> {
    let change_set = past
        .change_set
        .as_deref()
        .map(|id| one_line(id, CHANGE_SET_BYTES_MAX));
    let goal = past
        .goal
        .as_deref()
        .map(|goal| shown(goal, GOAL_BYTES_MAX))
        .filter(|goal| !goal.is_empty());
    db.query_row(
        "SELECT EXISTS(SELECT 1 FROM project_activity WHERE project=?1 AND kind=?2 AND
         ((?3 IS NOT NULL AND change_set=?3)
          OR (?3 IS NULL AND at_ms=?4 AND text=?5 AND goal IS ?6)))",
        params![
            project.as_str(),
            past.kind.name(),
            change_set,
            past.at_ms,
            shown(&past.text, TEXT_BYTES_MAX),
            goal
        ],
        |row| row.get(0),
    )
    .map_err(state)
}

/// Clears a removed project's events and its backfill mark. Runs inside the removal's
/// transaction. Takes the database and the project. Returns Ok or the SQLite failure.
pub fn forget_project(db: &Connection, project: &ProjectId) -> Result<(), EngineError> {
    for table in [
        "project_activity",
        "project_activity_backfill",
        "project_activity_logged",
    ] {
        db.execute(
            &format!("DELETE FROM {table} WHERE project=?1"),
            [project.as_str()],
        )
        .map_err(state)?;
    }
    Ok(())
}

/// The text one event of an accepted change set carries, the same for the live writer
/// and the backfill: "You accepted change set #14 (Wren: checkout UI, 9 files)" when the
/// owner accepted it, "Accepted ..." when the engine finished an Accept. Takes who
/// accepted and the words naming the set. Returns the text.
pub fn accepted_text(by: &str, named: &str) -> String {
    if by == OWNER_ACTOR {
        format!("You accepted {named}")
    } else {
        format!("Accepted {named}")
    }
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
    research_line(research_verb(kind), job)
}

fn research_verb(kind: ActivityKind) -> &'static str {
    match kind {
        ActivityKind::ResearchStarted => "started",
        ActivityKind::ResearchSaved => "saved",
        ActivityKind::ResearchStopped => "stopped",
        ActivityKind::ResearchWaiting => "waiting for a pool",
        _ => unreachable!("research_text takes a research event"),
    }
}

fn research_line(verb: &str, job: &str) -> String {
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

/// Names a team agent for the activity the way the team list does: its kept persona and
/// handle, the persona alone, "Master", or its role and number when it has no kept name.
/// Takes the database and the agent id. Returns its label, or "Agent" and the id for a row
/// that is gone.
pub fn agent_actor(db: &Connection, agent: i64) -> Result<String, EngineError> {
    let role: Option<String> = db
        .query_row("SELECT role FROM team_agents WHERE id=?1", [agent], |row| {
            row.get(0)
        })
        .optional()
        .map_err(state)?;
    Ok(
        match role.as_deref().and_then(crate::team::AgentRole::parse) {
            Some(role) => {
                let name = crate::team::names::name(db, agent)?;
                crate::team::names::label(role, agent, name.as_ref())
            }
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
    .map_err(state)
}

/// Text as a row shows it: through `landing::scrubbed`, so a character a reader cannot see
/// is shown as an escape or breaks the line into words and an absolute local path is cut to
/// its last component, then made one line of at most `bytes`. Takes the text and the
/// bound. Returns the line.
fn shown(text: &str, bytes: usize) -> String {
    one_line(&crate::landing::scrubbed(text, &[]), bytes)
}

fn one_line(text: &str, bytes: usize) -> String {
    let joined = text
        .split_whitespace()
        .map(|word| word.chars().filter(|c| !c.is_control()).collect::<String>())
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
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
