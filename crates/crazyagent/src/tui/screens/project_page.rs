use super::new_project::harness_name;
use super::new_project_page::{hints_of, key_event, look, paint, pool_choices_from};
use super::new_project_view::{Row, Screen as View};
use super::project::{
    MasterStatus, OpenRequest, ProjectReply, ProjectScreen, Speaker, Tab, Transcript,
    plain_problem, project_pool,
};
use super::project_markdown::sanitize;
use super::project_master::{MasterRead, master_problem, transcript_from};
use super::project_plugins::PluginsReply;
use super::project_time::local_offset;
use super::project_view::project_screen;
use super::settings_accounts::send as send_settings;
use super::settings_plugins::{AddPlugin, EditServer, SkillDetailPage, ToolsPage};
use super::{Flow, Place, ProjectTab, Screen, SettingsSection};
use crate::engine::owner::{ImportFile, OwnerResponse, OwnerResult};
use crate::ide::bridge::reveal::RevealTarget;
use crate::master_session::{MasterBackend, MasterOwnerAction};
use crate::project_setup::{ProjectSetup, SetupAction, SetupQuery, SetupView};
use crate::protocol::{CredentialRef, ProjectId};
use crate::sandbox::policy::{AgentAccess, AgentPolicy};
use crate::tui::app::UiEvent;
use crate::tui::client::editor;
use crate::tui::client::ide::RevealQuery;
use crate::tui::client::process::{ProcessEvent, ProcessQuery, ProcessReply};
use crate::tui::client::settings::{SettingsEvent, SettingsJob};
use crate::tui::client::{OwnerEvent, SetupEvent};
use crate::tui::clock::now_ms;
use crate::tui::ctx::{Ctx, Link, Load};
use crate::tui::frame::{Hint, Message, Modal, draw_lines, hint};
use crate::tui::keys::Key;
use crate::tui::master_session::{
    Context as MasterContext, Event as MasterEvent, Job as MasterJob, NEW_SESSION_STARTING,
    RENEWAL_NOT_STARTED, State as MasterState, Submission as MasterSubmission,
};
use crate::tui::seam::{Arrival, NOT_READY, PoolChange, Request};
use crate::tui::text::{cut, styled, wrap};
use crate::tui::theme::Role;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::text::Line;
use std::path::{Path, PathBuf};

const SENT_MAX: usize = 8;
const MASTER_SENT_MAX: usize = 4;
const TEAM_READ_MS: i64 = 3_000;
const TEAM_WAIT_MS: i64 = 15_000;
const CHANGED_MISSING: &str =
    "That file isn't in your project folder yet. It lands there when you accept the change.";
const CHANGED_NOTE: &str =
    "That was your copy in the project folder. The agent's version lands there when you accept.";
const IMPORT_MODAL_WIDTH: u16 = 72;
const IMPORT_ROWS_MAX: usize = 12;
pub(in crate::tui) const BANNER_ROWS_MAX: usize = 3;
const BANNER_BRANCH_CELLS: usize = 32;

#[derive(PartialEq)]
enum MasterSent {
    Catalog(MasterContext),
    Read(MasterContext),
    Mutation(MasterSubmission),
}

impl MasterSent {
    fn of_job(job: &MasterJob) -> Self {
        match job {
            MasterJob::Catalog { context } => Self::Catalog(context.clone()),
            MasterJob::Read { context, .. } => Self::Read(context.clone()),
            MasterJob::Mutation { submission, .. } => Self::Mutation(submission.clone()),
        }
    }

    fn of_event(event: &MasterEvent) -> Self {
        match event {
            MasterEvent::Catalog { context, .. } => Self::Catalog(context.clone()),
            MasterEvent::Read { context, .. } => Self::Read(context.clone()),
            MasterEvent::Mutation { submission, .. } => Self::Mutation(submission.clone()),
        }
    }
}

struct Waiting {
    text: String,
    opening: bool,
}

fn rejection(event: &MasterEvent) -> Option<String> {
    let MasterEvent::Mutation {
        result: Ok(response),
        ..
    } = event
    else {
        return None;
    };
    match response.as_ref() {
        OwnerResponse::Rejected { code } => Some(code.clone()),
        OwnerResponse::Applied { operation } | OwnerResponse::Committed { operation } => {
            match &operation.result {
                OwnerResult::Rejected { code } => Some(code.clone()),
                _ => None,
            }
        }
        _ => None,
    }
}

fn not_recorded(event: &MasterEvent) -> bool {
    matches!(
        event,
        MasterEvent::Mutation { result: Ok(response), .. }
            if matches!(response.as_ref(), OwnerResponse::NotRecordedAtBarrier { .. })
    )
}

fn submitted_text(event: &MasterEvent) -> Option<&str> {
    match event {
        MasterEvent::Mutation {
            submission:
                MasterSubmission {
                    action: MasterOwnerAction::Submit { text, .. },
                    ..
                },
            ..
        } => Some(text),
        _ => None,
    }
}

fn backend_name(backend: MasterBackend) -> String {
    match backend {
        MasterBackend::Rcli => "RCLI".to_owned(),
        MasterBackend::Codex => harness_name("codex", None),
        MasterBackend::Claude => harness_name("claude", None),
    }
}

fn tab_of(tab: ProjectTab) -> Tab {
    Tab::ALL[ProjectTab::ALL
        .iter()
        .position(|each| *each == tab)
        .expect("every tab is listed")]
}

fn project_tab(tab: Tab) -> ProjectTab {
    ProjectTab::ALL[Tab::ALL
        .iter()
        .position(|each| *each == tab)
        .expect("every tab is listed")]
}

/// What the TUI hands the system opener for one request.
/// `Reveal` shows a folder in the file manager; `Open` opens a file with its default app.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::tui) enum Opening {
    Reveal(PathBuf),
    Open(PathBuf),
}

/// Decides what the system opener gets for one open request, with the desktop's checks
/// (`platform::project_folder` and `platform::context_file`): a project folder is revealed,
/// never opened, so a folder named like an app bundle is not launched; a context file opens
/// only as a project document type, not a link, and a regular file inside the folder.
/// Takes the request. Returns the opening, or the plain problem.
pub(in crate::tui) fn opening(request: &OpenRequest) -> Result<Opening, String> {
    let missing = |_| "That file or folder is not there any more.".to_owned();
    match request {
        OpenRequest::Folder(folder) => crate::platform::project_folder(folder)
            .map(Opening::Reveal)
            .map_err(missing),
        OpenRequest::Context { folder, path } => {
            let folder = crate::platform::project_folder(folder).map_err(missing)?;
            crate::platform::context_file(&folder, path)
                .map(Opening::Open)
                .map_err(|_| "CrazyAgent opens only documents inside the project folder.".into())
        }
    }
}

/// Shows a project folder or opens one context file once [`opening`] allows it.
/// Takes the request. Returns the plain problem when it is refused or nothing opened it.
fn open_request(request: &OpenRequest) -> Result<(), String> {
    match opening(request)? {
        Opening::Reveal(folder) => crate::platform::reveal(&folder),
        Opening::Open(file) => crate::platform::open(file.as_os_str()),
    }
    .map_err(|_| "Could not open it on this computer.".to_owned())
}

/// Checks one changed file before it goes to the owner's editor: the owner's own copy
/// in the project folder, never an agent's worktree, any type, and never through a link.
/// Takes the project folder and the path the change set lists. Returns the file, or the
/// plain problem.
pub(in crate::tui) fn changed_file(folder: &Path, path: &str) -> Result<PathBuf, &'static str> {
    let folder = crate::platform::project_folder(&folder.to_string_lossy())
        .map_err(|_| "That file or folder is not there any more.")?;
    crate::platform::project_file(&folder, path).map_err(|code| match code {
        "project_file_missing" => CHANGED_MISSING,
        _ => "CrazyAgent opens only files inside the project folder.",
    })
}

/// One project as a shell screen.
/// Holds the project state, its last drawn view, the engine reads it sent and its master conversation.
/// Reads go one at a time through the shell queue; only replies to its own reads are applied.
pub(in crate::tui) struct ProjectPage {
    screen: ProjectScreen,
    view: View,
    sent: Vec<SetupQuery>,
    assigning: Option<(Option<CredentialRef>, Option<String>)>,
    recording: Option<u64>,
    access_wanted: Option<AgentAccess>,
    live_wanted: bool,
    created_access: Option<u64>,
    master: MasterState,
    master_sent: Vec<MasterSent>,
    waiting: Option<Waiting>,
    starting: Option<Message>,
    retried: bool,
    renewed: bool,
    schedule_asked: bool,
    team_asked: Option<(i64, bool)>,
    files_asked: Option<String>,
    covered: bool,
    from_master: bool,
    reopen_master: bool,
    master_entry: bool,
    import: Option<Vec<ImportFile>>,
    import_asked: bool,
    import_sent: Option<ProcessQuery>,
    moved: Option<Vec<ImportFile>>,
    uncommitted: Vec<String>,
    uncommitted_branch: Option<String>,
    uncommitted_asked: bool,
}

/// Says in a few words why the engine leaves one instruction file in place (DEC-106).
/// Input: the refusal code from the pending import list. Output: the reason, read after
/// "stays in place,".
fn import_reason(code: &str) -> &'static str {
    match code {
        "process_import_outside" => "it links outside the project folder",
        "process_import_unsafe" => "it isn't a plain file you own",
        "process_import_unreadable" => "it couldn't be read",
        "process_text_too_large" => "it is over 64 KB",
        "process_text_invalid" => "it has characters CrazyAgent can't keep",
        _ => "CrazyAgent can't import it",
    }
}

/// Words a whole refusal of the confirmed import, when nothing was moved (DEC-106).
/// Input: the refusal code. Output: the message for the status line.
fn import_problem(code: &str) -> String {
    match code {
        "process_text_too_large" => {
            "Together these files are over 64 KB, so nothing moved. Shorten one, then try again."
                .into()
        }
        other => super::process::message(other),
    }
}

/// The banner text shown while an instruction import's move stays uncommitted (DEC-106
/// addendum): the files and the checked-out branch merges into are refused, or "this
/// branch" without one; a name wider than `BANNER_BRANCH_CELLS` is cut with the ellipsis,
/// so "are refused" stays within the banner's rows. Inputs: the paths, the branch and the
/// ellipsis glyph. Output: the text, sanitized.
pub(in crate::tui) fn move_banner(
    files: &[String],
    branch: Option<&str>,
    ellipsis: &str,
) -> String {
    assert!(!files.is_empty());
    let files: Vec<String> = files.iter().map(|path| sanitize(path)).collect();
    let target = branch.map_or_else(
        || "this branch".to_owned(),
        |name| cut(&sanitize(name), BANNER_BRANCH_CELLS, ellipsis),
    );
    assert!(crate::tui::text::width(&target) <= BANNER_BRANCH_CELLS);
    format!(
        "Git still shows {} as deleted after the instruction import, so merges into {target} are refused. Ctrl-G Commit the move makes one local commit holding only that move; it is never pushed.",
        files.join(", ")
    )
}

impl ProjectPage {
    /// Opens a project on one tab.
    /// Takes the project id and name from Home, the tab and the shell context.
    /// Returns the screen; engine reads start when it is shown.
    pub fn new(project: ProjectId, name: String, tab: ProjectTab, ctx: &Ctx) -> Self {
        let mut screen = ProjectScreen::new(project, name, ctx.home.as_ref().map(PathBuf::from));
        screen.open(tab_of(tab));
        let now = now_ms();
        screen.now_ms = now;
        screen.offset = local_offset(now);
        let mut page = Self {
            screen,
            view: View::default(),
            sent: Vec::new(),
            assigning: None,
            recording: None,
            access_wanted: None,
            live_wanted: false,
            created_access: None,
            master: MasterState::default(),
            master_sent: Vec::new(),
            waiting: None,
            starting: None,
            retried: false,
            renewed: false,
            schedule_asked: false,
            team_asked: None,
            files_asked: None,
            covered: false,
            from_master: false,
            reopen_master: false,
            master_entry: tab == ProjectTab::Conversation,
            import: None,
            import_asked: false,
            import_sent: None,
            moved: None,
            uncommitted: Vec::new(),
            uncommitted_branch: None,
            uncommitted_asked: false,
        };
        page.refresh_pool(ctx);
        page.select_master(ctx);
        page.screen.transcript = page.transcript();
        page.redraw(ctx);
        page
    }

    /// Saves the agent access level the owner picked in New project, right after the project
    /// was made: it reads the project's policy first and saves at the version read, since a
    /// project made again under an earlier id keeps that id's policy version. Until the save
    /// lands the project stays Sandboxed, and a refusal says so.
    /// Takes the picked level and the context. Returns nothing; the replies arrive later.
    pub(in crate::tui) fn save_created_access(&mut self, access: AgentAccess, ctx: &mut Ctx) {
        assert_ne!(access, AgentAccess::Sandboxed);
        self.access_wanted = Some(access);
        self.send(
            SetupQuery::AgentPolicy {
                project: self.screen.project.clone(),
            },
            ctx,
        );
    }

    fn send_created_access(&mut self, access: AgentAccess, version: u64, ctx: &mut Ctx) {
        let generation = ctx.next_generation();
        self.created_access = Some(generation);
        self.send(
            SetupQuery::Change {
                action: SetupAction::SetAgentPolicy {
                    project_id: self.screen.project.clone(),
                    version,
                    policy: AgentPolicy {
                        access,
                        toolchains: Vec::new(),
                        ide_git_hooks: false,
                        network: crate::sandbox::policy::AgentNetwork::default(),
                    },
                },
                generation,
            },
            ctx,
        );
    }

    fn stays_sandboxed(why: &str, ctx: &mut Ctx) {
        ctx.say(Message::warning(
            "The project is made, but its agents stay Sandboxed.",
            &format!("{why} Choose Agent access again on the Settings tab with c."),
        ));
    }

    /// Asks the engine, once per opening, which instruction files wait to be copied into the
    /// project's instructions.md (DEC-106). Input: the context. Output: none; the list arrives
    /// as a process event and shows as a prompt when it isn't empty.
    fn ask_import(&mut self, ctx: &mut Ctx) {
        if self.import_asked || self.import_sent.is_some() {
            return;
        }
        let query = ProcessQuery::PendingImport {
            project: self.screen.project.clone(),
        };
        if ctx.try_request(Request::Process(Box::new(query.clone()))) {
            self.import_asked = true;
            self.import_sent = Some(query);
        }
    }

    /// Reads which moved instruction files still wait for a commit, for the banner shown
    /// while the move stays uncommitted (DEC-106 addendum). Input: the context. Output:
    /// none; the paths arrive as a process event.
    fn ask_uncommitted(&mut self, ctx: &mut Ctx) {
        if self.uncommitted_asked {
            return;
        }
        let query = ProcessQuery::Uncommitted {
            project: self.screen.project.clone(),
        };
        self.uncommitted_asked = ctx.try_request(Request::Process(Box::new(query)));
    }

    /// Asks the engine to commit the instruction import's move: one local commit on the
    /// checkout's current branch holding only the moved files, never pushed. Only the owner's
    /// key calls this. Input: the context. Output: none; the commit arrives as a process event.
    fn commit_move(&mut self, ctx: &mut Ctx) {
        if self.import_sent.is_some() {
            return;
        }
        let query = ProcessQuery::CommitImport {
            project: self.screen.project.clone(),
        };
        if ctx.try_request(Request::Process(Box::new(query.clone()))) {
            self.import_sent = Some(query);
        } else {
            ctx.say(Message::error(
                "The move wasn't committed: too many requests are waiting.",
                "",
            ));
        }
    }

    /// Applies the engine's answer to the pending-import read, the confirmed move, the
    /// uncommitted-move read or the commit. Inputs: the process event and the context.
    /// Output: none; a list shows the prompt, a move with uncommitted files offers Commit the
    /// move or Leave it, any other move closes it and names the moved files, a commit names
    /// its branch and files, and a refusal says why.
    fn import_arrived(&mut self, event: &ProcessEvent, ctx: &mut Ctx) {
        if let (ProcessQuery::Uncommitted { project }, result) = (&event.query, &event.result)
            && *project == self.screen.project
        {
            self.uncommitted_asked = false;
            if let Ok(ProcessReply::Uncommitted(waiting)) = result {
                self.uncommitted = waiting.files.clone();
                self.uncommitted_branch = waiting.branch.clone();
            }
            return;
        }
        if self.import_sent.as_ref() != Some(&event.query) {
            return;
        }
        self.import_sent = None;
        match (&event.query, &event.result) {
            (ProcessQuery::PendingImport { .. }, Ok(ProcessReply::Files(files))) => {
                self.import = (!files.is_empty()).then(|| files.clone());
            }
            (ProcessQuery::PendingImport { .. }, _) => self.import_asked = false,
            (ProcessQuery::ConfirmImport { .. }, Ok(ProcessReply::Files(moved))) => {
                self.import = None;
                if moved.iter().any(|file| file.uncommitted) {
                    self.moved = Some(moved.clone());
                } else {
                    let names: Vec<String> =
                        moved.iter().map(|file| sanitize(&file.path)).collect();
                    ctx.say(Message::success(
                        "Moved to .crazyagent/imported/:",
                        &names.join(", "),
                    ));
                }
                self.ask_uncommitted(ctx);
            }
            (ProcessQuery::ConfirmImport { .. }, Err(code)) => {
                ctx.say(Message::error(&import_problem(code), ""));
            }
            (ProcessQuery::CommitImport { .. }, Ok(ProcessReply::Committed(commit))) => {
                self.moved = None;
                let files: Vec<String> = commit.files.iter().map(|path| sanitize(path)).collect();
                ctx.say(Message::success(
                    &format!("Committed the move on {}:", sanitize(&commit.branch)),
                    &format!("{}. Nothing was pushed.", files.join(", ")),
                ));
                self.ask_uncommitted(ctx);
            }
            (ProcessQuery::CommitImport { .. }, Err(code)) => {
                if code == "process_commit_none" {
                    self.moved = None;
                }
                ctx.say(Message::error(&super::process::message(code), ""));
                self.ask_uncommitted(ctx);
            }
            _ => {}
        }
    }

    /// Handles a key while the moved files still wait for a commit after the confirm: Enter
    /// or y commits the move, Esc or n leaves it for the project page banner. Inputs: the key
    /// and the context. Output: handled, so no key reaches the page meanwhile.
    fn moved_key(&mut self, key: &Key, ctx: &mut Ctx) -> Flow {
        match key {
            Key::Enter | Key::Char('y') => self.commit_move(ctx),
            Key::Esc | Key::Char('n') if self.import_sent.is_none() => {
                self.moved = None;
                self.ask_uncommitted(ctx);
            }
            _ => {}
        }
        Flow::Handled
    }

    /// The banner rows shown above the tabs while an instruction import's move stays
    /// uncommitted: the files, the branch merges into are refused and the Ctrl-G action.
    /// Inputs: the width and the context. Output: the rows, none when no move waits.
    fn banner(&self, width: usize, ctx: &Ctx) -> Vec<Line<'static>> {
        if self.uncommitted.is_empty() || width < 8 {
            return Vec::new();
        }
        let text = move_banner(
            &self.uncommitted,
            self.uncommitted_branch.as_deref(),
            ctx.glyphs.ellipsis,
        );
        wrap(&text, width - 2)
            .into_iter()
            .take(BANNER_ROWS_MAX)
            .map(|line| styled(format!(" {line}"), Role::Warning, &ctx.theme))
            .collect()
    }

    /// Handles a key while the pending-import prompt shows: Enter or y copies the files into
    /// instructions.md and moves them aside, Esc or n closes it until the next opening.
    /// Inputs: the key and the context. Output: handled, so no key reaches the page meanwhile.
    fn import_key(&mut self, key: &Key, ctx: &mut Ctx) -> Flow {
        let movable = self
            .import
            .as_ref()
            .is_some_and(|files| files.iter().any(|file| file.refused.is_none()));
        match key {
            Key::Enter | Key::Char('y') if movable && self.import_sent.is_none() => {
                let query = ProcessQuery::ConfirmImport {
                    project: self.screen.project.clone(),
                };
                if ctx.try_request(Request::Process(Box::new(query.clone()))) {
                    self.import_sent = Some(query);
                } else {
                    ctx.say(Message::error(
                        "The files weren't moved: too many requests are waiting.",
                        "",
                    ));
                }
            }
            Key::Esc | Key::Char('n') if self.import_sent.is_none() => self.import = None,
            _ => {}
        }
        Flow::Handled
    }

    /// The step after a confirmed import whose moved files git still shows as deleted: the
    /// moved files, which of them wait for a commit, and Commit the move or Leave it.
    /// Inputs: the moved files and the context. Output: the modal and its keys.
    fn moved_modal(&self, moved: &[ImportFile], ctx: &Ctx) -> (Modal, Vec<Hint>) {
        let theme = &ctx.theme;
        let room = usize::from(IMPORT_MODAL_WIDTH) - 4;
        let mut body: Vec<Line<'static>> = wrap(
            "Their text is in this project's .crazyagent/instructions.md. Moved:",
            room,
        )
        .into_iter()
        .map(Line::raw)
        .collect();
        for file in moved.iter().take(IMPORT_ROWS_MAX) {
            let path = sanitize(&file.path);
            let text = if file.uncommitted {
                format!("- {path} (git shows it as deleted)")
            } else {
                format!("- {path}")
            };
            body.extend(wrap(&text, room).into_iter().map(Line::raw));
        }
        body.push(Line::default());
        body.extend(
            wrap(
                "Merges into this branch are refused until the move is committed. Commit the move makes one local commit on your current branch holding only that move; it is never pushed. Leave it keeps a banner on the project page.",
                room,
            )
            .into_iter()
            .map(|text| styled(text, Role::Warning, theme)),
        );
        let hints = if self.import_sent.is_none() {
            vec![hint("Enter", "Commit the move"), hint("Esc", "Leave it")]
        } else {
            Vec::new()
        };
        (
            Modal {
                width: IMPORT_MODAL_WIDTH,
                title: "Moved to .crazyagent/imported/".into(),
                body,
                top: None,
            },
            hints,
        )
    }

    /// Reads the team graph for the panel beside the master, while this page is on top on
    /// the Conversation tab: at most one read out at a time, at most one every
    /// `TEAM_READ_MS`, and a read lost for `TEAM_WAIT_MS` is asked again. Takes the context.
    fn ask_team(&mut self, ctx: &mut Ctx) {
        if self.covered || self.screen.tab != Tab::Conversation || ctx.link != Link::Ready {
            return;
        }
        let now = now_ms();
        if let Some((at, waiting)) = self.team_asked
            && now - at < if waiting { TEAM_WAIT_MS } else { TEAM_READ_MS }
        {
            return;
        }
        self.team_asked = Some((now, true));
        ctx.request(Request::TeamGraph {
            project: self.screen.project.clone(),
        });
    }

    /// Lists the project files for the `@` word the owner is typing: one read out at a time,
    /// and only when the word differs from the last listed one. A read the full request
    /// queue drops is not counted as out, so the next key asks again. Takes the context.
    fn ask_files(&mut self, ctx: &mut Ctx) {
        if self.covered || self.files_asked.is_some() || ctx.link != Link::Ready {
            return;
        }
        let Some(query) = self.screen.files_query() else {
            return;
        };
        if self
            .screen
            .files
            .as_ref()
            .is_some_and(|(listed, _)| *listed == query)
        {
            return;
        }
        let queued = ctx.queued();
        ctx.request(Request::ProjectFiles {
            project: self.screen.project.clone(),
            query: query.clone(),
        });
        if ctx.queued() > queued {
            self.files_asked = Some(query);
        }
    }

    fn select_master(&mut self, ctx: &Ctx) {
        let ready = ctx.link == Link::Ready && ctx.identity.is_some();
        self.master
            .select(
                ctx.identity.clone(),
                Some(self.screen.project.clone()),
                ready,
            )
            .expect("the master view generation never runs out");
    }

    fn sync_master(&mut self, ctx: &mut Ctx) {
        self.select_master(ctx);
        if let Some(reason) = self.master.take_renewal_problem() {
            ctx.say(Message::warning(RENEWAL_NOT_STARTED, &reason));
        }
        self.master
            .show_sessions(self.screen.tab == Tab::Conversation);
        if self.master_sent.len() < MASTER_SENT_MAX
            && let Some(job) = self.master.request(self.screen.tab == Tab::Conversation)
        {
            self.master_sent.push(MasterSent::of_job(&job));
            ctx.request(Request::Master(Box::new(job)));
        }
        self.screen.transcript = self.transcript();
        if let Some(listed) = self.master.sessions() {
            self.screen.sessions.set(super::sessions::rows_of(listed));
        }
    }

    fn transcript(&self) -> Option<Transcript> {
        let (session, turns, _) = self.master.transcript()?;
        let read = MasterRead {
            session: session.map(|session| session.phase),
            turns,
            inputs: self.master.inputs(),
        };
        let harness = match session {
            Some(session) => backend_name(session.selection.backend),
            None => self.screen.setup.as_ref().map_or_else(
                || "Master".to_owned(),
                |setup| harness_name(&setup.team.master, None),
            ),
        };
        let tools = self
            .screen
            .setup
            .as_ref()
            .is_some_and(|setup| setup.team.master_tools);
        let mut transcript = transcript_from(&read, &harness, None, tools, self.screen.offset);
        if session.is_none() {
            transcript.status = MasterStatus::Ready;
        }
        if let Some(waiting) = &self.waiting {
            transcript.messages.push(super::project::Message {
                speaker: Speaker::You,
                time: None,
                text: waiting.text.clone(),
                error: false,
            });
            transcript.status = MasterStatus::Thinking { since_ms: None };
        }
        Some(transcript)
    }

    fn drop_waiting(&mut self, ctx: &mut Ctx, problem: &str) {
        let Some(waiting) = self.waiting.take() else {
            return;
        };
        if self.screen.input.value().is_empty() {
            self.screen.input.set(&waiting.text);
        }
        ctx.say(Message::error(
            problem,
            "What you typed is back in the box.",
        ));
    }

    fn send_master(&mut self, text: String, ctx: &mut Ctx) {
        if self.waiting.is_some() || self.master.has_pending() {
            self.screen.input.set(&text);
            ctx.say(Message::warning(
                "The master is still taking your last message.",
                "Wait a moment, then send again.",
            ));
            return;
        }
        self.renewed = false;
        let session = self.master.transcript().and_then(|(session, turns, _)| {
            session.map(|session| {
                (
                    session.phase,
                    crate::tui::master_session::turn_holds(session.current_turn.as_ref(), turns),
                )
            })
        });
        match session {
            Some((crate::master_session::MasterSessionPhase::Configured, true)) => {
                self.screen.input.set(&text);
                ctx.say(Message::warning(
                    "The master is still working.",
                    "Press Ctrl-X to stop it, or wait for its reply.",
                ));
            }
            Some((crate::master_session::MasterSessionPhase::Configured, false)) => {
                self.retried = false;
                if self.master.submit(&text, ctx.next_generation()) != Ok(true) {
                    self.screen.input.set(&text);
                    ctx.say(Message::warning(
                        "The master is not ready for a message yet.",
                        "Wait a moment, then send again.",
                    ));
                }
            }
            _ => self.open_master(text, ctx),
        }
    }

    fn open_master(&mut self, text: String, ctx: &mut Ctx) {
        self.waiting = Some(Waiting {
            text,
            opening: false,
        });
        self.retried = false;
        if self.master.open().is_err() {
            self.drop_waiting(ctx, "The master could not start.");
        }
    }

    fn master_arrived(&mut self, event: &MasterEvent, ctx: &mut Ctx) {
        let sent = MasterSent::of_event(event);
        let Some(index) = self.master_sent.iter().position(|each| *each == sent) else {
            return;
        };
        self.master_sent.remove(index);
        let rejected = rejection(event);
        let barrier = not_recorded(event);
        let failed = matches!(event, MasterEvent::Mutation { result: Err(_), .. });
        let applied = self.master.apply(event.clone());
        let renewing = self.renew(rejected.as_deref(), submitted_text(event));
        if let Some(code) = rejected.as_deref().filter(|_| !renewing) {
            self.refused(event, code, ctx);
        }
        if (barrier || failed) && !self.retried {
            self.retried = true;
            if barrier {
                self.master.retry();
            } else {
                self.master.access();
            }
        } else if failed {
            ctx.say(Message::warning(
                "The engine did not confirm the master's last step.",
                "It is checked again when the connection is back.",
            ));
        }
        match applied {
            Err(_) => self.drop_waiting(ctx, "The master conversation stopped updating."),
            Ok(Some(confirmed)) => self.confirmed(&confirmed.action, ctx),
            Ok(None) if renewing => {}
            Ok(None) => self.advance_open(event, rejected.is_some() || failed, ctx),
        }
    }

    /// Ends a conversation the engine refused a message for because it opened before a
    /// settings change, so the message is sent once more in a new conversation.
    /// Takes the rejection code and the refused text.
    /// Returns true when the stale conversation is being ended.
    fn renew(&mut self, code: Option<&str>, text: Option<&str>) -> bool {
        let (Some("master_source_conflict"), Some(text)) = (code, text) else {
            return false;
        };
        if self.renewed || self.waiting.is_some() || self.master.control(true).is_err() {
            return false;
        }
        self.renewed = true;
        self.waiting = Some(Waiting {
            text: text.to_owned(),
            opening: false,
        });
        true
    }

    /// Tells the owner the engine refused a master request, in plain words with its code.
    /// Takes the event and the rejection code; a refused message goes back in the input box.
    fn refused(&mut self, event: &MasterEvent, code: &str, ctx: &mut Ctx) {
        let MasterEvent::Mutation { submission, .. } = event else {
            return;
        };
        if code == "master_turn_running"
            && self.screen.sessions.turn_was_running(&submission.action)
        {
            return;
        }
        if code == "master_session_unused" {
            ctx.say(Message::warning(
                "This session has no messages yet, so it already is a new session.",
                "",
            ));
            return;
        }
        let lead = match &submission.action {
            MasterOwnerAction::Submit { text, .. } => {
                if self.screen.input.value().is_empty() {
                    self.screen.input.set(text);
                }
                "The master did not take that message."
            }
            MasterOwnerAction::Open { .. }
            | MasterOwnerAction::NewSession { .. }
            | MasterOwnerAction::RegisterRcliProfile { .. } => "The master could not start.",
            MasterOwnerAction::Resume { .. } => "The session did not resume.",
            MasterOwnerAction::Cancel { .. } => "The master's turn did not stop.",
            MasterOwnerAction::Close { .. } => "The conversation did not end.",
            MasterOwnerAction::Resolve { .. } => "The master did not start resolving the update.",
            MasterOwnerAction::DeleteSession { .. } => "The session was not deleted.",
        };
        let problem = master_problem(code);
        if self.waiting.is_some() {
            self.drop_waiting(ctx, &format!("{lead} {problem}"));
        } else {
            ctx.say(Message::error(lead, &problem));
        }
    }

    /// Carries on after the engine confirmed a master request: an opened master takes the
    /// waiting message, a conversation ended for a renewal opens a new one, a session
    /// switch the owner started from the live master view opens that view again, and a new
    /// session of a master that runs live turns the live switch on, so the live master
    /// starts on the fresh session. A new session also clears the error that said it was
    /// already starting, while that error still shows. Takes the confirmed action and the
    /// context.
    fn confirmed(&mut self, action: &MasterOwnerAction, ctx: &mut Ctx) {
        match action {
            MasterOwnerAction::NewSession { .. } | MasterOwnerAction::Resume { .. } => {
                if self.from_master {
                    self.from_master = false;
                    self.reopen_master = true;
                }
                if matches!(action, MasterOwnerAction::NewSession { .. })
                    && let Some(said) = self.starting.take()
                {
                    ctx.unsay(&said);
                }
                if matches!(action, MasterOwnerAction::NewSession { .. })
                    && self.screen.live_master()
                {
                    self.turn_live_on(ctx);
                }
            }
            MasterOwnerAction::Open { .. } => {
                let Some(waiting) = self.waiting.take() else {
                    return;
                };
                self.retried = false;
                if self.master.submit(&waiting.text, ctx.next_generation()) != Ok(true) {
                    self.waiting = Some(waiting);
                    self.drop_waiting(ctx, "The master started but did not take the message.");
                } else if self.renewed {
                    ctx.say(Message::warning(
                        "Settings changed, so a new conversation started.",
                        "",
                    ));
                }
            }
            MasterOwnerAction::Close { .. }
                if self.waiting.is_some() && self.master.open().is_err() =>
            {
                self.drop_waiting(ctx, "The master could not start.");
            }
            _ => {}
        }
    }

    /// Turns the project's live master switch on, for a new session the owner asked for: it
    /// reads the team status first and switches at the team settings version read, since
    /// the switch is a versioned team setting (DEC-098). Takes the context; the replies
    /// arrive later.
    fn turn_live_on(&mut self, ctx: &mut Ctx) {
        self.live_wanted = true;
        self.send(
            SetupQuery::Team {
                project: self.screen.project.clone(),
            },
            ctx,
        );
    }

    fn send_live_on(&self, version: u64, ctx: &mut Ctx) {
        let generation = ctx.next_generation();
        ctx.request(Request::Setup(SetupQuery::TeamChange {
            action: crate::team::TeamAction::LiveMaster {
                project_id: self.screen.project.clone(),
                on: true,
                version,
            },
            generation,
        }));
    }

    fn advance_open(&mut self, event: &MasterEvent, stopped: bool, ctx: &mut Ctx) {
        let Some(waiting) = self.waiting.as_mut() else {
            return;
        };
        if stopped {
            self.drop_waiting(ctx, "The master could not start.");
            return;
        }
        if waiting.opening || !matches!(event, MasterEvent::Catalog { .. }) {
            return;
        }
        waiting.opening = true;
        if self.master.reduce(&UiEvent::Submit) != Ok(true) || !self.master.has_pending() {
            let _ = self.master.reduce(&UiEvent::Cancel);
            self.drop_waiting(
                ctx,
                "The master could not start. Check that the pool has a signed-in account.",
            );
        }
    }

    fn setup_arrived(&mut self, event: &SetupEvent, ctx: &mut Ctx) {
        let Some(index) = self.sent.iter().position(|query| query == &event.query) else {
            return;
        };
        let query = self.sent.remove(index);
        self.screen.now_ms = now_ms();
        self.screen.apply(&query, event.result.clone());
        let applied = matches!(event.result, Ok(SetupView::Changed(OwnerResult::Applied)));
        if matches!(query, SetupQuery::AgentPolicy { .. })
            && let Some(access) = self.access_wanted.take()
        {
            match &event.result {
                Ok(SetupView::AgentPolicy(view)) => {
                    self.send_created_access(access, view.version, ctx);
                }
                Err(code) => Self::stays_sandboxed(&plain_problem(code), ctx),
                Ok(_) => Self::stays_sandboxed(&plain_problem("change_outcome_unknown"), ctx),
            }
            return;
        }
        if matches!(query, SetupQuery::Team { .. }) && std::mem::take(&mut self.live_wanted) {
            let why = match &event.result {
                Ok(SetupView::Team(status)) => {
                    self.send_live_on(status.settings_version, ctx);
                    return;
                }
                Err(code) => plain_problem(code),
                Ok(_) => plain_problem("change_outcome_unknown"),
            };
            ctx.say(Message::warning(
                "The live master was not turned on for the new session.",
                &why,
            ));
            return;
        }
        let SetupQuery::Change { action, generation } = &query else {
            return;
        };
        if self.recording == Some(*generation) {
            self.recording = None;
            if !applied {
                ctx.say(Message::warning(
                    "The pool is changed, but the project details still name the old one.",
                    "Pick the pool again to save it.",
                ));
            }
            return;
        }
        if self.created_access == Some(*generation) {
            self.created_access = None;
            if !applied {
                let why = match &event.result {
                    Err(code) => plain_problem(code),
                    Ok(SetupView::Changed(OwnerResult::Rejected { code })) => plain_problem(code),
                    Ok(_) => plain_problem("change_outcome_unknown"),
                };
                Self::stays_sandboxed(&why, ctx);
            }
            return;
        }
        let name = |job: i64| {
            self.screen
                .jobs
                .iter()
                .flatten()
                .find(|known| known.id == job)
                .map_or_else(
                    || "The research".to_owned(),
                    |known| known.kind_label.clone(),
                )
        };
        match action {
            SetupAction::CancelResearch { job, .. } if applied => {
                ctx.say(Message::success(&format!("Stopping {}.", name(*job)), ""));
            }
            SetupAction::RetryResearch { job, .. } if applied => {
                ctx.say(Message::success(
                    &format!("{} will run again.", name(*job)),
                    "",
                ));
            }
            SetupAction::AddDocuments { .. } if applied => {
                ctx.say(Message::success("Document added.", ""));
            }
            SetupAction::Update { .. } if applied => {
                ctx.say(Message::success("Changes saved.", ""));
            }
            SetupAction::Remove { .. } if applied => {
                ctx.request(Request::Projects);
                ctx.say(Message::success(
                    &format!("{} was removed.", self.screen.name),
                    "Its folder stays on disk.",
                ));
            }
            _ => {}
        }
    }

    fn say_problem(&mut self, ctx: &mut Ctx) {
        if let Some(problem) = self.screen.problem.take() {
            ctx.say(Message::error(&problem, ""));
        }
    }

    fn stop_master(&mut self, ctx: &mut Ctx) {
        match self.master.control(false) {
            Ok(()) => ctx.say(Message::success("Stopping the master's turn.", "")),
            Err(_) => ctx.say(Message::warning(
                "The master is not working on anything.",
                "",
            )),
        }
    }

    fn end_master(&mut self, ctx: &mut Ctx) {
        if self.waiting.is_some() || self.master.has_pending() {
            ctx.say(Message::warning(
                "The master is still taking your last message.",
                "Wait a moment, then try again.",
            ));
            return;
        }
        match self.master.control(true) {
            Ok(()) => ctx.say(Message::success(
                "Ending the conversation.",
                "Your next message starts a new one.",
            )),
            Err(_) => ctx.say(Message::warning(
                "There is no open conversation to end.",
                "",
            )),
        }
    }

    fn retry_master(&mut self, ctx: &mut Ctx) {
        let last = self
            .master
            .transcript()
            .and_then(|(_, _, input)| input)
            .map(|entry| entry.text.clone());
        match last {
            Some(text) => self.send_master(text, ctx),
            None => ctx.say(Message::warning("There is no message to send again.", "")),
        }
    }

    fn refresh_pool(&mut self, ctx: &Ctx) {
        let (Some(pools), Some(scopes)) =
            (ctx.store.pools.ready(), ctx.store.pool_projects.ready())
        else {
            self.screen.pool_ready = false;
            return;
        };
        let picked = self
            .screen
            .setup
            .as_ref()
            .and_then(|setup| setup.pool.as_ref());
        self.screen.pool = project_pool(pools, scopes, &self.screen.project, picked);
        self.screen.set_pool_choices(pool_choices_from(pools));
        self.screen.pool_ready = true;
    }

    fn assign(&mut self, pool: Option<CredentialRef>, ctx: &mut Ctx) {
        if self.assigning.is_some() {
            ctx.say(Message::warning(
                "Still assigning the pool.",
                "Wait a moment, then try again.",
            ));
            return;
        }
        let name = pool.as_ref().map(|pool| {
            self.screen
                .pool_choices
                .iter()
                .find(|choice| &choice.id == pool)
                .map_or_else(|| "The pool".to_owned(), |choice| choice.name.clone())
        });
        self.assigning = Some((pool.clone(), name));
        ctx.request(Request::EditPool {
            change: PoolChange::Assign {
                pool,
                project: self.screen.project.clone(),
            },
        });
    }

    /// Records the pool the owner just gave the project in its details too, so a removed
    /// pool is not offered again as the picked one. Takes the pool, None for no pool.
    /// Sends nothing when the details already name it; its reply stays quiet unless it fails.
    fn record_pool(&mut self, pool: Option<CredentialRef>, ctx: &mut Ctx) {
        let Some(setup) = self.screen.setup.as_ref() else {
            return;
        };
        if setup.pool == pool {
            return;
        }
        let action = SetupAction::Update {
            project_id: setup.project_id.clone(),
            expected_revision: setup.revision,
            name: None,
            setup: ProjectSetup {
                about: setup.about.clone(),
                tech_stack: setup.tech_stack.clone(),
                platforms: setup.platforms.clone(),
                research: setup.research.clone(),
                pool,
                team: Some(setup.team.clone()),
                documents: Vec::new(),
            },
        };
        let generation = ctx.next_generation();
        self.recording = Some(generation);
        self.send(SetupQuery::Change { action, generation }, ctx);
    }

    fn assigned(&mut self, result: &Result<(), String>, ctx: &mut Ctx) {
        let Some((pool, name)) = self.assigning.take() else {
            return;
        };
        if result.is_ok() {
            self.record_pool(pool, ctx);
        }
        match (result, name) {
            (Ok(()), Some(name)) => ctx.say(Message::success(
                &format!("{name} is assigned."),
                "Work can start now.",
            )),
            (Ok(()), None) => ctx.say(Message::success(
                "This project has no pool now.",
                "Pick a pool to start.",
            )),
            (Err(code), _) => ctx.say(Message::error(
                "The pool was not changed.",
                &plain_problem(code),
            )),
        }
        ctx.request(Request::Pools);
        ctx.request(Request::PoolProjects);
    }

    fn redraw(&mut self, ctx: &Ctx) {
        self.screen.width = ctx.width;
        self.screen.changes.painting(false);
        self.view = project_screen(&mut self.screen, ctx.width, ctx.height, &look(ctx));
        self.screen.changes.painting(true);
    }

    fn pump(&mut self, ctx: &mut Ctx) {
        if let Some(query) = self.screen.recovery.next_query(now_ms()) {
            ctx.request(Request::Recovery(Box::new(query)));
        }
        if self.screen.tab == Tab::Changes
            && let Some(query) = self.screen.changes.next_query(now_ms())
        {
            ctx.request(Request::Changes(Box::new(query)));
        }
        if self.screen.tab == Tab::Questions
            && let Some(query) = self.screen.questions.next_query(now_ms())
        {
            ctx.request(Request::Questions(Box::new(query)));
        }
        if self.screen.tab == Tab::Settings
            && let Some(job) = self.screen.plugins.next_job()
        {
            send_settings(ctx, job);
        }
        if self.screen.tab == Tab::Settings && !self.schedule_asked {
            self.schedule_asked = true;
            send_settings(ctx, SettingsJob::Schedule);
        }
        if self.sent.len() >= SENT_MAX {
            return;
        }
        if let Some(query) = self.screen.next_query(now_ms()) {
            self.sent.push(query.clone());
            ctx.request(Request::Setup(query));
        }
    }

    fn send(&mut self, query: SetupQuery, ctx: &mut Ctx) {
        if self.sent.len() >= SENT_MAX {
            ctx.say(Message::warning(
                "Still working.",
                "Wait a moment, then try again.",
            ));
            return;
        }
        self.sent.push(query.clone());
        ctx.request(Request::Setup(query));
    }

    /// Notes a key's move onto or off the Conversation tab: entering the tab offers the live
    /// master view, a key typed on the tab or leaving it withdraws the offer.
    /// Inputs: the tab before the key and the flow the key gave. Output: the flow to apply.
    fn entered(&mut self, before: Tab, flow: Flow) -> Flow {
        self.master_entry = self.screen.tab == Tab::Conversation && before != Tab::Conversation;
        self.offer_master(flow)
    }

    /// Opens the master view on the Conversation tab just entered, once the stored plan is
    /// read, when its master runs live (Codex or Claude Code), as the desktop shows that
    /// master's terminal there; another master keeps the console.
    /// Input: the flow so far. Output: that flow, or the master view.
    fn offer_master(&mut self, flow: Flow) -> Flow {
        if !matches!(flow, Flow::Handled)
            || !self.master_entry
            || self.covered
            || self.screen.setup.is_none()
        {
            return flow;
        }
        self.master_entry = false;
        if !self.screen.live_master() {
            return flow;
        }
        self.covered = true;
        Flow::Open(Place::Master {
            id: self.screen.project.clone(),
            name: self.screen.name.clone(),
        })
    }

    fn reply(&mut self, reply: ProjectReply, ctx: &mut Ctx) -> Flow {
        let flow = match reply {
            ProjectReply::Ignored => Flow::Ignored,
            ProjectReply::Handled => Flow::Handled,
            ProjectReply::Home => Flow::Back,
            ProjectReply::Change(action) => {
                let generation = ctx.next_generation();
                self.send(
                    SetupQuery::Change {
                        action: *action,
                        generation,
                    },
                    ctx,
                );
                Flow::Handled
            }
            ProjectReply::Review {
                entry,
                version,
                decision,
            } => {
                let generation = ctx.next_generation();
                self.send(
                    SetupQuery::ReviewMemory {
                        project: self.screen.project.clone(),
                        entry_id: entry,
                        version,
                        decision,
                        generation,
                    },
                    ctx,
                );
                Flow::Handled
            }
            ProjectReply::Open(request) => {
                match open_request(&request) {
                    Ok(()) => ctx.say(Message::success("Opened.", "")),
                    Err(problem) => ctx.say(Message::error(&problem, "")),
                }
                Flow::Handled
            }
            ProjectReply::Reveal(target) => {
                self.reveal(target, ctx);
                Flow::Handled
            }
            ProjectReply::Edit(request) => {
                match opening(&request) {
                    Ok(Opening::Open(file)) => editor::ask(ctx, file, None),
                    Ok(Opening::Reveal(_)) => unreachable!("a folder never goes to the editor"),
                    Err(problem) => ctx.say(Message::error(&problem, "")),
                }
                Flow::Handled
            }
            ProjectReply::Assign(pool) => {
                self.assign(pool, ctx);
                Flow::Handled
            }
            ProjectReply::PickPool => {
                ctx.say(Message::warning(
                    "There are no pools yet.",
                    "Create one here, then assign it in the project's Settings tab.",
                ));
                Flow::Open(Place::Settings(SettingsSection::Pools))
            }
            ProjectReply::Send(text) => {
                self.send_master(text, ctx);
                Flow::Handled
            }
            ProjectReply::StopTurn => {
                self.stop_master(ctx);
                Flow::Handled
            }
            ProjectReply::EndConversation => {
                self.end_master(ctx);
                Flow::Handled
            }
            ProjectReply::Session(action) => {
                let in_view = self
                    .master
                    .transcript()
                    .is_some_and(|(session, _, _)| session.is_some());
                if let Err(problem) = super::sessions::act(&mut self.master, &action) {
                    let said = Message::error("The session did not switch.", &problem);
                    if problem == NEW_SESSION_STARTING {
                        self.starting = Some(said.clone());
                    }
                    ctx.say(said);
                } else if matches!(action, super::sessions::SessionsAction::New { .. })
                    && !in_view
                    && self.screen.live_master()
                {
                    self.turn_live_on(ctx);
                }
                Flow::Handled
            }
            ProjectReply::Retry => {
                self.retry_master(ctx);
                Flow::Handled
            }
            ProjectReply::Team => Flow::Open(Place::Team {
                id: self.screen.project.clone(),
                name: self.screen.name.clone(),
            }),
            ProjectReply::TeamSettings => Flow::Open(Place::TeamSettings {
                id: self.screen.project.clone(),
                name: self.screen.name.clone(),
            }),
            ProjectReply::Master => Flow::Open(Place::Master {
                id: self.screen.project.clone(),
                name: self.screen.name.clone(),
            }),
            ProjectReply::Process => Flow::Open(Place::ProjectProcess {
                id: self.screen.project.clone(),
                name: self.screen.name.clone(),
            }),
            ProjectReply::Usage => Flow::Open(Place::Usage),
            ProjectReply::AccountOrder(strategy) => {
                self.set_order(strategy, ctx);
                Flow::Handled
            }
        };
        self.sync_master(ctx);
        self.pump(ctx);
        self.say_problem(ctx);
        self.redraw(ctx);
        flow
    }

    fn set_order(&mut self, strategy: Option<crate::scheduling::Strategy>, ctx: &mut Ctx) {
        let Ok(project) = CredentialRef::new(self.screen.project.as_str()) else {
            self.screen.problem = Some("This project can't keep its own account order.".into());
            return;
        };
        let generation = ctx.next_generation();
        send_settings(
            ctx,
            SettingsJob::SetSchedule {
                change: crate::scheduling::ScheduleChange::ProjectStrategy { project, strategy },
                version: self
                    .screen
                    .schedule
                    .as_ref()
                    .map_or(0, |schedule| schedule.version),
                generation,
            },
        );
    }

    fn schedule_arrived(&mut self, event: &SettingsEvent, ctx: &mut Ctx) {
        let failed = |code: &str, fallback: &'static str| {
            crate::config::plain_problem(code)
                .unwrap_or(fallback)
                .to_owned()
        };
        match event {
            SettingsEvent::Schedule(Ok(schedule)) => self.screen.schedule = Some(schedule.clone()),
            SettingsEvent::Schedule(Err(code)) => {
                self.screen.problem = Some(failed(code, "Could not read the account order."));
            }
            SettingsEvent::ScheduleSet(_, result) => {
                if let Err(code) = result {
                    self.screen.problem = Some(failed(code, "Could not save the account order."));
                }
                send_settings(ctx, SettingsJob::Schedule);
            }
            _ => {}
        }
    }

    fn reveal(&self, target: RevealTarget, ctx: &mut Ctx) {
        ctx.request(Request::Reveal(Box::new(RevealQuery {
            project: self.screen.project.clone(),
            target,
        })));
    }

    fn edit_changed(&self, path: &str, ctx: &mut Ctx) {
        let Some(folder) = self.screen.project_folder() else {
            ctx.say(Message::error(
                "The project folder isn't loaded yet.",
                "Try again in a moment.",
            ));
            return;
        };
        match changed_file(&folder, path) {
            Ok(file) => editor::ask(ctx, file, Some(CHANGED_NOTE)),
            Err(problem) => ctx.say(Message::error(problem, "")),
        }
    }

    fn recovery_has_keys(&self) -> bool {
        self.screen.edit.is_none()
            && self.screen.review.is_none()
            && self.screen.pool_picker.is_none()
            && self.screen.confirm_delete.is_none()
            && self.screen.confirm_stop.is_none()
            && self.screen.adding.is_none()
            && self.screen.folders.is_none()
            && self.screen.general.is_none()
            && !self.screen.confirm_remove
            && !self.screen.changes.editing()
            && !self.typing()
    }

    fn changes_have_keys(&self) -> bool {
        self.screen.tab == Tab::Changes && !self.screen.boxed()
    }

    fn questions_have_keys(&self) -> bool {
        self.screen.tab == Tab::Questions
            && self.screen.edit.is_none()
            && self.screen.review.is_none()
    }

    fn plugins_have_keys(&self) -> bool {
        self.screen.tab == Tab::Settings && !self.screen.boxed()
    }

    fn plugins_reply(&mut self, reply: PluginsReply, ctx: &mut Ctx) -> Flow {
        let project = self.screen.project.clone();
        let name = self.screen.name.clone();
        let flow = match reply {
            PluginsReply::Ignored => Flow::Ignored,
            PluginsReply::Handled => Flow::Handled,
            PluginsReply::Send(job) => {
                send_settings(ctx, job);
                Flow::Handled
            }
            PluginsReply::AddSkill => {
                return Flow::Replace(Box::new(AddPlugin::skill().for_project(project, name)));
            }
            PluginsReply::AddServer => {
                return Flow::Replace(Box::new(AddPlugin::server().for_project(project, name)));
            }
            PluginsReply::AddHooks => {
                return Flow::Replace(Box::new(AddPlugin::hooks().for_project(project, name)));
            }
            PluginsReply::OpenSkill { id, name: skill } => {
                return Flow::Replace(Box::new(
                    SkillDetailPage::new(id, skill).for_project(project, name),
                ));
            }
            PluginsReply::Edit(server) => {
                return Flow::Replace(Box::new(
                    EditServer::new(*server).for_project(project, name),
                ));
            }
            PluginsReply::Tools { servers, at } => {
                return Flow::Replace(Box::new(
                    ToolsPage::new(servers, at).for_project(project, name),
                ));
            }
        };
        self.pump(ctx);
        self.redraw(ctx);
        flow
    }
}

impl Screen for ProjectPage {
    fn crumbs(&self) -> Vec<String> {
        self.view.crumbs.clone()
    }

    fn status(&self, _ctx: &Ctx) -> Line<'static> {
        self.view.status.clone().map(Row::line).unwrap_or_default()
    }

    fn hints(&self, _ctx: &Ctx) -> Vec<Hint> {
        let mut hints = hints_of(&self.view);
        if !self.uncommitted.is_empty() {
            hints.insert(0, hint("Ctrl-G", "Commit the move"));
        }
        hints
    }

    fn typing(&self) -> bool {
        self.import.is_some()
            || self.moved.is_some()
            || self.screen.edit.is_some()
            || self.screen.adding.is_some()
            || self.screen.folders.is_some()
            || self.screen.hidden.is_some()
            || self.screen.access.is_some()
            || self.screen.general.is_some()
            || (self.screen.tab == Tab::Conversation && self.screen.typing)
            || (self.changes_have_keys() && self.screen.changes.editing())
            || (self.questions_have_keys() && self.screen.questions.editing())
    }

    fn key(&mut self, key: &Key, ctx: &mut Ctx) -> Flow {
        let before = self.screen.tab;
        if self.moved.is_some() {
            return self.moved_key(key, ctx);
        }
        if self.import.is_some() {
            return self.import_key(key, ctx);
        }
        if !self.uncommitted.is_empty() && *key == Key::Ctrl('g') {
            self.commit_move(ctx);
            return Flow::Handled;
        }
        self.screen.width = ctx.width;
        if self.recovery_has_keys() && self.screen.recovery.key(key) {
            self.pump(ctx);
            self.redraw(ctx);
            return Flow::Handled;
        }
        if self.changes_have_keys()
            && let Some(event) = key_event(key)
            && self.screen.changes.key(event, now_ms())
        {
            if let Some(path) = self.screen.changes.take_edit() {
                self.edit_changed(&path, ctx);
            }
            if let Some(target) = self.screen.changes.take_reveal() {
                self.reveal(target, ctx);
            }
            self.pump(ctx);
            self.redraw(ctx);
            return Flow::Handled;
        }
        if self.questions_have_keys()
            && let Some(event) = key_event(key)
            && self.screen.questions.key(event)
        {
            self.pump(ctx);
            self.redraw(ctx);
            return Flow::Handled;
        }
        if self.plugins_have_keys()
            && let Some(event) = key_event(key)
        {
            match self.screen.plugins.key(event) {
                PluginsReply::Ignored => {}
                reply => return self.plugins_reply(reply, ctx),
            }
        }
        let reply = match key {
            Key::Paste(text) => self.screen.paste(text),
            other => match key_event(other) {
                Some(event) => self.screen.key(event),
                None => ProjectReply::Ignored,
            },
        };
        let flow = self.reply(reply, ctx);
        let flow = self.entered(before, flow);
        self.covered = matches!(&flow, Flow::Open(place) if Some(place) != self.place().as_ref());
        self.ask_team(ctx);
        self.ask_files(ctx);
        flow
    }

    fn step(&mut self, forward: bool, ctx: &mut Ctx) -> Flow {
        let code = if forward {
            KeyCode::Right
        } else {
            KeyCode::Left
        };
        let before = self.screen.tab;
        let reply = self.screen.key(KeyEvent::new(code, KeyModifiers::SHIFT));
        let flow = self.reply(reply, ctx);
        self.entered(before, flow)
    }

    fn arrive(&mut self, arrival: &Arrival, ctx: &mut Ctx) -> Flow {
        match arrival {
            Arrival::TeamGraph { project, result } if *project == self.screen.project => {
                self.team_asked = self.team_asked.map(|(at, _)| (at, false));
                self.screen.team = match result {
                    Ok(graph) => Load::Ready(graph.clone()),
                    Err(code) if code == NOT_READY => Load::NotReady,
                    Err(_) if self.screen.team.ready().is_some() => self.screen.team.clone(),
                    Err(code) => Load::Failed(code.clone()),
                };
                self.redraw(ctx);
                return Flow::Handled;
            }
            Arrival::ProjectFiles {
                project,
                query,
                result,
            } if *project == self.screen.project => {
                self.files_asked = None;
                let paths = result.as_ref().map_or_else(|_| Vec::new(), Clone::clone);
                self.screen.files = Some((query.clone(), paths));
                self.ask_files(ctx);
                self.redraw(ctx);
                return Flow::Handled;
            }
            Arrival::Owner(event) => match event.as_ref() {
                OwnerEvent::Setup(event) => self.setup_arrived(event, ctx),
                OwnerEvent::Master(event) => self.master_arrived(event, ctx),
                OwnerEvent::Settings(event) => {
                    self.schedule_arrived(event, ctx);
                    self.screen.plugins.apply(event);
                }
                OwnerEvent::Changes(event) => self.screen.changes.apply(event),
                OwnerEvent::Questions(event) => self.screen.questions.apply(event),
                OwnerEvent::Recovery(event) if event.query.project() == &self.screen.project => {
                    self.screen.recovery.apply(event);
                }
                OwnerEvent::Process(event) => {
                    self.import_arrived(event, ctx);
                    return Flow::Handled;
                }
                _ => return Flow::Handled,
            },
            Arrival::PoolEdited(result) => self.assigned(result, ctx),
            Arrival::Pools(_) | Arrival::PoolProjects(_) => {}
            Arrival::Connected(_) => {
                self.select_master(ctx);
                self.master.access();
                self.ask_import(ctx);
                self.ask_uncommitted(ctx);
            }
            Arrival::Disconnected(_) => {
                if let Some(ProcessQuery::PendingImport { .. }) = self.import_sent.take() {
                    self.import_asked = false;
                }
                self.uncommitted_asked = false;
                self.sent.clear();
                self.screen.schedule = None;
                self.schedule_asked = false;
                self.team_asked = None;
                self.files_asked = None;
                self.screen.files = None;
                self.screen.forget_reads();
                self.screen.plugins.forget_reads();
                self.screen.changes.forget_reads();
                self.screen.questions.forget_reads();
                self.screen.recovery.forget_reads();
                self.master_sent
                    .retain(|sent| matches!(sent, MasterSent::Mutation(_)));
            }
            _ => return Flow::Handled,
        }
        if self.screen.removed {
            return Flow::Back;
        }
        self.refresh_pool(ctx);
        self.sync_master(ctx);
        self.pump(ctx);
        self.say_problem(ctx);
        self.ask_team(ctx);
        self.redraw(ctx);
        if std::mem::take(&mut self.reopen_master) && !self.covered && self.screen.live_master() {
            return Flow::Open(Place::Master {
                id: self.screen.project.clone(),
                name: self.screen.name.clone(),
            });
        }
        self.offer_master(Flow::Handled)
    }

    fn shown(&mut self, ctx: &mut Ctx) {
        ctx.request(Request::Pools);
        ctx.request(Request::PoolProjects);
        self.schedule_asked = false;
        self.covered = false;
        self.screen.setup_due = true;
        if let Some((_, code)) = ctx
            .sessions_key
            .take_if(|(project, _)| *project == self.screen.project)
        {
            assert!(matches!(code, 'n' | 'l'));
            self.from_master = true;
            self.screen.open(Tab::Conversation);
            self.screen.typing = false;
            let reply = self
                .screen
                .key(KeyEvent::new(KeyCode::Char(code), KeyModifiers::NONE));
            let _ = self.reply(reply, ctx);
        }
        if let Some((_, forward)) = ctx
            .project_step
            .take_if(|(project, _)| *project == self.screen.project)
        {
            let _ = self.step(forward, ctx);
        }
        self.ask_team(ctx);
        self.ask_import(ctx);
        self.ask_uncommitted(ctx);
        self.refresh_pool(ctx);
        self.sync_master(ctx);
        self.pump(ctx);
        self.say_problem(ctx);
        self.redraw(ctx);
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect, ctx: &Ctx) -> Option<Position> {
        let banner = self.banner(usize::from(area.width), ctx);
        let rows = u16::try_from(banner.len())
            .unwrap_or(u16::MAX)
            .min(area.height / 2);
        if rows > 0 {
            draw_lines(
                frame.buffer_mut(),
                Rect {
                    height: rows,
                    ..area
                },
                &banner,
            );
        }
        let area = Rect {
            y: area.y + rows,
            height: area.height - rows,
            ..area
        };
        assert!(rows == 0 || !self.uncommitted.is_empty());
        self.screen.width = area.width;
        self.screen.now_ms = now_ms();
        self.view = project_screen(
            &mut self.screen,
            area.width,
            area.height.saturating_add(4),
            &look(ctx),
        );
        paint(frame, area, &self.view)
    }

    fn modal(&self, ctx: &Ctx) -> Option<(Modal, Vec<Hint>)> {
        if let Some(moved) = &self.moved {
            return Some(self.moved_modal(moved, ctx));
        }
        let files = self.import.as_ref()?;
        let theme = &ctx.theme;
        let room = usize::from(IMPORT_MODAL_WIDTH) - 4;
        let mut body: Vec<Line<'static>> = wrap(
            "CrazyAgent keeps this project's instructions in .crazyagent/instructions.md and no longer reads these files when agents start:",
            room,
        )
        .into_iter()
        .map(Line::raw)
        .collect();
        for file in files.iter().take(IMPORT_ROWS_MAX) {
            let size = if file.bytes < 1024 {
                format!("{} bytes", file.bytes)
            } else {
                format!("{} KB", file.bytes.div_ceil(1024))
            };
            let path = sanitize(&file.path);
            let text = match &file.refused {
                None if file.tracked => format!("- {path} ({size}, tracked by git)"),
                None => format!("- {path} ({size})"),
                Some(code) => format!("- {path}: stays in place, {}", import_reason(code)),
            };
            body.extend(wrap(&text, room).into_iter().map(Line::raw));
        }
        if files.len() > IMPORT_ROWS_MAX {
            body.push(styled(
                format!("and {} more", files.len() - IMPORT_ROWS_MAX),
                Role::Muted,
                theme,
            ));
        }
        body.push(Line::default());
        if files
            .iter()
            .any(|file| file.refused.is_none() && file.tracked)
        {
            body.extend(
                wrap(
                    "Git tracks some of them, so after the move git shows them as deleted.",
                    room,
                )
                .into_iter()
                .map(|text| styled(text, Role::Warning, theme)),
            );
        }
        let movable = files.iter().any(|file| file.refused.is_none());
        let note = if movable {
            "Enter copies them into instructions.md and moves them to .crazyagent/imported/. Not now asks again the next time you open the project."
        } else {
            "None of them can be moved. Not now asks again the next time you open the project."
        };
        body.extend(
            wrap(note, room)
                .into_iter()
                .map(|text| styled(text, Role::Muted, theme)),
        );
        let mut hints = Vec::new();
        if self.import_sent.is_none() {
            if movable {
                hints.push(hint("Enter", "Copy into instructions.md and move aside"));
            }
            hints.push(hint("Esc", "Not now"));
        }
        Some((
            Modal {
                width: IMPORT_MODAL_WIDTH,
                title: "Instruction files found".into(),
                body,
                top: None,
            },
            hints,
        ))
    }

    fn busy(&self, _ctx: &Ctx) -> bool {
        self.import_sent.is_some()
            || self.screen.counts().0 > 0
            || self.master.has_pending()
            || self.waiting.is_some()
            || self.screen.plugins.busy()
            || self.screen.changes.busy()
            || self.screen.questions.busy()
            || self.screen.recovery.busy()
    }

    fn draft(&self) -> bool {
        self.screen.edit.is_some()
            || self.screen.general.is_some()
            || self.screen.folders.is_some()
            || self.screen.hidden.is_some()
            || self.screen.access.is_some()
            || !self.screen.input.value().trim().is_empty()
    }

    fn loss(&self) -> &'static str {
        if self.screen.edit.is_some() {
            "Your memory edit will be lost."
        } else if self.screen.general.is_some() {
            "Your project changes will be lost."
        } else if self.screen.folders.is_some() {
            "Your read-only folders will not be saved."
        } else if self.screen.hidden.is_some() {
            "Your hidden folders will not be saved."
        } else if self.screen.access.is_some() {
            "Your agent access changes will not be saved."
        } else {
            "Your unsent message will be lost."
        }
    }

    fn place(&self) -> Option<Place> {
        Some(Place::Project {
            id: self.screen.project.clone(),
            name: self.screen.name.clone(),
            tab: project_tab(self.screen.tab),
        })
    }
}
