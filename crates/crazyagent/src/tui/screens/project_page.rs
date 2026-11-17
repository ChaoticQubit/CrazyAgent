use super::intake_job;
use super::new_project::harness_name;
use super::new_project_page::{hints_of, key_event, look, paint, pool_choices_from};
use super::new_project_publish::{
    PUBLISH_MAY_HAVE_STARTED, PUBLISH_NOT_STARTED, PUBLISH_STARTED, PUBLISH_WHERE_IT_STANDS,
};
use super::new_project_view::{Row, Screen as View};
use super::project::{
    MasterStatus, OpenRequest, ProjectReply, ProjectScreen, Speaker, Tab, Transcript,
    plain_problem, project_pool,
};
use super::project_markdown::sanitize;
use super::project_master::{MasterRead, master_problem, transcript_from};
use super::project_plugins::PluginsReply;
use super::project_publish::{
    AskKey, AskPane, DRAFT_POLL_MS, PUBLISH_AGAIN, PUBLISH_NOT_SEEN, PaneKey, PublishPane,
    left_line, lost_start_over, opens, query,
};
use super::project_time::local_offset;
use super::project_view::{asks_github, project_screen};
use super::settings_accounts::send as send_settings;
use super::settings_plugins::{AddPlugin, EditServer, SkillDetailPage, ToolsPage};
use super::{Flow, Place, ProjectTab, Screen, SettingsSection};
use crate::engine::owner::{ImportFile, OwnerResponse, OwnerResult};
use crate::github::{GitHubPublishPlan, GitHubPublishStep};
use crate::ide::bridge::reveal::RevealTarget;
use crate::master_session::{MasterBackend, MasterOwnerAction};
use crate::project_setup::{ProjectSetup, SetupAction, SetupQuery, SetupView};
use crate::projects_home::AddMode;
use crate::projects_home::job::{JobKind, JobSubject};
use crate::protocol::{CredentialRef, ProjectId, RequestId};
use crate::sandbox::policy::{AgentAccess, AgentPolicy};
use crate::tui::app::UiEvent;
use crate::tui::client::editor;
use crate::tui::client::ide::RevealQuery;
use crate::tui::client::intake::{IntakeEvent, IntakeQuery, IntakeView, start_id};
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

mod asks;
mod replies;

const SENT_MAX: usize = 8;
const MASTER_SENT_MAX: usize = 4;
const TEAM_READ_MS: i64 = 3_000;
const TEAM_WAIT_MS: i64 = 15_000;
const CHANGED_MISSING: &str =
    "That file isn't in your project folder yet. It lands there when you accept the change.";
const CHANGED_NOTE: &str =
    "That was your copy in the project folder. The agent's version lands there when you accept.";
const ANSWER_NOT_TAKEN: &str = "The publish answer wasn't taken.";
const START_REPLY_LOST: [&str; 2] = ["github_outcome_unknown", "github_reply_bounds"];
const ASK_STARTING: &str = "Starting the publish...";
const ASK_SENDING: &str = "Sending your answer...";
const ASK_READING: &str = "Checking...";
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

/// Whether a GitHub publish Start's reply was lost after the engine may have run it: the
/// outcome is unknown, the reply was too large, or it was not a publish. Input: the reply.
/// Output: the answer, false for a publish or a refusal.
fn start_reply_lost(result: &Result<IntakeView, String>) -> bool {
    match result {
        Ok(IntakeView::Publish(_)) => false,
        Ok(_) => true,
        Err(code) => START_REPLY_LOST.contains(&code.as_str()),
    }
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
        MasterBackend::Listed(id) => harness_name(id, None),
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
    job_sent: Option<IntakeQuery>,
    publish_sent: Option<IntakeQuery>,
    publish_pane: Option<PublishPane>,
    publish_asked: Option<IntakeQuery>,
    publish_later: bool,
    publish_read_ms: Option<i64>,
    publish_lost_ms: Option<i64>,
    ask_pane: Option<AskPane>,
    publish_offered: bool,
    moving: Option<RequestId>,
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
            || self.publish_shows()
            || self.ask_pane.is_some()
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
        if self.publish_shows() {
            return self.publish_key(key, ctx);
        }
        if self.ask_pane.is_some() {
            return self.ask_key(key, ctx);
        }
        if !self.uncommitted.is_empty() && *key == Key::Ctrl('g') {
            self.commit_move(ctx);
            return Flow::Handled;
        }
        self.screen.width = ctx.width;
        if self.recovery_has_keys() && self.screen.recovery.key(key) {
            if self.screen.recovery.take_delete_again() {
                self.screen.ask_delete_again();
            }
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
        if reply == ProjectReply::Ignored
            && *key == Key::Char('G')
            && self.screen.tab == Tab::Overview
            && asks_github(&self.screen)
            && self.publish_sent.is_none()
        {
            self.ask_pane = Some(AskPane::default());
            return Flow::Handled;
        }
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
                return self.offer_master(Flow::Handled);
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
                OwnerEvent::Intake(event) => {
                    if !self.intake_arrived(event, ctx) {
                        return Flow::Handled;
                    }
                }
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
                OwnerEvent::Roadmap(event) if event.query.project() == &self.screen.project => {
                    self.screen.roadmap.apply(event);
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
                self.job_sent = None;
                self.publish_sent = None;
                self.publish_asked = None;
                self.publish_lost_ms = None;
                self.ask_pane = None;
                self.publish_offered = false;
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
            return Flow::Go(Place::Home);
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
        if self.screen.tab != Tab::Conversation {
            self.screen.team = Load::Idle;
        }
        self.ask_team(ctx);
        self.ask_import(ctx);
        self.ask_uncommitted(ctx);
        self.refresh_pool(ctx);
        self.sync_master(ctx);
        self.pump(ctx);
        self.publish_later = false;
        self.ask_publish(ctx);
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
        self.screen.changes.painting(true);
        self.screen.plugins.painting(true);
        self.view = project_screen(
            &mut self.screen,
            area.width,
            area.height.saturating_add(4),
            &look(ctx),
        );
        self.screen.plugins.painting(false);
        self.screen.changes.painting(false);
        paint(frame, area, &self.view)
    }

    fn modal(&self, ctx: &Ctx) -> Option<(Modal, Vec<Hint>)> {
        if let Some(moved) = &self.moved {
            return Some(self.moved_modal(moved, ctx));
        }
        let Some(files) = self.import.as_ref() else {
            return self.publish_modal(ctx);
        };
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
            || self.publish_asked.is_some()
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
