use super::new_project::PoolChoice;
use super::new_project_input::TextInput;
use super::project_markdown::{MdLine, render};
use crate::memory::{KnowledgeEntry, KnowledgeStatus, ReviewDecision};
use crate::pools::PoolSummary;
use crate::project_setup::{
    CONTEXT_DIRECTORY, ContextItem, ContextKind, ProjectContext, ResearchJob, ResearchKind,
    ResearchStatus, SetupAction, SetupQuery, SetupView, StoredSetup,
};
use crate::protocol::{CredentialRef, ProjectId, RequestId};
use crate::tui::seam::PoolScope;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::path::PathBuf;

pub(in crate::tui) const REFRESH_MS: i64 = 3_000;
const PREVIEW_BYTES_MAX: usize = 1024 * 1024;
const MEMORY_BYTES_MAX: usize = 4096;
const MESSAGE_BYTES_MAX: usize = 8192;

/// Names the five tabs of a project.
/// Each tab has a label and a section glyph for the tab strip.
/// The order is the tab order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::tui) enum Tab {
    Overview,
    Conversation,
    Agents,
    Context,
    Settings,
}

impl Tab {
    pub const ALL: [Tab; 5] = [
        Self::Overview,
        Self::Conversation,
        Self::Agents,
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
/// Holds the master harness name, its account, whether tools are on, the status and the messages.
/// The host fills it from the engine's master session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::tui) struct Transcript {
    pub harness: String,
    pub account: Option<String>,
    pub tools: bool,
    pub status: MasterStatus,
    pub messages: Vec<Message>,
}

/// A loaded context file for the reader.
/// Holds the path inside the project folder, the text read so far and where the next chunk starts.
/// Rendered lines are cached per width.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(in crate::tui) struct Preview {
    pub path: String,
    pub text: String,
    pub binary: bool,
    pub next: Option<u64>,
    pub scroll: usize,
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
    Open(String),
    PickPool,
    Assign(crate::protocol::CredentialRef),
    Send(String),
    StopTurn,
    Retry,
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
    pub confirm_delete: Option<String>,
    pub problem: Option<String>,
    pub now_ms: i64,
    pub offset: i64,
    jobs_read_ms: Option<i64>,
    context_read_ms: Option<i64>,
    memories_read_ms: Option<i64>,
    waiting: Option<SetupQuery>,
}

impl ProjectScreen {
    /// Opens a project on its Overview tab.
    /// Takes the project, its name from the engine snapshot and the owner's home folder.
    /// Returns the screen; engine reads start with `next_query`.
    pub fn new(project: ProjectId, name: String, home: Option<PathBuf>) -> Self {
        Self {
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
            confirm_delete: None,
            problem: None,
            now_ms: 0,
            offset: 0,
            jobs_read_ms: None,
            context_read_ms: None,
            memories_read_ms: None,
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
        self.typing = tab == Tab::Conversation;
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
        let query = if self.setup.is_none() && !self.setup_missing {
            SetupQuery::Setup { project }
        } else if self.setup_missing {
            return None;
        } else if due(self.jobs_read_ms) && self.jobs_live() {
            SetupQuery::Jobs { project }
        } else if self.context.is_none()
            || (matches!(self.tab, Tab::Context) && due(self.context_read_ms))
        {
            SetupQuery::Context { project }
        } else if self.memories.is_none()
            || (matches!(self.tab, Tab::Overview) && due(self.memories_read_ms))
        {
            SetupQuery::Memories {
                project,
                after: None,
            }
        } else if let Some(preview) = self
            .preview
            .as_ref()
            .filter(|p| p.text.is_empty() && !p.binary)
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
                if let SetupQuery::Jobs { .. } = query {
                    self.jobs_read_ms = Some(self.now_ms);
                }
                self.problem = Some(plain_problem(&problem));
                return;
            }
        };
        match view {
            SetupView::Setup(Some(setup)) => {
                self.name = setup.name.clone();
                self.setup = Some(*setup);
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
            SetupView::Item(chunk) => {
                if let Some(preview) = self.preview.as_mut().filter(|p| p.path == chunk.path) {
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
            SetupView::Changed(result) => self.changed(query, result),
            SetupView::Catalog(_) | SetupView::Team(_) | SetupView::Library(_) => {}
        }
        self.assert_consistent();
    }

    fn changed(&mut self, query: &SetupQuery, result: crate::engine::owner::OwnerResult) {
        use crate::engine::owner::OwnerResult;
        let applied = matches!(result, OwnerResult::Applied);
        if let OwnerResult::Rejected { code } = &result {
            self.problem = Some(plain_problem(code));
        }
        match query {
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

    /// Gives the absolute path of a context file.
    /// Takes the path inside the project folder.
    /// Returns the path, or none before the folder is known.
    pub fn absolute(&self, path: &str) -> Option<String> {
        let folder = self
            .setup
            .as_ref()
            .map(|setup| setup.folder.clone())
            .or_else(|| self.context.as_ref().map(|context| context.folder.clone()))?;
        Some(format!("{}/{path}", folder.trim_end_matches('/')))
    }

    /// Handles one key for the open tab, the edit box or the delete question.
    /// Takes the key event.
    /// Returns what the host should do next.
    pub fn key(&mut self, key: KeyEvent) -> ProjectReply {
        self.problem = None;
        if self.edit.is_some() {
            return self.edit_key(key);
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
        if self.tab == Tab::Conversation && self.typing {
            self.input.insert_str(text);
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
        let decision = match code {
            KeyCode::Char('y') => ReviewDecision::Keep,
            KeyCode::Char('n') => ReviewDecision::Reject,
            _ => {
                let mut input = TextInput::multi(MEMORY_BYTES_MAX);
                input.set(&super::project_markdown::sanitize(&text));
                self.edit = Some(MemoryEdit {
                    entry,
                    version,
                    input,
                });
                return ProjectReply::Handled;
            }
        };
        ProjectReply::Review {
            entry,
            version,
            decision,
        }
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
                    ProjectReply::Open(s.folder.clone())
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
        if self.preview.as_ref().is_none_or(|p| p.path != path) {
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
                if let Some(path) = item.and_then(|item| self.absolute(&item.path)) {
                    return ProjectReply::Open(path);
                }
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
            (Some(pool), Some(id)) if !pool.assigned => ProjectReply::Assign(id),
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

    fn picker_key(&mut self, key: KeyEvent, cursor: usize) -> ProjectReply {
        assert!(cursor < self.pool_choices.len().max(1));
        let last = self.pool_choices.len().saturating_sub(1);
        match key.code {
            KeyCode::Up => self.pool_picker = Some(cursor.saturating_sub(1)),
            KeyCode::Down => self.pool_picker = Some((cursor + 1).min(last)),
            KeyCode::Esc => self.pool_picker = None,
            KeyCode::Enter => {
                self.pool_picker = None;
                if let Some(choice) = self.pool_choices.get(cursor) {
                    return ProjectReply::Assign(choice.id.clone());
                }
            }
            _ => {}
        }
        ProjectReply::Handled
    }

    /// Replaces the pools the picker lists.
    /// Takes the choices in the engine's order; an open picker keeps a cursor inside the list.
    /// Returns nothing.
    pub fn set_pool_choices(&mut self, choices: Vec<PoolChoice>) {
        self.pool_choices = choices;
        if let Some(cursor) = self.pool_picker {
            self.pool_picker = if self.pool_choices.is_empty() {
                None
            } else {
                Some(cursor.min(self.pool_choices.len() - 1))
            };
        }
    }

    fn settings_key(&mut self, key: KeyEvent) -> ProjectReply {
        match key.code {
            KeyCode::Enter => self.pool_reply(),
            KeyCode::Char('o') => self.setup.as_ref().map_or(ProjectReply::Handled, |s| {
                ProjectReply::Open(s.folder.clone())
            }),
            _ => ProjectReply::Ignored,
        }
    }

    fn assert_consistent(&self) {
        if let Some(cursor) = self.pool_picker {
            assert!(cursor < self.pool_choices.len());
        }
        if let Some(jobs) = &self.jobs {
            assert!(self.agents_cursor <= jobs.len());
        }
        if let Some(context) = &self.context {
            assert!(self.tree_cursor <= context.items.len());
        }
        assert!(!self.reading || self.preview.is_some() || self.tab != Tab::Context);
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
/// Returns what happened and what to do.
pub(in crate::tui) fn plain_problem(code: &str) -> String {
    match code {
        "research_report_missing" => {
            "This report isn't written yet. Try again when the research is done."
        }
        "research_section_missing" => "That part of the report is gone. Open the report again.",
        "memory_entry_exists" => "This report is already saved to memory.",
        "memory_text_too_long" => "This report is too long to save to memory as one entry.",
        "context_item_protected" => "The project brief can't be deleted.",
        "context_item_missing" => "That file is already gone.",
        "project_folder_missing" => "The project folder is missing. Check it in Finder.",
        "project_setup_missing" => {
            "This project was made before project setup. Its context is not available."
        }
        "trusted_connection_unavailable" => "Not connected to the engine. Retrying.",
        "pool_project_limit" => "That pool already serves 64 projects. Pick another pool.",
        "pool_not_found" => "That pool no longer exists. Pick another pool.",
        "pool_changed" => "The pools changed meanwhile. Try again.",
        "model_access_read_only" => "Pools can't be changed in this setup.",
        _ => "That didn't work. Try again.",
    }
    .into()
}
