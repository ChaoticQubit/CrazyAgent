use super::client::OwnerEvent;
use super::client::app_setup::AppSetupEvent;
use super::client::changes::{ChangesQuery, ChangesView};
use super::client::intake::{IntakeEvent, IntakeQuery, IntakeView};
use super::client::learning::{LearningEvent, LearningQuery, LearningReply};
use super::client::questions::{QuestionsEvent, QuestionsQuery, QuestionsReply};
use super::client::settings::{SettingsEvent, SettingsJob};
use super::clock;
use super::ctx::{Ctx, Link, Load};
use super::frame::{
    HINT_ROWS_MAX, Hint, MIN_HEIGHT, MIN_WIDTH, Message, Modal, bands, draw_lines, header, hint,
    strip_bidi, too_small, wrapped_footer,
};
use super::glyphs::Glyphs;
use super::keys::{Global, Key, global};
use super::screens::intake::not_ready;
use super::screens::new_project_page::kept_draft;
use super::screens::settings::restore;
use super::screens::settings_accounts::settings_event;
use super::screens::startup::Startup;
use super::screens::usage::extras::{ResetWatch, reset_notice};
use super::screens::{DRAFT_LOSS, Flow, Place, ProjectTab, Screen, SettingsSection, open};
use super::screens::{intake, intake_job};
use super::seam::{Arrival, ProjectCard, Request, account_labels};
use super::text::{cells, cut, join, list_row, pad, styled};
use super::theme::{Role, Theme};
use super::widgets::{TextField, buttons};
use crate::harnesses::HarnessStatus;
use crate::learning::MarkStep;
use crate::projects_home::job::{JobKind, JobState, JobView};
use crate::terminal::TerminalFrame;
use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::Block;
use std::time::{Duration, Instant};

const STACK_MAX: usize = 32;
const GO_TO_ROWS_MAX: usize = 8;
const GO_TO_QUERY_MAX: usize = 100;
const HELP_WIDTH: u16 = 74;
const GO_TO_WIDTH: u16 = 58;
const GO_TO_TOP: u16 = 5;
const QUIT_WIDTH: u16 = 46;
const QUIET: Duration = Duration::from_millis(150);
const JOB_NOTE_CELLS_MAX: usize = 64;

/// One Go to destination.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    pub label: String,
    pub kind: &'static str,
    pub place: Place,
}

/// Lists every Go to destination: projects and their tabs, Settings sections
/// and actions.
/// Input: the context. Output: targets in display order.
pub fn targets(ctx: &Ctx) -> Vec<Target> {
    let mut out = Vec::new();
    for card in ctx.store.projects.ready().into_iter().flatten() {
        for tab in ProjectTab::ALL {
            out.push(Target {
                label: if tab == ProjectTab::Overview {
                    card.name.clone()
                } else {
                    format!("{} / {}", card.name, tab.label())
                },
                kind: if tab == ProjectTab::Overview {
                    "Project"
                } else {
                    "Project tab"
                },
                place: Place::Project {
                    id: card.id.clone(),
                    name: card.name.clone(),
                    tab,
                },
            });
        }
    }
    out.push(Target {
        label: "Home".into(),
        kind: "Place",
        place: Place::Home,
    });
    out.push(Target {
        label: "Usage".into(),
        kind: "Place",
        place: Place::Usage,
    });
    for section in SettingsSection::ALL {
        out.push(Target {
            label: format!("Settings / {}", section.label()),
            kind: "Settings",
            place: Place::Settings(section),
        });
    }
    out.push(Target {
        label: "Settings / Setup".into(),
        kind: "Settings",
        place: Place::Setup { first_run: false },
    });
    out.push(Target {
        label: "Settings / GitHub".into(),
        kind: "Settings",
        place: Place::GitHubDefaults,
    });
    for (label, place) in [
        ("New project", Place::NewProject),
        ("Add account", Place::AddAccount),
        ("New pool", Place::NewPool),
    ] {
        out.push(Target {
            label: label.into(),
            kind: "Action",
            place,
        });
    }
    out
}

/// Fuzzy-matches a Go to query against a label.
/// Inputs: the query and the label. Output: a rank, lower is better: prefix,
/// then contained, then letters in order; None when it does not match.
pub fn rank(query: &str, label: &str) -> Option<u8> {
    let query = query.trim().to_lowercase();
    let label = label.to_lowercase();
    if query.is_empty() || label.starts_with(&query) {
        return Some(0);
    }
    if label.contains(&query) {
        return Some(1);
    }
    let mut letters = label.chars();
    query
        .chars()
        .filter(|character| !character.is_whitespace())
        .all(|wanted| letters.any(|character| character == wanted))
        .then_some(2)
}

struct GoTo {
    query: TextField,
    selected: usize,
}

impl GoTo {
    fn matches(&self, ctx: &Ctx) -> Vec<Target> {
        let mut ranked: Vec<(u8, usize, Target)> = targets(ctx)
            .into_iter()
            .enumerate()
            .filter_map(|(index, target)| {
                rank(self.query.value(), &target.label).map(|rank| (rank, index, target))
            })
            .collect();
        ranked.sort_by_key(|(rank, index, _)| (*rank, *index));
        ranked
            .into_iter()
            .take(GO_TO_ROWS_MAX)
            .map(|(_, _, target)| target)
            .collect()
    }
}

struct QuitAsk {
    quit_focused: bool,
    running: usize,
    loss: Option<&'static str>,
}

enum Overlay {
    Help,
    GoTo(GoTo),
    Quit(QuitAsk),
}

/// The TUI shell: a stack of screens inside one fixed frame, the shared
/// overlays (Keys, Go to, Quit) and the routing of keys and engine arrivals.
/// Input: keys, pastes, resizes and arrivals from the run loop. Output: drawn
/// frames and engine requests queued on `ctx`.
pub struct Shell {
    pub ctx: Ctx,
    stack: Vec<Box<dyn Screen>>,
    overlay: Option<Overlay>,
    quit: bool,
    busy_since: Option<Instant>,
    resets: ResetWatch,
}

impl Shell {
    /// A shell showing the startup screen.
    /// Input: the context. Output: the shell.
    pub fn new(ctx: Ctx) -> Self {
        Self::with(ctx, Box::new(Startup::new()))
    }

    /// A shell with one given screen at the bottom of the stack.
    /// Inputs: the context and the screen. Output: the shell.
    pub fn with(ctx: Ctx, screen: Box<dyn Screen>) -> Self {
        let mut shell = Self {
            ctx,
            stack: vec![screen],
            overlay: None,
            quit: false,
            busy_since: None,
            resets: ResetWatch::default(),
        };
        shell.refresh();
        shell
    }

    /// Delivers the busy reply, code `owner_busy`, of each request the full request queue
    /// refused during the last event, so the screen that asked stops waiting on it and says
    /// so. Replies to requests refused while these are delivered wait for the next event.
    /// Input: none. Output: none.
    fn answer_refused(&mut self) {
        for arrival in self.ctx.take_refused() {
            self.take_arrival(arrival);
        }
    }

    /// Says whether the owner asked to quit.
    /// Input: none. Output: true once the loop should end.
    pub fn quitting(&self) -> bool {
        self.quit
    }

    /// The place of the top screen.
    /// Input: none. Output: the place, None for screens without one.
    #[cfg(test)]
    pub fn place(&self) -> Option<Place> {
        self.top().place()
    }

    /// Number of screens on the stack.
    /// Input: none. Output: the depth.
    #[cfg(test)]
    pub fn depth(&self) -> usize {
        self.stack.len()
    }

    fn top(&self) -> &dyn Screen {
        self.stack
            .last()
            .expect("the stack is never empty")
            .as_ref()
    }

    /// Records a new window size, capped to the area the TUI draws, so screens size agent
    /// terminals for what is shown and drawing never resizes them again.
    /// Inputs: width and height. Output: none.
    pub fn resize(&mut self, width: u16, height: u16) {
        let area = super::bounded_area(width, height);
        self.ctx.width = area.width;
        self.ctx.height = area.height;
        let ctx = &mut self.ctx;
        self.stack
            .last_mut()
            .expect("the stack is never empty")
            .resized(ctx);
        self.answer_refused();
    }

    /// True while something on screen animates, so the loop ticks.
    /// Input: none. Output: whether a spinner shows.
    pub fn busy(&self) -> bool {
        self.top().busy(&self.ctx)
    }

    /// Tracks how long the top screen has been busy, so loading lines stay
    /// empty for the first 150 ms and quick replies never flash a spinner.
    /// Input: the current time. Output: true when the quiet state changed and
    /// the frame needs drawing.
    pub fn pace(&mut self, now: Instant) -> bool {
        self.busy_since = match (self.busy(), self.busy_since) {
            (true, Some(since)) => Some(since),
            (true, None) => Some(now),
            (false, _) => None,
        };
        let quiet = self
            .busy_since
            .is_some_and(|since| now.saturating_duration_since(since) < QUIET);
        let changed = quiet != self.ctx.quiet;
        self.ctx.quiet = quiet;
        assert!(!quiet || self.busy());
        changed
    }

    /// Asks the top screen to refresh its data.
    /// Input: none. Output: none; requests are queued on the context.
    pub fn refresh(&mut self) {
        self.show();
        self.answer_refused();
    }

    /// Asks the top screen to refresh its data inside an event, which answers the requests
    /// the full queue refuses once it ends. Input: none. Output: none.
    fn show(&mut self) {
        if self.ctx.link == Link::Ready {
            self.ctx
                .request(Request::Questions(Box::new(QuestionsQuery::Counts)));
        }
        let ctx = &mut self.ctx;
        self.stack
            .last_mut()
            .expect("the stack is never empty")
            .shown(ctx);
        for step in visit_marks(self.top().place()) {
            self.ctx.learn(*step);
        }
    }

    /// Runs the periodic refresh the run loop makes every few seconds: asks the engine how
    /// many change sets wait for the owner, in all and in the top screen's project, reads the
    /// stored extras for the new free reset notice once a minute, then refreshes the top
    /// screen.
    /// Input: none. Output: none; requests are queued on the context.
    pub fn tick(&mut self) {
        let project = self.top().project();
        self.ctx
            .request(Request::Changes(Box::new(ChangesQuery::Waiting {
                project,
            })));
        intake::ask(&mut self.ctx, IntakeQuery::JobStatus);
        self.watch_resets();
        self.refresh();
    }

    /// Reads the engine's intake job again between ticks while the stored one runs, so a
    /// New Project page that follows its job sees the end soon, as the desktop's one-second
    /// read does. Input: none. Output: whether it asked.
    pub fn read_running_job(&mut self) -> bool {
        let running = self
            .ctx
            .store
            .intake
            .as_ref()
            .is_some_and(intake_job::running);
        if running {
            intake::ask(&mut self.ctx, IntakeQuery::JobStatus);
        }
        running
    }

    /// Keeps the engine's intake job in the store from a status read or a start, says once on
    /// the message line when a running job ended and reads the found folders again so Home
    /// lists the new one, and the projects too after a project move, whose folder changed; a
    /// stop or a dismiss reads the status at once. A failed read keeps the last view. Input:
    /// the intake reply. Output: none.
    fn intake_job(&mut self, event: &IntakeEvent) {
        let view = match (&event.query, &event.result) {
            (IntakeQuery::JobStatus | IntakeQuery::JobStart { .. }, Ok(IntakeView::Job(view))) => {
                view
            }
            (IntakeQuery::JobStop { .. } | IntakeQuery::JobDismiss { .. }, _) => {
                intake::ask(&mut self.ctx, IntakeQuery::JobStatus);
                return;
            }
            _ => return,
        };
        let said = intake_job::end_line(self.ctx.store.intake.as_ref(), view.as_ref());
        self.ctx.store.intake = view.clone();
        let Some(text) = said else {
            return;
        };
        let landed = view.as_ref().map(|job| &job.state) == Some(&JobState::Done);
        self.ctx.say(if landed {
            Message::success(&text, "")
        } else {
            Message::warning(&text, "")
        });
        intake::ask(&mut self.ctx, IntakeQuery::Found);
        let kind = view.as_ref().map(|job| job.kind);
        if matches!(
            kind,
            Some(JobKind::MoveProject | JobKind::Delete | JobKind::Setup)
        ) {
            self.ctx.request(Request::Projects);
        }
        assert!(self.ctx.store.intake.is_some());
    }

    /// Asks for the stored extras, never the detail rows, once the watch is due, with the
    /// account names when none are loaded yet. Input: none. Output: none.
    fn watch_resets(&mut self) {
        if self.ctx.link != Link::Ready || !self.resets.due(clock::now_ms()) {
            return;
        }
        self.ctx
            .request(Request::Settings(Box::new(SettingsJob::Extras {
                detail: false,
            })));
        if self.ctx.store.accounts.ready().is_none() {
            self.ctx
                .request(Request::Settings(Box::new(SettingsJob::AllAccounts)));
        }
    }

    /// Says once when an account gains a free reset, from any extras read. Input: the read.
    /// Output: none.
    fn new_resets(&mut self, rows: &[crate::engine::owner::AccountExtras]) {
        let send = self.resets.look(rows, clock::now_ms());
        let labels = account_labels(
            self.ctx
                .store
                .accounts
                .ready()
                .map(Vec::as_slice)
                .unwrap_or(&[]),
        );
        if let Some(notice) = reset_notice(&send, &labels) {
            self.ctx
                .say(Message::success(&notice, "See it in Settings, Pools."));
        }
    }

    fn running(&self) -> usize {
        self.ctx
            .store
            .projects
            .ready()
            .map_or(0, |cards| cards.iter().map(ProjectCard::working).sum())
    }

    fn loss(&self) -> Option<&'static str> {
        self.stack
            .iter()
            .rev()
            .find(|screen| screen.draft())
            .map(|screen| screen.loss())
            .or_else(|| kept_draft().map(|_| DRAFT_LOSS))
    }

    fn ask_quit(&mut self) {
        let running = self.running();
        let loss = self.loss();
        if running == 0 && loss.is_none() {
            self.quit = true;
            return;
        }
        self.overlay = Some(Overlay::Quit(QuitAsk {
            quit_focused: false,
            running,
            loss,
        }));
    }

    fn go_to_allowed(&self) -> bool {
        self.stack
            .first()
            .and_then(|screen| screen.place())
            .is_some_and(|place| place != Place::Welcome)
    }

    fn apply(&mut self, flow: Flow) {
        match flow {
            Flow::Handled | Flow::Ignored => {}
            Flow::Open(place) => {
                if self.top().place().as_ref() != Some(&place) {
                    let screen = open(&place, &mut self.ctx);
                    self.push(screen);
                }
            }
            Flow::Replace(screen) => {
                self.stack.pop();
                self.push(screen);
            }
            Flow::Back => {
                if self.stack.len() > 1 {
                    self.stack.pop();
                    self.show();
                }
            }
            Flow::Go(place) => self.go(place),
        }
    }

    fn push(&mut self, screen: Box<dyn Screen>) {
        if self.stack.len() >= STACK_MAX {
            self.stack.remove(1.min(self.stack.len() - 1));
        }
        self.stack.push(screen);
        assert!(self.stack.len() <= STACK_MAX);
        self.show();
    }

    fn home(&mut self) {
        let root_is_home =
            self.stack.first().and_then(|screen| screen.place()) == Some(Place::Home);
        if root_is_home {
            self.stack.truncate(1);
            self.show();
        } else {
            self.stack.clear();
            let screen = open(&Place::Home, &mut self.ctx);
            self.stack.push(screen);
            self.show();
        }
    }

    fn go(&mut self, place: Place) {
        self.home();
        if place != Place::Home {
            let screen = open(&place, &mut self.ctx);
            self.push(screen);
        }
    }

    /// Handles one key: below the smallest window only Ctrl+Q or Ctrl+C acts
    /// ([`Self::small_key`]); otherwise overlays first, then a screen in raw mode
    /// takes every key, then the global keys, then the top screen; an Esc the
    /// screen ignores goes back.
    /// Input: the key. Output: none; check `quitting` afterwards.
    pub fn key(&mut self, key: Key) {
        self.take_key(key);
        self.answer_refused();
    }

    fn take_key(&mut self, key: Key) {
        if self.ctx.width < MIN_WIDTH || self.ctx.height < MIN_HEIGHT {
            return self.small_key(&key);
        }
        if let Some(overlay) = self.overlay.take() {
            self.overlay_key(overlay, &key);
            return;
        }
        if self.top().raw() {
            let ctx = &mut self.ctx;
            let flow = self
                .stack
                .last_mut()
                .expect("the stack is never empty")
                .key(&key, ctx);
            return self.apply(flow);
        }
        let modal = self.top().modal(&self.ctx).is_some();
        let typing = self.top().typing();
        match global(&key, typing) {
            Some(Global::Quit) => return self.ask_quit(),
            Some(Global::GoTo) if !modal && self.go_to_allowed() => {
                self.overlay = Some(Overlay::GoTo(GoTo {
                    query: TextField::new("Go to", GO_TO_QUERY_MAX),
                    selected: 0,
                }));
                return;
            }
            Some(Global::Help) if !modal => {
                self.overlay = Some(Overlay::Help);
                return;
            }
            Some(Global::PreviousSection | Global::NextSection) if !modal => {
                let forward = global(&key, typing) == Some(Global::NextSection);
                let ctx = &mut self.ctx;
                let flow = self
                    .stack
                    .last_mut()
                    .expect("the stack is never empty")
                    .step(forward, ctx);
                return self.apply(flow);
            }
            _ => {}
        }
        let ctx = &mut self.ctx;
        let flow = self
            .stack
            .last_mut()
            .expect("the stack is never empty")
            .key(&key, ctx);
        match flow {
            Flow::Ignored if key == Key::Esc => self.apply(Flow::Back),
            other => self.apply(other),
        }
    }

    /// The one key a window below [`MIN_WIDTH`] x [`MIN_HEIGHT`] acts on, where the shell
    /// draws only the bigger-window notice and no screen or overlay: Ctrl+Q or Ctrl+C quits
    /// as it does elsewhere, past a quit question already open, so no screen, overlay or raw
    /// terminal takes a key it could not show (DS-99 r2). Input: the key. Output: none.
    fn small_key(&mut self, key: &Key) {
        if global(key, false) != Some(Global::Quit) {
            return;
        }
        match self.overlay.take() {
            Some(Overlay::Quit(_)) => self.quit = true,
            _ => self.ask_quit(),
        }
    }

    fn overlay_key(&mut self, overlay: Overlay, key: &Key) {
        match overlay {
            Overlay::Help => {
                if !matches!(key, Key::Esc | Key::Char('?') | Key::Enter) {
                    if matches!(key, Key::Ctrl('q' | 'c')) {
                        self.ask_quit();
                        return;
                    }
                    self.overlay = Some(Overlay::Help);
                }
            }
            Overlay::Quit(mut ask) => match key {
                Key::Esc => {}
                Key::Left | Key::Right | Key::Tab | Key::BackTab => {
                    ask.quit_focused = !ask.quit_focused;
                    self.overlay = Some(Overlay::Quit(ask));
                }
                Key::Enter => self.quit = ask.quit_focused,
                Key::Ctrl('q' | 'c') => self.quit = true,
                _ => self.overlay = Some(Overlay::Quit(ask)),
            },
            Overlay::GoTo(mut go) => match key {
                Key::Esc | Key::Ctrl('k') => {}
                Key::Ctrl('q' | 'c') => self.ask_quit(),
                Key::Up => {
                    go.selected = go.selected.saturating_sub(1);
                    self.overlay = Some(Overlay::GoTo(go));
                }
                Key::Down => {
                    let count = go.matches(&self.ctx).len();
                    go.selected = (go.selected + 1).min(count.saturating_sub(1));
                    self.overlay = Some(Overlay::GoTo(go));
                }
                Key::Enter => match go.matches(&self.ctx).into_iter().nth(go.selected) {
                    Some(target) => self.go(target.place),
                    None => self.overlay = Some(Overlay::GoTo(go)),
                },
                other => {
                    if go.query.key(other) {
                        go.selected = 0;
                    }
                    self.overlay = Some(Overlay::GoTo(go));
                }
            },
        }
    }

    /// Takes one engine arrival: updates shared data, then lets every screen
    /// react, top first; only the top screen may change the stack.
    /// Input: the arrival. Output: none.
    pub fn arrive(&mut self, arrival: Arrival) {
        self.take_arrival(arrival);
        self.answer_refused();
    }

    fn take_arrival(&mut self, arrival: Arrival) {
        let was_ready = self.ctx.link == Link::Ready;
        match &arrival {
            Arrival::Connected(identity) => {
                if self.ctx.link == Link::Reconnecting {
                    self.ctx
                        .say(Message::success("Connected to the engine again.", ""));
                }
                self.ctx.link = Link::Ready;
                self.ctx.identity = identity.clone();
            }
            Arrival::Disconnected(problem) => self.lose_engine(problem),
            Arrival::Profile(result) => {
                self.ctx.store.profile = match result {
                    Ok(profile) => Load::Ready(profile.clone()),
                    Err(problem) => Load::Failed(problem.clone()),
                }
            }
            Arrival::Pools(result) => {
                self.ctx.store.pools = match result {
                    Ok(pools) => Load::Ready(pools.clone()),
                    Err(problem) => Load::Failed(problem.clone()),
                }
            }
            Arrival::PoolProjects(result) => {
                self.ctx.store.pool_projects = match result {
                    Ok(scopes) => Load::Ready(scopes.clone()),
                    Err(problem) => Load::Failed(problem.clone()),
                }
            }
            Arrival::Projects(result) => match result {
                Ok(cards) => {
                    self.ctx.store.projects = Load::Ready(cards.clone());
                    self.ctx.store.projects_seen =
                        Some(clock::clock(clock::now_ms(), clock::local_offset()));
                }
                Err(problem) if self.ctx.store.projects.ready().is_none() => {
                    self.ctx.store.projects = Load::Failed(problem.clone());
                }
                Err(_) => {}
            },
            Arrival::Owner(_) => {
                if let Some(event) = intake::event(&arrival) {
                    self.intake_job(event);
                }
                if let Some(SettingsEvent::Harnesses(Ok(entries))) = settings_event(&arrival) {
                    let ready = entries
                        .iter()
                        .filter(|entry| entry.status == HarnessStatus::Ready)
                        .count();
                    self.ctx.store.harnesses = Load::Ready((ready, entries.len()));
                }
                if let Some(SettingsEvent::AppSetup(AppSetupEvent::Status(result))) =
                    settings_event(&arrival)
                {
                    let store = &mut self.ctx.store;
                    match result {
                        Ok(status) => store.setup = Load::Ready(status.clone()),
                        Err(_) if store.setup.ready().is_some() => {}
                        Err(code) if not_ready(code) => store.setup = Load::NotReady,
                        Err(code) => store.setup = Load::Failed(code.clone()),
                    }
                }
                if let Arrival::Owner(event) = &arrival
                    && let super::client::OwnerEvent::Settled(outcome) = event.as_ref()
                {
                    self.ctx.say(match outcome {
                        Ok(()) => Message::success("Your earlier change is saved.", ""),
                        Err(code) => Message::error(
                            "Your earlier change was not saved.",
                            super::screens::settings_accounts::plain(code),
                        ),
                    });
                }
                if let Arrival::Owner(event) = &arrival
                    && let super::client::OwnerEvent::Reveal(event) = event.as_ref()
                {
                    self.ctx.say(event.message());
                }
                if let Arrival::Owner(event) = &arrival
                    && let super::client::OwnerEvent::Changes(event) = event.as_ref()
                    && let ChangesQuery::Waiting { project } = &event.query
                {
                    self.ctx.store.waiting = match &event.result {
                        Ok(ChangesView::Waiting(count)) => Some((project.clone(), *count)),
                        _ => None,
                    };
                }
                super::screens::settings_accounts::keep_rows(&arrival, &mut self.ctx);
                if let Some(SettingsEvent::Extras(Ok(rows))) = settings_event(&arrival) {
                    self.new_resets(rows);
                }
                if let Arrival::Owner(event) = &arrival
                    && let OwnerEvent::Questions(event) = event.as_ref()
                {
                    self.questions(event);
                }
                if let Arrival::Owner(event) = &arrival
                    && let OwnerEvent::Learning(event) = event.as_ref()
                {
                    self.learning(event);
                }
            }
            Arrival::Terminal {
                frame: TerminalFrame::Attached { .. },
                ..
            } => self.ctx.learn(MarkStep::Terminal),
            _ => {}
        }
        super::screens::new_project_page::carry_background(&arrival, &mut self.ctx);
        let count = self.stack.len();
        let mut top_flow = Flow::Handled;
        for index in (0..count).rev() {
            let ctx = &mut self.ctx;
            let flow = self.stack[index].arrive(&arrival, ctx);
            if index == count - 1 {
                top_flow = flow;
            }
        }
        self.apply(top_flow);
        if !was_ready && self.ctx.link == Link::Ready && matches!(arrival, Arrival::Connected(_)) {
            self.show();
        }
    }

    /// Takes a lost or refused connection: before the first connect, or once a restore
    /// brought the 2.4 data back, the engine-down screen shows it, with the restore's
    /// sentence and `r` as the desktop's engine-down view (L5 F1); otherwise the TUI retries
    /// every 3 s, saying a restore that still runs in its sentence. Input: the problem code.
    /// Output: none.
    fn lose_engine(&mut self, problem: &str) {
        self.ctx.identity = None;
        let started = self
            .stack
            .first()
            .and_then(|screen| screen.place())
            .is_some();
        if started && problem == crate::restore::ENGINE_RESTORED {
            self.overlay = None;
            self.stack.clear();
            self.stack.push(Box::new(Startup::new()));
            if let Some(said) = self.ctx.message().cloned() {
                self.ctx.unsay(&said);
            }
            assert_eq!(self.stack.len(), 1);
        }
        if !started || problem == crate::restore::ENGINE_RESTORED {
            self.ctx.link = Link::Down(problem.to_owned());
            return;
        }
        self.ctx.link = Link::Reconnecting;
        if restore::holds(problem) {
            self.ctx.say(Message::error(restore::plain(problem), ""));
        } else {
            self.ctx.say(Message::error(
                "Lost the connection to the engine.",
                "Retrying in 3 s. What you typed is kept.",
            ));
        }
        assert!(self.ctx.message().is_some());
    }

    /// Keeps the learning read for Home, keeping the last good read over a failed one, and
    /// answers a mark: a set mark reads the steps again, a refused one is sent again on the
    /// next visit. Input: the learning reply. Output: none.
    fn learning(&mut self, event: &LearningEvent) {
        if let LearningQuery::Mark { step, .. } = &event.query {
            match &event.result {
                Ok(_) => self
                    .ctx
                    .request(Request::Learning(Box::new(LearningQuery::Progress))),
                Err(_) => self.ctx.unlearn(*step),
            }
            return;
        }
        let learning = &mut self.ctx.store.learning;
        match &event.result {
            Ok(LearningReply::Progress(progress)) => *learning = Load::Ready(progress.clone()),
            Ok(LearningReply::Marked) => unreachable!("a learning read answers with the steps"),
            Err(_) if learning.ready().is_some() => {}
            Err(code) if not_ready(code) => *learning = Load::NotReady,
            Err(code) => *learning = Load::Failed(code.clone()),
        }
    }

    /// Keeps the pending counts for the footer badge and says once when a project has a new
    /// question; the first read only fills the badge. A settled answer or dismissal reads
    /// the counts again. Input: one Questions reply. Output: none.
    fn questions(&mut self, event: &QuestionsEvent) {
        match (&event.query, &event.result) {
            (QuestionsQuery::Counts, Ok(QuestionsReply::Counts(counts))) => {
                if let Some(before) = self.ctx.store.questions.ready()
                    && let Some((project, _)) = counts.iter().find(|(project, count)| {
                        let was = before
                            .iter()
                            .find(|(seen, _)| seen == project)
                            .map_or(0, |(_, count)| *count);
                        *count > was
                    })
                {
                    let name = self
                        .ctx
                        .store
                        .projects
                        .ready()
                        .and_then(|cards| cards.iter().find(|card| card.id == *project))
                        .map_or_else(|| "a project".to_owned(), |card| card.name.clone());
                    self.ctx.say(Message::warning(
                        &format!("New question in {name}."),
                        "Answer it on the project's Questions tab.",
                    ));
                }
                self.ctx.store.questions = Load::Ready(counts.clone());
            }
            (QuestionsQuery::Counts, Err(code)) if self.ctx.store.questions.ready().is_none() => {
                self.ctx.store.questions = Load::Failed(code.clone());
            }
            (QuestionsQuery::Answer { .. } | QuestionsQuery::Dismiss { .. }, Ok(_)) => {
                self.ctx
                    .request(Request::Questions(Box::new(QuestionsQuery::Counts)));
            }
            _ => {}
        }
    }

    /// The hints the help bar shows now: an overlay's, else the top screen's modal's, else
    /// the top screen's own. Input: none. Output: the hints in order.
    fn bar_hints(&self) -> Vec<Hint> {
        self.overlay_hints()
            .or_else(|| self.top().modal(&self.ctx).map(|(_, hints)| hints))
            .unwrap_or_else(|| self.top().hints(&self.ctx))
    }

    fn overlay_hints(&self) -> Option<Vec<Hint>> {
        let arrows = format!("{}{}", self.ctx.glyphs.left, self.ctx.glyphs.right);
        let vertical = format!("{}{}", self.ctx.glyphs.up, self.ctx.glyphs.down);
        match &self.overlay {
            Some(Overlay::Help) => Some(vec![hint("Esc", "Close"), hint("?", "Close")]),
            Some(Overlay::GoTo(_)) => Some(vec![
                hint(&vertical, "Move"),
                hint("Enter", "Go"),
                hint("Esc", "Close"),
            ]),
            Some(Overlay::Quit(_)) => Some(vec![
                hint(&arrows, "Choose"),
                hint("Enter", "Confirm"),
                hint("Esc", "Stay"),
            ]),
            None => None,
        }
    }

    fn help_modal(&self) -> Modal {
        let ctx = &self.ctx;
        let theme = &ctx.theme;
        let glyphs = ctx.glyphs;
        let key = |name: &str, label: &str, width: usize| {
            join([
                pad(styled(name.to_owned(), Role::Muted, theme), width, glyphs),
                Line::raw(label.to_owned()),
            ])
        };
        let arrows = format!("{}{}", glyphs.left, glyphs.right);
        let vertical = format!("{}{}", glyphs.up, glyphs.down);
        let left = vec![
            styled("Everywhere", Role::Heading, theme),
            key("Ctrl-K", "Go to anything", 13),
            key("Esc", "Back or cancel", 13),
            key("Tab", "Next field or panel", 13),
            key("Shift-Tab", "Previous", 13),
            key(&format!("Shift-{arrows}"), "Next tab", 13),
            key("?", "This help", 13),
            key("Ctrl-Q", "Quit", 13),
            Line::default(),
            styled("Text fields", Role::Heading, theme),
            key(&arrows, "Move the cursor", 13),
            key("Ctrl-A E", "Start, end", 13),
            key("Ctrl-W", "Delete a word", 13),
            key("Ctrl-U", "Clear the field", 13),
            key("Ctrl-J", "New line", 13),
            key("Shift-Enter", "New line, if supported", 13),
        ];
        let mut right = vec![
            styled("Lists", Role::Heading, theme),
            key(&vertical, "Move", 10),
            key("PgUp PgDn", "Page", 10),
            key("Home End", "First, last", 10),
            key("/", "Filter", 10),
            key("Space", "Select", 10),
            key("Enter", "Open", 10),
            Line::default(),
            Line::default(),
            styled("This screen", Role::Heading, theme),
        ];
        for item in self.top().keys(ctx) {
            right.push(key(&item.key, &item.label, 10));
        }
        let rows = left.len().max(right.len());
        let mut body: Vec<Line<'static>> = (0..rows)
            .map(|index| {
                join([
                    pad(left.get(index).cloned().unwrap_or_default(), 36, glyphs),
                    pad(right.get(index).cloned().unwrap_or_default(), 32, glyphs),
                ])
            })
            .collect();
        body.push(Line::default());
        body.push(styled(
            "Letter keys work outside fields. In tmux, use Ctrl-J for a new line.",
            Role::Muted,
            theme,
        ));
        Modal {
            width: HELP_WIDTH,
            title: "Keys".into(),
            body,
            top: None,
        }
    }

    fn go_to_modal(&self, go: &GoTo) -> (Modal, Position) {
        let ctx = &self.ctx;
        let theme = &ctx.theme;
        let mut body = vec![Line::from(vec![
            Span::styled(">", theme.accent),
            Span::raw(format!(" {}", go.query.value())),
        ])];
        body.push(Line::default());
        let matches = go.matches(ctx);
        if matches.is_empty() {
            body.push(Line::raw("  Nothing matches."));
        }
        for (index, target) in matches.iter().enumerate() {
            body.push(list_row(
                cells(
                    &[32, 14],
                    vec![
                        Line::raw(target.label.clone()),
                        styled(target.kind, Role::Muted, theme),
                    ],
                    2,
                    ctx.glyphs,
                ),
                50,
                index == go.selected,
                true,
                theme,
                ctx.glyphs,
            ));
        }
        body.push(Line::default());
        body.push(styled(
            "Projects, tabs, settings, actions. Enter goes there.",
            Role::Muted,
            theme,
        ));
        let column = 2 + super::text::width(go.query.value()) as u16;
        (
            Modal {
                width: GO_TO_WIDTH,
                title: "Go to".into(),
                body,
                top: Some(GO_TO_TOP),
            },
            Position::new(column, 0),
        )
    }

    fn quit_modal(&self, ask: &QuitAsk) -> Modal {
        let mut body = Vec::new();
        if ask.running > 0 {
            body.push(Line::raw(format!(
                "{} agent{} keep{} running in the background.",
                ask.running,
                if ask.running == 1 { "" } else { "s" },
                if ask.running == 1 { "s" } else { "" },
            )));
        }
        if let Some(loss) = ask.loss {
            body.push(Line::raw(loss));
        }
        body.push(Line::default());
        body.push(buttons(
            &["Quit", "Stay"],
            Some(if ask.quit_focused { 0 } else { 1 }),
            &self.ctx.theme,
        ));
        Modal {
            width: QUIT_WIDTH,
            title: "Quit CrazyAgent?".into(),
            body,
            top: None,
        }
    }

    /// Draws the whole frame, then removes direction-changing characters from every cell.
    /// Input: the ratatui frame. Output: none.
    pub fn draw(&mut self, frame: &mut Frame) {
        self.paint(frame);
        strip_bidi(frame.buffer_mut());
    }

    /// Paints background, header, rule, body, message line, help line, then any modal and
    /// the cursor. Input: the ratatui frame. Output: none.
    fn paint(&mut self, frame: &mut Frame) {
        let area = frame.area();
        self.ctx.width = area.width;
        self.ctx.height = area.height;
        if let Some(paint) = self.ctx.theme.paint {
            frame.render_widget(Block::new().style(paint), area);
        }
        if area.width < MIN_WIDTH || area.height < MIN_HEIGHT {
            too_small(frame.buffer_mut(), area, &self.ctx.theme);
            return;
        }
        let width = area.width as usize;
        let theme = self.ctx.theme.clone();
        let glyphs = self.ctx.glyphs;
        let pending = self.ctx.store.pending();
        let rows_max = if self.top().wraps_hints() {
            HINT_ROWS_MAX
        } else {
            1
        };
        let bar_rows =
            wrapped_footer(width, &self.bar_hints(), pending, &theme, glyphs, rows_max).len();
        self.ctx.hint_rows = u16::try_from(bar_rows).expect("the bar has at most three rows");
        let [head, rule, body, message, help] = bands(area, self.ctx.hint_rows);
        let top = self.stack.last().expect("the stack is never empty");
        let waiting = waiting_text(self.ctx.store.waiting.as_ref(), top.project().as_ref());
        let status = with_waiting(waiting, top.status(&self.ctx), &theme);
        let status = with_intake(self.ctx.store.intake.as_ref(), status, &theme, glyphs);
        let header_line = header(width, &top.crumbs(), status, &theme, glyphs);
        draw_lines(frame.buffer_mut(), head, &[header_line]);
        draw_lines(
            frame.buffer_mut(),
            rule,
            &[Line::from(Span::styled(
                glyphs.hline.repeat(width),
                theme.muted,
            ))],
        );
        let ctx = &self.ctx;
        let cursor = self
            .stack
            .last_mut()
            .expect("the stack is never empty")
            .draw(frame, body, ctx);
        if let Some(note) = self.ctx.message() {
            draw_lines(frame.buffer_mut(), message, &[note.line(&theme, glyphs)]);
        }
        let screen_modal = self.top().modal(&self.ctx);
        let bar = wrapped_footer(width, &self.bar_hints(), pending, &theme, glyphs, bar_rows);
        let shown = u16::try_from(bar.len()).expect("the bar has at most three rows");
        let help = Rect::new(help.x, help.y + help.height - shown, help.width, shown);
        draw_lines(frame.buffer_mut(), help, &bar);
        let mut cursor = if screen_modal.is_some() || self.overlay.is_some() {
            None
        } else {
            cursor
        };
        if let Some((modal, _)) = &screen_modal {
            modal.draw(frame.buffer_mut(), area, &theme, glyphs);
        }
        match &self.overlay {
            Some(Overlay::Help) => {
                self.help_modal()
                    .draw(frame.buffer_mut(), area, &theme, glyphs);
            }
            Some(Overlay::GoTo(go)) => {
                let (modal, at) = self.go_to_modal(go);
                let inner = modal.draw(frame.buffer_mut(), area, &theme, glyphs);
                cursor = Some(Position::new(inner.x + at.x, inner.y + at.y));
            }
            Some(Overlay::Quit(ask)) => {
                self.quit_modal(ask)
                    .draw(frame.buffer_mut(), area, &theme, glyphs);
            }
            None => {}
        }
        if let Some(cursor) = cursor
            && cursor.x < area.width
            && cursor.y < area.height
        {
            frame.set_cursor_position(cursor);
        }
    }
}

/// The learning marks opening a place sets: Usage shows the owner's usage and metrics, the
/// app's or a project's Process page the process. Input: the top screen's place. Output:
/// the marks, none for other places.
fn visit_marks(place: Option<Place>) -> &'static [MarkStep] {
    match place {
        Some(Place::Usage) => &[MarkStep::Metrics, MarkStep::Usage],
        Some(Place::Settings(SettingsSection::Process) | Place::ProjectProcess { .. }) => {
            &[MarkStep::Process]
        }
        _ => &[],
    }
}

/// The header status with the count of change sets that wait for the owner in front of it.
/// Inputs: the count sentence, the top screen's own status and the theme.
/// Output: the status line; no sentence leaves the screen's status as it is.
fn with_waiting(waiting: Option<String>, status: Line<'static>, theme: &Theme) -> Line<'static> {
    let Some(text) = waiting else {
        return status;
    };
    let note = Line::from(Span::styled(text, theme.warning));
    if status.width() == 0 {
        return note;
    }
    join([note, Line::raw("  "), status])
}

/// The header status with the running intake job's brief in front, cut to a bounded width so
/// a long folder name never hides the crumbs, so a folder being added shows on every screen;
/// with no running job the status is unchanged. Inputs: the job, if any, the status, the
/// theme and the glyphs. Output: the line.
fn with_intake(
    job: Option<&JobView>,
    status: Line<'static>,
    theme: &Theme,
    glyphs: &Glyphs,
) -> Line<'static> {
    let Some(job) = job.filter(|job| intake_job::running(job)) else {
        return status;
    };
    let brief = intake_job::brief(job, glyphs.ellipsis);
    let text = cut(&brief, JOB_NOTE_CELLS_MAX, glyphs.ellipsis);
    assert!(super::text::width(&text) <= JOB_NOTE_CELLS_MAX);
    let note = Line::from(Span::styled(text, theme.accent));
    if status.width() == 0 {
        return note;
    }
    join([note, Line::raw("  "), status])
}

/// The header's count of change sets that wait for the owner. On a project's screens, once
/// the count was read for that project, it says how many wait in its Changes and how many
/// in other projects; elsewhere it counts every project. Inputs: the last count with the
/// project it was read for, and the top screen's project. Output: the sentence, or None
/// when nothing waits.
fn waiting_text(
    waiting: Option<&(
        Option<crate::protocol::ProjectId>,
        crate::landing::WaitingChanges,
    )>,
    top: Option<&crate::protocol::ProjectId>,
) -> Option<String> {
    let (asked, count) = waiting?;
    let Some(here) = count.here.filter(|_| asked.as_ref() == top) else {
        return waiting_sets(count.all, "in your projects");
    };
    let others = count.all.checked_sub(here)?;
    match (waiting_sets(here, "in Changes"), others) {
        (None, _) => waiting_sets(others, "in other projects"),
        (Some(text), 0) => Some(text),
        (Some(text), others) => Some(format!("{text}, {others} in other projects")),
    }
}

/// Counts change sets in one sentence. Inputs: the count and where they wait.
/// Output: "1 change set waits <place>" or "N change sets wait <place>"; None for zero.
fn waiting_sets(count: u32, place: &str) -> Option<String> {
    match count {
        0 => None,
        1 => Some(format!("1 change set waits {place}")),
        count => Some(format!("{count} change sets wait {place}")),
    }
}

#[cfg(all(test, unix))]
mod tests;
