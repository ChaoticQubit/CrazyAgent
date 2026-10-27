use super::changes::ProjectChanges;
use super::new_project::PoolChoice;
use super::new_project_input::TextInput;
use super::new_project_view::wrap_words;
use super::project_markdown::{MdLine, render, sanitize};
use super::project_plugins::ProjectPlugins;
use crate::memory::{KnowledgeEntry, KnowledgeStatus, ReviewDecision};
use crate::pools::PoolSummary;
use crate::project_setup::{
    ABOUT_BYTES_MAX, CONTEXT_DIRECTORY, ContextItem, ContextKind, DOCUMENT_PATH_BYTES_MAX,
    Platform, ProjectContext, ProjectSetup, RESEARCH_LOG_LINES_MAX, ResearchJob, ResearchKind,
    ResearchStatus, SetupAction, SetupQuery, SetupView, StoredSetup,
};
use crate::protocol::{CredentialRef, ProjectId, RequestId};
use crate::tui::seam::PoolScope;
use crate::tui::text::coded;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::path::PathBuf;

pub(in crate::tui) const REFRESH_MS: i64 = 3_000;
const PREVIEW_BYTES_MAX: usize = 1024 * 1024;
const MEMORY_BYTES_MAX: usize = 4096;
pub(in crate::tui) const REVIEW_WIDTH: usize = 54;
pub(in crate::tui) const REVIEW_ROWS: usize = 12;
const MESSAGE_BYTES_MAX: usize = 8192;
const NAME_BYTES_MAX: usize = 256;
const FOLDERS_BYTES_MAX: usize = crate::sandbox::extra_paths::EXTRA_PATHS_MAX * 1025;
const STACK_BYTES_MAX: usize = 2048;
pub(in crate::tui) const GENERAL_FIELDS: usize = 3 + Platform::ALL.len() + ResearchKind::ALL.len();

/// Names the six tabs of a project.
/// Each tab has a label and a section glyph for the tab strip.
/// The order is the tab order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::tui) enum Tab {
    Overview,
    Conversation,
    Agents,
    Changes,
    Context,
    Settings,
}

impl Tab {
    pub const ALL: [Tab; 6] = [
        Self::Overview,
        Self::Conversation,
        Self::Agents,
        Self::Changes,
        Self::Context,
        Self::Settings,
    ];

    /// Gives the tab label.
    /// Takes the tab.
    /// Returns the label, for example "Agents".
    pub fn label(self) -> &'static str {
        match self {
            Self::Overview => "Overview",
            Self::Conversation => "Conversation",
            Self::Agents => "Agents",
            Self::Changes => "Changes",
            Self::Context => "Context",
            Self::Settings => "Settings",
        }
    }

    /// Gives the section glyph shown before the label.
    /// Takes the tab.
    /// Returns the Unicode glyph from DESIGN.md section 6.3.
    pub fn glyph(self) -> &'static str {
        match self {
            Self::Overview => "\u{25eb}",
            Self::Conversation => "\u{00bb}",
            Self::Agents => "\u{229b}",
            Self::Changes => "\u{2213}",
            Self::Context => "\u{29c9}",
            Self::Settings => "\u{2630}",
        }
    }

    fn index(self) -> usize {
        self as usize
    }
}

/// The project's pool as the screen shows it.
/// Holds the plain name, the account count and whether the engine has it assigned to the project.
/// A pool picked in New project but not yet assigned is shown as waiting.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::tui) struct PoolInfo {
    pub id: CredentialRef,
    pub name: String,
    pub accounts: usize,
    pub assigned: bool,
}

/// Finds the project's pool in the engine's pool list.
/// Takes the pool summaries, the projects each pool serves, the project and the pool picked when it was created.
/// Returns the assigned pool with accounts, else the picked pool marked unassigned, else none.
pub(in crate::tui) fn project_pool(
    pools: &[PoolSummary],
    scopes: &[PoolScope],
    project: &ProjectId,
    picked: Option<&CredentialRef>,
) -> Option<PoolInfo> {
    let info = |pool: &PoolSummary, assigned| PoolInfo {
        id: pool.id.clone(),
        name: pool.name.clone(),
        accounts: pool.members.len(),
        assigned,
    };
    let holds = |id: &CredentialRef| {
        scopes.iter().any(|scope| {
            &scope.pool == id
                && scope
                    .projects
                    .iter()
                    .any(|held| held.as_str() == project.as_str())
        })
    };
    if let Some(pool) = pools
        .iter()
        .find(|pool| !pool.members.is_empty() && holds(&pool.id))
    {
        return Some(info(pool, true));
    }
    let picked = picked?;
    pools
        .iter()
        .find(|pool| &pool.id == picked)
        .map(|pool| info(pool, false))
}

/// Who wrote one transcript message.
/// You is the owner, Master the leading harness, Notice a plain status from CrazyAgent.
/// Notices that are errors carry the error flag on the message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::tui) enum Speaker {
    You,
    Master,
    Notice,
}

/// One message in the master conversation.
/// Holds who wrote it, the local time when known, the text and whether it reports a failure.
/// The text is untrusted and sanitized when drawn.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::tui) struct Message {
    pub speaker: Speaker,
    pub time: Option<String>,
    pub text: String,
    pub error: bool,
}

/// The master's status word in the conversation header.
/// Uses the status words of DESIGN.md section 2.
/// NotReady is shown until the engine reports the master; Thinking carries the start time when the engine knows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::tui) enum MasterStatus {
    Ready,
    Thinking { since_ms: Option<i64> },
    Replied,
    NeedsYou,
    NeedsPool,
    NotSignedIn,
    Stopped,
    NotReady,
}

impl MasterStatus {
    /// Says whether the last turn stopped and r can try it again.
    /// Takes the status.
    /// Returns true for Stopped and Not signed in.
    pub fn retryable(&self) -> bool {
        matches!(self, Self::Stopped | Self::NotSignedIn)
    }
}

/// The master conversation the Conversation tab draws.
/// Holds the master harness name, its account, whether tools are on, whether its conversation
/// is open, the status and the messages.
/// The host fills it from the engine's master session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::tui) struct Transcript {
    pub harness: String,
    pub account: Option<String>,
    pub tools: bool,
    pub open: bool,
    pub status: MasterStatus,
    pub messages: Vec<Message>,
}

/// A loaded context file for the reader.
/// Holds the path inside the project folder, the text read so far, where the next chunk starts,
/// whether a chunk has arrived and why the read failed. Rendered lines are cached per width.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(in crate::tui) struct Preview {
    pub path: String,
    pub text: String,
    pub binary: bool,
    pub next: Option<u64>,
    pub scroll: usize,
    pub loaded: bool,
    pub problem: Option<String>,
    cache: Option<(usize, Vec<MdLine>)>,
}

impl Preview {
    /// Gives the report rendered for a width, rendering once per width.
    /// Takes the width in cells.
    /// Returns the wrapped lines.
    pub fn lines(&mut self, width: usize) -> &[MdLine] {
        if self
            .cache
            .as_ref()
            .is_none_or(|(cached, _)| *cached != width)
        {
            self.cache = Some((width, render(&self.text, width)));
        }
        self.cache
            .as_ref()
            .map_or(&[][..], |(_, lines)| lines.as_slice())
    }
}

/// The "Save to memory" edit box over Overview.
/// Holds the memory being edited and the text field.
/// Enter saves the edited text; Esc cancels.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::tui) struct MemoryEdit {
    pub entry: RequestId,
    pub version: u64,
    pub input: TextInput,
}

/// The whole text of a suggested memory, shown before the owner saves it.
/// Holds the memory, its stored text and the first line on screen.
/// y saves once the last line is on screen; e edits, n dismisses, Esc goes back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::tui) struct MemoryReview {
    pub entry: RequestId,
    pub version: u64,
    pub text: String,
    pub scroll: usize,
}

impl MemoryReview {
    /// Wraps the memory the way the review box shows it.
    /// Takes nothing; hidden characters show as escapes and terminal controls are dropped.
    /// Returns the lines, at most REVIEW_WIDTH cells each.
    pub fn lines(&self) -> Vec<String> {
        wrap_words(&sanitize(&self.text), REVIEW_WIDTH)
    }

    /// Tells whether the last line of the memory is on screen.
    /// Takes nothing.
    /// Returns true once the owner has scrolled to the end.
    pub fn at_end(&self) -> bool {
        self.scroll + REVIEW_ROWS >= self.lines().len()
    }
}

/// The Add file box over the Context tab: the full path of one file to copy
/// into the project's documents.
/// Busy while the engine copies it; a refusal keeps the path with the reason.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::tui) struct AddFile {
    pub input: TextInput,
    pub busy: bool,
    pub error: Option<String>,
}

/// One row of the General form on the Settings tab, in form order.
/// Text rows edit the name, description and tech stack; the others are
/// platform and research switches.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::tui) enum Field {
    Name,
    About,
    Stack,
    Platform(Platform),
    Research(ResearchKind),
}

impl Field {
    /// Gives the form row at one position.
    /// Takes a position below `GENERAL_FIELDS`.
    /// Returns the row: name, description, stack, then platforms, then research.
    pub fn at(index: usize) -> Self {
        assert!(index < GENERAL_FIELDS);
        let platforms = Platform::ALL.len();
        match index {
            0 => Self::Name,
            1 => Self::About,
            2 => Self::Stack,
            index if index < 3 + platforms => Self::Platform(Platform::ALL[index - 3]),
            index => Self::Research(ResearchKind::ALL[index - 3 - platforms]),
        }
    }
}

/// The General form on the Settings tab: the project's name, description,
/// tech stack, platforms and research, saved as one engine edit.
/// Holds the setup revision it started from, so a change made elsewhere is refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::tui) struct GeneralEdit {
    pub name: TextInput,
    pub about: TextInput,
    pub stack: TextInput,
    pub platforms: Vec<Platform>,
    pub research: Vec<ResearchKind>,
    pub cursor: usize,
    pub revision: u64,
    pub busy: bool,
    pub error: Option<String>,
}

impl GeneralEdit {
    fn of(setup: &StoredSetup) -> Self {
        let field = |limit: usize, multi: bool, value: &str| {
            let mut input = if multi {
                TextInput::multi(limit)
            } else {
                TextInput::single(limit)
            };
            input.set(value);
            input
        };
        Self {
            name: field(NAME_BYTES_MAX, false, &setup.name),
            about: field(ABOUT_BYTES_MAX, true, &setup.about),
            stack: field(STACK_BYTES_MAX, false, &setup.tech_stack.join(", ")),
            platforms: setup.platforms.clone(),
            research: setup.research.clone(),
            cursor: 0,
            revision: setup.revision,
            busy: false,
            error: None,
        }
    }

    /// Gives the text field under the cursor.
    /// Takes the form.
    /// Returns the field, or none on a switch row.
    pub fn text(&mut self) -> Option<&mut TextInput> {
        match Field::at(self.cursor) {
            Field::Name => Some(&mut self.name),
            Field::About => Some(&mut self.about),
            Field::Stack => Some(&mut self.stack),
            Field::Platform(_) | Field::Research(_) => None,
        }
    }

    /// Says whether the cursor is on a text row rather than a switch.
    /// Takes the form.
    /// Returns true on the name, description and stack rows.
    pub fn on_text(&self) -> bool {
        matches!(
            Field::at(self.cursor),
            Field::Name | Field::About | Field::Stack
        )
    }

    fn toggle(&mut self) {
        match Field::at(self.cursor) {
            Field::Platform(platform) => toggle(&mut self.platforms, platform),
            Field::Research(kind) => toggle(&mut self.research, kind),
            Field::Name | Field::About | Field::Stack => {}
        }
    }

    /// Builds the engine edit from the form.
    /// Takes the form and the setup it edits.
    /// Returns the edit with the name only when it changed, or none when nothing changed.
    pub fn action(&self, setup: &StoredSetup) -> Option<SetupAction> {
        let name = self.name.value().trim();
        let about = self.about.value().trim().to_owned();
        let tech_stack: Vec<String> = self
            .stack
            .value()
            .split(',')
            .map(str::trim)
            .filter(|entry| !entry.is_empty())
            .map(str::to_owned)
            .collect();
        let platforms: Vec<Platform> = Platform::ALL
            .into_iter()
            .filter(|platform| self.platforms.contains(platform))
            .collect();
        let research: Vec<ResearchKind> = ResearchKind::ALL
            .into_iter()
            .filter(|kind| self.research.contains(kind))
            .collect();
        let unchanged = name == setup.name
            && about == setup.about
            && tech_stack == setup.tech_stack
            && platforms == sorted(&setup.platforms)
            && research == sorted(&setup.research);
        if unchanged {
            return None;
        }
        Some(SetupAction::Update {
            project_id: setup.project_id.clone(),
            expected_revision: self.revision,
            name: (name != setup.name).then(|| name.to_owned()),
            setup: ProjectSetup {
                about,
                tech_stack,
                platforms,
                research,
                pool: setup.pool.clone(),
                team: Some(setup.team.clone()),
                documents: Vec::new(),
            },
        })
    }
}

fn sorted<T: Ord + Copy>(list: &[T]) -> Vec<T> {
    let mut list = list.to_vec();
    list.sort();
    list
}

fn toggle<T: PartialEq + Copy>(list: &mut Vec<T>, value: T) {
    if let Some(at) = list.iter().position(|item| *item == value) {
        list.remove(at);
    } else {
        list.push(value);
    }
}

/// The last lines of one research agent's output, as the engine last read them.
/// Holds the job, its status at that read, the lines (none when the read failed) and when it was read.
/// The engine hides secrets and control characters before the lines arrive.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::tui) struct JobLog {
    pub job: i64,
    pub status: ResearchStatus,
    pub lines: Option<Vec<String>>,
    pub read_ms: i64,
}

/// What the owner asked to see from a project page: its folder, shown in the file manager,
/// or one context file inside it, as the engine's project record names them.
/// `Context` keeps the folder and the path inside it apart so the host can check both.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::tui) enum OpenRequest {
    Folder(String),
    Context { folder: String, path: String },
}

/// What the project screen asks its host to do after a key.
/// Ignored lets global keys run; the other replies are engine calls or navigation.
/// Engine calls carry exactly what the engine needs.
#[derive(Debug, PartialEq, Eq)]
pub(in crate::tui) enum ProjectReply {
    Ignored,
    Handled,
    Home,
    Change(Box<SetupAction>),
    Review {
        entry: RequestId,
        version: u64,
        decision: ReviewDecision,
    },
    Open(OpenRequest),
    PickPool,
    Assign(Option<crate::protocol::CredentialRef>),
    Send(String),
    StopTurn,
    EndConversation,
    Retry,
    Team,
    Process,
    AccountOrder(Option<crate::scheduling::Strategy>),
}

/// One project's screen: its tabs, engine data and what each tab has focused.
/// Takes engine reads through `apply` and keys through `key`.
/// Never invents data: missing reads show as loading or not ready.
pub(in crate::tui) struct ProjectScreen {
    pub project: ProjectId,
    pub name: String,
    pub tab: Tab,
    pub width: u16,
    pub home: Option<PathBuf>,
    pub setup: Option<StoredSetup>,
    pub setup_missing: bool,
    pub jobs: Option<Vec<ResearchJob>>,
    pub context: Option<ProjectContext>,
    pub memories: Option<Vec<KnowledgeEntry>>,
    pub pool: Option<PoolInfo>,
    pub pool_ready: bool,
    pub pool_choices: Vec<PoolChoice>,
    pub pool_picker: Option<usize>,
    pub transcript: Option<Transcript>,
    pub input: TextInput,
    pub typing: bool,
    pub transcript_scroll: usize,
    pub overview_cursor: usize,
    pub agents_cursor: usize,
    pub tree_cursor: usize,
    pub preview: Option<Preview>,
    pub reading: bool,
    pub edit: Option<MemoryEdit>,
    pub review: Option<MemoryReview>,
    pub confirm_delete: Option<String>,
    pub confirm_stop: Option<i64>,
    pub confirm_remove: bool,
    pub adding: Option<AddFile>,
    pub folders: Option<AddFile>,
    pub read_paths: Option<Vec<String>>,
    pub schedule: Option<crate::scheduling::Schedule>,
    pub general: Option<GeneralEdit>,
    pub removed: bool,
    pub log: Option<JobLog>,
    pub problem: Option<String>,
    pub plugins: ProjectPlugins,
    pub changes: ProjectChanges,
    pub now_ms: i64,
    pub offset: i64,
    jobs_read_ms: Option<i64>,
    context_read_ms: Option<i64>,
    read_paths_read_ms: Option<i64>,
    memories_read_ms: Option<i64>,
    setup_failed_ms: Option<i64>,
    setup_due: bool,
    waiting: Option<SetupQuery>,
}

impl ProjectScreen {
    /// Opens a project on its Overview tab.
    /// Takes the project, its name from the engine snapshot and the owner's home folder.
    /// Returns the screen; engine reads start with `next_query`.
    pub fn new(project: ProjectId, name: String, home: Option<PathBuf>) -> Self {
        Self {
            plugins: ProjectPlugins::new(project.clone()),
            changes: ProjectChanges::new(project.clone()),
            project,
            name,
            tab: Tab::Overview,
            width: 80,
            home,
            setup: None,
            setup_missing: false,
            jobs: None,
            context: None,
            memories: None,
            pool: None,
            pool_ready: true,
            pool_choices: Vec::new(),
            pool_picker: None,
            transcript: None,
            input: TextInput::multi(MESSAGE_BYTES_MAX),
            typing: false,
            transcript_scroll: 0,
            overview_cursor: 0,
            agents_cursor: 0,
            tree_cursor: 0,
            preview: None,
            reading: false,
            edit: None,
            review: None,
            confirm_delete: None,
            confirm_stop: None,
            confirm_remove: false,
            adding: None,
            folders: None,
            read_paths: None,
            schedule: None,
            general: None,
            removed: false,
            log: None,
            problem: None,
            now_ms: 0,
            offset: 0,
            jobs_read_ms: None,
            context_read_ms: None,
            read_paths_read_ms: None,
            memories_read_ms: None,
            setup_failed_ms: None,
            setup_due: false,
            waiting: None,
        }
    }

    /// Opens one tab.
    /// Takes the tab; the Conversation tab puts focus in the message input.
    /// Returns nothing.
    pub fn open(&mut self, tab: Tab) {
        self.tab = tab;
        self.reading = false;
        self.confirm_delete = None;
        self.confirm_stop = None;
        self.confirm_remove = false;
        self.adding = None;
        self.folders = None;
        self.general = None;
        self.typing = tab == Tab::Conversation;
        if tab == Tab::Changes {
            self.changes.refresh();
        }
    }

    /// Gives the next engine read this screen needs, at most one at a time.
    /// Takes the screen and the current time in milliseconds.
    /// Returns the query to send, or none while one is waiting or nothing is due.
    pub fn next_query(&mut self, now_ms: i64) -> Option<SetupQuery> {
        self.now_ms = now_ms;
        if self.waiting.is_some() {
            return None;
        }
        let project = self.project.clone();
        let due = |last: Option<i64>| last.is_none_or(|last| now_ms - last >= REFRESH_MS);
        let setup_wanted = (self.setup.is_none() || self.setup_due) && !self.setup_missing;
        let query = if setup_wanted && due(self.setup_failed_ms) {
            SetupQuery::Setup { project }
        } else if self.setup_missing || self.setup.is_none() {
            return None;
        } else if self.jobs_read_ms.is_none() || (due(self.jobs_read_ms) && self.jobs_live()) {
            SetupQuery::Jobs { project }
        } else if let Some(job) = self.log_due(now_ms) {
            SetupQuery::Log { project, job }
        } else if due(self.context_read_ms)
            && (self.context.is_none() || matches!(self.tab, Tab::Context))
        {
            SetupQuery::Context { project }
        } else if self.read_paths.is_none()
            && self.tab == Tab::Settings
            && due(self.read_paths_read_ms)
        {
            SetupQuery::ReadPaths { project }
        } else if due(self.memories_read_ms)
            && (self.memories.is_none() || matches!(self.tab, Tab::Overview))
        {
            SetupQuery::Memories {
                project,
                after: None,
            }
        } else if let Some(preview) = self
            .preview
            .as_ref()
            .filter(|p| p.text.is_empty() && !p.binary && !p.loaded && p.problem.is_none())
        {
            SetupQuery::Item {
                project,
                path: preview.path.clone(),
                offset: 0,
            }
        } else {
            let preview = self
                .preview
                .as_ref()
                .filter(|p| p.next.is_some() && p.text.len() < PREVIEW_BYTES_MAX)?;
            SetupQuery::Item {
                project,
                path: preview.path.clone(),
                offset: preview.next.unwrap_or(0),
            }
        };
        self.waiting = Some(query.clone());
        Some(query)
    }

    /// Forgets the read in flight after the engine connection dropped, so the next read is sent again.
    /// Takes the screen; loaded data stays on screen.
    /// Returns nothing.
    pub fn forget_reads(&mut self) {
        self.waiting = None;
    }

    /// Gives the research job focused on the Agents tab.
    /// Takes the screen.
    /// Returns the job under the cursor, or none before the jobs are read.
    pub fn focused_job(&self) -> Option<&ResearchJob> {
        self.jobs.as_ref()?.get(self.agents_cursor)
    }

    fn log_due(&self, now_ms: i64) -> Option<i64> {
        if self.tab != Tab::Agents {
            return None;
        }
        let job = self.focused_job()?;
        let fresh = self.log.as_ref().is_some_and(|log| {
            log.job == job.id
                && log.status == job.status
                && (job.status != ResearchStatus::Running || now_ms - log.read_ms < REFRESH_MS)
        });
        (!fresh).then_some(job.id)
    }

    fn log_read(&mut self, job: i64, lines: Option<Vec<String>>) {
        let Some(status) = self
            .jobs
            .iter()
            .flatten()
            .find(|known| known.id == job)
            .map(|known| known.status)
        else {
            return;
        };
        let lines = match (lines, self.log.take()) {
            (None, Some(old)) if old.job == job => old.lines,
            (Some(mut lines), _) => {
                let start = lines.len().saturating_sub(RESEARCH_LOG_LINES_MAX);
                Some(lines.split_off(start))
            }
            (None, _) => None,
        };
        self.log = Some(JobLog {
            job,
            status,
            lines,
            read_ms: self.now_ms,
        });
    }

    fn jobs_live(&self) -> bool {
        self.jobs.as_ref().is_none_or(|jobs| {
            jobs.iter().any(|job| {
                matches!(
                    job.status,
                    ResearchStatus::Queued | ResearchStatus::Running | ResearchStatus::NeedsPool
                )
            })
        })
    }

    /// Applies one engine read or change result.
    /// Takes the query that was sent and the engine's view or plain problem.
    /// Returns nothing; a failed read keeps the last good data and shows the problem.
    pub fn apply(&mut self, query: &SetupQuery, result: Result<SetupView, String>) {
        if self.waiting.as_ref() == Some(query) {
            self.waiting = None;
        }
        let view = match result {
            Ok(view) => view,
            Err(problem) if problem == "project_setup_missing" => {
                self.setup_missing = true;
                return;
            }
            Err(problem) => {
                if self.refused(query, &problem) {
                    return;
                }
                match query {
                    SetupQuery::Setup { .. } => {
                        self.setup_due = false;
                        self.setup_failed_ms = Some(self.now_ms);
                    }
                    SetupQuery::Jobs { .. } => self.jobs_read_ms = Some(self.now_ms),
                    SetupQuery::Context { .. } => self.context_read_ms = Some(self.now_ms),
                    SetupQuery::ReadPaths { .. } => self.read_paths_read_ms = Some(self.now_ms),
                    SetupQuery::Memories { .. } => self.memories_read_ms = Some(self.now_ms),
                    SetupQuery::Item { path, .. } => {
                        if let Some(preview) = self.preview.as_mut().filter(|p| &p.path == path) {
                            preview.next = None;
                            preview.problem = Some(plain_problem(&problem));
                            preview.cache = None;
                        }
                    }
                    SetupQuery::Log { job, .. } => self.log_read(*job, None),
                    SetupQuery::Change {
                        action:
                            SetupAction::CancelResearch { .. } | SetupAction::RetryResearch { .. },
                        ..
                    } => self.jobs_read_ms = None,
                    _ => {}
                }
                self.problem = Some(plain_problem(&problem));
                return;
            }
        };
        match view {
            SetupView::Setup(Some(setup)) => {
                self.name = setup.name.clone();
                self.setup = Some(*setup);
                self.setup_due = false;
                self.setup_failed_ms = None;
            }
            SetupView::Setup(None) => self.setup_missing = true,
            SetupView::Jobs(jobs) => {
                self.agents_cursor = self.agents_cursor.min(jobs.len().saturating_sub(1));
                self.jobs = Some(jobs);
                self.jobs_read_ms = Some(self.now_ms);
            }
            SetupView::Context(context) => {
                self.tree_cursor = self.tree_cursor.min(context.items.len().saturating_sub(1));
                self.context = Some(context);
                self.context_read_ms = Some(self.now_ms);
            }
            SetupView::Memories(memories) => {
                self.memories = Some(memories);
                self.memories_read_ms = Some(self.now_ms);
            }
            SetupView::ReadPaths(paths) => {
                self.read_paths = Some(paths);
                self.read_paths_read_ms = Some(self.now_ms);
            }
            SetupView::Item(chunk) => {
                if let Some(preview) = self.preview.as_mut().filter(|p| p.path == chunk.path) {
                    preview.loaded = true;
                    match chunk.text {
                        Some(text) => {
                            preview.text.push_str(&text);
                            preview.next = chunk.next_offset;
                        }
                        None => {
                            preview.binary = true;
                            preview.next = None;
                        }
                    }
                    preview.cache = None;
                }
            }
            SetupView::Log(lines) => {
                if let SetupQuery::Log { job, .. } = query {
                    self.log_read(*job, Some(lines));
                }
            }
            SetupView::Changed(result) => self.changed(query, result),
            SetupView::Catalog(_) | SetupView::Team(_) | SetupView::Library(_) => {}
        }
        self.assert_consistent();
    }

    /// Puts a refused add, edit or remove back in its box with the reason.
    /// Takes the change query and the plain problem code.
    /// Returns true when an open box took the refusal.
    fn refused(&mut self, query: &SetupQuery, code: &str) -> bool {
        let SetupQuery::Change { action, .. } = query else {
            return false;
        };
        let reason = Some(plain_problem(code));
        match action {
            SetupAction::AddDocuments { .. } if self.adding.is_some() => {
                if let Some(adding) = self.adding.as_mut() {
                    adding.busy = false;
                    adding.error = reason;
                }
                true
            }
            SetupAction::SetReadPaths { .. } if self.folders.is_some() => {
                if let Some(folders) = self.folders.as_mut() {
                    folders.busy = false;
                    folders.error = reason;
                }
                true
            }
            SetupAction::Update { .. } if self.general.is_some() => {
                if let Some(general) = self.general.as_mut() {
                    general.busy = false;
                    general.error = reason;
                }
                true
            }
            _ => false,
        }
    }

    fn changed(&mut self, query: &SetupQuery, result: crate::engine::owner::OwnerResult) {
        use crate::engine::owner::OwnerResult;
        let applied = matches!(result, OwnerResult::Applied);
        match &result {
            OwnerResult::Applied => {}
            OwnerResult::Rejected { code } => {
                if !self.refused(query, code) {
                    self.problem = Some(plain_problem(code));
                }
            }
            _ => {
                self.refused(query, "change_outcome_unknown");
            }
        }
        match query {
            SetupQuery::Change {
                action: SetupAction::AddDocuments { .. },
                ..
            } => {
                self.context_read_ms = None;
                if applied {
                    self.adding = None;
                }
            }
            SetupQuery::Change {
                action: SetupAction::Update { .. },
                ..
            } => {
                self.setup_due = true;
                if applied {
                    self.general = None;
                }
            }
            SetupQuery::Change {
                action: SetupAction::Remove { .. },
                ..
            } => self.removed = applied,
            SetupQuery::Change {
                action: SetupAction::SetReadPaths { .. },
                ..
            } => {
                self.read_paths = None;
                self.read_paths_read_ms = None;
                if applied {
                    self.folders = None;
                }
            }
            SetupQuery::Change {
                action: SetupAction::DeleteContext { path, .. },
                ..
            } if applied => {
                if self.preview.as_ref().is_some_and(|p| &p.path == path) {
                    self.preview = None;
                    self.reading = false;
                }
                self.context_read_ms = None;
                self.context = None;
            }
            SetupQuery::Change {
                action: SetupAction::CancelResearch { .. } | SetupAction::RetryResearch { .. },
                ..
            } => {
                self.jobs_read_ms = None;
                if applied {
                    self.log = None;
                }
            }
            SetupQuery::Change { .. } | SetupQuery::ReviewMemory { .. } => {
                self.memories_read_ms = None;
                self.memories = None;
                self.context_read_ms = None;
            }
            _ => {}
        }
    }

    /// Lists the memories waiting for the owner's approval.
    /// Takes the screen.
    /// Returns suggested memories, oldest first.
    pub fn suggested(&self) -> Vec<&KnowledgeEntry> {
        self.memories
            .as_ref()
            .map(|memories| {
                memories
                    .iter()
                    .filter(|m| m.status == KnowledgeStatus::Provisional && !m.human_reviewed)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Lists the context files in tree order: brief, research, then documents.
    /// Takes the screen.
    /// Returns the items, or an empty list before the first read.
    pub fn tree(&self) -> Vec<&ContextItem> {
        let mut items: Vec<&ContextItem> = self
            .context
            .as_ref()
            .map(|context| context.items.iter().collect())
            .unwrap_or_default();
        items.sort_by_key(|item| {
            (
                match item.kind {
                    ContextKind::Brief => 0,
                    ContextKind::Research => 1,
                    ContextKind::Document => 2,
                },
                item.path.clone(),
            )
        });
        items
    }

    /// Finds the research job writing one report.
    /// Takes the context path of the report.
    /// Returns the job, when one exists.
    pub fn job_for(&self, path: &str) -> Option<&ResearchJob> {
        self.jobs
            .as_ref()?
            .iter()
            .filter(|job| job.report_path == path)
            .max_by_key(|job| job.id)
    }

    /// Counts agents by state for the header and the Agents title.
    /// Takes the screen.
    /// Returns running, waiting, failed and done counts.
    pub fn counts(&self) -> (usize, usize, usize, usize) {
        let mut counts = (0, 0, 0, 0);
        for job in self.jobs.iter().flatten() {
            match job.status {
                ResearchStatus::Running => counts.0 += 1,
                ResearchStatus::Queued | ResearchStatus::NeedsPool => counts.1 += 1,
                ResearchStatus::Failed => counts.2 += 1,
                ResearchStatus::Done => counts.3 += 1,
                ResearchStatus::Cancelled => {}
            }
        }
        counts
    }

    /// Names one context file to open: the folder its context paths start from and the
    /// path inside it. Takes the path inside the project folder.
    /// Returns the request, or none before the folder is known.
    pub fn context_request(&self, path: &str) -> Option<OpenRequest> {
        let folder = self
            .setup
            .as_ref()
            .map(|setup| setup.folder.clone())
            .or_else(|| self.context.as_ref().map(|context| context.folder.clone()))?;
        Some(OpenRequest::Context {
            folder,
            path: path.to_owned(),
        })
    }

    /// Handles one key for the open tab, the edit box or the delete question.
    /// Takes the key event.
    /// Returns what the host should do next.
    pub fn key(&mut self, key: KeyEvent) -> ProjectReply {
        self.problem = None;
        if self.edit.is_some() {
            return self.edit_key(key);
        }
        if self.review.is_some() {
            return self.review_key(key);
        }
        if self.adding.is_some() {
            return self.add_key(key);
        }
        if self.folders.is_some() {
            return self.folders_key(key);
        }
        if self.general.is_some() {
            return self.general_key(key);
        }
        if self.confirm_remove {
            return match key.code {
                KeyCode::Char('y') => {
                    self.confirm_remove = false;
                    ProjectReply::Change(Box::new(SetupAction::Remove {
                        project_id: self.project.clone(),
                    }))
                }
                KeyCode::Char('n') | KeyCode::Esc => {
                    self.confirm_remove = false;
                    ProjectReply::Handled
                }
                _ => ProjectReply::Handled,
            };
        }
        if let Some(cursor) = self.pool_picker {
            return self.picker_key(key, cursor);
        }
        if let Some(path) = self.confirm_delete.clone() {
            return match key.code {
                KeyCode::Char('y') => {
                    self.confirm_delete = None;
                    ProjectReply::Change(Box::new(SetupAction::DeleteContext {
                        project_id: self.project.clone(),
                        path,
                    }))
                }
                KeyCode::Char('n') | KeyCode::Esc => {
                    self.confirm_delete = None;
                    ProjectReply::Handled
                }
                _ => ProjectReply::Handled,
            };
        }
        if let Some(job) = self.confirm_stop {
            return match key.code {
                KeyCode::Char('y') => {
                    self.confirm_stop = None;
                    ProjectReply::Change(Box::new(SetupAction::CancelResearch {
                        project_id: self.project.clone(),
                        job,
                    }))
                }
                KeyCode::Char('n') | KeyCode::Esc => {
                    self.confirm_stop = None;
                    ProjectReply::Handled
                }
                _ => ProjectReply::Handled,
            };
        }
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Left if shift => return self.tab_by(-1),
            KeyCode::Right if shift => return self.tab_by(1),
            KeyCode::Tab if self.tab == Tab::Conversation => {
                self.typing = !self.typing;
                return ProjectReply::Handled;
            }
            _ => {}
        }
        if self.tab == Tab::Conversation && self.typing {
            return self.input_key(key);
        }
        if !control {
            match key.code {
                KeyCode::Char('[') => return self.tab_by(-1),
                KeyCode::Char(']') => return self.tab_by(1),
                _ => {}
            }
        }
        let reply = match self.tab {
            Tab::Overview => self.overview_key(key),
            Tab::Conversation => self.conversation_key(key),
            Tab::Agents => self.agents_key(key),
            Tab::Changes => ProjectReply::Ignored,
            Tab::Context => self.context_key(key),
            Tab::Settings => self.settings_key(key),
        };
        if reply == ProjectReply::Ignored && key.code == KeyCode::Esc {
            return ProjectReply::Home;
        }
        self.assert_consistent();
        reply
    }

    /// Inserts pasted text into the message input or the memory edit box.
    /// Takes the pasted text.
    /// Returns Handled when a field took it.
    pub fn paste(&mut self, text: &str) -> ProjectReply {
        if let Some(edit) = self.edit.as_mut() {
            edit.input.insert_str(text);
            return ProjectReply::Handled;
        }
        if self.review.is_some() {
            return ProjectReply::Handled;
        }
        if let Some(adding) = self.adding.as_mut().filter(|adding| !adding.busy) {
            adding.input.insert_str(text);
            adding.error = None;
            return ProjectReply::Handled;
        }
        if let Some(folders) = self.folders.as_mut().filter(|folders| !folders.busy) {
            folders.input.insert_str(text);
            folders.error = None;
            return ProjectReply::Handled;
        }
        if let Some(general) = self.general.as_mut().filter(|general| !general.busy) {
            if let Some(field) = general.text() {
                field.insert_str(text);
                general.error = None;
            }
            return ProjectReply::Handled;
        }
        if self.tab == Tab::Conversation && self.typing {
            self.input.insert_str(text);
            return ProjectReply::Handled;
        }
        if self.tab == Tab::Changes && self.changes.paste(text) {
            return ProjectReply::Handled;
        }
        ProjectReply::Ignored
    }

    fn tab_by(&mut self, offset: isize) -> ProjectReply {
        let count = Tab::ALL.len() as isize;
        let next = (self.tab.index() as isize + offset).rem_euclid(count);
        self.open(Tab::ALL[next as usize]);
        ProjectReply::Handled
    }

    fn input_key(&mut self, key: KeyEvent) -> ProjectReply {
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => {
                self.typing = false;
                ProjectReply::Handled
            }
            KeyCode::Enter if key.modifiers.contains(KeyModifiers::SHIFT) => {
                self.input.insert_str("\n");
                ProjectReply::Handled
            }
            KeyCode::Enter => {
                let text = self.input.value().trim().to_owned();
                if text.is_empty() || !self.master_usable() {
                    return ProjectReply::Handled;
                }
                self.input.set("");
                self.transcript_scroll = 0;
                ProjectReply::Send(text)
            }
            KeyCode::Char('x') if control => ProjectReply::StopTurn,
            KeyCode::PageUp => self.scroll_transcript(5),
            KeyCode::PageDown => self.scroll_transcript(-5),
            _ => {
                if self.input.handle(key) || matches!(key.code, KeyCode::Char(_)) && !control {
                    ProjectReply::Handled
                } else {
                    ProjectReply::Ignored
                }
            }
        }
    }

    /// Says whether the master can take a message now.
    /// Takes the screen.
    /// Returns false without a pool or while the master is not ready.
    pub fn master_usable(&self) -> bool {
        self.pool.as_ref().is_some_and(|pool| pool.assigned)
            && self.transcript.as_ref().is_some_and(|t| {
                !matches!(
                    t.status,
                    MasterStatus::NeedsPool | MasterStatus::NotReady | MasterStatus::NotSignedIn
                )
            })
    }

    fn scroll_transcript(&mut self, by: isize) -> ProjectReply {
        self.transcript_scroll = self.transcript_scroll.saturating_add_signed(by);
        ProjectReply::Handled
    }

    fn conversation_key(&mut self, key: KeyEvent) -> ProjectReply {
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Char('s') if !self.pool.as_ref().is_some_and(|p| p.assigned) => {
                self.open(Tab::Settings);
                ProjectReply::Handled
            }
            KeyCode::Char('r')
                if self
                    .transcript
                    .as_ref()
                    .is_some_and(|t| t.status.retryable()) =>
            {
                ProjectReply::Retry
            }
            KeyCode::Char('x') if control => ProjectReply::StopTurn,
            KeyCode::Char('e') if !control && self.transcript.as_ref().is_some_and(|t| t.open) => {
                ProjectReply::EndConversation
            }
            KeyCode::PageUp => self.scroll_transcript(5),
            KeyCode::PageDown => self.scroll_transcript(-5),
            KeyCode::Enter => {
                self.typing = true;
                ProjectReply::Handled
            }
            _ => ProjectReply::Ignored,
        }
    }

    /// Finds the suggested memory that came from the report being read.
    /// Takes the screen; only research reports have one.
    /// Returns the memory, when the report being read suggested one.
    pub fn report_suggestion(&self) -> Option<&KnowledgeEntry> {
        let kind = research_kind(&self.preview.as_ref()?.path)?;
        self.suggested()
            .into_iter()
            .find(|memory| memory_source(memory) == Some(kind))
    }

    fn decide(&mut self, memory: Option<(RequestId, u64, String)>, code: KeyCode) -> ProjectReply {
        let Some((entry, version, text)) = memory else {
            return ProjectReply::Handled;
        };
        match code {
            KeyCode::Char('y') => {
                self.review = Some(MemoryReview {
                    entry,
                    version,
                    text,
                    scroll: 0,
                });
                ProjectReply::Handled
            }
            KeyCode::Char('n') => ProjectReply::Review {
                entry,
                version,
                decision: ReviewDecision::Reject,
            },
            _ => {
                self.edit_memory(entry, version, &text);
                ProjectReply::Handled
            }
        }
    }

    fn edit_memory(&mut self, entry: RequestId, version: u64, text: &str) {
        let mut input = TextInput::multi(MEMORY_BYTES_MAX);
        input.set(&sanitize(text));
        self.edit = Some(MemoryEdit {
            entry,
            version,
            input,
        });
    }

    fn review_key(&mut self, key: KeyEvent) -> ProjectReply {
        let Some(review) = self.review.as_mut() else {
            return ProjectReply::Ignored;
        };
        let last = review.lines().len().saturating_sub(REVIEW_ROWS);
        match key.code {
            KeyCode::Esc => self.review = None,
            KeyCode::Up => review.scroll = review.scroll.saturating_sub(1),
            KeyCode::Down => review.scroll = (review.scroll + 1).min(last),
            KeyCode::PageUp => review.scroll = review.scroll.saturating_sub(REVIEW_ROWS),
            KeyCode::PageDown => review.scroll = (review.scroll + REVIEW_ROWS).min(last),
            KeyCode::Char('y') if review.at_end() => {
                let Some(review) = self.review.take() else {
                    return ProjectReply::Ignored;
                };
                let shown = sanitize(&review.text);
                let decision = if shown == review.text {
                    ReviewDecision::Keep
                } else {
                    ReviewDecision::Edit {
                        text: shown.trim().to_owned(),
                    }
                };
                return ProjectReply::Review {
                    entry: review.entry,
                    version: review.version,
                    decision,
                };
            }
            KeyCode::Char('n') => {
                let Some(review) = self.review.take() else {
                    return ProjectReply::Ignored;
                };
                return ProjectReply::Review {
                    entry: review.entry,
                    version: review.version,
                    decision: ReviewDecision::Reject,
                };
            }
            KeyCode::Char('e') => {
                if let Some(review) = self.review.take() {
                    self.edit_memory(review.entry, review.version, &review.text);
                }
            }
            _ => {}
        }
        ProjectReply::Handled
    }

    fn overview_key(&mut self, key: KeyEvent) -> ProjectReply {
        let suggested: Vec<(RequestId, u64, String)> = self
            .suggested()
            .iter()
            .map(|m| (m.id.clone(), m.version, m.text.clone()))
            .collect();
        let work = self.setup.as_ref().map_or(0, |s| s.research.len());
        let rows = suggested.len() + work;
        let focused = suggested.get(self.overview_cursor).cloned();
        match key.code {
            KeyCode::Up => self.overview_cursor = self.overview_cursor.saturating_sub(1),
            KeyCode::Down => {
                self.overview_cursor = (self.overview_cursor + 1).min(rows.saturating_sub(1));
            }
            KeyCode::Char('y' | 'n' | 'e') => return self.decide(focused, key.code),
            KeyCode::Char('o') => {
                return self.setup.as_ref().map_or(ProjectReply::Handled, |s| {
                    ProjectReply::Open(OpenRequest::Folder(
                        s.project_folder().display().to_string(),
                    ))
                });
            }
            KeyCode::Enter
                if !self.pool.as_ref().is_some_and(|p| p.assigned) && suggested.is_empty() =>
            {
                return self.pool_reply();
            }
            KeyCode::Enter => {
                let index = self.overview_cursor.checked_sub(suggested.len());
                let kind = index.and_then(|i| self.setup.as_ref()?.research.get(i).copied());
                if let Some(kind) = kind {
                    self.read(&kind.report_path());
                }
            }
            _ => return ProjectReply::Ignored,
        }
        ProjectReply::Handled
    }

    fn edit_key(&mut self, key: KeyEvent) -> ProjectReply {
        let Some(edit) = self.edit.as_mut() else {
            return ProjectReply::Ignored;
        };
        match key.code {
            KeyCode::Esc => {
                self.edit = None;
                ProjectReply::Handled
            }
            KeyCode::Enter => {
                let text = edit.input.value().trim().to_owned();
                if text.is_empty() {
                    return ProjectReply::Handled;
                }
                let reply = ProjectReply::Review {
                    entry: edit.entry.clone(),
                    version: edit.version,
                    decision: ReviewDecision::Edit { text },
                };
                self.edit = None;
                reply
            }
            _ => {
                edit.input.handle(key);
                ProjectReply::Handled
            }
        }
    }

    fn agents_key(&mut self, key: KeyEvent) -> ProjectReply {
        let count = self.jobs.as_ref().map_or(0, Vec::len);
        match key.code {
            KeyCode::Up => self.agents_cursor = self.agents_cursor.saturating_sub(1),
            KeyCode::Down => {
                self.agents_cursor = (self.agents_cursor + 1).min(count.saturating_sub(1))
            }
            KeyCode::Enter => {
                let path = self
                    .jobs
                    .as_ref()
                    .and_then(|jobs| jobs.get(self.agents_cursor))
                    .map(|job| job.report_path.clone());
                if let Some(path) = path {
                    self.read(&path);
                }
            }
            KeyCode::Char('x') => {
                let Some((job, status, stopping)) = self
                    .focused_job()
                    .map(|job| (job.id, job.status, job.cancel_requested))
                else {
                    return ProjectReply::Handled;
                };
                match status {
                    ResearchStatus::Running if stopping => {
                        self.problem = Some("Stop was already asked. The agent is ending.".into());
                    }
                    ResearchStatus::Queued
                    | ResearchStatus::NeedsPool
                    | ResearchStatus::Running => {
                        self.confirm_stop = Some(job);
                    }
                    ResearchStatus::Done | ResearchStatus::Failed | ResearchStatus::Cancelled => {
                        self.problem = Some("This research is not running.".into());
                    }
                }
            }
            KeyCode::Char('t') => return ProjectReply::Team,
            KeyCode::Char('r') => {
                let Some((job, status)) = self.focused_job().map(|job| (job.id, job.status)) else {
                    return ProjectReply::Handled;
                };
                if matches!(status, ResearchStatus::Failed | ResearchStatus::Cancelled) {
                    return ProjectReply::Change(Box::new(SetupAction::RetryResearch {
                        project_id: self.project.clone(),
                        job,
                    }));
                }
                self.problem = Some("Only failed or stopped research can run again.".into());
            }
            _ => return ProjectReply::Ignored,
        }
        ProjectReply::Handled
    }

    /// Opens a context file in the reader on the Context tab.
    /// Takes the path inside the project folder.
    /// Returns nothing; the text loads with the next reads.
    pub fn read(&mut self, path: &str) {
        self.open(Tab::Context);
        if let Some(index) = self.tree().iter().position(|item| item.path == path) {
            self.tree_cursor = index;
        }
        if self
            .preview
            .as_ref()
            .is_none_or(|p| p.path != path || p.problem.is_some())
        {
            self.preview = Some(Preview {
                path: path.to_owned(),
                ..Preview::default()
            });
        }
        self.reading = true;
    }

    fn context_key(&mut self, key: KeyEvent) -> ProjectReply {
        let items: Vec<ContextItem> = self.tree().into_iter().cloned().collect();
        if self.reading {
            let wide = self.width >= 100;
            match key.code {
                KeyCode::Esc => {
                    self.reading = false;
                    return ProjectReply::Handled;
                }
                KeyCode::PageDown | KeyCode::Down if !wide || key.code == KeyCode::PageDown => {
                    if let Some(preview) = self.preview.as_mut() {
                        preview.scroll += if key.code == KeyCode::PageDown { 10 } else { 1 };
                    }
                    return ProjectReply::Handled;
                }
                KeyCode::PageUp | KeyCode::Up if !wide || key.code == KeyCode::PageUp => {
                    if let Some(preview) = self.preview.as_mut() {
                        preview.scroll = preview
                            .scroll
                            .saturating_sub(if key.code == KeyCode::PageUp { 10 } else { 1 });
                    }
                    return ProjectReply::Handled;
                }
                KeyCode::Char('y' | 'n' | 'e') => {
                    let memory = self
                        .report_suggestion()
                        .map(|m| (m.id.clone(), m.version, m.text.clone()));
                    if memory.is_some() {
                        return self.decide(memory, key.code);
                    }
                }
                _ => {}
            }
        }
        let item = items.get(self.tree_cursor).cloned();
        match key.code {
            KeyCode::Up => {
                self.tree_cursor = self.tree_cursor.saturating_sub(1);
                self.follow_cursor();
            }
            KeyCode::Down => {
                self.tree_cursor = (self.tree_cursor + 1).min(items.len().saturating_sub(1));
                self.follow_cursor();
            }
            KeyCode::Enter => {
                if let Some(item) = item {
                    self.read(&item.path);
                }
            }
            KeyCode::Char('o') => {
                if let Some(request) = item.and_then(|item| self.context_request(&item.path)) {
                    return ProjectReply::Open(request);
                }
            }
            KeyCode::Char('a') => {
                self.adding = Some(AddFile {
                    input: TextInput::single(DOCUMENT_PATH_BYTES_MAX),
                    busy: false,
                    error: None,
                });
            }
            KeyCode::Char('d') => match item {
                Some(item) if item.kind != ContextKind::Brief => {
                    self.confirm_delete = Some(item.path);
                }
                Some(_) => self.problem = Some("The project brief can't be deleted.".into()),
                None => {}
            },
            KeyCode::Char('m') => {
                let research = item
                    .as_ref()
                    .filter(|item| item.kind == ContextKind::Research)
                    .and_then(|item| research_kind(&item.path));
                return match research {
                    Some(kind) => ProjectReply::Change(Box::new(SetupAction::SaveToMemory {
                        project_id: self.project.clone(),
                        research: kind,
                        section: None,
                    })),
                    None => {
                        self.problem = Some("Only research reports can be saved to memory.".into());
                        ProjectReply::Handled
                    }
                };
            }
            _ => return ProjectReply::Ignored,
        }
        ProjectReply::Handled
    }

    fn follow_cursor(&mut self) {
        if self.width < 100 {
            return;
        }
        let path = self
            .tree()
            .get(self.tree_cursor)
            .map(|item| item.path.clone());
        if let Some(path) = path
            && self.preview.as_ref().is_none_or(|p| p.path != path)
        {
            self.preview = Some(Preview {
                path,
                ..Preview::default()
            });
        }
    }

    fn pool_reply(&mut self) -> ProjectReply {
        if !self.pool_ready {
            return ProjectReply::Handled;
        }
        let picked = self.setup.as_ref().and_then(|setup| setup.pool.clone());
        match (&self.pool, picked) {
            (Some(pool), Some(id)) if !pool.assigned => ProjectReply::Assign(Some(id)),
            _ if self.pool_choices.is_empty() => ProjectReply::PickPool,
            (pool, _) => {
                let current = pool.as_ref().filter(|pool| pool.assigned).and_then(|pool| {
                    self.pool_choices
                        .iter()
                        .position(|choice| choice.id == pool.id)
                });
                self.pool_picker = Some(current.unwrap_or(0));
                ProjectReply::Handled
            }
        }
    }

    /// Moves the pool picker or settles it. The rows are the pools, then "No pool" last.
    /// Takes the key and the cursor. Returns the chosen pool, None for "No pool", or Handled.
    fn picker_key(&mut self, key: KeyEvent, cursor: usize) -> ProjectReply {
        let last = self.pool_choices.len();
        assert!(cursor <= last);
        match key.code {
            KeyCode::Up => self.pool_picker = Some(cursor.saturating_sub(1)),
            KeyCode::Down => self.pool_picker = Some((cursor + 1).min(last)),
            KeyCode::Esc => self.pool_picker = None,
            KeyCode::Enter => {
                self.pool_picker = None;
                return ProjectReply::Assign(
                    self.pool_choices
                        .get(cursor)
                        .map(|choice| choice.id.clone()),
                );
            }
            _ => {}
        }
        ProjectReply::Handled
    }

    /// Replaces the pools the picker lists.
    /// Takes the choices in the engine's order; an open picker keeps a cursor on a pool
    /// or on "No pool", and closes when no pool is left. Returns nothing.
    pub fn set_pool_choices(&mut self, choices: Vec<PoolChoice>) {
        self.pool_choices = choices;
        if let Some(cursor) = self.pool_picker {
            self.pool_picker = if self.pool_choices.is_empty() {
                None
            } else {
                Some(cursor.min(self.pool_choices.len()))
            };
        }
    }

    fn settings_key(&mut self, key: KeyEvent) -> ProjectReply {
        match key.code {
            KeyCode::Enter => self.pool_reply(),
            KeyCode::Char('o') => self.setup.as_ref().map_or(ProjectReply::Handled, |s| {
                ProjectReply::Open(OpenRequest::Folder(
                    s.project_folder().display().to_string(),
                ))
            }),
            KeyCode::Char('g') => {
                match self.setup.as_ref() {
                    Some(setup) => self.general = Some(GeneralEdit::of(setup)),
                    None if self.setup_missing => {
                        self.problem = Some(plain_problem("project_setup_missing"));
                    }
                    None => self.problem = Some("Still loading the project. Try again.".into()),
                }
                ProjectReply::Handled
            }
            KeyCode::Char('x') => {
                self.confirm_remove = true;
                ProjectReply::Handled
            }
            KeyCode::Char('f') => {
                match &self.read_paths {
                    Some(paths) => {
                        let mut input = TextInput::single(FOLDERS_BYTES_MAX);
                        input.set(&paths.join(":"));
                        self.folders = Some(AddFile {
                            input,
                            busy: false,
                            error: None,
                        });
                    }
                    None => {
                        self.problem =
                            Some("Still loading the read-only folders. Try again.".into());
                    }
                }
                ProjectReply::Handled
            }
            KeyCode::Char('i') => ProjectReply::Process,
            KeyCode::Char('u') => match &self.schedule {
                Some(schedule) => ProjectReply::AccountOrder(
                    crate::tui::screens::settings::account_choice::cycle(
                        self.own_order(schedule),
                        true,
                        true,
                    ),
                ),
                None => {
                    self.problem = Some("Still loading the account order. Try again.".into());
                    ProjectReply::Handled
                }
            },
            _ => ProjectReply::Ignored,
        }
    }

    fn own_order(
        &self,
        schedule: &crate::scheduling::Schedule,
    ) -> Option<crate::scheduling::Strategy> {
        schedule
            .projects
            .iter()
            .find(|entry| entry.id.as_str() == self.project.as_str())
            .map(|entry| entry.strategy)
    }

    /// The project's account order for its Settings tab: its own strategy, or the one it
    /// follows from its pool or the app. Takes the screen. Returns the text, or none while
    /// the schedule loads.
    pub fn account_order(&self) -> Option<String> {
        let schedule = self.schedule.as_ref()?;
        if let Some(own) = self.own_order(schedule) {
            return Some(format!("{} (this project)", own.label()));
        }
        let pools: Vec<&CredentialRef> = self
            .pool
            .iter()
            .filter(|pool| pool.assigned)
            .map(|pool| &pool.id)
            .collect();
        Some(format!(
            "Default ({})",
            schedule.strategy_for(None, &pools).label()
        ))
    }

    /// Handles one key in the read-only folders box: Enter saves the folders typed between
    /// colons, an empty box clears them, Esc closes it.
    /// Takes the key event.
    /// Returns the change to send, or what the box did with the key.
    fn folders_key(&mut self, key: KeyEvent) -> ProjectReply {
        let Some(folders) = self.folders.as_mut() else {
            return ProjectReply::Ignored;
        };
        match key.code {
            KeyCode::Esc => {
                self.folders = None;
                ProjectReply::Handled
            }
            _ if folders.busy => ProjectReply::Handled,
            KeyCode::Enter => {
                let paths: Vec<String> = folders
                    .input
                    .value()
                    .split(':')
                    .map(str::trim)
                    .filter(|path| !path.is_empty())
                    .map(str::to_owned)
                    .collect();
                let action = SetupAction::SetReadPaths {
                    project_id: self.project.clone(),
                    paths,
                };
                if let Err(error) = action.validate() {
                    folders.error = Some(plain_problem(&error.0));
                    return ProjectReply::Handled;
                }
                folders.busy = true;
                folders.error = None;
                ProjectReply::Change(Box::new(action))
            }
            _ => {
                if folders.input.handle(key) {
                    folders.error = None;
                }
                ProjectReply::Handled
            }
        }
    }

    fn add_key(&mut self, key: KeyEvent) -> ProjectReply {
        let Some(adding) = self.adding.as_mut() else {
            return ProjectReply::Ignored;
        };
        match key.code {
            KeyCode::Esc => {
                self.adding = None;
                ProjectReply::Handled
            }
            _ if adding.busy => ProjectReply::Handled,
            KeyCode::Enter => {
                let typed = adding.input.value().trim();
                if typed.is_empty() {
                    adding.error = Some("Type the full path of a file on this computer.".into());
                    return ProjectReply::Handled;
                }
                let path = match (typed.strip_prefix("~/"), &self.home) {
                    (Some(rest), Some(home)) => home.join(rest).display().to_string(),
                    _ => typed.to_owned(),
                };
                let action = SetupAction::AddDocuments {
                    project_id: self.project.clone(),
                    documents: vec![path],
                };
                if let Err(error) = action.validate() {
                    adding.error = Some(plain_problem(&error.0));
                    return ProjectReply::Handled;
                }
                adding.busy = true;
                adding.error = None;
                ProjectReply::Change(Box::new(action))
            }
            _ => {
                if adding.input.handle(key) {
                    adding.error = None;
                }
                ProjectReply::Handled
            }
        }
    }

    fn general_key(&mut self, key: KeyEvent) -> ProjectReply {
        let Some(general) = self.general.as_mut() else {
            return ProjectReply::Ignored;
        };
        let last = GENERAL_FIELDS - 1;
        match key.code {
            KeyCode::Esc => {
                self.general = None;
                return ProjectReply::Handled;
            }
            _ if general.busy => return ProjectReply::Handled,
            KeyCode::Tab | KeyCode::Down => general.cursor = (general.cursor + 1).min(last),
            KeyCode::BackTab | KeyCode::Up => general.cursor = general.cursor.saturating_sub(1),
            KeyCode::Char(' ') if !general.on_text() => general.toggle(),
            KeyCode::Enter => return self.save_general(),
            _ => {
                if let Some(field) = general.text() {
                    field.handle(key);
                }
            }
        }
        if let Some(general) = self.general.as_mut() {
            general.error = None;
        }
        ProjectReply::Handled
    }

    fn save_general(&mut self) -> ProjectReply {
        let action = match (&self.general, &self.setup) {
            (Some(general), Some(setup)) => general.action(setup),
            _ => None,
        };
        let Some(action) = action else {
            self.general = None;
            return ProjectReply::Handled;
        };
        let Some(general) = self.general.as_mut() else {
            return ProjectReply::Handled;
        };
        if let Err(error) = action.validate() {
            general.error = Some(plain_problem(&error.0));
            return ProjectReply::Handled;
        }
        general.busy = true;
        general.error = None;
        ProjectReply::Change(Box::new(action))
    }

    fn assert_consistent(&self) {
        if let Some(cursor) = self.pool_picker {
            assert!(!self.pool_choices.is_empty() && cursor <= self.pool_choices.len());
        }
        if let Some(jobs) = &self.jobs {
            assert!(self.agents_cursor <= jobs.len());
        }
        if let Some(context) = &self.context {
            assert!(self.tree_cursor <= context.items.len());
        }
        assert!(!self.reading || self.preview.is_some() || self.tab != Tab::Context);
        assert!(self.confirm_stop.is_none() || self.confirm_delete.is_none());
        let boxes = [
            self.edit.is_some(),
            self.review.is_some(),
            self.adding.is_some(),
            self.folders.is_some(),
            self.general.is_some(),
            self.confirm_remove,
        ];
        assert!(boxes.iter().filter(|open| **open).count() <= 1);
        if let Some(review) = &self.review {
            assert!(review.scroll < review.lines().len().max(1));
        }
        if let Some(general) = &self.general {
            assert!(general.cursor < GENERAL_FIELDS);
        }
        if self.adding.is_some() {
            assert_eq!(self.tab, Tab::Context);
        }
        if self.general.is_some() || self.confirm_remove || self.folders.is_some() {
            assert_eq!(self.tab, Tab::Settings);
        }
        if let Some(job) = self.confirm_stop {
            assert_eq!(self.tab, Tab::Agents);
            assert!(job > 0);
        }
        if let Some(log) = &self.log {
            assert!(log.job > 0);
            assert!(
                log.lines
                    .as_ref()
                    .is_none_or(|lines| lines.len() <= RESEARCH_LOG_LINES_MAX)
            );
        }
        if let Some(preview) = &self.preview {
            assert!(preview.path.starts_with(CONTEXT_DIRECTORY));
        }
    }
}

/// Finds which research report suggested a memory.
/// Takes a suggested memory; the engine names research suggestions by a digest of project, research and text.
/// Returns the research kind, or none for a memory from anywhere else.
pub(in crate::tui) fn memory_source(memory: &KnowledgeEntry) -> Option<ResearchKind> {
    let digest = memory.id.as_str().strip_prefix("research-")?;
    ResearchKind::ALL.into_iter().find(|kind| {
        let expected = crate::memory::hash(
            format!(
                "{}\n{}\n{}",
                memory.project_id.as_str(),
                kind.name(),
                memory.text
            )
            .as_bytes(),
        );
        expected.get(..32) == Some(digest)
    })
}

/// Finds the research kind of a report path.
/// Takes a context path such as `context/research/market_research.md`.
/// Returns the kind, or none for other files.
pub(in crate::tui) fn research_kind(path: &str) -> Option<ResearchKind> {
    let name = path
        .strip_prefix(CONTEXT_DIRECTORY)?
        .strip_prefix("/research/")?
        .strip_suffix(".md")?;
    ResearchKind::parse(name)
}

/// Turns an engine rejection code into a plain sentence.
/// Takes the code.
/// Returns what happened and what to do; a code with no sentence is named after a retry sentence.
pub(in crate::tui) fn plain_problem(code: &str) -> String {
    match code {
        "research_report_missing" => {
            "This report isn't written yet. Try again when the research is done."
        }
        "research_section_missing" => "That part of the report is gone. Open the report again.",
        "memory_entry_exists" => "This report is already saved to memory.",
        "memory_text_too_long" => "This report is too long to save to memory as one entry.",
        "memory_text_hidden" => {
            "This part has hidden or direction-changing characters, so it can't be saved to memory."
        }
        "context_item_protected" => "The project brief can't be deleted.",
        "read_path_not_absolute" => "Type the full path of each folder, starting with /.",
        "read_path_missing" => "One of those folders doesn't exist on this computer.",
        "read_path_not_folder" => "One of those paths is a file. Add folders only.",
        "read_path_holds_home" => {
            "Agents can't read your whole home folder. Add a folder inside it."
        }
        "read_path_credentials" => {
            "One of those folders holds or sits in a credential location, such as .ssh or .aws."
        }
        "read_path_engine_data" => "One of those folders holds CrazyAgent's own data.",
        "read_path_refused" => "One of those folders can't be shared with agents.",
        "read_paths_too_many" | "read_paths_invalid" => {
            "Share at most 8 folders, each one plain path of up to 1024 characters."
        }
        "context_item_missing" => "That file is already gone.",
        "project_folder_missing" => "The project folder is missing. Check it in Finder.",
        "project_folder_unwritable" => {
            "CrazyAgent can't write in the project folder. Check its permissions."
        }
        "project_folder_unavailable" => "Documents can't be added on this system yet.",
        "project_setup_missing" => {
            "This project was made before project setup. Its context is not available."
        }
        "trusted_connection_unavailable" => "Not connected to the engine. Retrying.",
        "pool_project_limit" => "That pool already serves 64 projects. Pick another pool.",
        "pool_not_found" | "pool_missing" => "That pool no longer exists. Pick another pool.",
        "pool_changed" => "The pools changed meanwhile. Try again.",
        "model_access_read_only" => "Pools can't be changed in this setup.",
        "research_job_finished" => "This research already ended.",
        "research_job_not_retryable" => "Only failed or stopped research can run again.",
        "research_job_missing" => "That research is gone. The list is up to date now.",
        "research_log_unreadable" => "The agent's output could not be read.",
        "documents_empty" => "Type the full path of a file on this computer.",
        "documents_too_many" => "Add at most 16 documents at once.",
        "document_path_invalid" => "Use the full path of a file, starting with / or ~/.",
        "document_not_found" => "There is no file at that path. Check it and try again.",
        "document_not_a_regular_file" => "Add a file, not a folder or a shortcut.",
        "document_unreadable" => "That file can't be read. Check its permissions.",
        "document_changed_while_copying" => "The file changed while it was copied. Try again.",
        "document_name_duplicate" => "The project already has a document with that name.",
        "document_name_invalid" => "That file name can't be used. Rename the file first.",
        "document_too_large" => "Files can be up to 50 MB.",
        "document_type_not_allowed" => "This file type isn't supported.",
        "project_documents_too_large" => {
            "The project's documents would be over 200 MB. Remove some first."
        }
        "project_spec_bounds" => "Enter a project name of 1 to 256 characters.",
        "project_setup_bounds" => "Some answers are too long or repeated. Check each one.",
        "team_harness_unavailable" => {
            "A harness in this team can't run team agents yet. Pick another harness."
        }
        "project_about_invalid" => {
            "The description is too long or has characters that can't be saved."
        }
        "tech_stack_too_long" => "List at most 32 tech stack items.",
        "tech_stack_entry_invalid" => {
            "Each tech stack item must be unique and at most 64 characters."
        }
        "platforms_duplicate" => "Each platform can be picked once.",
        "research_duplicate" => "Each kind of research can be picked once.",
        "stale_revision" | "project_setup_revision" => {
            "This project changed since you opened the form. Open it again."
        }
        "invalid_project" => "This project no longer exists.",
        "change_outcome_unknown" => "We couldn't confirm the change. Check it and try again.",
        _ => return coded("That didn't work. Try again.", code),
    }
    .into()
}
