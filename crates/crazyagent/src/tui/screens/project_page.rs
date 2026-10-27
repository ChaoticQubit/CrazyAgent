use super::new_project::harness_name;
use super::new_project_page::{hints_of, key_event, look, paint, pool_choices_from};
use super::new_project_view::{Row, Screen as View};
use super::project::{
    MasterStatus, OpenRequest, ProjectReply, ProjectScreen, Speaker, Tab, Transcript,
    plain_problem, project_pool,
};
use super::project_master::{MasterRead, master_problem, transcript_from};
use super::project_plugins::PluginsReply;
use super::project_time::local_offset;
use super::project_view::project_screen;
use super::settings_accounts::send as send_settings;
use super::settings_plugins::{AddPlugin, EditServer, SkillDetailPage};
use super::{Flow, Place, ProjectTab, Screen, SettingsSection};
use crate::engine::owner::{OwnerResponse, OwnerResult};
use crate::master_session::{MasterBackend, MasterOwnerAction};
use crate::project_setup::{ProjectSetup, SetupAction, SetupQuery, SetupView};
use crate::protocol::{CredentialRef, ProjectId};
use crate::tui::app::UiEvent;
use crate::tui::client::{OwnerEvent, SetupEvent};
use crate::tui::clock::now_ms;
use crate::tui::ctx::{Ctx, Link, Load};
use crate::tui::frame::{Hint, Message};
use crate::tui::keys::Key;
use crate::tui::master_session::{
    Context as MasterContext, Event as MasterEvent, Job as MasterJob, State as MasterState,
    Submission as MasterSubmission,
};
use crate::tui::seam::{Arrival, NOT_READY, PoolChange, Request};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::text::Line;
use std::path::PathBuf;

const SENT_MAX: usize = 8;
const MASTER_SENT_MAX: usize = 4;
const TEAM_READ_MS: i64 = 3_000;
const TEAM_WAIT_MS: i64 = 15_000;

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

/// One project as a shell screen.
/// Holds the project state, its last drawn view, the engine reads it sent and its master conversation.
/// Reads go one at a time through the shell queue; only replies to its own reads are applied.
pub(in crate::tui) struct ProjectPage {
    screen: ProjectScreen,
    view: View,
    sent: Vec<SetupQuery>,
    assigning: Option<(Option<CredentialRef>, Option<String>)>,
    recording: Option<u64>,
    master: MasterState,
    master_sent: Vec<MasterSent>,
    waiting: Option<Waiting>,
    retried: bool,
    renewed: bool,
    live_shown: bool,
    team_asked: Option<(i64, bool)>,
    covered: bool,
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
            master: MasterState::default(),
            master_sent: Vec::new(),
            waiting: None,
            retried: false,
            renewed: false,
            live_shown: false,
            team_asked: None,
            covered: false,
        };
        page.refresh_pool(ctx);
        page.select_master(ctx);
        page.screen.transcript = page.transcript();
        page.redraw(ctx);
        page
    }

    /// Opens the master's live terminal the first time the Conversation tab
    /// shows for a master harness that runs its own app; going back from it
    /// leaves the earlier conversation readable on this tab.
    /// Input: none. Output: the flow opening the master view, or None.
    fn live_master(&mut self) -> Option<Flow> {
        if self.screen.tab != Tab::Conversation {
            self.live_shown = false;
            return None;
        }
        let live = self.screen.setup.as_ref().is_some_and(|setup| {
            crate::harnesses::interactive::support(&setup.team.master)
                == crate::harnesses::interactive::Support::Terminal
        });
        if !live || self.live_shown {
            return None;
        }
        self.live_shown = true;
        Some(Flow::Open(Place::Master {
            id: self.screen.project.clone(),
            name: self.screen.name.clone(),
        }))
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
        if self.master_sent.len() < MASTER_SENT_MAX
            && let Some(job) = self.master.request(self.screen.tab == Tab::Conversation)
        {
            self.master_sent.push(MasterSent::of_job(&job));
            ctx.request(Request::Master(Box::new(job)));
        }
        self.screen.transcript = self.transcript();
    }

    fn transcript(&self) -> Option<Transcript> {
        let (session, turns, input) = self.master.transcript()?;
        let read = MasterRead {
            session: session.map(|session| session.phase),
            turns,
            inputs: input.map_or(&[][..], std::slice::from_ref),
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
        let lead = match &submission.action {
            MasterOwnerAction::Submit { text, .. } => {
                if self.screen.input.value().is_empty() {
                    self.screen.input.set(text);
                }
                "The master did not take that message."
            }
            MasterOwnerAction::Open { .. } | MasterOwnerAction::RegisterRcliProfile { .. } => {
                "The master could not start."
            }
            MasterOwnerAction::Cancel { .. } => "The master's turn did not stop.",
            MasterOwnerAction::Close { .. } => "The conversation did not end.",
            MasterOwnerAction::Resolve { .. } => "The master did not start resolving the update.",
        };
        let problem = master_problem(code);
        if self.waiting.is_some() {
            self.drop_waiting(ctx, &format!("{lead} {problem}"));
        } else {
            ctx.say(Message::error(lead, &problem));
        }
    }

    /// Carries on after the engine confirmed a master request: an opened master takes the
    /// waiting message, and a conversation ended for a renewal opens a new one.
    /// Takes the confirmed action.
    fn confirmed(&mut self, action: &MasterOwnerAction, ctx: &mut Ctx) {
        match action {
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
        self.view = project_screen(&mut self.screen, ctx.width, ctx.height, &look(ctx));
    }

    fn pump(&mut self, ctx: &mut Ctx) {
        if self.screen.tab == Tab::Changes
            && let Some(query) = self.screen.changes.next_query(now_ms())
        {
            ctx.request(Request::Changes(Box::new(query)));
        }
        if self.screen.tab == Tab::Settings
            && let Some(job) = self.screen.plugins.next_job()
        {
            send_settings(ctx, job);
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
            ProjectReply::Retry => {
                self.retry_master(ctx);
                Flow::Handled
            }
            ProjectReply::Team => Flow::Open(Place::Team {
                id: self.screen.project.clone(),
                name: self.screen.name.clone(),
            }),
            ProjectReply::Process => Flow::Open(Place::ProjectProcess {
                id: self.screen.project.clone(),
                name: self.screen.name.clone(),
            }),
        };
        self.sync_master(ctx);
        self.pump(ctx);
        self.say_problem(ctx);
        self.redraw(ctx);
        match flow {
            Flow::Handled => self.live_master().unwrap_or(Flow::Handled),
            other => other,
        }
    }

    fn changes_have_keys(&self) -> bool {
        self.screen.tab == Tab::Changes
            && self.screen.edit.is_none()
            && self.screen.review.is_none()
    }

    fn plugins_have_keys(&self) -> bool {
        self.screen.tab == Tab::Settings
            && self.screen.pool_picker.is_none()
            && self.screen.edit.is_none()
            && self.screen.confirm_delete.is_none()
            && self.screen.general.is_none()
            && !self.screen.confirm_remove
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
        hints_of(&self.view)
    }

    fn typing(&self) -> bool {
        self.screen.edit.is_some()
            || self.screen.adding.is_some()
            || self.screen.general.is_some()
            || (self.screen.tab == Tab::Conversation && self.screen.typing)
            || (self.changes_have_keys() && self.screen.changes.editing())
    }

    fn key(&mut self, key: &Key, ctx: &mut Ctx) -> Flow {
        self.screen.width = ctx.width;
        if self.changes_have_keys()
            && let Some(event) = key_event(key)
            && self.screen.changes.key(event, now_ms())
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
        self.covered = matches!(&flow, Flow::Open(place) if Some(place) != self.place().as_ref());
        self.ask_team(ctx);
        flow
    }

    fn step(&mut self, forward: bool, ctx: &mut Ctx) -> Flow {
        let code = if forward {
            KeyCode::Right
        } else {
            KeyCode::Left
        };
        let reply = self.screen.key(KeyEvent::new(code, KeyModifiers::SHIFT));
        self.reply(reply, ctx)
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
            Arrival::Owner(event) => match event.as_ref() {
                OwnerEvent::Setup(event) => self.setup_arrived(event, ctx),
                OwnerEvent::Master(event) => self.master_arrived(event, ctx),
                OwnerEvent::Settings(event) => self.screen.plugins.apply(event),
                OwnerEvent::Changes(event) => self.screen.changes.apply(event),
                _ => return Flow::Handled,
            },
            Arrival::PoolEdited(result) => self.assigned(result, ctx),
            Arrival::Pools(_) | Arrival::PoolProjects(_) => {}
            Arrival::Connected(_) => {
                self.select_master(ctx);
                self.master.access();
            }
            Arrival::Disconnected(_) => {
                self.sent.clear();
                self.team_asked = None;
                self.screen.forget_reads();
                self.screen.plugins.forget_reads();
                self.screen.changes.forget_reads();
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
        self.live_master().unwrap_or(Flow::Handled)
    }

    fn shown(&mut self, ctx: &mut Ctx) {
        ctx.request(Request::Pools);
        ctx.request(Request::PoolProjects);
        self.covered = false;
        self.ask_team(ctx);
        self.refresh_pool(ctx);
        self.sync_master(ctx);
        self.pump(ctx);
        self.say_problem(ctx);
        self.redraw(ctx);
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect, ctx: &Ctx) -> Option<Position> {
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

    fn busy(&self, _ctx: &Ctx) -> bool {
        self.screen.counts().0 > 0
            || self.master.has_pending()
            || self.waiting.is_some()
            || self.screen.plugins.busy()
            || self.screen.changes.busy()
    }

    fn draft(&self) -> bool {
        self.screen.edit.is_some()
            || self.screen.general.is_some()
            || !self.screen.input.value().trim().is_empty()
    }

    fn loss(&self) -> &'static str {
        if self.screen.edit.is_some() {
            "Your memory edit will be lost."
        } else if self.screen.general.is_some() {
            "Your project changes will be lost."
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
