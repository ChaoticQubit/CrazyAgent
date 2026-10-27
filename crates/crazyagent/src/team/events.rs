use super::{
    AgentRole, AgentStatus, MessageAuthor, MessageState, append_message, bound_text, error,
    make_room,
};
use crate::engine::EngineError;
use crate::protocol::ProjectId;
use crate::terminal::{AgentId, Host, HostError, type_prompt};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use std::collections::btree_map::Entry;
use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

pub const EVENT_TEXT_BYTES_MAX: usize = 1000;
pub const EVENTS_PER_PROJECT_MAX: i64 = 2000;
pub const EVENTS_QUEUED_MAX: i64 = 200;
pub const EVENTS_PER_WAKE_MAX: usize = 20;
pub const WAKE_BYTES_MAX: usize = super::MESSAGE_BYTES_MAX;
pub const HOURLY_CAP_MAX: u32 = 500;
pub const HOURLY_CAP_DEFAULT: u32 = 30;
pub const SEND_BYTES_MAX: usize = crate::terminal::PROMPT_BYTES_MAX;
pub const ESCALATION_BYTES_MAX: usize = 2000;
pub const NOTIFICATIONS_PER_PROJECT_MAX: i64 = 200;
pub const STATUS_EVENTS_MAX: i64 = 50;
pub const STATUS_AGENTS_MAX: i64 = super::STATUS_AGENTS_MAX;
pub const VIEW_NOTIFICATIONS_MAX: i64 = 20;
pub const APP_NOTIFICATIONS_MAX: i64 = 50;
pub const HOUR_MS: i64 = 3_600_000;
pub const SCAN_EVERY: Duration = Duration::from_secs(2);
const SCREEN_ROWS_SCANNED: usize = 40;
const QUESTION_ROWS_ABOVE: usize = 12;
const OPTIONS_MAX: usize = 9;
const TASK_EXCERPT_BYTES_MAX: usize = 200;
const EVENT_HISTORY: &str = "state<>'queued'";
const NOTIFICATION_HISTORY: &str = "1=1";
const MARKERS: [char; 5] = ['❯', '›', '>', '▶', '→'];
const BORDERS: [char; 4] = ['│', '|', '┃', '║'];

const _: () = assert!(EVENTS_QUEUED_MAX < EVENTS_PER_PROJECT_MAX);
const _: () = assert!(EVENT_TEXT_BYTES_MAX + 256 < WAKE_BYTES_MAX);
const _: () = assert!(HOURLY_CAP_DEFAULT <= HOURLY_CAP_MAX);

/// What a team event says about a sub-agent: it asks a question or waits on a choice, it
/// finished or failed its task, its change set is ready for review, or a progress summary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    Question,
    Finished,
    Failed,
    ChangeSetReady,
    Progress,
}

impl EventKind {
    pub const ALL: [Self; 5] = [
        Self::Question,
        Self::Finished,
        Self::Failed,
        Self::ChangeSetReady,
        Self::Progress,
    ];

    /// Names the kind as stored in the engine database. Takes the kind.
    /// Returns the snake case name.
    pub fn name(self) -> &'static str {
        match self {
            Self::Question => "question",
            Self::Finished => "finished",
            Self::Failed => "failed",
            Self::ChangeSetReady => "change_set_ready",
            Self::Progress => "progress",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.name() == value)
    }

    fn verb(self) -> &'static str {
        match self {
            Self::Question => "asks",
            Self::Finished => "finished",
            Self::Failed => "failed",
            Self::ChangeSetReady => "has changes ready",
            Self::Progress => "reports progress",
        }
    }
}

/// Where an event is on its way to the master: waiting in the queue, typed into the master,
/// muted by a wake rule that has its kind off, or carried by the master's own results
/// message because the master delegated that task.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventState {
    Queued,
    Delivered,
    Muted,
    Carried,
}

impl EventState {
    const ALL: [Self; 4] = [Self::Queued, Self::Delivered, Self::Muted, Self::Carried];

    fn name(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Delivered => "delivered",
            Self::Muted => "muted",
            Self::Carried => "carried",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|state| state.name() == value)
    }
}

/// One recorded team event as clients and the master's status read it. `label` names the
/// agent the way the master addresses it; `text` is one bounded line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TeamEvent {
    pub id: i64,
    pub agent: i64,
    pub label: String,
    pub task: Option<i64>,
    pub kind: EventKind,
    pub text: String,
    pub state: EventState,
    pub created_ms: i64,
    pub delivered_ms: Option<i64>,
}

/// When events wake the master: each kind on or off, and at most `hourly_cap` wakes an hour,
/// from 0, which never wakes, to `HOURLY_CAP_MAX`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WakeRule {
    pub question: bool,
    pub finished: bool,
    pub failed: bool,
    pub change_set_ready: bool,
    pub progress: bool,
    pub hourly_cap: u32,
}

impl Default for WakeRule {
    fn default() -> Self {
        Self {
            question: true,
            finished: true,
            failed: true,
            change_set_ready: true,
            progress: false,
            hourly_cap: HOURLY_CAP_DEFAULT,
        }
    }
}

impl WakeRule {
    /// Tells whether events of one kind wake the master under this rule. Takes the kind.
    /// Returns true when that kind is on.
    pub fn wakes(&self, kind: EventKind) -> bool {
        match kind {
            EventKind::Question => self.question,
            EventKind::Finished => self.finished,
            EventKind::Failed => self.failed,
            EventKind::ChangeSetReady => self.change_set_ready,
            EventKind::Progress => self.progress,
        }
    }
}

/// Which level a wake rule is set at: the app default, one project, or one agent of a project.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "level", rename_all = "snake_case", deny_unknown_fields)]
pub enum RuleTarget {
    App,
    Project { project_id: ProjectId },
    Agent { project_id: ProjectId, agent: i64 },
}

/// The level the rule in force came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleLevel {
    #[default]
    Shipped,
    App,
    Project,
    Agent,
}

/// Why the owner gets an app notification: the master escalated an owner-only decision, or
/// auto-wake stopped at its hourly cap.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotificationKind {
    Escalation,
    WakePaused,
}

impl NotificationKind {
    const ALL: [Self; 2] = [Self::Escalation, Self::WakePaused];

    fn name(self) -> &'static str {
        match self {
            Self::Escalation => "escalation",
            Self::WakePaused => "wake_paused",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.name() == value)
    }
}

/// One app notification for the owner; unseen ones count toward the app badge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Notification {
    pub id: i64,
    pub project_id: ProjectId,
    pub kind: NotificationKind,
    pub text: String,
    pub created_ms: i64,
    pub seen_ms: Option<i64>,
}

/// One live sub-agent as the master's status reads it, with the question it waits on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentLine {
    pub agent: i64,
    pub label: String,
    pub harness: String,
    pub status: AgentStatus,
    pub status_label: String,
    pub task: Option<i64>,
    pub task_prompt: Option<String>,
    pub waiting: Option<String>,
}

/// What the master's status tool shows: the project's live sub-agents, the newest events
/// and whether auto-wake is paused at its cap.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventsStatus {
    pub project_id: ProjectId,
    pub agents: Vec<AgentLine>,
    pub events: Vec<TeamEvent>,
    pub paused: bool,
}

impl EventsStatus {
    /// Writes the status as plain lines for the master's tool reply. Takes the status.
    /// Returns one line per agent, then the newest events, oldest first.
    pub fn text(&self) -> String {
        let mut text = String::new();
        if self.agents.is_empty() {
            text.push_str("No sub-agents are running.\n");
        }
        for agent in &self.agents {
            text.push_str(&format!(
                "{} ({}): {}",
                agent.label, agent.harness, agent.status_label
            ));
            if let (Some(task), Some(prompt)) = (agent.task, &agent.task_prompt) {
                text.push_str(&format!(", task {task}: {prompt}"));
            }
            if let Some(question) = &agent.waiting {
                text.push_str(&format!(", waiting on: {question}"));
            }
            text.push('\n');
        }
        if self.paused {
            text.push_str("Auto-wake is paused at its hourly cap.\n");
        }
        for event in &self.events {
            text.push_str(&line(event));
            text.push('\n');
        }
        text
    }
}

/// One agent's own wake rule, set at the agent level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentRule {
    pub agent: i64,
    pub rule: WakeRule,
}

/// The wake rules a project's settings show: what is stored at each level and the rule in
/// force for the project with the level it came from.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct WakeView {
    pub app: Option<WakeRule>,
    pub project: Option<WakeRule>,
    pub agents: Vec<AgentRule>,
    pub effective: WakeRule,
    pub level: RuleLevel,
}

/// One sub-agent that waits on a question its screen shows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WaitingAgent {
    pub agent: i64,
    pub text: String,
}

/// What the clients show about a project's events: the newest events, agents waiting on a
/// question, the project's newest notifications, the auto-wake pause and the wake rules.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct EventsView {
    pub recent: Vec<TeamEvent>,
    pub waiting: Vec<WaitingAgent>,
    pub notifications: Vec<Notification>,
    pub paused: bool,
    pub wake: WakeView,
    #[serde(default)]
    pub questions: Vec<AgentQuestions>,
}

/// How many question events of one sub-agent wait in the queue for the master.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentQuestions {
    pub agent: i64,
    pub count: u16,
}

/// What the app shows across projects: the app-level wake rule, the rule in force without a
/// project rule, the newest notifications of every project and how many are unseen.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct AppEvents {
    pub rule: Option<WakeRule>,
    pub effective: WakeRule,
    pub notifications: Vec<Notification>,
    pub unseen: i64,
}

/// The events typed into the master in one wake, oldest first, and their text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Wake {
    pub project: ProjectId,
    pub events: Vec<i64>,
    pub text: String,
}

/// One busy sub-agent whose terminal the watch reads for a waiting question.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Watched {
    pub agent: i64,
    pub project: ProjectId,
    pub task: Option<i64>,
}

/// Reads busy sub-agents' screens for a question or choice that waits on an answer, and
/// records each new one once. Remembers the last question seen per agent.
#[derive(Debug, Default)]
pub struct Watch {
    next: Option<Instant>,
    seen: BTreeMap<i64, String>,
}

impl Watch {
    /// Tells whether a scan is due, at most once each `SCAN_EVERY`, and starts the next wait.
    /// Takes the watch. Returns true when the caller should scan now.
    pub fn due(&mut self) -> bool {
        let now = Instant::now();
        if self.next.is_some_and(|next| now < next) {
            return false;
        }
        self.next = Some(now + SCAN_EVERY);
        true
    }

    /// Reads each watched agent's screen once: a new question is recorded as a question event
    /// and marks the agent waiting; a question that left the screen clears the mark. Agents no
    /// longer watched are forgotten. Takes the connection, the terminal host, the busy
    /// sub-agents and the time. Returns any SQLite failure; an unreadable screen is skipped.
    pub fn scan(
        &mut self,
        connection: &Connection,
        host: &dyn Host,
        agents: &[Watched],
        now: i64,
    ) -> Result<(), EngineError> {
        let current: BTreeSet<i64> = agents.iter().map(|watched| watched.agent).collect();
        let gone: Vec<i64> = self
            .seen
            .keys()
            .filter(|agent| !current.contains(agent))
            .copied()
            .collect();
        for agent in gone {
            self.seen.remove(&agent);
            clear_waiting(connection, agent)?;
        }
        for watched in agents {
            let Ok(name) = AgentId::team(watched.agent) else {
                continue;
            };
            let Ok(stream) = host.attach(&name) else {
                continue;
            };
            match question(&stream.screen) {
                Some(text) if self.seen.get(&watched.agent) != Some(&text) => {
                    record(
                        connection,
                        &watched.project,
                        watched.agent,
                        watched.task,
                        EventKind::Question,
                        &text,
                        now,
                    )?;
                    set_waiting(connection, &watched.project, watched.agent, &text, now)?;
                    self.seen.insert(watched.agent, text);
                }
                Some(_) => {}
                None => {
                    if self.seen.remove(&watched.agent).is_some() {
                        clear_waiting(connection, watched.agent)?;
                    }
                }
            }
        }
        assert!(self.seen.keys().all(|agent| current.contains(agent)));
        Ok(())
    }
}

/// Creates the team event tables when the engine database opens. Takes the engine
/// connection. Returns any SQLite failure.
pub fn initialize(connection: &Connection) -> Result<(), EngineError> {
    connection
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS team_events (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                project TEXT NOT NULL,
                agent INTEGER NOT NULL,
                task INTEGER,
                kind TEXT NOT NULL,
                text TEXT NOT NULL,
                state TEXT NOT NULL,
                wake INTEGER,
                created_ms INTEGER NOT NULL,
                delivered_ms INTEGER
            );
            CREATE INDEX IF NOT EXISTS team_events_project ON team_events(project, state);
            CREATE TABLE IF NOT EXISTS team_event_wakes (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                project TEXT NOT NULL,
                at_ms INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS team_event_wakes_project ON team_event_wakes(project, at_ms);
            CREATE TABLE IF NOT EXISTS team_event_pauses (
                project TEXT NOT NULL,
                agent INTEGER NOT NULL,
                since_ms INTEGER NOT NULL,
                PRIMARY KEY(project, agent)
            );
            CREATE TABLE IF NOT EXISTS team_event_waiting (
                agent INTEGER PRIMARY KEY,
                project TEXT NOT NULL,
                text TEXT NOT NULL,
                since_ms INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS team_wake_rules (
                project TEXT NOT NULL,
                agent INTEGER NOT NULL,
                question INTEGER NOT NULL,
                finished INTEGER NOT NULL,
                failed INTEGER NOT NULL,
                change_set_ready INTEGER NOT NULL,
                progress INTEGER NOT NULL,
                hourly_cap INTEGER NOT NULL,
                PRIMARY KEY(project, agent)
            );
            CREATE TABLE IF NOT EXISTS team_notifications (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                project TEXT NOT NULL,
                kind TEXT NOT NULL,
                text TEXT NOT NULL,
                created_ms INTEGER NOT NULL,
                seen_ms INTEGER
            );
            CREATE INDEX IF NOT EXISTS team_notifications_project ON team_notifications(project, seen_ms);",
        )
        .map_err(error)
}

fn immediate(connection: &Connection) -> Result<Transaction<'_>, EngineError> {
    Transaction::new_unchecked(connection, TransactionBehavior::Immediate).map_err(error)
}

fn one_line(text: &str, max: usize) -> String {
    let joined = text
        .split_whitespace()
        .map(|word| word.chars().filter(|c| !c.is_control()).collect::<String>())
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    bound_text(&joined, max)
}

/// Names an agent the way events and the master address it until persona names land.
/// Takes the harness and the team agent id. Returns `<harness>-<id>`.
pub fn label(harness: &str, agent: i64) -> String {
    format!("{harness}-{agent}")
}

/// Records one event about a project's sub-agent in the queue for the master: the text
/// becomes one line of at most `EVENT_TEXT_BYTES_MAX` bytes, and the oldest history makes room.
/// Takes the connection, project, agent, its task, the kind, the text and the time. Returns
/// the event id, or none for blank text, an agent of another project or a full queue.
pub fn record(
    connection: &Connection,
    project: &ProjectId,
    agent: i64,
    task: Option<i64>,
    kind: EventKind,
    text: &str,
    now: i64,
) -> Result<Option<i64>, EngineError> {
    let tx = immediate(connection)?;
    let id = record_in(
        &tx,
        project,
        agent,
        task,
        kind,
        text,
        EventState::Queued,
        now,
    )?;
    tx.commit().map_err(error)?;
    Ok(id)
}

#[allow(clippy::too_many_arguments)]
fn record_in(
    tx: &Transaction<'_>,
    project: &ProjectId,
    agent: i64,
    task: Option<i64>,
    kind: EventKind,
    text: &str,
    state: EventState,
    now: i64,
) -> Result<Option<i64>, EngineError> {
    let text = one_line(text, EVENT_TEXT_BYTES_MAX);
    if text.is_empty() {
        return Ok(None);
    }
    let owned: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM team_agents WHERE id=?1 AND project=?2)",
            params![agent, project.as_str()],
            |row| row.get(0),
        )
        .map_err(error)?;
    if !owned {
        return Ok(None);
    }
    let queued: i64 = tx
        .query_row(
            "SELECT COUNT(*) FROM team_events WHERE project=?1 AND state='queued'",
            [project.as_str()],
            |row| row.get(0),
        )
        .map_err(error)?;
    if state == EventState::Queued && queued >= EVENTS_QUEUED_MAX {
        return Ok(None);
    }
    make_room(
        tx,
        "team_events",
        EVENT_HISTORY,
        project,
        EVENTS_PER_PROJECT_MAX,
    )?;
    tx.execute(
        "INSERT INTO team_events(project,agent,task,kind,text,state,created_ms) VALUES (?1,?2,?3,?4,?5,?6,?7)",
        params![project.as_str(), agent, task, kind.name(), text, state.name(), now],
    )
    .map_err(error)?;
    let id = tx.last_insert_rowid();
    assert!(id > 0);
    Ok(Some(id))
}

/// Records how a sub-agent's task ended once its turn finished: done becomes a finished
/// event, or a question when its last line asks one; failed becomes a failed event. A task
/// the master delegated reaches it in the results message, so its event is kept as carried.
/// The agent's waiting mark ends with the turn. Takes the connection, the task and the time.
/// Returns the event id, or none for a task still queued, cancelled or unknown.
pub fn task_ended(
    connection: &Connection,
    task: i64,
    now: i64,
) -> Result<Option<i64>, EngineError> {
    type Row = (
        String,
        Option<i64>,
        String,
        String,
        Option<String>,
        Option<String>,
    );
    let row: Option<Row> = connection
        .query_row(
            "SELECT project,agent,source,status,result,error FROM team_tasks WHERE id=?1",
            [task],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            },
        )
        .optional()
        .map_err(error)?;
    let Some((project, Some(agent), source, status, result, problem)) = row else {
        return Ok(None);
    };
    clear_waiting(connection, agent)?;
    let Ok(project) = ProjectId::new(project) else {
        return Ok(None);
    };
    let (kind, text) = match status.as_str() {
        "done" => {
            let result = result.unwrap_or_default();
            let asks = result
                .lines()
                .rev()
                .map(str::trim)
                .find(|line| !line.is_empty())
                .is_some_and(|line| line.ends_with('?'));
            let kind = if asks {
                EventKind::Question
            } else {
                EventKind::Finished
            };
            let text = if result.trim().is_empty() {
                "Done, with no reply text.".to_owned()
            } else {
                result
            };
            (kind, text)
        }
        "failed" => (
            EventKind::Failed,
            problem.unwrap_or_else(|| "The task failed.".to_owned()),
        ),
        _ => return Ok(None),
    };
    let state = if source == "master" {
        EventState::Carried
    } else {
        EventState::Queued
    };
    let tx = immediate(connection)?;
    let id = record_in(&tx, &project, agent, Some(task), kind, &text, state, now)?;
    tx.commit().map_err(error)?;
    Ok(id)
}

fn set_waiting(
    connection: &Connection,
    project: &ProjectId,
    agent: i64,
    text: &str,
    now: i64,
) -> Result<(), EngineError> {
    connection
        .execute(
            "INSERT INTO team_event_waiting(agent,project,text,since_ms) VALUES (?1,?2,?3,?4)
             ON CONFLICT(agent) DO UPDATE SET project=excluded.project, text=excluded.text, since_ms=excluded.since_ms",
            params![agent, project.as_str(), one_line(text, EVENT_TEXT_BYTES_MAX), now],
        )
        .map_err(error)?;
    Ok(())
}

fn clear_waiting(connection: &Connection, agent: i64) -> Result<(), EngineError> {
    connection
        .execute("DELETE FROM team_event_waiting WHERE agent=?1", [agent])
        .map_err(error)?;
    Ok(())
}

/// Reads which of a project's agents wait on a question their screen shows.
/// Takes the connection and the project. Returns each waiting agent's question.
pub fn waiting(
    connection: &Connection,
    project: &ProjectId,
) -> Result<BTreeMap<i64, String>, EngineError> {
    let mut statement = connection
        .prepare("SELECT agent,text FROM team_event_waiting WHERE project=?1")
        .map_err(error)?;
    statement
        .query_map([project.as_str()], |row| Ok((row.get(0)?, row.get(1)?)))
        .map_err(error)?
        .collect::<Result<_, _>>()
        .map_err(error)
}

/// Counts each sub-agent's question events that have not reached the master yet.
/// Takes the connection and the project. Returns one entry per agent with any, by agent id.
pub fn questions(
    connection: &Connection,
    project: &ProjectId,
) -> Result<Vec<AgentQuestions>, EngineError> {
    let mut statement = connection
        .prepare(
            "SELECT agent,COUNT(*) FROM team_events WHERE project=?1 AND kind='question' AND state='queued'
             GROUP BY agent ORDER BY agent",
        )
        .map_err(error)?;
    statement
        .query_map([project.as_str()], |row| {
            let count: i64 = row.get(1)?;
            assert!(count > 0);
            Ok(AgentQuestions {
                agent: row.get(0)?,
                count: u16::try_from(count).unwrap_or(u16::MAX),
            })
        })
        .map_err(error)?
        .collect::<Result<_, _>>()
        .map_err(error)
}

/// Finds the project's sub-agent a master names: `<harness>-<id>` as events show it,
/// `team-<id>` or the bare id. Takes the connection, the project and the name.
/// Returns the team agent id, or none for an unknown name, another project's agent or a master.
pub fn resolve(
    connection: &Connection,
    project: &ProjectId,
    name: &str,
) -> Result<Option<i64>, EngineError> {
    let name = name.trim().trim_start_matches('@');
    let (prefix, digits) = match name.rsplit_once('-') {
        Some((prefix, digits)) => (Some(prefix), digits),
        None => (None, name),
    };
    let Ok(agent) = digits.parse::<i64>() else {
        return Ok(None);
    };
    if agent <= 0 || agent.to_string() != digits {
        return Ok(None);
    }
    let row: Option<(String, String)> = connection
        .query_row(
            "SELECT harness,role FROM team_agents WHERE id=?1 AND project=?2",
            params![agent, project.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(error)?;
    let Some((harness, role)) = row else {
        return Ok(None);
    };
    let named = prefix.is_none_or(|prefix| prefix == harness || prefix == "team");
    Ok((named && AgentRole::parse(&role) == Some(AgentRole::Sub)).then_some(agent))
}

/// Types a master's message or answer into one of its project's sub-agents' terminal as one
/// submission, the way an owner prompt is typed: control keys are dropped, so the text
/// cannot press keys. Clears the agent's waiting mark. Takes the connection, the terminal
/// host, the project, the agent's name as `resolve` reads it and the text. Returns the agent
/// id, or a plain refusal code for blank or long text, an unknown or stopped agent or an
/// unreachable terminal.
pub fn send_to_agent(
    connection: &Connection,
    host: &dyn Host,
    project: &ProjectId,
    agent: &str,
    text: &str,
) -> Result<Result<i64, &'static str>, EngineError> {
    if text.trim().is_empty() || text.len() > SEND_BYTES_MAX {
        return Ok(Err("team_message_invalid"));
    }
    let Some(id) = resolve(connection, project, agent)? else {
        return Ok(Err("team_agent_not_found"));
    };
    let status: String = connection
        .query_row("SELECT status FROM team_agents WHERE id=?1", [id], |row| {
            row.get(0)
        })
        .map_err(error)?;
    if !AgentStatus::parse(&status).is_some_and(AgentStatus::live) {
        return Ok(Err("team_agent_not_running"));
    }
    let name = AgentId::team(id).map_err(EngineError::from)?;
    match type_prompt(host, &name, text) {
        Ok(true) => {}
        Ok(false) => return Ok(Err("team_message_invalid")),
        Err(HostError(code)) if code == "terminal_agent_unknown" => {
            return Ok(Err("team_agent_not_running"));
        }
        Err(_) => return Ok(Err("team_agent_unreachable")),
    }
    clear_waiting(connection, id)?;
    Ok(Ok(id))
}

/// Raises an app notification for a decision only the owner makes: security, destructive or
/// irreversible actions, spending, product or scope. The desktop shows it as a system
/// notification and counts it in the badge until seen. Takes the connection, the project,
/// the text and the time. Returns the notification id, or `team_escalation_invalid`.
pub fn escalate(
    connection: &Connection,
    project: &ProjectId,
    text: &str,
    now: i64,
) -> Result<Result<i64, &'static str>, EngineError> {
    let text = one_line(text, ESCALATION_BYTES_MAX + 1);
    if text.is_empty() || text.len() > ESCALATION_BYTES_MAX {
        return Ok(Err("team_escalation_invalid"));
    }
    let tx = immediate(connection)?;
    let id = notify(&tx, project, NotificationKind::Escalation, &text, now)?;
    tx.commit().map_err(error)?;
    Ok(Ok(id))
}

fn notify(
    tx: &Transaction<'_>,
    project: &ProjectId,
    kind: NotificationKind,
    text: &str,
    now: i64,
) -> Result<i64, EngineError> {
    make_room(
        tx,
        "team_notifications",
        NOTIFICATION_HISTORY,
        project,
        NOTIFICATIONS_PER_PROJECT_MAX,
    )?;
    tx.execute(
        "INSERT INTO team_notifications(project,kind,text,created_ms) VALUES (?1,?2,?3,?4)",
        params![project.as_str(), kind.name(), text, now],
    )
    .map_err(error)?;
    let id = tx.last_insert_rowid();
    assert!(id > 0);
    Ok(id)
}

/// Reads the owner's notifications, newest first: one project's, or every project's for the
/// app badge. Takes the connection, the project or none, and the most to read.
/// Returns the notifications.
pub fn notifications(
    connection: &Connection,
    project: Option<&ProjectId>,
    limit: i64,
) -> Result<Vec<Notification>, EngineError> {
    let mut statement = connection
        .prepare(
            "SELECT id,project,kind,text,created_ms,seen_ms FROM team_notifications
             WHERE ?1 IS NULL OR project=?1 ORDER BY id DESC LIMIT ?2",
        )
        .map_err(error)?;
    let rows: Vec<(i64, String, String, String, i64, Option<i64>)> = statement
        .query_map(
            params![project.map(ProjectId::as_str), limit.max(0)],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            },
        )
        .map_err(error)?
        .collect::<Result<_, _>>()
        .map_err(error)?;
    Ok(rows
        .into_iter()
        .filter_map(|(id, project, kind, text, created_ms, seen_ms)| {
            Some(Notification {
                id,
                project_id: ProjectId::new(project).ok()?,
                kind: NotificationKind::parse(&kind)?,
                text,
                created_ms,
                seen_ms,
            })
        })
        .collect())
}

/// Marks every notification up to `upto` as seen, so it leaves the badge.
/// Takes the connection, the project or none for every project, the newest id and the time.
/// Returns how many changed.
pub fn seen(
    connection: &Connection,
    project: Option<&ProjectId>,
    upto: i64,
    now: i64,
) -> Result<usize, EngineError> {
    connection
        .execute(
            "UPDATE team_notifications SET seen_ms=?3 WHERE seen_ms IS NULL AND id<=?2 AND (?1 IS NULL OR project=?1)",
            params![project.map(ProjectId::as_str), upto, now],
        )
        .map_err(error)
}

fn agent_label(connection: &Connection, agent: i64) -> Result<String, EngineError> {
    let harness: Option<String> = connection
        .query_row(
            "SELECT harness FROM team_agents WHERE id=?1",
            [agent],
            |row| row.get(0),
        )
        .optional()
        .map_err(error)?;
    Ok(label(harness.as_deref().unwrap_or("agent"), agent))
}

fn event_row(connection: &Connection, row: &rusqlite::Row<'_>) -> rusqlite::Result<TeamEvent> {
    let kind: String = row.get(3)?;
    let state: String = row.get(5)?;
    let agent: i64 = row.get(1)?;
    let invalid = |column: &str| {
        rusqlite::Error::InvalidColumnType(0, column.into(), rusqlite::types::Type::Text)
    };
    Ok(TeamEvent {
        id: row.get(0)?,
        agent,
        label: agent_label(connection, agent).unwrap_or_else(|_| label("agent", agent)),
        task: row.get(2)?,
        kind: EventKind::parse(&kind).ok_or_else(|| invalid("kind"))?,
        text: row.get(4)?,
        state: EventState::parse(&state).ok_or_else(|| invalid("state"))?,
        created_ms: row.get(6)?,
        delivered_ms: row.get(7)?,
    })
}

const EVENT_COLUMNS: &str = "id,agent,task,kind,text,state,created_ms,delivered_ms";

/// Reads a project's newest events, oldest first. Takes the connection, the project and the
/// most to read. Returns the events.
pub fn events(
    connection: &Connection,
    project: &ProjectId,
    limit: i64,
) -> Result<Vec<TeamEvent>, EngineError> {
    let mut statement = connection
        .prepare(&format!(
            "SELECT * FROM (SELECT {EVENT_COLUMNS} FROM team_events WHERE project=?1 ORDER BY id DESC LIMIT ?2) ORDER BY id"
        ))
        .map_err(error)?;
    statement
        .query_map(params![project.as_str(), limit.max(0)], |row| {
            event_row(connection, row)
        })
        .map_err(error)?
        .collect::<Result<_, _>>()
        .map_err(error)
}

/// Reads what the master's status tool shows for a project: its live sub-agents with their
/// task and any question they wait on, the newest events and the auto-wake pause.
/// Takes the connection and the project. Returns the bounded status.
pub fn status(connection: &Connection, project: &ProjectId) -> Result<EventsStatus, EngineError> {
    let waiting = waiting(connection, project)?;
    type Row = (i64, String, String, Option<i64>, Option<String>);
    let mut statement = connection
        .prepare(
            "SELECT a.id,a.harness,a.status,a.task,t.prompt FROM team_agents a
             LEFT JOIN team_tasks t ON t.id=a.task
             WHERE a.project=?1 AND a.role='sub' AND a.status IN ('starting','idle','working')
             ORDER BY a.id LIMIT ?2",
        )
        .map_err(error)?;
    let rows: Vec<Row> = statement
        .query_map(params![project.as_str(), STATUS_AGENTS_MAX], |row| {
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
    let agents = rows
        .into_iter()
        .filter_map(|(agent, harness, status, task, prompt)| {
            let status = AgentStatus::parse(&status)?;
            Some(AgentLine {
                agent,
                label: label(&harness, agent),
                harness,
                status,
                status_label: status.label().to_owned(),
                task,
                task_prompt: prompt.map(|prompt| one_line(&prompt, TASK_EXCERPT_BYTES_MAX)),
                waiting: waiting.get(&agent).cloned(),
            })
        })
        .collect();
    let paused: bool = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM team_event_pauses WHERE project=?1)",
            [project.as_str()],
            |row| row.get(0),
        )
        .map_err(error)?;
    Ok(EventsStatus {
        project_id: project.clone(),
        agents,
        events: events(connection, project, STATUS_EVENTS_MAX)?,
        paused,
    })
}

fn line(event: &TeamEvent) -> String {
    let task = event
        .task
        .map_or_else(String::new, |task| format!(" [task {task}]"));
    format!(
        "[team] {} {}: {}{task}",
        event.label,
        event.kind.verb(),
        event.text
    )
}

/// Turns a terminal screen, as an attach redraws it, into its text rows: escape sequences
/// and control characters other than tab are dropped, rows end at line breaks and lose
/// trailing spaces. Takes the screen bytes. Returns the rows in order.
pub fn screen_rows(screen: &[u8]) -> Vec<String> {
    let mut plain = Vec::with_capacity(screen.len());
    let mut at = 0;
    while at < screen.len() {
        let byte = screen[at];
        at += 1;
        if byte == 0x1b {
            match screen.get(at) {
                Some(b'[') => {
                    at += 1;
                    while at < screen.len() && !(0x40..=0x7e).contains(&screen[at]) {
                        at += 1;
                    }
                    at += 1;
                }
                Some(b']') => {
                    at += 1;
                    while at < screen.len() && screen[at] != 0x07 && screen[at] != 0x1b {
                        at += 1;
                    }
                    at += if screen.get(at) == Some(&0x1b) { 2 } else { 1 };
                }
                Some(_) => at += 1,
                None => {}
            }
            continue;
        }
        if byte == b'\n' || byte == b'\t' || byte >= 0x20 {
            plain.push(byte);
        }
    }
    String::from_utf8_lossy(&plain)
        .split('\n')
        .map(|row| row.trim_end().to_owned())
        .collect()
}

fn option(row: &str) -> Option<(usize, &str, bool)> {
    let row = row.trim_start();
    let (marked, rest) = match row.chars().next() {
        Some(marker) if MARKERS.contains(&marker) => (true, row[marker.len_utf8()..].trim_start()),
        _ => (false, row),
    };
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    if digits == 0 || digits > 2 {
        return None;
    }
    let number: usize = rest[..digits].parse().ok()?;
    let after = &rest[digits..];
    let text = after
        .strip_prefix('.')
        .or_else(|| after.strip_prefix(')'))?
        .strip_prefix(' ')?
        .trim();
    (!text.is_empty()).then_some((number, text, marked))
}

/// Finds a question an agent's app waits on in its screen: a numbered choice list with the
/// app's selection marker on one option, such as Claude Code's permission and question
/// prompts and Codex's approval prompts, with the question line above it. A plain numbered
/// list without a marker is not a question. Takes the screen bytes. Returns one bounded line
/// with the question and its options, or none.
pub fn question(screen: &[u8]) -> Option<String> {
    let rows: Vec<String> = screen_rows(screen)
        .into_iter()
        .map(|row| {
            row.trim_matches(|c: char| BORDERS.contains(&c) || c.is_whitespace())
                .to_owned()
        })
        .filter(|row| !row.is_empty())
        .collect();
    let rows = &rows[rows.len().saturating_sub(SCREEN_ROWS_SCANNED)..];
    let first = rows
        .iter()
        .rposition(|row| option(row).is_some_and(|(number, ..)| number == 1))?;
    let mut options = Vec::new();
    let mut marked = false;
    for row in &rows[first..] {
        match option(row) {
            Some((number, text, marker))
                if number == options.len() + 1 && options.len() < OPTIONS_MAX =>
            {
                marked |= marker;
                options.push(format!("{number}. {text}"));
            }
            Some(_) => break,
            None => {}
        }
    }
    if options.len() < 2 || !marked {
        return None;
    }
    let above = &rows[first.saturating_sub(QUESTION_ROWS_ABOVE)..first];
    let asked = above
        .iter()
        .rev()
        .find(|row| row.ends_with('?'))
        .or_else(|| above.last())?;
    let text = one_line(
        &format!("{asked} | {}", options.join(" | ")),
        EVENT_TEXT_BYTES_MAX,
    );
    assert!(text.len() <= EVENT_TEXT_BYTES_MAX);
    Some(text)
}

/// Reads the wake rule in force for a project's agent: the agent's own, else the project's,
/// else the app default, else the shipped default. Takes the connection, the project or none
/// for the app level, and the agent or none for the project level.
/// Returns the rule and the level it came from.
pub fn rule(
    connection: &Connection,
    project: Option<&ProjectId>,
    agent: Option<i64>,
) -> Result<(WakeRule, RuleLevel), EngineError> {
    let found: Option<(String, i64, WakeRule)> = connection
        .query_row(
            "SELECT project,agent,question,finished,failed,change_set_ready,progress,hourly_cap
             FROM team_wake_rules
             WHERE (project='' AND agent=0) OR (project=?1 AND agent=0) OR (project=?1 AND agent=?2)
             ORDER BY agent DESC, project DESC LIMIT 1",
            params![project.map_or("", ProjectId::as_str), agent.unwrap_or(0)],
            |row| {
                let cap: i64 = row.get(7)?;
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    WakeRule {
                        question: row.get(2)?,
                        finished: row.get(3)?,
                        failed: row.get(4)?,
                        change_set_ready: row.get(5)?,
                        progress: row.get(6)?,
                        hourly_cap: u32::try_from(cap).unwrap_or(0).min(HOURLY_CAP_MAX),
                    },
                ))
            },
        )
        .optional()
        .map_err(error)?;
    Ok(match found {
        None => (WakeRule::default(), RuleLevel::Shipped),
        Some((stored, 0, rule)) if stored.is_empty() => (rule, RuleLevel::App),
        Some((_, 0, rule)) => (rule, RuleLevel::Project),
        Some((_, _, rule)) => (rule, RuleLevel::Agent),
    })
}

fn stored_rules(
    connection: &Connection,
    project: &str,
) -> Result<Vec<(i64, WakeRule)>, EngineError> {
    let mut statement = connection
        .prepare(
            "SELECT agent,question,finished,failed,change_set_ready,progress,hourly_cap
             FROM team_wake_rules WHERE project=?1 ORDER BY agent",
        )
        .map_err(error)?;
    statement
        .query_map([project], |row| {
            let cap: i64 = row.get(6)?;
            Ok((
                row.get(0)?,
                WakeRule {
                    question: row.get(1)?,
                    finished: row.get(2)?,
                    failed: row.get(3)?,
                    change_set_ready: row.get(4)?,
                    progress: row.get(5)?,
                    hourly_cap: u32::try_from(cap).unwrap_or(0).min(HOURLY_CAP_MAX),
                },
            ))
        })
        .map_err(error)?
        .collect::<Result<_, _>>()
        .map_err(error)
}

/// Reads what a project's team view shows about events and wake rules.
/// Takes the connection and the project. Returns the bounded view.
pub fn view(connection: &Connection, project: &ProjectId) -> Result<EventsView, EngineError> {
    let app = stored_rules(connection, "")?
        .into_iter()
        .find(|(agent, _)| *agent == 0)
        .map(|(_, rule)| rule);
    let mut own = stored_rules(connection, project.as_str())?;
    let mine = own
        .iter()
        .position(|(agent, _)| *agent == 0)
        .map(|at| own.remove(at).1);
    let (effective, level) = rule(connection, Some(project), None)?;
    let paused: bool = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM team_event_pauses WHERE project=?1)",
            [project.as_str()],
            |row| row.get(0),
        )
        .map_err(error)?;
    Ok(EventsView {
        recent: events(connection, project, STATUS_EVENTS_MAX)?,
        waiting: waiting(connection, project)?
            .into_iter()
            .map(|(agent, text)| WaitingAgent { agent, text })
            .collect(),
        notifications: notifications(connection, Some(project), VIEW_NOTIFICATIONS_MAX)?,
        paused,
        wake: WakeView {
            app,
            project: mine,
            agents: own
                .into_iter()
                .map(|(agent, rule)| AgentRule { agent, rule })
                .collect(),
            effective,
            level,
        },
        questions: questions(connection, project)?,
    })
}

/// Reads what the app shows across projects: the app wake rule and the newest notifications.
/// Takes the connection. Returns the view with the unseen count for the badge.
pub fn app(connection: &Connection) -> Result<AppEvents, EngineError> {
    let rule = stored_rules(connection, "")?
        .into_iter()
        .find(|(agent, _)| *agent == 0)
        .map(|(_, rule)| rule);
    let unseen: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM team_notifications WHERE seen_ms IS NULL",
            [],
            |row| row.get(0),
        )
        .map_err(error)?;
    Ok(AppEvents {
        rule,
        effective: rule.unwrap_or_default(),
        notifications: notifications(connection, None, APP_NOTIFICATIONS_MAX)?,
        unseen,
    })
}

/// Sets or clears the wake rule at one level; clearing returns that level to the level
/// above it. Takes the connection, the level and the rule or none.
/// Returns Ok, or `team_wake_cap_invalid` or `team_agent_not_found`.
pub fn set_rule(
    connection: &Connection,
    target: &RuleTarget,
    rule: Option<&WakeRule>,
) -> Result<Result<(), &'static str>, EngineError> {
    if rule.is_some_and(|rule| rule.hourly_cap > HOURLY_CAP_MAX) {
        return Ok(Err("team_wake_cap_invalid"));
    }
    let (project, agent) = match target {
        RuleTarget::App => ("", 0),
        RuleTarget::Project { project_id } => (project_id.as_str(), 0),
        RuleTarget::Agent { project_id, agent } => {
            let owned: bool = connection
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM team_agents WHERE id=?1 AND project=?2 AND role='sub')",
                    params![agent, project_id.as_str()],
                    |row| row.get(0),
                )
                .map_err(error)?;
            if *agent <= 0 || !owned {
                return Ok(Err("team_agent_not_found"));
            }
            (project_id.as_str(), *agent)
        }
    };
    match rule {
        None => connection.execute(
            "DELETE FROM team_wake_rules WHERE project=?1 AND agent=?2",
            params![project, agent],
        ),
        Some(rule) => connection.execute(
            "INSERT INTO team_wake_rules(project,agent,question,finished,failed,change_set_ready,progress,hourly_cap)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8)
             ON CONFLICT(project,agent) DO UPDATE SET question=excluded.question, finished=excluded.finished,
             failed=excluded.failed, change_set_ready=excluded.change_set_ready, progress=excluded.progress,
             hourly_cap=excluded.hourly_cap",
            params![
                project,
                agent,
                rule.question,
                rule.finished,
                rule.failed,
                rule.change_set_ready,
                rule.progress,
                rule.hourly_cap
            ],
        ),
    }
    .map_err(error)?;
    Ok(Ok(()))
}

fn pause(
    tx: &Transaction<'_>,
    project: &ProjectId,
    agent: i64,
    cap: u32,
    now: i64,
) -> Result<(), EngineError> {
    let added = tx
        .execute(
            "INSERT OR IGNORE INTO team_event_pauses(project,agent,since_ms) VALUES (?1,?2,?3)",
            params![project.as_str(), agent, now],
        )
        .map_err(error)?;
    if added == 0 {
        return Ok(());
    }
    let whose = if agent == 0 {
        format!("project {}", project.as_str())
    } else {
        agent_label(tx, agent)?
    };
    let text = format!(
        "Auto-wake of the master for {whose} paused after {cap} wakes in the last hour. Events wait in the queue until the hour rolls over or you raise the limit."
    );
    notify(tx, project, NotificationKind::WakePaused, &text, now).map(drop)
}

/// Picks the events that wake the master next, oldest first, under each agent's wake rule:
/// an event whose kind is off is muted; at the project's hourly cap, or an agent's own cap,
/// those events wait and the owner is notified once per pause. At most `EVENTS_PER_WAKE_MAX`
/// events and `WAKE_BYTES_MAX` bytes go in one wake. Takes the transaction, the project and
/// the time. Returns the wake, or none when nothing may wake the master now.
pub fn due(
    tx: &Transaction<'_>,
    project: &ProjectId,
    now: i64,
) -> Result<Option<Wake>, EngineError> {
    let since = now.saturating_sub(HOUR_MS);
    tx.execute(
        "DELETE FROM team_event_wakes WHERE project=?1 AND at_ms<?2",
        params![project.as_str(), since],
    )
    .map_err(error)?;
    let queued: Vec<TeamEvent> = {
        let mut statement = tx
            .prepare(&format!(
                "SELECT {EVENT_COLUMNS} FROM team_events WHERE project=?1 AND state='queued' ORDER BY id LIMIT ?2"
            ))
            .map_err(error)?;
        statement
            .query_map(params![project.as_str(), EVENTS_QUEUED_MAX], |row| {
                event_row(tx, row)
            })
            .map_err(error)?
            .collect::<Result<_, _>>()
            .map_err(error)?
    };
    if queued.is_empty() {
        return Ok(None);
    }
    let mut rules: BTreeMap<i64, (WakeRule, RuleLevel)> = BTreeMap::new();
    let mut wanted = Vec::new();
    for event in queued {
        if let Entry::Vacant(slot) = rules.entry(event.agent) {
            slot.insert(rule(tx, Some(project), Some(event.agent))?);
        }
        if rules[&event.agent].0.wakes(event.kind) {
            wanted.push(event);
        } else {
            tx.execute(
                "UPDATE team_events SET state='muted' WHERE id=?1 AND state='queued'",
                [event.id],
            )
            .map_err(error)?;
        }
    }
    if wanted.is_empty() {
        return Ok(None);
    }
    let cap = rule(tx, Some(project), None)?.0.hourly_cap;
    let woken: i64 = tx
        .query_row(
            "SELECT COUNT(*) FROM team_event_wakes WHERE project=?1 AND at_ms>=?2",
            params![project.as_str(), since],
            |row| row.get(0),
        )
        .map_err(error)?;
    if woken >= i64::from(cap) {
        pause(tx, project, 0, cap, now)?;
        return Ok(None);
    }
    let mut capped = BTreeSet::new();
    for (agent, (rule, level)) in &rules {
        if *level != RuleLevel::Agent {
            continue;
        }
        let woke_for: i64 = tx
            .query_row(
                "SELECT COUNT(DISTINCT e.wake) FROM team_events e JOIN team_event_wakes w ON w.id=e.wake
                 WHERE e.project=?1 AND e.agent=?2 AND w.at_ms>=?3",
                params![project.as_str(), agent, since],
                |row| row.get(0),
            )
            .map_err(error)?;
        if woke_for >= i64::from(rule.hourly_cap) {
            pause(tx, project, *agent, rule.hourly_cap, now)?;
            capped.insert(*agent);
        }
    }
    let mut events = Vec::new();
    let mut text = String::new();
    for event in wanted.iter().filter(|event| !capped.contains(&event.agent)) {
        let next = line(event);
        if events.len() >= EVENTS_PER_WAKE_MAX
            || (!text.is_empty() && text.len() + 1 + next.len() > WAKE_BYTES_MAX)
        {
            break;
        }
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(&next);
        events.push(event.id);
    }
    if events.is_empty() {
        return Ok(None);
    }
    assert!(text.len() <= WAKE_BYTES_MAX && events.len() <= EVENTS_PER_WAKE_MAX);
    Ok(Some(Wake {
        project: project.clone(),
        events,
        text,
    }))
}

/// Marks a wake's events delivered once its text reached the master, counts the wake toward
/// the hourly cap and ends the pauses of that project and of the agents it carried.
/// Takes the transaction, the wake and the time. Returns the wake id.
pub fn woke(tx: &Transaction<'_>, wake: &Wake, now: i64) -> Result<i64, EngineError> {
    assert!(!wake.events.is_empty());
    tx.execute(
        "INSERT INTO team_event_wakes(project,at_ms) VALUES (?1,?2)",
        params![wake.project.as_str(), now],
    )
    .map_err(error)?;
    let id = tx.last_insert_rowid();
    assert!(id > 0);
    for event in &wake.events {
        let changed = tx
            .execute(
                "UPDATE team_events SET state='delivered', delivered_ms=?2, wake=?3 WHERE id=?1 AND state='queued'",
                params![event, now, id],
            )
            .map_err(error)?;
        assert!(changed <= 1);
    }
    tx.execute(
        "DELETE FROM team_event_pauses WHERE project=?1 AND (agent=0 OR agent IN (SELECT agent FROM team_events WHERE wake=?2))",
        params![wake.project.as_str(), id],
    )
    .map_err(error)?;
    Ok(id)
}

/// Delivers due events through the engine-driven master: once nothing else waits for the
/// master, one engine message with the wake's lines starts its next turn. With no live
/// master, only a queued question starts one; other events wait for the master to run.
/// Takes the connection, the project, whether its master runs and the time.
/// Returns whether a wake was sent.
pub fn deliver_by_message(
    connection: &Connection,
    project: &ProjectId,
    master_live: bool,
    now: i64,
) -> Result<bool, EngineError> {
    let tx = immediate(connection)?;
    let (waiting, asking): (i64, bool) = tx
        .query_row(
            "SELECT (SELECT COUNT(*) FROM team_messages WHERE project=?1 AND state IN ('pending','sent')),
                    EXISTS(SELECT 1 FROM team_events WHERE project=?1 AND state='queued' AND kind='question')",
            [project.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(error)?;
    if waiting > 0 || !(master_live || asking) {
        return Ok(false);
    }
    let Some(wake) = due(&tx, project, now)? else {
        tx.commit().map_err(error)?;
        return Ok(false);
    };
    let sent = append_message(
        &tx,
        project,
        MessageAuthor::Engine,
        &wake.text,
        MessageState::Pending,
        now,
    )?
    .is_ok();
    if sent {
        woke(&tx, &wake, now)?;
    }
    tx.commit().map_err(error)?;
    Ok(sent)
}

#[cfg(all(test, unix))]
mod tests;
