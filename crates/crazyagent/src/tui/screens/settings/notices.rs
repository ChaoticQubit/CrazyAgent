use crate::notifications::{
    Banners, Delivery, NoticeAction, NoticeClass, NoticeKind, NoticeSettings, RETENTION_DAYS_MAX,
    RETENTION_DAYS_MIN,
};
use crate::tui::client::OwnerEvent;
use crate::tui::client::notifications::{NoticesEvent, NoticesQuery, NoticesReply};
use crate::tui::ctx::{Ctx, Load};
use crate::tui::frame::{Hint, Message, draw_lines, hint};
use crate::tui::keys::Key;
use crate::tui::screens::settings_accounts::plain;
use crate::tui::screens::{Flow, Place, Screen};
use crate::tui::seam::{Arrival, Request};
use crate::tui::text::{list_row, wrap};
use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::text::{Line, Span};

const GUTTER: u16 = 2;
const TITLE: &str = "Notifications";
const PURPOSE: &str = "What reaches you, and how long finished items stay.";
const SECTIONS: [(NoticeClass, &str, &str); 2] = [
    (
        NoticeClass::Action,
        "Action items",
        "Things waiting on you. They stay pending until they are dealt with, so they can be quiet but never off. On also sends a macOS notification; Bell only keeps them to the bell.",
    ),
    (
        NoticeClass::Info,
        "Info items",
        "Things that happened. On also sends a macOS notification, Bell only keeps them to the bell, and Off keeps them out of the store.",
    ),
];
const ACTION_CHOICES: [(Delivery, &str); 2] =
    [(Delivery::On, "On"), (Delivery::BellOnly, "Bell only")];
const INFO_CHOICES: [(Delivery, &str); 3] = [
    (Delivery::On, "On"),
    (Delivery::BellOnly, "Bell only"),
    (Delivery::Off, "Off"),
];
const BANNERS_TITLE: &str = "macOS notifications";
const BANNERS_ABOUT: &str =
    "Which items also show as a macOS notification. A click on one opens its page.";
const BANNERS_LABEL: &str = "Show";
const BANNER_CHOICES: [(Banners, &str); 3] = [
    (Banners::Off, "Off"),
    (Banners::ActionOnly, "Action items only"),
    (Banners::All, "All"),
];
const DAYS_TITLE: &str = "Done items";
const DAYS_ABOUT: &str = "Finished and deleted items leave the Done filter after this many days.";
const DAYS_LABEL: &str = "Keep finished items for";
const DAYS_WRONG: &str = "Enter a whole number of days from 1 to 365.";
const DAYS_DIGITS_MAX: usize = 3;
const SAVED: &str = "Notification settings saved.";
const NOT_SAVED: &str = "The notification settings were not saved.";
const NOT_READ: &str = "Couldn't load the notification settings.";
const ROWS: usize = NoticeKind::ALL.len() + 2;

const _: () = assert!(RETENTION_DAYS_MIN == 1 && RETENTION_DAYS_MAX == 365);
const _: () = assert!(RETENTION_DAYS_MAX < 1_000);

/// One line of the page the cursor can rest on: a kind's delivery, the macOS choice or
/// the days Done items stay.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Row {
    Kind(NoticeKind),
    Banners,
    Days,
}

/// The rows in page order: the Action kinds, then the Info kinds, each in
/// `NoticeKind::ALL` order, then the macOS choice and the days. Input: none. Output: the
/// rows, `ROWS` of them.
fn rows() -> Vec<Row> {
    let mut rows: Vec<Row> = SECTIONS
        .iter()
        .flat_map(|(class, _, _)| {
            NoticeKind::ALL
                .into_iter()
                .filter(move |kind| kind.class() == *class)
                .map(Row::Kind)
        })
        .collect();
    rows.extend([Row::Banners, Row::Days]);
    assert_eq!(rows.len(), ROWS);
    rows
}

/// The choices one kind offers: Action kinds are never Off. Input: the kind. Output: the
/// choices with their desktop words.
fn choices(kind: NoticeKind) -> &'static [(Delivery, &'static str)] {
    match kind.class() {
        NoticeClass::Action => &ACTION_CHOICES,
        NoticeClass::Info => &INFO_CHOICES,
    }
}

/// The choice next to `now` in `options`, wrapping at both ends; the first one when `now`
/// is not offered. Inputs: the options, the current choice and the direction. Output: it.
fn turn<T: Copy + PartialEq>(options: &[(T, &str)], now: T, forward: bool) -> T {
    assert!(!options.is_empty());
    let count = options.len();
    let Some(at) = options.iter().position(|(choice, _)| *choice == now) else {
        return options[0].0;
    };
    let next = if forward {
        (at + 1) % count
    } else {
        (at + count - 1) % count
    };
    options[next].0
}

/// Settings, Notifications (DEC-136a slice 3): each kind's delivery, the macOS
/// notifications choice and the days Done items stay, in the desktop's words. Picks stay
/// unsaved until Enter sends the whole settings as one change; Esc drops them first.
/// Reached from Go to and from s on the notifications list.
pub(in crate::tui) struct NotificationSettingsPage {
    stored: Load<NoticeSettings>,
    picks: NoticeSettings,
    days: String,
    fresh: bool,
    selected: usize,
    top: usize,
    reading: bool,
    saving: Option<NoticesQuery>,
}

impl NotificationSettingsPage {
    /// Opens the page with nothing read yet. Input: none. Output: the page; the read goes
    /// out when it is shown.
    pub fn new() -> Self {
        let picks = NoticeSettings::default();
        Self {
            days: picks.retention_days.to_string(),
            picks,
            stored: Load::Idle,
            fresh: true,
            selected: 0,
            top: 0,
            reading: false,
            saving: None,
        }
    }

    fn read(&mut self, ctx: &mut Ctx) {
        if self.reading {
            return;
        }
        if self.stored.ready().is_none() {
            self.stored = Load::Loading;
        }
        self.reading = true;
        ctx.request(Request::Notices(Box::new(NoticesQuery::Settings)));
    }

    /// Puts the picks and the typed days back to the stored settings. Input: none.
    /// Output: none.
    fn reset(&mut self) {
        if let Some(stored) = self.stored.ready() {
            self.picks = stored.clone();
            self.days = stored.retention_days.to_string();
        }
        self.fresh = true;
    }

    fn edited(&self) -> bool {
        self.stored.ready().is_some_and(|stored| {
            self.picks != *stored || self.days != stored.retention_days.to_string()
        })
    }

    /// The typed days when they are a whole number within bounds. Input: none. Output:
    /// the days, or none.
    fn typed_days(&self) -> Option<u16> {
        if self.days.is_empty() || !self.days.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        let days: u16 = self.days.parse().ok()?;
        (RETENTION_DAYS_MIN..=RETENTION_DAYS_MAX)
            .contains(&days)
            .then_some(days)
    }

    fn set_delivery(&mut self, kind: NoticeKind, delivery: Delivery) {
        assert!(kind.class() == NoticeClass::Info || delivery != Delivery::Off);
        for item in &mut self.picks.kinds {
            if item.kind == kind {
                item.delivery = delivery;
            }
        }
    }

    fn change(&mut self, forward: bool) {
        match rows()[self.selected] {
            Row::Kind(kind) => {
                let next = turn(choices(kind), self.picks.delivery(kind), forward);
                self.set_delivery(kind, next);
            }
            Row::Banners => {
                self.picks.banners = turn(&BANNER_CHOICES, self.picks.banners, forward);
            }
            Row::Days => {
                let now = self
                    .typed_days()
                    .or_else(|| self.stored.ready().map(|stored| stored.retention_days))
                    .unwrap_or(RETENTION_DAYS_MIN);
                let next = if forward {
                    now.saturating_add(1).min(RETENTION_DAYS_MAX)
                } else {
                    now.saturating_sub(1).max(RETENTION_DAYS_MIN)
                };
                self.days = next.to_string();
                self.fresh = true;
            }
        }
    }

    /// Types one key into the days: a digit replaces the shown days first, then adds to
    /// them up to three digits, and Backspace takes the last one off. Input: the key.
    /// Output: whether the days took it.
    fn type_days(&mut self, key: &Key) -> bool {
        match key {
            Key::Char(digit) if digit.is_ascii_digit() => {
                if self.fresh {
                    self.days.clear();
                    self.fresh = false;
                }
                if self.days.len() < DAYS_DIGITS_MAX {
                    self.days.push(*digit);
                }
                true
            }
            Key::Backspace => {
                self.fresh = false;
                self.days.pop();
                true
            }
            _ => false,
        }
    }

    /// Sends the picks and the typed days as one change, unless nothing changed, a save is
    /// on its way or the days are out of bounds. Input: the context. Output: none.
    fn save(&mut self, ctx: &mut Ctx) {
        let Some(stored) = self.stored.ready().cloned() else {
            return;
        };
        if !self.edited() || self.saving.is_some() {
            return;
        }
        let Some(days) = self.typed_days() else {
            ctx.say(Message::error(DAYS_WRONG, ""));
            return;
        };
        let settings = NoticeSettings {
            retention_days: days,
            ..self.picks.clone()
        };
        if settings == stored {
            self.reset();
            return;
        }
        let query = NoticesQuery::Act {
            action: NoticeAction::SaveSettings { settings },
            generation: ctx.next_generation(),
        };
        self.saving = Some(query.clone());
        ctx.request(Request::Notices(Box::new(query)));
    }

    fn take(&mut self, event: &NoticesEvent, ctx: &mut Ctx) {
        if event.query == NoticesQuery::Settings && self.reading {
            self.reading = false;
            match &event.result {
                Ok(NoticesReply::Settings(settings)) => {
                    let kept = self.edited();
                    self.stored = Load::Ready(settings.clone());
                    if !kept {
                        self.reset();
                    }
                }
                Ok(_) => {}
                Err(code) => {
                    if self.stored.ready().is_none() {
                        self.stored = Load::Failed(code.clone());
                    }
                }
            }
            return;
        }
        if self.saving.as_ref() != Some(&event.query) {
            return;
        }
        let Some(NoticesQuery::Act {
            action: NoticeAction::SaveSettings { settings },
            ..
        }) = self.saving.take()
        else {
            return;
        };
        match &event.result {
            Ok(_) => {
                self.stored = Load::Ready(settings);
                self.reset();
                ctx.say(Message::success(SAVED, ""));
                self.read(ctx);
            }
            Err(code) => ctx.say(Message::error(NOT_SAVED, plain(code))),
        }
    }

    fn label_width() -> usize {
        NoticeKind::ALL
            .iter()
            .map(|kind| kind.label().chars().count())
            .chain([BANNERS_LABEL.len(), DAYS_LABEL.len()])
            .max()
            .unwrap_or(0)
    }

    fn choice_spans<T: Copy + PartialEq>(
        options: &[(T, &str)],
        now: T,
        ctx: &Ctx,
    ) -> Vec<Span<'static>> {
        options
            .iter()
            .map(|(choice, label)| {
                if *choice == now {
                    Span::styled(format!("[{label}] "), ctx.theme.heading)
                } else {
                    Span::styled(format!(" {label}  "), ctx.theme.muted)
                }
            })
            .collect()
    }

    fn row_line(&self, row: Row, width: usize, ctx: &Ctx) -> Line<'static> {
        let label_width = Self::label_width();
        let (label, choices): (&str, Vec<Span<'static>>) = match row {
            Row::Kind(kind) => (
                kind.label(),
                Self::choice_spans(choices(kind), self.picks.delivery(kind), ctx),
            ),
            Row::Banners => (
                BANNERS_LABEL,
                Self::choice_spans(&BANNER_CHOICES, self.picks.banners, ctx),
            ),
            Row::Days => (
                DAYS_LABEL,
                vec![
                    Span::styled(format!("[{:>3}]", self.days), ctx.theme.heading),
                    Span::raw(" days"),
                ],
            ),
        };
        let mut spans = vec![Span::raw(format!("{label:<label_width$}  "))];
        spans.extend(choices);
        list_row(
            Line::from(spans),
            width,
            rows()[self.selected] == row,
            true,
            &ctx.theme,
            ctx.glyphs,
        )
    }

    /// The page's lines and, for each row, the first and last line that must show with it:
    /// the first row brings the page title, the first row of a section the section title
    /// and words, and the days their problem line. Inputs: the width and the context.
    /// Output: the lines and one span per row.
    fn body(&self, width: usize, ctx: &Ctx) -> (Vec<Line<'static>>, Vec<(usize, usize)>) {
        let theme = &ctx.theme;
        let about = |lines: &mut Vec<Line<'static>>, text: &str| {
            for part in wrap(text, width.max(8)) {
                lines.push(Line::styled(part, theme.muted));
            }
        };
        let mut lines = vec![
            Line::styled(TITLE, theme.heading),
            Line::styled(PURPOSE, theme.muted),
        ];
        let mut spans = Vec::new();
        let all = rows();
        for (class, title, words) in SECTIONS {
            lines.push(Line::default());
            let start = if spans.is_empty() { 0 } else { lines.len() };
            lines.push(Line::styled(title, theme.heading));
            about(&mut lines, words);
            let mut first = true;
            for row in &all {
                let Row::Kind(kind) = row else { continue };
                if kind.class() != class {
                    continue;
                }
                spans.push((if first { start } else { lines.len() }, lines.len()));
                first = false;
                lines.push(self.row_line(*row, width, ctx));
            }
        }
        for (title, words, row) in [
            (BANNERS_TITLE, BANNERS_ABOUT, Row::Banners),
            (DAYS_TITLE, DAYS_ABOUT, Row::Days),
        ] {
            lines.push(Line::default());
            let start = lines.len();
            lines.push(Line::styled(title, theme.heading));
            about(&mut lines, words);
            lines.push(self.row_line(row, width, ctx));
            if row == Row::Days && self.typed_days().is_none() {
                lines.push(Line::styled(DAYS_WRONG, theme.warning));
            }
            spans.push((start, lines.len() - 1));
        }
        assert_eq!(spans.len(), ROWS);
        (lines, spans)
    }

    fn moved(&mut self, to: usize) {
        let to = to.min(ROWS - 1);
        if to != self.selected {
            self.selected = to;
            self.fresh = true;
        }
    }
}

impl Screen for NotificationSettingsPage {
    fn crumbs(&self) -> Vec<String> {
        vec!["Settings".into(), "Notifications".into()]
    }

    fn status(&self, ctx: &Ctx) -> Line<'static> {
        if self.saving.is_some() {
            return Line::styled(format!("Saving{}", ctx.glyphs.ellipsis), ctx.theme.muted);
        }
        Line::default()
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
        let vertical = format!("{}{}", ctx.glyphs.up, ctx.glyphs.down);
        let sideways = format!("{}{}", ctx.glyphs.left, ctx.glyphs.right);
        let mut hints = vec![hint(&vertical, "Move"), hint(&sideways, "Change")];
        if rows()[self.selected] == Row::Days {
            hints.push(hint("0-9", "Type days"));
        }
        if self.edited() {
            hints.push(hint("Enter", "Save changes"));
            hints.push(hint("Esc", "Discard changes"));
        } else {
            hints.push(hint("Esc", "Back"));
        }
        hints
    }

    fn key(&mut self, key: &Key, ctx: &mut Ctx) -> Flow {
        if matches!(self.stored, Load::Failed(_)) && *key == Key::Char('r') {
            self.read(ctx);
            return Flow::Handled;
        }
        if self.stored.ready().is_none() {
            return Flow::Ignored;
        }
        if self.saving.is_some() {
            return if *key == Key::Esc {
                Flow::Ignored
            } else {
                Flow::Handled
            };
        }
        let on_days = rows()[self.selected] == Row::Days;
        match key {
            Key::Up => self.moved(self.selected.saturating_sub(1)),
            Key::Down => self.moved(self.selected + 1),
            Key::Home => self.moved(0),
            Key::End => self.moved(ROWS - 1),
            Key::Right => self.change(true),
            Key::Left => self.change(false),
            Key::Enter => self.save(ctx),
            Key::Esc if self.edited() => self.reset(),
            other if on_days && self.type_days(other) => {}
            _ => return Flow::Ignored,
        }
        Flow::Handled
    }

    fn arrive(&mut self, arrival: &Arrival, ctx: &mut Ctx) -> Flow {
        match arrival {
            Arrival::Owner(event) => {
                if let OwnerEvent::Notices(event) = event.as_ref() {
                    self.take(event, ctx);
                }
            }
            Arrival::Disconnected(_) => {
                self.reading = false;
                self.saving = None;
                if self.stored.ready().is_none() {
                    self.stored = Load::Idle;
                }
            }
            _ => {}
        }
        Flow::Handled
    }

    fn shown(&mut self, ctx: &mut Ctx) {
        if matches!(self.stored, Load::Idle) {
            self.read(ctx);
        }
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect, ctx: &Ctx) -> Option<Position> {
        let inner = Rect {
            x: area.x.saturating_add(GUTTER),
            width: area.width.saturating_sub(2 * GUTTER),
            ..area
        };
        let width = usize::from(inner.width);
        let room = usize::from(inner.height);
        let lines = match &self.stored {
            Load::Ready(_) => {
                let (lines, spans) = self.body(width, ctx);
                let (start, end) = spans[self.selected];
                if start < self.top {
                    self.top = start;
                }
                if room > 0 && end >= self.top + room {
                    self.top = end + 1 - room;
                }
                self.top = self.top.min(lines.len().saturating_sub(1));
                lines.into_iter().skip(self.top).take(room).collect()
            }
            Load::Failed(code) => vec![
                Line::styled(NOT_READ, ctx.theme.warning),
                Line::styled(plain(code), ctx.theme.muted),
            ],
            Load::Idle | Load::Loading | Load::NotReady => {
                if ctx.quiet {
                    Vec::new()
                } else {
                    vec![Line::styled(
                        format!("Loading the notification settings{}", ctx.glyphs.ellipsis),
                        ctx.theme.muted,
                    )]
                }
            }
        };
        draw_lines(frame.buffer_mut(), inner, &lines);
        None
    }

    fn place(&self) -> Option<Place> {
        Some(Place::NotificationSettings)
    }
}

#[cfg(all(test, unix))]
mod tests;
