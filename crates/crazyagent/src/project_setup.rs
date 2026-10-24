use crate::config::{EffectiveConfig, ModelAuthentication};
use crate::engine::EngineError;
use crate::protocol::{CredentialRef, Outcome, ProjectId, ProjectSpec};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
#[cfg(unix)]
use rustix::fs::{AtFlags, FileType, Mode, OFlags};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
#[cfg(unix)]
use std::io::{Read, Write};
#[cfg(unix)]
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};

pub const ABOUT_BYTES_MAX: usize = 8192;
pub const TECH_ENTRIES_MAX: usize = 32;
pub const TECH_ENTRY_BYTES_MAX: usize = 64;
pub const DOCUMENTS_MAX: usize = 16;
pub const DOCUMENT_PATH_BYTES_MAX: usize = 1024;
pub const DOCUMENT_NAME_BYTES_MAX: usize = 255;
pub const DOCUMENT_BYTES_MAX: u64 = 50 * 1024 * 1024;
pub const PROJECT_DOCUMENT_BYTES_MAX: u64 = 200 * 1024 * 1024;
pub const DOCUMENT_EXTENSIONS: [&str; 8] =
    ["pdf", "md", "txt", "docx", "csv", "png", "jpg", "jpeg"];
pub const HARNESS_BYTES_MAX: usize = 64;
pub const SUB_HARNESSES_MAX: usize = 8;
pub const HARNESS_AGENTS_MAX: u8 = 32;
pub const TEAM_AGENTS_MAX: u8 = 64;
pub const CONTEXT_ITEMS_MAX: usize = 64;
pub const CONTEXT_CHUNK_BYTES: usize = 8192;
pub const REPORT_BYTES_MAX: u64 = 1024 * 1024;
pub const CONTEXT_DIRECTORY: &str = "context";
pub const PROJECTS_DIRECTORY: &str = "projects";
#[cfg(unix)]
const BRIEF_PARTIAL: &str = ".brief.partial";
pub const RESEARCH_LOG_LINES_MAX: usize = 200;
pub const RESEARCH_LOG_LINE_CHARS_MAX: usize = 400;
pub const RESEARCH_LOG_BYTES_MAX: usize = 24 * 1024;
#[cfg(unix)]
const RESEARCH_LOG_TAIL_BYTES: u64 = 64 * 1024;
#[cfg(unix)]
const RESEARCH_LOG_FILES: [&str; 3] = ["events.jsonl", "output.md", "stderr.log"];
#[cfg(unix)]
const RESEARCH_RUNS_SCANNED_MAX: usize = 4096;
#[cfg(unix)]
const SECRET_MARKERS: [&str; 10] = [
    "api_key",
    "api-key",
    "apikey",
    "secret",
    "password",
    "authorization",
    "bearer ",
    "access_token",
    "refresh_token",
    "id_token",
];
#[cfg(unix)]
const SECRET_PREFIXES: [&str; 13] = [
    "sk-",
    "sk_",
    "pk_",
    "rk_",
    "ghp_",
    "gho_",
    "ghs_",
    "github_pat_",
    "xoxb-",
    "xoxp-",
    "glpat-",
    "AKIA",
    "eyJ",
];
#[cfg(unix)]
const HIDDEN_LINE: &str = "[hidden: this line may contain a secret]";
#[cfg(unix)]
const HIDDEN_WORD: &str = "[hidden]";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Platform {
    Website,
    MobileApp,
    DesktopApp,
    Cli,
    ApiBackend,
    Other,
}

impl Platform {
    pub const ALL: [Platform; 6] = [
        Self::Website,
        Self::MobileApp,
        Self::DesktopApp,
        Self::Cli,
        Self::ApiBackend,
        Self::Other,
    ];

    /// Names the platform in plain language for every client.
    /// Takes the platform value.
    /// Returns its short label.
    pub fn label(self) -> &'static str {
        match self {
            Self::Website => "Website",
            Self::MobileApp => "Mobile app",
            Self::DesktopApp => "Desktop app",
            Self::Cli => "CLI",
            Self::ApiBackend => "API or backend",
            Self::Other => "Other",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResearchKind {
    MarketResearch,
    CompetitorResearch,
    DesignResearch,
    FrontEndDesign,
    TechStackResearch,
}

impl ResearchKind {
    pub const ALL: [ResearchKind; 5] = [
        Self::MarketResearch,
        Self::CompetitorResearch,
        Self::DesignResearch,
        Self::FrontEndDesign,
        Self::TechStackResearch,
    ];

    /// Names the research toggle in plain language for every client.
    /// Takes the research kind.
    /// Returns its short label.
    pub fn label(self) -> &'static str {
        match self {
            Self::MarketResearch => "Market research",
            Self::CompetitorResearch => "Competitor research",
            Self::DesignResearch => "Design research",
            Self::FrontEndDesign => "Front-end design",
            Self::TechStackResearch => "Tech stack research",
        }
    }

    /// Gives the stable storage and file name of the research kind.
    /// Takes the research kind.
    /// Returns the snake case name used for jobs and report files.
    pub fn name(self) -> &'static str {
        match self {
            Self::MarketResearch => "market_research",
            Self::CompetitorResearch => "competitor_research",
            Self::DesignResearch => "design_research",
            Self::FrontEndDesign => "front_end_design",
            Self::TechStackResearch => "tech_stack_research",
        }
    }

    /// Parses a stored research kind name.
    /// Takes the snake case name.
    /// Returns the kind, or none for an unknown name.
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.name() == name)
    }

    /// Gives the report path of this kind relative to the project folder.
    /// Takes the research kind.
    /// Returns `context/research/<kind>.md`.
    pub fn report_path(self) -> String {
        format!("{CONTEXT_DIRECTORY}/research/{}.md", self.name())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TechGroup {
    Language,
    Framework,
    Database,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TechOption {
    pub id: String,
    pub label: String,
    pub group: TechGroup,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LabeledOption<T> {
    pub kind: T,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HarnessSlot {
    pub harness: String,
    pub agents: u8,
    pub tools: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TeamPlan {
    pub master: String,
    pub master_tools: bool,
    pub sub_harnesses: Vec<HarnessSlot>,
    pub min_agents: u8,
    pub max_agents: u8,
}

impl TeamPlan {
    /// Validates harness identifiers, agent counts and the agent range.
    /// Takes an untrusted team plan.
    /// Returns a plain rejection code when any bound fails.
    pub fn validate(&self) -> Result<(), EngineError> {
        if !harness_valid(&self.master) {
            return Err(failure("team_harness_invalid"));
        }
        if self.sub_harnesses.len() > SUB_HARNESSES_MAX {
            return Err(failure("team_harnesses_too_many"));
        }
        for slot in &self.sub_harnesses {
            if !harness_valid(&slot.harness) {
                return Err(failure("team_harness_invalid"));
            }
            if !(1..=HARNESS_AGENTS_MAX).contains(&slot.agents) {
                return Err(failure("team_agent_count_invalid"));
            }
        }
        let total = self.total_agents();
        if self.min_agents == 0
            || self.min_agents > self.max_agents
            || self.max_agents > TEAM_AGENTS_MAX
            || total < u32::from(self.min_agents)
            || total > u32::from(self.max_agents)
        {
            return Err(failure("team_agent_range_invalid"));
        }
        Ok(())
    }

    /// Counts the master and every sub-harness agent.
    /// Takes the team plan.
    /// Returns the total number of agents the plan describes.
    pub fn total_agents(&self) -> u32 {
        1 + self
            .sub_harnesses
            .iter()
            .map(|slot| u32::from(slot.agents))
            .sum::<u32>()
    }

    fn research_harnesses(&self) -> Vec<(&str, bool)> {
        let slots: Vec<(&str, bool)> = self
            .sub_harnesses
            .iter()
            .flat_map(|slot| {
                std::iter::repeat_n(
                    (slot.harness.as_str(), slot.tools),
                    usize::from(slot.agents),
                )
            })
            .collect();
        if slots.is_empty() {
            vec![(self.master.as_str(), self.master_tools)]
        } else {
            slots
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectSetup {
    pub about: String,
    pub tech_stack: Vec<String>,
    pub platforms: Vec<Platform>,
    pub research: Vec<ResearchKind>,
    pub pool: Option<CredentialRef>,
    pub team: Option<TeamPlan>,
    pub documents: Vec<String>,
}

impl ProjectSetup {
    /// Validates every field of a new project request without touching the filesystem.
    /// Takes an untrusted setup from a client.
    /// Returns a plain rejection code when any bound fails.
    pub fn validate(&self) -> Result<(), EngineError> {
        if self.about.len() > ABOUT_BYTES_MAX
            || self
                .about
                .chars()
                .any(|c| c.is_control() && c != '\n' && c != '\t')
        {
            return Err(failure("project_about_invalid"));
        }
        if self.tech_stack.len() > TECH_ENTRIES_MAX {
            return Err(failure("tech_stack_too_long"));
        }
        let mut seen = BTreeSet::new();
        for entry in &self.tech_stack {
            if entry.trim().is_empty()
                || entry.len() > TECH_ENTRY_BYTES_MAX
                || entry.chars().any(char::is_control)
                || !seen.insert(entry.trim().to_lowercase())
            {
                return Err(failure("tech_stack_entry_invalid"));
            }
        }
        if self.platforms.iter().collect::<BTreeSet<_>>().len() != self.platforms.len() {
            return Err(failure("platforms_duplicate"));
        }
        if self.research.iter().collect::<BTreeSet<_>>().len() != self.research.len() {
            return Err(failure("research_duplicate"));
        }
        if self.documents.len() > DOCUMENTS_MAX {
            return Err(failure("documents_too_many"));
        }
        for path in &self.documents {
            if path.len() > DOCUMENT_PATH_BYTES_MAX
                || path.chars().any(char::is_control)
                || !Path::new(path).is_absolute()
            {
                return Err(failure("document_path_invalid"));
            }
        }
        if let Some(team) = &self.team {
            team.validate()?;
        }
        assert!(self.platforms.len() <= Platform::ALL.len());
        assert!(self.research.len() <= ResearchKind::ALL.len());
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoredDocument {
    pub name: String,
    pub bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoredSetup {
    pub project_id: ProjectId,
    pub name: String,
    pub about: String,
    pub tech_stack: Vec<String>,
    pub platforms: Vec<Platform>,
    pub research: Vec<ResearchKind>,
    pub pool: Option<CredentialRef>,
    pub team: TeamPlan,
    pub documents: Vec<StoredDocument>,
    pub folder: String,
    pub created_ms: i64,
    #[serde(default)]
    pub revision: u64,
}

impl StoredSetup {
    /// The folder holding this project's context and agent worktrees:
    /// `.crazyagent/` inside the project for projects in the projects home,
    /// or the project folder itself for projects made before it.
    pub fn state_folder(&self) -> PathBuf {
        crate::projects_home::state_folder(Path::new(&self.folder))
    }

    /// The project's own folder: the recorded folder, or the folder holding
    /// it when onboarding recorded the project's `.crazyagent/` folder.
    pub fn project_folder(&self) -> PathBuf {
        let folder = Path::new(&self.folder);
        match folder.parent() {
            Some(parent) if folder.ends_with(crate::projects_home::CRAZYAGENT_DIRECTORY) => {
                parent.to_path_buf()
            }
            _ => folder.to_path_buf(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectCatalog {
    pub tech_stack: Vec<TechOption>,
    pub platforms: Vec<LabeledOption<Platform>>,
    pub research: Vec<LabeledOption<ResearchKind>>,
    pub document_extensions: Vec<String>,
    pub documents_max: usize,
    pub document_bytes_max: u64,
    pub project_document_bytes_max: u64,
    pub default_team: TeamPlan,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_team_status: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResearchStatus {
    Queued,
    NeedsPool,
    Running,
    Done,
    Failed,
    Cancelled,
}

impl ResearchStatus {
    pub const ALL: [ResearchStatus; 6] = [
        Self::Queued,
        Self::NeedsPool,
        Self::Running,
        Self::Done,
        Self::Failed,
        Self::Cancelled,
    ];

    /// Names the job status in plain language.
    /// Takes the status.
    /// Returns its label, for example "Needs a pool".
    pub fn label(self) -> &'static str {
        match self {
            Self::Queued => "Queued",
            Self::NeedsPool => "Needs a pool",
            Self::Running => "Running",
            Self::Done => "Done",
            Self::Failed => "Failed",
            Self::Cancelled => "Cancelled",
        }
    }

    fn stored(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::NeedsPool => "needs_pool",
            Self::Running => "running",
            Self::Done => "done",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    fn parse(value: &str) -> Result<Self, EngineError> {
        Self::ALL
            .into_iter()
            .find(|status| status.stored() == value)
            .ok_or_else(|| failure("research_status_corrupt"))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResearchJob {
    pub id: i64,
    pub project_id: ProjectId,
    pub kind: ResearchKind,
    pub kind_label: String,
    pub status: ResearchStatus,
    pub status_label: String,
    pub report_path: String,
    pub harness: String,
    pub tools: bool,
    pub attempts: u32,
    pub claimer: Option<String>,
    pub error: Option<String>,
    pub cancel_requested: bool,
    pub output_bytes: u64,
    pub created_ms: i64,
    pub started_ms: Option<i64>,
    pub heartbeat_ms: Option<i64>,
    pub finished_ms: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextKind {
    Brief,
    Document,
    Research,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextItem {
    pub path: String,
    pub kind: ContextKind,
    pub bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectContext {
    pub project_id: ProjectId,
    pub folder: String,
    pub items: Vec<ContextItem>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextChunk {
    pub path: String,
    pub bytes: u64,
    pub offset: u64,
    pub text: Option<String>,
    pub next_offset: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SetupQuery {
    Catalog {
        pool: Option<CredentialRef>,
        research: Vec<ResearchKind>,
    },
    Setup {
        project: ProjectId,
    },
    Jobs {
        project: ProjectId,
    },
    Context {
        project: ProjectId,
    },
    Item {
        project: ProjectId,
        path: String,
        offset: u64,
    },
    Log {
        project: ProjectId,
        job: i64,
    },
    Change {
        action: SetupAction,
        generation: u64,
    },
    Memories {
        project: ProjectId,
        after: Option<crate::protocol::RequestId>,
    },
    ReviewMemory {
        project: ProjectId,
        entry_id: crate::protocol::RequestId,
        version: u64,
        decision: crate::memory::ReviewDecision,
        generation: u64,
    },
    Team {
        project: ProjectId,
    },
    TeamChange {
        action: crate::team::TeamAction,
        generation: u64,
    },
    Library(crate::memory::library::LibraryQuery),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SetupView {
    Catalog(Box<ProjectCatalog>),
    Setup(Option<Box<StoredSetup>>),
    Jobs(Vec<ResearchJob>),
    Context(ProjectContext),
    Item(ContextChunk),
    Log(Vec<String>),
    Changed(crate::engine::owner::OwnerResult),
    Memories(Vec<crate::memory::KnowledgeEntry>),
    Team(Box<crate::team::TeamStatus>),
    Library(crate::memory::library::LibraryView),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", deny_unknown_fields)]
pub enum SetupAction {
    Create {
        project_id: ProjectId,
        spec: ProjectSpec,
        setup: Box<ProjectSetup>,
    },
    SaveToMemory {
        project_id: ProjectId,
        research: ResearchKind,
        section: Option<String>,
    },
    AddDocuments {
        project_id: ProjectId,
        documents: Vec<String>,
    },
    DeleteContext {
        project_id: ProjectId,
        path: String,
    },
    Update {
        project_id: ProjectId,
        expected_revision: u64,
        name: Option<String>,
        setup: ProjectSetup,
    },
    Remove {
        project_id: ProjectId,
    },
    CancelResearch {
        project_id: ProjectId,
        job: i64,
    },
    RetryResearch {
        project_id: ProjectId,
        job: i64,
    },
}

impl SetupAction {
    /// Validates the action before it reaches storage.
    /// Takes an untrusted owner action.
    /// Returns a plain rejection code when any bound fails.
    pub fn validate(&self) -> Result<(), EngineError> {
        match self {
            Self::Create { spec, setup, .. } => {
                spec.validate().map_err(|e| failure(&e.0))?;
                setup.validate()
            }
            Self::SaveToMemory { section, .. } => {
                if section.as_ref().is_some_and(|s| {
                    s.trim().is_empty() || s.len() > 256 || s.chars().any(char::is_control)
                }) {
                    return Err(failure("research_section_invalid"));
                }
                Ok(())
            }
            Self::AddDocuments { documents, .. } => ProjectSetup {
                about: String::new(),
                tech_stack: Vec::new(),
                platforms: Vec::new(),
                research: Vec::new(),
                pool: None,
                team: None,
                documents: documents.clone(),
            }
            .validate()
            .and_then(|()| {
                if documents.is_empty() {
                    return Err(failure("documents_empty"));
                }
                Ok(())
            }),
            Self::DeleteContext { path, .. } => {
                if path.len() > 512 || context_kind(path).is_none() {
                    return Err(failure("context_path_invalid"));
                }
                Ok(())
            }
            Self::Update { name, setup, .. } => {
                let renamed = name.as_ref().map(|name| ProjectSpec {
                    name: name.clone(),
                    repositories: Vec::new(),
                });
                if renamed.is_some_and(|spec| spec.validate().is_err()) {
                    return Err(failure("project_spec_bounds"));
                }
                if !setup.documents.is_empty() {
                    return Err(failure("project_setup_bounds"));
                }
                setup.validate()
            }
            Self::Remove { .. } => Ok(()),
            Self::CancelResearch { job, .. } | Self::RetryResearch { job, .. } => {
                if *job <= 0 {
                    return Err(failure("research_job_missing"));
                }
                Ok(())
            }
        }
    }
}

fn failure(code: &str) -> EngineError {
    EngineError(code.into())
}

fn error(value: impl std::fmt::Display) -> EngineError {
    EngineError(value.to_string())
}

fn harness_valid(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= HARNESS_BYTES_MAX
        && value.as_bytes()[0].is_ascii_alphanumeric()
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

const DEFAULT_TEAM_HARNESS: &str = "codex";
const SIGNED_IN_TEAM_HARNESSES: [&str; 3] = ["codex", "claude", "claude-code"];
const TEAM_NEEDS_POOL: &str = "Needs a pool";
const TEAM_NEEDS_SIGN_IN: &str = "Needs a signed-in account";

const TECH_CATALOG: [(&str, &str, TechGroup); 30] = [
    ("typescript", "TypeScript", TechGroup::Language),
    ("javascript", "JavaScript", TechGroup::Language),
    ("python", "Python", TechGroup::Language),
    ("rust", "Rust", TechGroup::Language),
    ("go", "Go", TechGroup::Language),
    ("java", "Java", TechGroup::Language),
    ("kotlin", "Kotlin", TechGroup::Language),
    ("swift", "Swift", TechGroup::Language),
    ("csharp", "C#", TechGroup::Language),
    ("ruby", "Ruby", TechGroup::Language),
    ("php", "PHP", TechGroup::Language),
    ("react", "React", TechGroup::Framework),
    ("nextjs", "Next.js", TechGroup::Framework),
    ("vue", "Vue", TechGroup::Framework),
    ("svelte", "Svelte", TechGroup::Framework),
    ("angular", "Angular", TechGroup::Framework),
    ("react-native", "React Native", TechGroup::Framework),
    ("flutter", "Flutter", TechGroup::Framework),
    ("tauri", "Tauri", TechGroup::Framework),
    ("electron", "Electron", TechGroup::Framework),
    ("django", "Django", TechGroup::Framework),
    ("fastapi", "FastAPI", TechGroup::Framework),
    ("rails", "Ruby on Rails", TechGroup::Framework),
    ("spring", "Spring", TechGroup::Framework),
    ("postgresql", "PostgreSQL", TechGroup::Database),
    ("mysql", "MySQL", TechGroup::Database),
    ("sqlite", "SQLite", TechGroup::Database),
    ("mongodb", "MongoDB", TechGroup::Database),
    ("redis", "Redis", TechGroup::Database),
    ("supabase", "Supabase", TechGroup::Database),
];

/// Serves the curated New Project choices and the smart team defaults.
/// Takes the configuration, the chosen pool, the research toggles and the signed-in connections.
/// Returns the catalog, or `pool_missing` for a pool that does not exist.
pub fn catalog(
    config: &EffectiveConfig,
    pool: Option<&CredentialRef>,
    research: &[ResearchKind],
    signed_in: &BTreeSet<CredentialRef>,
) -> Result<ProjectCatalog, &'static str> {
    if pool.is_some_and(|pool| !pool_exists(config, pool)) {
        return Err("pool_missing");
    }
    if research.len() > ResearchKind::ALL.len() {
        return Err("research_duplicate");
    }
    let catalog = ProjectCatalog {
        tech_stack: TECH_CATALOG
            .iter()
            .map(|(id, label, group)| TechOption {
                id: (*id).into(),
                label: (*label).into(),
                group: *group,
            })
            .collect(),
        platforms: Platform::ALL
            .into_iter()
            .map(|kind| LabeledOption {
                kind,
                label: kind.label().into(),
            })
            .collect(),
        research: ResearchKind::ALL
            .into_iter()
            .map(|kind| LabeledOption {
                kind,
                label: kind.label().into(),
            })
            .collect(),
        document_extensions: DOCUMENT_EXTENSIONS.iter().map(|e| (*e).into()).collect(),
        documents_max: DOCUMENTS_MAX,
        document_bytes_max: DOCUMENT_BYTES_MAX,
        project_document_bytes_max: PROJECT_DOCUMENT_BYTES_MAX,
        default_team: default_team(config, pool, research, signed_in),
        default_team_status: default_team_status(config, pool, signed_in).map(str::to_owned),
    };
    assert!(catalog.default_team.validate().is_ok());
    Ok(catalog)
}

/// Whether the settings define the pool `pool`.
pub(crate) fn pool_exists(config: &EffectiveConfig, pool: &CredentialRef) -> bool {
    config
        .settings
        .model_access
        .as_ref()
        .is_some_and(|access| access.pools.iter().any(|p| &p.id == pool))
}

/// Finds the pool's natively managed connections whose latest sign-in observed an account.
/// Takes a transaction over the engine database, the configuration and the pool.
/// Returns the signed-in connection ids; an absent pool or sign-in table gives none.
pub fn signed_in_connections(
    tx: &Transaction<'_>,
    config: &EffectiveConfig,
    pool: Option<&CredentialRef>,
) -> Result<BTreeSet<CredentialRef>, EngineError> {
    let mut signed = BTreeSet::new();
    let Some((access, pool)) = pool.and_then(|pool| {
        let access = config.settings.model_access.as_ref()?;
        Some((access, access.pools.iter().find(|p| &p.id == pool)?))
    }) else {
        return Ok(signed);
    };
    for connection in access
        .connections
        .iter()
        .filter(|c| pool.members.contains(&c.id))
    {
        let ModelAuthentication::NativeManaged { profile_ref } = &connection.auth else {
            continue;
        };
        let key = crate::memory::hash(profile_ref.as_str().as_bytes());
        let phase: Option<String> = tx
            .query_row(
                "SELECT phase FROM native_auth_operations WHERE profile_key=?1
                ORDER BY generation DESC LIMIT 1",
                [&key],
                |row| row.get(0),
            )
            .optional()
            .map_err(error)?;
        if phase.as_deref() == Some("authenticated_observed") {
            signed.insert(connection.id.clone());
        }
    }
    assert!(signed.len() <= pool.members.len());
    Ok(signed)
}

/// Reports whether a pool with members is explicitly assigned to the project.
/// Takes the effective configuration and the project.
/// Returns true only for an assignment present in the settings document.
pub fn project_has_pool(config: &EffectiveConfig, project: &ProjectId) -> bool {
    config.settings.model_access.as_ref().is_some_and(|access| {
        access.pools.iter().any(|pool| {
            !pool.members.is_empty() && pool.projects.iter().any(|p| p.as_str() == project.as_str())
        })
    })
}

/// Builds the smart team defaults for the New Project Team step.
/// Takes the configuration, the chosen pool, the research toggles and the signed-in connections.
/// Returns the pool's best signed-in harness, Codex before Claude Code, as master and as one
/// research agent per toggle, tools on; Codex when the pool has no signed-in harness, never RCLI.
pub fn default_team(
    config: &EffectiveConfig,
    pool: Option<&CredentialRef>,
    research: &[ResearchKind],
    signed_in: &BTreeSet<CredentialRef>,
) -> TeamPlan {
    let master = best_signed_in_harness(config, pool, signed_in)
        .unwrap_or(DEFAULT_TEAM_HARNESS)
        .to_owned();
    assert!(master != "rcli" && harness_valid(&master));
    let agents = u8::try_from(research.len().min(usize::from(HARNESS_AGENTS_MAX))).unwrap_or(1);
    let sub_harnesses = if agents == 0 {
        Vec::new()
    } else {
        vec![HarnessSlot {
            harness: master.clone(),
            agents,
            tools: true,
        }]
    };
    let team = TeamPlan {
        master,
        master_tools: true,
        sub_harnesses,
        min_agents: 1,
        max_agents: 1 + agents,
    };
    assert!(team.validate().is_ok());
    team
}

/// Finds the best harness among the pool's signed-in managed connections that can lead a
/// project and run its research. Takes the configuration, the chosen pool and the signed-in
/// connections. Returns the harness id, Codex before Claude Code, or none without one.
fn best_signed_in_harness<'a>(
    config: &'a EffectiveConfig,
    pool: Option<&CredentialRef>,
    signed_in: &BTreeSet<CredentialRef>,
) -> Option<&'a str> {
    let access = config.settings.model_access.as_ref()?;
    let pool = access.pools.iter().find(|p| Some(&p.id) == pool)?;
    access
        .connections
        .iter()
        .filter(|c| {
            pool.members.contains(&c.id)
                && signed_in.contains(&c.id)
                && matches!(c.auth, ModelAuthentication::NativeManaged { .. })
        })
        .filter_map(|c| {
            SIGNED_IN_TEAM_HARNESSES
                .iter()
                .position(|harness| *harness == c.harness.as_str())
                .map(|rank| (rank, c.harness.as_str()))
        })
        .min()
        .map(|(_, harness)| harness)
}

/// Says in plain words why the default team cannot start yet.
/// Takes the configuration, the chosen pool and the signed-in connections.
/// Returns "Needs a pool" without a pool that has members, "Needs a signed-in account" when no
/// member can run the default harness, or none when the default team can start.
pub fn default_team_status(
    config: &EffectiveConfig,
    pool: Option<&CredentialRef>,
    signed_in: &BTreeSet<CredentialRef>,
) -> Option<&'static str> {
    let Some((access, chosen)) = pool
        .and_then(|pool| {
            let access = config.settings.model_access.as_ref()?;
            Some((access, access.pools.iter().find(|p| &p.id == pool)?))
        })
        .filter(|(_, chosen)| !chosen.members.is_empty())
    else {
        return Some(TEAM_NEEDS_POOL);
    };
    let harness = best_signed_in_harness(config, pool, signed_in).unwrap_or(DEFAULT_TEAM_HARNESS);
    let ready = |connection: &crate::config::ModelConnection| signed_in.contains(&connection.id);
    match crate::research::accounts::pool_account(access, chosen, harness, &ready) {
        Some(_) => None,
        None => Some(TEAM_NEEDS_SIGN_IN),
    }
}

#[cfg(unix)]
fn extension_allowed(name: &str) -> bool {
    Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| DOCUMENT_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
}

/// A directory opened beneath a project folder without following links.
#[cfg(unix)]
type Directory = rustix::fd::OwnedFd;

/// A directory opened beneath a project folder, which cannot exist where no
/// directory-relative no-follow open does; every use is unreachable by construction.
#[cfg(not(unix))]
enum Directory {}

#[cfg(unix)]
fn open_flags() -> OFlags {
    OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC
}

#[cfg(unix)]
fn create_flags() -> OFlags {
    OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC
}

#[cfg(unix)]
fn open_regular(path: &Path) -> Result<(std::fs::File, u64), &'static str> {
    let fd = rustix::fs::open(path, open_flags(), Mode::empty()).map_err(|e| match e {
        rustix::io::Errno::NOENT | rustix::io::Errno::NOTDIR => "document_not_found",
        rustix::io::Errno::LOOP => "document_not_a_regular_file",
        _ => "document_unreadable",
    })?;
    regular(fd, "document_not_a_regular_file")
}

#[cfg(unix)]
fn regular(
    fd: rustix::fd::OwnedFd,
    code: &'static str,
) -> Result<(std::fs::File, u64), &'static str> {
    let stat = rustix::fs::fstat(&fd).map_err(|_| "document_unreadable")?;
    if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile {
        return Err(code);
    }
    let bytes = u64::try_from(stat.st_size).map_err(|_| "document_unreadable")?;
    Ok((std::fs::File::from(fd), bytes))
}

/// Copies owner-picked documents into a project's document folder without executing anything.
/// Takes absolute source paths, the destination folder and the byte budget left for the project.
/// Returns the stored names and sizes, or a plain rejection code after removing partial copies.
#[cfg(unix)]
pub fn copy_documents(
    sources: &[String],
    destination: &Path,
    budget: u64,
) -> Result<Vec<StoredDocument>, &'static str> {
    let directory = rustix::fs::open(destination, open_flags() | OFlags::DIRECTORY, Mode::empty())
        .map_err(|_| "project_folder_unwritable")?;
    copy_into(sources, &directory, &[], budget)
}

/// Refuses to copy documents where no directory-relative no-follow copy exists.
/// Takes the sources, destination and budget, all unused.
/// Returns `project_folder_unavailable`.
#[cfg(not(unix))]
pub fn copy_documents(
    _sources: &[String],
    _destination: &Path,
    _budget: u64,
) -> Result<Vec<StoredDocument>, &'static str> {
    Err("project_folder_unavailable")
}

#[cfg(not(unix))]
fn copy_into(
    _sources: &[String],
    directory: &Directory,
    _kept: &[StoredDocument],
    _budget: u64,
) -> Result<Vec<StoredDocument>, &'static str> {
    match *directory {}
}

#[cfg(not(unix))]
fn remove_documents(directory: &Directory, _documents: &[StoredDocument]) {
    match *directory {}
}

#[cfg(not(unix))]
fn present(directory: &Directory, _name: &str) -> bool {
    match *directory {}
}

#[cfg(not(unix))]
fn unlink_in(directory: &Directory, _name: &str) -> std::io::Result<()> {
    match *directory {}
}

/// Removes one entry from an opened directory without following a link.
/// Takes the directory and the entry name. Returns the OS error on failure.
#[cfg(unix)]
fn unlink_in(directory: &Directory, name: &str) -> rustix::io::Result<()> {
    rustix::fs::unlinkat(directory, name, AtFlags::empty())
}

#[cfg(unix)]
fn copy_into(
    sources: &[String],
    directory: &Directory,
    kept: &[StoredDocument],
    budget: u64,
) -> Result<Vec<StoredDocument>, &'static str> {
    let mut stored: Vec<StoredDocument> = Vec::new();
    let result = copy_each(sources, directory, kept, budget, &mut stored);
    if result.is_err() {
        remove_documents(directory, &stored);
    }
    result.map(|()| stored)
}

#[cfg(unix)]
fn remove_documents(directory: &Directory, documents: &[StoredDocument]) {
    for document in documents {
        let _ = rustix::fs::unlinkat(directory, document.name.as_str(), AtFlags::empty());
    }
}

#[cfg(unix)]
fn present(directory: &Directory, name: &str) -> bool {
    rustix::fs::statat(directory, name, AtFlags::SYMLINK_NOFOLLOW)
        .is_ok_and(|stat| FileType::from_raw_mode(stat.st_mode) == FileType::RegularFile)
}

#[cfg(unix)]
fn copy_each(
    sources: &[String],
    directory: &Directory,
    kept: &[StoredDocument],
    budget: u64,
    stored: &mut Vec<StoredDocument>,
) -> Result<(), &'static str> {
    if sources.len() + kept.len() > DOCUMENTS_MAX {
        return Err("documents_too_many");
    }
    let mut names: BTreeSet<String> = kept.iter().map(|d| d.name.to_lowercase()).collect();
    let mut total: u64 = 0;
    for source in sources {
        let path = Path::new(source);
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .filter(|n| {
                !n.starts_with('.')
                    && n.len() <= DOCUMENT_NAME_BYTES_MAX
                    && !n.chars().any(char::is_control)
            })
            .ok_or("document_name_invalid")?;
        if !extension_allowed(name) {
            return Err("document_type_not_allowed");
        }
        if !names.insert(name.to_lowercase()) {
            return Err("document_name_duplicate");
        }
        let (file, bytes) = open_regular(path)?;
        if bytes > DOCUMENT_BYTES_MAX {
            return Err("document_too_large");
        }
        if total.saturating_add(bytes) > budget {
            return Err("project_documents_too_large");
        }
        let mut output = std::fs::File::from(
            rustix::fs::openat(directory, name, create_flags(), Mode::RUSR | Mode::WUSR).map_err(
                |e| match e {
                    rustix::io::Errno::EXIST => "document_name_duplicate",
                    _ => "project_folder_unwritable",
                },
            )?,
        );
        stored.push(StoredDocument {
            name: name.to_owned(),
            bytes,
        });
        let copied = std::io::copy(&mut file.take(bytes + 1), &mut output)
            .map_err(|_| "document_unreadable")?;
        if copied != bytes {
            return Err("document_changed_while_copying");
        }
        output.sync_all().map_err(|_| "project_folder_unwritable")?;
        total += bytes;
        assert!(total <= budget);
    }
    assert_eq!(stored.len(), sources.len());
    Ok(())
}

/// Creates the setup and research job tables when the engine database opens.
/// Takes the engine connection; the job columns match the research runner's queue.
/// Returns any SQLite failure.
pub fn initialize(connection: &Connection) -> Result<(), EngineError> {
    connection
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS project_setups (project TEXT PRIMARY KEY, body TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS research_jobs (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                project TEXT NOT NULL,
                kind TEXT NOT NULL,
                harness TEXT NOT NULL,
                tools INTEGER NOT NULL,
                status TEXT NOT NULL,
                attempts INTEGER NOT NULL DEFAULT 0,
                claimer TEXT,
                report_path TEXT,
                error TEXT,
                cancel_requested INTEGER NOT NULL DEFAULT 0,
                output_bytes INTEGER NOT NULL DEFAULT 0,
                created_ms INTEGER NOT NULL,
                started_ms INTEGER,
                heartbeat_ms INTEGER,
                finished_ms INTEGER
            );
            CREATE INDEX IF NOT EXISTS research_jobs_project_status
                ON research_jobs(project, status, id);",
        )
        .map_err(error)
}

/// Queues one research job per toggle for a project.
/// Takes the transaction, project, toggles, team plan, current pool assignment and time.
/// Returns any SQLite failure; jobs start as Queued with a pool and Needs a pool without one.
pub fn insert_jobs(
    tx: &Transaction<'_>,
    project: &ProjectId,
    kinds: &[ResearchKind],
    team: &TeamPlan,
    has_pool: bool,
    now: i64,
) -> Result<(), EngineError> {
    let harnesses = team.research_harnesses();
    assert!(!harnesses.is_empty());
    let status = if has_pool {
        ResearchStatus::Queued
    } else {
        ResearchStatus::NeedsPool
    };
    for (index, kind) in kinds.iter().enumerate() {
        insert_job(
            tx,
            project,
            *kind,
            harnesses[index % harnesses.len()],
            status,
            now,
        )?;
    }
    Ok(())
}

/// Queues one research job for a project.
/// Takes the transaction, project, research kind, the harness with its tools switch, the
/// waiting status and the time. Returns any SQLite failure.
fn insert_job(
    tx: &Transaction<'_>,
    project: &ProjectId,
    kind: ResearchKind,
    (harness, tools): (&str, bool),
    status: ResearchStatus,
    now: i64,
) -> Result<(), EngineError> {
    assert!(matches!(
        status,
        ResearchStatus::Queued | ResearchStatus::NeedsPool
    ));
    tx.execute(
        "INSERT INTO research_jobs(project,kind,harness,tools,status,report_path,created_ms)
        VALUES (?1,?2,?3,?4,?5,?6,?7)",
        params![
            project.as_str(),
            kind.name(),
            harness,
            tools,
            status.stored(),
            kind.report_path(),
            now
        ],
    )
    .map_err(error)?;
    Ok(())
}

const JOB_COLUMNS: &str = "id,project,kind,harness,tools,status,attempts,claimer,report_path,error,cancel_requested,output_bytes,created_ms,started_ms,heartbeat_ms,finished_ms";

type JobRow = (
    i64,
    [String; 4],
    (bool, u32, Option<String>, Option<String>, Option<String>),
    (bool, i64),
    [Option<i64>; 4],
);

fn job_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<JobRow> {
    Ok((
        row.get(0)?,
        [row.get(1)?, row.get(2)?, row.get(3)?, row.get(5)?],
        (
            row.get(4)?,
            row.get(6)?,
            row.get(7)?,
            row.get(8)?,
            row.get(9)?,
        ),
        (row.get(10)?, row.get(11)?),
        [row.get(12)?, row.get(13)?, row.get(14)?, row.get(15)?],
    ))
}

fn job(row: JobRow) -> Result<ResearchJob, EngineError> {
    let corrupt = || failure("research_job_corrupt");
    let (id, [project, kind, harness, status], (tools, attempts, claimer, report, job_error)) =
        (row.0, row.1, row.2);
    let (cancel_requested, output_bytes) = row.3;
    let [created_ms, started_ms, heartbeat_ms, finished_ms] = row.4;
    let kind = ResearchKind::parse(&kind).ok_or_else(corrupt)?;
    let status = ResearchStatus::parse(&status)?;
    Ok(ResearchJob {
        id,
        project_id: ProjectId::new(project).map_err(|_| corrupt())?,
        kind,
        kind_label: kind.label().into(),
        status,
        status_label: status.label().into(),
        report_path: report.unwrap_or_else(|| kind.report_path()),
        harness,
        tools,
        attempts,
        claimer,
        error: job_error,
        cancel_requested,
        output_bytes: u64::try_from(output_bytes).map_err(|_| corrupt())?,
        created_ms: created_ms.ok_or_else(corrupt)?,
        started_ms,
        heartbeat_ms,
        finished_ms,
    })
}

fn load_jobs(
    tx: &Transaction<'_>,
    filter: &str,
    value: &dyn rusqlite::ToSql,
) -> Result<Vec<ResearchJob>, EngineError> {
    let mut statement = tx
        .prepare(&format!(
            "SELECT {JOB_COLUMNS} FROM research_jobs WHERE {filter}=?1 ORDER BY id LIMIT 65"
        ))
        .map_err(error)?;
    let rows = statement.query_map([value], job_row).map_err(error)?;
    let mut jobs = Vec::new();
    for row in rows {
        jobs.push(job(row.map_err(error)?)?);
    }
    if jobs.len() > 64 {
        return Err(failure("research_job_capacity"));
    }
    Ok(jobs)
}

fn effective(status: ResearchStatus, has_pool: bool) -> ResearchStatus {
    match status {
        ResearchStatus::Queued | ResearchStatus::NeedsPool if has_pool => ResearchStatus::Queued,
        ResearchStatus::Queued | ResearchStatus::NeedsPool => ResearchStatus::NeedsPool,
        other => other,
    }
}

/// Lists a project's research jobs with the current pool assignment applied.
/// Takes a read transaction, the configuration and the project.
/// Returns at most 64 jobs in creation order with plain labels.
pub fn list_jobs(
    tx: &Transaction<'_>,
    config: &EffectiveConfig,
    project: &ProjectId,
) -> Result<Vec<ResearchJob>, EngineError> {
    let has_pool = project_has_pool(config, project);
    let mut jobs = load_jobs(tx, "project", &project.as_str())?;
    for job in &mut jobs {
        job.status = effective(job.status, has_pool);
        job.status_label = job.status.label().into();
        assert_eq!(&job.project_id, project);
    }
    Ok(jobs)
}

fn sync_pool_status(tx: &Transaction<'_>, config: &EffectiveConfig) -> Result<(), EngineError> {
    let projects: Vec<String> = {
        let mut statement = tx
            .prepare("SELECT DISTINCT project FROM research_jobs WHERE status IN ('queued','needs_pool') LIMIT 4096")
            .map_err(error)?;
        let rows = statement.query_map([], |row| row.get(0)).map_err(error)?;
        rows.collect::<Result<_, _>>().map_err(error)?
    };
    for project in projects {
        let id = ProjectId::new(project.clone()).map_err(|e| failure(&e.0))?;
        let status = effective(ResearchStatus::Queued, project_has_pool(config, &id));
        tx.execute(
            "UPDATE research_jobs SET status=?1
            WHERE project=?2 AND status IN ('queued','needs_pool') AND status<>?1",
            params![status.stored(), project],
        )
        .map_err(error)?;
    }
    Ok(())
}

/// Atomically claims the oldest queued research job for one runner.
/// Takes a connection, the configuration, a claimer identity and the time.
/// Returns the claimed job marked Running, or none when nothing is claimable.
pub fn claim_research_job(
    connection: &mut Connection,
    config: &EffectiveConfig,
    claimer: &str,
    now: i64,
) -> Result<Option<ResearchJob>, EngineError> {
    crate::protocol::RequestId::new(claimer).map_err(|_| failure("research_claimer_invalid"))?;
    let tx = connection
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(error)?;
    sync_pool_status(&tx, config)?;
    let id: Option<i64> = tx
        .query_row(
            "UPDATE research_jobs SET status='running', claimer=?1, attempts=attempts+1,
            started_ms=?2, heartbeat_ms=?2, finished_ms=NULL, error=NULL, output_bytes=0
            WHERE id=(SELECT id FROM research_jobs WHERE status='queued' ORDER BY id LIMIT 1)
            AND status='queued' RETURNING id",
            params![claimer, now],
            |row| row.get(0),
        )
        .optional()
        .map_err(error)?;
    let Some(id) = id else {
        tx.commit().map_err(error)?;
        return Ok(None);
    };
    let job = load_jobs(&tx, "id", &id)?
        .pop()
        .ok_or_else(|| failure("research_job_corrupt"))?;
    tx.commit().map_err(error)?;
    assert_eq!(job.status, ResearchStatus::Running);
    assert_eq!(job.claimer.as_deref(), Some(claimer));
    Ok(Some(job))
}

/// Finishes a running job that the same claimer holds.
/// Takes a connection, the job id, the claimer, an optional failure code and the time.
/// Returns `research_job_not_claimed` unless the job is running under that claimer.
pub fn finish_research_job(
    connection: &mut Connection,
    id: i64,
    claimer: &str,
    failure_code: Option<&str>,
    now: i64,
) -> Result<(), EngineError> {
    if failure_code.is_some_and(|code| crate::protocol::RequestId::new(code).is_err()) {
        return Err(failure("research_failure_invalid"));
    }
    let status = if failure_code.is_some() {
        ResearchStatus::Failed
    } else {
        ResearchStatus::Done
    };
    let tx = connection
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(error)?;
    let changed = tx
        .execute(
            "UPDATE research_jobs SET status=?1, error=?2, finished_ms=?3
            WHERE id=?4 AND status='running' AND claimer=?5",
            params![status.stored(), failure_code, now, id, claimer],
        )
        .map_err(error)?;
    if changed != 1 {
        return Err(failure("research_job_not_claimed"));
    }
    tx.commit().map_err(error)
}

/// Selects the whole report or one markdown section by its heading text.
/// Takes the report text and an optional heading.
/// Returns the trimmed text through the next heading of the same or higher level.
pub fn section(report: &str, heading: Option<&str>) -> Option<String> {
    let Some(heading) = heading else {
        return Some(report.trim().to_owned());
    };
    let level = |line: &str| {
        let hashes = line.bytes().take_while(|b| *b == b'#').count();
        (hashes > 0 && line[hashes..].starts_with(' ')).then_some(hashes)
    };
    let lines: Vec<&str> = report.lines().collect();
    let start = lines
        .iter()
        .position(|line| level(line).is_some_and(|n| line[n..].trim() == heading.trim()))?;
    let depth = level(lines[start])?;
    let end = lines[start + 1..]
        .iter()
        .position(|line| level(line).is_some_and(|n| n <= depth))
        .map_or(lines.len(), |offset| start + 1 + offset);
    Some(lines[start..end].join("\n").trim().to_owned())
}

#[cfg(unix)]
fn projects_root(database: &Path) -> Result<PathBuf, EngineError> {
    let runtime = std::fs::canonicalize(
        database
            .parent()
            .ok_or_else(|| failure("runtime_parent_missing"))?,
    )
    .map_err(error)?;
    let root = runtime.join(PROJECTS_DIRECTORY);
    match std::fs::DirBuilder::new().mode(0o700).create(&root) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(error(e)),
    }
    if !std::fs::symlink_metadata(&root).map_err(error)?.is_dir() {
        return Err(failure("projects_root_not_a_directory"));
    }
    Ok(root)
}

#[cfg(unix)]
fn brief(stored: &StoredSetup) -> String {
    let list = |items: Vec<String>| {
        if items.is_empty() {
            "- None\n".to_owned()
        } else {
            items.iter().map(|item| format!("- {item}\n")).collect()
        }
    };
    format!(
        "# {}\n\n## About\n\n{}\n\n## Tech stack\n\n{}\n## Target platforms\n\n{}\n## Research\n\n{}\n## Documents\n\n{}",
        stored.name,
        if stored.about.trim().is_empty() {
            "Not described yet."
        } else {
            stored.about.trim()
        },
        list(
            stored
                .tech_stack
                .iter()
                .map(|t| t.trim().to_owned())
                .collect()
        ),
        list(
            stored
                .platforms
                .iter()
                .map(|p| p.label().to_owned())
                .collect()
        ),
        list(
            stored
                .research
                .iter()
                .map(|r| format!("{} -> {}", r.label(), r.report_path()))
                .collect()
        ),
        list(
            stored
                .documents
                .iter()
                .map(|d| format!("{CONTEXT_DIRECTORY}/documents/{}", d.name))
                .collect()
        ),
    )
}

/// Writes `stored`'s brief to `<folder>/context/brief.md`, opened beneath the
/// folder without following links. Takes the project folder and its setup.
/// Returns a plain code when the context folder is missing or unwritable.
pub(crate) fn write_brief_at(folder: &Path, stored: &StoredSetup) -> Result<(), &'static str> {
    let context =
        open_beneath(folder, &[CONTEXT_DIRECTORY]).map_err(|_| "project_folder_missing")?;
    write_brief(&context, stored)
}

#[cfg(not(unix))]
fn write_brief(context: &Directory, _stored: &StoredSetup) -> Result<(), &'static str> {
    match *context {}
}

#[cfg(unix)]
fn write_brief(context: &Directory, stored: &StoredSetup) -> Result<(), &'static str> {
    let unwritable = |_| "project_folder_unwritable";
    let _ = rustix::fs::unlinkat(context, BRIEF_PARTIAL, AtFlags::empty());
    let mut file = std::fs::File::from(
        rustix::fs::openat(
            context,
            BRIEF_PARTIAL,
            create_flags(),
            Mode::RUSR | Mode::WUSR,
        )
        .map_err(unwritable)?,
    );
    file.write_all(brief(stored).as_bytes())
        .and_then(|()| file.sync_all())
        .map_err(|_| "project_folder_unwritable")?;
    rustix::fs::renameat(context, BRIEF_PARTIAL, context, "brief.md").map_err(unwritable)
}

/// Refuses to build a new project's folder where its private 0700 git home cannot
/// be made. Takes the database path, staging folder and setup, all
/// unused. Returns `project_folder_unavailable`.
#[cfg(not(unix))]
fn stage_in_home(
    _database: &Path,
    _staging: &Path,
    _setup: &ProjectSetup,
) -> Result<Vec<StoredDocument>, &'static str> {
    Err("project_folder_unavailable")
}

/// Builds a new project's folder in a hidden staging folder of the projects
/// home: a git repository with one empty start commit, the `.crazyagent/`
/// layout with its git exclude lines, and the owner's documents.
/// Takes the database path (for git's private home), the staging folder and
/// the setup. Returns the copied documents, or a plain code with nothing kept.
#[cfg(unix)]
fn stage_in_home(
    database: &Path,
    staging: &Path,
    setup: &ProjectSetup,
) -> Result<Vec<StoredDocument>, &'static str> {
    const FAILED: &str = "home_new_project_failed";
    let failed = |_: EngineError| FAILED;
    let io_failed = |_: std::io::Error| FAILED;
    let git_home = projects_root(database).map_err(failed)?.join(".git-home");
    match std::fs::DirBuilder::new().mode(0o700).create(&git_home) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(_) => return Err(FAILED),
    }
    std::fs::DirBuilder::new()
        .mode(0o755)
        .create(staging)
        .map_err(io_failed)?;
    crate::research::runner::ensure_repository(staging, &git_home).map_err(|_| FAILED)?;
    let state = crate::projects_home::prepare_layout(staging).map_err(failed)?;
    if !crate::projects_home::exclude_from_git(staging).map_err(failed)? {
        return Err(FAILED);
    }
    let context = state.join(CONTEXT_DIRECTORY);
    for directory in ["documents", "research"] {
        std::fs::DirBuilder::new()
            .mode(0o755)
            .create(context.join(directory))
            .map_err(io_failed)?;
    }
    assert!(state.ends_with(crate::projects_home::CRAZYAGENT_DIRECTORY));
    copy_documents(
        &setup.documents,
        &context.join("documents"),
        PROJECT_DOCUMENT_BYTES_MAX,
    )
}

/// Executes a host-only project setup action inside the owner transaction.
/// Takes the transaction, configuration, database path, action and revision.
/// Returns Applied, or a plain rejection before any durable row is written.
pub(crate) fn execute(
    tx: &Transaction<'_>,
    config: &EffectiveConfig,
    database: &Path,
    action: &SetupAction,
    revision: u64,
) -> Result<Outcome, EngineError> {
    action.validate()?;
    match action {
        SetupAction::Create {
            project_id,
            spec,
            setup,
        } => create(tx, config, database, project_id, spec, setup),
        SetupAction::AddDocuments {
            project_id,
            documents,
        } => add_documents(tx, project_id, documents),
        SetupAction::DeleteContext { project_id, path } => delete_context(tx, project_id, path),
        SetupAction::Update {
            project_id,
            expected_revision,
            name,
            setup,
        } => update(
            tx,
            config,
            project_id,
            *expected_revision,
            name.as_deref(),
            setup,
        ),
        SetupAction::Remove { project_id } => remove(tx, project_id),
        SetupAction::CancelResearch { project_id, job } => cancel_research(tx, project_id, *job),
        SetupAction::RetryResearch { project_id, job } => {
            retry_research(tx, config, project_id, *job)
        }
        SetupAction::SaveToMemory {
            project_id,
            research,
            section: heading,
        } => {
            let text = match research_text(tx, project_id, *research, heading.as_deref())? {
                Ok(text) => text,
                Err(code) => return Ok(rejected(code)),
            };
            crate::memory::propose_research(
                tx,
                config,
                project_id,
                research.name(),
                &text,
                revision,
            )
        }
    }
}

/// Reads a whole context file as text, up to the report bound, without following links.
/// Takes the project and the file path relative to its folder. Returns the text, or a
/// plain rejection code when the setup or file is missing or the path is not a context path.
pub(crate) fn context_text(
    tx: &Transaction<'_>,
    project_id: &ProjectId,
    path: &str,
) -> Result<Result<String, &'static str>, EngineError> {
    let Some(stored) = load_setup(tx, project_id)? else {
        return Ok(Err("project_setup_missing"));
    };
    Ok(
        read_beneath(&stored.state_folder(), path, 0, REPORT_BYTES_MAX)
            .map(|(bytes, _)| String::from_utf8_lossy(&bytes).into_owned()),
    )
}

/// Reads one research report, or one of its sections, from the project folder.
/// Takes the project, research kind and optional heading. Returns the text, or a
/// plain rejection code when the setup, report or section is missing.
pub(crate) fn research_text(
    tx: &Transaction<'_>,
    project_id: &ProjectId,
    research: ResearchKind,
    heading: Option<&str>,
) -> Result<Result<String, &'static str>, EngineError> {
    let Some(stored) = load_setup(tx, project_id)? else {
        return Ok(Err("project_setup_missing"));
    };
    let report = match read_beneath(
        &stored.state_folder(),
        &research.report_path(),
        0,
        REPORT_BYTES_MAX,
    ) {
        Ok((bytes, _)) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(_) => return Ok(Err("research_report_missing")),
    };
    Ok(section(&report, heading).ok_or("research_section_missing"))
}

fn save_setup(tx: &Transaction<'_>, stored: &StoredSetup) -> Result<(), EngineError> {
    let changed = tx
        .execute(
            "UPDATE project_setups SET body=?2 WHERE project=?1",
            params![
                stored.project_id.as_str(),
                serde_json::to_string(stored).map_err(error)?
            ],
        )
        .map_err(error)?;
    assert_eq!(changed, 1);
    Ok(())
}

fn add_documents(
    tx: &Transaction<'_>,
    project_id: &ProjectId,
    sources: &[String],
) -> Result<Outcome, EngineError> {
    let Some(mut stored) = load_setup(tx, project_id)? else {
        return Ok(rejected("project_setup_missing"));
    };
    let Ok(context) = open_beneath(&stored.state_folder(), &[CONTEXT_DIRECTORY]) else {
        return Ok(rejected("project_folder_missing"));
    };
    let Ok(directory) = open_directory(&context, "documents") else {
        return Ok(rejected("project_folder_missing"));
    };
    stored
        .documents
        .retain(|document| present(&directory, &document.name));
    let used: u64 = stored.documents.iter().map(|d| d.bytes).sum();
    let budget = PROJECT_DOCUMENT_BYTES_MAX.saturating_sub(used);
    let added = match copy_into(sources, &directory, &stored.documents, budget) {
        Ok(added) => added,
        Err(code) => return Ok(rejected(code)),
    };
    stored.documents.extend(added.iter().cloned());
    assert!(stored.documents.len() <= DOCUMENTS_MAX);
    let saved =
        save_setup(tx, &stored).and_then(|()| write_brief(&context, &stored).map_err(failure));
    if saved.is_err() {
        remove_documents(&directory, &added);
    }
    saved.map(|()| Outcome::Applied)
}

fn delete_context(
    tx: &Transaction<'_>,
    project_id: &ProjectId,
    path: &str,
) -> Result<Outcome, EngineError> {
    let Some(mut stored) = load_setup(tx, project_id)? else {
        return Ok(rejected("project_setup_missing"));
    };
    let kind = context_kind(path).ok_or_else(|| failure("context_path_invalid"))?;
    if kind == ContextKind::Brief {
        return Ok(rejected("context_item_protected"));
    }
    let (directory, name) = path
        .strip_prefix("context/")
        .and_then(|rest| rest.split_once('/'))
        .ok_or_else(|| failure("context_path_invalid"))?;
    let Ok(context) = open_beneath(&stored.state_folder(), &[CONTEXT_DIRECTORY]) else {
        return Ok(rejected("context_item_missing"));
    };
    let Ok(parent) = open_directory(&context, directory) else {
        return Ok(rejected("context_item_missing"));
    };
    if !present(&parent, name) {
        return Ok(rejected("context_item_missing"));
    }
    unlink_in(&parent, name).map_err(error)?;
    assert!(!present(&parent, name));
    if kind == ContextKind::Document {
        stored
            .documents
            .retain(|document| document.name != name && present(&parent, &document.name));
        save_setup(tx, &stored)?;
        write_brief(&context, &stored).map_err(failure)?;
    }
    Ok(Outcome::Applied)
}

fn rejected(code: &str) -> Outcome {
    Outcome::Rejected { code: code.into() }
}

/// Changes an existing project's name, description, stack, platforms, research, pool and team.
/// Takes the transaction, configuration, project, the setup revision the owner edited, an
/// optional new name and the new setup; documents keep their own add and delete actions.
/// Returns Applied, or a plain rejection such as `stale_revision` after a concurrent change.
fn update(
    tx: &Transaction<'_>,
    config: &EffectiveConfig,
    project_id: &ProjectId,
    expected_revision: u64,
    name: Option<&str>,
    setup: &ProjectSetup,
) -> Result<Outcome, EngineError> {
    let Some(mut stored) = load_setup(tx, project_id)? else {
        return Ok(rejected("project_setup_missing"));
    };
    if stored.revision != expected_revision {
        return Ok(rejected("stale_revision"));
    }
    if setup.pool != stored.pool
        && setup
            .pool
            .as_ref()
            .is_some_and(|pool| !pool_exists(config, pool))
    {
        return Ok(rejected("pool_missing"));
    }
    let Ok(context) = open_beneath(&stored.state_folder(), &[CONTEXT_DIRECTORY]) else {
        return Ok(rejected("project_folder_missing"));
    };
    if let Some(name) = name {
        rename_project(tx, project_id, name)?;
        stored.name = name.to_owned();
    }
    let previous = std::mem::take(&mut stored.research);
    stored.about = setup.about.clone();
    stored.tech_stack = setup
        .tech_stack
        .iter()
        .map(|t| t.trim().to_owned())
        .collect();
    stored.platforms = setup.platforms.clone();
    stored.research = setup.research.clone();
    stored.pool = setup.pool.clone();
    if let Some(team) = &setup.team {
        stored.team = team.clone();
    }
    stored.team.validate()?;
    stored.revision = expected_revision
        .checked_add(1)
        .ok_or_else(|| failure("project_setup_revision_limit"))?;
    replan_jobs(
        tx,
        project_id,
        &previous,
        &stored.research,
        &stored.team,
        project_has_pool(config, project_id),
    )?;
    save_setup(tx, &stored)?;
    write_brief(&context, &stored).map_err(failure)?;
    assert!(stored.revision > expected_revision);
    Ok(Outcome::Applied)
}

/// Renames a project in the project list.
/// Takes the transaction, the project and an already validated name.
/// Returns any storage failure; the project row must exist.
fn rename_project(
    tx: &Transaction<'_>,
    project_id: &ProjectId,
    name: &str,
) -> Result<(), EngineError> {
    let body: String = tx
        .query_row(
            "SELECT body FROM projects WHERE id=?1",
            [project_id.as_str()],
            |row| row.get(0),
        )
        .map_err(error)?;
    let mut project: crate::protocol::StoredProject = serde_json::from_str(&body).map_err(error)?;
    assert_eq!(&project.project_id, project_id);
    project.spec.name = name.to_owned();
    project
        .spec
        .validate()
        .map_err(|_| failure("project_spec_bounds"))?;
    let changed = tx
        .execute(
            "UPDATE projects SET body=?2 WHERE id=?1",
            params![
                project_id.as_str(),
                serde_json::to_string(&project).map_err(error)?
            ],
        )
        .map_err(error)?;
    assert_eq!(changed, 1);
    Ok(())
}

/// Brings a project's research jobs in line with its toggles after an owner edit.
/// Takes the transaction, project, the toggles before and after, the team and pool state.
/// Turned-off research stops, turned-on research is queued again or added, and waiting
/// jobs move to the new team's harness; running and finished jobs keep theirs.
fn replan_jobs(
    tx: &Transaction<'_>,
    project: &ProjectId,
    previous: &[ResearchKind],
    kinds: &[ResearchKind],
    team: &TeamPlan,
    has_pool: bool,
) -> Result<(), EngineError> {
    let now = now_ms();
    let jobs = load_jobs(tx, "project", &project.as_str())?;
    for job in jobs.iter().filter(|job| !kinds.contains(&job.kind)) {
        cancel_jobs(tx, "id", &job.id, now)?;
    }
    let harnesses = team.research_harnesses();
    assert!(!harnesses.is_empty());
    let waiting = effective(ResearchStatus::Queued, has_pool);
    for (index, kind) in kinds.iter().enumerate() {
        let (harness, tools) = harnesses[index % harnesses.len()];
        let restartable = if previous.contains(kind) {
            "'queued','needs_pool'"
        } else {
            "'queued','needs_pool','failed','cancelled'"
        };
        match jobs.iter().rev().find(|job| job.kind == *kind) {
            None => insert_job(tx, project, *kind, (harness, tools), waiting, now)?,
            Some(job) => {
                tx.execute(
                    &format!(
                        "UPDATE research_jobs SET status=?2, harness=?3, tools=?4,
                        attempts=CASE WHEN status IN ('failed','cancelled') THEN 0 ELSE attempts END,
                        cancel_requested=0, error=NULL, claimer=NULL, finished_ms=NULL
                        WHERE id=?1 AND status IN ({restartable})"
                    ),
                    params![job.id, waiting.stored(), harness, tools],
                )
                .map_err(error)?;
            }
        }
    }
    Ok(())
}

/// Stops active research: waiting jobs become Cancelled at once and running jobs are
/// flagged, so their runner stops the agent's whole process group at its next heartbeat.
/// Takes the transaction, the column to match (`id` or `project`), its value and the time.
/// Returns how many jobs were still active.
fn cancel_jobs(
    tx: &Transaction<'_>,
    filter: &str,
    value: &dyn rusqlite::ToSql,
    now: i64,
) -> Result<usize, EngineError> {
    assert!(filter == "id" || filter == "project");
    tx.execute(
        &format!(
            "UPDATE research_jobs
            SET status = CASE WHEN status = 'running' THEN 'running' ELSE 'cancelled' END,
                cancel_requested = 1,
                finished_ms = CASE WHEN status = 'running' THEN finished_ms ELSE ?2 END
            WHERE {filter} = ?1 AND status IN ('queued', 'needs_pool', 'running')
                AND cancel_requested = 0"
        ),
        [value, &now as &dyn rusqlite::ToSql],
    )
    .map_err(error)
}

/// Loads one research job when it belongs to the project.
/// Takes a transaction, the project and the job id.
/// Returns the job, or none for an unknown job or another project's job.
pub(crate) fn project_job(
    tx: &Transaction<'_>,
    project_id: &ProjectId,
    id: i64,
) -> Result<Option<ResearchJob>, EngineError> {
    let job = load_jobs(tx, "id", &id)?.pop();
    Ok(job.filter(|job| &job.project_id == project_id))
}

/// Stops one research job of a project; a running agent is stopped by its runner.
/// Takes the transaction, project and job id. Returns Applied, `research_job_missing` for
/// another project's job, or `research_job_finished` once nothing is left to stop.
fn cancel_research(
    tx: &Transaction<'_>,
    project_id: &ProjectId,
    id: i64,
) -> Result<Outcome, EngineError> {
    let Some(job) = project_job(tx, project_id, id)? else {
        return Ok(rejected("research_job_missing"));
    };
    if job.status == ResearchStatus::Running && job.cancel_requested {
        return Ok(Outcome::Applied);
    }
    if cancel_jobs(tx, "id", &id, now_ms())? == 0 {
        return Ok(rejected("research_job_finished"));
    }
    Ok(Outcome::Applied)
}

/// Queues a failed or cancelled research job again with a fresh attempt budget.
/// Takes the transaction, configuration, project and job id. Returns Applied,
/// `research_job_missing`, or `research_job_not_retryable` for a job that is not stopped.
fn retry_research(
    tx: &Transaction<'_>,
    config: &EffectiveConfig,
    project_id: &ProjectId,
    id: i64,
) -> Result<Outcome, EngineError> {
    let Some(job) = project_job(tx, project_id, id)? else {
        return Ok(rejected("research_job_missing"));
    };
    if !matches!(
        job.status,
        ResearchStatus::Failed | ResearchStatus::Cancelled
    ) {
        return Ok(rejected("research_job_not_retryable"));
    }
    let status = effective(ResearchStatus::Queued, project_has_pool(config, project_id));
    let changed = tx
        .execute(
            "UPDATE research_jobs SET status=?2, attempts=0, claimer=NULL, error=NULL,
            cancel_requested=0, output_bytes=0, started_ms=NULL, heartbeat_ms=NULL,
            finished_ms=NULL WHERE id=?1 AND status IN ('failed','cancelled')",
            params![id, status.stored()],
        )
        .map_err(error)?;
    if changed != 1 {
        return Ok(rejected("research_job_not_retryable"));
    }
    Ok(Outcome::Applied)
}

/// Removes a project from CrazyAgent and leaves its folder on disk where it is.
/// Takes the transaction and the project; its research stops as in `cancel_jobs`.
/// Returns `project_master_open` while its master conversation is open, since ending it
/// releases the master's worktree, and `invalid_project` for an unknown project.
fn remove(tx: &Transaction<'_>, project_id: &ProjectId) -> Result<Outcome, EngineError> {
    let (exists, master_open): (bool, bool) = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM projects WHERE id=?1),
            EXISTS(SELECT 1 FROM master_sessions WHERE project=?1 AND phase='configured')",
            [project_id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(error)?;
    if !exists {
        return Ok(rejected("invalid_project"));
    }
    if master_open {
        return Ok(rejected("project_master_open"));
    }
    cancel_jobs(tx, "project", &project_id.as_str(), now_ms())?;
    tx.execute(
        "DELETE FROM project_setups WHERE project=?1",
        [project_id.as_str()],
    )
    .map_err(error)?;
    let removed = tx
        .execute("DELETE FROM projects WHERE id=?1", [project_id.as_str()])
        .map_err(error)?;
    assert_eq!(removed, 1);
    Ok(Outcome::Applied)
}

fn create(
    tx: &Transaction<'_>,
    config: &EffectiveConfig,
    database: &Path,
    project_id: &ProjectId,
    spec: &ProjectSpec,
    setup: &ProjectSetup,
) -> Result<Outcome, EngineError> {
    if setup
        .pool
        .as_ref()
        .is_some_and(|pool| !pool_exists(config, pool))
    {
        return Ok(rejected("pool_missing"));
    }
    let exists: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM projects WHERE id=?1)",
            [project_id.as_str()],
            |row| row.get(0),
        )
        .map_err(error)?;
    if exists {
        return Ok(rejected("project_exists"));
    }
    let team = match setup.team.clone() {
        Some(team) => team,
        None => {
            let signed = signed_in_connections(tx, config, setup.pool.as_ref())?;
            default_team(config, setup.pool.as_ref(), &setup.research, &signed)
        }
    };
    team.validate()?;
    let Some(home) = crate::projects_home::load(tx)? else {
        return Ok(rejected("projects_home_unset"));
    };
    let Some(name) = crate::projects_home::slug(&spec.name)
        .or_else(|| crate::projects_home::slug(project_id.as_str()))
    else {
        return Ok(rejected("home_folder_name_invalid"));
    };
    let folder = match crate::projects_home::destination(&home, &name) {
        Ok(folder) => folder,
        Err(e) => return Ok(rejected(&e.0)),
    };
    let staging = home.join(format!(".crazyagent-new-{}", project_id.as_str()));
    if std::fs::symlink_metadata(&staging).is_ok() {
        std::fs::remove_dir_all(&staging).map_err(error)?;
    }
    let documents = match stage_in_home(database, &staging, setup) {
        Ok(documents) => documents,
        Err(code) => {
            let _ = std::fs::remove_dir_all(&staging);
            return Ok(rejected(code));
        }
    };
    let result = record(
        tx, config, project_id, spec, setup, team, documents, &folder,
    )
    .and_then(|outcome| {
        if outcome == Outcome::Applied {
            let stored =
                load_setup(tx, project_id)?.ok_or_else(|| failure("project_setup_missing"))?;
            let state = crate::projects_home::state_folder(&staging);
            assert_ne!(state, staging);
            let context = open_beneath(&state, &[CONTEXT_DIRECTORY]).map_err(error)?;
            write_brief(&context, &stored).map_err(failure)?;
            if std::fs::symlink_metadata(&folder).is_ok() {
                return Err(failure("home_destination_exists"));
            }
            std::fs::rename(&staging, &folder).map_err(error)?;
        }
        Ok(outcome)
    });
    if !matches!(result, Ok(Outcome::Applied)) {
        let _ = std::fs::remove_dir_all(&staging);
    }
    result
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn record(
    tx: &Transaction<'_>,
    config: &EffectiveConfig,
    project_id: &ProjectId,
    spec: &ProjectSpec,
    setup: &ProjectSetup,
    team: TeamPlan,
    documents: Vec<StoredDocument>,
    folder: &Path,
) -> Result<Outcome, EngineError> {
    let outcome = crate::storage::create_project(tx, project_id, spec)?;
    if outcome != Outcome::Applied {
        return Ok(outcome);
    }
    let now = now_ms();
    let stored = StoredSetup {
        project_id: project_id.clone(),
        name: spec.name.clone(),
        about: setup.about.clone(),
        tech_stack: setup
            .tech_stack
            .iter()
            .map(|t| t.trim().to_owned())
            .collect(),
        platforms: setup.platforms.clone(),
        research: setup.research.clone(),
        pool: setup.pool.clone(),
        team,
        documents,
        folder: folder
            .to_str()
            .ok_or_else(|| failure("project_folder_unwritable"))?
            .to_owned(),
        created_ms: now,
        revision: 1,
    };
    tx.execute(
        "INSERT INTO project_setups VALUES (?1,?2)",
        params![
            project_id.as_str(),
            serde_json::to_string(&stored).map_err(error)?
        ],
    )
    .map_err(error)?;
    insert_jobs(
        tx,
        project_id,
        &setup.research,
        &stored.team,
        project_has_pool(config, project_id),
        now,
    )?;
    assert!(!tx.is_autocommit());
    Ok(Outcome::Applied)
}

/// Loads the stored setup of one project.
/// Takes a transaction and the project.
/// Returns the setup, or none for a project created without one.
pub fn load_setup(
    tx: &Transaction<'_>,
    project: &ProjectId,
) -> Result<Option<StoredSetup>, EngineError> {
    let body: Option<String> = tx
        .query_row(
            "SELECT body FROM project_setups WHERE project=?1",
            [project.as_str()],
            |row| row.get(0),
        )
        .optional()
        .map_err(error)?;
    body.map(|body| {
        let stored: StoredSetup = serde_json::from_str(&body).map_err(error)?;
        assert_eq!(&stored.project_id, project);
        Ok(stored)
    })
    .transpose()
}

fn context_kind(path: &str) -> Option<ContextKind> {
    let rest = path.strip_prefix("context/")?;
    if rest == "brief.md" {
        return Some(ContextKind::Brief);
    }
    let (directory, name) = rest.split_once('/')?;
    if name.is_empty()
        || name.starts_with('.')
        || name.contains('/')
        || name.len() > DOCUMENT_NAME_BYTES_MAX
        || name.chars().any(char::is_control)
    {
        return None;
    }
    match directory {
        "documents" => Some(ContextKind::Document),
        "research" => Some(ContextKind::Research),
        _ => None,
    }
}

#[cfg(not(unix))]
fn open_directory(parent: &Directory, _name: &str) -> std::io::Result<Directory> {
    match *parent {}
}

/// Refuses to open a project folder where no directory-relative no-follow open exists.
/// Takes the folder and directories, both unused. Returns an `Unsupported` error reading
/// `project_folder_unavailable`.
#[cfg(not(unix))]
fn open_beneath(_folder: &Path, _directories: &[&str]) -> std::io::Result<Directory> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "project_folder_unavailable",
    ))
}

/// Refuses to read a project context item where no no-follow open exists.
/// Takes the folder, path, offset and limit, all unused. Returns `project_folder_unavailable`.
#[cfg(not(unix))]
fn read_beneath(
    _folder: &Path,
    _relative: &str,
    _offset: u64,
    _limit: u64,
) -> Result<(Vec<u8>, u64), &'static str> {
    Err("project_folder_unavailable")
}

#[cfg(unix)]
fn open_directory(parent: &Directory, name: &str) -> rustix::io::Result<Directory> {
    rustix::fs::openat(
        parent,
        name,
        open_flags() | OFlags::DIRECTORY,
        Mode::empty(),
    )
}

#[cfg(unix)]
fn open_beneath(folder: &Path, directories: &[&str]) -> rustix::io::Result<Directory> {
    let mut fd = rustix::fs::open(folder, open_flags() | OFlags::DIRECTORY, Mode::empty())?;
    for directory in directories {
        fd = open_directory(&fd, directory)?;
    }
    Ok(fd)
}

#[cfg(unix)]
fn read_beneath(
    folder: &Path,
    relative: &str,
    offset: u64,
    limit: u64,
) -> Result<(Vec<u8>, u64), &'static str> {
    context_kind(relative).ok_or("context_path_invalid")?;
    let missing = |_| "context_item_missing";
    let parts: Vec<&str> = relative.split('/').collect();
    let fd = open_beneath(folder, &parts[..parts.len() - 1]).map_err(missing)?;
    let file = rustix::fs::openat(&fd, parts[parts.len() - 1], open_flags(), Mode::empty())
        .map_err(missing)?;
    let (mut file, bytes) = regular(file, "context_item_missing")?;
    if offset > bytes {
        return Err("context_offset_invalid");
    }
    std::io::Seek::seek(&mut file, std::io::SeekFrom::Start(offset))
        .map_err(|_| "context_item_unreadable")?;
    let mut buffer = Vec::new();
    file.take(limit)
        .read_to_end(&mut buffer)
        .map_err(|_| "context_item_unreadable")?;
    assert!(buffer.len() as u64 <= limit);
    Ok((buffer, bytes))
}

/// Lists the brief, documents and research reports in a project folder.
/// Takes the stored setup.
/// Returns at most 64 regular-file items; links and special files are skipped.
pub fn list_context(stored: &StoredSetup) -> Result<ProjectContext, EngineError> {
    let state = stored.state_folder();
    let folder = state.as_path();
    let mut items = Vec::new();
    let mut push = |path: String, kind: ContextKind| -> Result<(), EngineError> {
        if items.len() >= CONTEXT_ITEMS_MAX {
            return Ok(());
        }
        if let Ok(metadata) = std::fs::symlink_metadata(folder.join(&path))
            && metadata.is_file()
        {
            items.push(ContextItem {
                path,
                kind,
                bytes: metadata.len(),
            });
        }
        Ok(())
    };
    push(format!("{CONTEXT_DIRECTORY}/brief.md"), ContextKind::Brief)?;
    for (directory, kind) in [
        ("documents", ContextKind::Document),
        ("research", ContextKind::Research),
    ] {
        let path = folder.join(CONTEXT_DIRECTORY).join(directory);
        if !std::fs::symlink_metadata(&path).is_ok_and(|m| m.is_dir()) {
            continue;
        }
        let mut names: Vec<String> = std::fs::read_dir(&path)
            .map_err(error)?
            .take(CONTEXT_ITEMS_MAX * 4)
            .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
            .collect();
        names.sort();
        for name in names {
            let relative = format!("{CONTEXT_DIRECTORY}/{directory}/{name}");
            if context_kind(&relative) == Some(kind) {
                push(relative, kind)?;
            }
        }
    }
    assert!(items.len() <= CONTEXT_ITEMS_MAX);
    Ok(ProjectContext {
        project_id: stored.project_id.clone(),
        folder: folder.to_string_lossy().into_owned(),
        items,
    })
}

/// Reads one bounded chunk of a context item without following links.
/// Takes the stored setup, the relative item path and a byte offset.
/// Returns text for markdown, text and csv files; other kinds return only their size.
pub fn read_context(
    stored: &StoredSetup,
    path: &str,
    offset: u64,
) -> Result<ContextChunk, EngineError> {
    let textual = Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| ["md", "txt", "csv"].contains(&e.to_ascii_lowercase().as_str()));
    let limit = if textual {
        CONTEXT_CHUNK_BYTES as u64
    } else {
        0
    };
    let (mut bytes, size) =
        read_beneath(&stored.state_folder(), path, offset, limit).map_err(failure)?;
    if !textual {
        return Ok(ContextChunk {
            path: path.into(),
            bytes: size,
            offset,
            text: None,
            next_offset: None,
        });
    }
    let complete = offset + bytes.len() as u64 >= size;
    if !complete {
        let valid = match std::str::from_utf8(&bytes) {
            Ok(_) => bytes.len(),
            Err(e) if e.error_len().is_none() => e.valid_up_to(),
            Err(_) => bytes.len(),
        };
        bytes.truncate(valid.max(1));
    }
    let consumed = bytes.len() as u64;
    let chunk = ContextChunk {
        path: path.into(),
        bytes: size,
        offset,
        text: Some(String::from_utf8_lossy(&bytes).into_owned()),
        next_offset: (offset + consumed < size).then_some(offset + consumed),
    };
    assert!(consumed <= CONTEXT_CHUNK_BYTES as u64);
    Ok(chunk)
}

/// Reads the last plain lines of one research job's agent output, with secrets hidden.
/// Takes the engine database path, whose folder holds the research runner's agent state,
/// and the job id. Returns the newest lines within RESEARCH_LOG_LINES_MAX and
/// RESEARCH_LOG_BYTES_MAX, so the reply fits one owner frame; none before the first run.
#[cfg(unix)]
pub fn research_log(database: &Path, job: i64) -> Result<Vec<String>, EngineError> {
    assert!(job > 0);
    let agents = database
        .parent()
        .ok_or_else(|| failure("runtime_parent_missing"))?
        .join("research")
        .join("agents");
    let Ok(directory) = rustix::fs::open(
        agents.as_path(),
        open_flags() | OFlags::DIRECTORY,
        Mode::empty(),
    ) else {
        return Ok(Vec::new());
    };
    let Some(run) = latest_run(&agents, job)? else {
        return Ok(Vec::new());
    };
    let Ok(run) = open_directory(&directory, &run) else {
        return Ok(Vec::new());
    };
    let mut lines = Vec::new();
    for name in RESEARCH_LOG_FILES {
        let Ok(fd) = rustix::fs::openat(&run, name, open_flags(), Mode::empty()) else {
            continue;
        };
        let Ok((mut file, bytes)) = regular(fd, "research_log_unreadable") else {
            continue;
        };
        let start = bytes.saturating_sub(RESEARCH_LOG_TAIL_BYTES);
        std::io::Seek::seek(&mut file, std::io::SeekFrom::Start(start)).map_err(error)?;
        let mut tail = Vec::new();
        file.take(RESEARCH_LOG_TAIL_BYTES)
            .read_to_end(&mut tail)
            .map_err(error)?;
        let text = String::from_utf8_lossy(&tail);
        let mut rows = text.lines();
        if start > 0 {
            rows.next();
        }
        lines.extend(rows.filter_map(plain_log_line));
    }
    let mut kept = 0;
    let mut bytes = 0;
    for line in lines.iter().rev() {
        if kept == RESEARCH_LOG_LINES_MAX || bytes + line.len() > RESEARCH_LOG_BYTES_MAX {
            break;
        }
        kept += 1;
        bytes += line.len();
    }
    let lines = lines.split_off(lines.len() - kept);
    assert!(lines.len() <= RESEARCH_LOG_LINES_MAX);
    assert!(lines.iter().map(String::len).sum::<usize>() <= RESEARCH_LOG_BYTES_MAX);
    Ok(lines)
}

/// Refuses research job output where no directory-relative no-follow read exists.
/// Takes the engine database path and job id, both unused.
/// Returns `research_log_unavailable`.
#[cfg(not(unix))]
pub fn research_log(_database: &Path, _job: i64) -> Result<Vec<String>, EngineError> {
    Err(failure("research_log_unavailable"))
}

/// Names the newest run folder of a research job in the runner's agent state.
/// Takes the agents folder and the job id; run folders are named job-<id>-<attempt> with an
/// optional -<launch>. Returns the highest attempt and launch, or none before any run.
#[cfg(unix)]
fn latest_run(agents: &Path, job: i64) -> Result<Option<String>, EngineError> {
    let prefix = format!("job-{job}-");
    let mut latest: Option<((u32, u32), String)> = None;
    for entry in std::fs::read_dir(agents)
        .map_err(error)?
        .take(RESEARCH_RUNS_SCANNED_MAX)
    {
        let Ok(name) = entry.map_err(error)?.file_name().into_string() else {
            continue;
        };
        let Some(rest) = name.strip_prefix(prefix.as_str()) else {
            continue;
        };
        let mut parts = rest.split('-');
        let attempt = parts.next().and_then(|part| part.parse::<u32>().ok());
        let launch = parts
            .next()
            .map_or(Some(0), |part| part.parse::<u32>().ok());
        let (Some(attempt), Some(launch), None) = (attempt, launch, parts.next()) else {
            continue;
        };
        if latest
            .as_ref()
            .is_none_or(|(order, _)| (attempt, launch) > *order)
        {
            latest = Some(((attempt, launch), name));
        }
    }
    Ok(latest.map(|(_, name)| name))
}

/// Turns one raw agent log line into plain text with likely secrets hidden.
/// Takes the raw line; a JSON event gives its message text and textless events are skipped.
/// Returns the bounded plain line, or none for an empty or textless line.
#[cfg(unix)]
fn plain_log_line(raw: &str) -> Option<String> {
    let text = match serde_json::from_str::<serde_json::Value>(raw.trim()) {
        Ok(event) if event.is_object() => event_text(&event)?,
        _ => raw.to_owned(),
    };
    let text: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    let line = redact(text);
    if line.chars().count() <= RESEARCH_LOG_LINE_CHARS_MAX {
        return Some(line);
    }
    let mut cut: String = line.chars().take(RESEARCH_LOG_LINE_CHARS_MAX).collect();
    cut.push_str("...");
    Some(cut)
}

#[cfg(unix)]
fn event_text(event: &serde_json::Value) -> Option<String> {
    [Some(event), event.get("item"), event.get("msg")]
        .into_iter()
        .flatten()
        .find_map(|scope| {
            ["text", "message", "command"]
                .into_iter()
                .find_map(|key| scope.get(key)?.as_str().map(str::to_owned))
        })
}

/// Hides words and lines that look like credentials before a log line leaves the engine.
/// Takes one plain line. Returns it with token-like words replaced, or a fixed notice for a
/// line that names a credential field.
#[cfg(unix)]
fn redact(line: &str) -> String {
    let lower = line.to_ascii_lowercase();
    if SECRET_MARKERS.iter().any(|marker| lower.contains(marker)) {
        return HIDDEN_LINE.into();
    }
    line.split(' ')
        .map(|word| if secret_like(word) { HIDDEN_WORD } else { word })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(unix)]
fn secret_like(word: &str) -> bool {
    let core = word.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '-' && c != '_');
    let prefixed = SECRET_PREFIXES
        .iter()
        .any(|prefix| core.starts_with(*prefix) && core.len() >= 16);
    let opaque = core.len() >= 32
        && core
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        && core.bytes().any(|b| b.is_ascii_digit())
        && core.bytes().any(|b| b.is_ascii_alphabetic());
    prefixed || opaque
}

#[cfg(all(test, unix))]
mod engine_tests;
#[cfg(all(test, unix))]
mod tests;
