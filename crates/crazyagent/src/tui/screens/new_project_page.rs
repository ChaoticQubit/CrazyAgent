use super::new_project::{HarnessChoice, HarnessState, Phase, PoolChoice, Reply, Wizard};
use super::new_project_view::{Look, Row, Screen as View, wizard_screen};
use super::project::plain_problem;
use super::project_page::ProjectPage;
use super::settings_accounts::{plain, settings_event};
use super::settings_harnesses::{control_sentence, control_word};
use super::{Flow, Place, ProjectTab, Screen, intake, intake_job};
use crate::engine::owner::random_id;
use crate::harnesses::{HarnessEntry, HarnessStatus, TeamUse};
use crate::pools::{MemberKind, PoolSummary};
use crate::project_setup::{ProjectSetup, SetupQuery, SetupView};
use crate::projects_home::AddMode;
use crate::projects_home::job::{JobKind, JobState, JobSubject, JobView};
use crate::protocol::{CredentialRef, ProjectId, ProjectSpec, RequestId};
use crate::sandbox::policy::AgentAccess;
use crate::tui::client::OwnerEvent;
use crate::tui::client::intake::{IntakeEvent, IntakeQuery, IntakeView, start_id};
use crate::tui::client::settings::{SettingsEvent, SettingsJob};
use crate::tui::ctx::{Ctx, Load};
use crate::tui::frame::{Hint, Message, draw_lines, hint};
use crate::tui::keys::Key;
use crate::tui::seam::{Arrival, PoolChange, Request};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::text::Line;
use std::cell::RefCell;
use std::collections::VecDeque;
use std::path::PathBuf;

/// The most New project drafts kept at once. A creation refused in the background past it
/// keeps no draft and its refusal says so; a wizard left open over it still keeps its own,
/// so the list holds at most this many plus the wizards open at that moment.
const DRAFTS_MAX: usize = 8;

thread_local! {
    static KEPT: RefCell<VecDeque<Wizard>> = const { RefCell::new(VecDeque::new()) };
    static RUNNING: RefCell<Vec<NewProjectPage>> = const { RefCell::new(Vec::new()) };
}

/// Carries New project creations whose page the shell closed, such as after Run in
/// background and then Home, through the engine's New Project job and the pool assignment.
/// Takes every shell arrival and the context; every page reads each job read, only one page
/// takes the reply to its own start, and one waiting assignment takes a pool edit reply.
/// Returns nothing; a made project is announced and assigned, a refused one keeps its draft.
pub(in crate::tui) fn carry_background(arrival: &Arrival, ctx: &mut Ctx) {
    if !matches!(arrival, Arrival::Owner(_) | Arrival::PoolEdited(_)) {
        return;
    }
    let mut running = RUNNING.take();
    if running.is_empty() {
        return;
    }
    for page in &mut running {
        if page.carry(arrival, ctx) {
            break;
        }
    }
    let (settled, waiting): (Vec<_>, Vec<_>) = running.into_iter().partition(|page| {
        page.creation
            .as_ref()
            .is_none_or(|creation| creation.stage == Stage::Finished)
    });
    RUNNING.with_borrow_mut(|running| running.extend(waiting));
    drop(settled);
}

/// Names the kept New project draft the wizard opens next, the oldest one kept.
/// Takes nothing; drafts live on the TUI thread until the wizard opens them again.
/// Returns the typed name, "Untitled project" for a draft without one, or none.
pub(in crate::tui) fn kept_draft() -> Option<String> {
    KEPT.with_borrow(|kept| {
        kept.front().map(|wizard| {
            let name = wizard.name.value().trim();
            if name.is_empty() {
                "Untitled project".to_owned()
            } else {
                name.to_owned()
            }
        })
    })
}

/// Gives the wizard and project views the shell's colors and glyph set.
/// Takes the shell context.
/// Returns the look with one style per design role.
pub(in crate::tui) fn look(ctx: &Ctx) -> Look {
    let theme = &ctx.theme;
    Look {
        fg: theme.text,
        heading: theme.heading,
        muted: theme.muted,
        accent: theme.accent,
        mark: theme.mark,
        success: theme.success,
        warning: theme.warning,
        error: theme.error,
        running: theme.running,
        selection: theme.selection,
        selection_dim: theme.selection_dim,
        backdrop: theme.backdrop,
        ascii: ctx.glyphs.ascii,
    }
}

/// Turns one shell key into the key event the wizard and project views take.
/// Takes the normalized shell key.
/// Returns the event, or none for a paste, which goes to `paste` instead.
pub(in crate::tui) fn key_event(key: &Key) -> Option<KeyEvent> {
    let plain = |code| KeyEvent::new(code, KeyModifiers::NONE);
    let shifted = |code| KeyEvent::new(code, KeyModifiers::SHIFT);
    Some(match key {
        Key::Char(character) => plain(KeyCode::Char(*character)),
        Key::Ctrl(letter) => KeyEvent::new(KeyCode::Char(*letter), KeyModifiers::CONTROL),
        Key::Enter => plain(KeyCode::Enter),
        Key::Esc => plain(KeyCode::Esc),
        Key::Tab => plain(KeyCode::Tab),
        Key::BackTab => shifted(KeyCode::BackTab),
        Key::Backspace => plain(KeyCode::Backspace),
        Key::Delete => plain(KeyCode::Delete),
        Key::Left => plain(KeyCode::Left),
        Key::Right => plain(KeyCode::Right),
        Key::Up => plain(KeyCode::Up),
        Key::Down => plain(KeyCode::Down),
        Key::Home => plain(KeyCode::Home),
        Key::End => plain(KeyCode::End),
        Key::PageUp => plain(KeyCode::PageUp),
        Key::PageDown => plain(KeyCode::PageDown),
        Key::ShiftLeft => shifted(KeyCode::Left),
        Key::ShiftRight => shifted(KeyCode::Right),
        Key::Paste(_) => return None,
    })
}

/// Draws a rendered view body into the shell's body area.
/// Takes the frame, the body area and the view.
/// Returns the cursor position on screen when a field has focus inside the area.
pub(in crate::tui) fn paint(frame: &mut Frame, area: Rect, view: &View) -> Option<Position> {
    let lines: Vec<Line<'static>> = view.rows.iter().cloned().map(Row::line).collect();
    draw_lines(frame.buffer_mut(), area, &lines);
    view.cursor
        .filter(|(x, y)| *x < area.width && *y < area.height)
        .map(|(x, y)| Position::new(area.x + x, area.y + y))
}

/// Turns the view's help keys into shell hints.
/// Takes the view.
/// Returns the hints in the view's priority order.
pub(in crate::tui) fn hints_of(view: &View) -> Vec<Hint> {
    view.help
        .iter()
        .map(|(key, label)| hint(key, label))
        .collect()
}

/// Builds the wizard's pool choices from the engine's pool list.
/// Takes the pool summaries; ChatGPT accounts run on Codex and Claude accounts on Claude, other accounts have no harness count.
/// Returns one choice per pool in list order.
pub(in crate::tui) fn pool_choices_from(pools: &[PoolSummary]) -> Vec<PoolChoice> {
    pools
        .iter()
        .map(|pool| {
            let mut harnesses: Vec<(String, usize)> = Vec::new();
            for member in &pool.members {
                let harness = match member.kind {
                    MemberKind::Chatgpt => "codex",
                    MemberKind::Claude => "claude",
                    _ => continue,
                };
                match harnesses.iter_mut().find(|(known, _)| known == harness) {
                    Some((_, count)) => *count += 1,
                    None => harnesses.push((harness.to_owned(), 1)),
                }
            }
            PoolChoice {
                id: pool.id.clone(),
                name: pool.name.clone(),
                accounts: pool.members.len(),
                harnesses,
            }
        })
        .collect()
}

/// Builds the Team step's harness choices from the engine's harness catalog.
/// Takes the catalog entries in owner priority order. Returns one choice per entry with its
/// state, CantRun when the tool resolver can't start it, CantStart when it cannot run team
/// agents, MasterOnly when it runs only as the project master, and the limited-control
/// sentence, if any.
pub(in crate::tui) fn harness_choices_from(entries: &[HarnessEntry]) -> Vec<HarnessChoice> {
    entries
        .iter()
        .map(|entry| HarnessChoice {
            id: entry.id.clone(),
            name: entry.name.clone(),
            state: match (&entry.status, &entry.team) {
                (HarnessStatus::NotInstalled, _) => HarnessState::NotInstalled,
                (HarnessStatus::CantRun { .. }, _) => HarnessState::CantRun,
                (_, TeamUse::Unavailable { .. }) => HarnessState::CantStart,
                (_, TeamUse::MasterOnly { .. }) => HarnessState::MasterOnly,
                (_, TeamUse::Available) => HarnessState::Ready,
            },
            limited: (control_word(entry) == "Limited control").then(|| control_sentence(entry)),
        })
        .collect()
}

/// The New project wizard as a shell screen.
/// Holds the wizard, its last drawn view and the catalog read it waits for.
/// Engine reads go through the shell queue; creation runs as the engine's New Project job.
pub(in crate::tui) struct NewProjectPage {
    wizard: Wizard,
    view: View,
    creation: Option<Creation>,
    checking: bool,
    asked: bool,
}

/// One New Project this page started as the engine's job (DEC-177 step 3b): the start's
/// id, the project it makes, its name, the pool and agent access to set once it is made,
/// and where it stands.
struct Creation {
    id: RequestId,
    project_id: ProjectId,
    name: String,
    pool: Option<CredentialRef>,
    access: AgentAccess,
    stage: Stage,
}

/// Where one New Project stands: its start is out; its start failed or went unanswered, so
/// the next job read decides it with the code kept; the engine runs it; it is made and its
/// pool is being assigned; it is made and its follow-ups are due; it is finished.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Stage {
    Starting,
    Checking(String),
    Following,
    Assigning,
    Made,
    Finished,
}

impl Creation {
    /// Whether an intake reply answers this creation's own start. Takes the reply. Returns
    /// true only for the reply to its start while that start is out.
    fn started_by(&self, event: &IntakeEvent) -> bool {
        self.stage == Stage::Starting
            && matches!(&event.query, IntakeQuery::JobStart { id, .. } if *id == self.id)
    }

    /// How the engine's job ended for this creation, read from the job the shell keeps. A
    /// job read comes over the wire, so a state a New Project never ends in, a project that
    /// is not this one or a job that is gone reads as unconfirmed. Takes the job read.
    /// Returns none while it runs, else Ok for a made project or the code it ended with.
    fn ended(&self, job: Option<&JobView>) -> Option<Result<(), String>> {
        let unknown = || Err("creation_outcome_unknown".to_owned());
        let Some(job) = job.filter(|job| job.id == self.id) else {
            return Some(unknown());
        };
        if intake_job::running(job) {
            return None;
        }
        let ours = job.kind == JobKind::Setup && job.project_id.as_ref() == Some(&self.project_id);
        let ended = match &job.state {
            JobState::Done if ours => Ok(()),
            JobState::Failed { code, .. } if ours && !code.is_empty() => Err(code.clone()),
            JobState::Cancelled { .. } if ours => Err("home_add_cancelled".to_owned()),
            _ => unknown(),
        };
        assert!(ended.as_ref().err().is_none_or(|code| !code.is_empty()));
        Some(ended)
    }
}

impl NewProjectPage {
    /// Opens the oldest kept draft, or an empty wizard with the pools the shell already has.
    /// Takes the shell context for pools, the home folder and the size.
    /// Returns the screen, drawn once so the header and help line are current.
    pub fn new(ctx: &Ctx) -> Self {
        let pools = ctx.store.pools.ready();
        let wizard = match KEPT.with_borrow_mut(VecDeque::pop_front) {
            Some(mut wizard) => {
                wizard.resume();
                if let Some(pools) = pools {
                    wizard.set_pools(pool_choices_from(pools));
                }
                wizard
            }
            None => Wizard::new(
                pools
                    .map(|pools| pool_choices_from(pools))
                    .unwrap_or_default(),
                ctx.home.as_ref().map(PathBuf::from),
            ),
        };
        let mut page = Self {
            wizard,
            view: View::default(),
            creation: None,
            checking: false,
            asked: false,
        };
        page.redraw(ctx);
        page
    }

    fn ask_harnesses(&mut self, asked: bool, ctx: &mut Ctx) {
        self.asked |= asked;
        if !self.checking {
            self.checking = true;
            ctx.request(Request::Settings(Box::new(SettingsJob::Harnesses)));
        }
    }

    fn harnesses_arrived(&mut self, result: &Result<Vec<HarnessEntry>, String>, ctx: &mut Ctx) {
        let asked = std::mem::take(&mut self.asked);
        self.checking = false;
        match result {
            Ok(entries) => {
                let choices = harness_choices_from(entries);
                let ready = choices
                    .iter()
                    .filter(|choice| choice.state == HarnessState::Ready)
                    .count();
                ctx.store.harnesses = Load::Ready((ready, choices.len()));
                self.wizard.set_harnesses(choices);
                if asked {
                    ctx.say(Message::success("Harnesses checked.", ""));
                }
            }
            Err(code) if asked => {
                ctx.say(Message::error("Could not check harnesses.", plain(code)));
            }
            Err(_) => {}
        }
    }

    fn redraw(&mut self, ctx: &Ctx) {
        self.wizard.width = ctx.width;
        self.view = wizard_screen(&self.wizard, ctx.width, ctx.height, &look(ctx));
    }

    fn ask_catalog(&mut self, ctx: &mut Ctx) {
        if let Some((pool, research)) = self.wizard.catalog_request() {
            ctx.request(Request::Setup(SetupQuery::Catalog { pool, research }));
        }
    }

    fn reply(&mut self, reply: Reply, ctx: &mut Ctx) -> Flow {
        let flow = match reply {
            Reply::Ignored => Flow::Ignored,
            Reply::Handled => Flow::Handled,
            Reply::Leave => Flow::Back,
            Reply::Background => Flow::Open(Place::Home),
            Reply::CreatePool => Flow::Open(Place::NewPool),
            Reply::CheckHarnesses => {
                self.ask_harnesses(true, ctx);
                Flow::Handled
            }
            Reply::Create(request) => {
                let (spec, setup) = *request;
                self.create(spec, setup, ctx);
                Flow::Handled
            }
        };
        self.ask_catalog(ctx);
        self.redraw(ctx);
        flow
    }

    /// Starts the engine's New Project job for the answers under a fresh id and a new project
    /// id, as a move with no overrides (DEC-177 step 3b); the reply and the job reads arrive
    /// later. Takes the spec, the setup and the context. Returns nothing; a start that could
    /// not be made says why in the wizard.
    fn create(&mut self, spec: ProjectSpec, setup: ProjectSetup, ctx: &mut Ctx) {
        assert!(self.creation.is_none());
        let project_id = random_id()
            .ok()
            .and_then(|id| ProjectId::new(format!("project-{id}")).ok());
        let Some(project_id) = project_id else {
            self.wizard.failed("creation_not_ready");
            return;
        };
        let id = match start_id() {
            Ok(id) => id,
            Err(code) => {
                self.wizard.failed(&code);
                return;
            }
        };
        let name = spec.name.clone();
        let pool = setup.pool.clone();
        intake::ask(
            ctx,
            IntakeQuery::JobStart {
                id: id.clone(),
                subject: JobSubject::Setup {
                    project_id: project_id.clone(),
                    spec,
                    setup,
                },
                mode: AddMode::Move,
                overrides: Vec::new(),
            },
        );
        self.creation = Some(Creation {
            id,
            project_id,
            name,
            pool,
            access: self.wizard.access,
            stage: Stage::Starting,
        });
    }

    /// Takes one intake reply for this page's New Project: the reply to its start, which
    /// follows the job it started or, failed or unanswered, reads the job before saying why,
    /// since the start may have reached the engine; then each read of the job the shell keeps.
    /// A start is never sent twice. Takes the reply and the context. Returns the flow.
    fn intake_arrived(&mut self, event: &IntakeEvent, ctx: &mut Ctx) -> Flow {
        let Some(creation) = self.creation.as_mut() else {
            return Flow::Handled;
        };
        if creation.started_by(event) {
            creation.stage = match &event.result {
                Ok(IntakeView::Job(Some(job))) if job.id == creation.id => Stage::Following,
                Ok(_) => Stage::Checking("creation_outcome_unknown".to_owned()),
                Err(code) => Stage::Checking(code.clone()),
            };
            if matches!(creation.stage, Stage::Checking(_)) {
                intake::ask(ctx, IntakeQuery::JobStatus);
            }
        } else if let Stage::Checking(code) = &creation.stage
            && event.query == IntakeQuery::JobStatus
            && matches!(event.result, Ok(IntakeView::Job(_)))
        {
            let code = code.clone();
            let found = ctx
                .store
                .intake
                .as_ref()
                .is_some_and(|job| job.id == creation.id);
            if !found {
                self.creation = None;
                self.wizard.failed(&code);
                return Flow::Handled;
            }
            creation.stage = Stage::Following;
        }
        if creation.stage == Stage::Following {
            return self.follow(ctx);
        }
        Flow::Handled
    }

    /// Follows this page's New Project in the job the shell keeps, by its id. Once it ended,
    /// the page takes it over and dismisses it before anything else, so the empty read after
    /// that dismiss is never a lost job; a made project then gets its pool and agent access
    /// and opens, any other end keeps the draft with its code. Takes the context. Returns the
    /// flow.
    fn follow(&mut self, ctx: &mut Ctx) -> Flow {
        let creation = self
            .creation
            .as_mut()
            .expect("only a started creation follows its job");
        assert_eq!(creation.stage, Stage::Following);
        let job = ctx.store.intake.as_ref();
        let Some(ended) = creation.ended(job) else {
            return Flow::Handled;
        };
        let seen = job.is_some_and(|job| job.id == creation.id);
        creation.stage = Stage::Made;
        if seen {
            intake::ask(
                ctx,
                IntakeQuery::JobDismiss {
                    id: creation.id.clone(),
                },
            );
        }
        if let Err(code) = ended {
            self.creation = None;
            self.wizard.failed(&code);
            return Flow::Handled;
        }
        self.wizard.created();
        ctx.request(Request::Projects);
        let Some(pool) = creation.pool.clone() else {
            return self.finish(ctx);
        };
        creation.stage = Stage::Assigning;
        ctx.request(Request::EditPool {
            change: PoolChange::Assign {
                pool: Some(pool),
                project: creation.project_id.clone(),
            },
        });
        Flow::Handled
    }

    fn assigned(&mut self, result: &Result<(), String>, ctx: &mut Ctx) -> Flow {
        let Some(creation) = self
            .creation
            .as_mut()
            .filter(|creation| creation.stage == Stage::Assigning)
        else {
            return Flow::Handled;
        };
        creation.stage = Stage::Made;
        match result {
            Ok(()) => ctx.request(Request::PoolProjects),
            Err(code) => ctx.say(Message::error(
                "The project is made, but its pool is not assigned.",
                &format!(
                    "{} Open the project's Settings tab to assign it.",
                    plain_problem(code)
                ),
            )),
        }
        self.finish(ctx)
    }

    fn finish(&mut self, ctx: &mut Ctx) -> Flow {
        let creation = self
            .creation
            .as_mut()
            .expect("only a made project opens its project");
        assert!(matches!(creation.stage, Stage::Made | Stage::Finished));
        let first = creation.stage == Stage::Made;
        if first {
            ctx.say(Message::success(
                &format!("{} is ready.", creation.name),
                "",
            ));
        }
        let saves_access = first && creation.access != AgentAccess::Sandboxed;
        creation.stage = Stage::Finished;
        let mut page = ProjectPage::new(
            creation.project_id.clone(),
            creation.name.clone(),
            ProjectTab::Overview,
            ctx,
        );
        if saves_access {
            page.save_created_access(creation.access, ctx);
        }
        Flow::Replace(Box::new(page))
    }

    /// Carries one arrival for a page the shell closed. Takes the arrival and the context.
    /// Returns true only when the arrival was this page's alone: the reply to its own start
    /// or the pool edit its assignment waits for. A job read is every page's, so it returns
    /// false for one.
    fn carry(&mut self, arrival: &Arrival, ctx: &mut Ctx) -> bool {
        let Some(creation) = &self.creation else {
            return false;
        };
        if let Some(event) = intake::event(arrival) {
            let own = creation.started_by(event);
            self.intake_arrived(event, ctx);
            if self.creation.is_none() {
                self.refused(ctx);
            }
            return own;
        }
        match arrival {
            Arrival::PoolEdited(result) if creation.stage == Stage::Assigning => {
                self.assigned(result, ctx);
                true
            }
            _ => false,
        }
    }

    fn refused(&mut self, ctx: &mut Ctx) {
        let name = self.wizard.name.value().trim().to_owned();
        let title = match &self.wizard.phase {
            Phase::Failed { title, .. } => title.clone(),
            _ => "Could not create the project.".to_owned(),
        };
        let kept = KEPT.with_borrow(VecDeque::len);
        let rest = if kept == 0 {
            format!("{name} is kept as a draft. Open New project to fix it.")
        } else if kept < DRAFTS_MAX {
            format!("{name} is kept as a draft. New project opens your earlier drafts first.")
        } else {
            self.wizard = Wizard::new(Vec::new(), None);
            format!("{DRAFTS_MAX} drafts are already kept. Create {name} again in New project.")
        };
        ctx.say(Message::error(&title, &rest));
    }

    fn set_pools(&mut self, pools: &[PoolSummary]) {
        let first = self.wizard.pools.is_empty() && self.wizard.pool.is_none();
        self.wizard.set_pools(pool_choices_from(pools));
        if first {
            self.wizard.pool = self.wizard.pools.first().map(|pool| pool.id.clone());
        }
    }
}

impl Drop for NewProjectPage {
    fn drop(&mut self) {
        if self
            .creation
            .as_ref()
            .is_some_and(|creation| creation.stage != Stage::Finished)
        {
            let mut page = Some(Self {
                wizard: std::mem::replace(&mut self.wizard, Wizard::new(Vec::new(), None)),
                view: View::default(),
                creation: self.creation.take(),
                checking: false,
                asked: false,
            });
            let _ = RUNNING.try_with(|running| {
                if let Some(page) = page.take() {
                    running.borrow_mut().push(page);
                }
            });
            if let Some(creation) = page.as_mut().and_then(|page| page.creation.as_mut()) {
                creation.stage = Stage::Finished;
            }
            return;
        }
        if !matches!(self.wizard.phase, Phase::Creating { .. }) && self.draft() {
            let wizard = std::mem::replace(&mut self.wizard, Wizard::new(Vec::new(), None));
            KEPT.with_borrow_mut(|kept| kept.push_back(wizard));
        }
    }
}

impl Screen for NewProjectPage {
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
        self.wizard.typing || self.wizard.filtering
    }

    fn key(&mut self, key: &Key, ctx: &mut Ctx) -> Flow {
        if self
            .creation
            .as_ref()
            .is_some_and(|creation| creation.stage == Stage::Finished)
        {
            return self.finish(ctx);
        }
        self.wizard.width = ctx.width;
        let reply = match key {
            Key::Paste(text) => self.wizard.paste(text),
            other => match key_event(other) {
                Some(event) => self.wizard.key(event),
                None => Reply::Ignored,
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
        let reply = self.wizard.key(KeyEvent::new(code, KeyModifiers::SHIFT));
        self.reply(reply, ctx)
    }

    fn arrive(&mut self, arrival: &Arrival, ctx: &mut Ctx) -> Flow {
        if let Some(SettingsEvent::Harnesses(result)) = settings_event(arrival) {
            self.harnesses_arrived(result, ctx);
            self.redraw(ctx);
            return Flow::Handled;
        }
        if matches!(arrival, Arrival::Disconnected(_)) {
            self.checking = false;
            return Flow::Handled;
        }
        match arrival {
            Arrival::Pools(Ok(pools)) => self.set_pools(pools),
            Arrival::PoolEdited(result) => {
                let flow = self.assigned(result, ctx);
                self.redraw(ctx);
                return flow;
            }
            Arrival::Owner(event) => match event.as_ref() {
                OwnerEvent::Intake(event) => {
                    let flow = self.intake_arrived(event, ctx);
                    self.redraw(ctx);
                    return flow;
                }
                OwnerEvent::Setup(event) => {
                    if let SetupQuery::Catalog { pool, research } = &event.query {
                        let result = match &event.result {
                            Ok(SetupView::Catalog(catalog)) => Ok(catalog.as_ref().clone()),
                            Ok(_) => Err("project_catalog_unavailable".to_owned()),
                            Err(problem) => Err(problem.clone()),
                        };
                        self.wizard
                            .apply_catalog(pool.clone(), research.clone(), result);
                    }
                }
                _ => return Flow::Handled,
            },
            _ => return Flow::Handled,
        }
        self.ask_catalog(ctx);
        self.redraw(ctx);
        Flow::Handled
    }

    fn shown(&mut self, ctx: &mut Ctx) {
        ctx.request(Request::Pools);
        if self.wizard.harnesses.is_none() {
            self.ask_harnesses(false, ctx);
        }
        self.ask_catalog(ctx);
        self.redraw(ctx);
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect, ctx: &Ctx) -> Option<Position> {
        self.wizard.width = area.width;
        self.view = wizard_screen(
            &self.wizard,
            area.width,
            area.height.saturating_add(4),
            &look(ctx),
        );
        paint(frame, area, &self.view)
    }

    fn busy(&self, _ctx: &Ctx) -> bool {
        matches!(self.wizard.phase, Phase::Creating { .. })
    }

    fn draft(&self) -> bool {
        !self.wizard.name.value().trim().is_empty()
            || !self.wizard.about.value().trim().is_empty()
            || !self.wizard.documents.is_empty()
    }

    fn place(&self) -> Option<Place> {
        Some(Place::NewProject)
    }
}

#[cfg(test)]
mod tests;
