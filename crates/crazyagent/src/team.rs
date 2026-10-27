use crate::config::{ModelAccess, ModelAuthentication, ModelEndpoint};
use crate::engine::EngineError;
use crate::project_setup::{HarnessSlot, StoredSetup, TEAM_AGENTS_MAX, TeamPlan};
use crate::protocol::{CredentialRef, Outcome, ProjectId, RequestId, SessionId};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub mod direct;
pub mod graph;
pub mod master;
pub mod names;
pub mod runtime;
pub mod tool;

pub use graph::{EdgeState, GraphEdge, GraphNode, NodeKind, TeamGraph, graph};

pub const MESSAGE_BYTES_MAX: usize = 8192;
pub const TASK_PROMPT_BYTES_MAX: usize = 4000;
pub const RESULT_BYTES_MAX: usize = 16 * 1024;
pub const DELEGATIONS_PER_REPLY_MAX: usize = 8;
pub const NOTE_BYTES_MAX: usize = 800;
pub const CONVERSATION_TASKS_MAX: i64 = 1024;
pub const TASKS_ACTIVE_MAX: i64 = 32;
pub const TASKS_PER_PROJECT_MAX: i64 = 1000;
pub const MESSAGES_PER_PROJECT_MAX: i64 = 2000;
pub const MESSAGES_WAITING_MAX: i64 = 64;
const MESSAGE_HISTORY: &str = "state NOT IN ('pending','sent')";
const TASK_HISTORY: &str =
    "status IN ('done','failed','cancelled') AND (source='owner' OR delivered=1)";
pub const STATUS_TASKS_MAX: i64 = 100;
pub const STATUS_MESSAGES_MAX: i64 = 50;
pub const STATUS_AGENTS_MAX: i64 = 80;
pub const DELEGATE_MARKER: &str = "DELEGATE";
pub const DIRECT_INPUT_BYTES_MAX: usize = 4096;
const NATIVE_DEFAULT_MODEL: &str = "native-default";

/// A prompt the owner sent straight to one team sub-agent with an explicit send. It is kept
/// on the agent's task until the master hears a short note of it on its next turn.
/// Keys typed into the terminal are never kept. `agent` is the team agent id; `text` is at
/// most `DIRECT_INPUT_BYTES_MAX` bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectInput {
    pub project: ProjectId,
    pub agent: i64,
    pub text: String,
    pub at_ms: i64,
}

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

    /// Names one team agent the same way in the team list and the team graph of every client.
    /// Takes the role and the agent's id.
    /// Returns "Master", or "Sub-agent" and the id.
    pub fn agent_label(self, id: i64) -> String {
        match self {
            Self::Master => "Master".into(),
            Self::Sub => format!("Sub-agent {id}"),
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
    Tool,
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
            Self::Tool => {
                "Calls the engine's delegate tool or asks in its reply; the engine checks the team plan and creates the tasks"
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
/// Takes the team plan harness identifier and the master's tools switch.
/// Returns the tool path with tools on, the reply path with tools off, unavailable when it cannot launch.
pub fn delegation_path(harness: &str, tools: bool) -> DelegationPath {
    match launch_harness(harness) {
        Some(_) if tools => DelegationPath::Tool,
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
    LiveMaster {
        project_id: ProjectId,
        on: bool,
    },
    Pause {
        project_id: ProjectId,
    },
    Resume {
        project_id: ProjectId,
    },
    HandOff {
        project_id: ProjectId,
        reply: i64,
        step: u8,
    },
    Rename {
        project_id: ProjectId,
        agent: i64,
        handle: String,
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
            Self::HandOff { reply, step, .. } => {
                if *reply <= 0 || usize::from(*step) >= DELEGATIONS_PER_REPLY_MAX {
                    return Err(failure("team_plan_step_missing"));
                }
                Ok(())
            }
            Self::Rename { agent, handle, .. } => {
                if *agent <= 0 || handle.len() > names::HANDLE_CHARS_MAX + 1 {
                    return Err(failure("team_handle_invalid"));
                }
                Ok(())
            }
            Self::KeepWarm { .. }
            | Self::LiveMaster { .. }
            | Self::Pause { .. }
            | Self::Resume { .. } => Ok(()),
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
            | Self::KeepWarm { project_id, .. }
            | Self::LiveMaster { project_id, .. }
            | Self::Pause { project_id }
            | Self::Resume { project_id }
            | Self::HandOff { project_id, .. }
            | Self::Rename { project_id, .. } => project_id,
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

/// One pool account a team agent can run on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TeamAccount {
    pub connection: CredentialRef,
    pub reach: TeamReach,
}

/// How a team agent reaches its model: a keyless HTTP endpoint, or a native managed
/// profile the owner signed in, which the engine lends to one agent turn at a time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TeamReach {
    Endpoint {
        base_url: String,
        model: String,
    },
    Managed {
        profile_ref: CredentialRef,
        model: Option<String>,
    },
}

/// The accounts a harness may use for one project, or why there are none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Accounts {
    NoPool,
    Usable(Vec<TeamAccount>),
}

/// Lists pool accounts the launch harness can run on for one project: keyless endpoints
/// whose protocol fits and native managed profiles of that harness, signed in or not.
/// Takes the live model access, the project and the launch harness identifier.
/// Returns NoPool when no pool is assigned, otherwise the fitting members in pool order.
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
            let reach = match (&connection.endpoint, &connection.auth) {
                (ModelEndpoint::Http { url }, ModelAuthentication::None {}) => {
                    let protocol = connection.protocol.as_str();
                    let fits = match harness {
                        "codex" => protocol.starts_with("openai"),
                        "claude-code" => protocol == "anthropic-messages",
                        _ => false,
                    };
                    fits.then(|| TeamReach::Endpoint {
                        base_url: url.clone(),
                        model: connection.model.id.clone(),
                    })
                }
                (ModelEndpoint::Native {}, ModelAuthentication::NativeManaged { profile_ref }) => {
                    let fits = crate::harnesses::spec(connection.harness.as_str())
                        .is_some_and(|spec| spec.id == harness);
                    fits.then(|| TeamReach::Managed {
                        profile_ref: profile_ref.clone(),
                        model: (connection.model.id != NATIVE_DEFAULT_MODEL)
                            .then(|| connection.model.id.clone()),
                    })
                }
                _ => None,
            };
            if let Some(reach) = reach {
                usable.push(TeamAccount {
                    connection: connection.id.clone(),
                    reach,
                });
            }
        }
    }
    assert!(usable.len() <= seen.len());
    Accounts::Usable(usable)
}

/// Picks the usable account the owner's schedule puts first among those not paused for a
/// rate limit; with no limits or ranks that is the least loaded, then pool order.
/// Takes the usable accounts in pool order, live agents per connection, paused connections
/// and the scheduling choice. Returns the chosen account, or none when every account is
/// paused or left out by the schedule.
pub fn pick_account<'a>(
    accounts: &'a [TeamAccount],
    load: &dyn Fn(&CredentialRef) -> usize,
    paused: &BTreeSet<CredentialRef>,
    choice: &crate::scheduling::Choice,
) -> Option<&'a TeamAccount> {
    let open: Vec<&TeamAccount> = accounts
        .iter()
        .filter(|account| !paused.contains(&account.connection))
        .collect();
    let candidates: Vec<crate::scheduling::Candidate> = open
        .iter()
        .map(|account| crate::scheduling::Candidate {
            connection: account.connection.clone(),
            load: load(&account.connection),
            current: false,
        })
        .collect();
    choice
        .order(&candidates)
        .ready
        .first()
        .map(|index| open[*index])
}

/// One task the master asked for in its reply, with the role handle it gave the job, if any.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Delegation {
    pub harness: Option<String>,
    pub prompt: String,
    #[serde(default)]
    pub role: Option<String>,
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
        if let Some(delegation) = delegation(target, task.trim().trim_matches('`')) {
            found.push(delegation);
        }
    }
    assert!(found.len() <= DELEGATIONS_PER_REPLY_MAX);
    found
}

fn delegation(target: &str, task: &str) -> Option<Delegation> {
    let target = target.trim().to_ascii_lowercase();
    let task = task.trim();
    if !(target.is_empty() || (target.len() <= 64 && harness_name(&target))) {
        return None;
    }
    if !text_valid(task, TASK_PROMPT_BYTES_MAX) {
        return None;
    }
    Some(Delegation {
        harness: (!target.is_empty()).then_some(target),
        prompt: task.to_owned(),
        role: None,
    })
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
            CREATE TABLE IF NOT EXISTS team_conversation_tasks (
                task INTEGER PRIMARY KEY,
                project TEXT NOT NULL,
                session TEXT NOT NULL,
                turn TEXT NOT NULL,
                note TEXT,
                noted_ms INTEGER
            );
            CREATE INDEX IF NOT EXISTS team_conversation_tasks_turn ON team_conversation_tasks(session, turn);
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
            );
            CREATE TABLE IF NOT EXISTS team_paused_projects (
                project TEXT PRIMARY KEY,
                since_ms INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS team_panes (
                agent INTEGER PRIMARY KEY,
                record TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS team_task_pins (
                task INTEGER PRIMARY KEY,
                project TEXT NOT NULL,
                agent INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS team_worktrees (
                agent INTEGER PRIMARY KEY,
                project TEXT NOT NULL,
                repository TEXT NOT NULL,
                worktree TEXT NOT NULL,
                git_dir TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS team_owner_prompts (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                project TEXT NOT NULL,
                agent INTEGER NOT NULL,
                task INTEGER,
                harness TEXT NOT NULL,
                text TEXT NOT NULL,
                created_ms INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS team_owner_prompts_project ON team_owner_prompts(project, id);",
        )
        .map_err(error)?;
    master::initialize(connection)?;
    names::initialize(connection)?;
    direct::erase_typed_input(connection)
}

/// Finds the plan slot a sub-agent on this harness runs under. An empty plan means any
/// harness (DEC-096 addendum 3): each harness that can run team agents gets a slot whose
/// count never binds before the team maximum, with tools following the master's switch.
/// Takes the plan and a harness id. Returns the slot, or none when the plan does not allow it.
fn slot(plan: &TeamPlan, harness: &str) -> Option<HarnessSlot> {
    let wanted = launch_harness(harness);
    if plan.sub_harnesses.is_empty() {
        return wanted.map(|id| HarnessSlot {
            harness: id.to_owned(),
            agents: TEAM_AGENTS_MAX,
            tools: plan.master_tools,
        });
    }
    plan.sub_harnesses
        .iter()
        .find(|slot| {
            slot.harness == harness || (wanted.is_some() && launch_harness(&slot.harness) == wanted)
        })
        .cloned()
}

/// Lists every harness that can run team agents, in catalog order.
fn runnable_harnesses() -> impl Iterator<Item = &'static str> {
    crate::harnesses::specs()
        .iter()
        .filter_map(|spec| launch_harness(spec.id))
}

/// Lists the harnesses an empty team plan can use now: those that run team agents and have
/// an eligible account in the project's pool. Takes the live model access and the project.
/// Returns the launch harness ids in catalog order, empty without a pool or fitting account.
pub fn available_harnesses(access: &ModelAccess, project: &ProjectId) -> Vec<&'static str> {
    runnable_harnesses()
        .filter(|id| {
            matches!(eligible_accounts(access, project, id),
                Accounts::Usable(accounts) if !accounts.is_empty())
        })
        .collect()
}

/// Lists the harnesses a plan hands work to: its own slots, or for an empty plan every
/// harness that can run team agents. Takes the plan. Returns the harness ids.
fn plan_harnesses(plan: &TeamPlan) -> Vec<String> {
    if plan.sub_harnesses.is_empty() {
        return runnable_harnesses().map(str::to_owned).collect();
    }
    plan.sub_harnesses
        .iter()
        .map(|slot| slot.harness.clone())
        .collect()
}

/// Tells a master leading an empty team plan which harnesses it can use and that only the
/// team maximum limits them, or that a maximum of one agent leaves no room for sub-agents.
/// Takes the plan and the harnesses with an eligible account now. Returns plain lines, each
/// ending in a newline.
pub fn any_harness_text(plan: &TeamPlan, available: &[&str]) -> String {
    assert!(plan.sub_harnesses.is_empty());
    if plan.max_agents <= 1 {
        return "The team maximum is 1 agent, you, so CrazyAgent refuses every task you hand over. Do the work yourself, or tell the owner to raise the team maximum in the project's Team settings.\n".to_owned();
    }
    let mut text = if available.is_empty() {
        "The team plan names no harness, so any harness that can run sub-agents may take your tasks, but no harness has an account in the project's pool yet. Tasks you hand over wait until the owner adds one.\n".to_owned()
    } else {
        let mut list =
            "The team plan names no harness, so you may use any harness that can run now:\n"
                .to_owned();
        for harness in available {
            list.push_str(&format!("- {harness}\n"));
        }
        list
    };
    text.push_str(&format!(
        "There is no per-harness limit, only the team maximum of {} agents, counting you.\n",
        plan.max_agents
    ));
    text
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

/// Picks the slot for a task under an empty team plan (DEC-096 addendum 3); a team maximum
/// of one agent leaves no room. A named harness must run team agents and, while any harness
/// has an eligible account, be one of those.
/// Without a name the least loaded of those wins, the master's harness on a tie. With no
/// eligible account anywhere the task parks on the named harness, or the master's.
/// Takes the transaction, live access, the setup and the named harness. Returns the slot,
/// or a plain refusal code.
fn any_slot(
    tx: &Transaction<'_>,
    access: &ModelAccess,
    stored: &StoredSetup,
    harness: Option<&str>,
) -> Result<Result<HarnessSlot, &'static str>, EngineError> {
    let plan = &stored.team;
    assert!(plan.sub_harnesses.is_empty());
    if plan.max_agents <= 1 {
        return Ok(Err("team_has_no_sub_agents"));
    }
    let available = available_harnesses(access, &stored.project_id);
    if let Some(harness) = harness {
        let Some(chosen) = slot(plan, harness) else {
            return Ok(Err(if crate::harnesses::spec(harness).is_some() {
                "team_harness_cannot_run"
            } else {
                "team_harness_not_in_plan"
            }));
        };
        if !available.is_empty() && !available.contains(&chosen.harness.as_str()) {
            return Ok(Err("team_harness_cannot_run"));
        }
        return Ok(Ok(chosen));
    }
    let master = launch_harness(&plan.master);
    let mut candidates = if available.is_empty() {
        master
            .or_else(|| runnable_harnesses().next())
            .into_iter()
            .collect()
    } else {
        available
    };
    candidates.sort_by_key(|id| Some(*id) != master);
    let mut best: Option<(&str, i64)> = None;
    for candidate in candidates {
        let load = active_tasks(tx, &stored.project_id, Some(candidate))?;
        if best.is_none_or(|(_, seen)| load < seen) {
            best = Some((candidate, load));
        }
    }
    let Some((chosen, _)) = best else {
        return Ok(Err("team_harness_cannot_run"));
    };
    Ok(Ok(slot(plan, chosen).expect(
        "a harness that runs team agents has a slot in an empty plan",
    )))
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
    let chosen = if plan.sub_harnesses.is_empty() {
        match any_slot(tx, access, stored, harness)? {
            Ok(chosen) => chosen,
            Err(code) => return Ok(Err(code)),
        }
    } else {
        match harness {
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
                best.map(|(chosen, _)| chosen.clone())
                    .ok_or_else(|| failure("team_has_no_sub_agents"))?
            }
        }
    };
    let Some(launch) = launch_harness(&chosen.harness) else {
        return Ok(Err("team_harness_cannot_run"));
    };
    if active_tasks(tx, &stored.project_id, None)? >= TASKS_ACTIVE_MAX {
        return Ok(Err("team_queue_full"));
    }
    let kept: i64 = tx
        .query_row(
            &format!("SELECT COUNT(*) FROM team_tasks WHERE project=?1 AND NOT ({TASK_HISTORY})"),
            [stored.project_id.as_str()],
            |row| row.get(0),
        )
        .map_err(error)?;
    if kept >= TASKS_PER_PROJECT_MAX {
        return Ok(Err("team_task_limit"));
    }
    let status = match eligible_accounts(access, &stored.project_id, launch) {
        Accounts::Usable(accounts) if !accounts.is_empty() => TaskStatus::Queued,
        _ => TaskStatus::NeedsPool,
    };
    make_room(
        tx,
        "team_tasks",
        TASK_HISTORY,
        &stored.project_id,
        TASKS_PER_PROJECT_MAX,
    )?;
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

/// Starts the work that resolves what an Ask agent to update merged into a team agent's
/// worktree (DEC-095): a sub-agent gets a task pinned to it, so it runs in the same
/// worktree and lands in the same change set; the team master gets an engine message. A
/// sub-agent holds at most one queued resolve task, whose prompt the newest update
/// replaces, so the global task cap never refuses a merge already made. Takes the
/// transaction, project, agent, prompt and time. Returns Ok, or a plain refusal code.
pub fn admit_resolve(
    tx: &Transaction<'_>,
    project: &ProjectId,
    agent: i64,
    prompt: &str,
    now: i64,
) -> Result<Result<(), &'static str>, EngineError> {
    if !text_valid(prompt, TASK_PROMPT_BYTES_MAX) {
        return Ok(Err("team_task_invalid"));
    }
    let found: Option<(String, String, bool)> = tx
        .query_row(
            "SELECT a.role,a.harness,a.tools FROM team_agents a JOIN team_worktrees w ON w.agent=a.id
             WHERE a.id=?1 AND a.project=?2 AND w.project=?2",
            params![agent, project.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()
        .map_err(error)?;
    let Some((role, harness, tools)) = found else {
        return Ok(Err("team_agent_not_found"));
    };
    match AgentRole::parse(&role) {
        Some(AgentRole::Sub) => {}
        Some(AgentRole::Master) => {
            return append_message(
                tx,
                project,
                MessageAuthor::Engine,
                prompt,
                MessageState::Pending,
                now,
            )
            .map(|sent| sent.map(|_| ()));
        }
        None => return Ok(Err("team_agent_not_found")),
    }
    let queued: Option<i64> = tx
        .query_row(
            "SELECT t.id FROM team_task_pins p JOIN team_tasks t ON t.id=p.task
             WHERE p.project=?1 AND p.agent=?2 AND t.status=?3",
            params![project.as_str(), agent, TaskStatus::Queued.name()],
            |row| row.get(0),
        )
        .optional()
        .map_err(error)?;
    if let Some(task) = queued {
        tx.execute(
            "UPDATE team_tasks SET prompt=?2 WHERE id=?1",
            params![task, prompt],
        )
        .map_err(error)?;
        return Ok(Ok(()));
    }
    make_room(
        tx,
        "team_tasks",
        TASK_HISTORY,
        project,
        TASKS_PER_PROJECT_MAX,
    )?;
    tx.execute(
        "INSERT INTO team_tasks(project,source,turn,harness,tools,prompt,status,created_ms) VALUES (?1,?2,NULL,?3,?4,?5,?6,?7)",
        params![
            project.as_str(),
            TaskSource::Owner.name(),
            harness,
            tools,
            prompt,
            TaskStatus::Queued.name(),
            now
        ],
    )
    .map_err(error)?;
    let task = tx.last_insert_rowid();
    assert!(task > 0);
    tx.execute(
        "DELETE FROM team_task_pins WHERE project=?1 AND task NOT IN (SELECT id FROM team_tasks WHERE project=?1)",
        [project.as_str()],
    )
    .map_err(error)?;
    tx.execute(
        "INSERT INTO team_task_pins(task,project,agent) VALUES (?1,?2,?3)",
        params![task, project.as_str(), agent],
    )
    .map_err(error)?;
    Ok(Ok(()))
}

/// Hands the tasks a project's conversation master proposed with its `delegate` tool to the
/// project's team (DEC-093) as the owner's own tasks: each passes `admit` against the team
/// plan and pool, and the refused ones are listed in one engine note in the Team view, never
/// dropped silently. Each admitted task is tied to the conversation turn that proposed it,
/// so the engine's note on how it ended comes back to that conversation.
/// Takes the transaction, live model access, the project, the conversation session and
/// turn, at most `DELEGATIONS_PER_REPLY_MAX` proposals and the time. Returns the task ids.
pub fn admit_conversation(
    tx: &Transaction<'_>,
    access: &ModelAccess,
    project: &ProjectId,
    origin: (&SessionId, &RequestId),
    proposed: &[Delegation],
    now: i64,
) -> Result<Vec<i64>, EngineError> {
    assert!(proposed.len() <= DELEGATIONS_PER_REPLY_MAX);
    let stored = crate::project_setup::load_setup(tx, project)?;
    let mut admitted = Vec::new();
    let mut refused = Vec::new();
    for delegation in proposed {
        let outcome = match &stored {
            Some(stored) => admit(
                tx,
                access,
                stored,
                delegation.harness.as_deref(),
                &delegation.prompt,
                TaskSource::Owner,
                None,
                now,
            )?,
            None => Err("team_plan_missing"),
        };
        match outcome {
            Ok(id) => {
                tx.execute(
                    "INSERT INTO team_conversation_tasks(task,project,session,turn) VALUES (?1,?2,?3,?4)",
                    params![id, project.as_str(), origin.0.as_str(), origin.1.as_str()],
                )
                .map_err(error)?;
                admitted.push(id);
            }
            Err(code) => refused.push(format!(
                "- {}: {}",
                bound_text(&delegation.prompt, 200),
                runtime::refusal(code)
            )),
        }
    }
    if !refused.is_empty() {
        let _ = append_message(
            tx,
            project,
            MessageAuthor::Engine,
            &format!(
                "The conversation master's tasks were not handed off:\n{}",
                refused.join("\n")
            ),
            MessageState::Note,
            now,
        )?;
    }
    if !admitted.is_empty() {
        tx.execute(
            "DELETE FROM team_conversation_tasks WHERE project=?1
             AND task NOT IN (SELECT task FROM team_conversation_tasks WHERE project=?1 ORDER BY task DESC LIMIT ?2)
             AND (note IS NOT NULL OR task NOT IN (SELECT id FROM team_tasks))",
            params![project.as_str(), CONVERSATION_TASKS_MAX],
        )
        .map_err(error)?;
    }
    assert_eq!(admitted.len() + refused.len(), proposed.len());
    Ok(admitted)
}

fn rejected(code: &str) -> Outcome {
    Outcome::Rejected { code: code.into() }
}

/// Deletes a project's oldest history rows so that one more row fits within `max`.
/// Takes the transaction, the table, the SQL condition that marks history, the project and the bound.
/// Returns any SQLite failure; live rows are never deleted, so live rows filling the bound is a broken invariant.
fn make_room(
    tx: &Transaction<'_>,
    table: &str,
    history: &str,
    project: &ProjectId,
    max: i64,
) -> Result<(), EngineError> {
    let count = |tx: &Transaction<'_>| -> Result<i64, EngineError> {
        tx.query_row(
            &format!("SELECT COUNT(*) FROM {table} WHERE project=?1"),
            [project.as_str()],
            |row| row.get(0),
        )
        .map_err(error)
    };
    let excess = count(tx)? + 1 - max;
    if excess > 0 {
        tx.execute(
            &format!(
                "DELETE FROM {table} WHERE id IN
                 (SELECT id FROM {table} WHERE project=?1 AND {history} ORDER BY id LIMIT ?2)"
            ),
            params![project.as_str(), excess],
        )
        .map_err(error)?;
    }
    assert!(count(tx)? < max, "team_live_rows_fill_the_window");
    Ok(())
}

/// Appends one message to a project's team conversation, keeping its newest
/// `MESSAGES_PER_PROJECT_MAX` rows: older answered, failed, reply and note rows make room.
/// Takes the transaction, project, author, text, delivery state and time.
/// Returns the message id, or a rejection when too many messages already wait for the master.
pub fn append_message(
    tx: &Transaction<'_>,
    project: &ProjectId,
    author: MessageAuthor,
    text: &str,
    state: MessageState,
    now: i64,
) -> Result<Result<i64, &'static str>, EngineError> {
    if matches!(state, MessageState::Pending | MessageState::Sent) {
        let waiting: i64 = tx
            .query_row(
                &format!(
                    "SELECT COUNT(*) FROM team_messages WHERE project=?1 AND NOT ({MESSAGE_HISTORY})"
                ),
                [project.as_str()],
                |row| row.get(0),
            )
            .map_err(error)?;
        if waiting >= MESSAGES_WAITING_MAX {
            return Ok(Err("team_conversation_full"));
        }
    }
    make_room(
        tx,
        "team_messages",
        MESSAGE_HISTORY,
        project,
        MESSAGES_PER_PROJECT_MAX,
    )?;
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
        TeamAction::LiveMaster { on, .. } => {
            if *on
                && crate::harnesses::interactive::support(&stored.team.master)
                    != crate::harnesses::interactive::Support::Terminal
            {
                return Ok(rejected("team_master_no_terminal"));
            }
            master::set_live(tx, project, *on)?;
            Ok(Outcome::Applied)
        }
        TeamAction::Pause { .. } => {
            tx.execute(
                "INSERT INTO team_paused_projects(project,since_ms) VALUES (?1,?2)
                 ON CONFLICT(project) DO NOTHING",
                params![project.as_str(), now],
            )
            .map_err(error)?;
            Ok(Outcome::Applied)
        }
        TeamAction::Resume { .. } => {
            tx.execute(
                "DELETE FROM team_paused_projects WHERE project=?1",
                [project.as_str()],
            )
            .map_err(error)?;
            Ok(Outcome::Applied)
        }
        TeamAction::HandOff { reply, step, .. } => {
            let Some(text) = master_reply(tx, project, Some(*reply))?.map(|(_, text)| text) else {
                return Ok(rejected("team_plan_step_missing"));
            };
            let steps = plan_steps(tx, project, *reply, &text)?;
            let Some(found) = steps.get(usize::from(*step)) else {
                return Ok(rejected("team_plan_step_missing"));
            };
            if found.task.is_some() {
                return Ok(rejected("team_plan_step_handed"));
            }
            Ok(
                match admit(
                    tx,
                    access,
                    &stored,
                    found.harness.as_deref(),
                    &found.prompt,
                    TaskSource::Master,
                    Some(*reply),
                    now,
                )? {
                    Ok(_) => Outcome::Applied,
                    Err(code) => rejected(code),
                },
            )
        }
        TeamAction::Rename { agent, handle, .. } => {
            Ok(match names::rename(tx, project, *agent, handle)? {
                Ok(_) => Outcome::Applied,
                Err(code) => rejected(code),
            })
        }
    }
}

fn master_reply(
    connection: &Connection,
    project: &ProjectId,
    reply: Option<i64>,
) -> Result<Option<(i64, String)>, EngineError> {
    connection
        .query_row(
            "SELECT id,text FROM team_messages WHERE project=?1 AND author=?2 AND state=?3
             AND (?4 IS NULL OR id=?4) ORDER BY id DESC LIMIT 1",
            params![
                project.as_str(),
                MessageAuthor::Master.name(),
                MessageState::Reply.name(),
                reply
            ],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(error)
}

/// Lists the sub-agent tasks a master reply proposed, each with the task handed off for it.
/// Takes a connection, the project, the master reply id and its text.
/// Returns at most eight steps in reply order; equal prompts match tasks in creation order.
pub fn plan_steps(
    connection: &Connection,
    project: &ProjectId,
    reply: i64,
    text: &str,
) -> Result<Vec<PlanStep>, EngineError> {
    let mut statement = connection
        .prepare(
            "SELECT id,prompt FROM team_tasks WHERE project=?1 AND turn=?2 ORDER BY id LIMIT ?3",
        )
        .map_err(error)?;
    let mut tasks = statement
        .query_map(
            params![project.as_str(), reply, DELEGATIONS_PER_REPLY_MAX as i64],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
        )
        .map_err(error)?
        .map(|task| task.map(Some))
        .collect::<Result<Vec<_>, _>>()
        .map_err(error)?;
    let steps: Vec<PlanStep> = parse_delegations(text)
        .into_iter()
        .enumerate()
        .map(|(step, delegation)| PlanStep {
            reply,
            step: step as u8,
            task: tasks
                .iter_mut()
                .find(|task| {
                    task.as_ref()
                        .is_some_and(|(_, prompt)| *prompt == delegation.prompt)
                })
                .and_then(Option::take)
                .map(|(id, _)| id),
            harness: delegation.harness,
            prompt: delegation.prompt,
        })
        .collect();
    assert!(steps.len() <= DELEGATIONS_PER_REPLY_MAX);
    Ok(steps)
}

/// One team agent as the clients poll it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TeamAgent {
    pub id: i64,
    pub role: AgentRole,
    pub label: String,
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
    #[serde(default)]
    pub persona: Option<String>,
    #[serde(default)]
    pub handle: Option<String>,
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

/// One sub-agent task the master proposed in its latest reply, and the task handed off for it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanStep {
    pub reply: i64,
    pub step: u8,
    pub harness: Option<String>,
    pub prompt: String,
    pub task: Option<i64>,
}

/// Everything the clients show about a project's team.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TeamStatus {
    pub project_id: ProjectId,
    pub keep_warm: bool,
    pub paused: bool,
    pub min_agents: u8,
    pub max_agents: u8,
    pub harnesses: Vec<TeamHarness>,
    pub agents: Vec<TeamAgent>,
    pub tasks: Vec<TeamTask>,
    pub messages: Vec<TeamMessage>,
    pub plan: Vec<PlanStep>,
}

fn invalid_row(column: &str) -> rusqlite::Error {
    rusqlite::Error::InvalidColumnType(0, column.into(), rusqlite::types::Type::Text)
}

pub(crate) fn agent_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<TeamAgent> {
    let role: String = row.get(1)?;
    let status: String = row.get(6)?;
    let status = AgentStatus::parse(&status).ok_or_else(|| invalid_row("status"))?;
    let id: i64 = row.get(0)?;
    let role = AgentRole::parse(&role).ok_or_else(|| invalid_row("role"))?;
    let persona: Option<String> = row.get(14)?;
    let handle: Option<String> = row.get(15)?;
    let kept = persona.clone().map(|persona| names::AgentName {
        persona,
        handle: handle.clone(),
    });
    Ok(TeamAgent {
        id,
        role,
        label: names::label(role, id, kept.as_ref()),
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
        persona,
        handle,
    })
}

pub(crate) const AGENT_COLUMNS: &str = "id,role,harness,tools,connection,worktree,status,task,turns,error,process_group,created_ms,heartbeat_ms,stopped_ms,(SELECT persona FROM team_names WHERE team_names.agent=team_agents.id),(SELECT handle FROM team_names WHERE team_names.agent=team_agents.id)";
/// The team task columns the clients read, in `task_row` order, selected from `team_tasks`.
/// A task a project's conversation master handed off is stored as the owner's own (DEC-093),
/// so the team plan and history treat it like one, but reads as the master's, so the Agents
/// tab, Team view and TUI say it came from the master.
pub(crate) const TASK_COLUMNS: &str = "id,CASE WHEN EXISTS (SELECT 1 FROM team_conversation_tasks c WHERE c.task=team_tasks.id) THEN 'master' ELSE source END,harness,tools,prompt,status,agent,attempts,result,error,delivered,output_bytes,created_ms,started_ms,finished_ms";

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

/// Tells whether the owner paused the project's whole team.
/// Takes a connection and the project.
/// Returns true from a pause until the owner resumes the team.
pub fn paused(connection: &Connection, project: &ProjectId) -> Result<bool, EngineError> {
    connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM team_paused_projects WHERE project=?1)",
            [project.as_str()],
            |row| row.get(0),
        )
        .map_err(error)
}

/// Pauses a pool account that hit a rate or usage limit for as long as a team agent's
/// account pauses, in the pause every master, team and research turn honours. Takes a
/// connection and the account's connection. Returns when the pause ends, in ms since the
/// epoch; a longer pause already set is kept.
pub(crate) fn pause_account(
    connection: &Connection,
    account: &CredentialRef,
) -> rusqlite::Result<i64> {
    let pause = runtime::TeamLimits::default().rate_limit_pause;
    let until = now_ms().saturating_add(i64::try_from(pause.as_millis()).unwrap_or(i64::MAX));
    let stored: i64 = connection.query_row(
        "INSERT INTO team_paused_accounts(connection,until_ms) VALUES (?1,?2)
         ON CONFLICT(connection) DO UPDATE SET until_ms=MAX(until_ms,excluded.until_ms)
         RETURNING until_ms",
        params![account.as_str(), until],
        |row| row.get(0),
    )?;
    assert!(stored >= until);
    Ok(stored)
}

/// Tells until when a pool account stays paused after a limit. Takes a connection and the
/// account's connection. Returns the end of its pause in ms since the epoch, or `None`
/// when it is not paused now.
pub(crate) fn paused_until(
    connection: &Connection,
    account: &CredentialRef,
) -> rusqlite::Result<Option<i64>> {
    connection
        .query_row(
            "SELECT until_ms FROM team_paused_accounts WHERE connection=?1 AND until_ms>?2",
            params![account.as_str(), now_ms()],
            |row| row.get(0),
        )
        .optional()
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
    let master_path = delegation_path(&plan.master, plan.master_tools);
    let mut harnesses = vec![TeamHarness {
        harness: plan.master.clone(),
        role: AgentRole::Master,
        agents_max: 1,
        tools: plan.master_tools,
        live: live(AgentRole::Master, &plan.master),
        delegation: Some(master_path),
        delegation_label: Some(master_path.label().into()),
        runnable: launch_harness(&plan.master).is_some(),
    }];
    let slots = if plan.sub_harnesses.is_empty() {
        let subs_max = plan.max_agents.saturating_sub(1);
        runnable_harnesses()
            .filter(|_| subs_max > 0)
            .map(|harness| HarnessSlot {
                harness: harness.to_owned(),
                agents: subs_max,
                tools: plan.master_tools,
            })
            .collect()
    } else {
        plan.sub_harnesses.clone()
    };
    harnesses.extend(slots.into_iter().map(|slot| TeamHarness {
        live: live(AgentRole::Sub, &slot.harness),
        runnable: launch_harness(&slot.harness).is_some(),
        harness: slot.harness,
        role: AgentRole::Sub,
        agents_max: slot.agents,
        tools: slot.tools,
        delegation: None,
        delegation_label: None,
    }));
    let steps = match master_reply(connection, project, None)? {
        Some((reply, text)) => plan_steps(connection, project, reply, &text)?,
        None => Vec::new(),
    };
    assert!(agents.len() as i64 <= STATUS_AGENTS_MAX);
    Ok(TeamStatus {
        project_id: project.clone(),
        keep_warm: keep_warm(connection, project)?,
        paused: paused(connection, project)?,
        min_agents: plan.min_agents,
        max_agents: plan.max_agents,
        harnesses,
        agents,
        tasks,
        messages,
        plan: steps,
    })
}

#[cfg(all(test, unix))]
mod tests;
