pub use super::agent_messages::Said;
use super::{
    AgentRole, AgentStatus, MessageAuthor, MessageState, append_message, bound_text, error,
    make_room,
};
use crate::activity::ActivityKind;
use crate::engine::EngineError;
use crate::protocol::ProjectId;
use crate::storage::versioned;
use crate::terminal::{AgentId, Host, HostError, type_prompt};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use std::collections::btree_map::Entry;
use std::collections::{BTreeMap, BTreeSet};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::time::{Duration, Instant};

pub const EVENT_TEXT_BYTES_MAX: usize = 1000;
pub const EVENTS_PER_PROJECT_MAX: i64 = 2000;
pub const EVENTS_QUEUED_MAX: i64 = 200;
pub const EVENTS_QUEUED_PER_AGENT_MAX: i64 = 50;
pub const EVENTS_QUEUED_HARD_MAX: i64 = 2 * EVENTS_QUEUED_MAX;
pub const COLD_WAKES_PER_HOUR_MAX: u32 = 6;
pub const EVENTS_PER_WAKE_MAX: usize = 20;
pub const WAKE_BYTES_MAX: usize = super::MESSAGE_BYTES_MAX;
pub const HOURLY_CAP_MAX: u32 = 500;
pub const HOURLY_CAP_DEFAULT: u32 = 30;
pub const SEND_BYTES_MAX: usize = crate::terminal::PROMPT_BYTES_MAX;
pub const ESCALATION_BYTES_MAX: usize = 2000;
pub const NOTIFICATIONS_PER_PROJECT_MAX: i64 = 200;
pub const STATUS_EVENTS_MAX: i64 = 50;
const LEFT_OUT_LINE_BYTES_MAX: usize = 64;
pub const STATUS_AGENTS_MAX: i64 = super::STATUS_AGENTS_MAX;
pub const VIEW_NOTIFICATIONS_MAX: i64 = 20;
pub const APP_NOTIFICATIONS_MAX: i64 = 50;
pub const UNSEEN_PROJECTS_MAX: i64 = 200;
/// The projects the working list names. At this bound it and a full unseen list still fit one
/// reply at the longest legal project id, so a decoration cannot fail the whole read.
pub const WORKING_PROJECTS_MAX: i64 = 100;
pub const HOUR_MS: i64 = 3_600_000;
pub const SCAN_EVERY: Duration = Duration::from_secs(2);
pub(crate) const OWNER_RESTARTED: &str = "The owner restarted it in its own worktree. It is idle and takes the next task for its harness.";
pub(crate) const OWNER_WOKE: &str = "The owner woke it again in its own worktree. It is idle and takes the next task for its harness.";
pub(crate) const OWNER_STOPPED_AGENT: &str = "The owner stopped it and its teammates. Its task waits for it until the owner starts it again; do not hand that work to another agent.";
pub(crate) const MASTER_STOPPED_AGENT: &str = "The master stopped it and its teammates. Its unfinished tasks failed. The master or the owner can wake it.";
pub(crate) const LEAD_STOPPED_AGENT: &str = "Its lead stopped it and its teammates. Its unfinished tasks failed. Its lead, the master or the owner can wake it.";
pub(crate) const INSTRUCTIONS_RESTARTED: &str = "It restarted in its own worktree and conversation to load the owner's changed instructions. It is idle and takes the next task for its harness.";
const OWNER_NOTICES: [&str; 6] = [
    OWNER_RESTARTED,
    OWNER_WOKE,
    OWNER_STOPPED_AGENT,
    MASTER_STOPPED_AGENT,
    LEAD_STOPPED_AGENT,
    INSTRUCTIONS_RESTARTED,
];
const SCREEN_ROWS_SCANNED: usize = 40;
const QUESTION_ROWS_ABOVE: usize = 12;
const OPTIONS_MAX: usize = 9;
const TASK_EXCERPT_BYTES_MAX: usize = 200;
const EVENT_HISTORY: &str = "state<>'queued'";
const NOTIFICATION_HISTORY: &str = "1=1";
const COLD_PAUSE: i64 = -1;
const MARKERS: [char; 5] = ['❯', '›', '>', '▶', '→'];
const BORDERS: [char; 4] = ['│', '|', '┃', '║'];
const PROMPT_MARKS: [char; 3] = ['❯', '›', '>'];
const COMPOSER_MARK: char = '›';
const RULES: [char; 6] = ['─', '━', '╭', '╮', '╰', '╯'];
const RULE_CHARS_MIN: usize = 3;
const PROMPT_ROWS_BELOW_MAX: usize = 6;
const COMPOSER_ROWS_BELOW_MAX: usize = 2;

const _: () = assert!(EVENTS_QUEUED_MAX < EVENTS_PER_PROJECT_MAX);
const _: () = assert!(EVENTS_QUEUED_PER_AGENT_MAX < EVENTS_QUEUED_MAX);
const _: () = assert!(EVENTS_QUEUED_HARD_MAX < EVENTS_PER_PROJECT_MAX);
const _: () = assert!(OWNER_RESTARTED.len() <= EVENT_TEXT_BYTES_MAX);
const _: () = assert!(OWNER_WOKE.len() <= EVENT_TEXT_BYTES_MAX);
const _: () = assert!(OWNER_STOPPED_AGENT.len() <= EVENT_TEXT_BYTES_MAX);
const _: () = assert!(MASTER_STOPPED_AGENT.len() <= EVENT_TEXT_BYTES_MAX);
const _: () = assert!(LEAD_STOPPED_AGENT.len() <= EVENT_TEXT_BYTES_MAX);
const _: () = assert!(INSTRUCTIONS_RESTARTED.len() <= EVENT_TEXT_BYTES_MAX);
const _: () = assert!(COLD_WAKES_PER_HOUR_MAX <= HOURLY_CAP_DEFAULT);
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

    pub(super) fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.name() == value)
    }

    fn replaceable(self) -> bool {
        matches!(self, Self::Question | Self::Progress)
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
/// muted by a wake rule that has its kind off, carried by the master's own results
/// message because the master delegated that task, or dropped from a full queue for newer
/// events.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventState {
    Queued,
    Delivered,
    Muted,
    Carried,
    Dropped,
}

impl EventState {
    const ALL: [Self; 5] = [
        Self::Queued,
        Self::Delivered,
        Self::Muted,
        Self::Carried,
        Self::Dropped,
    ];

    fn name(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Delivered => "delivered",
            Self::Muted => "muted",
            Self::Carried => "carried",
            Self::Dropped => "dropped",
        }
    }

    pub(super) fn parse(value: &str) -> Option<Self> {
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

/// Why the owner gets an app notification: the master escalated an owner-only decision,
/// auto-wake stopped at its hourly cap, the event queue filled, or the project's token
/// budget stopped its team; or an agent asked the owner an urgent (High) or a Normal
/// question, an `ask` question among them that waits in the Questions view, or waits on
/// the owner in its own terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotificationKind {
    Escalation,
    WakePaused,
    QueueFull,
    BudgetStopped,
    UrgentQuestion,
    Question,
    WaitingOnYou,
}

impl NotificationKind {
    const ALL: [Self; 7] = [
        Self::Escalation,
        Self::WakePaused,
        Self::QueueFull,
        Self::BudgetStopped,
        Self::UrgentQuestion,
        Self::Question,
        Self::WaitingOnYou,
    ];

    fn name(self) -> &'static str {
        match self {
            Self::Escalation => "escalation",
            Self::WakePaused => "wake_paused",
            Self::QueueFull => "queue_full",
            Self::BudgetStopped => "budget_stopped",
            Self::UrgentQuestion => "urgent_question",
            Self::Question => "question",
            Self::WaitingOnYou => "waiting_on_you",
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

/// What the master's status tool shows: the project's live sub-agents, the finished ones it
/// can wake, the newest events and whether auto-wake is paused at its cap.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventsStatus {
    pub project_id: ProjectId,
    pub agents: Vec<AgentLine>,
    #[serde(default)]
    pub finished: Vec<String>,
    #[serde(default)]
    pub retired: Vec<String>,
    pub events: Vec<TeamEvent>,
    pub paused: bool,
}

impl EventsStatus {
    /// Writes the status as plain lines for the master's tool reply, within
    /// `REPLY_TEXT_MAX`. Takes the status. Returns one line per running agent, the finished
    /// agents it can wake, newest first, then the newest events that fit, oldest first,
    /// after one line saying how many older events were left out to fit (WKC-3).
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
                text.push_str(&format!(", waiting on: {}", agent_line(question)));
            }
            text.push('\n');
        }
        if !self.finished.is_empty() {
            text.push_str(&format!(
                "Finished sub-agents you can wake with {}(to, text), newest first:\n",
                super::tool::UPDATE_WORK_TOOL
            ));
            for line in &self.finished {
                text.push_str(line);
                text.push('\n');
            }
        }
        if !self.retired.is_empty() {
            text.push_str(&format!(
                "Retired sub-agents, whose worktrees are gone, so they cannot be woken; hand their work out with {}() instead: {}.\n",
                super::tool::SPAWN_TOOL,
                self.retired.join(", ")
            ));
        }
        if self.paused {
            text.push_str("Auto-wake is paused at its hourly cap.\n");
        }
        let budget =
            super::tool::REPLY_TEXT_MAX.saturating_sub(text.len() + LEFT_OUT_LINE_BYTES_MAX);
        let mut used = 0;
        let mut kept = Vec::new();
        for event in self.events.iter().rev() {
            let line = line(event);
            if used + line.len() + 1 > budget {
                break;
            }
            used += line.len() + 1;
            kept.push(line);
        }
        let left = self.events.len() - kept.len();
        if left > 0 {
            text.push_str(&format!(
                "{left} older events left out to fit this reply.\n"
            ));
        }
        for line in kept.iter().rev() {
            text.push_str(line);
            text.push('\n');
        }
        assert!(kept.is_empty() || text.len() <= super::tool::REPLY_TEXT_MAX);
        text
    }
}

/// One agent's own wake rule, set at the agent level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentRule {
    pub agent: i64,
    pub rule: WakeRule,
    #[serde(default)]
    pub version: u64,
}

/// One agent level the owner cleared: it has no rule of its own any more, yet its version
/// moved on, and a change of that level sends this version back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClearedRule {
    pub agent: i64,
    pub version: u64,
}

/// The wake rules a project's settings show: what is stored at each level with the version a
/// change sends back, and the rule in force for the project with the level it came from. A
/// level never set is at version 0; an agent level cleared since is listed in `cleared`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct WakeView {
    pub app: Option<WakeRule>,
    #[serde(default)]
    pub app_version: u64,
    pub project: Option<WakeRule>,
    #[serde(default)]
    pub project_version: u64,
    pub agents: Vec<AgentRule>,
    #[serde(default)]
    pub cleared: Vec<ClearedRule>,
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
/// project rule, the newest notifications of every project, how many are unseen and how many
/// of them each project has, and how many agents each project has at work.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct AppEvents {
    pub rule: Option<WakeRule>,
    #[serde(default)]
    pub rule_version: u64,
    pub effective: WakeRule,
    pub notifications: Vec<Notification>,
    pub unseen: i64,
    #[serde(default)]
    pub projects: Vec<ProjectUnseen>,
    #[serde(default)]
    pub working: Vec<ProjectWorking>,
}

/// How many agents one project has at work, for its sidebar dot: team agents working plus
/// research jobs running, the sum its project card shows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectWorking {
    pub project_id: ProjectId,
    pub agents: i64,
}

/// How many of one project's owner notifications are unseen, for its sidebar count, and how
/// many of those are questions-log notifications, which the app counts as pending
/// questions; a work question's alert is not one of them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectUnseen {
    pub project_id: ProjectId,
    pub unseen: i64,
    #[serde(default)]
    pub questions: i64,
}

/// The events typed into the master in one wake, oldest first, their text and whether one
/// of them is a question.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Wake {
    pub project: ProjectId,
    pub events: Vec<i64>,
    pub text: String,
    pub asks: bool,
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
    screens: BTreeMap<i64, u64>,
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

    /// Reads each watched agent's screen once: a new question on a screen unchanged since the
    /// last scan, so the harness stopped for input, is recorded as a question event and marks
    /// the agent waiting; a question that left the screen clears the mark. Agents no
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
        self.screens.retain(|agent, _| current.contains(agent));
        for watched in agents {
            let Ok(name) = AgentId::team(watched.agent) else {
                continue;
            };
            let Ok(screen) = host.screen(&name) else {
                continue;
            };
            let mut digest = DefaultHasher::new();
            screen.hash(&mut digest);
            let digest = digest.finish();
            let idle = self.screens.insert(watched.agent, digest) == Some(digest);
            match question(&screen) {
                Some(text) if idle && self.seen.get(&watched.agent) != Some(&text) => {
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
        assert!(self.screens.keys().all(|agent| current.contains(agent)));
        Ok(())
    }
}

/// Creates the team event tables when the engine database opens, and adds the cold start
/// column to a wake table made before it. Takes the engine connection. Returns any SQLite
/// failure.
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
                at_ms INTEGER NOT NULL,
                cold INTEGER NOT NULL DEFAULT 0
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
        .map_err(error)?;
    let cold: bool = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_info('team_event_wakes') WHERE name='cold')",
            [],
            |row| row.get(0),
        )
        .map_err(error)?;
    if !cold {
        connection
            .execute_batch(
                "ALTER TABLE team_event_wakes ADD COLUMN cold INTEGER NOT NULL DEFAULT 0;",
            )
            .map_err(error)?;
    }
    Ok(())
}

pub(super) fn immediate(connection: &Connection) -> Result<Transaction<'_>, EngineError> {
    Transaction::new_unchecked(connection, TransactionBehavior::Immediate).map_err(error)
}

/// Joins text onto one line of plain words without control characters, bounded to `max`
/// bytes. Takes the text and the bound. Returns the line.
pub(super) fn one_line(text: &str, max: usize) -> String {
    let joined = text
        .split_whitespace()
        .map(|word| word.chars().filter(|c| !c.is_control()).collect::<String>())
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    bound_text(&joined, max)
}

/// Names a sub-agent the way events, the master and every client show it: "Ada
/// (@onboarding-fix)", "Ada" before its first job, or "Sub-agent 7" without a kept name.
/// Takes the connection and the team agent id. Returns the label.
pub fn label(connection: &Connection, agent: i64) -> Result<String, EngineError> {
    let name = super::names::name(connection, agent)?;
    Ok(super::names::label(AgentRole::Sub, agent, name.as_ref()))
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
    if state == EventState::Queued {
        if kind.replaceable() {
            let same: Option<i64> = tx
                .query_row(
                    "SELECT id FROM team_events WHERE project=?1 AND agent=?2 AND kind=?3 AND state='queued'
                     ORDER BY id DESC LIMIT 1",
                    params![project.as_str(), agent, kind.name()],
                    |row| row.get(0),
                )
                .optional()
                .map_err(error)?;
            if let Some(id) = same {
                tx.execute(
                    "UPDATE team_events SET text=?2, task=?3 WHERE id=?1",
                    params![id, text, task],
                )
                .map_err(error)?;
                return Ok(Some(id));
            }
        }
        if !queue_room(tx, project, agent, kind, now)? {
            return Ok(None);
        }
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
    match kind {
        EventKind::Finished | EventKind::Failed => {
            let failed = kind == EventKind::Failed;
            crate::activity::note_task_end(tx, project, agent, task, failed, now);
        }
        EventKind::ChangeSetReady => {
            let shown = ActivityKind::ChangesReady;
            crate::activity::note_agent(tx, project, agent, shown, &text, now);
        }
        EventKind::Question | EventKind::Progress => {}
    }
    Ok(Some(id))
}

/// Makes room in a full project queue for one more event of an agent: the oldest queued
/// progress event, then question, is dropped, the agent's own first and only its own once it
/// holds `EVENTS_QUEUED_PER_AGENT_MAX`; progress never displaces a question. Finished, failed
/// and change set events are never dropped and may pass the bound up to
/// `EVENTS_QUEUED_HARD_MAX` while their agent is under its share. A refusal tells the owner
/// once while that notice is unseen. Takes the transaction, project, agent, kind and time.
/// Returns whether the event may queue.
fn queue_room(
    tx: &Transaction<'_>,
    project: &ProjectId,
    agent: i64,
    kind: EventKind,
    now: i64,
) -> Result<bool, EngineError> {
    let (all, mine): (i64, i64) = tx
        .query_row(
            "SELECT COUNT(*), COALESCE(SUM(agent=?2),0) FROM team_events WHERE project=?1 AND state='queued'",
            params![project.as_str(), agent],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(error)?;
    assert!(mine <= all);
    if all < EVENTS_QUEUED_MAX {
        return Ok(true);
    }
    let over_share = mine >= EVENTS_QUEUED_PER_AGENT_MAX;
    let victim: Option<i64> = tx
        .query_row(
            "SELECT id FROM team_events WHERE project=?1 AND state='queued'
             AND (kind='progress' OR (kind='question' AND ?4=0)) AND (?3=0 OR agent=?2)
             ORDER BY agent=?2 DESC, kind='question', id LIMIT 1",
            params![
                project.as_str(),
                agent,
                over_share,
                kind == EventKind::Progress
            ],
            |row| row.get(0),
        )
        .optional()
        .map_err(error)?;
    if let Some(victim) = victim {
        let changed = tx
            .execute(
                "UPDATE team_events SET state='dropped' WHERE id=?1 AND state='queued'",
                [victim],
            )
            .map_err(error)?;
        assert_eq!(changed, 1);
        return Ok(true);
    }
    if !kind.replaceable() && !over_share && all < EVENTS_QUEUED_HARD_MAX {
        return Ok(true);
    }
    let told: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM team_notifications WHERE project=?1 AND kind='queue_full' AND seen_ms IS NULL)",
            [project.as_str()],
            |row| row.get(0),
        )
        .map_err(error)?;
    if !told {
        let text = format!(
            "{} has a {} event the master did not get: this project's event queue is full. The Agents tab shows what waits.",
            label(tx, agent)?,
            kind.name().replace('_', " ")
        );
        notify(tx, project, NotificationKind::QueueFull, &text, now)?;
    }
    Ok(false)
}

/// Records how a sub-agent's task ended once its turn finished: done becomes a finished
/// event, whatever its last line asks; failed becomes a failed event. A task
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
        Option<i64>,
    );
    let row: Option<Row> = connection
        .query_row(
            "SELECT project,agent,source,status,result,error,parent FROM team_tasks WHERE id=?1",
            [task],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                ))
            },
        )
        .optional()
        .map_err(error)?;
    let Some((project, Some(agent), source, status, result, problem, lead)) = row else {
        return Ok(None);
    };
    clear_waiting(connection, agent)?;
    let Ok(project) = ProjectId::new(project) else {
        return Ok(None);
    };
    let (kind, text) = match status.as_str() {
        "done" => {
            let result = result.unwrap_or_default();
            let text = if result.trim().is_empty() {
                "Done, with no reply text.".to_owned()
            } else {
                result
            };
            (EventKind::Finished, text)
        }
        "failed" => (
            EventKind::Failed,
            problem.unwrap_or_else(|| "The task failed.".to_owned()),
        ),
        _ => return Ok(None),
    };
    let state = if source == "master" || lead.is_some() {
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

const SUB_AGENT_PREFIX: &str = "Sub-agent ";

/// Finds the project's live sub-agent a master names: by id as `Sub-agent 7`,
/// `<harness>-7`, `team-7` or `7` first, so no handle can stand for another agent's id and
/// a name in a numbered form is read only as that number (RV-DS-62), else its persona or
/// `@handle` as the names module resolves them. Takes the connection, the project and the
/// name. Returns the team agent id, or none for an unknown name, another project's agent
/// or a master.
pub fn resolve(
    connection: &Connection,
    project: &ProjectId,
    name: &str,
) -> Result<Option<i64>, EngineError> {
    let name = name.trim();
    if let Some(agent) = by_id_form(connection, project, name.trim_start_matches('@'))? {
        return Ok(Some(agent));
    }
    let Some(agent) = super::names::resolve(connection, project, name)? else {
        return Ok(None);
    };
    let sub: bool = connection
        .query_row(
            "SELECT role='sub' FROM team_agents WHERE id=?1 AND project=?2",
            params![agent, project.as_str()],
            |row| row.get(0),
        )
        .map_err(error)?;
    Ok(sub.then_some(agent))
}

/// Finds the project's sub-agent an id form names: `Sub-agent 7`, `team-7` or `7` for any
/// harness, `<harness>-7` only for an agent running that harness, in any letter case.
/// Takes the connection, the project and the name with no `@`. Returns the team agent id,
/// or none for any other name.
fn by_id_form(
    connection: &Connection,
    project: &ProjectId,
    name: &str,
) -> Result<Option<i64>, EngineError> {
    let numbered = name
        .get(..SUB_AGENT_PREFIX.len())
        .filter(|head| head.eq_ignore_ascii_case(SUB_AGENT_PREFIX))
        .map(|_| &name[SUB_AGENT_PREFIX.len()..]);
    let form = match numbered {
        Some(digits) => super::names::id_form(digits)
            .filter(|(prefix, _)| prefix.is_none())
            .map(|(_, agent)| (Some("team"), agent)),
        None => super::names::id_form(name),
    };
    let Some((prefix, agent)) = form else {
        return Ok(None);
    };
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
    let named = prefix.is_none_or(|prefix| {
        prefix.eq_ignore_ascii_case(&harness) || prefix.eq_ignore_ascii_case("team")
    });
    Ok((named && AgentRole::parse(&role) == Some(AgentRole::Sub)).then_some(agent))
}

/// Types a master's message or answer into one of its project's sub-agents' terminal as one
/// submission, the way an owner prompt is typed: control keys are dropped, so the text
/// cannot press keys. Clears the agent's waiting mark and keeps what was typed for the
/// agent's Messages timeline. Takes the connection, the terminal host, the project, the
/// agent's name as `resolve` reads it, whether it is a message or an answer and the text.
/// Returns the agent id, or a plain refusal code for blank or long text, an unknown or
/// stopped agent or an unreachable terminal.
pub fn send_to_agent(
    connection: &Connection,
    host: &dyn Host,
    project: &ProjectId,
    agent: &str,
    said: Said,
    text: &str,
) -> Result<Result<i64, &'static str>, EngineError> {
    let found = resolve(connection, project, agent)?;
    send_to(connection, host, found, said, text)
}

/// Types a master's message or answer into the sub-agent [`resolve`] found, as
/// [`send_to_agent`] does, so a caller that already resolved the name passes its id rather
/// than a name to read again. Only text the terminal took is kept, with the waiting mark
/// cleared in the same transaction. Takes the connection, the terminal host, the agent or
/// None when no agent was found, whether it is a message, an answer or an engine notice,
/// and the text.
/// Returns the agent id, or the same plain refusal codes.
pub fn send_to(
    connection: &Connection,
    host: &dyn Host,
    found: Option<i64>,
    said: Said,
    text: &str,
) -> Result<Result<i64, &'static str>, EngineError> {
    assert!(
        said != Said::Prompt,
        "the master sends messages and answers, the engine notices"
    );
    if text.trim().is_empty() || text.len() > SEND_BYTES_MAX {
        return Ok(Err("team_message_invalid"));
    }
    let Some(id) = found else {
        return Ok(Err("team_agent_not_found"));
    };
    let (status, project): (String, String) = connection
        .query_row(
            "SELECT status,project FROM team_agents WHERE id=?1",
            [id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
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
    let project = ProjectId::new(project).map_err(|_| error("team_agent_project_invalid"))?;
    let tx = immediate(connection)?;
    super::agent_messages::record(&tx, &project, id, said, text, super::now_ms())?;
    clear_waiting(&tx, id)?;
    tx.commit().map_err(error)?;
    Ok(Ok(id))
}

/// Types text into the terminal of exactly one sub-agent of a project, chosen by its team
/// agent id, as `send_to_agent` types it, and keeps nothing for its Messages timeline: the
/// caller keeps the owner's words there itself. Takes the connection, the terminal host, the
/// project, the team agent id and the text. Returns a plain refusal code for blank or long
/// text, an id that is no sub-agent of the project, a stopped agent or an unreachable terminal.
pub fn send_to_id(
    connection: &Connection,
    host: &dyn Host,
    project: &ProjectId,
    id: i64,
    text: &str,
) -> Result<Result<(), &'static str>, EngineError> {
    if let Err(code) = type_to_id(connection, host, project, id, text)? {
        return Ok(Err(code));
    }
    clear_waiting(connection, id)?;
    Ok(Ok(()))
}

/// Types an engine notice into exactly one sub-agent of a project, chosen by its team agent
/// id, as `send_to_id` types it, and keeps it in the agent's Messages timeline as the
/// engine's, with the waiting mark cleared in the same transaction, as `send_to` keeps what
/// the master says. Takes the connection, the terminal host, the project, the team agent id
/// and the text. Returns the same plain refusal codes as `send_to_id`.
pub fn send_notice_to_id(
    connection: &Connection,
    host: &dyn Host,
    project: &ProjectId,
    id: i64,
    text: &str,
) -> Result<Result<(), &'static str>, EngineError> {
    if let Err(code) = type_to_id(connection, host, project, id, text)? {
        return Ok(Err(code));
    }
    assert!(id > 0, "only a sub-agent row takes a notice");
    let tx = immediate(connection)?;
    super::agent_messages::record(&tx, project, id, Said::Notice, text, super::now_ms())?;
    clear_waiting(&tx, id)?;
    tx.commit().map_err(error)?;
    Ok(Ok(()))
}

/// Admits and types text into one sub-agent of a project by its team agent id, writing
/// nothing to the store. Takes the connection, the terminal host, the project, the team
/// agent id and the text. Returns Ok once its terminal took the text, or the plain refusal
/// code of `send_to_id`.
fn type_to_id(
    connection: &Connection,
    host: &dyn Host,
    project: &ProjectId,
    id: i64,
    text: &str,
) -> Result<Result<(), &'static str>, EngineError> {
    if text.trim().is_empty() || text.len() > SEND_BYTES_MAX {
        return Ok(Err("team_message_invalid"));
    }
    let status: Option<String> = connection
        .query_row(
            "SELECT status FROM team_agents WHERE id=?1 AND project=?2 AND role='sub'",
            params![id, project.as_str()],
            |row| row.get(0),
        )
        .optional()
        .map_err(error)?;
    let Some(status) = status else {
        return Ok(Err("team_agent_not_found"));
    };
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
    Ok(Ok(()))
}

/// The most queued worker events one tick forwards to their leads.
pub const FORWARDED_PER_TICK_MAX: i64 = 64;

/// Sends the queued events of a project's workers to each worker's own lead instead of
/// the master, which hears only its leads' events (DEC-104 K2b): one coordination note per
/// lead from the engine, holding the event lines up to the message body bound, under each
/// worker's wake rule, so a muted kind stays muted. The engine's notices of what the owner
/// did to a worker go in a note of their own under `NOTICES_KEY` whatever the rule: it
/// wakes no one, so the lead reads them at its next real turn. Events the layer refuses are
/// dropped, never sent elsewhere. Takes the connection, the project and the time. Returns
/// any failure.
pub fn forward_to_parents(
    connection: &Connection,
    project: &ProjectId,
    now: i64,
) -> Result<(), EngineError> {
    let queued: Vec<(i64, i64, i64)> = {
        let mut statement = connection
            .prepare(
                "SELECT e.id,e.agent,a.parent FROM team_events e JOIN team_agents a ON a.id=e.agent
                 WHERE e.project=?1 AND e.state='queued' AND a.parent IS NOT NULL ORDER BY e.id LIMIT ?2",
            )
            .map_err(error)?;
        statement
            .query_map(params![project.as_str(), FORWARDED_PER_TICK_MAX], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })
            .map_err(error)?
            .collect::<Result<_, _>>()
            .map_err(error)?
    };
    let mut full = connection
        .prepare(&format!(
            "SELECT {EVENT_COLUMNS} FROM team_events WHERE id=?1"
        ))
        .map_err(error)?;
    let mut by_lead: BTreeMap<(i64, bool), Vec<TeamEvent>> = BTreeMap::new();
    for (id, agent, lead) in queued {
        let event = full
            .query_row([id], |row| event_row(connection, row))
            .map_err(error)?;
        let notice = owner_notice(&event);
        if notice
            || rule(connection, Some(project), Some(agent))?
                .0
                .wakes(event.kind)
        {
            by_lead.entry((lead, notice)).or_default().push(event);
        } else {
            connection
                .execute(
                    "UPDATE team_events SET state='muted' WHERE id=?1 AND state='queued'",
                    [id],
                )
                .map_err(error)?;
        }
    }
    for ((lead, notice), events) in by_lead {
        assert!(events.iter().all(|event| owner_notice(event) == notice));
        let mut text = String::new();
        let mut carried = Vec::new();
        for event in &events {
            let next = line(event);
            if !text.is_empty() && text.len() + 1 + next.len() > crate::coord::BODY_BYTES_MAX {
                break;
            }
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str(&next);
            carried.push(event.id);
        }
        assert!(!carried.is_empty() && text.len() <= crate::coord::BODY_BYTES_MAX);
        let tx = immediate(connection)?;
        let sent = crate::coord::send(
            &tx,
            crate::coord::NewMessage {
                project: project.clone(),
                from: crate::coord::Party::Engine,
                to: crate::coord::Party::Agent(lead),
                kind: crate::coord::MessageKind::Note,
                question: None,
                work: None,
                reply_to: None,
                body: text,
                idempotency_key: Some(format!(
                    "{}{}",
                    if notice {
                        crate::coord::delivery::NOTICES_KEY
                    } else {
                        crate::coord::delivery::EVENTS_KEY
                    },
                    carried[0]
                )),
            },
            now,
        );
        let state = match sent {
            Ok(_) => EventState::Delivered,
            Err(problem) if problem.0.starts_with("coord_state_unavailable") => {
                return Err(problem);
            }
            Err(_) => EventState::Dropped,
        };
        for id in &carried {
            tx.execute(
                "UPDATE team_events SET state=?2, delivered_ms=?3 WHERE id=?1 AND state='queued'",
                params![id, state.name(), now],
            )
            .map_err(error)?;
        }
        tx.commit().map_err(error)?;
    }
    Ok(())
}

/// Tells whether an event is the engine's own notice that the owner woke, restarted or
/// stopped the agent, or restarted it for the owner's changed instructions (run 5 task 73:
/// a lead woken for one spent a turn on nothing). Takes the event. Returns the answer.
fn owner_notice(event: &TeamEvent) -> bool {
    event.kind == EventKind::Progress && OWNER_NOTICES.contains(&event.text.as_str())
}

/// The handlers the live master's `message`, `answer` and `status` tools call, set on the
/// team runtime with `TeamRuntime::with_events`.
pub const HANDLERS: super::master::TeamEvents = super::master::TeamEvents {
    send_to_agent: tool_message,
    answer: tool_answer,
    status: tool_status,
};

fn tool_message(
    connection: &Connection,
    host: &dyn Host,
    project: &ProjectId,
    agent: &str,
    text: &str,
) -> Result<String, String> {
    tool_send(connection, host, project, agent, Said::Message, text)
}

fn tool_answer(
    connection: &Connection,
    host: &dyn Host,
    project: &ProjectId,
    agent: &str,
    text: &str,
) -> Result<String, String> {
    tool_send(connection, host, project, agent, Said::Answer, text)
}

/// Types a live master's message or answer into one of its sub-agents for its team tools.
/// Takes the connection, terminal host, project, the agent's name, whether it is a message
/// or an answer and the text. Returns the sentence the master reads, or the plain reason
/// nothing was typed.
fn tool_send(
    connection: &Connection,
    host: &dyn Host,
    project: &ProjectId,
    agent: &str,
    said: Said,
    text: &str,
) -> Result<String, String> {
    let refused = |reason: &str| Err(format!("Not sent: {reason}"));
    match send_to_agent(connection, host, project, agent, said, text) {
        Ok(Ok(id)) => Ok(format!(
            "Typed into {}.",
            label(connection, id).map_err(|_| "Not sent: the team database is unavailable.")?
        )),
        Ok(Err("team_message_invalid")) => refused("the text is empty or too long."),
        Ok(Err("team_agent_not_found")) => {
            refused("no live sub-agent of this project has that name.")
        }
        Ok(Err("team_agent_not_running")) => refused("that sub-agent's terminal is not running."),
        Ok(Err(_)) => refused("that sub-agent's terminal did not take the text."),
        Err(_) => refused("the team database is unavailable."),
    }
}

/// Describes the project's team for a live master's status tool. Takes the connection and
/// the project. Returns the status lines, or the plain reason it could not be read.
fn tool_status(connection: &Connection, project: &ProjectId) -> Result<String, String> {
    status(connection, project)
        .map(|status| status.text())
        .map_err(|_| "The team status is unavailable right now.".to_owned())
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

/// Adds one owner notification inside the caller's transaction, dropping the project's
/// oldest ones beyond `NOTIFICATIONS_PER_PROJECT_MAX`. Inputs: the transaction, the
/// project, the kind, the bounded text and the time. Output: the notification id.
pub(crate) fn notify(
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
    read_notifications(
        connection,
        "SELECT id,project,kind,text,created_ms,seen_ms FROM team_notifications
         WHERE ?1 IS NULL OR project=?1 ORDER BY id DESC LIMIT ?2",
        params![project.map(ProjectId::as_str), limit.max(0)],
    )
}

/// Reads the app's notifications, newest first. Every project gets its newest one before
/// any project gets a second, so a busy project never pushes another's out of the window.
/// Takes the connection. Returns at most `APP_NOTIFICATIONS_MAX`, at most
/// `VIEW_NOTIFICATIONS_MAX` of one project.
fn app_notifications(connection: &Connection) -> Result<Vec<Notification>, EngineError> {
    let read = read_notifications(
        connection,
        "SELECT id,project,kind,text,created_ms,seen_ms FROM (
             SELECT id,project,kind,text,created_ms,seen_ms,place FROM (
                 SELECT id,project,kind,text,created_ms,seen_ms,
                        ROW_NUMBER() OVER (PARTITION BY project ORDER BY id DESC) AS place
                 FROM team_notifications)
             WHERE place<=?1 ORDER BY place, id DESC LIMIT ?2)
         ORDER BY id DESC",
        params![VIEW_NOTIFICATIONS_MAX, APP_NOTIFICATIONS_MAX],
    )?;
    assert!(read.len() as i64 <= APP_NOTIFICATIONS_MAX);
    assert!(read.windows(2).all(|pair| pair[0].id > pair[1].id));
    Ok(read)
}

/// Counts the agents at work in each project that has any, in one read for all projects, so
/// the sidebar never asks project by project: team agents working plus research jobs running,
/// the sum each project card shows. Takes the connection. Returns at most
/// `WORKING_PROJECTS_MAX` projects by id, each with a count above zero.
fn working_by_project(connection: &Connection) -> Result<Vec<ProjectWorking>, EngineError> {
    let mut statement = connection
        .prepare(
            "SELECT project, SUM(agents) FROM (
                 SELECT project, COUNT(*) AS agents FROM team_agents
                 WHERE status = 'working' GROUP BY project
                 UNION ALL
                 SELECT project, COUNT(*) FROM research_jobs
                 WHERE status = 'running' GROUP BY project
             ) GROUP BY project ORDER BY project LIMIT ?1",
        )
        .map_err(error)?;
    let rows: Vec<(String, i64)> = statement
        .query_map([WORKING_PROJECTS_MAX], |row| Ok((row.get(0)?, row.get(1)?)))
        .map_err(error)?
        .collect::<Result<_, _>>()
        .map_err(error)?;
    let working: Vec<ProjectWorking> = rows
        .into_iter()
        .filter_map(|(project, agents)| {
            Some(ProjectWorking {
                project_id: ProjectId::new(project).ok()?,
                agents,
            })
        })
        .collect();
    assert!(working.len() as i64 <= WORKING_PROJECTS_MAX);
    assert!(working.iter().all(|item| item.agents > 0));
    Ok(working)
}

/// Counts the unseen notifications of each project that has any, and the questions-log
/// notifications among them, by project id, so a project's count never depends on what fits
/// in the app window. A work question's alert counts as a question only once its question is
/// linked into the questions log (F12), where it is counted as pending. Takes the connection.
/// Returns at most `UNSEEN_PROJECTS_MAX` projects, each with a count above zero.
fn unseen_by_project(connection: &Connection) -> Result<Vec<ProjectUnseen>, EngineError> {
    let mut statement = connection
        .prepare(
            "SELECT project, COUNT(*), SUM(
                 (kind = 'question' AND id NOT IN
                     (SELECT notification FROM team_owner_alerts WHERE notification IS NOT NULL))
                 OR (kind IN ('question','urgent_question') AND id IN
                     (SELECT a.notification FROM team_owner_alerts a
                      JOIN questions_log q ON q.coord_message = a.ref
                      WHERE a.source = 'question' AND a.notification IS NOT NULL)))
             FROM team_notifications
             WHERE seen_ms IS NULL GROUP BY project ORDER BY project LIMIT ?1",
        )
        .map_err(error)?;
    let rows: Vec<(String, i64, i64)> = statement
        .query_map([UNSEEN_PROJECTS_MAX], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })
        .map_err(error)?
        .collect::<Result<_, _>>()
        .map_err(error)?;
    let projects: Vec<ProjectUnseen> = rows
        .into_iter()
        .filter_map(|(project, unseen, questions)| {
            Some(ProjectUnseen {
                project_id: ProjectId::new(project).ok()?,
                unseen,
                questions,
            })
        })
        .collect();
    assert!(projects.len() as i64 <= UNSEEN_PROJECTS_MAX);
    assert!(
        projects
            .iter()
            .all(|item| item.unseen > 0 && (0..=item.unseen).contains(&item.questions))
    );
    Ok(projects)
}

fn read_notifications(
    connection: &Connection,
    sql: &str,
    values: impl rusqlite::Params,
) -> Result<Vec<Notification>, EngineError> {
    let mut statement = connection.prepare(sql).map_err(error)?;
    let rows: Vec<(i64, String, String, String, i64, Option<i64>)> = statement
        .query_map(values, |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
            ))
        })
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
        label: label(connection, agent).map_err(|_| invalid("agent"))?,
        task: row.get(2)?,
        kind: EventKind::parse(&kind).ok_or_else(|| invalid("kind"))?,
        text: row.get(4)?,
        state: EventState::parse(&state).ok_or_else(|| invalid("state"))?,
        created_ms: row.get(6)?,
        delivered_ms: row.get(7)?,
    })
}

/// The most finished sub-agents a master is told it can wake.
pub(crate) const FINISHED_LISTED_MAX: usize = 20;
const FINISHED_TASK_BYTES_MAX: usize = 120;

/// Lists a project's finished sub-agents a master can wake, newest first and at most
/// `FINISHED_LISTED_MAX`: each stopped or failed sub-agent whose worktree is still recorded,
/// with its name and Sub-agent number, harness, last task on one line and when it finished.
/// Takes the database, the project and now. Returns each agent's id with its line, or none
/// when no such agent exists.
pub(crate) fn finished_agents(
    connection: &Connection,
    project: &ProjectId,
    now: i64,
) -> Result<Vec<(i64, String)>, EngineError> {
    let rows: Vec<(i64, String, Option<i64>, Option<String>)> = {
        let mut statement = connection
            .prepare(
                "SELECT a.id,a.harness,a.stopped_ms,
                        (SELECT t.prompt FROM team_tasks t WHERE t.agent=a.id ORDER BY COALESCE(t.started_ms,t.created_ms) DESC, t.id DESC LIMIT 1)
                 FROM team_agents a
                 WHERE a.project=?1 AND a.role='sub' AND a.status IN ('stopped','failed')
                   AND EXISTS(SELECT 1 FROM team_worktrees w WHERE w.agent=a.id AND w.project=a.project)
                 ORDER BY COALESCE(a.stopped_ms,a.heartbeat_ms) DESC, a.id DESC LIMIT ?2",
            )
            .map_err(error)?;
        statement
            .query_map(
                params![project.as_str(), FINISHED_LISTED_MAX as i64],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .map_err(error)?
            .collect::<Result<_, _>>()
            .map_err(error)?
    };
    assert!(rows.len() <= FINISHED_LISTED_MAX);
    rows.into_iter()
        .map(
            |(id, harness, stopped, task)| -> Result<(i64, String), EngineError> {
                let task = task
                    .map(|task| one_line(&task, FINISHED_TASK_BYTES_MAX))
                    .filter(|task| !task.is_empty())
                    .unwrap_or_else(|| "no task yet".to_owned());
                let when =
                    stopped.map_or_else(|| "finished".to_owned(), |at| finished_ago(at, now));
                let label = label(connection, id)?;
                let number = AgentRole::Sub.agent_label(id);
                let named = if label == number || name_reaches_running(connection, project, id)? {
                    number
                } else {
                    format!("{label}, {number}")
                };
                Ok((
                    id,
                    format!(
                        "- {named} ({}): {task}; {when}",
                        super::names::harness_name(&harness)
                    ),
                ))
            },
        )
        .collect()
}

/// The most retired sub-agents a master is told about.
pub(crate) const RETIRED_LISTED_MAX: usize = 20;

/// Lists a project's retired sub-agents by label, newest first and at most
/// `RETIRED_LISTED_MAX`: each stopped or failed sub-agent with no worktree on record, as
/// cleanup leaves one it retired, so the master hands their work to a new agent instead
/// of waking them (F10). Takes the database and the project. Returns the labels.
pub(crate) fn retired_agents(
    connection: &Connection,
    project: &ProjectId,
) -> Result<Vec<String>, EngineError> {
    let ids: Vec<i64> = {
        let mut statement = connection
            .prepare(
                "SELECT a.id FROM team_agents a
                 WHERE a.project=?1 AND a.role='sub' AND a.status IN ('stopped','failed')
                   AND NOT EXISTS(SELECT 1 FROM team_worktrees w WHERE w.agent=a.id AND w.project=a.project)
                 ORDER BY COALESCE(a.stopped_ms,a.heartbeat_ms) DESC, a.id DESC LIMIT ?2",
            )
            .map_err(error)?;
        statement
            .query_map(
                params![project.as_str(), RETIRED_LISTED_MAX as i64],
                |row| row.get(0),
            )
            .map_err(error)?
            .collect::<Result<_, _>>()
            .map_err(error)?
    };
    assert!(ids.len() <= RETIRED_LISTED_MAX);
    ids.into_iter().map(|id| label(connection, id)).collect()
}

/// Says whether a sub-agent's persona or handle, ignoring case, is also the persona or handle
/// of another running sub-agent of the project, so a message sent by that name reaches the
/// running agent and the finished one is listed by its number only (DEC-111 Q19).
/// Takes the database, the project and the agent id. Returns true when such an agent runs.
pub(crate) fn name_reaches_running(
    connection: &Connection,
    project: &ProjectId,
    agent: i64,
) -> Result<bool, EngineError> {
    connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM team_names f
                 JOIN team_names r ON r.project=f.project AND r.agent<>f.agent
                 JOIN team_agents a ON a.id=r.agent AND a.project=r.project
                 WHERE f.project=?1 AND f.agent=?2
                   AND a.role='sub' AND a.status IN ('starting','idle','working')
                   AND (f.persona=r.persona COLLATE NOCASE OR f.persona=r.handle COLLATE NOCASE
                        OR f.handle=r.persona COLLATE NOCASE OR f.handle=r.handle COLLATE NOCASE))",
            params![project.as_str(), agent],
            |row| row.get(0),
        )
        .map_err(error)
}

/// Says how long ago an agent finished in whole units: "finished just now", "finished 4 min
/// ago", "finished 3 h ago" or "finished 2 days ago". Takes the stop time and now in epoch
/// milliseconds. Returns the phrase.
fn finished_ago(at: i64, now: i64) -> String {
    let minutes = now.saturating_sub(at).max(0) / 60_000;
    match minutes {
        0 => "finished just now".to_owned(),
        1..60 => format!("finished {minutes} min ago"),
        60..1440 => format!("finished {} h ago", minutes / 60),
        _ => format!("finished {} days ago", minutes / 1440),
    }
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
/// task and any question they wait on, the finished sub-agents it can wake as they are now,
/// the newest events and the auto-wake pause.
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
    let mut agents = Vec::with_capacity(rows.len());
    for (agent, harness, status, task, prompt) in rows {
        let Some(status) = AgentStatus::parse(&status) else {
            continue;
        };
        agents.push(AgentLine {
            agent,
            label: label(connection, agent)?,
            harness,
            status,
            status_label: status.label().to_owned(),
            task,
            task_prompt: prompt.map(|prompt| one_line(&prompt, TASK_EXCERPT_BYTES_MAX)),
            waiting: waiting.get(&agent).cloned(),
        });
    }
    let paused: bool = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM team_event_pauses WHERE project=?1 AND agent>=0)",
            [project.as_str()],
            |row| row.get(0),
        )
        .map_err(error)?;
    Ok(EventsStatus {
        project_id: project.clone(),
        agents,
        finished: finished_agents(connection, project, super::now_ms())?
            .into_iter()
            .map(|(_, line)| line)
            .collect(),
        retired: retired_agents(connection, project)?,
        events: events(connection, project, STATUS_EVENTS_MAX)?,
        paused,
    })
}

/// Writes one event as the master reads it: the agent's text goes through `agent_line`, so
/// a tag such as `[owner]` inside it can never pass for a speaker. Takes the event.
/// Returns the line.
fn line(event: &TeamEvent) -> String {
    let task = event
        .task
        .map_or_else(String::new, |task| format!(" [task {task}]"));
    format!(
        "[team] {} {}: {}{task}",
        event.label,
        event.kind.verb(),
        agent_line(&event.text)
    )
}

const OPEN_BRACKETS: [char; 16] = [
    '[', '\u{FF3B}', '\u{27E6}', '\u{3010}', '\u{3014}', '\u{3016}', '\u{3018}', '\u{301A}',
    '\u{2045}', '\u{2772}', '\u{298B}', '\u{298D}', '\u{298F}', '\u{27EC}', '\u{FE5D}', '\u{FE47}',
];
const CLOSE_BRACKETS: [char; 16] = [
    ']', '\u{FF3D}', '\u{27E7}', '\u{3011}', '\u{3015}', '\u{3017}', '\u{3019}', '\u{301B}',
    '\u{2046}', '\u{2773}', '\u{298C}', '\u{298E}', '\u{2990}', '\u{27ED}', '\u{FE5E}', '\u{FE48}',
];

const QUOTE: &str = "> ";
const BIDI_CONTROLS: [char; 12] = [
    '\u{061c}', '\u{200e}', '\u{200f}', '\u{202a}', '\u{202b}', '\u{202c}', '\u{202d}', '\u{202e}',
    '\u{2066}', '\u{2067}', '\u{2068}', '\u{2069}',
];

/// Writes text a sub-agent wrote as the master reads it on lines of its own: made plain by
/// `plain_agent_text`, and every line opened with `> `, so no line of it can start as the
/// owner, the engine or any other speaker however it is written. A line that already opens
/// with `> ` is kept, so quoting twice changes nothing. Task results and problems and the master's
/// history reach the master through it. Takes the text. Returns it.
pub(super) fn agent_text(text: &str) -> String {
    let plain = plain_agent_text(text);
    let mut quoted = String::with_capacity(plain.len() + QUOTE.len());
    for line in plain.split_inclusive(line_break) {
        if !line.starts_with(QUOTE) {
            quoted.push_str(QUOTE);
        }
        quoted.push_str(line);
    }
    assert!(
        quoted
            .split_inclusive(line_break)
            .all(|line| line.starts_with(QUOTE))
    );
    quoted
}

/// Writes text with every line opened by `> `, so no line of it can start as the owner, the
/// engine or any other speaker, whichever break opens it. Keeps the square brackets
/// `agent_text` rewrites, since agents pass each other code. Takes the text. Returns it.
pub(crate) fn quoted_lines(text: &str) -> String {
    let mut quoted = String::with_capacity(text.len() + QUOTE.len());
    for line in text.split_inclusive(line_break) {
        quoted.push_str(QUOTE);
        quoted.push_str(line);
    }
    quoted
}

/// Writes text that follows an engine head on its first line: that line as written and
/// every later one through `quoted_lines`. Takes the text. Returns it.
pub(crate) fn quoted_after_first_line(text: &str) -> String {
    let first = text.split_inclusive(line_break).next().unwrap_or_default();
    format!("{first}{}", quoted_lines(&text[first.len()..]))
}

/// Writes text a sub-agent wrote as part of one engine line, such as an event or the
/// question an agent waits on: made plain by `plain_agent_text` and every line break made a
/// space, so it never starts a line of its own. Takes the text. Returns one line.
fn agent_line(text: &str) -> String {
    let line: String = plain_agent_text(text)
        .chars()
        .map(|c| if line_break(c) { ' ' } else { c })
        .collect();
    assert!(!line.contains(line_break));
    line
}

/// Says whether a character breaks a line for some reader, the one rule for text an agent
/// wrote. Takes the character. Returns whether it does.
pub(crate) fn line_break(c: char) -> bool {
    matches!(
        c,
        '\n' | '\r' | '\u{0b}' | '\u{0c}' | '\u{85}' | '\u{2028}' | '\u{2029}'
    )
}

/// Makes agent text plain for the master, the owner's timelines and IDE review comments:
/// square brackets and the brackets that look like them, fullwidth, white, lenticular and
/// tortoise shell ones among them, become parentheses, so agent text never shows a `[owner]`
/// or `[team]` tag; control characters other than line feed and tab are dropped, so one escape
/// or carriage return never makes the master refuse a whole message; and bidirectional controls
/// are dropped, so no text reorders what is shown around it. Takes the text. Returns it.
pub(crate) fn plain_agent_text(text: &str) -> String {
    text.chars()
        .filter(|c| (!c.is_control() || matches!(c, '\n' | '\t')) && !BIDI_CONTROLS.contains(c))
        .map(|c| {
            if OPEN_BRACKETS.contains(&c) {
                '('
            } else if CLOSE_BRACKETS.contains(&c) {
                ')'
            } else {
                c
            }
        })
        .collect()
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

fn option(row: &str) -> Option<(usize, &str, Option<char>)> {
    let row = row.trim_start();
    let (marked, rest) = match row.chars().next() {
        Some(marker) if MARKERS.contains(&marker) => {
            (Some(marker), row[marker.len_utf8()..].trim_start())
        }
        _ => (None, row),
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
/// app's selection marker on exactly one option, such as Claude Code's permission and
/// question prompts and Codex's approval prompts, with the question line above it. A plain
/// list, or one quoted with `> ` on every line, is not a question. Takes the screen bytes.
/// Returns one bounded line with the question and its options, or none.
pub fn question(screen: &[u8]) -> Option<String> {
    choice(screen).map(|choice| choice.line())
}

/// A numbered choice an agent's app waits on, read from its screen: the question line above
/// it, each option's text in order without its number, the selection marker on the marked
/// option and the non-empty rows below the last option, down to the screen's end.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Choice {
    pub asked: String,
    pub options: Vec<String>,
    pub marker: char,
    pub below: Vec<String>,
}

impl Choice {
    /// The question and its numbered options as one line of at most EVENT_TEXT_BYTES_MAX
    /// bytes. Takes the choice. Returns the line.
    pub fn line(&self) -> String {
        let numbered: Vec<String> = self
            .options
            .iter()
            .enumerate()
            .map(|(at, text)| format!("{}. {text}", at + 1))
            .collect();
        let text = one_line(
            &format!("{} | {}", self.asked, numbered.join(" | ")),
            EVENT_TEXT_BYTES_MAX,
        );
        assert!(text.len() <= EVENT_TEXT_BYTES_MAX);
        text
    }
}

/// Finds the choice an agent's app waits on in its screen, as `question` describes: a real
/// selection list has one cursor, so a list with every option marked, such as a message the
/// engine typed with each line quoted as `> `, is no choice (F16). Takes the screen bytes.
/// Returns the question line and at least two options, or none.
pub fn choice(screen: &[u8]) -> Option<Choice> {
    let rows = shown_rows(screen);
    let rows = &rows[rows.len().saturating_sub(SCREEN_ROWS_SCANNED)..];
    let first = rows
        .iter()
        .rposition(|row| option(row).is_some_and(|(number, ..)| number == 1))?;
    let mut options = Vec::new();
    let mut marker = None;
    let mut marked_options = 0_usize;
    let mut last = first;
    for (at, row) in rows.iter().enumerate().skip(first) {
        match option(row) {
            Some((number, text, marked))
                if number == options.len() + 1 && options.len() < OPTIONS_MAX =>
            {
                marker = marker.or(marked);
                marked_options += usize::from(marked.is_some());
                last = at;
                options.push(text.to_owned());
            }
            Some(_) => break,
            None => {}
        }
    }
    assert!(marked_options <= options.len() && options.len() <= OPTIONS_MAX);
    let marker = marker?;
    if options.len() < 2 || marked_options != 1 {
        return None;
    }
    let above = &rows[first.saturating_sub(QUESTION_ROWS_ABOVE)..first];
    let asked = above
        .iter()
        .rev()
        .find(|row| row.ends_with('?'))
        .or_else(|| above.last())?;
    Some(Choice {
        asked: asked.clone(),
        options,
        marker,
        below: rows[last + 1..].to_vec(),
    })
}

/// Reads a screen's rows that show something, each without its box borders and the spaces
/// around it. Takes the screen bytes. Returns the rows in order.
fn shown_rows(screen: &[u8]) -> Vec<String> {
    screen_rows(screen)
        .into_iter()
        .map(|row| {
            row.trim_matches(|c: char| BORDERS.contains(&c) || c.is_whitespace())
                .to_owned()
        })
        .filter(|row| !row.is_empty())
        .collect()
}

/// Says whether an agent's app shows its own input prompt at the bottom of its screen,
/// empty or with a draft, so it waits for a message and for nothing else: Claude Code's
/// prompt row between two rules with at most `PROMPT_ROWS_BELOW_MAX` rows below it, or
/// Codex's composer row with at most `COMPOSER_ROWS_BELOW_MAX`. A dialog, a notice or a
/// list drawn in its place is not one. Takes the screen bytes. Returns the answer.
pub fn idle_prompt(screen: &[u8]) -> bool {
    let rows = shown_rows(screen);
    let Some(at) = rows.iter().rposition(|row| prompt_row(row)) else {
        return false;
    };
    let below = &rows[at + 1..];
    let boxed = at > 0 && ruled_row(&rows[at - 1]) && below.iter().any(|row| ruled_row(row));
    let composer = rows[at].starts_with(COMPOSER_MARK);
    (boxed && below.len() <= PROMPT_ROWS_BELOW_MAX)
        || (composer && below.len() <= COMPOSER_ROWS_BELOW_MAX)
}

/// Says whether a shown row is an input prompt's first row: a prompt mark, then nothing or
/// a space and a draft, and no numbered option. Takes the row. Returns the answer.
fn prompt_row(row: &str) -> bool {
    let mut rest = row.chars();
    rest.next().is_some_and(|mark| PROMPT_MARKS.contains(&mark))
        && matches!(rest.next(), None | Some(' '))
        && option(row).is_none()
}

/// Says whether a shown row is a horizontal rule or a box's top or bottom edge. Takes the
/// row. Returns the answer.
fn ruled_row(row: &str) -> bool {
    row.chars().count() >= RULE_CHARS_MIN && row.chars().all(|c| RULES.contains(&c))
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
    let cleared = cleared_levels(connection, project, &own)?;
    let (effective, level) = rule(connection, Some(project), None)?;
    let paused: bool = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM team_event_pauses WHERE project=?1 AND agent>=0)",
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
            app_version: rule_version(connection, &RuleTarget::App)?,
            project: mine,
            project_version: rule_version(
                connection,
                &RuleTarget::Project {
                    project_id: project.clone(),
                },
            )?,
            agents: own
                .into_iter()
                .map(|(agent, rule)| {
                    let target = RuleTarget::Agent {
                        project_id: project.clone(),
                        agent,
                    };
                    Ok(AgentRule {
                        agent,
                        rule,
                        version: rule_version(connection, &target)?,
                    })
                })
                .collect::<Result<_, EngineError>>()?,
            cleared,
            effective,
            level,
        },
        questions: questions(connection, project)?,
    })
}

/// Reads the agent levels a screen can edit that hold no rule of their own but whose version
/// moved on, because the owner cleared their rule, so the next change of such a level sends
/// the current version. Covers the sub-agents among the newest `STATUS_AGENTS_MAX` agents of
/// the project, the ones the team status lists. Takes the connection, the project and the
/// agent levels with a rule. Returns the levels in agent order, at most `STATUS_AGENTS_MAX`.
fn cleared_levels(
    connection: &Connection,
    project: &ProjectId,
    ruled: &[(i64, WakeRule)],
) -> Result<Vec<ClearedRule>, EngineError> {
    let mut statement = connection
        .prepare(
            "SELECT id FROM (SELECT id,role FROM team_agents WHERE project=?1 ORDER BY id DESC LIMIT ?2)
             WHERE role='sub' ORDER BY id",
        )
        .map_err(error)?;
    let listed: Vec<i64> = statement
        .query_map(params![project.as_str(), STATUS_AGENTS_MAX], |row| {
            row.get(0)
        })
        .map_err(error)?
        .collect::<Result<_, _>>()
        .map_err(error)?;
    let mut cleared = Vec::new();
    for agent in listed {
        if ruled.iter().any(|(own, _)| *own == agent) {
            continue;
        }
        let target = RuleTarget::Agent {
            project_id: project.clone(),
            agent,
        };
        let version = rule_version(connection, &target)?;
        if version > 0 {
            cleared.push(ClearedRule { agent, version });
        }
    }
    assert!(cleared.len() <= usize::try_from(STATUS_AGENTS_MAX).unwrap_or(usize::MAX));
    Ok(cleared)
}

/// Reads what the app shows across projects: the app wake rule and the newest notifications.
/// Takes the connection. Returns the view fitted to one owner reply by `fit_app`, with the
/// unseen count for the badge, the unseen count of each project for the sidebar and the
/// agents at work in each project for its live dot.
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
    fit_app(AppEvents {
        rule,
        rule_version: rule_version(connection, &RuleTarget::App)?,
        effective: rule.unwrap_or_default(),
        notifications: app_notifications(connection)?,
        unseen,
        projects: unseen_by_project(connection)?,
        working: working_by_project(connection)?,
    })
}

/// Names the versioned item of one wake-rule level.
fn rule_item(target: &RuleTarget) -> versioned::Item {
    match target {
        RuleTarget::App => versioned::team_rule_item(None, 0),
        RuleTarget::Project { project_id } => versioned::team_rule_item(Some(project_id), 0),
        RuleTarget::Agent { project_id, agent } => {
            versioned::team_rule_item(Some(project_id), *agent)
        }
    }
}

/// Reads the version of one wake-rule level, 0 when the level has no rule of its own.
/// Takes the connection and the level. Returns the version, or `versioned_unavailable`.
pub fn rule_version(connection: &Connection, target: &RuleTarget) -> Result<u64, EngineError> {
    if matches!(target, RuleTarget::Agent { agent, .. } if *agent <= 0) {
        return Ok(0);
    }
    Ok(versioned::current(connection, &rule_item(target))?.version)
}

/// Fits the app-wide view to one owner reply: the oldest seen notifications go first, then
/// the texts are cut to one shared cap, halved until the view fits, then the oldest go. The
/// unseen and working counts stay whole. Takes the view. Returns it, or
/// `team_events_bounds` when even an empty list does not fit.
fn fit_app(mut view: AppEvents) -> Result<AppEvents, EngineError> {
    let fits = |view: &AppEvents| -> Result<bool, EngineError> {
        Ok(super::encoded_len(view)? <= super::STATUS_REPLY_BYTES_MAX)
    };
    while !fits(&view)? {
        if !super::drop_older(&mut view.notifications, true, |item| item.seen_ms.is_some()) {
            break;
        }
    }
    if fits(&view)? {
        return Ok(view);
    }
    let whole = view.notifications.clone();
    for cap in super::STATUS_TEXT_CAPS {
        for (item, kept) in view.notifications.iter_mut().zip(&whole) {
            item.text = super::cut_text(&kept.text, cap);
        }
        if fits(&view)? {
            return Ok(view);
        }
    }
    while super::drop_older(&mut view.notifications, true, |_| true) {
        if fits(&view)? {
            return Ok(view);
        }
    }
    Err(error("team_events_bounds"))
}

/// Sets or clears the wake rule at one level when the owner's screen showed its current
/// version; clearing returns that level to the level above it and still moves its version
/// on, so a screen read before the clear cannot save over a newer rule. Takes the
/// transaction, the level, the rule or none and the version the screen showed.
/// Returns Ok, or `team_wake_cap_invalid`, `team_agent_not_found` or `stale_revision`.
pub fn set_rule(
    tx: &Transaction<'_>,
    target: &RuleTarget,
    rule: Option<&WakeRule>,
    version: u64,
) -> Result<Result<(), &'static str>, EngineError> {
    if rule.is_some_and(|rule| rule.hourly_cap > HOURLY_CAP_MAX) {
        return Ok(Err("team_wake_cap_invalid"));
    }
    let (project, agent) = match target {
        RuleTarget::App => ("", 0),
        RuleTarget::Project { project_id } => (project_id.as_str(), 0),
        RuleTarget::Agent { project_id, agent } => {
            let owned: bool = tx
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
    let item = rule_item(target);
    if versioned::owner_write(tx, &item, version)?.is_err() {
        return Ok(Err(versioned::STALE_CODE));
    }
    match rule {
        None => tx.execute(
            "DELETE FROM team_wake_rules WHERE project=?1 AND agent=?2",
            params![project, agent],
        ),
        Some(rule) => tx.execute(
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
    let text = match agent {
        COLD_PAUSE => format!(
            "Questions stopped starting the master of project {} after {cap} starts in the last hour. They wait until the master runs or the hour rolls over.",
            project.as_str()
        ),
        0 => format!(
            "Auto-wake of the master for project {} paused after {cap} wakes in the last hour. Events wait in the queue until the hour rolls over or you raise the limit.",
            project.as_str()
        ),
        _ => format!(
            "Auto-wake of the master for {} paused after {cap} wakes in the last hour. Events wait in the queue until the hour rolls over or you raise the limit.",
            label(tx, agent)?
        ),
    };
    notify(tx, project, NotificationKind::WakePaused, &text, now).map(drop)
}

/// Picks the events that wake the master next, oldest first, under each agent's wake rule:
/// an event whose kind is off is muted; at the project's hourly cap, or an agent's own cap,
/// those events wait and the owner is notified once per pause. At most `EVENTS_PER_WAKE_MAX`
/// events and `WAKE_BYTES_MAX` bytes go in one wake, and only those are read in full and
/// named. Takes the transaction, the project and the time. Returns the wake, or none when
/// nothing may wake the master now.
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
    let queued: Vec<(i64, i64, EventKind)> = {
        let mut statement = tx
            .prepare(
                "SELECT id,agent,kind FROM team_events WHERE project=?1 AND state='queued'
                 AND agent NOT IN (SELECT id FROM team_agents WHERE parent IS NOT NULL)
                 ORDER BY id LIMIT ?2",
            )
            .map_err(error)?;
        statement
            .query_map(params![project.as_str(), EVENTS_QUEUED_MAX], |row| {
                let kind: String = row.get(2)?;
                let kind = EventKind::parse(&kind).ok_or_else(|| {
                    rusqlite::Error::InvalidColumnType(
                        2,
                        "kind".into(),
                        rusqlite::types::Type::Text,
                    )
                })?;
                Ok((row.get(0)?, row.get(1)?, kind))
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
    for (id, agent, kind) in queued {
        if let Entry::Vacant(slot) = rules.entry(agent) {
            slot.insert(rule(tx, Some(project), Some(agent))?);
        }
        if rules[&agent].0.wakes(kind) {
            wanted.push((id, agent));
        } else {
            tx.execute(
                "UPDATE team_events SET state='muted' WHERE id=?1 AND state='queued'",
                [id],
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
    let mut full = tx
        .prepare(&format!(
            "SELECT {EVENT_COLUMNS} FROM team_events WHERE id=?1"
        ))
        .map_err(error)?;
    let mut events = Vec::new();
    let mut text = String::new();
    let mut asks = false;
    for (id, _) in wanted.iter().filter(|(_, agent)| !capped.contains(agent)) {
        if events.len() >= EVENTS_PER_WAKE_MAX {
            break;
        }
        let event = full
            .query_row([id], |row| event_row(tx, row))
            .map_err(error)?;
        let next = line(&event);
        if !text.is_empty() && text.len() + 1 + next.len() > WAKE_BYTES_MAX {
            break;
        }
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(&next);
        events.push(event.id);
        asks |= event.kind == EventKind::Question;
    }
    if events.is_empty() {
        return Ok(None);
    }
    assert!(text.len() <= WAKE_BYTES_MAX && events.len() <= EVENTS_PER_WAKE_MAX);
    Ok(Some(Wake {
        project: project.clone(),
        events,
        text,
        asks,
    }))
}

/// Marks a wake's events delivered once its text reached the master, counts the wake toward
/// the hourly cap, and toward the cold cap when it started a master that was not running,
/// and ends the pauses of that project and of the agents it carried.
/// Takes the transaction, the wake, whether it was cold and the time. Returns the wake id.
pub fn woke(tx: &Transaction<'_>, wake: &Wake, cold: bool, now: i64) -> Result<i64, EngineError> {
    assert!(!wake.events.is_empty());
    tx.execute(
        "INSERT INTO team_event_wakes(project,at_ms,cold) VALUES (?1,?2,?3)",
        params![wake.project.as_str(), now, cold],
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
        "DELETE FROM team_event_pauses WHERE project=?1 AND (agent<=0 OR agent IN (SELECT agent FROM team_events WHERE wake=?2))",
        params![wake.project.as_str(), id],
    )
    .map_err(error)?;
    Ok(id)
}

/// Delivers due events through the engine-driven master: once nothing else waits for the
/// master, one engine message with the wake's lines starts its next turn. With no live
/// master, only a queued question starts one, at most `COLD_WAKES_PER_HOUR_MAX` an hour;
/// other events wait for the master to run.
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
                    EXISTS(SELECT 1 FROM team_events WHERE project=?1 AND state='queued' AND kind='question'
                           AND agent NOT IN (SELECT id FROM team_agents WHERE parent IS NOT NULL))",
            [project.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(error)?;
    if waiting > 0 || !(master_live || asking) {
        return Ok(false);
    }
    if !master_live {
        let cap = rule(&tx, Some(project), None)?
            .0
            .hourly_cap
            .min(COLD_WAKES_PER_HOUR_MAX);
        let cold: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM team_event_wakes WHERE project=?1 AND cold=1 AND at_ms>=?2",
                params![project.as_str(), now.saturating_sub(HOUR_MS)],
                |row| row.get(0),
            )
            .map_err(error)?;
        if cold >= i64::from(cap) {
            pause(&tx, project, COLD_PAUSE, cap, now)?;
            tx.commit().map_err(error)?;
            return Ok(false);
        }
    }
    let Some(wake) = due(&tx, project, now)? else {
        tx.commit().map_err(error)?;
        return Ok(false);
    };
    if !master_live && !wake.asks {
        tx.commit().map_err(error)?;
        return Ok(false);
    }
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
        woke(&tx, &wake, !master_live, now)?;
    }
    tx.commit().map_err(error)?;
    Ok(sent)
}

#[cfg(all(test, unix))]
mod tests;
