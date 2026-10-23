use crate::config::{ModelAccess, ModelAuthentication, ModelEndpoint};
use crate::engine::EngineError;
use crate::project_setup::{HarnessSlot, StoredSetup, TeamPlan};
use crate::protocol::{CredentialRef, Outcome, ProjectId};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub mod runtime;

pub const MESSAGE_BYTES_MAX: usize = 8192;
pub const TASK_PROMPT_BYTES_MAX: usize = 4000;
pub const RESULT_BYTES_MAX: usize = 16 * 1024;
pub const DELEGATIONS_PER_REPLY_MAX: usize = 8;
pub const TASKS_ACTIVE_MAX: i64 = 32;
pub const TASKS_PER_PROJECT_MAX: i64 = 1000;
pub const MESSAGES_PER_PROJECT_MAX: i64 = 2000;
pub const STATUS_TASKS_MAX: i64 = 100;
pub const STATUS_MESSAGES_MAX: i64 = 50;
pub const STATUS_AGENTS_MAX: i64 = 80;
pub const DELEGATE_MARKER: &str = "DELEGATE";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentRole {
    Master,
    Sub,
}

impl AgentRole {
    fn name(self) -> &'static str {
        match self {
            Self::Master => "master",
            Self::Sub => "sub",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "master" => Some(Self::Master),
            "sub" => Some(Self::Sub),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentStatus {
    Starting,
    Idle,
    Working,
    Stopped,
    Failed,
}

impl AgentStatus {
    pub const ALL: [Self; 5] = [
        Self::Starting,
        Self::Idle,
        Self::Working,
        Self::Stopped,
        Self::Failed,
    ];

    /// Names the status as stored in the engine database.
    /// Takes the status.
    /// Returns the snake case name.
    pub fn name(self) -> &'static str {
        match self {
            Self::Starting => "starting",
            Self::Idle => "idle",
            Self::Working => "working",
            Self::Stopped => "stopped",
            Self::Failed => "failed",
        }
    }

    /// Gives the plain words the clients show for an agent.
    /// Takes the status.
    /// Returns a short label without internal terms.
    pub fn label(self) -> &'static str {
        match self {
            Self::Starting => "Starting",
            Self::Idle => "Waiting for work",
            Self::Working => "Working",
            Self::Stopped => "Stopped",
            Self::Failed => "Stopped with a problem",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|status| status.name() == value)
    }

    /// Tells whether the agent still holds a running process.
    /// Takes the status.
    /// Returns true for starting, idle and working agents.
    pub fn live(self) -> bool {
        matches!(self, Self::Starting | Self::Idle | Self::Working)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Queued,
    NeedsPool,
    Running,
    Done,
    Failed,
    Cancelled,
}

impl TaskStatus {
    pub const ALL: [Self; 6] = [
        Self::Queued,
        Self::NeedsPool,
        Self::Running,
        Self::Done,
        Self::Failed,
        Self::Cancelled,
    ];

    /// Names the status as stored in the engine database.
    /// Takes the status.
    /// Returns the snake case name.
    pub fn name(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::NeedsPool => "needs_pool",
            Self::Running => "running",
            Self::Done => "done",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    /// Gives the plain words the clients show for a task.
    /// Takes the status.
    /// Returns a short label without internal terms.
    pub fn label(self) -> &'static str {
        match self {
            Self::Queued => "Waiting to start",
            Self::NeedsPool => "Needs a pool",
            Self::Running => "Running",
            Self::Done => "Done",
            Self::Failed => "Failed",
            Self::Cancelled => "Cancelled",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|status| status.name() == value)
    }

    /// Tells whether the task reached an end state.
    /// Takes the status.
    /// Returns true for done, failed and cancelled tasks.
    pub fn finished(self) -> bool {
        matches!(self, Self::Done | Self::Failed | Self::Cancelled)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskSource {
    Master,
    Owner,
}

impl TaskSource {
    fn name(self) -> &'static str {
        match self {
            Self::Master => "master",
            Self::Owner => "owner",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "master" => Some(Self::Master),
            "owner" => Some(Self::Owner),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageAuthor {
    Owner,
    Master,
    Engine,
}

impl MessageAuthor {
    fn name(self) -> &'static str {
        match self {
            Self::Owner => "owner",
            Self::Master => "master",
            Self::Engine => "engine",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "owner" => Some(Self::Owner),
            "master" => Some(Self::Master),
            "engine" => Some(Self::Engine),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageState {
    Pending,
    Sent,
    Answered,
    Failed,
    Reply,
    Note,
}

impl MessageState {
    const ALL: [Self; 6] = [
        Self::Pending,
        Self::Sent,
        Self::Answered,
        Self::Failed,
        Self::Reply,
        Self::Note,
    ];

    fn name(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Sent => "sent",
            Self::Answered => "answered",
            Self::Failed => "failed",
            Self::Reply => "reply",
            Self::Note => "note",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|state| state.name() == value)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DelegationPath {
    Reply,
    Unavailable,
}

impl DelegationPath {
    /// Gives the plain words explaining how this harness hands work to sub-agents.
    /// Takes the path.
    /// Returns a short sentence for the clients.
    pub fn label(self) -> &'static str {
        match self {
            Self::Reply => {
                "Asks for sub-agents in its reply; the engine checks the team plan and creates the tasks"
            }
            Self::Unavailable => {
                "Cannot lead sub-agents yet; you can still hand tasks to sub-agents yourself"
            }
        }
    }
}

/// Maps a team plan harness identifier to the launch adapter that runs it.
/// Takes the opaque harness identifier stored in the team plan.
/// Returns the launch adapter identifier, or none when no adapter exists.
pub fn launch_harness(harness: &str) -> Option<&'static str> {
    crate::harnesses::spec(harness)
        .map(|spec| spec.id)
        .filter(|id| matches!(*id, "codex" | "claude-code"))
}

/// Records how a master on this harness asks for sub-agents.
/// Takes the team plan harness identifier.
/// Returns the reply path for launchable harnesses, otherwise unavailable.
pub fn delegation_path(harness: &str) -> DelegationPath {
    match launch_harness(harness) {
        Some(_) => DelegationPath::Reply,
        None => DelegationPath::Unavailable,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", deny_unknown_fields)]
pub enum TeamAction {
    Message {
        project_id: ProjectId,
        text: String,
    },
    Delegate {
        project_id: ProjectId,
        harness: Option<String>,
        prompt: String,
    },
    Cancel {
        project_id: ProjectId,
        task: Option<i64>,
    },
    KeepWarm {
        project_id: ProjectId,
        on: bool,
    },
}

impl TeamAction {
    /// Validates an untrusted owner team action before storage.
    /// Takes the action.
    /// Returns a plain rejection code when a bound fails.
    pub fn validate(&self) -> Result<(), EngineError> {
        match self {
            Self::Message { text, .. } => text_valid(text, MESSAGE_BYTES_MAX)
                .then_some(())
                .ok_or_else(|| failure("team_message_invalid")),
            Self::Delegate {
                harness, prompt, ..
            } => {
                if harness
                    .as_ref()
                    .is_some_and(|h| h.is_empty() || h.len() > 64 || !harness_name(h))
                {
                    return Err(failure("team_harness_invalid"));
                }
                text_valid(prompt, TASK_PROMPT_BYTES_MAX)
                    .then_some(())
                    .ok_or_else(|| failure("team_task_invalid"))
            }
            Self::Cancel { task, .. } => {
                if task.is_some_and(|task| task <= 0) {
                    return Err(failure("team_task_missing"));
                }
                Ok(())
            }
            Self::KeepWarm { .. } => Ok(()),
        }
    }

    /// Names the project the action is for.
    /// Takes the action.
    /// Returns its project id.
    pub fn project(&self) -> &ProjectId {
        match self {
            Self::Message { project_id, .. }
            | Self::Delegate { project_id, .. }
            | Self::Cancel { project_id, .. }
            | Self::KeepWarm { project_id, .. } => project_id,
        }
    }
}

fn failure(code: &str) -> EngineError {
    EngineError(code.into())
}

fn error(value: impl std::fmt::Display) -> EngineError {
    EngineError(value.to_string())
}

fn harness_name(value: &str) -> bool {
    value
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
}

fn text_valid(text: &str, max: usize) -> bool {
    !text.trim().is_empty()
        && text.len() <= max
        && !text
            .chars()
            .any(|c| c.is_control() && c != '\n' && c != '\t')
}

pub(crate) fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

/// One keyless pool account a team agent can run on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TeamAccount {
    pub connection: CredentialRef,
    pub base_url: String,
    pub model: String,
}

/// The accounts a harness may use for one project, or why there are none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Accounts {
    NoPool,
    Usable(Vec<TeamAccount>),
}

/// Lists pool accounts the launch harness can run on for one project.
/// Takes the live model access, the project and the launch harness identifier.
/// Returns NoPool when no pool is assigned, otherwise the usable members in pool order.
pub fn eligible_accounts(access: &ModelAccess, project: &ProjectId, harness: &str) -> Accounts {
    let assigned: Vec<_> = access
        .pools
        .iter()
        .filter(|pool| {
            pool.projects
                .iter()
                .any(|id| id.as_str() == project.as_str())
        })
        .collect();
    if assigned.is_empty() {
        return Accounts::NoPool;
    }
    let mut seen = BTreeSet::new();
    let mut usable = Vec::new();
    for pool in assigned {
        for member in &pool.members {
            if !seen.insert(member.clone()) {
                continue;
            }
            let Some(connection) = access.connections.iter().find(|c| &c.id == member) else {
                continue;
            };
            let (ModelEndpoint::Http { url }, ModelAuthentication::None {}) =
                (&connection.endpoint, &connection.auth)
            else {
                continue;
            };
            let protocol = connection.protocol.as_str();
            let fits = match harness {
                "codex" => protocol.starts_with("openai"),
                "claude-code" => protocol == "anthropic-messages",
                _ => false,
            };
            if fits {
                usable.push(TeamAccount {
                    connection: connection.id.clone(),
                    base_url: url.clone(),
                    model: connection.model.id.clone(),
                });
            }
        }
    }
    assert!(usable.len() <= seen.len());
    Accounts::Usable(usable)
}

/// Picks the least loaded usable account that is not paused for a rate limit.
/// Takes the usable accounts in pool order, live agents per connection and paused connections.
/// Returns the chosen account, or none when every account is paused.
pub fn pick_account<'a>(
    accounts: &'a [TeamAccount],
    load: &dyn Fn(&CredentialRef) -> usize,
    paused: &BTreeSet<CredentialRef>,
) -> Option<&'a TeamAccount> {
    accounts
        .iter()
        .filter(|account| !paused.contains(&account.connection))
        .enumerate()
        .min_by_key(|(order, account)| (load(&account.connection), *order))
        .map(|(_, account)| account)
}

/// One task the master asked for in its reply.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Delegation {
    pub harness: Option<String>,
    pub prompt: String,
}

/// Reads delegation lines from a master reply: `DELEGATE: task` or `DELEGATE harness: task`.
/// Takes the reply text; list bullets, quotes and backticks around a line are ignored.
/// Returns at most eight delegations whose task text is within bounds.
pub fn parse_delegations(reply: &str) -> Vec<Delegation> {
    let mut found = Vec::new();
    for line in reply.lines() {
        if found.len() == DELEGATIONS_PER_REPLY_MAX {
            break;
        }
        let line = line
            .trim()
            .trim_start_matches(|c: char| {
                c == '-' || c == '*' || c == '>' || c == '`' || c.is_ascii_digit() || c == '.'
            })
            .trim()
            .trim_end_matches('`')
            .trim();
        let Some(head) = line.get(..DELEGATE_MARKER.len()) else {
            continue;
        };
        if !head.eq_ignore_ascii_case(DELEGATE_MARKER) {
            continue;
        }
        let Some((target, task)) = line[DELEGATE_MARKER.len()..].split_once(':') else {
            continue;
        };
        let target = target.trim().to_ascii_lowercase();
        let task = task.trim().trim_matches('`').trim();
        if !(target.is_empty() || (target.len() <= 64 && harness_name(&target))) {
            continue;
        }
        if !text_valid(task, TASK_PROMPT_BYTES_MAX) {
            continue;
        }
        found.push(Delegation {
            harness: (!target.is_empty()).then_some(target),
            prompt: task.to_owned(),
        });
    }
    assert!(found.len() <= DELEGATIONS_PER_REPLY_MAX);
    found
}

/// Creates the team runtime tables when the engine database opens.
/// Takes the engine connection.
/// Returns any SQLite failure.
pub fn initialize(connection: &Connection) -> Result<(), EngineError> {
    connection
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS team_agents (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                project TEXT NOT NULL,
                role TEXT NOT NULL,
                harness TEXT NOT NULL,
                tools INTEGER NOT NULL,
                connection TEXT,
                worktree TEXT,
                status TEXT NOT NULL,
                task INTEGER,
                process_group INTEGER,
                turns INTEGER NOT NULL DEFAULT 0,
                error TEXT,
                created_ms INTEGER NOT NULL,
                heartbeat_ms INTEGER NOT NULL,
                stopped_ms INTEGER
            );
            CREATE INDEX IF NOT EXISTS team_agents_project ON team_agents(project, status);
            CREATE TABLE IF NOT EXISTS team_tasks (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                project TEXT NOT NULL,
                source TEXT NOT NULL,
                turn INTEGER,
                harness TEXT NOT NULL,
                tools INTEGER NOT NULL,
                prompt TEXT NOT NULL,
                status TEXT NOT NULL,
                agent INTEGER,
                attempts INTEGER NOT NULL DEFAULT 0,
                result TEXT,
                error TEXT,
                cancel_requested INTEGER NOT NULL DEFAULT 0,
                delivered INTEGER NOT NULL DEFAULT 0,
                output_bytes INTEGER NOT NULL DEFAULT 0,
                created_ms INTEGER NOT NULL,
                started_ms INTEGER,
                finished_ms INTEGER
            );
            CREATE INDEX IF NOT EXISTS team_tasks_project ON team_tasks(project, status);
            CREATE TABLE IF NOT EXISTS team_messages (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                project TEXT NOT NULL,
                author TEXT NOT NULL,
                text TEXT NOT NULL,
                state TEXT NOT NULL,
                created_ms INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS team_messages_project ON team_messages(project, state);
            CREATE TABLE IF NOT EXISTS team_settings (
                project TEXT PRIMARY KEY,
                keep_warm INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS team_paused_accounts (
                connection TEXT PRIMARY KEY,
                until_ms INTEGER NOT NULL
            );",
        )
        .map_err(error)
}

fn slot<'a>(plan: &'a TeamPlan, harness: &str) -> Option<&'a HarnessSlot> {
    let wanted = launch_harness(harness);
    plan.sub_harnesses.iter().find(|slot| {
        slot.harness == harness || (wanted.is_some() && launch_harness(&slot.harness) == wanted)
    })
}

fn active_tasks(
    tx: &Transaction<'_>,
    project: &ProjectId,
    harness: Option<&str>,
) -> Result<i64, EngineError> {
    tx.query_row(
        "SELECT COUNT(*) FROM team_tasks WHERE project=?1 AND status IN ('queued','needs_pool','running') AND (?2 IS NULL OR harness=?2)",
        params![project.as_str(), harness],
        |row| row.get(0),
    )
    .map_err(error)
}

/// Admits one task for a sub-agent against the project's team plan and pool.
/// Takes the transaction, live model access, stored setup, the proposed harness and task, its source and the master reply id.
/// Returns the new task id, or a plain refusal code when the plan does not allow it.
#[allow(clippy::too_many_arguments)]
pub fn admit(
    tx: &Transaction<'_>,
    access: &ModelAccess,
    stored: &StoredSetup,
    harness: Option<&str>,
    prompt: &str,
    source: TaskSource,
    turn: Option<i64>,
    now: i64,
) -> Result<Result<i64, &'static str>, EngineError> {
    if !text_valid(prompt, TASK_PROMPT_BYTES_MAX) {
        return Ok(Err("team_task_invalid"));
    }
    let plan = &stored.team;
    if plan.sub_harnesses.is_empty() {
        return Ok(Err("team_has_no_sub_agents"));
    }
    let chosen = match harness {
        Some(harness) => match slot(plan, harness) {
            Some(chosen) => chosen,
            None => return Ok(Err("team_harness_not_in_plan")),
        },
        None => {
            let mut best: Option<(&HarnessSlot, i64)> = None;
            for candidate in &plan.sub_harnesses {
                let load = active_tasks(tx, &stored.project_id, Some(&candidate.harness))? * 64
                    / i64::from(candidate.agents.max(1));
                if best.is_none_or(|(_, seen)| load < seen) {
                    best = Some((candidate, load));
                }
            }
            best.map(|(chosen, _)| chosen)
                .ok_or_else(|| failure("team_has_no_sub_agents"))?
        }
    };
    let Some(launch) = launch_harness(&chosen.harness) else {
        return Ok(Err("team_harness_cannot_run"));
    };
    if active_tasks(tx, &stored.project_id, None)? >= TASKS_ACTIVE_MAX {
        return Ok(Err("team_queue_full"));
    }
    let total: i64 = tx
        .query_row(
            "SELECT COUNT(*) FROM team_tasks WHERE project=?1",
            [stored.project_id.as_str()],
            |row| row.get(0),
        )
        .map_err(error)?;
    if total >= TASKS_PER_PROJECT_MAX {
        return Ok(Err("team_task_limit"));
    }
    let status = match eligible_accounts(access, &stored.project_id, launch) {
        Accounts::Usable(accounts) if !accounts.is_empty() => TaskStatus::Queued,
        _ => TaskStatus::NeedsPool,
    };
    tx.execute(
        "INSERT INTO team_tasks(project,source,turn,harness,tools,prompt,status,created_ms) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
        params![
            stored.project_id.as_str(),
            source.name(),
            turn,
            chosen.harness,
            chosen.tools,
            prompt,
            status.name(),
            now
        ],
    )
    .map_err(error)?;
    let id = tx.last_insert_rowid();
    assert!(id > 0);
    Ok(Ok(id))
}

fn rejected(code: &str) -> Outcome {
    Outcome::Rejected { code: code.into() }
}

/// Appends one message to a project's team conversation.
/// Takes the transaction, project, author, text, delivery state and time.
/// Returns the message id, or a rejection when the conversation is full.
pub fn append_message(
    tx: &Transaction<'_>,
    project: &ProjectId,
    author: MessageAuthor,
    text: &str,
    state: MessageState,
    now: i64,
) -> Result<Result<i64, &'static str>, EngineError> {
    let count: i64 = tx
        .query_row(
            "SELECT COUNT(*) FROM team_messages WHERE project=?1",
            [project.as_str()],
            |row| row.get(0),
        )
        .map_err(error)?;
    if count >= MESSAGES_PER_PROJECT_MAX {
        return Ok(Err("team_conversation_full"));
    }
    let bounded = bound_text(text, MESSAGE_BYTES_MAX * 2);
    tx.execute(
        "INSERT INTO team_messages(project,author,text,state,created_ms) VALUES (?1,?2,?3,?4,?5)",
        params![project.as_str(), author.name(), bounded, state.name(), now],
    )
    .map_err(error)?;
    Ok(Ok(tx.last_insert_rowid()))
}

/// Cuts text to at most `max` bytes on a character boundary.
/// Takes the text and the byte bound.
/// Returns the bounded text.
pub fn bound_text(text: &str, max: usize) -> String {
    let mut end = text.len().min(max);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    assert!(end <= max);
    text[..end].to_owned()
}

/// Executes one owner team action inside the owner transaction.
/// Takes the transaction, the engine configuration and the action.
/// Returns Applied, or a plain rejection before any row is written.
pub fn execute(
    tx: &Transaction<'_>,
    access: &ModelAccess,
    action: &TeamAction,
) -> Result<Outcome, EngineError> {
    action.validate()?;
    let project = action.project();
    let Some(stored) = crate::project_setup::load_setup(tx, project)? else {
        return Ok(rejected("project_setup_missing"));
    };
    let now = now_ms();
    match action {
        TeamAction::Message { text, .. } => Ok(
            match append_message(
                tx,
                project,
                MessageAuthor::Owner,
                text,
                MessageState::Pending,
                now,
            )? {
                Ok(_) => Outcome::Applied,
                Err(code) => rejected(code),
            },
        ),
        TeamAction::Delegate {
            harness, prompt, ..
        } => Ok(
            match admit(
                tx,
                access,
                &stored,
                harness.as_deref(),
                prompt,
                TaskSource::Owner,
                None,
                now,
            )? {
                Ok(_) => Outcome::Applied,
                Err(code) => rejected(code),
            },
        ),
        TeamAction::Cancel { task, .. } => {
            let matched: i64 = tx
                .query_row(
                    "SELECT COUNT(*) FROM team_tasks WHERE project=?1 AND (?2 IS NULL OR id=?2)",
                    params![project.as_str(), task],
                    |row| row.get(0),
                )
                .map_err(error)?;
            if task.is_some() && matched == 0 {
                return Ok(rejected("team_task_missing"));
            }
            tx.execute(
                "UPDATE team_tasks SET status='cancelled', finished_ms=?3, error='Cancelled by you'
                 WHERE project=?1 AND (?2 IS NULL OR id=?2) AND status IN ('queued','needs_pool')",
                params![project.as_str(), task, now],
            )
            .map_err(error)?;
            tx.execute(
                "UPDATE team_tasks SET cancel_requested=1
                 WHERE project=?1 AND (?2 IS NULL OR id=?2) AND status='running'",
                params![project.as_str(), task],
            )
            .map_err(error)?;
            Ok(Outcome::Applied)
        }
        TeamAction::KeepWarm { on, .. } => {
            tx.execute(
                "INSERT INTO team_settings(project,keep_warm) VALUES (?1,?2)
                 ON CONFLICT(project) DO UPDATE SET keep_warm=excluded.keep_warm",
                params![project.as_str(), on],
            )
            .map_err(error)?;
            Ok(Outcome::Applied)
        }
    }
}

/// One team agent as the clients poll it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TeamAgent {
    pub id: i64,
    pub role: AgentRole,
    pub harness: String,
    pub tools: bool,
    pub account: Option<String>,
    pub worktree: Option<String>,
    pub status: AgentStatus,
    pub status_label: String,
    pub task: Option<i64>,
    pub turns: i64,
    pub problem: Option<String>,
    pub process_group: Option<i64>,
    pub created_ms: i64,
    pub heartbeat_ms: i64,
    pub stopped_ms: Option<i64>,
}

/// One sub-agent task as the clients poll it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TeamTask {
    pub id: i64,
    pub source: TaskSource,
    pub harness: String,
    pub tools: bool,
    pub prompt: String,
    pub status: TaskStatus,
    pub status_label: String,
    pub agent: Option<i64>,
    pub attempts: i64,
    pub result: Option<String>,
    pub problem: Option<String>,
    pub delivered: bool,
    pub output_bytes: i64,
    pub created_ms: i64,
    pub started_ms: Option<i64>,
    pub finished_ms: Option<i64>,
}

/// One entry of the team conversation between owner, master and engine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TeamMessage {
    pub id: i64,
    pub author: MessageAuthor,
    pub text: String,
    pub state: MessageState,
    pub created_ms: i64,
}

/// One harness of the team plan with its live use.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TeamHarness {
    pub harness: String,
    pub role: AgentRole,
    pub agents_max: u8,
    pub tools: bool,
    pub live: i64,
    pub delegation: Option<DelegationPath>,
    pub delegation_label: Option<String>,
    pub runnable: bool,
}

/// Everything the clients show about a project's team.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TeamStatus {
    pub project_id: ProjectId,
    pub keep_warm: bool,
    pub min_agents: u8,
    pub max_agents: u8,
    pub harnesses: Vec<TeamHarness>,
    pub agents: Vec<TeamAgent>,
    pub tasks: Vec<TeamTask>,
    pub messages: Vec<TeamMessage>,
}

fn invalid_row(column: &str) -> rusqlite::Error {
    rusqlite::Error::InvalidColumnType(0, column.into(), rusqlite::types::Type::Text)
}

pub(crate) fn agent_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<TeamAgent> {
    let role: String = row.get(1)?;
    let status: String = row.get(6)?;
    let status = AgentStatus::parse(&status).ok_or_else(|| invalid_row("status"))?;
    Ok(TeamAgent {
        id: row.get(0)?,
        role: AgentRole::parse(&role).ok_or_else(|| invalid_row("role"))?,
        harness: row.get(2)?,
        tools: row.get(3)?,
        account: row.get(4)?,
        worktree: row.get(5)?,
        status,
        status_label: status.label().into(),
        task: row.get(7)?,
        turns: row.get(8)?,
        problem: row.get(9)?,
        process_group: row.get(10)?,
        created_ms: row.get(11)?,
        heartbeat_ms: row.get(12)?,
        stopped_ms: row.get(13)?,
    })
}

pub(crate) const AGENT_COLUMNS: &str = "id,role,harness,tools,connection,worktree,status,task,turns,error,process_group,created_ms,heartbeat_ms,stopped_ms";
pub(crate) const TASK_COLUMNS: &str = "id,source,harness,tools,prompt,status,agent,attempts,result,error,delivered,output_bytes,created_ms,started_ms,finished_ms";

pub(crate) fn task_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<TeamTask> {
    let source: String = row.get(1)?;
    let status: String = row.get(5)?;
    let status = TaskStatus::parse(&status).ok_or_else(|| invalid_row("status"))?;
    Ok(TeamTask {
        id: row.get(0)?,
        source: TaskSource::parse(&source).ok_or_else(|| invalid_row("source"))?,
        harness: row.get(2)?,
        tools: row.get(3)?,
        prompt: row.get(4)?,
        status,
        status_label: status.label().into(),
        agent: row.get(6)?,
        attempts: row.get(7)?,
        result: row.get(8)?,
        problem: row.get(9)?,
        delivered: row.get(10)?,
        output_bytes: row.get(11)?,
        created_ms: row.get(12)?,
        started_ms: row.get(13)?,
        finished_ms: row.get(14)?,
    })
}

fn message_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<TeamMessage> {
    let author: String = row.get(1)?;
    let state: String = row.get(3)?;
    Ok(TeamMessage {
        id: row.get(0)?,
        author: MessageAuthor::parse(&author).ok_or_else(|| invalid_row("author"))?,
        text: row.get(2)?,
        state: MessageState::parse(&state).ok_or_else(|| invalid_row("state"))?,
        created_ms: row.get(4)?,
    })
}

/// Tells whether the owner asked to keep the minimum agents warm.
/// Takes a connection and the project.
/// Returns false until the owner turns it on.
pub fn keep_warm(connection: &Connection, project: &ProjectId) -> Result<bool, EngineError> {
    connection
        .query_row(
            "SELECT keep_warm FROM team_settings WHERE project=?1",
            [project.as_str()],
            |row| row.get(0),
        )
        .optional()
        .map(|value| value.unwrap_or(false))
        .map_err(error)
}

/// Reads a project's team plan use, agents, recent tasks and recent conversation.
/// Takes a connection and the stored setup.
/// Returns bounded lists in creation order.
pub fn status(connection: &Connection, stored: &StoredSetup) -> Result<TeamStatus, EngineError> {
    let project = &stored.project_id;
    let recent = |sql: &str, limit: i64| {
        format!("SELECT * FROM ({sql} ORDER BY id DESC LIMIT {limit}) ORDER BY id")
    };
    let mut statement = connection
        .prepare(&recent(
            &format!("SELECT {AGENT_COLUMNS} FROM team_agents WHERE project=?1"),
            STATUS_AGENTS_MAX,
        ))
        .map_err(error)?;
    let agents = statement
        .query_map([project.as_str()], agent_row)
        .map_err(error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(error)?;
    let mut statement = connection
        .prepare(&recent(
            &format!("SELECT {TASK_COLUMNS} FROM team_tasks WHERE project=?1"),
            STATUS_TASKS_MAX,
        ))
        .map_err(error)?;
    let tasks = statement
        .query_map([project.as_str()], task_row)
        .map_err(error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(error)?;
    let mut statement = connection
        .prepare(&recent(
            "SELECT id,author,text,state,created_ms FROM team_messages WHERE project=?1",
            STATUS_MESSAGES_MAX,
        ))
        .map_err(error)?;
    let messages = statement
        .query_map([project.as_str()], message_row)
        .map_err(error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(error)?;
    let live = |role: AgentRole, harness: &str| {
        agents
            .iter()
            .filter(|a| a.role == role && a.harness == harness && a.status.live())
            .count() as i64
    };
    let plan = &stored.team;
    let mut harnesses = vec![TeamHarness {
        harness: plan.master.clone(),
        role: AgentRole::Master,
        agents_max: 1,
        tools: plan.master_tools,
        live: live(AgentRole::Master, &plan.master),
        delegation: Some(delegation_path(&plan.master)),
        delegation_label: Some(delegation_path(&plan.master).label().into()),
        runnable: launch_harness(&plan.master).is_some(),
    }];
    harnesses.extend(plan.sub_harnesses.iter().map(|slot| TeamHarness {
        harness: slot.harness.clone(),
        role: AgentRole::Sub,
        agents_max: slot.agents,
        tools: slot.tools,
        live: live(AgentRole::Sub, &slot.harness),
        delegation: None,
        delegation_label: None,
        runnable: launch_harness(&slot.harness).is_some(),
    }));
    assert!(agents.len() as i64 <= STATUS_AGENTS_MAX);
    Ok(TeamStatus {
        project_id: project.clone(),
        keep_warm: keep_warm(connection, project)?,
        min_agents: plan.min_agents,
        max_agents: plan.max_agents,
        harnesses,
        agents,
        tasks,
        messages,
    })
}

#[cfg(test)]
mod tests;
