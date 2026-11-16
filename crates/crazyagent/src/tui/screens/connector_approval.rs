use crate::connectors::approvals::{ApprovalView, State};
use crate::tui::client::connectors::{ConnectorsEvent, ConnectorsJob};
use crate::tui::client::settings::{SettingsEvent, SettingsJob};
use crate::tui::clock::{local_offset, now_ms, stamp};
use crate::tui::ctx::{Ctx, Load};
use crate::tui::frame::{Hint, Message, draw_lines, hint};
use crate::tui::keys::Key;
use crate::tui::screens::settings::connectors::words::refusal;
use crate::tui::screens::settings_accounts::{send, settings_event};
use crate::tui::screens::{Flow, Place, Screen};
use crate::tui::seam::Arrival;
use crate::tui::text::{styled, wrap};
use crate::tui::theme::Role;
use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::text::Line;

#[cfg(all(test, unix))]
mod tests;

const GUTTER: u16 = 2;
/// The page's title (api-connectors.ts APPROVAL_PAGE.title).
pub(in crate::tui) const TITLE: &str = "Connector write";
/// The page's purpose (APPROVAL_PAGE.purpose).
pub(in crate::tui) const PURPOSE: &str =
    "An agent wants a connector to change something. Nothing is sent until you allow it.";
/// The notice over a write whose arguments hold hidden characters (APPROVAL_PAGE.hidden).
pub(in crate::tui) const HIDDEN: &str =
    "This write holds hidden or control characters; each one is shown as a \\u escape.";
/// The fold that holds the full arguments (APPROVAL_PAGE.fold).
pub(in crate::tui) const FOLD: &str = "All arguments";
/// The toast after an allow (APPROVAL_PAGE.allowed).
pub(in crate::tui) const ALLOWED: &str = "Write allowed.";
/// The toast after a deny (APPROVAL_PAGE.denied).
pub(in crate::tui) const DENIED: &str = "Write denied.";
/// The line of a write whose wait ran out (APPROVAL_PAGE.expired).
pub(in crate::tui) const EXPIRED: &str = "Expired";
/// The line an a or d says before the whole write has been on screen (rv-sec2 T13d F2).
pub(in crate::tui) const READ_FIRST: &str =
    "Read the whole write to its time left first, with Down, PgDn or End.";
/// The Scroll hint while the end of the write is still below the page (rv-sec2 T13d F2).
const READ_TO_ANSWER: &str = "Read to the end to answer";
const CLOSED: &str = "connector_approval_closed";

/// Text as the page shows it on one row with nothing taken out (rv-sec2 T13d F3): every
/// control, escape and its sequence included, and every hidden character becomes a visible
/// escape, as the desktop's displayText shows them, and line breaks and tabs become spaces.
/// Input: the text. Output: the text to show.
fn shown_text(text: &str) -> String {
    let shown = crate::memory::visible_text(text).replace(['\n', '\t'], " ");
    assert!(!shown.chars().any(char::is_control));
    shown
}

/// One agent- or vendor-supplied text as the page shows it (approvalText): each typed
/// backslash doubled, then [`shown_text`], never cut. Input: the text the engine answered.
/// Output: the text to show.
fn approval_text(text: &str) -> String {
    shown_text(&text.replace('\\', "\\\\"))
}

/// The time a waiting write has left (timeLeft), in whole seconds rounded up and never
/// below 0:00. Inputs: now and the end of the wait, in epoch milliseconds. Output: such as
/// "9:59".
pub(in crate::tui) fn time_left(now: i64, expires: i64) -> String {
    let left = expires.saturating_sub(now).max(0);
    let seconds = left / 1000 + i64::from(left % 1000 != 0);
    assert!(seconds >= 0);
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

/// Where a write stands as the page shows it (shownState): one still waiting once its wait
/// ran out reads as expired, since the engine refuses an answer from then on. Inputs: the
/// approval and now in epoch milliseconds. Output: the state to show.
pub(in crate::tui) fn shown_state(view: &ApprovalView, now: i64) -> State {
    if view.state == State::Waiting && now >= view.expires_ms {
        State::Expired
    } else {
        view.state
    }
}

/// The page's lines in order (approvalLines): connector, tool, project, agent, when it
/// asked and what it sends. Inputs: the approval and the UTC offset. Output: the lines.
pub(in crate::tui) fn approval_lines(view: &ApprovalView, offset: i64) -> Vec<String> {
    vec![
        format!("Connector: {}", approval_text(&view.title)),
        format!("Tool: {}", approval_text(&view.tool)),
        format!("Project: {}", shown_text(&view.project_name)),
        format!("Agent: {}", approval_text(&view.agent)),
        format!("Asked: {}", stamp(view.created_ms, offset)),
        format!("What it sends: {}", approval_text(&view.preview)),
    ]
}

/// Where an answered write stands (answeredLine): "Allowed <stamp>", "Denied <stamp>" or
/// "Expired". Inputs: the approval, its shown state and the UTC offset. Output: the line,
/// empty while it waits.
pub(in crate::tui) fn answered_line(view: &ApprovalView, state: State, offset: i64) -> String {
    let when = |word: &str| {
        view.answered_ms.map_or_else(
            || word.to_owned(),
            |ms| format!("{word} {}", stamp(ms, offset)),
        )
    };
    match state {
        State::Waiting => String::new(),
        State::Expired => EXPIRED.into(),
        State::Allowed => when("Allowed"),
        State::Denied => when("Denied"),
    }
}

/// The one request in flight: a read of the write or an answer to it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pending {
    Read,
    Answer { allow: bool },
}

/// The approval page the ConnectorWrite notice opens (DEC-168 addendum 32): what asks and
/// what it sends, the time left with a and d while it waits, else where it stands, and the
/// full arguments under a fold Enter opens. a and d answer only once every line down to the
/// time left has been drawn. An answer the engine took shows with no keys until the re-read
/// lands, and the end of the wait reads the write once more.
pub(in crate::tui) struct ConnectorApprovalPage {
    id: i64,
    stored: Load<ApprovalView>,
    kept: Option<ApprovalView>,
    pending: Option<Pending>,
    reread: Option<i64>,
    open: bool,
    top: usize,
    last_top: usize,
    room: usize,
    read_end: bool,
}

impl ConnectorApprovalPage {
    /// Opens the page for one write with nothing read yet. Input: the approval id. Output:
    /// the page; the read goes out when it is shown.
    pub fn new(id: i64) -> Self {
        Self {
            id,
            stored: Load::Idle,
            kept: None,
            pending: None,
            reread: None,
            open: false,
            top: 0,
            last_top: 0,
            room: 1,
            read_end: false,
        }
    }

    fn ask(&mut self, job: ConnectorsJob, ctx: &mut Ctx) {
        assert!(self.pending.is_none(), "one request at a time");
        let pending = match &job {
            ConnectorsJob::Approval { id } if *id == self.id => Pending::Read,
            ConnectorsJob::Answer { id, allow } if *id == self.id => {
                Pending::Answer { allow: *allow }
            }
            _ => panic!("the page asks only about its own write"),
        };
        self.pending = Some(pending);
        send(ctx, SettingsJob::Connectors(Box::new(job)));
    }

    fn read(&mut self, ctx: &mut Ctx) {
        if self.pending.is_some() {
            return;
        }
        if self.stored.ready().is_none() {
            self.stored = Load::Loading;
        }
        self.ask(ConnectorsJob::Approval { id: self.id }, ctx);
    }

    fn shown_view(&self) -> Option<&ApprovalView> {
        self.kept.as_ref().or_else(|| self.stored.ready())
    }

    fn waiting(&self, now: i64) -> bool {
        self.pending.is_none()
            && self.kept.is_none()
            && self
                .stored
                .ready()
                .is_some_and(|view| shown_state(view, now) == State::Waiting)
    }

    fn answerable(&self, now: i64) -> bool {
        self.waiting(now) && self.read_end
    }

    fn answer(&mut self, allow: bool, ctx: &mut Ctx) {
        if !self.waiting(now_ms()) {
            return;
        }
        if !self.read_end {
            ctx.say(Message::warning("Not yet.", READ_FIRST));
            return;
        }
        self.ask(ConnectorsJob::Answer { id: self.id, allow }, ctx);
    }

    fn ended(&self, now: i64) -> Option<i64> {
        let view = self.stored.ready()?;
        let ran_out = self.kept.is_none() && view.state == State::Waiting && now >= view.expires_ms;
        (ran_out && self.reread != Some(view.expires_ms)).then_some(view.expires_ms)
    }

    fn take(&mut self, result: &Result<ApprovalView, String>, ctx: &mut Ctx) {
        let Some(pending) = self.pending else {
            return;
        };
        if let Ok(view) = result
            && view.id != self.id
        {
            return;
        }
        self.pending = None;
        match (result, pending) {
            (Ok(view), Pending::Read) => {
                self.stored = Load::Ready(view.clone());
                self.kept = None;
            }
            (Ok(view), Pending::Answer { allow }) => {
                self.kept = Some(view.clone());
                ctx.say(Message::success(if allow { ALLOWED } else { DENIED }, ""));
                self.read(ctx);
            }
            (Err(code), Pending::Read) => match self.kept.take() {
                Some(view) => {
                    self.stored = Load::Ready(view);
                    ctx.say(Message::error(&refusal(code), ""));
                }
                None if self.stored.ready().is_some() => {
                    ctx.say(Message::error(&refusal(code), ""));
                }
                None => self.stored = Load::Failed(code.clone()),
            },
            (Err(code), Pending::Answer { .. }) => {
                ctx.say(Message::error(&refusal(code), ""));
                if code == CLOSED {
                    self.read(ctx);
                }
            }
        }
        assert!(self.kept.is_none() || self.pending == Some(Pending::Read));
    }

    /// The page's lines. Inputs: the width and the context. Output: every line, the
    /// arguments included while their fold is open, and how many of them run down to the
    /// time left or where the write stands, none while no write is shown.
    fn body(&self, width: usize, ctx: &Ctx) -> (Vec<Line<'static>>, Option<usize>) {
        let theme = &ctx.theme;
        let paint = |text: &str, role: Role| {
            wrap(text, width)
                .into_iter()
                .map(|part| styled(part, role, theme))
                .collect::<Vec<_>>()
        };
        let mut lines = vec![styled(TITLE, Role::Heading, theme)];
        lines.extend(paint(PURPOSE, Role::Muted));
        lines.push(Line::default());
        let Some(view) = self.shown_view() else {
            match &self.stored {
                Load::Failed(code) => lines.extend(paint(&refusal(code), Role::Warning)),
                _ => lines.push(styled(
                    format!("Reading the write{}", ctx.glyphs.ellipsis),
                    Role::Muted,
                    theme,
                )),
            }
            return (lines, None);
        };
        if view.hidden {
            lines.extend(paint(HIDDEN, Role::Warning));
        }
        let offset = local_offset();
        let hang = width.saturating_sub(2).max(1);
        for line in approval_lines(view, offset) {
            for (at, part) in wrap(&line, hang).into_iter().enumerate() {
                lines.push(Line::raw(if at == 0 { part } else { format!("  {part}") }));
            }
        }
        lines.push(Line::default());
        let now = now_ms();
        lines.push(match shown_state(view, now) {
            State::Waiting => Line::raw(format!("Time left: {}", time_left(now, view.expires_ms))),
            state => styled(answered_line(view, state, offset), Role::Muted, theme),
        });
        let summary = lines.len();
        lines.push(Line::default());
        let mark = if self.open {
            ctx.glyphs.down
        } else {
            ctx.glyphs.right
        };
        lines.push(styled(format!("{mark} {FOLD}"), Role::Accent, theme));
        if self.open {
            let room = width.saturating_sub(2).max(1);
            let arguments = shown_text(&view.arguments);
            lines.extend(
                wrap(&arguments, room)
                    .into_iter()
                    .map(|part| Line::raw(format!("  {part}"))),
            );
        }
        assert!(summary < lines.len());
        (lines, Some(summary))
    }
}

impl Screen for ConnectorApprovalPage {
    fn crumbs(&self) -> Vec<String> {
        vec![TITLE.into()]
    }

    fn wraps_hints(&self) -> bool {
        true
    }

    fn hints(&self, ctx: &Ctx) -> Vec<Hint> {
        match &self.stored {
            Load::Failed(_) => return vec![hint("r", "Try again"), hint("Esc", "Back")],
            Load::Ready(_) => {}
            Load::Idle | Load::Loading | Load::NotReady => return vec![hint("Esc", "Back")],
        }
        if self.pending.is_some() {
            return Vec::new();
        }
        let mut hints = Vec::new();
        if self.last_top > 0 {
            let unread = self.waiting(now_ms()) && !self.read_end;
            hints.push(hint(
                &format!("{}{}", ctx.glyphs.up, ctx.glyphs.down),
                if unread { READ_TO_ANSWER } else { "Scroll" },
            ));
        }
        if self.answerable(now_ms()) {
            hints.push(hint("a", "Allow"));
            hints.push(hint("d", "Deny"));
        }
        hints.push(hint("Enter", FOLD));
        hints.push(hint("r", "Read again"));
        hints.push(hint("Esc", "Back"));
        hints
    }

    fn key(&mut self, key: &Key, ctx: &mut Ctx) -> Flow {
        if self.stored.ready().is_none() {
            if matches!(self.stored, Load::Failed(_)) && *key == Key::Char('r') {
                self.read(ctx);
                return Flow::Handled;
            }
            return Flow::Ignored;
        }
        if self.pending.is_some() && !matches!(key, Key::Esc) {
            return Flow::Handled;
        }
        match key {
            Key::Char('a') => self.answer(true, ctx),
            Key::Char('d') => self.answer(false, ctx),
            Key::Enter => self.open = !self.open,
            Key::Up => self.top = self.top.saturating_sub(1),
            Key::Down => self.top = (self.top + 1).min(self.last_top),
            Key::PageUp => self.top = self.top.saturating_sub(self.room),
            Key::PageDown => self.top = (self.top + self.room).min(self.last_top),
            Key::Home => self.top = 0,
            Key::End => self.top = self.last_top,
            Key::Char('r') => self.read(ctx),
            _ => return Flow::Ignored,
        }
        Flow::Handled
    }

    fn arrive(&mut self, arrival: &Arrival, ctx: &mut Ctx) -> Flow {
        if let Some(SettingsEvent::Connectors(event)) = settings_event(arrival)
            && let ConnectorsEvent::Approval { id, result } = &**event
            && *id == self.id
        {
            self.take(result, ctx);
        }
        Flow::Handled
    }

    fn shown(&mut self, ctx: &mut Ctx) {
        if matches!(self.stored, Load::Idle) {
            self.read(ctx);
            return;
        }
        if self.pending.is_none()
            && let Some(ends) = self.ended(now_ms())
        {
            self.reread = Some(ends);
            self.read(ctx);
        }
    }

    fn busy(&self, _ctx: &Ctx) -> bool {
        self.pending.is_some()
            || self.kept.is_none()
                && self.stored.ready().is_some_and(|view| {
                    view.state == State::Waiting && self.reread != Some(view.expires_ms)
                })
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect, ctx: &Ctx) -> Option<Position> {
        let inner = Rect {
            x: area.x.saturating_add(GUTTER),
            width: area.width.saturating_sub(2 * GUTTER),
            ..area
        };
        let width = usize::from(inner.width).max(8);
        let room = usize::from(inner.height).max(1);
        let (lines, summary) = self.body(width, ctx);
        self.room = room;
        self.last_top = lines.len().saturating_sub(room);
        self.top = self.top.min(self.last_top);
        if let Some(summary) = summary {
            self.read_end |= self.top + room >= summary;
        }
        let shown: Vec<_> = lines.into_iter().skip(self.top).take(room).collect();
        draw_lines(frame.buffer_mut(), inner, &shown);
        None
    }

    fn place(&self) -> Option<Place> {
        Some(Place::ConnectorApproval { id: self.id })
    }
}
