use super::changes::ProjectChanges;
use super::console;
use super::new_project::PoolChoice;
use super::new_project_input::TextInput;
use super::new_project_view::wrap_words;
use super::planning_line::{Planning, StatusLine, planning, planning_key, refreshes_status};
use super::project_access::{AccessEdit, AccessKey};
use super::project_markdown::{MdLine, render, sanitize};
use super::project_plugins::ProjectPlugins;
use super::questions::ProjectQuestions;
use super::recovery_banner::ProjectRecovery;
use super::sessions::{Sessions, SessionsAction, SessionsReply};
use crate::ide::bridge::reveal::RevealTarget;
use crate::memory::{KnowledgeEntry, KnowledgeStatus, ReviewDecision};
use crate::pools::PoolSummary;
use crate::project_setup::{
    ABOUT_BYTES_MAX, CONTEXT_DIRECTORY, ContextItem, ContextKind, DOCUMENT_PATH_BYTES_MAX,
    Platform, ProjectContext, ProjectSetup, RESEARCH_LOG_LINES_MAX, ResearchJob, ResearchKind,
    ResearchStatus, SetupAction, SetupQuery, SetupView, StoredSetup,
};
use crate::protocol::{CredentialRef, ProjectId, RequestId};
use crate::team::TeamGraph;
use crate::tui::ctx::Load;
use crate::tui::seam::PoolScope;
use crate::tui::text::coded;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::path::{Path, PathBuf};

mod state;
mod tabs;

pub(in crate::tui) const REFRESH_MS: i64 = 3_000;
pub(in crate::tui) const ACTIVITY_SHOWN: usize = 5;
pub(in crate::tui) const ACTIVITY_MAX: usize = 20;
const _: () = assert!(ACTIVITY_SHOWN <= ACTIVITY_MAX);
const PREVIEW_BYTES_MAX: usize = 1024 * 1024;
const MEMORY_BYTES_MAX: usize = 4096;
pub(in crate::tui) const REVIEW_WIDTH: usize = 54;
pub(in crate::tui) const REVIEW_ROWS: usize = 12;
const MESSAGE_BYTES_MAX: usize = 8192;
const NAME_BYTES_MAX: usize = 256;
const FOLDERS_BYTES_MAX: usize = crate::sandbox::extra_paths::EXTRA_PATHS_MAX * 1025;
const STACK_BYTES_MAX: usize = 2048;
pub(in crate::tui) const GENERAL_FIELDS: usize = 3 + Platform::ALL.len() + ResearchKind::ALL.len();
const STALE_READING: &str = "This project changed since you opened the form. Your edit is kept while the saved one is read.";
const KEEP_MINE: &str = "Enter saves your edit over it; Esc keeps the saved one.";
/// The Overview tabs the project view lists, in the desktop's order: the four an agent
/// writes, in `project_overview::Tab::ALL` order, then the README.
pub(in crate::tui) const OVERVIEW_TAB_LABELS: [&str; 5] = [
    "Quick info",
    "Features",
    "Competitors",
    "Install & use",
    "README",
];
/// How many lines PgUp and PgDn move the picked Overview tab's text.
const OVERVIEW_PAGE_ROWS: usize = 10;
/// The most lines the picked Overview tab's text can be scrolled down; the view stops at
/// the text's last line.
pub(in crate::tui) const OVERVIEW_SCROLL_MAX: usize = 1 << 16;

/// Names the seven tabs of a project.
/// Each tab has a label and a section glyph for the tab strip.
/// The order is the tab order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::tui) enum Tab {
    Overview,
    Conversation,
    Agents,
    Changes,
    Questions,
    Context,
    Settings,
}

impl Tab {
    pub const ALL: [Tab; 7] = [
        Self::Overview,
        Self::Conversation,
        Self::Agents,
        Self::Changes,
        Self::Questions,
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
            Self::Questions => "Questions",
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
            Self::Questions => "\u{225f}",
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
/// You is the owner, Master the leading harness, Notice a plain status from CrazyAgent and
/// Task the line of a task a reply handed to the team.
/// Notices that are errors carry the error flag on the message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::tui) enum Speaker {
    You,
    Master,
    Notice,
    Task,
}

/// How the remove ask removes the project: Remove from dashboard keeps every file, and
/// Delete everything also moves its folders to the Trash (DEC-118).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RemoveChoice {
    Dashboard,
    Everything,
}

/// What the prompt's inline menu offers at the cursor: the commands matching a `/` word, or
/// the tags completing an `@` word with how many characters that word has, `@` included.
#[derive(Debug, PartialEq, Eq)]
pub(in crate::tui) enum Picks {
    None,
    Commands(Vec<&'static console::Command>),
    Tags { typed: usize, tags: Vec<String> },
}

impl Picks {
    /// Counts the menu's entries.
    /// Takes the menu.
    /// Returns how many entries it shows.
    pub fn len(&self) -> usize {
        match self {
            Self::None => 0,
            Self::Commands(commands) => commands.len(),
            Self::Tags { tags, .. } => tags.len(),
        }
    }
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
/// NotReady is shown until the engine reports the master. Thinking carries the start time when
/// the engine knows it and the running turn's activity, oldest first, at most `ACTIVITY_MAX` lines.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::tui) enum MasterStatus {
    Ready,
    Thinking {
        since_ms: Option<i64>,
        activity: Vec<crate::master_session::MasterActivityLine>,
    },
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

    /// Counts the activity lines of a running turn.
    /// Takes the status.
    /// Returns the count, zero for every status but Thinking.
    pub fn activity_len(&self) -> usize {
        match self {
            Self::Thinking { activity, .. } => activity.len(),
            _ => 0,
        }
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
    /// The offset of the chunk this preview reads next: 0 before its first chunk, then the
    /// engine's next offset, so a chunk read for an earlier preview of the same file is
    /// told apart. Takes the preview. Returns the offset, or none once it is complete.
    fn wanted(&self) -> Option<u64> {
        if self.loaded { self.next } else { Some(0) }
    }

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
/// y saves and e edits once the last line is on screen; n dismisses, Esc goes back.
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
/// Holds the setup revision it started from, so a change made elsewhere is refused; after
/// such a refusal it keeps the draft, takes the revision of the setup read next and names the
/// fields changed since the owner last saw them.
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
    pub stale: bool,
    seen: GeneralSeen,
}

/// The General form's fields in the setup last shown to the owner: the one the form opened
/// at, or the one read after a stale refusal. Platforms and research are sorted, and research
/// leaves out roadmap planning, which the form has no switch for.
#[derive(Clone, Debug, PartialEq, Eq)]
struct GeneralSeen {
    name: String,
    about: String,
    tech_stack: Vec<String>,
    platforms: Vec<Platform>,
    research: Vec<ResearchKind>,
}

impl GeneralSeen {
    fn of(setup: &StoredSetup) -> Self {
        Self {
            name: setup.name.clone(),
            about: setup.about.clone(),
            tech_stack: setup.tech_stack.clone(),
            platforms: sorted(&setup.platforms),
            research: sorted(&setup.research)
                .into_iter()
                .filter(|kind| *kind != ResearchKind::RoadmapPlanning)
                .collect(),
        }
    }

    /// Names the fields that differ between two readings of the setup.
    /// Takes the earlier reading and the later one.
    /// Returns the field names as the form labels them, in form order.
    fn changed(&self, now: &Self) -> Vec<&'static str> {
        [
            ("name", self.name != now.name),
            ("description", self.about != now.about),
            ("tech stack", self.tech_stack != now.tech_stack),
            ("platforms", self.platforms != now.platforms),
            ("research", self.research != now.research),
        ]
        .into_iter()
        .filter_map(|(field, differs)| differs.then_some(field))
        .collect()
    }
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
            stale: false,
            seen: GeneralSeen::of(setup),
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

    /// Builds the engine edit from the form, with the stack read by [`stack_entries`]. The
    /// form has no roadmap switch, so the roadmap planning choice comes from the setup as it
    /// is now, not as the form opened. Takes the form and the current setup.
    /// Returns the edit with the name only when it changed, or none when nothing changed.
    pub fn action(&self, setup: &StoredSetup) -> Option<SetupAction> {
        let name = self.name.value().trim();
        let about = self.about.value().trim().to_owned();
        let tech_stack = stack_entries(self.stack.value(), &setup.tech_stack);
        let platforms: Vec<Platform> = Platform::ALL
            .into_iter()
            .filter(|platform| self.platforms.contains(platform))
            .collect();
        let research: Vec<ResearchKind> = ResearchKind::CHOICES
            .into_iter()
            .filter(|kind| {
                if *kind == ResearchKind::RoadmapPlanning {
                    setup.research.contains(kind)
                } else {
                    self.research.contains(kind)
                }
            })
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
            kept: None,
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

/// Reads the stack field: entries split by commas, where a saved entry that holds a comma
/// and still shows whole between separators stays one entry, so editing the field never
/// splits an entry the owner did not touch. Inputs: the field text and the saved entries.
/// Output: the trimmed entries without empty ones, or the saved entries themselves while
/// the field still shows them as the form wrote them.
fn stack_entries(text: &str, saved: &[String]) -> Vec<String> {
    if text == saved.join(", ") {
        return saved.to_vec();
    }
    let mut whole: Vec<&str> = saved
        .iter()
        .map(|entry| entry.trim())
        .filter(|entry| entry.contains(','))
        .collect();
    whole.sort_by_key(|entry| std::cmp::Reverse(entry.len()));
    let mut entries = Vec::new();
    let mut rest = text;
    loop {
        rest = rest.trim_start_matches(|c: char| c == ',' || c.is_whitespace());
        if rest.is_empty() {
            break;
        }
        let kept = whole.iter().copied().find(|entry| {
            rest.strip_prefix(*entry).is_some_and(|after| {
                let after = after.trim_start();
                after.is_empty() || after.starts_with(',')
            })
        });
        let (entry, after) = match kept {
            Some(entry) => (entry, &rest[entry.len()..]),
            None => rest.split_once(',').unwrap_or((rest, "")),
        };
        let entry = entry.trim();
        if !entry.is_empty() {
            entries.push(entry.to_owned());
        }
        rest = after;
    }
    entries
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
    Edit(OpenRequest),
    Reveal(RevealTarget),
    PickPool,
    Assign(Option<crate::protocol::CredentialRef>),
    Send(String),
    StopTurn,
    EndConversation,
    Retry,
    Team,
    TeamSettings,
    Master,
    Process,
    Memory,
    Usage,
    Session(SessionsAction),
    AccountOrder(Option<crate::scheduling::Strategy>),
    RefreshOverview,
    MoveHome(crate::projects_home::AddMode),
    DeleteEverything(String),
    GitHubRules,
    TeamChange(Box<crate::team::TeamAction>),
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
    pub team: Load<Box<TeamGraph>>,
    pub input: TextInput,
    pub typing: bool,
    pub history: console::History,
    pub menu_cursor: usize,
    pub console_note: Vec<String>,
    pub cleared: Option<(usize, Message)>,
    pub files: Option<(String, Vec<String>)>,
    pub transcript_scroll: usize,
    pub activity_open: bool,
    pub overview_cursor: usize,
    pub overview_tabs: Option<crate::project_overview::Overview>,
    pub overview_tab: usize,
    pub overview_scroll: usize,
    pub agents_cursor: usize,
    pub tree_cursor: usize,
    pub preview: Option<Preview>,
    pub reading: bool,
    pub edit: Option<MemoryEdit>,
    pub review: Option<MemoryReview>,
    pub confirm_delete: Option<String>,
    pub confirm_stop: Option<i64>,
    pub confirm_remove: bool,
    pub confirm_move: bool,
    pub removal: Option<crate::team::cleanup::RemovalPreview>,
    pub remove_choice: RemoveChoice,
    remove_everything: bool,
    pub remove_typed: String,
    pub confirm_plan: Option<SetupAction>,
    pub adding: Option<AddFile>,
    pub folders: Option<AddFile>,
    pub read_paths: Option<crate::project_setup::ReadPaths>,
    pub hidden: Option<AddFile>,
    pub hidden_paths: Option<crate::project_setup::HiddenPaths>,
    pub access: Option<AccessEdit>,
    pub agent_policy: Option<crate::sandbox::policy::PolicyView>,
    pub schedule: Option<crate::scheduling::Schedule>,
    pub general: Option<GeneralEdit>,
    pub removed: bool,
    pub log: Option<JobLog>,
    pub problem: Option<String>,
    pub plugins: ProjectPlugins,
    pub changes: ProjectChanges,
    pub questions: ProjectQuestions,
    pub recovery: ProjectRecovery,
    pub roadmap: StatusLine,
    pub github_notice: bool,
    pub sessions: Sessions,
    pub now_ms: i64,
    pub offset: i64,
    jobs_read_ms: Option<i64>,
    context_read_ms: Option<i64>,
    read_paths_read_ms: Option<i64>,
    folders_version: u64,
    folders_stale: bool,
    hidden_paths_read_ms: Option<i64>,
    hidden_version: u64,
    hidden_stale: bool,
    agent_policy_read_ms: Option<i64>,
    access_stale: bool,
    memories_read_ms: Option<i64>,
    overview_read_ms: Option<i64>,
    removal_read_ms: Option<i64>,
    setup_failed_ms: Option<i64>,
    pub setup_due: bool,
    waiting: Option<SetupQuery>,
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

/// What the Context tab says when the owner asks to delete a file linked from the repository.
pub(in crate::tui) const LINKED_KEPT: &str =
    "This file is linked from your repo. It can't be deleted here.";

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

/// Whether an Overview run is writing or waits to start: after a refresh, a landed change
/// set or while no account of the pool can run it. Takes the overview. Returns the answer;
/// tabs that are off or must move into the projects home never wait.
pub(in crate::tui) fn overview_pending(overview: &crate::project_overview::Overview) -> bool {
    let waits = overview.stale
        && overview.refresh != crate::roadmap_status::Refresh::Off
        && !overview.needs_home;
    overview.running || waits
}

/// The plain sentence for a hidden folder refused as not plain, in this system's path form:
/// a plain drive path on Windows, a path with no quote or backslash elsewhere.
const HIDDEN_NOT_PLAIN: &str = if cfg!(windows) {
    "A folder path must be a plain drive path such as C:\\Users\\you\\project\\secrets, with no quotes, / or control characters."
} else {
    "A folder path can't hold quotes, backslashes or control characters."
};

/// Turns an engine rejection code into a plain sentence.
/// Takes the code.
/// Returns what happened and what to do; a code with no sentence is named after a retry sentence.
pub(in crate::tui) fn plain_problem(code: &str) -> String {
    match code {
        "research_report_missing" => {
            "This report isn't written yet. Try again when the research is done."
        }
        "research_section_missing" => "That part of the report is gone. Open the report again.",
        "overview_off" => "Overview tabs are off for this project. Turn them on in Project settings.",
        "overview_needs_project_home" => {
            "Overview tabs need this project in the CrazyAgent folder. Move it there in the desktop app, in Project settings."
        }
        "overview_unavailable" => "The Overview tabs can't be read right now. Try again in a moment.",
        "overview_project_unknown" => "CrazyAgent doesn't know this project any more.",
        "memory_entry_exists" => "This report is already saved to memory.",
        "memory_text_too_long" => "This report is too long to save to memory as one entry.",
        "memory_text_hidden" => {
            "This part has hidden or direction-changing characters, so it can't be saved to memory."
        }
        "context_item_protected" => "The project brief can't be deleted.",
        "read_path_not_absolute" => "Type the full path of each folder, starting with /.",
        "team_no_hidden_seers" => "No running agent can still open a hidden folder.",
        "agent_access_unavailable" => "That access level can't run agents on this computer.",
        crate::setup::AGENT_GIT_NEEDS_TOOLS => crate::setup::plain_reason(code),
        "agent_policy_invalid" => {
            "That choice isn't valid: each toolchain once, at most 16 picked folders and 16 secret paths."
        }
        "agent_policy_unreadable" => "CrazyAgent couldn't read this project's agent access. Try again.",
        "agent_access_lowered" => {
            "This project's agent access was lowered below what the agent started with, so it was stopped. What it changed in its own folder is still in Changes; an agent that ran at Full access may also have changed files outside it."
        }
        "agent_network_narrowed" => {
            "This project's agent network was narrowed, so the agent was stopped. Its next start uses the new network."
        }
        "agent_toolchain_lowered" => {
            "A toolchain the agent started with is no longer allowed for this project, so the agent was stopped. Its next start goes without it."
        }
        "agent_project_unreadable" => {
            "CrazyAgent couldn't read the project folder to check the agent's toolchains, so the agent was stopped. Try again."
        }
        "shared_folders_unlisted" => {
            "CrazyAgent couldn't list the project folder's files with git, so the agent didn't start. Choose All local folders with Read-only in Agent access, or check the project's git."
        }
        "shared_folders_too_many" => {
            "The project folder has more than 256 untracked files and folders to keep out, or more than 32 folders to open for writing, so the agent didn't start. Choose All local folders in Agent access, or gather those files into fewer folders."
        }
        "shared_folders_refused" => {
            "The sandbox couldn't keep the project's secret files closed or open a shared folder, so the agent didn't start. Check Shared project folders in Agent access."
        }
        "shared_folders_need_macos" => {
            "On Linux and Windows only All local folders with Read-only can start agents. Change Shared project folders in Agent access."
        }
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
        "hidden_paths_too_many" => "Hide at most 8 folders.",
        "hidden_paths_invalid" | "hidden_path_not_absolute" => {
            "Each folder must be one plain path of up to 1024 characters."
        }
        "hidden_path_missing" => "That folder can't be found. Pick a folder that exists.",
        "hidden_path_not_folder" => "That path isn't a folder. Pick a folder.",
        "hidden_path_outside_project" => {
            "A hidden folder must sit inside the project. Pick a folder within it."
        }
        "hidden_path_not_plain" => HIDDEN_NOT_PLAIN,
        "hidden_path_tracked" => {
            "git tracks a file in that folder. Only folders git does not track can be hidden."
        }
        "hidden_path_unchecked" => "CrazyAgent couldn't check that folder against git. Try again.",
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
        "research_unavailable_here" => {
            "Research does not run on this computer in this version, so it cannot run again."
        }
        "planning_busy" => {
            "A roadmap is already being planned for this project. Wait for it to finish."
        }
        "setup_unconfirmed" => {
            "Confirm this project's setup first. Its setup review has the roadmap switch."
        }
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
        "stale_revision" => "This project changed since you opened the form. Open it again.",
        "invalid_project" => "This project no longer exists.",
        "change_outcome_unknown" => "We couldn't confirm the change. Check it and try again.",
        "move_project_busy" => {
            "Stop the team first (Stop all on the Team tab), then move the project. If its agents are still stopping, try again in a few seconds."
        }
        code if code.starts_with("move_project_")
            || code.starts_with("home_")
            || code.starts_with("delete_")
            || code == "projects_removed_full" =>
        {
            return crate::projects_home::plain_problem(code).map_or_else(
                || coded("That didn't work. Try again.", code),
                str::to_owned,
            );
        }
        _ => return coded("That didn't work. Try again.", code),
    }
    .into()
}
