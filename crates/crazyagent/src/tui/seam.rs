use super::client::{ConversationJob, CreationJob, OwnerEvent, SetupQuery};
use super::master_session;
use crate::engine::bootstrap::OwnerConnectionIdentity;
use crate::owner_profile::OwnerProfile;
use crate::pools::PoolSummary;
use crate::project_setup::{Platform, ResearchJob, ResearchStatus, StoredSetup};
use crate::protocol::{Command, CredentialRef, Cursor, ProjectId, StoredProject};

/// A pool change asked for in Settings, Pools or a Project. The engine side
/// builds the exact pool records, keeping an existing pool's projects and roles.
/// Input: built by the pool list, the pool editor and the Project screen.
/// Output: model access edits; `Assign` moves the project out of every other
/// pool before adding it to the chosen one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PoolChange {
    Create {
        name: String,
        members: Vec<CredentialRef>,
    },
    Update {
        id: CredentialRef,
        name: String,
        members: Vec<CredentialRef>,
    },
    Remove {
        id: CredentialRef,
    },
    Assign {
        pool: CredentialRef,
        project: ProjectId,
    },
}

/// The projects one pool serves, as saved in the settings document.
/// Input: read from the engine's pool records. Output: lets the Project screen
/// tell an assigned pool from a picked one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PoolScope {
    pub pool: CredentialRef,
    pub projects: Vec<CredentialRef>,
}

/// One account as lists show it: never a secret, only plain labels.
/// Input: built from the engine's account list. Output: list and pool editor
/// rows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccountRow {
    pub id: CredentialRef,
    pub name: String,
    pub provider: String,
    pub status: String,
}

/// The problem code a request gets while the engine has no support for it yet.
/// Screens show "Not ready yet" for it instead of a failure.
pub const NOT_READY: &str = "engine_support_not_ready";

/// The kind of one node in a project's team graph.
/// Input: from the engine's TeamGraph read. Output: the tree draws the master
/// first, then one group per harness, each with its agents.
#[expect(
    dead_code,
    reason = "the client builds team graphs from track B1's TeamGraph read"
)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NodeKind {
    Master,
    HarnessGroup,
    Agent,
}

/// What one agent is doing, as the engine reads it from the harness session.
/// Input: from the engine's TeamGraph read. Output: a status glyph and word.
#[expect(
    dead_code,
    reason = "the client builds team graphs from track B1's TeamGraph read"
)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentState {
    Working,
    Idle,
    Waiting,
    Failed,
}

/// One node of a project's team graph: the master, a harness group or an agent.
/// Input: from the engine's TeamGraph read; `harness` ties agents to their
/// group, `agents` is a group's agent count. Output: one tree row.
#[expect(
    dead_code,
    reason = "the client builds team graphs from track B1's TeamGraph read"
)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GraphNode {
    pub id: String,
    pub kind: NodeKind,
    pub label: String,
    pub harness: Option<String>,
    pub status: Option<AgentState>,
    pub account: Option<String>,
    pub agents: Option<u8>,
}

/// Where one handed-out task stands.
/// Input: from the engine's TeamGraph read. Output: a status glyph and word.
#[expect(
    dead_code,
    reason = "the client builds team graphs from track B1's TeamGraph read"
)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EdgeState {
    Sent,
    Running,
    Done,
    Failed,
}

/// One task the master handed to an agent, from node id to node id.
/// Input: from the engine's TeamGraph read. Output: one task line under the
/// tree; `text` and `reply` show when the owner opens it.
#[expect(
    dead_code,
    reason = "the client builds team graphs from track B1's TeamGraph read"
)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GraphEdge {
    pub from: String,
    pub to: String,
    pub task_id: String,
    pub text: String,
    pub state: EdgeState,
    pub reply: Option<String>,
}

/// The live team of one project: its nodes and the tasks between them.
/// Input: from the engine's TeamGraph read. Output: the Team view tree.
#[expect(
    dead_code,
    reason = "the client builds team graphs from track B1's TeamGraph read"
)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TeamGraph {
    pub nodes: Vec<GraphNode>,
    pub edges: Vec<GraphEdge>,
}

/// One piece of an attached agent terminal's stream.
/// Input: from the engine's terminal stream. Output: `Screen` replaces the
/// pane with the captured screen and its zero-based (row, column) cursor,
/// `Output` is raw bytes to parse, `Closed` ends the stream with a problem code.
#[expect(
    dead_code,
    reason = "the client builds terminal chunks from track B1's terminal stream"
)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TerminalChunk {
    Screen {
        bytes: Vec<u8>,
        cursor: Option<(u16, u16)>,
    },
    Output(Vec<u8>),
    Closed(String),
}

/// One engine request a screen asks the shell to send.
/// Input: built by screens and queued with `Ctx::request`. Output: sent one at a
/// time on the owner connection; the reply comes back as an `Arrival`.
#[expect(
    dead_code,
    reason = "project setup, creation and conversation requests are built by the project screens"
)]
pub enum Request {
    Profile,
    SaveProfile { profile: OwnerProfile },
    Pools,
    PoolProjects,
    Projects,
    EditPool { change: PoolChange },
    Setup(SetupQuery),
    Creation(Box<CreationJob>),
    Conversation(Box<ConversationJob>),
    Master(Box<master_session::Job>),
    Wire { command: Command, revision: Cursor },
    Settings(Box<super::client::settings::SettingsJob>),
    TeamGraph { project: ProjectId },
    TerminalAttach { agent: String },
    TerminalInput { agent: String, bytes: Vec<u8> },
    TerminalResize { agent: String, cols: u16, rows: u16 },
    TerminalDetach { agent: String },
    Intake(Box<super::client::intake::IntakeJob>),
    Reconnect,
}

impl std::fmt::Debug for Request {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Profile => "Profile",
            Self::SaveProfile { .. } => "SaveProfile",
            Self::Pools => "Pools",
            Self::PoolProjects => "PoolProjects",
            Self::Projects => "Projects",
            Self::EditPool { .. } => "EditPool",
            Self::Setup(_) => "Setup",
            Self::Creation(_) => "Creation",
            Self::Conversation(_) => "Conversation",
            Self::Master(_) => "Master",
            Self::Wire { .. } => "Wire",
            Self::Settings(_) => "Settings",
            Self::TeamGraph { .. } => "TeamGraph",
            Self::TerminalAttach { .. } => "TerminalAttach",
            Self::TerminalInput { .. } => "TerminalInput",
            Self::TerminalResize { .. } => "TerminalResize",
            Self::TerminalDetach { .. } => "TerminalDetach",
            Self::Intake(_) => "Intake",
            Self::Reconnect => "Reconnect",
        })
    }
}

/// A shell request run through the owner controller.
/// Input: built from a `Request` by the shell. Output: executed on the engine
/// worker thread; the result comes back as a `ShellEvent`.
#[derive(Clone)]
pub(in crate::tui) enum ShellRequest {
    Profile,
    SaveProfile {
        profile: OwnerProfile,
        generation: u64,
    },
    Pools,
    PoolProjects,
    Projects,
    EditPool {
        change: PoolChange,
        generation: u64,
    },
}

/// The result of one shell request, with the engine's plain problem code on
/// failure.
/// Input: produced by the engine worker. Output: turned into an `Arrival`.
pub(in crate::tui) enum ShellEvent {
    Profile(Result<Option<OwnerProfile>, String>),
    ProfileSaved(Result<(), String>),
    Pools(Result<Vec<PoolSummary>, String>),
    PoolProjects(Result<Vec<PoolScope>, String>),
    Projects(Result<Vec<ProjectCard>, String>),
    PoolEdited(Result<(), String>),
}

/// Something that arrived from the engine, broadcast to every open screen.
/// Input: an engine reply. Output: screens react; the shell updates `Store`.
#[expect(
    dead_code,
    reason = "owner and wire replies are read by the project screens"
)]
pub enum Arrival {
    Connected(Option<OwnerConnectionIdentity>),
    Disconnected(String),
    Profile(Result<Option<OwnerProfile>, String>),
    ProfileSaved(Result<(), String>),
    Pools(Result<Vec<PoolSummary>, String>),
    PoolProjects(Result<Vec<PoolScope>, String>),
    Projects(Result<Vec<ProjectCard>, String>),
    PoolEdited(Result<(), String>),
    Owner(Box<OwnerEvent>),
    Wire(Result<crate::protocol::WireReply, String>),
    TeamGraph {
        project: ProjectId,
        result: Result<TeamGraph, String>,
    },
    Terminal {
        agent: String,
        chunk: TerminalChunk,
    },
}

/// Research progress of one project, counted by status.
/// Input: the project's research jobs. Output: counts per status.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ResearchCounts {
    pub done: usize,
    pub running: usize,
    pub queued: usize,
    pub needs_pool: usize,
    pub failed: usize,
}

impl ResearchCounts {
    /// Counts research jobs by status.
    /// Input: the jobs. Output: the counts; cancelled jobs are not counted.
    pub fn of(jobs: &[ResearchJob]) -> Self {
        let mut counts = Self::default();
        for job in jobs {
            match job.status {
                ResearchStatus::Done => counts.done += 1,
                ResearchStatus::Running => counts.running += 1,
                ResearchStatus::Queued => counts.queued += 1,
                ResearchStatus::NeedsPool => counts.needs_pool += 1,
                ResearchStatus::Failed => counts.failed += 1,
                ResearchStatus::Cancelled => {}
            }
        }
        counts
    }

    /// The plain research summary on a project card.
    /// Input: none. Output: "Research: 2 done, 1 running", "Research: needs a
    /// pool" or "No research".
    pub fn summary(&self) -> String {
        if self.needs_pool > 0 {
            return "Research: needs a pool".into();
        }
        let parts: Vec<String> = [
            (self.done, "done"),
            (self.running, "running"),
            (self.queued, "queued"),
            (self.failed, "failed"),
        ]
        .iter()
        .filter(|(count, _)| *count > 0)
        .map(|(count, word)| format!("{count} {word}"))
        .collect();
        if parts.is_empty() {
            "No research".into()
        } else {
            format!("Research: {}", parts.join(", "))
        }
    }
}

/// One project as Home shows it, built only from engine data.
/// Input: the stored project, its setup and research jobs, and pool names.
/// Output: the card fields in plain words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectCard {
    pub id: ProjectId,
    pub name: String,
    pub about: String,
    pub platforms: Vec<Platform>,
    pub pool: Option<String>,
    pub research: ResearchCounts,
    pub created_ms: Option<i64>,
}

impl ProjectCard {
    /// Builds a card from engine records.
    /// Inputs: the stored project, its setup if any, its jobs and the pools.
    /// Output: the card; a pool the engine no longer lists shows by its name
    /// only when known.
    pub fn build(
        project: &StoredProject,
        setup: Option<&StoredSetup>,
        jobs: &[ResearchJob],
        pools: &[PoolSummary],
    ) -> Self {
        let pool = setup
            .and_then(|setup| setup.pool.as_ref())
            .map(|reference| {
                pools
                    .iter()
                    .find(|pool| &pool.id == reference)
                    .map_or_else(|| "A removed pool".to_owned(), |pool| pool.name.clone())
            });
        Self {
            id: project.project_id.clone(),
            name: project.spec.name.clone(),
            about: setup.map(|setup| setup.about.clone()).unwrap_or_default(),
            platforms: setup
                .map(|setup| setup.platforms.clone())
                .unwrap_or_default(),
            pool,
            research: ResearchCounts::of(jobs),
            created_ms: setup.map(|setup| setup.created_ms),
        }
    }
}

impl ShellEvent {
    /// The failure event for a request that reached no trusted connection.
    /// Input: the request. Output: its event carrying the plain problem code.
    pub(in crate::tui) fn unavailable(request: &ShellRequest) -> Self {
        let problem = "trusted_connection_unavailable".to_owned();
        match request {
            ShellRequest::Profile => Self::Profile(Err(problem)),
            ShellRequest::SaveProfile { .. } => Self::ProfileSaved(Err(problem)),
            ShellRequest::Pools => Self::Pools(Err(problem)),
            ShellRequest::PoolProjects => Self::PoolProjects(Err(problem)),
            ShellRequest::Projects => Self::Projects(Err(problem)),
            ShellRequest::EditPool { .. } => Self::PoolEdited(Err(problem)),
        }
    }
}
