use super::new_project::harness_name;
use super::new_project_page::{hints_of, key_event, look, paint, pool_choices_from};
use super::new_project_view::{Row, Screen as View};
use super::project::{
    MasterStatus, ProjectReply, ProjectScreen, Speaker, Tab, Transcript, plain_problem,
    project_pool,
};
use super::project_master::{MasterRead, transcript_from};
use super::project_time::local_offset;
use super::project_view::project_screen;
use super::{Flow, Place, ProjectTab, Screen, SettingsSection};
use crate::engine::owner::{OwnerResponse, OwnerResult};
use crate::master_session::{MasterBackend, MasterOwnerAction};
use crate::project_setup::SetupQuery;
use crate::protocol::{CredentialRef, ProjectId};
use crate::tui::app::UiEvent;
use crate::tui::client::{OwnerEvent, SetupEvent};
use crate::tui::clock::now_ms;
use crate::tui::ctx::{Ctx, Link};
use crate::tui::frame::{Hint, Message};
use crate::tui::keys::Key;
use crate::tui::master_session::{
    Context as MasterContext, Event as MasterEvent, Job as MasterJob, State as MasterState,
    Submission as MasterSubmission,
};
use crate::tui::seam::{Arrival, PoolChange, Request};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::text::Line;
use std::path::{Path, PathBuf};

const SENT_MAX: usize = 8;
const MASTER_SENT_MAX: usize = 4;

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

/// Opens a project folder or file with the system's default app.
/// Takes an absolute path from the engine's project record.
/// Returns the plain problem when the path is not absolute, does not exist or nothing opened it.
pub(in crate::tui) fn open_path(path: &str) -> Result<(), String> {
    let target = Path::new(path);
    if !target.is_absolute() || !target.exists() {
        return Err("That file or folder is not there any more.".into());
    }
    let opener = if cfg!(target_os = "macos") {
        "/usr/bin/open"
    } else {
        "xdg-open"
    };
    let status = std::process::Command::new(opener)
        .arg(target)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map_err(|_| "Could not open it on this computer.".to_owned())?;
    if status.success() {
        Ok(())
    } else {
        Err("Could not open it on this computer.".into())
    }
}

/// One project as a shell screen.
/// Holds the project state, its last drawn view, the engine reads it sent and its master conversation.
/// Reads go one at a time through the shell queue; only replies to its own reads are applied.
pub(in crate::tui) struct ProjectPage {
    screen: ProjectScreen,
    view: View,
    sent: Vec<SetupQuery>,
    assigning: Option<String>,
    master: MasterState,
    master_sent: Vec<MasterSent>,
    waiting: Option<Waiting>,
    retried: bool,
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
            master: MasterState::default(),
            master_sent: Vec::new(),
            waiting: None,
            retried: false,
        };
        page.refresh_pool(ctx);
        page.select_master(ctx);
        page.screen.transcript = page.transcript();
        page.redraw(ctx);
        page
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
        let session = self
            .master
            .transcript()
            .and_then(|(session, _, _)| session)
            .map(|session| (session.phase, session.current_turn.is_some()));
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
        if let (Some(code), Some(text)) = (&rejected, submitted_text(event)) {
            if self.screen.input.value().is_empty() {
                self.screen.input.set(text);
            }
            ctx.say(Message::error(
                "The master did not take that message.",
                &plain_problem(code),
            ));
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
            Ok(Some(confirmed)) => {
                if matches!(confirmed.action, MasterOwnerAction::Open { .. })
                    && let Some(waiting) = self.waiting.take()
                {
                    self.retried = false;
                    if self.master.submit(&waiting.text, ctx.next_generation()) != Ok(true) {
                        self.waiting = Some(waiting);
                        self.drop_waiting(ctx, "The master started but did not take the message.");
                    }
                }
            }
            Ok(None) => self.advance_open(event, rejected.is_some() || failed, ctx),
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

    fn setup_arrived(&mut self, event: &SetupEvent) {
        let Some(index) = self.sent.iter().position(|query| query == &event.query) else {
            return;
        };
        let query = self.sent.remove(index);
        self.screen.now_ms = now_ms();
        self.screen.apply(&query, event.result.clone());
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

    fn assign(&mut self, pool: CredentialRef, ctx: &mut Ctx) {
        if self.assigning.is_some() {
            ctx.say(Message::warning(
                "Still assigning the pool.",
                "Wait a moment, then try again.",
            ));
            return;
        }
        let name = self
            .screen
            .pool_choices
            .iter()
            .find(|choice| choice.id == pool)
            .map_or_else(|| "The pool".to_owned(), |choice| choice.name.clone());
        self.assigning = Some(name);
        ctx.request(Request::EditPool {
            change: PoolChange::Assign {
                pool,
                project: self.screen.project.clone(),
            },
        });
    }

    fn assigned(&mut self, result: &Result<(), String>, ctx: &mut Ctx) {
        let Some(name) = self.assigning.take() else {
            return;
        };
        match result {
            Ok(()) => ctx.say(Message::success(
                &format!("{name} is assigned."),
                "Work can start now.",
            )),
            Err(code) => ctx.say(Message::error(
                "The pool was not assigned.",
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
            ProjectReply::Open(path) => {
                match open_path(&path) {
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
            ProjectReply::Retry => {
                self.retry_master(ctx);
                Flow::Handled
            }
        };
        self.sync_master(ctx);
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
        self.screen.edit.is_some() || (self.screen.tab == Tab::Conversation && self.screen.typing)
    }

    fn key(&mut self, key: &Key, ctx: &mut Ctx) -> Flow {
        self.screen.width = ctx.width;
        let reply = match key {
            Key::Paste(text) => self.screen.paste(text),
            other => match key_event(other) {
                Some(event) => self.screen.key(event),
                None => ProjectReply::Ignored,
            },
        };
        self.reply(reply, ctx)
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
            Arrival::Owner(event) => match event.as_ref() {
                OwnerEvent::Setup(event) => self.setup_arrived(event),
                OwnerEvent::Master(event) => self.master_arrived(event, ctx),
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
                self.screen.forget_reads();
                self.master_sent
                    .retain(|sent| matches!(sent, MasterSent::Mutation(_)));
            }
            _ => return Flow::Handled,
        }
        self.refresh_pool(ctx);
        self.sync_master(ctx);
        self.pump(ctx);
        self.redraw(ctx);
        Flow::Handled
    }

    fn shown(&mut self, ctx: &mut Ctx) {
        ctx.request(Request::Pools);
        ctx.request(Request::PoolProjects);
        self.refresh_pool(ctx);
        self.sync_master(ctx);
        self.pump(ctx);
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
        self.screen.counts().0 > 0 || self.master.has_pending() || self.waiting.is_some()
    }

    fn place(&self) -> Option<Place> {
        Some(Place::Project {
            id: self.screen.project.clone(),
            name: self.screen.name.clone(),
            tab: project_tab(self.screen.tab),
        })
    }
}
