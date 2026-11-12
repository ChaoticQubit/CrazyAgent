use crate::config::{ModelAccess, ModelAuthentication, ModelEndpoint};
use crate::engine::EngineError;
use crate::project_setup::{HarnessSlot, StoredSetup, TEAM_AGENTS_MAX, TeamPlan};
use crate::protocol::{CredentialRef, Outcome, ProjectId, RequestId, SessionId};
use crate::storage::versioned::{self, STALE_CODE};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub mod agent_messages;
#[cfg(unix)]
pub mod caller;
pub mod caps;
pub mod cleanup;
pub mod direct;
pub mod events;
pub mod graph;
pub mod history;
pub mod master;
pub mod names;
pub mod questions;
pub mod runtime;
pub mod screen_questions;
pub mod tool;
pub mod wall;

pub use graph::{
    EdgeState, GraphEdge, GraphFacts, GraphNode, MessageEdge, NodeKind, OverlapEdge, TeamGraph,
    graph,
};

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
pub const STATUS_REPLY_BYTES_MAX: usize = crate::protocol::FRAME_BYTES_MAX - 4096;
const STATUS_TEXT_CAPS: [usize; 8] = [8192, 4096, 2048, 1024, 512, 256, 128, 64];
pub const WAKE_NAME_BYTES_MAX: usize = 64;
pub const DIRECT_INPUT_BYTES_MAX: usize = 4096;
const NATIVE_DEFAULT_MODEL: &str = "native-default";
/// The task and session time limits the owner may choose, in seconds: 30 min, 1 h, 2 h,
/// 4 h and 8 h. No limit is the default and is stored as none.
pub const LIMIT_CHOICES_SECS: [u32; 5] = [1800, 3600, 7200, 14_400, 28_800];
/// How long an idle sub-agent may wait for work before it stops, in seconds: 2 min, 10 min
/// and 1 h. Never stopping is the other choice, stored as zero.
pub const IDLE_CHOICES_SECS: [u32; 3] = [120, 600, 3600];
/// How long the owner's 15 min choice keeps an idle master once its work is done, in seconds:
/// the engine's idle bound, of which the 1 h choice counts four. It is never stopped while
/// that work still runs.
pub const MASTER_IDLE_STOP_SECS: u32 = 900;

/// How many of the engine's master idle bounds the 1 h choice keeps a master for.
const MASTER_IDLE_HOUR_BOUNDS: u32 = 4;
const _: () = assert!(MASTER_IDLE_STOP_SECS == 900);
const _: () = assert!(MASTER_IDLE_STOP_SECS * MASTER_IDLE_HOUR_BOUNDS == 3600);
/// The longest Master's branch name the owner may choose, in bytes.
pub const MASTER_BRANCH_BYTES_MAX: usize = 255;
/// The idle stop a team starts with: 10 minutes.
pub const IDLE_STOP_DEFAULT_SECS: u32 = 600;

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
    Teammate,
}

impl AgentRole {
    fn name(self) -> &'static str {
        match self {
            Self::Master => "master",
            Self::Sub => "sub",
            Self::Teammate => "teammate",
        }
    }

    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value {
            "master" => Some(Self::Master),
            "sub" => Some(Self::Sub),
            "teammate" => Some(Self::Teammate),
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
            Self::Teammate => format!("Teammate {id}"),
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

    pub(crate) fn parse(value: &str) -> Option<Self> {
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
                "Writes a crazyagent-task block in its reply; the engine checks the team plan and creates the tasks"
            }
            Self::Tool => {
                "Calls the engine's team tools; the engine checks the team plan and creates the tasks"
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

/// What the Coordination rows of a harness kind no sub-agent runs on say instead of a live
/// control (DEC-141 PA-15), word for word in the TUI and the desktop (PALOW-L2), short enough
/// for a TUI row at 80 columns.
pub const NO_SUB_AGENTS: &str = "Not available: runs only as a master";

/// Tells why no sub-agent runs on a harness kind: a kind no launch adapter starts as a team
/// agent, today rcli, keeps its Coordination rows, which say so (DEC-141 PA-15).
/// Input: the harness kind. Output: [`NO_SUB_AGENTS`], or none for a kind sub-agents run on.
pub fn sub_agents_unavailable(kind: crate::coord::settings::HarnessKind) -> Option<&'static str> {
    match launch_harness(kind.as_str()) {
        Some(_) => None,
        None => Some(NO_SUB_AGENTS),
    }
}

/// Maps a team plan master harness to the backend that runs the project's master on it.
/// Takes the harness identifier or a connection name for it. Returns Codex, Claude or
/// RCLI, the only harnesses a master runs on, or none for any other harness.
pub fn master_backend(harness: &str) -> Option<crate::master_session::MasterBackend> {
    use crate::master_session::MasterBackend;
    match crate::harnesses::spec(harness)?.id {
        "codex" => Some(MasterBackend::Codex),
        "claude-code" => Some(MasterBackend::Claude),
        "rcli" => Some(MasterBackend::Rcli),
        _ => None,
    }
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

/// The owner's setting for a master harness switch: `Carry` continues the session's
/// conversation on the new harness with a bounded handoff of its earlier turns and open
/// tasks, `Fresh` starts the new harness's conversation without them. Carry is the default.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SwitchMode {
    #[default]
    Carry,
    Fresh,
}

impl SwitchMode {
    /// The stored name of the mode. Output: `carry` or `fresh`.
    pub fn name(self) -> &'static str {
        match self {
            Self::Carry => "carry",
            Self::Fresh => "fresh",
        }
    }

    /// Reads a stored mode name. Input: the name. Output: the mode, or None.
    pub fn named(name: &str) -> Option<Self> {
        [Self::Carry, Self::Fresh]
            .into_iter()
            .find(|mode| mode.name() == name)
    }
}

/// When a saved master harness change reaches the running live master: `WhenIdle`, the
/// default, switches it once it is idle and an account of its pool can run the new harness;
/// `NextStart` keeps it on its harness until it starts again.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SwitchApply {
    #[default]
    WhenIdle,
    NextStart,
}

impl SwitchApply {
    /// The stored name of the choice. Output: `when_idle` or `next_start`.
    pub fn name(self) -> &'static str {
        match self {
            Self::WhenIdle => "when_idle",
            Self::NextStart => "next_start",
        }
    }

    /// Reads a stored choice name. Input: the name. Output: the choice, or None.
    pub fn named(name: &str) -> Option<Self> {
        [Self::WhenIdle, Self::NextStart]
            .into_iter()
            .find(|apply| apply.name() == name)
    }
}

/// The owner's Max harnesses per project and Max teammates per harness choices besides No
/// limit, the default. The largest is the most agents one project runs at once.
pub const CAP_CHOICES: [u8; 9] = [1, 2, 3, 4, 5, 6, 8, 10, 15];
const _: () = assert!(CAP_CHOICES[8] as usize == crate::terminal::PANES_PER_PROJECT_MAX);

/// The owner's Token budget choices besides Off, the default: how many tokens a project's
/// agents may use, counted as the Usage page's headline (new input, cache writes and
/// output; re-reads from cache are not counted), from when the
/// owner set it or last started the team again, before the engine stops the whole team.
pub const BUDGET_CHOICES: [u64; 7] = [
    10_000_000,
    25_000_000,
    50_000_000,
    100_000_000,
    250_000_000,
    500_000_000,
    1_000_000_000,
];

/// The most teammates of one harness its status lists: running ones first, then the newest
/// that ended.
pub const TEAMMATES_SHOWN_MAX: usize = 16;
const _: () = assert!(TEAMMATES_SHOWN_MAX <= crate::terminal::PANES_LISTED_MAX);

pub use caps::Caps;
pub use wall::{TerminalsPerWindow, terminals_per_window};

/// A project's token budget: the tokens its agents may use, the tokens they used since the
/// owner set it or last started the team again, and whether reaching it stopped the team.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Budget {
    pub tokens: u64,
    pub used: u64,
    pub reached: bool,
}

/// The owner's Sub-agent depth choices: 1 lets only the master start sub-agents, 2 lets
/// the master's sub-agents lead workers of their own, 3 lets those workers lead too.
pub const DEPTH_CHOICES: [u8; 3] = [1, 2, 3];
/// The Sub-agent depth a project has until the owner picks another.
pub const DEPTH_DEFAULT: u8 = 2;
const _: () = assert!(DEPTH_DEFAULT >= DEPTH_CHOICES[0] && DEPTH_DEFAULT <= DEPTH_CHOICES[2]);
/// The choices for the most files one new top-level folder, a folder the owner's branch
/// does not have, may gain in a change set before the engine refuses the set (DEC-107).
pub const NEW_FOLDER_FILES_CHOICES: [u32; 4] = [100, 1_000, 10_000, 100_000];
/// The new-folder limit a project has until the owner picks another.
pub const NEW_FOLDER_FILES_DEFAULT: u32 = 1_000;
const _: () = assert!(NEW_FOLDER_FILES_DEFAULT == NEW_FOLDER_FILES_CHOICES[1]);

/// The owner's setting for where a lead's worker lands its change set: `Lead` lets the lead
/// accept it onto the lead's own branch, `Owner` keeps every change set for the owner's own
/// Accept in Changes. Lead is the default.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChildLanding {
    #[default]
    Lead,
    Owner,
}

impl ChildLanding {
    /// The stored name of the setting. Output: `lead` or `owner`.
    pub fn name(self) -> &'static str {
        match self {
            Self::Lead => "lead",
            Self::Owner => "owner",
        }
    }

    /// Reads a stored setting name. Input: the name. Output: the setting, or None.
    pub fn named(name: &str) -> Option<Self> {
        [Self::Lead, Self::Owner]
            .into_iter()
            .find(|mode| mode.name() == name)
    }
}

/// The owner's setting for where a new sub-agent commits: `Named` checks out the engine's
/// branch `crazyagent/agent-<id>` in the agent's worktree, so each agent works on its own
/// branch; `Detached` keeps the older detached HEAD. Named is the default.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BranchMode {
    #[default]
    Named,
    Detached,
}

impl BranchMode {
    /// The stored name of the mode. Output: `named` or `detached`.
    pub fn name(self) -> &'static str {
        match self {
            Self::Named => "named",
            Self::Detached => "detached",
        }
    }

    /// Reads a stored mode name. Input: the name. Output: the mode, or None.
    pub fn named(name: &str) -> Option<Self> {
        [Self::Named, Self::Detached]
            .into_iter()
            .find(|mode| mode.name() == name)
    }
}

/// What a sub-agent's task does when its turn ends while a command its agent started in
/// the background still runs (DEC-111 Q13): `Wait` keeps the task open until the command
/// ends, bounded by the task time limit; `Grace` waits at most `secs` seconds, then stops
/// it; `Stop` ends the task with its turn and stops it, and the agent's task prompt says
/// so wherever the engine stops it. A grace time of [`BACKGROUND_GRACE_SECS_DEFAULT`] is
/// the default (DEC-119).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum BackgroundAtEnd {
    Wait,
    Grace { secs: u32 },
    Stop,
}

/// The grace times, in seconds, the owner may give background commands at task end.
pub const BACKGROUND_GRACE_SECS: std::ops::RangeInclusive<u32> = 10..=3600;

/// The default grace time, in seconds, also offered when the owner first picks a grace
/// time: 30 minutes (DEC-119).
pub const BACKGROUND_GRACE_SECS_DEFAULT: u32 = 1800;

/// The grace times, in seconds, a client steps through, each inside
/// [`BACKGROUND_GRACE_SECS`].
pub const BACKGROUND_GRACE_CHOICES_SECS: [u32; 8] = [10, 30, 60, 120, 300, 600, 1800, 3600];
const _: () = assert!(BACKGROUND_GRACE_SECS_DEFAULT == BACKGROUND_GRACE_CHOICES_SECS[6]);

impl Default for BackgroundAtEnd {
    fn default() -> Self {
        Self::Grace {
            secs: BACKGROUND_GRACE_SECS_DEFAULT,
        }
    }
}

impl BackgroundAtEnd {
    /// The stored mode name and grace time. Output: `wait`, `grace` with its seconds, or
    /// `stop`, each without seconds but grace.
    pub fn stored(self) -> (&'static str, Option<u32>) {
        match self {
            Self::Wait => ("wait", None),
            Self::Grace { secs } => ("grace", Some(secs)),
            Self::Stop => ("stop", None),
        }
    }

    /// Reads a stored mode and grace time. Inputs: the name and the seconds. Output: the
    /// choice, or None for a pair it never stores or a grace time outside
    /// [`BACKGROUND_GRACE_SECS`].
    pub fn named(name: &str, secs: Option<u32>) -> Option<Self> {
        match (name, secs) {
            ("wait", None) => Some(Self::Wait),
            ("grace", Some(secs)) if BACKGROUND_GRACE_SECS.contains(&secs) => {
                Some(Self::Grace { secs })
            }
            ("stop", None) => Some(Self::Stop),
            _ => None,
        }
    }
}

/// A project's nested sub-agent settings: how deep leads may start workers and where a
/// worker's change set lands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Nesting {
    pub depth: u8,
    pub lands: ChildLanding,
}

/// The latest master harness switch of the project's live session, for the divider its
/// conversation shows: the harness's display name, what it was given under the owner's
/// setting and the note that says so.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionSwitch {
    pub harness: String,
    pub mode: SwitchMode,
    pub note: String,
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
        version: u64,
    },
    TimeLimits {
        project_id: ProjectId,
        turn_secs: Option<u32>,
        session_secs: Option<u32>,
        idle_secs: Option<u32>,
        version: u64,
    },
    OnHarnessSwitch {
        project_id: ProjectId,
        mode: SwitchMode,
        version: u64,
    },
    ApplyHarnessSwitch {
        project_id: ProjectId,
        mode: SwitchApply,
        version: u64,
    },
    SubAgentDepth {
        project_id: ProjectId,
        depth: u8,
    },
    ChildLanding {
        project_id: ProjectId,
        mode: ChildLanding,
    },
    LiveMaster {
        project_id: ProjectId,
        on: bool,
        version: u64,
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
    Wake {
        target: events::RuleTarget,
        rule: Option<events::WakeRule>,
        version: u64,
    },
    Seen {
        project_id: ProjectId,
        upto: i64,
    },
    Rename {
        project_id: ProjectId,
        agent: i64,
        handle: String,
    },
    FinishedAgents {
        project_id: ProjectId,
        show: bool,
        version: u64,
    },
    AgentBranches {
        project_id: ProjectId,
        mode: BranchMode,
    },
    MasterBranch {
        project_id: ProjectId,
        #[serde(default)]
        branch: Option<String>,
    },
    LandOn {
        project_id: ProjectId,
        #[serde(default)]
        branch: Option<String>,
    },
    BackgroundAtEnd {
        project_id: ProjectId,
        choice: BackgroundAtEnd,
    },
    AskBeforeMerge {
        project_id: ProjectId,
        on: bool,
    },
    MasterLands {
        project_id: ProjectId,
        mode: MasterLands,
    },
    ReaskEmptyReply {
        project_id: ProjectId,
        on: bool,
    },
    MasterIdleStop {
        project_id: ProjectId,
        choice: MasterIdleStop,
    },
    MasterLiveTerminal {
        project_id: ProjectId,
        on: bool,
    },
    CatchUpMain {
        project_id: ProjectId,
        mode: CatchUpMain,
    },
    ReuseIdle {
        project_id: ProjectId,
        mode: ReuseIdle,
    },
    LandingMode {
        project_id: ProjectId,
        mode: LandingMode,
    },
    NewFolderLimit {
        project_id: ProjectId,
        files: u32,
    },
    TerminalsPerWindow {
        project_id: ProjectId,
        per_window: TerminalsPerWindow,
    },
    WakeAgent {
        project_id: ProjectId,
        agent: i64,
    },
    MessageSettings {
        project_id: ProjectId,
        settings: agent_messages::MessageSettings,
    },
    StopAgent {
        project_id: ProjectId,
        agent: i64,
    },
    StopTeammate {
        project_id: ProjectId,
        agent: i64,
        pane: String,
    },
    MessageAgent {
        project_id: ProjectId,
        agent: i64,
        text: String,
    },
    MaxHarnesses {
        project_id: ProjectId,
        max: Option<u8>,
    },
    MaxTeammates {
        project_id: ProjectId,
        max: Option<u8>,
    },
    TokenBudget {
        project_id: ProjectId,
        tokens: Option<u64>,
    },
    AnswerQuestion {
        project_id: ProjectId,
        message: i64,
        text: String,
    },
    SpreadAccounts {
        project_id: ProjectId,
        on: bool,
    },
    CoordSettings {
        project_id: ProjectId,
        settings: crate::coord::settings::CoordSettings,
    },
    HarnessMode {
        project_id: ProjectId,
        harness: crate::coord::settings::HarnessKind,
        mode: crate::coord::settings::SubagentMode,
    },
    HarnessDelivery {
        project_id: ProjectId,
        harness: crate::coord::settings::HarnessKind,
        delivery: crate::coord::settings::Delivery,
    },
}

/// One harness kind's coordination choices as the team settings show them: how its sub-agents
/// start, how messages reach its agents, whether it has native teammates at all, and the
/// terminal displays its terminals can draw, so the one Terminal display setting names a kind
/// that stays as it is (DEC-141). An engine from before DEC-141 sends no displays, which
/// reads as an empty list: nothing known, so no kind is named.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HarnessCoordination {
    pub harness: crate::coord::settings::HarnessKind,
    pub mode: crate::coord::settings::SubagentMode,
    pub delivery: crate::coord::settings::Delivery,
    pub native: bool,
    #[serde(default)]
    pub displays: Vec<crate::coord::settings::TerminalDisplay>,
}

/// A project's coordination settings (DEC-104) with every harness kind's mode and delivery,
/// and its heavy lane: the heavy commands that run and wait (F28). An engine from before F28
/// sends no lane, which reads as empty.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoordinationView {
    pub settings: crate::coord::settings::CoordSettings,
    pub harnesses: Vec<HarnessCoordination>,
    #[serde(default)]
    pub lane: crate::coord::lane::LaneView,
}

/// Reads a project's coordination settings, each harness kind's sub-agent mode and delivery,
/// with the plan defaults for anything the owner never set, and its heavy lane (F28).
/// Takes a connection and the project. Returns the view, one row per harness kind.
pub fn coordination(
    connection: &Connection,
    project: &ProjectId,
) -> Result<CoordinationView, EngineError> {
    use crate::coord::settings;
    let mut harnesses = Vec::with_capacity(settings::HarnessKind::ALL.len());
    for harness in settings::HarnessKind::ALL {
        harnesses.push(HarnessCoordination {
            harness,
            mode: settings::mode(connection, project, harness)?,
            delivery: settings::delivery(connection, project, harness)?,
            native: settings::native_supported(harness),
            displays: crate::harnesses::interactive::displays(harness),
        });
    }
    let view = CoordinationView {
        settings: settings::read(connection, project)?,
        harnesses,
        lane: crate::coord::lane::view(connection, project)?,
    };
    assert!(
        view.harnesses
            .iter()
            .all(|row| row.native || row.mode != settings::SubagentMode::Native)
    );
    Ok(view)
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
            Self::Wake { target, rule, .. } => {
                if rule.is_some_and(|rule| rule.hourly_cap > events::HOURLY_CAP_MAX) {
                    return Err(failure("team_wake_cap_invalid"));
                }
                if matches!(target, events::RuleTarget::Agent { agent, .. } if *agent <= 0) {
                    return Err(failure("team_agent_not_found"));
                }
                Ok(())
            }
            Self::Seen { upto, .. } => {
                if *upto <= 0 {
                    return Err(failure("team_notification_missing"));
                }
                Ok(())
            }
            Self::Rename { agent, handle, .. } => {
                if *agent <= 0 || handle.len() > names::HANDLE_CHARS_MAX + 1 {
                    return Err(failure("team_handle_invalid"));
                }
                Ok(())
            }
            Self::TimeLimits {
                turn_secs,
                session_secs,
                idle_secs,
                ..
            } => {
                let choice =
                    |secs: &Option<u32>| secs.is_none_or(|secs| LIMIT_CHOICES_SECS.contains(&secs));
                let idle = idle_secs.is_none_or(|secs| IDLE_CHOICES_SECS.contains(&secs));
                let ordered = match (turn_secs, session_secs) {
                    (Some(turn), Some(session)) => session >= turn,
                    _ => true,
                };
                (choice(turn_secs) && choice(session_secs) && idle && ordered)
                    .then_some(())
                    .ok_or_else(|| failure("team_time_limits_invalid"))
            }
            Self::WakeAgent { agent, .. } | Self::StopAgent { agent, .. } => {
                if *agent <= 0 {
                    return Err(failure("team_agent_not_found"));
                }
                Ok(())
            }
            Self::CoordSettings { settings, .. } => settings.validate(),
            Self::HarnessMode { harness, mode, .. } => (*mode
                != crate::coord::settings::SubagentMode::Native
                || crate::coord::settings::native_supported(*harness))
            .then_some(())
            .ok_or_else(|| failure("coord_native_unsupported")),
            Self::StopTeammate { agent, pane, .. } => (*agent > 0
                && crate::terminal::pane_id_valid(pane))
            .then_some(())
            .ok_or_else(|| failure("team_teammate_not_found")),
            Self::MessageAgent { agent, text, .. } => (*agent > 0
                && text_valid(text, crate::coord::BODY_BYTES_MAX))
            .then_some(())
            .ok_or_else(|| failure("team_agent_message_invalid")),
            Self::MaxHarnesses { max, .. } | Self::MaxTeammates { max, .. } => max
                .is_none_or(|max| CAP_CHOICES.contains(&max))
                .then_some(())
                .ok_or_else(|| failure("team_cap_invalid")),
            Self::TokenBudget { tokens, .. } => tokens
                .is_none_or(|tokens| BUDGET_CHOICES.contains(&tokens))
                .then_some(())
                .ok_or_else(|| failure("team_budget_invalid")),
            Self::SubAgentDepth { depth, .. } => DEPTH_CHOICES
                .contains(depth)
                .then_some(())
                .ok_or_else(|| failure("team_depth_invalid")),
            Self::NewFolderLimit { files, .. } => NEW_FOLDER_FILES_CHOICES
                .contains(files)
                .then_some(())
                .ok_or_else(|| failure("team_new_folder_limit_invalid")),
            Self::MasterBranch { branch, .. } => branch
                .as_deref()
                .is_none_or(plain_branch_name)
                .then_some(())
                .ok_or_else(|| failure("team_master_branch_invalid")),
            Self::LandOn { branch, .. } => branch
                .as_deref()
                .is_none_or(plain_branch_name)
                .then_some(())
                .ok_or_else(|| failure("team_land_on_invalid")),
            Self::BackgroundAtEnd { choice, .. } => match choice {
                BackgroundAtEnd::Grace { secs } if !BACKGROUND_GRACE_SECS.contains(secs) => {
                    Err(failure("team_background_grace_invalid"))
                }
                _ => Ok(()),
            },
            Self::AnswerQuestion { message, text, .. } => {
                if *message <= 0 {
                    return Err(failure("coord_question_missing"));
                }
                text_valid(text, crate::coord::BODY_BYTES_MAX)
                    .then_some(())
                    .ok_or_else(|| failure("team_answer_invalid"))
            }
            Self::MessageSettings { settings, .. } => settings.validate(),
            Self::KeepWarm { .. }
            | Self::SpreadAccounts { .. }
            | Self::ChildLanding { .. }
            | Self::OnHarnessSwitch { .. }
            | Self::ApplyHarnessSwitch { .. }
            | Self::FinishedAgents { .. }
            | Self::AgentBranches { .. }
            | Self::AskBeforeMerge { .. }
            | Self::MasterLands { .. }
            | Self::ReaskEmptyReply { .. }
            | Self::MasterIdleStop { .. }
            | Self::MasterLiveTerminal { .. }
            | Self::TerminalsPerWindow { .. }
            | Self::CatchUpMain { .. }
            | Self::ReuseIdle { .. }
            | Self::LandingMode { .. }
            | Self::LiveMaster { .. }
            | Self::HarnessDelivery { .. }
            | Self::Pause { .. }
            | Self::Resume { .. } => Ok(()),
        }
    }

    /// Names the project the action is for.
    /// Takes the action.
    /// Returns its project id, or none for the app-wide wake rule.
    pub fn project(&self) -> Option<&ProjectId> {
        match self {
            Self::Message { project_id, .. }
            | Self::Delegate { project_id, .. }
            | Self::Cancel { project_id, .. }
            | Self::KeepWarm { project_id, .. }
            | Self::TimeLimits { project_id, .. }
            | Self::OnHarnessSwitch { project_id, .. }
            | Self::ApplyHarnessSwitch { project_id, .. }
            | Self::SubAgentDepth { project_id, .. }
            | Self::ChildLanding { project_id, .. }
            | Self::FinishedAgents { project_id, .. }
            | Self::AgentBranches { project_id, .. }
            | Self::MasterBranch { project_id, .. }
            | Self::LandOn { project_id, .. }
            | Self::BackgroundAtEnd { project_id, .. }
            | Self::AskBeforeMerge { project_id, .. }
            | Self::MasterLands { project_id, .. }
            | Self::ReaskEmptyReply { project_id, .. }
            | Self::MasterIdleStop { project_id, .. }
            | Self::MasterLiveTerminal { project_id, .. }
            | Self::CatchUpMain { project_id, .. }
            | Self::ReuseIdle { project_id, .. }
            | Self::LandingMode { project_id, .. }
            | Self::NewFolderLimit { project_id, .. }
            | Self::TerminalsPerWindow { project_id, .. }
            | Self::MessageSettings { project_id, .. }
            | Self::LiveMaster { project_id, .. }
            | Self::Pause { project_id }
            | Self::Resume { project_id }
            | Self::HandOff { project_id, .. }
            | Self::Seen { project_id, .. } => Some(project_id),
            Self::Wake { target, .. } => match target {
                events::RuleTarget::App => None,
                events::RuleTarget::Project { project_id }
                | events::RuleTarget::Agent { project_id, .. } => Some(project_id),
            },
            Self::Rename { project_id, .. }
            | Self::WakeAgent { project_id, .. }
            | Self::StopAgent { project_id, .. }
            | Self::StopTeammate { project_id, .. }
            | Self::MessageAgent { project_id, .. }
            | Self::MaxHarnesses { project_id, .. }
            | Self::MaxTeammates { project_id, .. }
            | Self::TokenBudget { project_id, .. }
            | Self::AnswerQuestion { project_id, .. }
            | Self::SpreadAccounts { project_id, .. }
            | Self::CoordSettings { project_id, .. }
            | Self::HarnessMode { project_id, .. }
            | Self::HarnessDelivery { project_id, .. } => Some(project_id),
        }
    }
}

/// Tells whether a branch name the owner typed for the Master's branch or Land on is one
/// git can hold as a local branch: printable ASCII without spaces or git's special characters, no `..`,
/// `@{`, `//` or leading `-`, no part starting with `.` or ending in `.lock`, at most
/// `MASTER_BRANCH_BYTES_MAX` bytes. Whether that branch exists is checked when it is used.
/// Input: the name. Output: the answer.
fn plain_branch_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MASTER_BRANCH_BYTES_MAX
        && !name.starts_with('-')
        && !name.starts_with('/')
        && !name.ends_with('/')
        && !name.ends_with('.')
        && !name.contains("..")
        && !name.contains("@{")
        && !name.contains("//")
        && name != "@"
        && name
            .split('/')
            .all(|part| !part.starts_with('.') && !part.ends_with(".lock"))
        && name.bytes().all(|byte| {
            byte.is_ascii_graphic()
                && !matches!(byte, b'~' | b'^' | b':' | b'?' | b'*' | b'[' | b'\\')
        })
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

/// How a team agent reaches its model: an HTTP endpoint, with the stored key it needs if
/// any (only ever reached through the engine's key relay), or a native managed profile the
/// owner signed in, which the engine lends to one agent turn at a time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TeamReach {
    Endpoint {
        base_url: String,
        model: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        key: Option<CredentialRef>,
    },
    Managed {
        profile_ref: CredentialRef,
        model: Option<String>,
    },
}

/// Tells how an agent of the launch harness reaches its model through one connection, by
/// the one account rule every harness shares (DEC-141 PA-4): an endpoint whose wire format
/// the harness speaks, with its key reference when it has one, or a native managed profile
/// of that harness. Takes the connection and the launch harness identifier. Returns the
/// reach, or none when the harness runs no team agents or cannot run on that connection.
fn reach(connection: &crate::config::ModelConnection, harness: &str) -> Option<TeamReach> {
    let harness = launch_harness(harness)?;
    if !crate::harnesses::account_fits(harness, connection) {
        return None;
    }
    match (&connection.endpoint, &connection.auth) {
        (ModelEndpoint::Http { url }, auth) => Some(TeamReach::Endpoint {
            base_url: crate::harnesses::endpoint_base_url(harness, url).to_owned(),
            model: connection.model.id.clone(),
            key: match auth {
                ModelAuthentication::ApiKey { credential_ref } => Some(credential_ref.clone()),
                _ => None,
            },
        }),
        (ModelEndpoint::Native {}, ModelAuthentication::NativeManaged { profile_ref }) => {
            Some(TeamReach::Managed {
                profile_ref: profile_ref.clone(),
                model: (connection.model.id != NATIVE_DEFAULT_MODEL)
                    .then(|| connection.model.id.clone()),
            })
        }
        _ => None,
    }
}

/// The accounts a harness may use for one project, or why there are none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Accounts {
    NoPool,
    Usable(Vec<TeamAccount>),
}

/// Lists pool accounts the launch harness can run on for one project: endpoints, keyed or
/// not, whose wire format it speaks and native managed profiles of that harness, signed in
/// or not.
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
            if let Some(reach) = reach(connection, harness) {
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
    pick_account_held(accounts, load, paused, choice, None)
}

/// Picks the usable account as `pick_account` does, for work that may already run on one
/// of them: the held account keeps the work unless it cannot take work or the schedule puts
/// the top account clearly before it (`Choice::clearly_before`: a pin, a limit, the
/// strategy's order with its margin under Balanced, or a rank), so near-equal usage
/// readings never move running work back and forth. Takes the usable accounts in pool
/// order, live agents per connection, paused connections, the scheduling choice and the
/// held connection, none for new work. Returns the chosen account, or none when every
/// account is paused or left out by the schedule.
pub fn pick_account_held<'a>(
    accounts: &'a [TeamAccount],
    load: &dyn Fn(&CredentialRef) -> usize,
    paused: &BTreeSet<CredentialRef>,
    choice: &crate::scheduling::Choice,
    held: Option<&CredentialRef>,
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
            current: held == Some(&account.connection),
        })
        .collect();
    let ready = choice.order(&candidates).ready;
    let top = *ready.first()?;
    let stays = ready
        .iter()
        .copied()
        .find(|index| candidates[*index].current)
        .filter(|kept| !choice.clearly_before(&candidates[top], &candidates[*kept]));
    Some(open[stays.unwrap_or(top)])
}

/// The accounts a pick looks at first under Spread harnesses across accounts: every usable
/// account no other agent of the project runs on, and the held account even when another
/// agent shares it, so `pick_account_held` weighs work that already runs against the unused
/// accounts and it never leaves a shared account for one that is not clearly before it.
/// Takes the usable accounts in pool order, the connections other agents of the project
/// run on and the held connection, none for new work. Returns those accounts in pool order.
pub fn spread_candidates(
    accounts: &[TeamAccount],
    used: &BTreeSet<&CredentialRef>,
    held: Option<&CredentialRef>,
) -> Vec<TeamAccount> {
    let apart: Vec<TeamAccount> = accounts
        .iter()
        .filter(|account| {
            let connection = &account.connection;
            !used.contains(connection) || held == Some(connection)
        })
        .cloned()
        .collect();
    assert!(apart.len() <= accounts.len());
    apart
}

/// Tells whether a master conversation kept while the master ran on `kept` may resume on the
/// `picked` account: always on the same account, and across accounts only from one native
/// subscription in the pool to another, never to or from an endpoint whose model may not take
/// that history. Takes the pool's usable accounts for the harness, the picked account and the
/// kept connection. Returns the answer.
pub fn resumes_on(pool: &[TeamAccount], picked: &TeamAccount, kept: &str) -> bool {
    let subscription = |account: &TeamAccount| matches!(account.reach, TeamReach::Managed { .. });
    picked.connection.as_str() == kept
        || (subscription(picked)
            && pool
                .iter()
                .any(|account| account.connection.as_str() == kept && subscription(account)))
}

/// One task the master asked for with its `delegate` tool or a `crazyagent-task` block (L6),
/// with the role handle it gave the job, if any.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Delegation {
    pub harness: Option<String>,
    pub prompt: String,
    #[serde(default)]
    pub role: Option<String>,
}

/// One finished sub-agent a master that is not live asked to wake (WK-R5), with its `wake`
/// tool or a `crazyagent-task` block (L6): the name or number it gave and the text for the
/// agent's next task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WakeLine {
    pub agent: String,
    pub text: String,
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
                stopped_ms INTEGER,
                session TEXT,
                parent INTEGER,
                pane TEXT,
                mode TEXT,
                hookless INTEGER NOT NULL DEFAULT 0,
                pane_pid INTEGER
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
                finished_ms INTEGER,
                session TEXT,
                parent INTEGER,
                mode TEXT
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
            CREATE TABLE IF NOT EXISTS team_time_limits (
                project TEXT PRIMARY KEY,
                turn_secs INTEGER,
                session_secs INTEGER,
                idle_secs INTEGER NOT NULL DEFAULT 600
            );
            CREATE TABLE IF NOT EXISTS team_nesting (
                project TEXT PRIMARY KEY,
                depth INTEGER NOT NULL CHECK(depth BETWEEN 1 AND 3),
                lands TEXT NOT NULL CHECK(lands IN ('lead','owner'))
            );
            CREATE TABLE IF NOT EXISTS team_new_folder_limit (
                project TEXT PRIMARY KEY,
                files INTEGER NOT NULL CHECK(files > 0)
            );
            CREATE TABLE IF NOT EXISTS team_master_switch (
                project TEXT PRIMARY KEY,
                mode TEXT NOT NULL CHECK(mode IN ('carry','fresh'))
            );
            CREATE TABLE IF NOT EXISTS team_master_apply (
                project TEXT PRIMARY KEY,
                mode TEXT NOT NULL CHECK(mode IN ('when_idle','next_start'))
            );
            CREATE TABLE IF NOT EXISTS team_finished_view (
                project TEXT PRIMARY KEY,
                show INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS team_agent_branches (
                project TEXT PRIMARY KEY,
                mode TEXT NOT NULL CHECK(mode IN ('named','detached'))
            );
            CREATE TABLE IF NOT EXISTS team_master_branch (
                project TEXT PRIMARY KEY,
                branch TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS team_land_on (
                project TEXT PRIMARY KEY,
                branch TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS team_background (
                project TEXT PRIMARY KEY,
                mode TEXT NOT NULL CHECK(mode IN ('wait','grace','stop')),
                grace_secs INTEGER CHECK((mode = 'grace') = (grace_secs IS NOT NULL))
            );
            CREATE TABLE IF NOT EXISTS team_merge_ask (
                project TEXT PRIMARY KEY,
                ask INTEGER NOT NULL CHECK(ask IN (0,1))
            );
            CREATE TABLE IF NOT EXISTS team_master_lands (
                project TEXT PRIMARY KEY,
                mode TEXT NOT NULL CHECK(mode IN ('merges','waits'))
            );
            CREATE TABLE IF NOT EXISTS team_reask_empty (
                project TEXT PRIMARY KEY,
                reask INTEGER NOT NULL CHECK(reask IN (0,1))
            );
            CREATE TABLE IF NOT EXISTS team_master_idle_stop (
                project TEXT PRIMARY KEY,
                choice TEXT NOT NULL CHECK(choice IN ('off','fifteen_minutes','one_hour'))
            );
            CREATE TABLE IF NOT EXISTS team_wall (
                project TEXT PRIMARY KEY,
                per_window INTEGER NOT NULL CHECK(per_window IN (4,6,9))
            );
            CREATE TABLE IF NOT EXISTS team_catch_up_main (
                project TEXT PRIMARY KEY,
                mode TEXT NOT NULL CHECK(mode IN ('on','off'))
            );
            CREATE TABLE IF NOT EXISTS team_reuse_idle (
                project TEXT PRIMARY KEY,
                mode TEXT NOT NULL CHECK(mode IN ('on','off'))
            );
            CREATE TABLE IF NOT EXISTS team_landing_mode (
                project TEXT PRIMARY KEY,
                mode TEXT NOT NULL CHECK(mode IN ('squash','merge'))
            );
            CREATE TABLE IF NOT EXISTS team_session_switches (
                session TEXT PRIMARY KEY,
                project TEXT NOT NULL,
                harness TEXT NOT NULL,
                mode TEXT NOT NULL CHECK(mode IN ('carry','fresh')),
                created_ms INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS team_paused_accounts (
                connection TEXT PRIMARY KEY,
                until_ms INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS team_project_pauses (
                project TEXT NOT NULL,
                connection TEXT NOT NULL,
                until_ms INTEGER NOT NULL,
                PRIMARY KEY(project, connection)
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
                agent INTEGER NOT NULL,
                held INTEGER NOT NULL DEFAULT 0
            );
            CREATE TABLE IF NOT EXISTS team_worktrees (
                agent INTEGER PRIMARY KEY,
                project TEXT NOT NULL,
                repository TEXT NOT NULL,
                worktree TEXT NOT NULL,
                git_dir TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS team_wake_requests (
                agent INTEGER PRIMARY KEY,
                project TEXT NOT NULL,
                requested_ms INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS team_owner_stops (
                agent INTEGER PRIMARY KEY,
                project TEXT NOT NULL,
                stopped_ms INTEGER NOT NULL,
                stopped_by TEXT NOT NULL DEFAULT 'owner',
                last_by TEXT
            );
            CREATE TABLE IF NOT EXISTS team_status_log (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                project TEXT NOT NULL,
                agent INTEGER NOT NULL,
                status TEXT NOT NULL,
                task INTEGER,
                at_ms INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS team_caps (
                project TEXT PRIMARY KEY,
                harnesses INTEGER CHECK(harnesses IS NULL OR harnesses BETWEEN 1 AND 15),
                teammates INTEGER CHECK(teammates IS NULL OR teammates BETWEEN 1 AND 15)
            );
            CREATE TABLE IF NOT EXISTS team_token_budgets (
                project TEXT PRIMARY KEY,
                tokens INTEGER NOT NULL CHECK(tokens > 0),
                since_ms INTEGER NOT NULL,
                baseline INTEGER NOT NULL CHECK(baseline >= 0),
                reached_ms INTEGER
            );
            CREATE TABLE IF NOT EXISTS team_account_spread (
                project TEXT PRIMARY KEY,
                since_ms INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS team_teammates (
                agent INTEGER NOT NULL,
                project TEXT NOT NULL,
                pane TEXT NOT NULL,
                seen_ms INTEGER NOT NULL,
                ended_ms INTEGER,
                stopped_ms INTEGER,
                PRIMARY KEY(agent, pane)
            );
            CREATE TABLE IF NOT EXISTS team_teammate_names (
                agent INTEGER PRIMARY KEY,
                name TEXT NOT NULL CHECK(length(name) BETWEEN 1 AND 64),
                save_refused TEXT CHECK(save_refused IS NULL OR length(save_refused) <= 128)
            );
            CREATE TABLE IF NOT EXISTS team_agent_conversations (
                agent INTEGER PRIMARY KEY,
                project TEXT NOT NULL,
                harness TEXT NOT NULL,
                connection TEXT NOT NULL,
                conversation TEXT NOT NULL CHECK(length(conversation) BETWEEN 1 AND 128),
                record TEXT NOT NULL CHECK(length(record) BETWEEN 1 AND 4096)
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
            CREATE INDEX IF NOT EXISTS team_owner_prompts_project ON team_owner_prompts(project, id);
            CREATE TABLE IF NOT EXISTS team_owner_turns (
                message INTEGER PRIMARY KEY,
                project TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS team_owner_turns_project ON team_owner_turns(project);",
        )
        .map_err(error)?;
    add_late_columns(connection)?;
    events::initialize(connection)?;
    agent_messages::initialize(connection)?;
    master::initialize(connection)?;
    names::initialize(connection)?;
    questions::initialize(connection)?;
    direct::erase_typed_input(connection)
}

/// Logs each real status change of a team agent while its project exists, so a live agent
/// stopped after a removal leaves no row (HB-S5-I). Apart from [`initialize`] because it
/// reads `projects`, which only the engine's own schema has. Takes the engine database.
/// Returns a SQLite failure.
#[cfg(unix)]
pub(crate) fn log_status_changes(connection: &Connection) -> Result<(), EngineError> {
    connection
        .execute_batch(
            "CREATE TRIGGER IF NOT EXISTS team_status_logged
            AFTER UPDATE OF status ON team_agents
            WHEN NEW.status <> OLD.status
                AND EXISTS (SELECT 1 FROM projects WHERE id = NEW.project)
            BEGIN
                INSERT INTO team_status_log(project,agent,status,task,at_ms)
                VALUES (NEW.project,NEW.id,NEW.status,NEW.task,
                        CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER));
                DELETE FROM team_status_log WHERE id <= last_insert_rowid() - 20000;
            END;",
        )
        .map_err(error)
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
    if let Some(session) = crate::master_sessions::live_session(tx, &stored.project_id)? {
        tx.execute(
            "UPDATE team_tasks SET session=?2 WHERE id=?1",
            params![id, session],
        )
        .map_err(error)?;
    }
    Ok(Ok(id))
}

/// Starts the work that resolves what an Ask agent to update merged into a team agent's
/// worktree (DEC-095): a sub-agent gets a task pinned to it, so it runs in the same
/// worktree and lands in the same change set; the team master gets an engine message. A
/// sub-agent holds at most one queued resolve task, whose prompt the newest update
/// replaces, so the global task cap never refuses a merge already made. The update is
/// the owner's, so it clears the owner stop of that sub-agent alone; the agents it leads
/// stay stopped. Takes the transaction, project, agent, prompt and time. Returns Ok, or a
/// plain refusal code.
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
        Some(AgentRole::Sub) => owner_start_alone(tx, project, agent)?,
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
        Some(AgentRole::Teammate) | None => return Ok(Err("team_agent_not_found")),
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

/// Wakes a finished sub-agent for its master (DEC-102): the message becomes a task pinned
/// to that agent, so the runtime starts the same agent again under its own id and name, in
/// its own worktree, with the message as its first input, and the master hears how it ends.
/// Only a stopped or failed sub-agent of the project whose worktree is still recorded wakes,
/// and never one stopped by someone above the waker: only the owner's Wake or restart starts
/// one the owner stopped (AW-1), and a lead does not wake one the master stopped (HB-S5-B).
/// Takes the transaction, live access, the setup, the agent, the message, who wakes it and
/// the time. Returns the task id, or a plain refusal code: `team_agent_not_found`,
/// `team_agent_running`, `team_agent_no_worktree`, `team_agent_owner_stopped`,
/// `team_agent_master_stopped`, or one of `admit`'s.
pub fn admit_wake(
    tx: &Transaction<'_>,
    access: &ModelAccess,
    stored: &StoredSetup,
    agent: i64,
    prompt: &str,
    waker: StoppedBy,
    now: i64,
) -> Result<Result<i64, &'static str>, EngineError> {
    assert!(
        waker != StoppedBy::Owner,
        "the owner wakes through WakeAgent"
    );
    let project = &stored.project_id;
    if let Some(code) = wake_refusal(tx, project, agent)? {
        return Ok(Err(code));
    }
    match stop_of(tx, agent)? {
        Some((StoppedBy::Owner, _)) => return Ok(Err(OWNER_STOPPED)),
        Some((StoppedBy::Master, _)) if waker == StoppedBy::Lead => {
            return Ok(Err(MASTER_STOPPED));
        }
        _ => {}
    }
    let harness: String = tx
        .query_row(
            "SELECT harness FROM team_agents WHERE id=?1",
            [agent],
            |row| row.get(0),
        )
        .map_err(error)?;
    let task = match admit(
        tx,
        access,
        stored,
        Some(&harness),
        prompt,
        TaskSource::Master,
        None,
        now,
    )? {
        Ok(task) => task,
        Err(code) => return Ok(Err(code)),
    };
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
    Ok(Ok(task))
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

/// Records a message the owner typed in the team chat as an owner turn, the only kind of
/// message a master may name as the owner's answer to a question, and forgets the turns of
/// messages the conversation no longer keeps, so the records stay within its bound.
/// Takes the transaction, the project and the message id.
fn owner_turn(tx: &Transaction<'_>, project: &ProjectId, message: i64) -> Result<(), EngineError> {
    tx.execute(
        "DELETE FROM team_owner_turns WHERE project=?1
         AND message NOT IN (SELECT id FROM team_messages WHERE project=?1)",
        [project.as_str()],
    )
    .map_err(error)?;
    tx.execute(
        "INSERT INTO team_owner_turns(message,project) VALUES (?1,?2)",
        params![message, project.as_str()],
    )
    .map_err(error)?;
    Ok(())
}

/// Takes the next version of a project's team settings for an owner change made from a read
/// of `version`, in the change's transaction before the value is written, so a change from a
/// screen that read older settings never replaces newer ones (V2SYNC-2). Inputs: the owner
/// transaction, the project and the version the owner's screen showed. Output: true when the
/// change may be written, false when it is stale.
fn settings_write(
    tx: &Transaction<'_>,
    project: &ProjectId,
    version: u64,
) -> Result<bool, EngineError> {
    Ok(versioned::owner_write(tx, &versioned::team_settings_item(project), version)?.is_ok())
}

/// Reads the version of a project's team settings that a change must name, 0 before the
/// owner first changed one. Inputs: the engine database and the project. Output: the version,
/// or `versioned_unavailable`.
pub fn settings_version(connection: &Connection, project: &ProjectId) -> Result<u64, EngineError> {
    Ok(versioned::current(connection, &versioned::team_settings_item(project))?.version)
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

/// Executes one owner team action inside the owner transaction. A message is the owner's
/// own chat turn, so it is recorded as one a master may name as the owner's answer.
/// Takes the transaction, the engine configuration and the action.
/// Returns Applied, or a plain rejection before any row is written.
pub fn execute(
    tx: &Transaction<'_>,
    access: &ModelAccess,
    action: &TeamAction,
) -> Result<Outcome, EngineError> {
    perform(tx, access, action, true)
}

/// Executes one owner team action whose message an engine surface framed around the
/// owner's words with text the owner did not type, such as an IDE review comment, so the
/// message is never an owner turn. Takes the transaction, the engine configuration and the
/// action. Returns what `execute` returns.
#[cfg(unix)]
pub(crate) fn execute_framed(
    tx: &Transaction<'_>,
    access: &ModelAccess,
    action: &TeamAction,
) -> Result<Outcome, EngineError> {
    perform(tx, access, action, false)
}

/// Executes one owner team action, recording its message as an owner turn only when the
/// owner typed all of it. Takes the transaction, the engine configuration, the action and
/// whether the owner typed it. Returns what `execute` returns.
fn perform(
    tx: &Transaction<'_>,
    access: &ModelAccess,
    action: &TeamAction,
    typed: bool,
) -> Result<Outcome, EngineError> {
    action.validate()?;
    let Some(project) = action.project() else {
        return wake(tx, action);
    };
    let Some(stored) = crate::project_setup::load_setup(tx, project)? else {
        return Ok(rejected("project_setup_missing"));
    };
    let now = now_ms();
    match action {
        TeamAction::Wake { .. } => wake(tx, action),
        TeamAction::Seen { upto, .. } => {
            events::seen(tx, Some(project), *upto, now)?;
            Ok(Outcome::Applied)
        }
        TeamAction::Message { text, .. } => Ok(
            match append_message(
                tx,
                project,
                MessageAuthor::Owner,
                text,
                MessageState::Pending,
                now,
            )? {
                Ok(message) => {
                    if typed {
                        owner_turn(tx, project, message)?;
                    }
                    Outcome::Applied
                }
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
        TeamAction::KeepWarm { on, version, .. } => {
            if !settings_write(tx, project, *version)? {
                return Ok(rejected(STALE_CODE));
            }
            tx.execute(
                "INSERT INTO team_settings(project,keep_warm) VALUES (?1,?2)
                 ON CONFLICT(project) DO UPDATE SET keep_warm=excluded.keep_warm",
                params![project.as_str(), on],
            )
            .map_err(error)?;
            Ok(Outcome::Applied)
        }
        TeamAction::TimeLimits {
            turn_secs,
            session_secs,
            idle_secs,
            version,
            ..
        } => {
            if !settings_write(tx, project, *version)? {
                return Ok(rejected(STALE_CODE));
            }
            tx.execute(
                "INSERT INTO team_time_limits(project,turn_secs,session_secs,idle_secs) VALUES (?1,?2,?3,?4)
                 ON CONFLICT(project) DO UPDATE SET turn_secs=excluded.turn_secs,
                 session_secs=excluded.session_secs, idle_secs=excluded.idle_secs",
                params![project.as_str(), turn_secs, session_secs, idle_secs.unwrap_or(0)],
            )
            .map_err(error)?;
            Ok(Outcome::Applied)
        }
        TeamAction::OnHarnessSwitch { mode, version, .. } => {
            if !settings_write(tx, project, *version)? {
                return Ok(rejected(STALE_CODE));
            }
            tx.execute(
                "INSERT INTO team_master_switch(project,mode) VALUES (?1,?2)
                 ON CONFLICT(project) DO UPDATE SET mode=excluded.mode",
                params![project.as_str(), mode.name()],
            )
            .map_err(error)?;
            Ok(Outcome::Applied)
        }
        TeamAction::ApplyHarnessSwitch { mode, version, .. } => {
            if !settings_write(tx, project, *version)? {
                return Ok(rejected(STALE_CODE));
            }
            tx.execute(
                "INSERT INTO team_master_apply(project,mode) VALUES (?1,?2)
                 ON CONFLICT(project) DO UPDATE SET mode=excluded.mode",
                params![project.as_str(), mode.name()],
            )
            .map_err(error)?;
            Ok(Outcome::Applied)
        }
        TeamAction::SubAgentDepth { depth, .. } => {
            tx.execute(
                "INSERT INTO team_nesting(project,depth,lands) VALUES (?1,?2,?3)
                 ON CONFLICT(project) DO UPDATE SET depth=excluded.depth",
                params![project.as_str(), depth, ChildLanding::default().name()],
            )
            .map_err(error)?;
            Ok(Outcome::Applied)
        }
        TeamAction::ChildLanding { mode, .. } => {
            tx.execute(
                "INSERT INTO team_nesting(project,depth,lands) VALUES (?1,?2,?3)
                 ON CONFLICT(project) DO UPDATE SET lands=excluded.lands",
                params![project.as_str(), DEPTH_DEFAULT, mode.name()],
            )
            .map_err(error)?;
            Ok(Outcome::Applied)
        }
        TeamAction::FinishedAgents { show, version, .. } => {
            if !settings_write(tx, project, *version)? {
                return Ok(rejected(STALE_CODE));
            }
            tx.execute(
                "INSERT INTO team_finished_view(project,show) VALUES (?1,?2)
                 ON CONFLICT(project) DO UPDATE SET show=excluded.show",
                params![project.as_str(), show],
            )
            .map_err(error)?;
            Ok(Outcome::Applied)
        }
        TeamAction::AgentBranches { mode, .. } => {
            tx.execute(
                "INSERT INTO team_agent_branches(project,mode) VALUES (?1,?2)
                 ON CONFLICT(project) DO UPDATE SET mode=excluded.mode",
                params![project.as_str(), mode.name()],
            )
            .map_err(error)?;
            Ok(Outcome::Applied)
        }
        TeamAction::MasterBranch { branch, .. } => {
            match branch {
                Some(branch) => tx.execute(
                    "INSERT INTO team_master_branch(project,branch) VALUES (?1,?2)
                     ON CONFLICT(project) DO UPDATE SET branch=excluded.branch",
                    params![project.as_str(), branch],
                ),
                None => tx.execute(
                    "DELETE FROM team_master_branch WHERE project=?1",
                    [project.as_str()],
                ),
            }
            .map_err(error)?;
            Ok(Outcome::Applied)
        }
        TeamAction::LandOn { branch, .. } => {
            match branch {
                Some(branch) => tx.execute(
                    "INSERT INTO team_land_on(project,branch) VALUES (?1,?2)
                     ON CONFLICT(project) DO UPDATE SET branch=excluded.branch",
                    params![project.as_str(), branch],
                ),
                None => tx.execute(
                    "DELETE FROM team_land_on WHERE project=?1",
                    [project.as_str()],
                ),
            }
            .map_err(error)?;
            Ok(Outcome::Applied)
        }
        TeamAction::BackgroundAtEnd { choice, .. } => {
            let (mode, secs) = choice.stored();
            tx.execute(
                "INSERT INTO team_background(project,mode,grace_secs) VALUES (?1,?2,?3)
                 ON CONFLICT(project) DO UPDATE SET mode=excluded.mode,grace_secs=excluded.grace_secs",
                params![project.as_str(), mode, secs],
            )
            .map_err(error)?;
            Ok(Outcome::Applied)
        }
        TeamAction::AskBeforeMerge { on, .. } => {
            tx.execute(
                "INSERT INTO team_merge_ask(project,ask) VALUES (?1,?2)
                 ON CONFLICT(project) DO UPDATE SET ask=excluded.ask",
                params![project.as_str(), on],
            )
            .map_err(error)?;
            Ok(Outcome::Applied)
        }
        TeamAction::MasterLands { mode, .. } => {
            set_master_lands(tx, project, *mode)?;
            Ok(Outcome::Applied)
        }
        TeamAction::ReaskEmptyReply { on, .. } => {
            tx.execute(
                "INSERT INTO team_reask_empty(project,reask) VALUES (?1,?2)
                 ON CONFLICT(project) DO UPDATE SET reask=excluded.reask",
                params![project.as_str(), on],
            )
            .map_err(error)?;
            Ok(Outcome::Applied)
        }
        TeamAction::MasterIdleStop { choice, .. } => {
            set_master_idle_stop(tx, project, *choice)?;
            Ok(Outcome::Applied)
        }
        TeamAction::MasterLiveTerminal { on, .. } => {
            master::set_live_terminal(tx, project, *on)?;
            Ok(Outcome::Applied)
        }
        TeamAction::TerminalsPerWindow { per_window, .. } => {
            wall::store(tx, project, *per_window)?;
            Ok(Outcome::Applied)
        }
        TeamAction::CatchUpMain { mode, .. } => {
            set_catch_up_main(tx, project, *mode)?;
            Ok(Outcome::Applied)
        }
        TeamAction::ReuseIdle { mode, .. } => {
            set_reuse_idle(tx, project, *mode)?;
            Ok(Outcome::Applied)
        }
        TeamAction::LandingMode { mode, .. } => {
            set_landing_mode(tx, project, *mode)?;
            Ok(Outcome::Applied)
        }
        TeamAction::NewFolderLimit { files, .. } => {
            tx.execute(
                "INSERT INTO team_new_folder_limit(project,files) VALUES (?1,?2)
                 ON CONFLICT(project) DO UPDATE SET files=excluded.files",
                params![project.as_str(), files],
            )
            .map_err(error)?;
            Ok(Outcome::Applied)
        }
        TeamAction::CoordSettings { settings, .. } => {
            crate::coord::settings::write(tx, project, settings)?;
            Ok(Outcome::Applied)
        }
        TeamAction::HarnessMode { harness, mode, .. } => {
            if *mode == crate::coord::settings::SubagentMode::Native
                && !crate::coord::settings::native_supported(*harness)
            {
                return Ok(rejected("coord_native_unsupported"));
            }
            crate::coord::settings::set_mode(tx, project, *harness, *mode)?;
            Ok(Outcome::Applied)
        }
        TeamAction::HarnessDelivery {
            harness, delivery, ..
        } => {
            crate::coord::settings::set_delivery(tx, project, *harness, *delivery)?;
            Ok(Outcome::Applied)
        }
        TeamAction::MessageSettings { settings, .. } => {
            agent_messages::save_settings(tx, project, settings)?;
            Ok(Outcome::Applied)
        }
        TeamAction::LiveMaster { on, version, .. } => {
            if *on
                && crate::harnesses::interactive::support(&stored.team.master)
                    != crate::harnesses::interactive::Support::Terminal
            {
                return Ok(rejected("team_master_no_terminal"));
            }
            if !settings_write(tx, project, *version)? {
                return Ok(rejected(STALE_CODE));
            }
            master::set_live(tx, project, *on)?;
            Ok(Outcome::Applied)
        }
        TeamAction::Pause { .. } => {
            pause(tx, project, now)?;
            Ok(Outcome::Applied)
        }
        TeamAction::Resume { .. } => {
            tx.execute(
                "DELETE FROM team_paused_projects WHERE project=?1",
                [project.as_str()],
            )
            .map_err(error)?;
            let baseline = usage_from(tx, project, hour_of(now))?;
            tx.execute(
                "UPDATE team_token_budgets SET since_ms=?2, baseline=?3, reached_ms=NULL WHERE project=?1",
                params![project.as_str(), now, baseline],
            )
            .map_err(error)?;
            Ok(Outcome::Applied)
        }
        TeamAction::StopAgent { agent, .. } => {
            if let Some(code) = stop_refusal(tx, project, *agent)? {
                return Ok(rejected(code));
            }
            owner_stop(tx, project, *agent, now)?;
            Ok(Outcome::Applied)
        }
        TeamAction::MessageAgent { agent, text, .. } => {
            let message = crate::coord::NewMessage {
                project: project.clone(),
                from: crate::coord::Party::Owner,
                to: crate::coord::Party::Agent(*agent),
                kind: crate::coord::MessageKind::Prompt,
                question: None,
                work: None,
                reply_to: None,
                body: text.clone(),
                idempotency_key: None,
            };
            match crate::coord::send(tx, message, now) {
                Ok(_) => Ok(Outcome::Applied),
                Err(problem) if problem.0.starts_with("coord_state_unavailable") => Err(problem),
                Err(problem) => Ok(rejected(&problem.0)),
            }
        }
        TeamAction::StopTeammate { agent, pane, .. } => {
            let changed = tx
                .execute(
                    "UPDATE team_teammates SET stopped_ms=COALESCE(stopped_ms,?4)
                     WHERE agent=?1 AND project=?2 AND pane=?3 AND ended_ms IS NULL",
                    params![agent, project.as_str(), pane, now],
                )
                .map_err(error)?;
            Ok(if changed == 0 {
                rejected("team_teammate_not_found")
            } else {
                Outcome::Applied
            })
        }
        TeamAction::SpreadAccounts { on, .. } => {
            if *on {
                tx.execute(
                    "INSERT OR IGNORE INTO team_account_spread(project,since_ms) VALUES (?1,?2)",
                    params![project.as_str(), now],
                )
            } else {
                tx.execute(
                    "DELETE FROM team_account_spread WHERE project=?1",
                    [project.as_str()],
                )
            }
            .map_err(error)?;
            Ok(Outcome::Applied)
        }
        TeamAction::MaxHarnesses { max, .. } => {
            tx.execute(
                "INSERT INTO team_caps(project,harnesses) VALUES (?1,?2)
                 ON CONFLICT(project) DO UPDATE SET harnesses=excluded.harnesses",
                params![project.as_str(), max],
            )
            .map_err(error)?;
            Ok(Outcome::Applied)
        }
        TeamAction::MaxTeammates { max, .. } => {
            tx.execute(
                "INSERT INTO team_caps(project,teammates) VALUES (?1,?2)
                 ON CONFLICT(project) DO UPDATE SET teammates=excluded.teammates",
                params![project.as_str(), max],
            )
            .map_err(error)?;
            Ok(Outcome::Applied)
        }
        TeamAction::TokenBudget { tokens, .. } => {
            match tokens {
                None => {
                    tx.execute(
                        "DELETE FROM team_token_budgets WHERE project=?1",
                        [project.as_str()],
                    )
                    .map_err(error)?;
                }
                Some(tokens) => {
                    let tokens =
                        i64::try_from(*tokens).map_err(|_| failure("team_budget_invalid"))?;
                    let baseline = usage_from(tx, project, hour_of(now))?;
                    tx.execute(
                        "INSERT INTO team_token_budgets(project,tokens,since_ms,baseline,reached_ms) VALUES (?1,?2,?3,?4,NULL)
                         ON CONFLICT(project) DO UPDATE SET tokens=excluded.tokens, since_ms=excluded.since_ms,
                         baseline=excluded.baseline, reached_ms=NULL",
                        params![project.as_str(), tokens, now, baseline],
                    )
                    .map_err(error)?;
                }
            }
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
        TeamAction::WakeAgent { agent, .. } => Ok(match request_wake(tx, project, *agent, now)? {
            Ok(()) => Outcome::Applied,
            Err(code) => rejected(code),
        }),
        TeamAction::AnswerQuestion { message, text, .. } => {
            questions::answer(tx, project, *message, text, now)
        }
    }
}

/// Words why a finished agent's Wake is off, for the button the owner sees. Takes the refusal
/// code from [`wake_refusal`]. Returns the line.
fn wake_note(code: &str) -> String {
    match code {
        "team_agent_running" => "Already running.".into(),
        "team_agent_no_worktree" => {
            "Its worktree was cleaned up, so it cannot be woken. Hand its work to a new agent."
                .into()
        }
        "team_agents_unavailable" => {
            crate::research::runner::confined_unavailable(std::env::consts::OS)
                .unwrap_or("Team agents cannot run on this system.")
                .into()
        }
        _ => "That agent is no longer part of this team.".into(),
    }
}

/// Tells why a sub-agent cannot be woken now (DEC-102), or none when it can: it must be a
/// sub-agent of the project that stopped or failed, whose worktree is still recorded, on a
/// system where team agents run. Takes a connection, the project and the agent. Returns the
/// refusal code: `team_agent_not_found`, `team_agent_running`, `team_agent_no_worktree` or
/// `team_agents_unavailable`.
pub fn wake_refusal(
    connection: &Connection,
    project: &ProjectId,
    agent: i64,
) -> Result<Option<&'static str>, EngineError> {
    let found: Option<(String, String, bool)> = connection
        .query_row(
            "SELECT a.role,a.status,EXISTS(SELECT 1 FROM team_worktrees w WHERE w.agent=a.id AND w.project=a.project)
             FROM team_agents a WHERE a.id=?1 AND a.project=?2",
            params![agent, project.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()
        .map_err(error)?;
    let Some((role, status, kept)) = found else {
        return Ok(Some("team_agent_not_found"));
    };
    Ok(if AgentRole::parse(&role) != Some(AgentRole::Sub) {
        Some("team_agent_not_found")
    } else if AgentStatus::parse(&status).is_none_or(AgentStatus::live) {
        Some("team_agent_running")
    } else if !kept {
        Some("team_agent_no_worktree")
    } else if crate::research::runner::confined_unavailable(std::env::consts::OS).is_some() {
        Some("team_agents_unavailable")
    } else {
        None
    })
}

/// The refusal code for a master's wake of a sub-agent the owner stopped (AW-1).
pub const OWNER_STOPPED: &str = "team_agent_owner_stopped";

/// The refusal code for the engine's own update in a team the owner paused (T8-R2-S3).
pub const TEAM_PAUSED: &str = "team_paused";

/// The refusal code for a lead's wake of a sub-agent the master stopped (HB-S5-B).
pub const MASTER_STOPPED: &str = "team_agent_master_stopped";

/// Tells whether the owner stopped a sub-agent that has not started again since (AW-1), so
/// only the owner's Wake or restart may start it. Takes a connection and the agent. Returns
/// whether it was the owner's stop, or a SQLite failure.
pub fn owner_stopped(connection: &Connection, agent: i64) -> Result<bool, EngineError> {
    connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM team_owner_stops WHERE agent=?1 AND stopped_by='owner')",
            [agent],
            |row| row.get(0),
        )
        .map_err(error)
}

/// Tells whether anyone's stop still holds a sub-agent. Takes a connection and the agent.
/// Returns the answer, or a SQLite failure.
pub(crate) fn stopped(connection: &Connection, agent: i64) -> Result<bool, EngineError> {
    Ok(stop_of(connection, agent)?.is_some())
}

/// Who stopped a sub-agent that has not started again since, and when. Takes a connection
/// and the agent. Returns them, None when no stop holds it, or a SQLite failure.
pub(crate) fn stop_of(
    connection: &Connection,
    agent: i64,
) -> Result<Option<(StoppedBy, i64)>, EngineError> {
    let row: Option<(String, i64)> = connection
        .query_row(
            "SELECT stopped_by,stopped_ms FROM team_owner_stops WHERE agent=?1",
            [agent],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(error)?;
    Ok(row.map(|(by, stopped_ms)| (StoppedBy::named(&by), stopped_ms)))
}

/// Who made the latest stop of a sub-agent that has not started again since, the stop whose
/// time fails its earlier tasks (HB-S5-B-L2): a lower stop after a higher one takes the time
/// while the higher maker keeps the standing. Takes a connection and the agent. Returns the
/// maker, the holder for a row from before the column, None when no stop holds it, or a
/// SQLite failure.
pub(crate) fn last_stopper(
    connection: &Connection,
    agent: i64,
) -> Result<Option<StoppedBy>, EngineError> {
    let by: Option<String> = connection
        .query_row(
            "SELECT COALESCE(last_by,stopped_by) FROM team_owner_stops WHERE agent=?1",
            [agent],
            |row| row.get(0),
        )
        .optional()
        .map_err(error)?;
    Ok(by.map(|by| StoppedBy::named(&by)))
}

/// Records the owner's Wake of a finished sub-agent (DEC-102), which the team runtime serves
/// on its next tick through the same wake as the master's: the agent starts again under its
/// own id, name and worktree, continuing its harness conversation where it can. A second
/// Wake before the first is served changes nothing. Takes the transaction, project, agent
/// and the time. Returns a plain refusal code from [`wake_refusal`] when it cannot wake.
fn request_wake(
    tx: &Transaction<'_>,
    project: &ProjectId,
    agent: i64,
    now: i64,
) -> Result<Result<(), &'static str>, EngineError> {
    if let Some(code) = wake_refusal(tx, project, agent)? {
        return Ok(Err(code));
    }
    tx.execute(
        "INSERT OR IGNORE INTO team_wake_requests(agent,project,requested_ms) VALUES (?1,?2,?3)",
        params![agent, project.as_str(), now],
    )
    .map_err(error)?;
    Ok(Ok(()))
}

fn wake(tx: &Transaction<'_>, action: &TeamAction) -> Result<Outcome, EngineError> {
    let TeamAction::Wake {
        target,
        rule,
        version,
    } = action
    else {
        unreachable!("only a wake rule action has no project");
    };
    Ok(
        match events::set_rule(tx, target, rule.as_ref(), *version)? {
            Ok(()) => Outcome::Applied,
            Err(code) => rejected(code),
        },
    )
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

/// Lists the sub-agent tasks a master reply proposed in its `crazyagent-task` blocks, read
/// by `tool::reply_blocks`, each with the task handed off for it.
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
    let delegations =
        tool::reply_blocks(text)
            .0
            .into_iter()
            .filter_map(|proposed| match proposed {
                tool::Proposal::Delegate(found) => Some(found),
                tool::Proposal::Wake(_) => None,
            });
    let steps: Vec<PlanStep> = delegations
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
    #[serde(default)]
    pub finished: bool,
    #[serde(default)]
    pub wakeable: bool,
    #[serde(default)]
    pub wake_note: Option<String>,
    #[serde(default)]
    pub parent: Option<i64>,
    #[serde(default)]
    pub owner_stopped: bool,
    #[serde(default)]
    pub teammates: Vec<Teammate>,
    #[serde(default)]
    pub pane: Option<String>,
    #[serde(default)]
    pub hookless: bool,
    #[serde(default)]
    pub instructions_changed: bool,
    #[serde(default)]
    pub message_note: Option<String>,
}

/// One teammate a harness split off in its own pane of the harness's terminal, such as a
/// Claude Code agent team member: its pane id, whether its pane closed, whether the owner or
/// the teammate cap stopped it, and when the engine first saw it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Teammate {
    pub pane: String,
    pub ended: bool,
    pub stopped: bool,
    pub seen_ms: i64,
}

impl TeamAgent {
    /// Tells whether this agent belongs in the Finished fold-out rather than the agents
    /// list and team graph (DEC-102, narrowed by the owner after the DEC-104 trial): a
    /// stopped or failed sub-agent or teammate holding no task and with no question waiting
    /// for the master. The master and every live agent stay in view, an idle one waiting
    /// for work or for its own workers too; Idle stop ends an idle agent, which then moves
    /// here. Takes the questions the agent has waiting. Returns true for a finished agent.
    pub fn finished_with(&self, questions: u16) -> bool {
        matches!(self.role, AgentRole::Sub | AgentRole::Teammate)
            && questions == 0
            && self.task.is_none()
            && match self.status {
                AgentStatus::Stopped | AgentStatus::Failed => true,
                AgentStatus::Starting | AgentStatus::Idle | AgentStatus::Working => false,
            }
    }
}

/// Orders agents as their lead tree (N2): each lead followed by its own workers, depth
/// first, keeping the given order among siblings; an agent whose lead is not listed starts
/// a tree of its own. Takes the agents in display order. Returns each agent once with its
/// depth under the listed leads, 0 for a top agent.
pub fn lead_tree<'a>(agents: &[&'a TeamAgent]) -> Vec<(&'a TeamAgent, usize)> {
    let listed: BTreeSet<i64> = agents.iter().map(|agent| agent.id).collect();
    let mut stack: Vec<(&TeamAgent, usize)> = agents
        .iter()
        .rev()
        .filter(|agent| {
            agent
                .parent
                .is_none_or(|lead| lead == agent.id || !listed.contains(&lead))
        })
        .map(|agent| (*agent, 0))
        .collect();
    let mut seen = BTreeSet::new();
    let mut ordered = Vec::with_capacity(agents.len());
    while let Some((agent, depth)) = stack.pop() {
        if !seen.insert(agent.id) {
            continue;
        }
        ordered.push((agent, depth));
        stack.extend(
            agents
                .iter()
                .rev()
                .filter(|worker| worker.parent == Some(agent.id) && worker.id != agent.id)
                .map(|worker| (*worker, depth + 1)),
        );
    }
    for agent in agents {
        if seen.insert(agent.id) {
            ordered.push((agent, 0));
        }
    }
    assert_eq!(ordered.len(), listed.len());
    ordered
}

/// One sub-agent task as the clients poll it. `parent` is the lead that handed it out, none
/// for a task of the master or the owner, so clients name the lead it came from.
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
    #[serde(default)]
    pub parent: Option<i64>,
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

/// The new-folder limit a team status read without one shows. Output: the default.
fn new_folder_default() -> u32 {
    NEW_FOLDER_FILES_DEFAULT
}

/// The re-ask choice a team status read without one shows: on, as the owner chose
/// (DEC-111). Output: the default.
fn reask_default() -> bool {
    true
}

/// The "Live terminal" setting a team status read without one shows: on (DEC-117 Q4).
/// Output: the default.
fn live_terminal_default() -> bool {
    true
}

/// Everything the clients show about a project's team.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TeamStatus {
    pub project_id: ProjectId,
    pub keep_warm: bool,
    #[serde(default)]
    pub turn_limit_secs: Option<u32>,
    #[serde(default)]
    pub session_limit_secs: Option<u32>,
    #[serde(default)]
    pub idle_stop_secs: Option<u32>,
    #[serde(default)]
    pub on_harness_switch: SwitchMode,
    #[serde(default)]
    pub apply_harness_switch: SwitchApply,
    #[serde(default = "depth_default")]
    pub sub_agent_depth: u8,
    #[serde(default)]
    pub child_landing: ChildLanding,
    #[serde(default)]
    pub show_finished: bool,
    #[serde(default)]
    pub message_settings: agent_messages::MessageSettings,
    #[serde(default)]
    pub agent_branches: BranchMode,
    #[serde(default)]
    pub master_branch: Option<String>,
    #[serde(default)]
    pub land_on: Option<String>,
    #[serde(default)]
    pub background_at_end: BackgroundAtEnd,
    #[serde(default)]
    pub ask_before_merge: bool,
    #[serde(default)]
    pub master_lands: Option<MasterLands>,
    #[serde(default)]
    pub access_unreadable: bool,
    #[serde(default = "reask_default")]
    pub reask_empty_reply: bool,
    #[serde(default)]
    pub master_idle_stop: MasterIdleStop,
    #[serde(default = "live_terminal_default")]
    pub master_live_terminal: bool,
    #[serde(default)]
    pub catch_up_main: CatchUpMain,
    #[serde(default)]
    pub reuse_idle: ReuseIdle,
    #[serde(default)]
    pub landing_mode: LandingMode,
    #[serde(default = "new_folder_default")]
    pub new_folder_files_max: u32,
    #[serde(default)]
    pub coordination: Option<CoordinationView>,
    #[serde(default)]
    pub live_switch: Option<SessionSwitch>,
    pub paused: bool,
    #[serde(default)]
    pub max_harnesses: Option<u8>,
    #[serde(default)]
    pub max_teammates: Option<u8>,
    #[serde(default)]
    pub token_budget: Option<u64>,
    #[serde(default)]
    pub budget_used: Option<u64>,
    #[serde(default)]
    pub budget_reached: bool,
    #[serde(default)]
    pub spread_accounts: bool,
    #[serde(default)]
    pub terminals_per_window: TerminalsPerWindow,
    #[serde(default)]
    pub live_master: Option<bool>,
    #[serde(default)]
    pub live_problem: Option<String>,
    /// Whether the line above is the one an idle stop left, so the master view offers Start
    /// again. Never set while the live master is on.
    #[serde(default)]
    pub live_idle_stopped: bool,
    #[serde(default)]
    pub settings_version: u64,
    pub min_agents: u8,
    pub max_agents: u8,
    pub harnesses: Vec<TeamHarness>,
    pub agents: Vec<TeamAgent>,
    pub tasks: Vec<TeamTask>,
    pub messages: Vec<TeamMessage>,
    pub plan: Vec<PlanStep>,
    pub events: events::EventsView,
    #[serde(default)]
    pub partial: bool,
}

fn depth_default() -> u8 {
    DEPTH_DEFAULT
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
        finished: false,
        wakeable: false,
        wake_note: None,
        parent: row.get(16)?,
        owner_stopped: false,
        teammates: Vec::new(),
        pane: row.get(17)?,
        hookless: row.get(18)?,
        instructions_changed: false,
        message_note: None,
    })
}

pub(crate) const AGENT_COLUMNS: &str = "id,role,harness,tools,connection,worktree,status,task,turns,error,process_group,created_ms,heartbeat_ms,stopped_ms,(SELECT persona FROM team_names WHERE team_names.agent=team_agents.id),(SELECT handle FROM team_names WHERE team_names.agent=team_agents.id),parent,pane,hookless";
/// The team task columns the clients read, in `task_row` order, selected from `team_tasks`.
/// A task a project's conversation master handed off is stored as the owner's own (DEC-093),
/// so the team plan and history treat it like one, but reads as the master's, so the Agents
/// tab, Team view and TUI say it came from the master.
pub(crate) const TASK_COLUMNS: &str = "id,CASE WHEN EXISTS (SELECT 1 FROM team_conversation_tasks c WHERE c.task=team_tasks.id) THEN 'master' ELSE source END,harness,tools,prompt,status,agent,attempts,result,error,delivered,output_bytes,created_ms,started_ms,finished_ms,parent";

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
        parent: row.get(15)?,
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

/// The time limits the owner set for a project's team agents, in seconds: one turn, one
/// agent process, and how long an idle sub-agent waits for work. None is no limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimeLimits {
    pub turn: Option<u32>,
    pub session: Option<u32>,
    pub idle: Option<u32>,
}

impl Default for TimeLimits {
    fn default() -> Self {
        Self {
            turn: None,
            session: None,
            idle: Some(IDLE_STOP_DEFAULT_SECS),
        }
    }
}

impl TimeLimits {
    /// The bound on one master or sub-agent turn. Output: the duration, or none for no limit.
    pub fn turn_time(self) -> Option<std::time::Duration> {
        self.turn
            .map(|secs| std::time::Duration::from_secs(secs.into()))
    }

    /// The bound on one agent process. Output: the duration, or none for no limit.
    pub fn session_time(self) -> Option<std::time::Duration> {
        self.session
            .map(|secs| std::time::Duration::from_secs(secs.into()))
    }

    /// How long an idle sub-agent waits for work. Output: the duration, or none to keep it.
    pub fn idle_time(self) -> Option<std::time::Duration> {
        self.idle
            .map(|secs| std::time::Duration::from_secs(secs.into()))
    }
}

/// Names a time span the way Team settings shows it: whole hours as "1 h", whole minutes as
/// "30 min", anything else in seconds. Input: the span in seconds. Output: the words.
pub fn span_words(secs: u64) -> String {
    if secs >= 3600 && secs.is_multiple_of(3600) {
        format!("{} h", secs / 3600)
    } else if secs >= 60 && secs.is_multiple_of(60) {
        format!("{} min", secs / 60)
    } else {
        format!("{secs} s")
    }
}

/// Reads the time limits the owner set for a project. Takes a connection and the project.
/// Returns the defaults until the owner sets them (no task or session limit, a 10 minute
/// idle stop), or `team_time_limits_invalid` for a stored bound outside 1 s to 24 h.
pub fn time_limits(
    connection: &Connection,
    project: &ProjectId,
) -> Result<TimeLimits, EngineError> {
    let stored: Option<(Option<i64>, Option<i64>, i64)> = connection
        .query_row(
            "SELECT turn_secs, session_secs, idle_secs FROM team_time_limits WHERE project=?1",
            [project.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()
        .map_err(error)?;
    let Some((turn, session, idle)) = stored else {
        return Ok(TimeLimits::default());
    };
    let bound = |secs: Option<i64>| match secs {
        None => Ok(None),
        Some(secs) => u32::try_from(secs)
            .ok()
            .filter(|secs| (1..=86_400).contains(secs))
            .map(Some)
            .ok_or_else(|| failure("team_time_limits_invalid")),
    };
    Ok(TimeLimits {
        turn: bound(turn)?,
        session: bound(session)?,
        idle: bound(Some(idle).filter(|secs| *secs != 0))?,
    })
}

/// What the project's live master does with a session's conversation when the master
/// harness changed since the session's last master ran: the owner's choice, carry over by
/// default. Takes a connection and the project. Returns the mode.
pub fn on_harness_switch(
    connection: &Connection,
    project: &ProjectId,
) -> Result<SwitchMode, EngineError> {
    let mode: Option<String> = connection
        .query_row(
            "SELECT mode FROM team_master_switch WHERE project=?1",
            [project.as_str()],
            |row| row.get(0),
        )
        .optional()
        .map_err(error)?;
    match mode.as_deref() {
        None => Ok(SwitchMode::default()),
        Some(name) => SwitchMode::named(name).ok_or_else(|| error("team_switch_mode_invalid")),
    }
}

/// When a saved master harness change reaches the project's running live master: the
/// owner's choice, when the master is idle by default. Takes a connection and the project.
/// Returns the choice.
pub fn apply_harness_switch(
    connection: &Connection,
    project: &ProjectId,
) -> Result<SwitchApply, EngineError> {
    let mode: Option<String> = connection
        .query_row(
            "SELECT mode FROM team_master_apply WHERE project=?1",
            [project.as_str()],
            |row| row.get(0),
        )
        .optional()
        .map_err(error)?;
    match mode.as_deref() {
        None => Ok(SwitchApply::default()),
        Some(name) => SwitchApply::named(name).ok_or_else(|| error("team_switch_apply_invalid")),
    }
}

/// The project's nested sub-agent settings: the owner's Sub-agent depth and where a lead's
/// worker lands, `DEPTH_DEFAULT` and `ChildLanding::Lead` until the owner picks others.
/// Takes a connection and the project. Returns the settings, or a SQLite failure.
pub fn nesting(connection: &Connection, project: &ProjectId) -> Result<Nesting, EngineError> {
    let row: Option<(i64, String)> = connection
        .query_row(
            "SELECT depth,lands FROM team_nesting WHERE project=?1",
            [project.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(error)?;
    let Some((depth, lands)) = row else {
        return Ok(Nesting {
            depth: DEPTH_DEFAULT,
            lands: ChildLanding::default(),
        });
    };
    let depth = u8::try_from(depth)
        .ok()
        .filter(|depth| DEPTH_CHOICES.contains(depth))
        .ok_or_else(|| error("team_depth_invalid"))?;
    let lands = ChildLanding::named(&lands).ok_or_else(|| error("team_landing_invalid"))?;
    Ok(Nesting { depth, lands })
}

/// How deep a sub-agent sits under the master: 1 for one the master started, one more for
/// each lead above it, counted up the recorded parents at most `DEPTH_WALK_MAX` steps.
/// Takes a connection and the agent. Returns its depth, or a SQLite failure.
pub fn agent_depth(connection: &Connection, agent: i64) -> Result<u8, EngineError> {
    let depth: Option<i64> = connection
        .query_row(
            "WITH RECURSIVE chain(id,parent,n) AS (
                 SELECT id,parent,1 FROM team_agents WHERE id=?1
                 UNION ALL
                 SELECT a.id,a.parent,chain.n+1 FROM team_agents a JOIN chain ON a.id=chain.parent
                 WHERE chain.n<?2)
             SELECT MAX(n) FROM chain",
            params![agent, DEPTH_WALK_MAX],
            |row| row.get(0),
        )
        .map_err(error)?;
    let depth = depth.ok_or_else(|| failure("team_agent_not_found"))?;
    assert!((1..=DEPTH_WALK_MAX).contains(&depth));
    Ok(u8::try_from(depth).expect("the walk is bounded"))
}

/// The most parent links `agent_depth` follows, past the deepest setting so a deeper chain
/// still reads as past every limit.
const DEPTH_WALK_MAX: i64 = 8;
const _: () = assert!(DEPTH_WALK_MAX > DEPTH_CHOICES[2] as i64);

/// The rows of a lead's native teammates that stopped or failed, the newest row of each
/// name only and none whose name a running row of the lead holds (team-20 r1 M1), as a SQL
/// test on `a` (team_agents) joined to `n` (team_teammate_names) with the project as ?1
/// and the lead as ?2, so the engine's save and the lead's finish read the same rows.
pub(crate) const ENDED_TEAMMATE: &str = "a.status IN ('stopped','failed')
     AND NOT EXISTS (
         SELECT 1 FROM team_agents b JOIN team_teammate_names m ON m.agent=b.id
         WHERE b.project=?1 AND b.parent=?2 AND b.role='teammate' AND m.name=n.name
         AND (b.status IN ('starting','idle','working') OR b.id>a.id))";

/// Gives team tables made before later columns those columns, older rows taking the
/// default: `team_agents.pane` (a native teammate's tmux pane id), `team_agents.mode` (the
/// sub-agent mode a lead started in, DEC-104), `team_tasks.mode` (the mode the master asked
/// for a lead's task), `team_agents.hookless` (1 when the agent's launch fires no hooks,
/// DEC-104 bare-hooks), `team_agents.pane_pid` (the pane process a native teammate was
/// first named by, PLC (b)), `team_task_pins.held` (1 when a hold pinned the task, not
/// its own purpose), `team_owner_stops.stopped_by` (who made the stop; one from before
/// the column was the owner's), `team_owner_stops.last_by` (who made the latest stop, whose
/// time the row holds; none from before the column, HB-S5-B-L2) and
/// `team_teammate_names.save_refused` (why the engine last gave up saving a teammate's
/// edits, empty once the lead's finish asked for another save, team-20 r2 R2-M1). Runs on
/// every open and adds each column once. Takes the engine connection. Returns any SQLite
/// failure.
fn add_late_columns(connection: &Connection) -> Result<(), EngineError> {
    for (table, column, definition) in [
        ("team_agents", "pane", "TEXT"),
        ("team_agents", "mode", "TEXT"),
        ("team_tasks", "mode", "TEXT"),
        ("team_agents", "hookless", "INTEGER NOT NULL DEFAULT 0"),
        ("team_agents", "pane_pid", "INTEGER"),
        ("team_task_pins", "held", "INTEGER NOT NULL DEFAULT 0"),
        (
            "team_owner_stops",
            "stopped_by",
            "TEXT NOT NULL DEFAULT 'owner'",
        ),
        ("team_owner_stops", "last_by", "TEXT"),
        (
            "team_teammate_names",
            "save_refused",
            "TEXT CHECK(save_refused IS NULL OR length(save_refused) <= 128)",
        ),
    ] {
        let present: bool = connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM pragma_table_info(?1) WHERE name=?2)",
                [table, column],
                |row| row.get(0),
            )
            .map_err(error)?;
        if !present {
            connection
                .execute_batch(&format!(
                    "ALTER TABLE {table} ADD COLUMN {column} {definition}"
                ))
                .map_err(error)?;
        }
    }
    Ok(())
}

/// The sub-agent mode an agent leads in (DEC-104), fixed at its first start: its row's
/// mode, else the project's setting for its harness kind when the master started it on a
/// kind with native teammates, else app-spawned, then stored so a later setting change
/// never moves it. Takes the connection, project and agent. Returns the mode, or a SQLite
/// failure or `team_lead_mode_invalid` for a stored name this build does not know.
pub(crate) fn lead_mode(
    connection: &Connection,
    project: &ProjectId,
    agent: i64,
) -> Result<crate::coord::settings::SubagentMode, EngineError> {
    use crate::coord::settings::{HarnessKind, SubagentMode, mode, native_supported};
    assert!(agent > 0);
    let read = |connection: &Connection| -> Result<(String, String, Option<i64>, Option<String>), EngineError> {
        connection
            .query_row(
                "SELECT role,harness,parent,mode FROM team_agents WHERE id=?1 AND project=?2",
                params![agent, project.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .map_err(error)
    };
    let (role, harness, parent, stored) = read(connection)?;
    let stored = match stored {
        Some(stored) => stored,
        None => {
            let chosen =
                match HarnessKind::from_harness(&harness).filter(|kind| native_supported(*kind)) {
                    Some(kind) if role == AgentRole::Sub.name() && parent.is_none() => {
                        mode(connection, project, kind)?
                    }
                    _ => SubagentMode::AppSpawned,
                };
            connection
                .execute(
                    "UPDATE team_agents SET mode=?2 WHERE id=?1 AND mode IS NULL",
                    params![agent, chosen.as_str()],
                )
                .map_err(error)?;
            read(connection)?.3.expect("the mode was just stored")
        }
    };
    SubagentMode::parse(&stored).ok_or_else(|| failure("team_lead_mode_invalid"))
}

/// Adds the parent agent column to the team task and agent tables (N1, migration 8), so
/// each task and agent records the lead that started it, NULL for the master's own. A
/// table the engine has not opened yet gets the column from `initialize`. Takes the
/// upgrade transaction. Returns any SQLite failure.
#[cfg(unix)]
pub(crate) fn add_parent_columns(tx: &Transaction<'_>) -> Result<(), EngineError> {
    for table in ["team_tasks", "team_agents"] {
        let exists: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name=?1)",
                [table],
                |row| row.get(0),
            )
            .map_err(error)?;
        if !exists {
            continue;
        }
        let present: bool = tx
            .query_row(
                &format!(
                    "SELECT EXISTS(SELECT 1 FROM pragma_table_info('{table}') WHERE name='parent')"
                ),
                [],
                |row| row.get(0),
            )
            .map_err(error)?;
        if !present {
            tx.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN parent INTEGER"))
                .map_err(error)?;
        }
    }
    Ok(())
}

/// Where the project's new sub-agents commit: on their own named branch, the default, or
/// on a detached HEAD, the owner's other choice. Takes a connection and the project.
/// Returns the mode.
pub fn agent_branches(
    connection: &Connection,
    project: &ProjectId,
) -> Result<BranchMode, EngineError> {
    let mode: Option<String> = connection
        .query_row(
            "SELECT mode FROM team_agent_branches WHERE project=?1",
            [project.as_str()],
            |row| row.get(0),
        )
        .optional()
        .map_err(error)?;
    match mode.as_deref() {
        None => Ok(BranchMode::default()),
        Some(name) => BranchMode::named(name).ok_or_else(|| error("team_branch_mode_invalid")),
    }
}

/// The branch the owner chose for the master to work on and merge harnesses into
/// (DEC-100), None for the repository's default branch. Takes a connection and the
/// project. Returns the choice.
pub fn master_branch_choice(
    connection: &Connection,
    project: &ProjectId,
) -> Result<Option<String>, EngineError> {
    let branch: Option<String> = connection
        .query_row(
            "SELECT branch FROM team_master_branch WHERE project=?1",
            [project.as_str()],
            |row| row.get(0),
        )
        .optional()
        .map_err(error)?;
    match branch {
        Some(branch) if !plain_branch_name(&branch) => Err(error("team_master_branch_invalid")),
        branch => Ok(branch),
    }
}

/// The branch the owner chose for the team's work to land on (DEC-111 Q1): the master's
/// merges, an Accept with no branch named and the measuring of change sets. None means
/// the Master's branch. Takes a connection and the project. Returns the choice.
pub fn land_on(
    connection: &Connection,
    project: &ProjectId,
) -> Result<Option<String>, EngineError> {
    let branch: Option<String> = connection
        .query_row(
            "SELECT branch FROM team_land_on WHERE project=?1",
            [project.as_str()],
            |row| row.get(0),
        )
        .optional()
        .map_err(error)?;
    match branch {
        Some(branch) if !plain_branch_name(&branch) => Err(error("team_land_on_invalid")),
        branch => Ok(branch),
    }
}

/// What the project's sub-agent tasks do at their turn end while commands their agents
/// started in the background still run (DEC-111 Q13): the owner's choice, a grace time of
/// [`BACKGROUND_GRACE_SECS_DEFAULT`] by default (DEC-119).
/// Takes a connection and the project. Returns the choice.
pub fn background_at_end(
    connection: &Connection,
    project: &ProjectId,
) -> Result<BackgroundAtEnd, EngineError> {
    let row: Option<(String, Option<u32>)> = connection
        .query_row(
            "SELECT mode,grace_secs FROM team_background WHERE project=?1",
            [project.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(error)?;
    match row {
        None => Ok(BackgroundAtEnd::default()),
        Some((mode, secs)) => {
            BackgroundAtEnd::named(&mode, secs).ok_or_else(|| error("team_background_invalid"))
        }
    }
}

/// The most owner messages one "accept all" check reads, newest first (DEC-147.7).
const ACCEPT_ALL_READ_MAX: i64 = 32;

/// Tells whether the owner sent "accept all" to the team in the app at or after a time
/// (DEC-147.7): an owner-authored team message whose whole text, trimmed and in any case, is
/// those two words with at most a closing full stop or exclamation mark. A master or engine
/// message never counts. Takes a connection, the project and the time in milliseconds.
/// Returns the answer, or any SQLite failure.
pub fn owner_said_accept_all(
    connection: &Connection,
    project: &ProjectId,
    since_ms: i64,
) -> Result<bool, EngineError> {
    let mut statement = connection
        .prepare(
            "SELECT text FROM team_messages WHERE project=?1 AND author=?2 AND created_ms>=?3
             ORDER BY id DESC LIMIT ?4",
        )
        .map_err(error)?;
    let texts = statement
        .query_map(
            params![
                project.as_str(),
                MessageAuthor::Owner.name(),
                since_ms,
                ACCEPT_ALL_READ_MAX
            ],
            |row| row.get::<_, String>(0),
        )
        .map_err(error)?;
    for text in texts {
        if says_accept_all(&text.map_err(error)?) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Tells whether one owner message is "accept all" and nothing else, so "don't accept all"
/// or a longer sentence never grants a merge. Takes the text. Returns the answer.
fn says_accept_all(text: &str) -> bool {
    let text = text.trim().trim_end_matches(['.', '!']).trim_end();
    let mut words = text.split_whitespace();
    matches!(
        (words.next(), words.next(), words.next()),
        (Some(first), Some(second), None)
            if first.eq_ignore_ascii_case("accept") && second.eq_ignore_ascii_case("all")
    )
}

/// Tells whether the owner's "Master accepts change sets" setting is Only when I click or say
/// accept all (DEC-147.7), so the master's merge of a finished harness leaves it as a pending
/// Accept unless the owner said accept all since the set last changed; off, Itself, by
/// default. Takes a connection and the project. Returns the setting.
pub fn ask_before_merge(connection: &Connection, project: &ProjectId) -> Result<bool, EngineError> {
    let ask: Option<bool> = connection
        .query_row(
            "SELECT ask FROM team_merge_ask WHERE project=?1",
            [project.as_str()],
            |row| row.get(0),
        )
        .optional()
        .map_err(error)?;
    Ok(ask.unwrap_or(false))
}

/// What a Full-access master does with the change set its own turn makes, such as a conflict
/// fix (DEC-117 Q2): merge it into the project's main itself, or leave it waiting for the
/// owner's Accept.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MasterLands {
    Merges,
    Waits,
}

impl MasterLands {
    /// The stored and sent name of the choice.
    pub fn name(self) -> &'static str {
        match self {
            Self::Merges => "merges",
            Self::Waits => "waits",
        }
    }

    /// Reads a stored or sent name. Input: the name. Output: the choice, or None.
    pub fn named(name: &str) -> Option<Self> {
        [Self::Merges, Self::Waits]
            .into_iter()
            .find(|mode| mode.name() == name)
    }
}

/// What the project's master does with its own change set, read at the moment it would land:
/// Waits whenever the project's access is not Full, else the owner's choice, Merges when none
/// was saved. Takes a connection and the project. Returns the choice.
pub fn master_lands(
    connection: &Connection,
    project: &ProjectId,
) -> Result<MasterLands, EngineError> {
    let access = crate::sandbox::policy::load(connection, project.as_str())
        .map_err(error)?
        .access;
    if access != crate::sandbox::policy::AgentAccess::Full {
        return Ok(MasterLands::Waits);
    }
    saved_master_lands(connection, project)
}

/// The owner's saved choice of what the master does with its own change set, as the team
/// status offers it: None below Full access, where the master always waits, and None while
/// the project's agent access cannot be read, so the rest of the status still reads and the
/// agents are told why they stopped; that case also says the access is unreadable, so the
/// owner is not told the project is below Full access. Takes a connection and the project.
/// Returns the choice and whether the access could not be read.
fn offered_master_lands(
    connection: &Connection,
    project: &ProjectId,
) -> Result<(Option<MasterLands>, bool), EngineError> {
    let access = match crate::sandbox::policy::load(connection, project.as_str()) {
        Ok(policy) => policy.access,
        Err(rusqlite::Error::FromSqlConversionFailure(..)) => return Ok((None, true)),
        Err(failed) => return Err(error(failed)),
    };
    if access != crate::sandbox::policy::AgentAccess::Full {
        return Ok((None, false));
    }
    saved_master_lands(connection, project).map(|lands| (Some(lands), false))
}

/// The owner's saved choice of what the master does with its own change set, Merges when
/// none was saved. Takes a connection and the project. Returns the choice.
fn saved_master_lands(
    connection: &Connection,
    project: &ProjectId,
) -> Result<MasterLands, EngineError> {
    let saved: Option<String> = connection
        .query_row(
            "SELECT mode FROM team_master_lands WHERE project=?1",
            [project.as_str()],
            |row| row.get(0),
        )
        .optional()
        .map_err(error)?;
    match saved {
        None => Ok(MasterLands::Merges),
        Some(name) => MasterLands::named(&name).ok_or_else(|| failure("team_master_lands_invalid")),
    }
}

/// Saves what the project's master does with its own change set. Takes a connection, the
/// project and the choice. Returns Ok once saved.
pub fn set_master_lands(
    connection: &Connection,
    project: &ProjectId,
    mode: MasterLands,
) -> Result<(), EngineError> {
    connection
        .execute(
            "INSERT INTO team_master_lands(project,mode) VALUES (?1,?2)
             ON CONFLICT(project) DO UPDATE SET mode=excluded.mode",
            params![project.as_str(), mode.name()],
        )
        .map_err(error)?;
    Ok(())
}

/// Tells whether the engine asks a sub-agent once more for its reply when a task turn
/// ends with no reply text, before it reports the task; on by default (DEC-111). Takes a
/// connection and the project. Returns the setting.
pub fn reask_empty_reply(
    connection: &Connection,
    project: &ProjectId,
) -> Result<bool, EngineError> {
    let reask: Option<bool> = connection
        .query_row(
            "SELECT reask FROM team_reask_empty WHERE project=?1",
            [project.as_str()],
            |row| row.get(0),
        )
        .optional()
        .map_err(error)?;
    Ok(reask.unwrap_or_else(reask_default))
}

/// The owner's "Stop the master when idle after" setting, per project (DEC-117 Q4). With no
/// saved choice each kind of master stays as it was: a live master is not stopped as idle
/// and any other stops after 15 minutes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MasterIdleStop {
    Off,
    #[default]
    FifteenMinutes,
    OneHour,
}

impl MasterIdleStop {
    /// The stored name of the setting. Output: `off`, `fifteen_minutes` or `one_hour`.
    pub fn name(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::FifteenMinutes => "fifteen_minutes",
            Self::OneHour => "one_hour",
        }
    }

    /// Reads a stored setting name. Input: the name. Output: the setting, or None.
    pub fn named(name: &str) -> Option<Self> {
        [Self::Off, Self::FifteenMinutes, Self::OneHour]
            .into_iter()
            .find(|value| value.name() == name)
    }

    /// The words the owner reads for the choice. Output: `Off`, `15 min` or `1 h`.
    pub fn label(self) -> &'static str {
        match self {
            Self::Off => "Off",
            Self::FifteenMinutes => "15 min",
            Self::OneHour => "1 h",
        }
    }

    /// What a project's saved choice means for one master. Inputs: the choice, none when the
    /// owner made none, and whether that master is live. Output: the choice in force.
    pub fn effective(chosen: Option<Self>, live: bool) -> Self {
        chosen.unwrap_or(if live {
            Self::Off
        } else {
            Self::FifteenMinutes
        })
    }

    /// How long this choice keeps an idle master. Input: the engine's idle bound, 15 minutes
    /// outside tests. Output: the time, none for Off.
    pub fn after(self, bound: std::time::Duration) -> Option<std::time::Duration> {
        match self {
            Self::Off => None,
            Self::FifteenMinutes => Some(bound),
            Self::OneHour => Some(bound.saturating_mul(MASTER_IDLE_HOUR_BOUNDS)),
        }
    }

    /// Says what the choice does to an idle master. Output: the sentence.
    pub fn sentence(self) -> String {
        match self {
            Self::Off => "An idle master keeps running until the team stops.".to_owned(),
            stop => format!(
                "An idle master stops {} after its work is done.",
                stop.label()
            ),
        }
    }
}

/// Reads a project's saved "Stop the master when idle after" choice. Takes a connection and
/// the project. Returns the choice, none when the owner made none.
pub fn master_idle_stop(
    connection: &Connection,
    project: &ProjectId,
) -> Result<Option<MasterIdleStop>, EngineError> {
    let name: Option<String> = connection
        .query_row(
            "SELECT choice FROM team_master_idle_stop WHERE project=?1",
            [project.as_str()],
            |row| row.get(0),
        )
        .optional()
        .map_err(error)?;
    name.map(|name| {
        MasterIdleStop::named(&name)
            .ok_or_else(|| EngineError("team_master_idle_stop_invalid".into()))
    })
    .transpose()
}

/// Saves a project's "Stop the master when idle after" choice, replacing an earlier one.
/// Takes a connection, the project and the choice. Returns done.
fn set_master_idle_stop(
    connection: &Connection,
    project: &ProjectId,
    choice: MasterIdleStop,
) -> Result<(), EngineError> {
    connection
        .execute(
            "INSERT INTO team_master_idle_stop(project,choice) VALUES (?1,?2)
             ON CONFLICT(project) DO UPDATE SET choice=excluded.choice",
            params![project.as_str(), choice.name()],
        )
        .map_err(error)?;
    Ok(())
}

/// The owner's "Catch up with main" setting (OS-8): `On`, the default, lets every agent's
/// own `git` merge `crazyagent/main`, the project's landed main, into its own branch and
/// tells it to; `Off` keeps the DEC-107 refusal of every merge and says nothing of main.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CatchUpMain {
    #[default]
    On,
    Off,
}

impl CatchUpMain {
    /// The stored name of the setting. Output: `on` or `off`.
    pub fn name(self) -> &'static str {
        match self {
            Self::On => "on",
            Self::Off => "off",
        }
    }

    /// Reads a stored setting name. Input: the name. Output: the setting, or None.
    pub fn named(name: &str) -> Option<Self> {
        [Self::On, Self::Off]
            .into_iter()
            .find(|value| value.name() == name)
    }
}

/// Reads a project's "Catch up with main" setting: `On` when no choice is saved and for a
/// store that keeps no team tables. Takes a connection and the project. Returns the
/// setting, or `team_catch_up_main_invalid` for a stored name it does not know.
pub fn catch_up_main(
    connection: &Connection,
    project: &ProjectId,
) -> Result<CatchUpMain, EngineError> {
    let kept: bool = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name='team_catch_up_main')",
            [],
            |row| row.get(0),
        )
        .map_err(error)?;
    if !kept {
        return Ok(CatchUpMain::default());
    }
    let mode: Option<String> = connection
        .query_row(
            "SELECT mode FROM team_catch_up_main WHERE project=?1",
            [project.as_str()],
            |row| row.get(0),
        )
        .optional()
        .map_err(error)?;
    match mode {
        None => Ok(CatchUpMain::default()),
        Some(mode) => {
            CatchUpMain::named(&mode).ok_or_else(|| failure("team_catch_up_main_invalid"))
        }
    }
}

/// Saves a project's "Catch up with main" setting, replacing an earlier choice. Takes a
/// connection and the project. Returns done.
pub fn set_catch_up_main(
    connection: &Connection,
    project: &ProjectId,
    value: CatchUpMain,
) -> Result<(), EngineError> {
    connection
        .execute(
            "INSERT INTO team_catch_up_main(project,mode) VALUES (?1,?2)
             ON CONFLICT(project) DO UPDATE SET mode=excluded.mode",
            params![project.as_str(), value.name()],
        )
        .map_err(error)?;
    Ok(())
}

/// The owner's "Reuse idle agents" setting (HB-S5-A): `Off`, the default, starts a new
/// agent for each new task, in its own worktree on the current branch; `On` lets an idle
/// agent of the same lead take it, on the history it already has.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReuseIdle {
    #[default]
    Off,
    On,
}

impl ReuseIdle {
    /// The stored name of the setting. Output: `off` or `on`.
    pub fn name(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::On => "on",
        }
    }

    /// Reads a stored setting name. Input: the name. Output: the setting, or None.
    pub fn named(name: &str) -> Option<Self> {
        [Self::Off, Self::On]
            .into_iter()
            .find(|value| value.name() == name)
    }
}

/// Reads a project's "Reuse idle agents" setting: `Off` when no choice is saved and for a
/// store that keeps no team tables. Takes a connection and the project. Returns the
/// setting, or `team_reuse_idle_invalid` for a stored name it does not know.
pub fn reuse_idle(connection: &Connection, project: &ProjectId) -> Result<ReuseIdle, EngineError> {
    let kept: bool = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name='team_reuse_idle')",
            [],
            |row| row.get(0),
        )
        .map_err(error)?;
    if !kept {
        return Ok(ReuseIdle::default());
    }
    let mode: Option<String> = connection
        .query_row(
            "SELECT mode FROM team_reuse_idle WHERE project=?1",
            [project.as_str()],
            |row| row.get(0),
        )
        .optional()
        .map_err(error)?;
    match mode {
        None => Ok(ReuseIdle::default()),
        Some(mode) => ReuseIdle::named(&mode).ok_or_else(|| failure("team_reuse_idle_invalid")),
    }
}

/// Saves a project's "Reuse idle agents" setting, replacing an earlier choice. Takes a
/// connection, the project and the setting. Returns done.
pub fn set_reuse_idle(
    connection: &Connection,
    project: &ProjectId,
    value: ReuseIdle,
) -> Result<(), EngineError> {
    connection
        .execute(
            "INSERT INTO team_reuse_idle(project,mode) VALUES (?1,?2)
             ON CONFLICT(project) DO UPDATE SET mode=excluded.mode",
            params![project.as_str(), value.name()],
        )
        .map_err(error)?;
    Ok(())
}

/// The owner's "Landing" setting (DEC-147.6): `Squash`, the default, lands each accepted
/// change set as one commit whose only parent is the branch tip; `Merge` lands the same
/// files as a merge commit whose second parent is the set's head, so every agent commit
/// stays in the branch's history and agents keep their ancestry.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LandingMode {
    #[default]
    Squash,
    Merge,
}

impl LandingMode {
    /// The stored name of the setting. Output: `squash` or `merge`.
    pub fn name(self) -> &'static str {
        match self {
            Self::Squash => "squash",
            Self::Merge => "merge",
        }
    }

    /// Reads a stored setting name. Input: the name. Output: the setting, or None.
    pub fn named(name: &str) -> Option<Self> {
        [Self::Squash, Self::Merge]
            .into_iter()
            .find(|value| value.name() == name)
    }
}

/// Reads a project's "Landing" setting: `Squash` when no choice is saved and for a store
/// that keeps no team tables. Takes a connection and the project. Returns the setting, or
/// `team_landing_mode_invalid` for a stored name it does not know.
pub fn landing_mode(
    connection: &Connection,
    project: &ProjectId,
) -> Result<LandingMode, EngineError> {
    let kept: bool = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name='team_landing_mode')",
            [],
            |row| row.get(0),
        )
        .map_err(error)?;
    if !kept {
        return Ok(LandingMode::default());
    }
    let mode: Option<String> = connection
        .query_row(
            "SELECT mode FROM team_landing_mode WHERE project=?1",
            [project.as_str()],
            |row| row.get(0),
        )
        .optional()
        .map_err(error)?;
    match mode {
        None => Ok(LandingMode::default()),
        Some(mode) => LandingMode::named(&mode).ok_or_else(|| failure("team_landing_mode_invalid")),
    }
}

/// Saves a project's "Landing" setting, replacing an earlier choice. Takes a connection,
/// the project and the setting. Returns done.
pub fn set_landing_mode(
    connection: &Connection,
    project: &ProjectId,
    value: LandingMode,
) -> Result<(), EngineError> {
    connection
        .execute(
            "INSERT INTO team_landing_mode(project,mode) VALUES (?1,?2)
             ON CONFLICT(project) DO UPDATE SET mode=excluded.mode",
            params![project.as_str(), value.name()],
        )
        .map_err(error)?;
    Ok(())
}

/// The most files one new top-level folder, a folder the owner's branch does not have,
/// may gain in one of the project's change sets before the engine refuses the set
/// (DEC-107); `NEW_FOLDER_FILES_DEFAULT` until the owner picks another. Takes a connection
/// and the project. Returns the limit.
pub fn new_folder_files_max(
    connection: &Connection,
    project: &ProjectId,
) -> Result<u32, EngineError> {
    let files: Option<u32> = connection
        .query_row(
            "SELECT files FROM team_new_folder_limit WHERE project=?1",
            [project.as_str()],
            |row| row.get(0),
        )
        .optional()
        .map_err(error)?;
    Ok(files.unwrap_or(NEW_FOLDER_FILES_DEFAULT))
}

/// Tells whether the owner chose to show finished agents (stopped and failed sub-agents)
/// in the agents list and team graph instead of their Finished fold-out.
/// Takes a connection and the project.
/// Returns false, the Hide default, until the owner picks Show.
pub fn show_finished(connection: &Connection, project: &ProjectId) -> Result<bool, EngineError> {
    connection
        .query_row(
            "SELECT show FROM team_finished_view WHERE project=?1",
            [project.as_str()],
            |row| row.get(0),
        )
        .optional()
        .map(|value| value.unwrap_or(false))
        .map_err(error)
}

/// The longest stored harness name a session switch row is read with.
const SWITCH_HARNESS_BYTES_MAX: i64 = 64;

/// Records that a session's master harness switched, replacing the session's earlier switch,
/// and stores the divider of the switch in the session's one conversation, so every view
/// shows where the new harness took over and the switch is settled. Takes a connection, the
/// project, the session, the new launch harness and the owner's mode. Returns nothing.
pub fn record_switch(
    connection: &Connection,
    project: &ProjectId,
    session: &str,
    harness: &str,
    mode: SwitchMode,
) -> Result<(), EngineError> {
    connection
        .execute(
            "INSERT INTO team_session_switches(session,project,harness,mode,created_ms)
             VALUES (?1,?2,?3,?4,?5)
             ON CONFLICT(session) DO UPDATE SET project=excluded.project,
                 harness=excluded.harness, mode=excluded.mode, created_ms=excluded.created_ms",
            params![session, project.as_str(), harness, mode.name(), now_ms()],
        )
        .map_err(error)?;
    crate::master_sessions::note_switch(
        connection,
        project.as_str(),
        session,
        crate::master_sessions::MasterMode::Live,
        harness,
        mode,
    )
}

/// The latest master harness switch of the project's live session, if it ever switched.
/// Takes a connection and the project. Returns the switch with its note, or None.
pub fn live_switch(
    connection: &Connection,
    project: &ProjectId,
) -> Result<Option<SessionSwitch>, EngineError> {
    let Some(session) = crate::master_sessions::live_session(connection, project)? else {
        return Ok(None);
    };
    let row: Option<(Option<String>, String)> = connection
        .query_row(
            "SELECT CASE WHEN length(CAST(harness AS BLOB))<=?3 THEN harness END,mode
             FROM team_session_switches WHERE session=?1 AND project=?2",
            params![session, project.as_str(), SWITCH_HARNESS_BYTES_MAX],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(error)?;
    let Some((Some(harness), mode)) = row else {
        return Ok(None);
    };
    let mode = SwitchMode::named(&mode).ok_or_else(|| error("team_switch_mode_invalid"))?;
    let harness = crate::master_sessions::harness_label(&harness).to_owned();
    let note = crate::master_sessions::switched_note(&harness, mode);
    Ok(Some(SessionSwitch {
        harness,
        mode,
        note,
    }))
}

/// Records the owner's Stop all for a project, which the runtime acts on at its next tick by
/// stopping the master and every agent of it. Inputs: the store, the project and the time
/// now. Output: Ok; a project already stopped keeps its first time.
pub(crate) fn pause(
    connection: &Connection,
    project: &ProjectId,
    now: i64,
) -> Result<(), EngineError> {
    connection
        .execute(
            "INSERT INTO team_paused_projects(project,since_ms) VALUES (?1,?2)
             ON CONFLICT(project) DO NOTHING",
            params![project.as_str(), now],
        )
        .map_err(error)?;
    Ok(())
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

/// The owner's Max teammates per harness for the project, for a lead's instructions and the
/// engine's own check. Takes a connection and the project. Returns the cap, None for No
/// limit, or a SQLite failure.
pub fn teammate_cap(
    connection: &Connection,
    project: &ProjectId,
) -> Result<Option<u8>, EngineError> {
    caps::read(connection, project).map(|caps| caps.teammates)
}

/// Tells whether the owner turned on Spread harnesses across accounts for the project, so a
/// new or woken agent prefers a pool account no running agent of the project uses. Takes a
/// connection and the project. Returns the switch, Off when unset, or a SQLite failure.
pub fn spread_accounts(connection: &Connection, project: &ProjectId) -> Result<bool, EngineError> {
    connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM team_account_spread WHERE project=?1)",
            [project.as_str()],
            |row| row.get(0),
        )
        .map_err(error)
}

/// Who made a stop, in rising standing: a stop holds against any waker below its maker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoppedBy {
    Lead,
    Master,
    Owner,
}

impl StoppedBy {
    /// The word stored for this maker. Returns it.
    fn name(self) -> &'static str {
        match self {
            Self::Lead => "lead",
            Self::Master => "master",
            Self::Owner => "owner",
        }
    }

    /// The maker a stored word names. Takes the word. Returns it, the owner for any word this
    /// engine does not know, as that stop is the one nobody else lifts.
    pub(crate) fn named(word: &str) -> Self {
        match word {
            "lead" => Self::Lead,
            "master" => Self::Master,
            _ => Self::Owner,
        }
    }
}

/// Why an agent cannot be stopped, if it cannot. Takes a connection, the project and the
/// agent. Returns a plain refusal code or None, or a SQLite failure.
fn stop_refusal(
    connection: &Connection,
    project: &ProjectId,
    agent: i64,
) -> Result<Option<&'static str>, EngineError> {
    let role: Option<String> = connection
        .query_row(
            "SELECT role FROM team_agents WHERE id=?1 AND project=?2",
            params![agent, project.as_str()],
            |row| row.get(0),
        )
        .optional()
        .map_err(error)?;
    Ok(
        match (role.as_deref(), role.as_deref().and_then(AgentRole::parse)) {
            (None, _) => Some("team_agent_not_found"),
            (_, Some(AgentRole::Master)) => Some("team_stop_master"),
            _ => None,
        },
    )
}

/// Records a stop made by the master or a lead (HB-S5-B), which never locks the agent to the
/// owner: its unfinished tasks fail and its maker or anyone above can wake it. Takes a
/// connection, the project, the agent, who stopped it and the time. Returns a plain refusal
/// code, or any SQLite failure.
pub(crate) fn agent_stop(
    connection: &Connection,
    project: &ProjectId,
    agent: i64,
    by: StoppedBy,
    now: i64,
) -> Result<Result<(), &'static str>, EngineError> {
    assert!(by != StoppedBy::Owner, "the owner stops through owner_stop");
    if let Some(code) = stop_refusal(connection, project, agent)? {
        return Ok(Err(code));
    }
    record_stop(connection, project, agent, by, now)?;
    Ok(Ok(()))
}

/// Records the owner's stop of a sub-agent and of every agent it leads, down its whole
/// lead tree, so each stays stopped across restarts until the owner wakes it or its lead.
/// Takes a connection, the project, the agent and the time. Returns any SQLite failure.
pub(crate) fn owner_stop(
    connection: &Connection,
    project: &ProjectId,
    agent: i64,
    now: i64,
) -> Result<(), EngineError> {
    record_stop(connection, project, agent, StoppedBy::Owner, now)
}

/// Records a stop of a sub-agent and of every agent it leads, down its whole lead tree. A
/// stop already there keeps the higher standing of its maker and this one and takes this
/// one's time and maker as the latest, so a wake queued before this stop fails and names
/// this maker as its cause (HB-S5-B-L2). Takes a connection, the project, the agent, who
/// stopped it and the time. Returns any SQLite failure.
fn record_stop(
    connection: &Connection,
    project: &ProjectId,
    agent: i64,
    by: StoppedBy,
    now: i64,
) -> Result<(), EngineError> {
    connection
        .execute(
            "WITH RECURSIVE led(id, depth) AS (
                 SELECT ?1, 0
                 UNION
                 SELECT a.id, led.depth + 1 FROM team_agents a JOIN led ON a.parent = led.id
                 WHERE a.project = ?2 AND a.id <> led.id AND led.depth < ?4
             )
             INSERT INTO team_owner_stops(agent,project,stopped_ms,stopped_by,last_by)
             SELECT id, ?2, ?3, ?5, ?5 FROM led WHERE true
             ON CONFLICT(agent) DO UPDATE SET stopped_ms=excluded.stopped_ms,
             last_by=excluded.stopped_by,
             stopped_by=CASE WHEN excluded.stopped_by='owner'
             OR (excluded.stopped_by='master' AND team_owner_stops.stopped_by='lead')
             THEN excluded.stopped_by ELSE team_owner_stops.stopped_by END",
            params![agent, project.as_str(), now, DEPTH_WALK_MAX, by.name()],
        )
        .map_err(error)?;
    Ok(())
}

/// Clears the owner's stop of an agent the owner woke and of every agent it leads, so a
/// woken harness brings its stopped sub-agents back with it: their held tasks wake them.
/// Only the owner's own Wake or restart of that agent clears the tree (AW-1); any other
/// start uses [`owner_start_alone`]. Takes a connection, the project and the agent.
/// Returns any SQLite failure.
pub(crate) fn owner_start(
    connection: &Connection,
    project: &ProjectId,
    agent: i64,
) -> Result<(), EngineError> {
    connection
        .execute(
            "WITH RECURSIVE led(id, depth) AS (
                 SELECT ?1, 0
                 UNION
                 SELECT a.id, led.depth + 1 FROM team_agents a JOIN led ON a.parent = led.id
                 WHERE a.project = ?2 AND a.id <> led.id AND led.depth < ?3
             )
             DELETE FROM team_owner_stops WHERE agent IN (SELECT id FROM led)",
            params![agent, project.as_str(), DEPTH_WALK_MAX],
        )
        .map_err(error)?;
    Ok(())
}

/// Clears the stop of one agent that starts again for anything but the owner's Wake or
/// restart of it, such as the owner's change-set Update or a pinned task. The stops of the
/// agents it leads stay, so each waits for whoever may wake it (AW-1). Takes a connection,
/// the project and the agent. Returns any SQLite failure.
pub(crate) fn owner_start_alone(
    connection: &Connection,
    project: &ProjectId,
    agent: i64,
) -> Result<(), EngineError> {
    assert!(agent > 0, "owner_start_alone_needs_a_stored_agent");
    connection
        .execute(
            "DELETE FROM team_owner_stops WHERE agent=?1 AND project=?2",
            params![agent, project.as_str()],
        )
        .map_err(error)?;
    Ok(())
}

/// The start of the usage hour that holds a time, as usage rows record it. Takes ms since
/// the epoch. Returns the hour's start in the same unit.
fn hour_of(ms: i64) -> i64 {
    ms - ms.rem_euclid(events::HOUR_MS)
}

/// Adds up the headline tokens the project's agents used in usage hours from `hour_ms` on:
/// new input, cache writes and output, never cache re-reads (DEC-117 Q6). Takes a
/// connection, the project and the hour start. Returns the total, 0 before any usage was
/// recorded, or a SQLite failure.
fn usage_from(
    connection: &Connection,
    project: &ProjectId,
    hour_ms: i64,
) -> Result<i64, EngineError> {
    let counted: bool = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name='usage_tokens')",
            [],
            |row| row.get(0),
        )
        .map_err(error)?;
    if !counted {
        return Ok(0);
    }
    let total: f64 = connection
        .query_row(
            "SELECT TOTAL(input)+TOTAL(output)+TOTAL(cache_write) FROM usage_tokens
             WHERE project=?1 AND hour_ms>=?2",
            params![project.as_str(), hour_ms],
            |row| row.get(0),
        )
        .map_err(error)?;
    Ok(total.clamp(0.0, i64::MAX as f64) as i64)
}

/// Reads the project's token budget and what its agents used against it: the tokens of the
/// usage hours since the budget's start, less what that first hour held at its start, so
/// the hour's earlier use never counts. Takes a connection and the project. Returns the
/// budget, None while it is Off, or a SQLite failure.
pub fn budget(connection: &Connection, project: &ProjectId) -> Result<Option<Budget>, EngineError> {
    let row: Option<(i64, i64, i64, Option<i64>)> = connection
        .query_row(
            "SELECT tokens,since_ms,baseline,reached_ms FROM team_token_budgets WHERE project=?1",
            [project.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()
        .map_err(error)?;
    let Some((tokens, since, baseline, reached)) = row else {
        return Ok(None);
    };
    let tokens = u64::try_from(tokens)
        .ok()
        .filter(|tokens| *tokens > 0)
        .ok_or_else(|| error("team_budget_invalid"))?;
    let used = usage_from(connection, project, hour_of(since))?
        .saturating_sub(baseline)
        .max(0);
    Ok(Some(Budget {
        tokens,
        used: u64::try_from(used).expect("used is never below zero"),
        reached: reached.is_some(),
    }))
}

/// Stops the project's whole team once its agents used the owner's token budget: the team
/// pauses as the owner's Stop all does, the budget records when, and the owner gets an app
/// notification. A budget that already stopped the team waits for the owner to start it
/// again. Takes a connection, the project and the time. Returns true when it stopped the
/// team now, or a SQLite failure.
pub(crate) fn budget_stop(
    connection: &Connection,
    project: &ProjectId,
    now: i64,
) -> Result<bool, EngineError> {
    let Some(budget) = budget(connection, project)? else {
        return Ok(false);
    };
    if budget.reached || budget.used < budget.tokens {
        return Ok(false);
    }
    let tx = events::immediate(connection)?;
    let changed = tx
        .execute(
            "UPDATE team_token_budgets SET reached_ms=?2 WHERE project=?1 AND reached_ms IS NULL",
            params![project.as_str(), now],
        )
        .map_err(error)?;
    if changed == 0 {
        return Ok(false);
    }
    tx.execute(
        "INSERT INTO team_paused_projects(project,since_ms) VALUES (?1,?2)
         ON CONFLICT(project) DO NOTHING",
        params![project.as_str(), now],
    )
    .map_err(error)?;
    let text = format!(
        "The team stopped: its agents used {} of the {} token budget. Raise the budget or start the team again to continue.",
        token_words(budget.used),
        token_words(budget.tokens)
    );
    events::notify(
        &tx,
        project,
        events::NotificationKind::BudgetStopped,
        &text,
        now,
    )?;
    tx.commit().map_err(error)?;
    Ok(true)
}

/// Words a token count for the owner: millions with one decimal at most, else the number.
/// Takes the count. Returns the words, such as `12.5M tokens`.
pub fn token_words(tokens: u64) -> String {
    if tokens < 1_000_000 {
        return format!("{tokens} tokens");
    }
    let tenths = tokens / 100_000;
    if tenths.is_multiple_of(10) {
        format!("{}M tokens", tenths / 10)
    } else {
        format!("{}.{}M tokens", tenths / 10, tenths % 10)
    }
}

/// Holds a pool account for one project for `pause` after an agent of that project reported a
/// rate or usage limit. The report comes from what the agent's harness wrote, which the agent
/// can write too, so only that project's team and research picks skip the account
/// (SECAUDIT-RATELIMIT-FORGE). The usage service is asked to poll, so a full window the
/// provider itself reports holds the account for every project. Takes a connection, the
/// project, the account's connection and the pause. Returns when the hold ends, in ms since
/// the epoch; a longer hold already set is kept.
pub(crate) fn hold_for_project(
    connection: &Connection,
    project: &str,
    account: &CredentialRef,
    pause: std::time::Duration,
) -> rusqlite::Result<i64> {
    assert!(!project.is_empty());
    assert!(!pause.is_zero());
    let now = now_ms();
    let until = now.saturating_add(i64::try_from(pause.as_millis()).unwrap_or(i64::MAX));
    assert!(until > now);
    connection.execute("DELETE FROM team_project_pauses WHERE until_ms<=?1", [now])?;
    let stored: i64 = connection.query_row(
        "INSERT INTO team_project_pauses(project,connection,until_ms) VALUES (?1,?2,?3)
         ON CONFLICT(project,connection) DO UPDATE SET until_ms=MAX(until_ms,excluded.until_ms)
         RETURNING until_ms",
        params![project, account.as_str(), until],
        |row| row.get(0),
    )?;
    assert!(stored >= until);
    crate::usage::service::request_confirm(connection, now)?;
    Ok(stored)
}

/// Tells until when a pool account stays held for one project: the later of its pause for
/// every project and the project's own hold after a limit its agents reported. Takes a
/// connection, the project and the account's connection. Returns the end in ms since the
/// epoch, or `None` when neither holds it now.
pub(crate) fn held_until(
    connection: &Connection,
    project: &str,
    account: &CredentialRef,
) -> rusqlite::Result<Option<i64>> {
    connection.query_row(
        "SELECT MAX(until_ms) FROM (
             SELECT until_ms FROM team_paused_accounts WHERE connection=?1 AND until_ms>?3
             UNION ALL
             SELECT until_ms FROM team_project_pauses
             WHERE project=?2 AND connection=?1 AND until_ms>?3)",
        params![account.as_str(), project, now_ms()],
        |row| row.get(0),
    )
}

/// The project's team status as the owner reads it: `status` with each agent's
/// [`message_notes`], fitted to one owner reply by `fit_status`. Takes a connection, the
/// stored setup and the model access, if any. Returns the status, partial when older
/// history had to go, or `team_status_bounds` when even an empty view does not fit.
pub fn owner_status(
    connection: &Connection,
    stored: &StoredSetup,
    access: Option<&ModelAccess>,
) -> Result<TeamStatus, EngineError> {
    let mut status = status(connection, stored)?;
    message_notes(connection, access, &mut status)?;
    fit_status(status, encoded_len::<TeamStatus>)
}

/// Says on each agent whether the project's Delivery setting leaves it no way to get its
/// team messages ([`crate::coord::delivery::no_channel`]); an agent runs a kept session when
/// its connection is a keyless endpoint, and one whose connection is gone from the model
/// access gets no note. Takes the connection, the model access and the status. Returns any
/// SQLite failure.
fn message_notes(
    connection: &Connection,
    access: Option<&ModelAccess>,
    status: &mut TeamStatus,
) -> Result<(), EngineError> {
    let project = status.project_id.clone();
    for agent in &mut status.agents {
        let kept = access
            .zip(agent.account.as_deref())
            .and_then(|(access, account)| {
                access
                    .connections
                    .iter()
                    .find(|found| found.id.as_str() == account)
            })
            .zip(launch_harness(&agent.harness))
            .and_then(|(found, harness)| reach(found, harness))
            .is_some_and(|reach| matches!(reach, TeamReach::Endpoint { .. }));
        agent.message_note = crate::coord::delivery::no_channel(
            connection,
            &project,
            agent.role,
            &agent.harness,
            agent.tools,
            kept,
        )?
        .map(str::to_owned);
    }
    Ok(())
}

/// The project's team graph as the owner reads it, built from a status with each agent's
/// [`message_notes`] that `fit_status` fitted until the graph fits one owner reply, with
/// the branches and coordination state `graph::facts` reads now. Takes a connection, the
/// stored setup and the model access, if any. Returns the graph, or `team_status_bounds`
/// when even an empty view does not fit.
pub fn owner_graph(
    connection: &Connection,
    stored: &StoredSetup,
    access: Option<&ModelAccess>,
) -> Result<TeamGraph, EngineError> {
    let mut status = status(connection, stored)?;
    message_notes(connection, access, &mut status)?;
    let facts = graph::facts(connection, &status, now_ms())?;
    let status = fit_status(status, |status| encoded_len(&graph(status, &facts)))?;
    Ok(graph(&status, &facts))
}

/// Fits a status to one owner reply. First every free-form text is cut to one shared cap,
/// halved until `size` of the status fits `STATUS_REPLY_BYTES_MAX`; a cut text says how many
/// bytes it left out. When the shortest cap still does not fit, the oldest finished history
/// goes, then anything oldest first, and the status is marked partial. Takes the status and
/// the size measure. Returns the status, or `team_status_bounds` when an empty view does not fit.
fn fit_status(
    status: TeamStatus,
    size: impl Fn(&TeamStatus) -> Result<usize, EngineError>,
) -> Result<TeamStatus, EngineError> {
    if size(&status)? <= STATUS_REPLY_BYTES_MAX {
        return Ok(status);
    }
    for cap in STATUS_TEXT_CAPS {
        let mut cut = status.clone();
        cut_texts(&mut cut, cap);
        if size(&cut)? <= STATUS_REPLY_BYTES_MAX {
            assert_eq!(cut.tasks.len(), status.tasks.len());
            assert_eq!(cut.messages.len(), status.messages.len());
            return Ok(cut);
        }
    }
    let mut cut = status;
    cut_texts(&mut cut, STATUS_TEXT_CAPS[STATUS_TEXT_CAPS.len() - 1]);
    cut.partial = true;
    for every in [false, true] {
        while drop_history(&mut cut, every) {
            if size(&cut)? <= STATUS_REPLY_BYTES_MAX {
                return Ok(cut);
            }
        }
    }
    Err(error("team_status_bounds"))
}

/// Cuts every free-form text of a status to `cap` bytes with `cut_text`: task prompts,
/// results and problems, messages, agent problems, plan prompts, the live master problem,
/// events, waiting questions and notifications. Takes the status and the cap.
fn cut_texts(status: &mut TeamStatus, cap: usize) {
    let cut = move |text: &mut String| *text = cut_text(text, cap);
    for task in &mut status.tasks {
        cut(&mut task.prompt);
        task.result
            .iter_mut()
            .chain(task.problem.iter_mut())
            .for_each(cut);
    }
    status.messages.iter_mut().for_each(|m| cut(&mut m.text));
    status
        .agents
        .iter_mut()
        .filter_map(|agent| agent.problem.as_mut())
        .for_each(cut);
    status
        .plan
        .iter_mut()
        .for_each(|step| cut(&mut step.prompt));
    status.live_problem.iter_mut().for_each(cut);
    let events = &mut status.events;
    events
        .recent
        .iter_mut()
        .for_each(|event| cut(&mut event.text));
    events
        .waiting
        .iter_mut()
        .for_each(|agent| cut(&mut agent.text));
    events
        .notifications
        .iter_mut()
        .for_each(|item| cut(&mut item.text));
}

/// Drops the older half of one list of a status, at least one entry, in this order: events,
/// seen notifications, finished messages, finished tasks and stopped agents; with `every`
/// also unseen notifications, waiting messages, live tasks and agents, the plan and the
/// waiting questions. Takes the status and whether live work may go. Returns whether
/// anything was dropped.
fn drop_history(status: &mut TeamStatus, every: bool) -> bool {
    let waiting = |state: MessageState| matches!(state, MessageState::Pending | MessageState::Sent);
    let events = &mut status.events;
    drop_older(&mut events.recent, false, |_| true)
        || drop_older(&mut events.notifications, true, |item| {
            every || item.seen_ms.is_some()
        })
        || drop_older(&mut status.messages, false, |message| {
            every || !waiting(message.state)
        })
        || drop_older(&mut status.tasks, false, |task| {
            every || task.status.finished()
        })
        || drop_older(&mut status.agents, false, |agent| {
            every || !agent.status.live()
        })
        || (every
            && (drop_older(&mut status.plan, false, |_| true)
                || drop_older(&mut events.waiting, false, |_| true)))
}

/// Drops the older half, at least one, of the entries of `items` that `droppable` picks.
/// Takes the list, whether it is ordered newest first, and the pick. Returns whether
/// anything was dropped.
fn drop_older<T>(items: &mut Vec<T>, newest_first: bool, droppable: impl Fn(&T) -> bool) -> bool {
    let picked: Vec<usize> = (0..items.len())
        .filter(|at| droppable(&items[*at]))
        .collect();
    let count = picked.len().div_ceil(2);
    if count == 0 {
        return false;
    }
    let gone = if newest_first {
        &picked[picked.len() - count..]
    } else {
        &picked[..count]
    };
    let before = items.len();
    let mut at = 0;
    items.retain(|_| {
        let kept = gone.binary_search(&at).is_err();
        at += 1;
        kept
    });
    assert_eq!(items.len(), before - count);
    true
}

/// Cuts text to `cap` bytes on a character boundary and, when anything was cut, ends it with
/// a note of how many bytes were left out. Takes the text and the cap. Returns the text,
/// whole when it fits.
fn cut_text(text: &str, cap: usize) -> String {
    if text.len() <= cap {
        return text.to_owned();
    }
    let kept = bound_text(text, cap);
    let left = text.len() - kept.len();
    assert!(left > 0 && kept.len() <= cap);
    format!("{kept} [... {left} more bytes]")
}

/// Measures a value as the JSON an owner reply carries. Takes the value. Returns its length
/// in bytes.
fn encoded_len<T: Serialize>(value: &T) -> Result<usize, EngineError> {
    Ok(serde_json::to_vec(value).map_err(error)?.len())
}

/// Reads a project's team plan use, agents, recent tasks and recent conversation.
/// Takes a connection and the stored setup.
/// Returns bounded lists in creation order.
pub fn status(connection: &Connection, stored: &StoredSetup) -> Result<TeamStatus, EngineError> {
    let project = &stored.project_id;
    let (master_lands, access_unreadable) = offered_master_lands(connection, project)?;
    let recent = |sql: &str, limit: i64| {
        format!("SELECT * FROM ({sql} ORDER BY id DESC LIMIT {limit}) ORDER BY id")
    };
    let mut statement = connection
        .prepare(&recent(
            &format!("SELECT {AGENT_COLUMNS} FROM team_agents WHERE project=?1"),
            STATUS_AGENTS_MAX,
        ))
        .map_err(error)?;
    let mut agents = statement
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
            .filter(|a| {
                a.role == role && crate::harnesses::same(&a.harness, harness) && a.status.live()
            })
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
        runnable: master_backend(&plan.master).is_some(),
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
    let events = events::view(connection, project)?;
    for agent in &mut agents {
        let questions = events
            .questions
            .iter()
            .find(|item| item.agent == agent.id)
            .map_or(0, |item| item.count);
        agent.finished = agent.finished_with(questions);
    }
    let waking: BTreeSet<i64> = {
        let mut statement = connection
            .prepare("SELECT agent FROM team_wake_requests WHERE project=?1")
            .map_err(error)?;
        statement
            .query_map([project.as_str()], |row| row.get(0))
            .map_err(error)?
            .collect::<Result<_, _>>()
            .map_err(error)?
    };
    for agent in agents.iter_mut().filter(|agent| agent.finished) {
        if agent.role == AgentRole::Sub
            && events::name_reaches_running(connection, project, agent.id)?
        {
            agent.label = AgentRole::Sub.agent_label(agent.id);
        }
        let refusal = wake_refusal(connection, project, agent.id)?;
        agent.wakeable = refusal.is_none() && !waking.contains(&agent.id);
        agent.wake_note = if waking.contains(&agent.id) {
            Some("Waking.".into())
        } else {
            refusal.map(wake_note)
        };
    }
    let stopped: BTreeSet<i64> = {
        let mut statement = connection
            .prepare("SELECT agent FROM team_owner_stops WHERE project=?1 AND stopped_by='owner'")
            .map_err(error)?;
        statement
            .query_map([project.as_str()], |row| row.get(0))
            .map_err(error)?
            .collect::<Result<_, _>>()
            .map_err(error)?
    };
    let changed = crate::instruction_files::badges(connection, project).map_err(error)?;
    for agent in &mut agents {
        agent.owner_stopped = stopped.contains(&agent.id);
        agent.instructions_changed = changed.contains(&agent.id) && agent.status.live();
        agent.teammates = teammates(connection, project, agent.id)?;
    }
    let (live_master, live_problem) = master::switch(connection, project)?;
    let live_idle_stopped =
        live_master == Some(false) && live_problem.as_deref().is_some_and(master::idle_stopped);
    let limits = time_limits(connection, project)?;
    let nested = nesting(connection, project)?;
    let limited = caps::read(connection, project)?;
    let spend = budget(connection, project)?;
    Ok(TeamStatus {
        project_id: project.clone(),
        keep_warm: keep_warm(connection, project)?,
        turn_limit_secs: limits.turn,
        session_limit_secs: limits.session,
        idle_stop_secs: limits.idle,
        on_harness_switch: on_harness_switch(connection, project)?,
        apply_harness_switch: apply_harness_switch(connection, project)?,
        sub_agent_depth: nested.depth,
        child_landing: nested.lands,
        show_finished: show_finished(connection, project)?,
        agent_branches: agent_branches(connection, project)?,
        master_branch: master_branch_choice(connection, project)?,
        land_on: land_on(connection, project)?,
        background_at_end: background_at_end(connection, project)?,
        ask_before_merge: ask_before_merge(connection, project)?,
        master_lands,
        access_unreadable,
        reask_empty_reply: reask_empty_reply(connection, project)?,
        master_idle_stop: MasterIdleStop::effective(
            master_idle_stop(connection, project)?,
            master::live(connection, project)?,
        ),
        master_live_terminal: master::live_terminal(connection, project)?,
        catch_up_main: catch_up_main(connection, project)?,
        reuse_idle: reuse_idle(connection, project)?,
        landing_mode: landing_mode(connection, project)?,
        new_folder_files_max: new_folder_files_max(connection, project)?,
        coordination: Some(coordination(connection, project)?),
        message_settings: agent_messages::settings(connection, project)?,
        live_switch: live_switch(connection, project)?,
        paused: paused(connection, project)?,
        max_harnesses: limited.harnesses,
        max_teammates: limited.teammates,
        token_budget: spend.map(|spend| spend.tokens),
        budget_used: spend.map(|spend| spend.used),
        budget_reached: spend.is_some_and(|spend| spend.reached),
        spread_accounts: spread_accounts(connection, project)?,
        terminals_per_window: terminals_per_window(connection, project)?,
        live_master,
        live_problem,
        live_idle_stopped,
        settings_version: settings_version(connection, project)?,
        min_agents: plan.min_agents,
        max_agents: plan.max_agents,
        harnesses,
        agents,
        tasks,
        messages,
        plan: steps,
        events,
        partial: false,
    })
}

/// Lists one harness's teammates as the status shows them: running ones first, then the
/// newest that ended, at most `TEAMMATES_SHOWN_MAX`. Takes a connection, the project and the
/// harness agent. Returns the teammates, or a SQLite failure.
fn teammates(
    connection: &Connection,
    project: &ProjectId,
    agent: i64,
) -> Result<Vec<Teammate>, EngineError> {
    let mut statement = connection
        .prepare(
            "SELECT pane,ended_ms IS NOT NULL,stopped_ms IS NOT NULL,seen_ms FROM team_teammates
             WHERE agent=?1 AND project=?2 ORDER BY ended_ms IS NOT NULL, COALESCE(ended_ms,seen_ms) DESC LIMIT ?3",
        )
        .map_err(error)?;
    let listed = statement
        .query_map(
            params![agent, project.as_str(), TEAMMATES_SHOWN_MAX as i64],
            |row| {
                Ok(Teammate {
                    pane: row.get(0)?,
                    ended: row.get(1)?,
                    stopped: row.get(2)?,
                    seen_ms: row.get(3)?,
                })
            },
        )
        .map_err(error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(error)?;
    assert!(listed.len() <= TEAMMATES_SHOWN_MAX);
    Ok(listed)
}

#[cfg(all(test, unix))]
mod tests;
