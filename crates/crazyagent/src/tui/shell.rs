use super::client::app_setup::AppSetupEvent;
use super::client::settings::SettingsEvent;
use super::clock;
use super::ctx::{Ctx, Link, Load};
use super::frame::{
    Hint, MIN_HEIGHT, MIN_WIDTH, Message, Modal, bands, draw_lines, header, help_line, hint,
    too_small,
};
use super::keys::{Global, Key, global};
use super::screens::intake::not_ready;
use super::screens::new_project_page::kept_draft;
use super::screens::settings_accounts::settings_event;
use super::screens::startup::Startup;
use super::screens::{DRAFT_LOSS, Flow, Place, ProjectTab, Screen, SettingsSection, open};
use super::seam::{Arrival, ProjectCard};
use super::text::{cells, join, list_row, pad, styled};
use super::theme::Role;
use super::widgets::{TextField, buttons};
use crate::harnesses::HarnessStatus;
use ratatui::Frame;
use ratatui::layout::Position;
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
        };
        shell.refresh();
        shell
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

    /// Records a new window size.
    /// Inputs: width and height. Output: none.
    pub fn resize(&mut self, width: u16, height: u16) {
        self.ctx.width = width;
        self.ctx.height = height;
        let ctx = &mut self.ctx;
        self.stack
            .last_mut()
            .expect("the stack is never empty")
            .resized(ctx);
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
        let ctx = &mut self.ctx;
        self.stack
            .last_mut()
            .expect("the stack is never empty")
            .shown(ctx);
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
                    self.refresh();
                }
            }
        }
    }

    fn push(&mut self, screen: Box<dyn Screen>) {
        if self.stack.len() >= STACK_MAX {
            self.stack.remove(1.min(self.stack.len() - 1));
        }
        self.stack.push(screen);
        assert!(self.stack.len() <= STACK_MAX);
        self.refresh();
    }

    fn home(&mut self) {
        let root_is_home =
            self.stack.first().and_then(|screen| screen.place()) == Some(Place::Home);
        if root_is_home {
            self.stack.truncate(1);
            self.refresh();
        } else {
            self.stack.clear();
            let screen = open(&Place::Home, &mut self.ctx);
            self.stack.push(screen);
            self.refresh();
        }
    }

    fn go(&mut self, place: Place) {
        self.home();
        if place != Place::Home {
            let screen = open(&place, &mut self.ctx);
            self.push(screen);
        }
    }

    /// Handles one key: overlays first, then a screen in raw mode takes every
    /// key, then the global keys, then the top screen; an Esc the screen
    /// ignores goes back.
    /// Input: the key. Output: none; check `quitting` afterwards.
    pub fn key(&mut self, key: Key) {
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
            Arrival::Disconnected(problem) => {
                self.ctx.identity = None;
                let started = self
                    .stack
                    .first()
                    .and_then(|screen| screen.place())
                    .is_some();
                if started {
                    self.ctx.link = Link::Reconnecting;
                    self.ctx.say(Message::error(
                        "Lost the connection to the engine.",
                        "Retrying in 3 s. What you typed is kept.",
                    ));
                } else {
                    self.ctx.link = Link::Down(problem.clone());
                }
            }
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
                super::screens::settings_accounts::keep_rows(&arrival, &mut self.ctx);
            }
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
            self.refresh();
        }
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
            key("Ctrl-K", "Go to anything", 11),
            key("Esc", "Back or cancel", 11),
            key("Tab", "Next field or panel", 11),
            key("Shift-Tab", "Previous", 11),
            key(&format!("Shift-{arrows}"), "Next tab", 11),
            key("?", "This help", 11),
            key("Ctrl-Q", "Quit", 11),
            Line::default(),
            styled("Text fields", Role::Heading, theme),
            key(&arrows, "Move the cursor", 11),
            key("Ctrl-A E", "Start, end", 11),
            key("Ctrl-W", "Delete a word", 11),
            key("Ctrl-U", "Clear the field", 11),
            key("Ctrl-J", "New line", 11),
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
            "Letter keys work when you are not typing in a field.",
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

    /// Draws the whole frame: background, header, rule, body, message line,
    /// help line, then any modal and the cursor.
    /// Input: the ratatui frame. Output: none.
    pub fn draw(&mut self, frame: &mut Frame) {
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
        let [head, rule, body, message, help] = bands(area);
        let width = area.width as usize;
        let theme = self.ctx.theme.clone();
        let glyphs = self.ctx.glyphs;
        let top = self.stack.last().expect("the stack is never empty");
        let header_line = header(width, &top.crumbs(), top.status(&self.ctx), &theme, glyphs);
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
        let hints = self
            .overlay_hints()
            .or_else(|| screen_modal.as_ref().map(|(_, hints)| hints.clone()))
            .unwrap_or_else(|| self.top().hints(&self.ctx));
        draw_lines(
            frame.buffer_mut(),
            help,
            &[help_line(width, &hints, &theme, glyphs)],
        );
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

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::owner_profile::OwnerProfile;
    use crate::protocol::ProjectId;
    use crate::tui::glyphs::UNICODE;
    use crate::tui::seam::{Activity, ResearchCounts};
    use crate::tui::theme::{Appearance, Colors, Depth};

    fn ctx() -> Ctx {
        let mut ctx = Ctx::new(
            Depth::NoColor,
            Colors::Dark,
            Appearance::Dark,
            &UNICODE,
            String::new(),
        );
        ctx.link = Link::Ready;
        ctx
    }

    fn card(name: &str, running: usize) -> ProjectCard {
        ProjectCard {
            id: ProjectId::new(format!("project-{}", name.to_lowercase())).unwrap(),
            name: name.into(),
            about: String::new(),
            platforms: Vec::new(),
            pool: None,
            research: ResearchCounts {
                running,
                ..Default::default()
            },
            created_ms: None,
            activity: Default::default(),
        }
    }

    fn at(place: Place, ctx: Ctx) -> Shell {
        let mut ctx = ctx;
        let screen = open(&place, &mut ctx);
        Shell::with(ctx, screen)
    }

    fn drain(shell: &mut Shell) -> Vec<String> {
        std::iter::from_fn(|| shell.ctx.next_request())
            .map(|request| format!("{request:?}"))
            .collect()
    }

    #[test]
    fn go_to_ranks_prefix_then_contained_then_letters_in_order() {
        assert_eq!(rank("tide", "Tidepool"), Some(0));
        assert_eq!(rank("", "Tidepool"), Some(0));
        assert_eq!(rank("POOL", "Tidepool"), Some(1));
        assert_eq!(rank("tdp", "Tidepool"), Some(2));
        assert_eq!(rank("t p", "Tidepool"), Some(2));
        assert_eq!(rank("pt", "Tidepool"), None);
        assert_eq!(rank("xyz", "Tidepool"), None);
    }

    #[test]
    fn go_to_lists_projects_tabs_settings_and_actions_and_opens_over_home() {
        let mut ctx = ctx();
        ctx.store.projects = Load::Ready(vec![card("Tidepool", 0)]);
        let labels: Vec<String> = targets(&ctx)
            .into_iter()
            .map(|target| target.label)
            .collect();
        assert_eq!(labels[0], "Tidepool");
        assert!(labels.contains(&"Tidepool / Agents".to_owned()));
        assert!(labels.contains(&"Settings / Plugins and skills".to_owned()));
        assert!(labels.contains(&"New pool".to_owned()));

        let mut shell = at(Place::Settings(SettingsSection::Profile), ctx);
        shell.key(Key::Ctrl('k'));
        assert!(matches!(shell.overlay, Some(Overlay::GoTo(_))));
        for character in "set pools".chars() {
            shell.key(Key::Char(character));
        }
        shell.key(Key::Enter);
        assert!(shell.overlay.is_none());
        assert_eq!(shell.depth(), 2);
        assert_eq!(shell.place(), Some(Place::Settings(SettingsSection::Pools)));
        assert_eq!(shell.stack[0].place(), Some(Place::Home));
    }

    #[test]
    fn go_to_and_help_stay_closed_where_they_do_not_apply() {
        let mut shell = Shell::new(ctx());
        shell.key(Key::Ctrl('k'));
        assert!(
            shell.overlay.is_none(),
            "no Go to before the owner is set up"
        );

        let mut shell = at(Place::Welcome, ctx());
        shell.key(Key::Ctrl('k'));
        assert!(shell.overlay.is_none(), "no Go to on the first-run page");
        shell.key(Key::Char('?'));
        assert!(
            shell.overlay.is_none(),
            "? types into the focused name field"
        );

        let mut shell = at(Place::Home, ctx());
        shell.key(Key::Char('?'));
        assert!(matches!(shell.overlay, Some(Overlay::Help)));
        shell.key(Key::Char('x'));
        assert!(
            matches!(shell.overlay, Some(Overlay::Help)),
            "other keys keep it open"
        );
        shell.key(Key::Esc);
        assert!(shell.overlay.is_none());
    }

    #[test]
    fn esc_goes_back_and_home_is_the_floor() {
        let mut shell = at(Place::Home, ctx());
        shell.key(Key::Char('s'));
        assert_eq!(shell.depth(), 2);
        assert_eq!(
            shell.place(),
            Some(Place::Settings(SettingsSection::Accounts))
        );
        shell.key(Key::Esc);
        assert_eq!(shell.place(), Some(Place::Home));
        shell.key(Key::Esc);
        assert_eq!((shell.depth(), shell.place()), (1, Some(Place::Home)));
        assert!(!shell.quitting());
    }

    #[test]
    fn settings_sections_step_both_ways_and_wrap() {
        let mut shell = at(Place::Home, ctx());
        shell.key(Key::Char('s'));
        shell.key(Key::ShiftLeft);
        assert_eq!(
            shell.place(),
            Some(Place::Settings(SettingsSection::Profile))
        );
        shell.key(Key::Char(']'));
        assert_eq!(
            shell.place(),
            Some(Place::Settings(SettingsSection::Accounts))
        );
        shell.key(Key::ShiftRight);
        assert_eq!(shell.place(), Some(Place::Settings(SettingsSection::Pools)));
        assert_eq!(shell.depth(), 2, "sections replace each other");
    }

    #[test]
    fn quit_asks_only_when_agents_run_and_stay_is_the_default() {
        let mut shell = at(Place::Home, ctx());
        shell.key(Key::Ctrl('q'));
        assert!(shell.quitting(), "nothing running, so no question");

        let mut ctx = ctx();
        ctx.store.projects = Load::Ready(vec![card("Tidepool", 2), card("Ledgerline", 1)]);
        let mut shell = at(Place::Home, ctx);
        shell.key(Key::Ctrl('c'));
        match &shell.overlay {
            Some(Overlay::Quit(ask)) => assert_eq!((ask.running, ask.quit_focused), (3, false)),
            _ => panic!("quit asks first"),
        }
        shell.key(Key::Enter);
        assert!(
            !shell.quitting() && shell.overlay.is_none(),
            "Enter on Stay stays"
        );
        shell.key(Key::Ctrl('q'));
        shell.key(Key::Esc);
        assert!(!shell.quitting() && shell.overlay.is_none());
        shell.key(Key::Ctrl('q'));
        shell.key(Key::Right);
        shell.key(Key::Enter);
        assert!(shell.quitting());

        let mut ctx = self::ctx();
        ctx.store.projects = Load::Ready(vec![card("Tidepool", 1)]);
        let mut shell = at(Place::Home, ctx);
        shell.key(Key::Ctrl('q'));
        shell.key(Key::Ctrl('q'));
        assert!(shell.quitting(), "a second Ctrl-Q quits");

        let mut ctx = self::ctx();
        let mut team = card("Ledgerline", 0);
        team.activity = Activity {
            working: Some(2),
            ..Activity::default()
        };
        ctx.store.projects = Load::Ready(vec![team, card("Tidepool", 1)]);
        let mut shell = at(Place::Home, ctx);
        shell.key(Key::Ctrl('q'));
        match &shell.overlay {
            Some(Overlay::Quit(ask)) => assert_eq!(ask.running, 3),
            _ => panic!("team agents at work count as running"),
        }
    }

    #[test]
    fn loading_lines_stay_empty_for_the_first_150_ms_of_a_busy_spell() {
        let mut shell = at(Place::Settings(SettingsSection::Profile), ctx());
        assert!(shell.busy(), "the profile is still loading");
        assert!(!shell.ctx.quiet);
        let start = Instant::now();
        assert!(shell.pace(start), "becoming busy starts the quiet spell");
        assert!(shell.ctx.quiet);
        assert_eq!(shell.ctx.loading("Loading your profile"), Line::default());
        assert!(!shell.pace(start + Duration::from_millis(149)));
        assert!(shell.ctx.quiet);
        assert!(shell.pace(start + Duration::from_millis(150)));
        assert!(!shell.ctx.quiet);
        assert_eq!(
            crate::tui::text::plain(&shell.ctx.loading("Loading your profile")),
            format!("{} Loading your profile", shell.ctx.spinner())
        );
        assert!(!shell.pace(start + Duration::from_millis(400)));
        assert!(!shell.ctx.quiet, "a long load keeps its spinner");

        shell.arrive(Arrival::Profile(Ok(Some(OwnerProfile {
            name: "Mira".into(),
            role: String::new(),
            bio: String::new(),
            preferred_stack: Vec::new(),
            working_style: Default::default(),
        }))));
        assert!(!shell.busy());
        assert!(!shell.pace(start + Duration::from_millis(500)));
        assert!(!shell.ctx.quiet);
        assert_eq!(
            shell.busy_since, None,
            "the next busy spell starts quiet again"
        );
    }

    #[test]
    fn a_profile_read_failure_says_so_and_r_reads_it_again() {
        let mut shell = Shell::new(ctx());
        shell.arrive(Arrival::Connected(None));
        assert!(drain(&mut shell).contains(&"Profile".to_owned()));
        shell.arrive(Arrival::Profile(Err("owner_profile_read".into())));
        assert_eq!(
            shell.ctx.link,
            Link::Ready,
            "the engine answered, so it is not down"
        );
        assert!(matches!(shell.ctx.store.profile, Load::Failed(_)));
        assert!(!shell.busy(), "no spinner while waiting for the owner");
        assert_eq!(shell.place(), None);
        shell.key(Key::Char('r'));
        assert_eq!(drain(&mut shell), vec!["Profile".to_owned()]);
        assert!(shell.busy());
        shell.arrive(Arrival::Profile(Ok(None)));
        assert_eq!(shell.place(), Some(Place::Welcome));
    }

    #[test]
    fn quit_on_welcome_asks_once_something_is_typed() {
        let mut shell = at(Place::Welcome, ctx());
        shell.key(Key::Ctrl('q'));
        assert!(shell.quitting(), "an empty welcome form quits at once");

        let mut shell = at(Place::Welcome, ctx());
        shell.key(Key::Char('M'));
        shell.key(Key::Ctrl('q'));
        match &shell.overlay {
            Some(Overlay::Quit(ask)) => {
                assert_eq!(ask.loss, Some("Your profile will not be saved."))
            }
            _ => panic!("typed profile asks first"),
        }
        shell.key(Key::Right);
        shell.key(Key::Enter);
        assert!(shell.quitting(), "Quit still quits");
    }

    #[test]
    fn home_asks_for_harness_readiness_until_a_reply_fills_the_store() {
        use crate::harnesses::{HarnessEntry, Search, catalog};
        use crate::tui::client::OwnerEvent;
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::Builder::new()
            .prefix("ca-hh-")
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap();
        let codex = root.path().join("codex");
        std::fs::write(&codex, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&codex, std::fs::Permissions::from_mode(0o755)).unwrap();
        let entries = catalog(&Search::new(
            vec![root.path().to_path_buf()],
            None,
            Vec::new(),
        ));
        let reply = |result: Result<Vec<HarnessEntry>, String>| {
            Arrival::Owner(Box::new(OwnerEvent::Settings(Box::new(
                SettingsEvent::Harnesses(result),
            ))))
        };

        let mut known = ctx();
        known.store.setup = Load::Ready(crate::tui::screens::home::tests::tools(&[]));
        known.store.accounts = Load::Ready(vec![crate::tui::seam::AccountRow {
            id: crate::protocol::CredentialRef::new("account-1").unwrap(),
            name: "Team key".into(),
            provider: "Anthropic API".into(),
            status: crate::accounts::AccountStatus::KeySaved.label().into(),
        }]);
        let mut shell = at(Place::Home, known);
        assert!(drain(&mut shell).contains(&"Settings".to_owned()));
        shell.arrive(reply(Err("controller_pending_intent".into())));
        assert_eq!(shell.ctx.store.harnesses, Load::NotReady);
        shell.arrive(reply(Ok(entries.clone())));
        assert_eq!(shell.ctx.store.harnesses, Load::Ready((1, entries.len())));

        shell.key(Key::Char('s'));
        drain(&mut shell);
        shell.key(Key::Esc);
        assert_eq!(shell.place(), Some(Place::Home));
        let requests = drain(&mut shell);
        assert!(requests.contains(&"Projects".to_owned()), "{requests:?}");
        assert!(
            !requests.contains(&"Settings".to_owned()),
            "known readiness is not asked for again"
        );
    }

    #[test]
    fn quit_asks_while_a_new_project_draft_is_kept_after_leaving_the_wizard() {
        let mut shell = at(Place::Home, ctx());
        shell.key(Key::Char('n'));
        assert_eq!(shell.place(), Some(Place::NewProject));
        shell.key(Key::Esc);
        shell.key(Key::Esc);
        assert_eq!(shell.place(), Some(Place::Home));
        shell.key(Key::Ctrl('q'));
        assert!(shell.quitting(), "an empty wizard keeps no draft");

        let mut shell = at(Place::Home, ctx());
        shell.key(Key::Char('n'));
        for character in "Fieldnotes".chars() {
            shell.key(Key::Char(character));
        }
        shell.key(Key::Esc);
        shell.key(Key::Esc);
        assert_eq!(shell.place(), Some(Place::Home));
        assert_eq!(kept_draft().as_deref(), Some("Fieldnotes"));
        shell.key(Key::Ctrl('q'));
        match &shell.overlay {
            Some(Overlay::Quit(ask)) => assert_eq!(ask.loss, Some(DRAFT_LOSS)),
            _ => panic!("a kept draft asks first"),
        }
        assert!(!shell.quitting());
    }

    #[test]
    fn quit_asks_before_unsaved_profile_changes_are_lost() {
        let loaded = || {
            let mut ctx = ctx();
            ctx.store.profile = Load::Ready(Some(OwnerProfile {
                name: "Mira".into(),
                role: String::new(),
                bio: String::new(),
                preferred_stack: Vec::new(),
                working_style: Default::default(),
            }));
            at(Place::Settings(SettingsSection::Profile), ctx)
        };
        let mut shell = loaded();
        shell.key(Key::Ctrl('q'));
        assert!(shell.quitting(), "an untouched profile quits at once");

        let mut shell = loaded();
        shell.key(Key::Tab);
        shell.key(Key::Char('!'));
        shell.key(Key::Ctrl('q'));
        match &shell.overlay {
            Some(Overlay::Quit(ask)) => assert_eq!(
                (ask.running, ask.loss),
                (0, Some("Your profile changes will be lost."))
            ),
            _ => panic!("edited profile asks first"),
        }
        shell.key(Key::Esc);
        assert!(!shell.quitting());
        shell.key(Key::Backspace);
        shell.key(Key::Ctrl('q'));
        assert!(shell.quitting(), "undoing the edit leaves nothing to lose");

        let mut shell = loaded();
        shell.key(Key::Tab);
        shell.key(Key::Char('!'));
        for _ in 0..7 {
            shell.key(Key::Tab);
        }
        shell.key(Key::Enter);
        assert!(drain(&mut shell).contains(&"SaveProfile".to_owned()));
        shell.key(Key::Ctrl('q'));
        assert!(
            matches!(&shell.overlay, Some(Overlay::Quit(ask)) if ask.loss.is_some()),
            "a save still in flight can be lost"
        );
        shell.key(Key::Esc);
        shell.arrive(Arrival::ProfileSaved(Ok(())));
        shell.key(Key::Ctrl('q'));
        assert!(shell.quitting(), "a saved profile has nothing to lose");
    }

    #[test]
    fn losing_the_engine_after_start_says_so_and_refreshes_on_return() {
        let mut shell = at(Place::Home, ctx());
        drain(&mut shell);
        shell.arrive(Arrival::Disconnected("trusted_connection_lost".into()));
        assert_eq!(shell.ctx.link, Link::Reconnecting);
        assert_eq!(
            shell.ctx.message().unwrap().lead,
            "Lost the connection to the engine."
        );
        shell.arrive(Arrival::Connected(None));
        assert_eq!(shell.ctx.link, Link::Ready);
        assert_eq!(
            shell.ctx.message().unwrap().lead,
            "Connected to the engine again."
        );
        let requests = drain(&mut shell);
        assert!(requests.contains(&"Projects".to_owned()), "{requests:?}");
        assert!(requests.contains(&"Pools".to_owned()), "{requests:?}");
    }

    #[test]
    fn startup_opens_welcome_on_first_run_and_home_afterwards() {
        let mut shell = Shell::new(Ctx::new(
            Depth::NoColor,
            Colors::Dark,
            Appearance::Dark,
            &UNICODE,
            String::new(),
        ));
        shell.arrive(Arrival::Disconnected("engine_unavailable".into()));
        assert_eq!(shell.ctx.link, Link::Down("engine_unavailable".into()));
        assert_eq!(shell.place(), None);
        shell.key(Key::Char('r'));
        assert!(shell.ctx.take_reconnect());
        assert_eq!(shell.ctx.link, Link::Connecting);

        shell.arrive(Arrival::Connected(None));
        assert!(drain(&mut shell).contains(&"Profile".to_owned()));
        shell.arrive(Arrival::Profile(Ok(None)));
        assert_eq!(shell.place(), Some(Place::Welcome));
        assert_eq!(shell.depth(), 1);

        let mut shell = Shell::new(ctx());
        shell.arrive(Arrival::Connected(None));
        let profile = OwnerProfile {
            name: "Mira".into(),
            role: String::new(),
            bio: String::new(),
            preferred_stack: Vec::new(),
            working_style: Default::default(),
        };
        shell.arrive(Arrival::Profile(Ok(Some(profile))));
        assert_eq!(shell.place(), Some(Place::Home));
        assert!(matches!(shell.ctx.store.profile, Load::Ready(Some(_))));
        assert!(drain(&mut shell).contains(&"Projects".to_owned()));
    }
}
