use super::client::{ConversationJob, CreationJob, OwnerEvent, SetupQuery};
use super::master_session;
use crate::config::ProviderQuotaPolicy;
use crate::engine::bootstrap::OwnerConnectionIdentity;
use crate::owner_profile::OwnerProfile;
use crate::pools::PoolSummary;
use crate::project_setup::{Platform, ResearchJob, ResearchStatus, StoredSetup};
use crate::protocol::{Command, CredentialRef, Cursor, ProjectId, StoredProject};
use crate::team::TeamGraph;
use crate::terminal::{AgentId, TerminalFrame};

/// A pool change asked for in Settings, Pools or a Project. The engine side
/// builds the exact pool records, keeping an existing pool's projects and roles;
/// an update carries the form as the editor opened it, so only what the owner
/// changed is applied onto the pool as it is when saved.
/// Input: built by the pool list, the pool editor and the Project screen.
/// Output: one model access edit; `Assign` gives the project exactly the chosen
/// pool, or none, and `Remove` also takes the pool off every project using it; it
/// carries how many projects its confirmation showed and is refused when that changed.
/// `Create` carries the id its form drew once, so a retry of the same form never adds
/// a second pool.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PoolChange {
    Create {
        id: CredentialRef,
        name: String,
        members: Vec<CredentialRef>,
        quota: ProviderQuotaPolicy,
    },
    Update {
        id: CredentialRef,
        name: String,
        members: Vec<CredentialRef>,
        quota: ProviderQuotaPolicy,
        opened_name: String,
        opened_members: Vec<CredentialRef>,
        opened_quota: ProviderQuotaPolicy,
    },
    Remove {
        id: CredentialRef,
        opened_projects: usize,
    },
    Assign {
        pool: Option<CredentialRef>,
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

/// One account as lists show it: never a secret, only plain labels, and for a key,
/// URL or local account what it has in place of a 5-hour or weekly window.
/// Input: built from the engine's account list. Output: list, pool editor and
/// usage rows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccountRow {
    pub id: CredentialRef,
    pub name: String,
    pub provider: String,
    pub status: String,
    pub no_window: Option<&'static str>,
}

/// The words an account goes by wherever a screen shows it outside its own row in Accounts:
/// the name the owner gave it, with the account id after it when another account has the
/// same name, so two accounts never read the same. Inputs: the account id and the owner's
/// account rows. Output: the words, or none for an id with no row.
pub fn account_label(id: &str, accounts: &[AccountRow]) -> Option<String> {
    let own = accounts.iter().find(|account| account.id.as_str() == id)?;
    let uses = accounts
        .iter()
        .filter(|account| account.name == own.name)
        .count();
    assert!(uses >= 1, "the account's own row has its name");
    Some(account_words(own, uses > 1))
}

/// The words of every account at once ([`account_label`]), in one pass over the rows, for a
/// screen that names many accounts. Input: the owner's account rows. Output: the words by
/// account id.
pub fn account_labels(accounts: &[AccountRow]) -> std::collections::HashMap<String, String> {
    let mut uses: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for account in accounts {
        *uses.entry(account.name.as_str()).or_default() += 1;
    }
    let labels: std::collections::HashMap<String, String> = accounts
        .iter()
        .map(|account| {
            let shared = uses[account.name.as_str()] > 1;
            (
                account.id.as_str().to_owned(),
                account_words(account, shared),
            )
        })
        .collect();
    assert!(labels.len() <= accounts.len());
    labels
}

fn account_words(account: &AccountRow, shared: bool) -> String {
    if shared {
        format!("{} ({})", account.name, account.id.as_str())
    } else {
        account.name.clone()
    }
}

/// The problem code a request gets while the engine has no support for it yet.
/// Screens show "Not ready yet" for it instead of a failure.
pub const NOT_READY: &str = "engine_support_not_ready";

/// The problem code a request gets when the full request queue refused it, so it never
/// reached the engine and nothing changed.
pub const OWNER_BUSY: &str = "owner_busy";

/// One engine request a screen asks the shell to send.
/// Input: built by screens and queued with `Ctx::request`. Output: sent one at a
/// time on the owner connection; the reply comes back as an `Arrival`.
#[expect(
    dead_code,
    reason = "project setup, creation and conversation requests are built by the project screens"
)]
pub enum Request {
    Profile,
    SaveProfile {
        profile: OwnerProfile,
    },
    Pools,
    PoolProjects,
    Projects,
    EditPool {
        change: PoolChange,
    },
    Setup(SetupQuery),
    Creation(Box<CreationJob>),
    Conversation(Box<ConversationJob>),
    Master(Box<master_session::Job>),
    Wire {
        command: Command,
        revision: Cursor,
    },
    Settings(Box<super::client::settings::SettingsJob>),
    TeamGraph {
        project: ProjectId,
    },
    ProjectFiles {
        project: ProjectId,
        query: String,
    },
    TerminalAttach {
        agent: AgentId,
    },
    TerminalAttachPane {
        agent: AgentId,
        pane: String,
    },
    TerminalInput {
        agent: AgentId,
        bytes: Vec<u8>,
    },
    TerminalResize {
        agent: AgentId,
        cols: u16,
        rows: u16,
    },
    TerminalDetach {
        agent: AgentId,
    },
    Intake(Box<super::client::intake::IntakeJob>),
    Changes(Box<super::client::changes::ChangesQuery>),
    Process(Box<super::client::process::ProcessQuery>),
    Recovery(Box<super::client::recovery::RecoveryQuery>),
    Roadmap(Box<super::client::roadmap::RoadmapQuery>),
    Questions(Box<super::client::questions::QuestionsQuery>),
    Cleanup(Box<super::client::cleanup::CleanupQuery>),
    Reveal(Box<super::client::ide::RevealQuery>),
    Learning(Box<super::client::learning::LearningQuery>),
    Reconnect,
}

impl Request {
    /// Says whether the request goes to an agent terminal, which streams on
    /// its own connection instead of waiting for the engine connection.
    /// Input: the request. Output: true for attach, input, resize and detach.
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::TerminalAttach { .. }
                | Self::TerminalAttachPane { .. }
                | Self::TerminalInput { .. }
                | Self::TerminalResize { .. }
                | Self::TerminalDetach { .. }
        )
    }
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
            Self::ProjectFiles { .. } => "ProjectFiles",
            Self::TerminalAttach { .. } => "TerminalAttach",
            Self::TerminalAttachPane { .. } => "TerminalAttachPane",
            Self::TerminalInput { .. } => "TerminalInput",
            Self::TerminalResize { .. } => "TerminalResize",
            Self::TerminalDetach { .. } => "TerminalDetach",
            Self::Intake(_) => "Intake",
            Self::Changes(_) => "Changes",
            Self::Process(_) => "Process",
            Self::Recovery(_) => "Recovery",
            Self::Roadmap(_) => "Roadmap",
            Self::Questions(_) => "Questions",
            Self::Cleanup(_) => "Cleanup",
            Self::Reveal(_) => "Reveal",
            Self::Learning(_) => "Learning",
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
        result: Result<Box<TeamGraph>, String>,
    },
    ProjectFiles {
        project: ProjectId,
        query: String,
        result: Result<Vec<String>, String>,
    },
    Terminal {
        agent: AgentId,
        frame: TerminalFrame,
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
    pub rate_limited: usize,
    pub failed: usize,
}

impl ResearchCounts {
    /// Counts research jobs by status, a job waiting out a rate limit apart from one that
    /// needs a pool. Input: the jobs. Output: the counts; cancelled jobs are not counted.
    pub fn of(jobs: &[ResearchJob]) -> Self {
        let mut counts = Self::default();
        for job in jobs {
            match job.status {
                ResearchStatus::Done => counts.done += 1,
                ResearchStatus::Running => counts.running += 1,
                ResearchStatus::Queued => counts.queued += 1,
                ResearchStatus::NeedsPool if job.rate_limited().is_some() => {
                    counts.rate_limited += 1;
                }
                ResearchStatus::NeedsPool => counts.needs_pool += 1,
                ResearchStatus::Failed => counts.failed += 1,
                ResearchStatus::Cancelled => {}
            }
        }
        counts
    }

    /// The plain research summary on a project card.
    /// Input: none. Output: "Research: 2 done, 1 running", "Research: needs a
    /// pool", "Research: rate limited" or "No research".
    pub fn summary(&self) -> String {
        if self.needs_pool > 0 {
            return "Research: needs a pool".into();
        }
        if self.rate_limited > 0 {
            return "Research: rate limited".into();
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

/// A project's live team work, memory suggestions and new team notifications
/// for its Home card. `working` is None when the team read failed or is not
/// ready, so the card shows only research; `suggestions` counts memories
/// waiting for review; `notifications` counts the team's unseen notifications.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Activity {
    pub working: Option<usize>,
    pub suggestions: usize,
    pub notifications: usize,
}

/// A fresh pool id from its name: `pool-<letters>-<random hex>`.
/// Input: the pool name. Output: the id, or `pool_id_unavailable`.
pub fn new_pool_id(name: &str) -> Result<CredentialRef, String> {
    let stem: String = name
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(24)
        .flat_map(|character| character.to_lowercase())
        .collect();
    let nonce = crate::engine::owner::random_id().map_err(|_| "pool_id_unavailable".to_owned())?;
    CredentialRef::new(format!("pool-{stem}-{nonce}")).map_err(|_| "pool_id_unavailable".into())
}

/// One project as Home shows it, built only from engine data.
/// Input: the stored project, its setup and research jobs, and pool names.
/// Output: the card fields in plain words, and the assigned pool's id, by which the
/// pool screens find the projects of one pool, as two pools may share a name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectCard {
    pub id: ProjectId,
    pub name: String,
    pub about: String,
    pub platforms: Vec<Platform>,
    pub pool: Option<String>,
    pub pool_id: Option<CredentialRef>,
    pub research: ResearchCounts,
    pub created_ms: Option<i64>,
    pub activity: Activity,
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
            pool_id: setup.and_then(|setup| setup.pool.clone()),
            research: ResearchCounts::of(jobs),
            created_ms: setup.map(|setup| setup.created_ms),
            activity: Activity::default(),
        }
    }

    /// Counts the project's agents at work: research agents running plus
    /// team agents working, as the desktop dashboard counts them.
    /// Input: the card. Output: the count.
    pub fn working(&self) -> usize {
        self.research.running + self.activity.working.unwrap_or(0)
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
