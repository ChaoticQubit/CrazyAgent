use crate::keep_awake::{
    KeepAwakeAction, KeepAwakeView, LidRule, LidSupport, Mode, Want, refusal_words,
};
use crate::tui::client::OwnerEvent;
use crate::tui::client::keep_awake::{KeepAwakeEvent, KeepAwakeQuery, KeepAwakeReply, Picks};
use crate::tui::clock;
use crate::tui::ctx::Ctx;
use crate::tui::frame::{Hint, Message, draw_lines, hint};
use crate::tui::keys::Key;
use crate::tui::screens::settings_accounts::plain;
use crate::tui::screens::{Flow, Place, Screen};
use crate::tui::seam::{Arrival, Request};
use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};

pub(in crate::tui) mod words;

const GUTTER: u16 = 2;
const LABEL_WIDTH: usize = 20;
const KEY_WIDTH: usize = 3;
const NAME_WIDTH: usize = 17;
const ACTION_WIDTH: usize = 22;
const HOUR_MINUTES: u16 = 60;
const FOUR_HOURS_MINUTES: u16 = 240;
const CUSTOM_DIGITS_MAX: usize = 4;
const MODES: [(Mode, &str); 3] = [
    (Mode::Auto, "Auto"),
    (Mode::Manual, "Manual"),
    (Mode::Off, "Off"),
];
const LID_RULES: [LidRule; 3] = [LidRule::AcOnly, LidRule::Always, LidRule::Off];
const EXTEND_NEEDS_TIMER: &str = "Extend works while a timer runs. Start one first.";
const OFF_FIRST: &str = "Keep awake is Off. Pick Auto or Manual to start a timer.";
const NOT_READ: &str = "Couldn't load Keep awake.";
const NOT_CHANGED: &str = "Keep awake was not changed.";
const MINUTES_INVALID: &str = "keep_awake_minutes_invalid";

const _: () = assert!(FOUR_HOURS_MINUTES <= crate::keep_awake::MINUTES_MAX);
const _: () = assert!(KEY_WIDTH + NAME_WIDTH == LABEL_WIDTH);

/// The sentence for a change the engine refused: the Keep awake words for its own codes,
/// else the shared plain sentence. Input: the code. Output: the sentence.
pub(in crate::tui) fn refusal(code: &str) -> &'static str {
    refusal_words(code).unwrap_or_else(|| plain(code))
}

/// The option after `now` in `options`, wrapping; the first one when `now` is not listed.
/// Inputs: the options and the current one. Output: the next one.
fn after<T: Copy + PartialEq>(options: &[T], now: T) -> T {
    assert!(!options.is_empty());
    let at = options.iter().position(|option| *option == now);
    let next = at.map_or(0, |at| (at + 1) % options.len());
    assert!(next < options.len());
    options[next]
}

/// The Keep awake screen (DEC-167 addendum 7): the state, what a closed lid does, the
/// timer actions 1, 4, c, e and x, the mode and the lid rule on m and l, saved at once,
/// the lid helper's t and d on macOS while it offers them, what ends a hold by itself, and
/// s for every setting. Opened by W and by Go to. It
/// draws the view the shell keeps in the store and sends one change at a time.
pub(in crate::tui) struct KeepAwakeScreen {
    custom: Option<String>,
    sent: Option<KeepAwakeQuery>,
    problem: Option<String>,
}

impl KeepAwakeScreen {
    /// Opens the screen with no field open and nothing sent. Input: none. Output: the
    /// screen; the shell reads the view whenever a screen is shown.
    pub fn new() -> Self {
        Self {
            custom: None,
            sent: None,
            problem: None,
        }
    }

    fn send(&mut self, query: KeepAwakeQuery, ctx: &mut Ctx) {
        assert!(self.sent.is_none());
        assert!(query != KeepAwakeQuery::Read);
        self.sent = Some(query.clone());
        ctx.request(Request::KeepAwake(Box::new(query)));
    }

    fn act(&mut self, action: KeepAwakeAction, ctx: &mut Ctx) {
        let generation = ctx.next_generation();
        self.send(KeepAwakeQuery::Act { action, generation }, ctx);
    }

    fn save(&mut self, picks: Picks, ctx: &mut Ctx) {
        assert!(picks != Picks::default());
        let generation = ctx.next_generation();
        self.send(KeepAwakeQuery::Save { picks, generation }, ctx);
    }

    /// Starts a timer, or says why not in mode Off. Inputs: the view, the minutes and the
    /// context. Output: none.
    fn start(&mut self, view: &KeepAwakeView, minutes: u16, ctx: &mut Ctx) {
        assert!((1..=crate::keep_awake::MINUTES_MAX).contains(&minutes));
        if !words::can_start(view) {
            ctx.say(Message::warning(refusal("keep_awake_off"), ""));
            return;
        }
        self.act(KeepAwakeAction::Start { minutes }, ctx);
    }

    /// Takes one key while the custom minutes field is open: digits up to four, Backspace,
    /// Enter starts the timer when the minutes are whole and from 1 to 1440, Esc closes the
    /// field. Inputs: the view, the key and the context. Output: none.
    fn type_custom(&mut self, view: &KeepAwakeView, key: &Key, ctx: &mut Ctx) {
        let Some(typed) = self.custom.as_mut() else {
            return;
        };
        match key {
            Key::Char(digit) if digit.is_ascii_digit() && typed.len() < CUSTOM_DIGITS_MAX => {
                typed.push(*digit);
            }
            Key::Backspace => {
                typed.pop();
            }
            Key::Esc => self.custom = None,
            Key::Enter => match words::custom_minutes(typed) {
                Some(minutes) => {
                    self.custom = None;
                    self.start(view, minutes, ctx);
                }
                None => ctx.say(Message::error(refusal(MINUTES_INVALID), "")),
            },
            _ => {}
        }
    }

    /// Takes one action key on a loaded view with no field open and nothing under way;
    /// t and d run only while the lid helper offers them. Inputs: the view, the key and
    /// the context. Output: where to go.
    fn action(&mut self, view: &KeepAwakeView, key: &Key, ctx: &mut Ctx) -> Flow {
        let offers = words::lid_offers(view, words::LidPlace::Screen);
        match key {
            Key::Char('t') if offers.turn_on.is_some() => {
                self.act(KeepAwakeAction::TurnOnLidHold, ctx);
            }
            Key::Char('d') if offers.remove => self.act(KeepAwakeAction::RemoveLidHelper, ctx),
            Key::Char('1') => self.start(view, HOUR_MINUTES, ctx),
            Key::Char('4') => self.start(view, FOUR_HOURS_MINUTES, ctx),
            Key::Char('c') if words::can_start(view) => self.custom = Some(String::new()),
            Key::Char('c') => ctx.say(Message::warning(refusal("keep_awake_off"), "")),
            Key::Char('e') if words::can_extend(view) => {
                let minutes = HOUR_MINUTES;
                self.act(KeepAwakeAction::Extend { minutes }, ctx);
            }
            Key::Char('e') => ctx.say(Message::warning(EXTEND_NEEDS_TIMER, "")),
            Key::Char('x') if words::can_end(view) => self.act(KeepAwakeAction::End, ctx),
            Key::Char('x') => ctx.say(Message::warning(refusal("keep_awake_not_holding"), "")),
            Key::Char('m') => {
                let modes = MODES.map(|(mode, _)| mode);
                let mode = Some(after(&modes, view.settings.mode));
                self.save(
                    Picks {
                        mode,
                        ..Picks::default()
                    },
                    ctx,
                );
            }
            Key::Char('l') => {
                let lid = Some(after(&LID_RULES, view.settings.lid));
                self.save(
                    Picks {
                        lid,
                        ..Picks::default()
                    },
                    ctx,
                );
            }
            Key::Char('s') => return Flow::Open(Place::KeepAwakeSettings),
            _ => return Flow::Ignored,
        }
        Flow::Handled
    }

    fn take(&mut self, event: &KeepAwakeEvent, ctx: &mut Ctx) {
        if event.query == KeepAwakeQuery::Read {
            self.problem = match &event.result {
                Ok(KeepAwakeReply::View(_)) => None,
                Ok(KeepAwakeReply::Done | KeepAwakeReply::Apps(_)) => self.problem.take(),
                Err(code) => Some(code.clone()),
            };
            return;
        }
        if self.sent.as_ref() != Some(&event.query) {
            return;
        }
        self.sent = None;
        if let Err(code) = &event.result {
            ctx.say(Message::error(NOT_CHANGED, refusal(code)));
        }
    }

    fn mode_spans(now: Mode, ctx: &Ctx) -> Vec<Span<'static>> {
        MODES
            .iter()
            .map(|(mode, label)| {
                if *mode == now {
                    Span::styled(format!("[{label}] "), ctx.theme.heading)
                } else {
                    Span::styled(format!(" {label}  "), ctx.theme.muted)
                }
            })
            .collect()
    }

    fn pair(
        left: (&str, &str, bool),
        right: Option<(&str, &str, bool)>,
        ctx: &Ctx,
    ) -> Line<'static> {
        let style = |on: bool| {
            if on {
                Style::default()
            } else {
                ctx.theme.muted
            }
        };
        let cell = |(key, label, on): (&str, &str, bool)| {
            Span::styled(format!("{key:<KEY_WIDTH$}{label}"), style(on))
        };
        let first = cell(left);
        let gap = ACTION_WIDTH.saturating_sub(first.width());
        let mut spans = vec![first];
        if let Some(right) = right {
            spans.push(Span::raw(" ".repeat(gap)));
            spans.push(cell(right));
        }
        Line::from(spans)
    }

    fn custom_line(&self, on: bool, ctx: &Ctx) -> Line<'static> {
        let Some(typed) = &self.custom else {
            return Self::pair(("c", "Custom time", on), None, ctx);
        };
        Line::from(vec![
            Span::raw(format!("{:<KEY_WIDTH$}{:<NAME_WIDTH$}", "c", "Custom time")),
            Span::styled(format!("[{typed:<CUSTOM_DIGITS_MAX$}]"), ctx.theme.heading),
            Span::raw(" minutes"),
        ])
    }

    /// The screen's lines for one view, in the approved board's order. Inputs: the view
    /// and the context. Output: the lines, 12 or, in mode Off, 13.
    fn body(&self, view: &KeepAwakeView, ctx: &Ctx) -> Vec<Line<'static>> {
        let theme = &ctx.theme;
        let state = words::label(view, clock::now_ms(), words::Form::Words);
        let tone = match words::tone(view) {
            words::Tone::Hold => theme.success,
            words::Tone::Limit => theme.warning,
            words::Tone::Ended => theme.error,
            words::Tone::Plain => theme.text,
        };
        let row = |key: &str, label: &str, value: Vec<Span<'static>>| {
            let mut spans = vec![Span::raw(format!("{key:<KEY_WIDTH$}{label:<NAME_WIDTH$}"))];
            spans.extend(value);
            Line::from(spans)
        };
        let start = words::can_start(view);
        let mut lines = vec![
            Line::from(vec![
                Span::raw(format!("{:<LABEL_WIDTH$}", "State")),
                Span::styled(state, tone),
            ]),
            Line::raw(format!(
                "{:<LABEL_WIDTH$}{}",
                "Lid closed",
                words::lid_now(view)
            )),
            Line::default(),
            Self::pair(
                ("1", "Start 1 h", start),
                Some(("e", "Extend 1 h", words::can_extend(view))),
                ctx,
            ),
            Self::pair(
                ("4", "Start 4 h", start),
                Some(("x", "End now", words::can_end(view))),
                ctx,
            ),
            self.custom_line(start, ctx),
        ];
        if !start {
            lines.push(Line::styled(OFF_FIRST, theme.muted));
        }
        lines.push(Line::default());
        lines.push(row("m", "Mode", Self::mode_spans(view.settings.mode, ctx)));
        let lid = words::lid_closed(view.settings.lid);
        lines.push(row("l", "Lid closed", vec![Span::raw(lid)]));
        let helper = Self::lid_helper_lines(view, ctx);
        let added = helper.len();
        lines.extend(helper);
        let ends = words::ends_by_itself(&view.settings);
        lines.push(row("", "Ends by itself", vec![Span::raw(ends)]));
        lines.push(Line::default());
        lines.push(row("s", "All Keep awake settings", Vec::new()));
        assert!(added <= 3);
        assert!(lines.len() == 12 + usize::from(!start) + added);
        lines
    }

    /// The lid helper's lines under the lid rule (QUESTION 35i, 35j): the approval line
    /// while macOS waits and nothing is held, then t and d as the lid helper offers them.
    /// Inputs: the view and the context. Output: none to three lines.
    fn lid_helper_lines(view: &KeepAwakeView, ctx: &Ctx) -> Vec<Line<'static>> {
        let offers = words::lid_offers(view, words::LidPlace::Screen);
        let waiting = view.lid_platform == LidSupport::NeedsApproval;
        let mut lines = Vec::new();
        if waiting && offers.turn_on.is_some() && view.state.want == Want::Nothing {
            lines.push(Line::styled(words::APPROVAL_LINE, ctx.theme.muted));
        }
        let row = |key: &str, label: &str| Line::raw(format!("{key:<KEY_WIDTH$}{label}"));
        if let Some(turn_on) = offers.turn_on {
            lines.push(row("t", turn_on));
        }
        if offers.remove {
            lines.push(row("d", "Remove lid helper"));
        }
        assert!(lines.len() <= 3);
        lines
    }
}

impl Screen for KeepAwakeScreen {
    fn crumbs(&self) -> Vec<String> {
        vec!["Keep awake".into()]
    }

    fn status(&self, ctx: &Ctx) -> Line<'static> {
        if self.sent.is_some() {
            return Line::styled(format!("Saving{}", ctx.glyphs.ellipsis), ctx.theme.muted);
        }
        Line::default()
    }

    fn hints(&self, ctx: &Ctx) -> Vec<Hint> {
        if self.custom.is_some() {
            return vec![
                hint("0-9", "Type minutes"),
                hint("Enter", "Start"),
                hint("Esc", "Cancel"),
            ];
        }
        if ctx.store.keep_awake.is_none() && self.problem.is_some() {
            return vec![hint("r", "Try again"), hint("Esc", "Back")];
        }
        vec![hint("Esc", "Back")]
    }

    fn keys(&self, ctx: &Ctx) -> Vec<Hint> {
        let mut keys = vec![
            hint("1", "Start 1 h"),
            hint("4", "Start 4 h"),
            hint("c", "Custom time"),
            hint("e", "Extend 1 h"),
            hint("x", "End now"),
            hint("m", "Mode"),
            hint("l", "Lid closed"),
        ];
        if let Some(view) = &ctx.store.keep_awake {
            let offers = words::lid_offers(view, words::LidPlace::Screen);
            if let Some(turn_on) = offers.turn_on {
                keys.push(hint("t", turn_on));
            }
            if offers.remove {
                keys.push(hint("d", "Remove lid helper"));
            }
        }
        keys.push(hint("s", "All Keep awake settings"));
        keys.push(hint("Esc", "Back"));
        keys
    }

    fn typing(&self) -> bool {
        self.custom.is_some()
    }

    fn key(&mut self, key: &Key, ctx: &mut Ctx) -> Flow {
        let Some(view) = ctx.store.keep_awake.clone() else {
            if self.problem.is_some() && *key == Key::Char('r') {
                ctx.request(Request::KeepAwake(Box::new(KeepAwakeQuery::Read)));
                return Flow::Handled;
            }
            return Flow::Ignored;
        };
        if self.sent.is_some() {
            return Flow::Handled;
        }
        if self.custom.is_some() {
            self.type_custom(&view, key, ctx);
            return Flow::Handled;
        }
        self.action(&view, key, ctx)
    }

    fn arrive(&mut self, arrival: &Arrival, ctx: &mut Ctx) -> Flow {
        match arrival {
            Arrival::Owner(event) => {
                if let OwnerEvent::KeepAwake(event) = event.as_ref() {
                    self.take(event, ctx);
                }
            }
            Arrival::Disconnected(_) => {
                self.sent = None;
                self.custom = None;
            }
            _ => {}
        }
        Flow::Handled
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect, ctx: &Ctx) -> Option<Position> {
        let inner = Rect {
            x: area.x.saturating_add(GUTTER),
            width: area.width.saturating_sub(2 * GUTTER),
            ..area
        };
        let lines = match (&ctx.store.keep_awake, &self.problem) {
            (Some(view), _) => self.body(view, ctx),
            (None, Some(code)) => vec![
                Line::styled(NOT_READ, ctx.theme.warning),
                Line::styled(plain(code), ctx.theme.muted),
            ],
            (None, None) if ctx.quiet => Vec::new(),
            (None, None) => vec![Line::styled(
                format!("Loading Keep awake{}", ctx.glyphs.ellipsis),
                ctx.theme.muted,
            )],
        };
        draw_lines(frame.buffer_mut(), inner, &lines);
        None
    }

    fn place(&self) -> Option<Place> {
        Some(Place::KeepAwake)
    }
}

#[cfg(all(test, unix))]
pub(in crate::tui) mod tests;
