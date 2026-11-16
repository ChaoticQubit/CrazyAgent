use crate::keep_awake::{
    BatteryFloor, Cap, DisplayAwake, Grace, HeatEnd, HeatNotice, KeepAwakeAction,
    KeepAwakeSettings, KeepAwakeView, LidDisplay, LidRule, Mode,
};
use crate::paths::Platform;
use crate::tui::client::OwnerEvent;
use crate::tui::client::keep_awake::{KeepAwakeEvent, KeepAwakeQuery, KeepAwakeReply, Picks};
use crate::tui::ctx::Ctx;
use crate::tui::frame::{Hint, Message, draw_lines, hint};
use crate::tui::keys::Key;
use crate::tui::screens::keep_awake::refusal;
use crate::tui::screens::keep_awake::words::{
    LidOffers, LidPlace, Setting, lid_helper_line, lid_offers, unavailable,
};
use crate::tui::screens::settings_accounts::plain;
use crate::tui::screens::{Flow, Place, Screen};
use crate::tui::seam::{Arrival, Request};
use crate::tui::text::{list_row, wrap};
use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::text::{Line, Span};

const GUTTER: u16 = 2;
const TITLE: &str = "Keep awake";
const PURPOSE: &str = "When the computer stays awake for your agents, and when that ends.";
const ROWS: usize = 9;
const OPTIONS_MAX: usize = 4;
const DISPLAY_AWAKE_HELP: &str =
    "Whether a hold also keeps the display on. While it does, the screen stays unlocked.";
const SECTIONS: [(&str, &str, &[Setting]); 4] = [
    (
        "When it holds",
        "What keeps the computer awake and for how long.",
        &[Setting::Mode, Setting::Grace, Setting::Cap],
    ),
    (
        "Lid closed",
        "Whether a hold goes on with the lid closed.",
        &[Setting::Lid, Setting::LidDisplay],
    ),
    (
        "Ends by itself",
        "The limits that end a hold to protect the battery and the computer.",
        &[Setting::BatteryFloor, Setting::HeatEnd, Setting::HeatNotice],
    ),
    (
        "Display",
        "What a hold does with the display.",
        &[Setting::DisplayAwake],
    ),
];
const MODE: [(Mode, &str); 3] = [
    (Mode::Auto, "Auto"),
    (Mode::Manual, "Manual timer"),
    (Mode::Off, "Off"),
];
const GRACE: [(Grace, &str); 4] = [
    (Grace::Min0, "0"),
    (Grace::Min5, "5 min"),
    (Grace::Min10, "10 min"),
    (Grace::Min30, "30 min"),
];
const CAP: [(Cap, &str); 4] = [
    (Cap::H4, "4 h"),
    (Cap::H8, "8 h"),
    (Cap::H24, "24 h"),
    (Cap::None, "No limit"),
];
const LID: [(LidRule, &str); 3] = [
    (LidRule::AcOnly, "On AC power only"),
    (LidRule::Always, "Always"),
    (LidRule::Off, "Off"),
];
const LID_DISPLAY: [(LidDisplay, &str); 2] = [
    (LidDisplay::Sleep, "Sleep the display"),
    (LidDisplay::LeaveOn, "Leave on"),
];
const BATTERY_FLOOR: [(BatteryFloor, &str); 4] = [
    (BatteryFloor::Off, "Off"),
    (BatteryFloor::Pct5, "5 %"),
    (BatteryFloor::Pct10, "10 %"),
    (BatteryFloor::Pct20, "20 %"),
];
const HEAT_END: [(HeatEnd, &str); 3] = [
    (HeatEnd::Critical, "Critical"),
    (HeatEnd::Serious, "Serious"),
    (HeatEnd::Never, "Never"),
];
const HEAT_NOTICE: [(HeatNotice, &str); 2] = [
    (HeatNotice::Serious, "At serious heat"),
    (HeatNotice::Off, "Off"),
];
const DISPLAY_AWAKE: [(DisplayAwake, &str); 2] = [
    (DisplayAwake::Off, "Off"),
    (DisplayAwake::WhileHolding, "While holding"),
];
const SAVED: &str = "Keep awake settings saved.";
const NOT_SAVED: &str = "The Keep awake settings were not saved.";
const NOT_READ: &str = "Couldn't load the Keep awake settings.";
const LID_NOT_CHANGED: &str = "The lid helper was not changed.";

/// One setting's label and help, in the desktop Settings page's words. Input: the
/// setting. Output: the label and the help sentence.
fn words(setting: Setting) -> (&'static str, &'static str) {
    match setting {
        Setting::Mode => (
            "Mode",
            "Auto holds while agents work. Manual timer holds only while a timer you start runs. Off never holds.",
        ),
        Setting::Grace => (
            "Grace after work ends",
            "How long the computer stays awake after the last agent finishes.",
        ),
        Setting::Cap => (
            "Longest single hold",
            "One hold ends by itself after this long, even while agents still work.",
        ),
        Setting::Lid => (
            "Lid-closed hold",
            "Whether a hold also keeps the computer awake with the lid closed.",
        ),
        Setting::LidDisplay => (
            "Display with the lid closed (macOS)",
            "What an outside display does while a hold runs with the lid closed.",
        ),
        Setting::BatteryFloor => (
            "End at battery",
            "On battery, a hold ends when the charge falls to this level.",
        ),
        Setting::HeatEnd => (
            "End at heat",
            "A hold ends when the computer gets this hot.",
        ),
        Setting::HeatNotice => (
            "Heat notice",
            "Tells you when the computer gets hot during a hold.",
        ),
        Setting::DisplayAwake => ("Display awake with the lid open", DISPLAY_AWAKE_HELP),
    }
}

/// The settings in page order: the four sections' settings, one after the other. Input:
/// none. Output: the settings, `ROWS` of them.
fn rows() -> Vec<Setting> {
    let rows: Vec<Setting> = SECTIONS
        .iter()
        .flat_map(|(_, _, settings)| settings.iter().copied())
        .collect();
    assert_eq!(rows.len(), ROWS);
    rows
}

fn names<T>(options: &[(T, &'static str)]) -> Vec<&'static str> {
    options.iter().map(|(_, label)| *label).collect()
}

fn index<T: Copy + PartialEq>(options: &[(T, &str)], now: T) -> usize {
    let at = options.iter().position(|(option, _)| *option == now);
    at.expect("every stored option is offered")
}

/// The option labels one setting offers, in the desktop's order. Input: the setting.
/// Output: two to four labels.
fn labels(setting: Setting) -> Vec<&'static str> {
    let labels = match setting {
        Setting::Mode => names(&MODE),
        Setting::Grace => names(&GRACE),
        Setting::Cap => names(&CAP),
        Setting::Lid => names(&LID),
        Setting::LidDisplay => names(&LID_DISPLAY),
        Setting::BatteryFloor => names(&BATTERY_FLOOR),
        Setting::HeatEnd => names(&HEAT_END),
        Setting::HeatNotice => names(&HEAT_NOTICE),
        Setting::DisplayAwake => names(&DISPLAY_AWAKE),
    };
    assert!((2..=OPTIONS_MAX).contains(&labels.len()));
    labels
}

/// Which of a setting's options the settings hold. Inputs: the setting and the settings.
/// Output: the option's place in `labels`.
fn chosen(setting: Setting, settings: &KeepAwakeSettings) -> usize {
    match setting {
        Setting::Mode => index(&MODE, settings.mode),
        Setting::Grace => index(&GRACE, settings.grace),
        Setting::Cap => index(&CAP, settings.cap),
        Setting::Lid => index(&LID, settings.lid),
        Setting::LidDisplay => index(&LID_DISPLAY, settings.lid_display),
        Setting::BatteryFloor => index(&BATTERY_FLOOR, settings.battery_floor),
        Setting::HeatEnd => index(&HEAT_END, settings.heat_end),
        Setting::HeatNotice => index(&HEAT_NOTICE, settings.heat_notice),
        Setting::DisplayAwake => index(&DISPLAY_AWAKE, settings.display_awake),
    }
}

/// Notes one option of one setting in the picks. Inputs: the setting, the option's place
/// in `labels` and the picks. Output: none; the pick replaces an earlier one.
fn pick(setting: Setting, at: usize, picks: &mut Picks) {
    assert!(at < labels(setting).len());
    match setting {
        Setting::Mode => picks.mode = Some(MODE[at].0),
        Setting::Grace => picks.grace = Some(GRACE[at].0),
        Setting::Cap => picks.cap = Some(CAP[at].0),
        Setting::Lid => picks.lid = Some(LID[at].0),
        Setting::LidDisplay => picks.lid_display = Some(LID_DISPLAY[at].0),
        Setting::BatteryFloor => picks.battery_floor = Some(BATTERY_FLOOR[at].0),
        Setting::HeatEnd => picks.heat_end = Some(HEAT_END[at].0),
        Setting::HeatNotice => picks.heat_notice = Some(HEAT_NOTICE[at].0),
        Setting::DisplayAwake => picks.display_awake = Some(DISPLAY_AWAKE[at].0),
    }
}

/// Settings, Keep awake (DEC-167 addendum 7): the nine settings in the desktop's four
/// sections, with its labels, help, options and reasons, and on macOS the lid helper's
/// line under the lid rule with t and d as it offers them, whatever the rule (QUESTION
/// 36c). Left and Right pick; the picks stay unsaved until Enter saves them over the
/// settings the engine holds at that moment, and Esc drops them first. Opened by Go to and
/// by s on the Keep awake screen.
pub(in crate::tui) struct KeepAwakeSettingsPage {
    picks: Picks,
    selected: usize,
    top: usize,
    saving: Option<KeepAwakeQuery>,
    acting: Option<KeepAwakeQuery>,
    problem: Option<String>,
}

impl KeepAwakeSettingsPage {
    /// Opens the page with no picks. Input: none. Output: the page; the shell reads the
    /// view whenever a screen is shown.
    pub fn new() -> Self {
        Self {
            picks: Picks::default(),
            selected: 0,
            top: 0,
            saving: None,
            acting: None,
            problem: None,
        }
    }

    /// Sends one lid helper command, unless a save or another command is on its way.
    /// Inputs: the command and the context. Output: none.
    fn lid(&mut self, action: KeepAwakeAction, ctx: &mut Ctx) {
        assert!(matches!(
            action,
            KeepAwakeAction::TurnOnLidHold | KeepAwakeAction::RemoveLidHelper
        ));
        if self.saving.is_some() || self.acting.is_some() {
            return;
        }
        let query = KeepAwakeQuery::Act {
            action,
            generation: ctx.next_generation(),
        };
        self.acting = Some(query.clone());
        ctx.request(Request::KeepAwake(Box::new(query)));
    }

    fn edited(&self, stored: &KeepAwakeSettings) -> bool {
        self.picks.over(*stored) != *stored
    }

    fn change(&mut self, stored: &KeepAwakeSettings, forward: bool) {
        let setting = rows()[self.selected];
        let count = labels(setting).len();
        let now = chosen(setting, &self.picks.over(*stored));
        assert!(now < count);
        let next = if forward {
            (now + 1) % count
        } else {
            (now + count - 1) % count
        };
        pick(setting, next, &mut self.picks);
    }

    /// Sends the picks as one save, unless nothing differs from the stored settings or a
    /// save is on its way. Inputs: the stored settings and the context. Output: none.
    fn save(&mut self, stored: &KeepAwakeSettings, ctx: &mut Ctx) {
        if !self.edited(stored) || self.saving.is_some() {
            return;
        }
        let query = KeepAwakeQuery::Save {
            picks: self.picks,
            generation: ctx.next_generation(),
        };
        self.saving = Some(query.clone());
        ctx.request(Request::KeepAwake(Box::new(query)));
    }

    fn take(&mut self, event: &KeepAwakeEvent, ctx: &mut Ctx) {
        if event.query == KeepAwakeQuery::Read {
            self.problem = match &event.result {
                Ok(KeepAwakeReply::View(_)) => None,
                Ok(KeepAwakeReply::Done) => self.problem.take(),
                Err(code) => Some(code.clone()),
            };
            return;
        }
        if self.acting.as_ref() == Some(&event.query) {
            self.acting = None;
            if let Err(code) = &event.result {
                ctx.say(Message::error(LID_NOT_CHANGED, refusal(code)));
            }
            return;
        }
        if self.saving.as_ref() != Some(&event.query) {
            return;
        }
        self.saving = None;
        match &event.result {
            Ok(_) => {
                self.picks = Picks::default();
                ctx.say(Message::success(SAVED, ""));
            }
            Err(code) => ctx.say(Message::error(NOT_SAVED, refusal(code))),
        }
    }

    fn label_width() -> usize {
        rows()
            .into_iter()
            .map(|setting| words(setting).0.chars().count())
            .max()
            .unwrap_or(0)
    }

    fn row_line(
        &self,
        setting: Setting,
        shown: &KeepAwakeSettings,
        width: usize,
        ctx: &Ctx,
    ) -> Line<'static> {
        let label_width = Self::label_width();
        let label = words(setting).0;
        let now = chosen(setting, shown);
        let mut spans = vec![Span::raw(format!("{label:<label_width$}  "))];
        for (at, option) in labels(setting).into_iter().enumerate() {
            spans.push(if at == now {
                Span::styled(format!("[{option}] "), ctx.theme.heading)
            } else {
                Span::styled(format!(" {option}  "), ctx.theme.muted)
            });
        }
        list_row(
            Line::from(spans),
            width,
            rows()[self.selected] == setting,
            true,
            &ctx.theme,
            ctx.glyphs,
        )
    }

    /// The page's lines and, for each row, the first and last line that must show with it:
    /// the first row brings the page title, the first row of a section the section title
    /// and words, and every row its help and, when this computer cannot honour it, the
    /// reason. Inputs: the width, the view and the context. Output: the lines and one span
    /// per row.
    fn body(
        &self,
        width: usize,
        view: &KeepAwakeView,
        ctx: &Ctx,
    ) -> (Vec<Line<'static>>, Vec<(usize, usize)>) {
        let theme = &ctx.theme;
        let shown = self.picks.over(view.settings);
        let about = |lines: &mut Vec<Line<'static>>, text: &str, indent: &str| {
            for part in wrap(text, width.saturating_sub(indent.len()).max(8)) {
                lines.push(Line::styled(format!("{indent}{part}"), theme.muted));
            }
        };
        let mut lines = vec![
            Line::styled(TITLE, theme.heading),
            Line::styled(PURPOSE, theme.muted),
        ];
        let mut spans = Vec::new();
        for (title, words_about, settings) in SECTIONS {
            lines.push(Line::default());
            let mut start = if spans.is_empty() { 0 } else { lines.len() };
            lines.push(Line::styled(title, theme.heading));
            about(&mut lines, words_about, "");
            for setting in settings {
                lines.push(self.row_line(*setting, &shown, width, ctx));
                about(&mut lines, words(*setting).1, "  ");
                if let Some(reason) = unavailable(*setting, view) {
                    lines.push(Line::styled(format!("  {reason}"), theme.warning));
                }
                if *setting == Setting::Lid && view.platform == Platform::MacOs {
                    let helper = format!("Lid helper  {}", lid_helper_line(view));
                    let room = width.saturating_sub(2).max(8);
                    if helper.chars().count() <= room {
                        lines.push(Line::raw(format!("  {helper}")));
                    } else {
                        for part in wrap(&helper, room) {
                            lines.push(Line::raw(format!("  {part}")));
                        }
                    }
                }
                spans.push((start, lines.len() - 1));
                start = lines.len();
            }
        }
        assert_eq!(spans.len(), ROWS);
        (lines, spans)
    }

    fn moved(&mut self, to: usize) {
        self.selected = to.min(ROWS - 1);
    }
}

impl Screen for KeepAwakeSettingsPage {
    fn crumbs(&self) -> Vec<String> {
        vec!["Settings".into(), "Keep awake".into()]
    }

    fn status(&self, ctx: &Ctx) -> Line<'static> {
        if self.saving.is_some() || self.acting.is_some() {
            return Line::styled(format!("Saving{}", ctx.glyphs.ellipsis), ctx.theme.muted);
        }
        Line::default()
    }

    fn wraps_hints(&self) -> bool {
        true
    }

    fn hints(&self, ctx: &Ctx) -> Vec<Hint> {
        let Some(view) = &ctx.store.keep_awake else {
            if self.problem.is_some() {
                return vec![hint("r", "Try again"), hint("Esc", "Back")];
            }
            return vec![hint("Esc", "Back")];
        };
        if self.saving.is_some() || self.acting.is_some() {
            return Vec::new();
        }
        let vertical = format!("{}{}", ctx.glyphs.up, ctx.glyphs.down);
        let sideways = format!("{}{}", ctx.glyphs.left, ctx.glyphs.right);
        let mut hints = vec![hint(&vertical, "Move"), hint(&sideways, "Change")];
        let offers = lid_offers(view, LidPlace::Settings);
        if let Some(turn_on) = offers.turn_on {
            hints.push(hint("t", turn_on));
        }
        if offers.remove {
            hints.push(hint("d", "Remove lid helper"));
        }
        if self.edited(&view.settings) {
            hints.push(hint("Enter", "Save changes"));
            hints.push(hint("Esc", "Discard changes"));
        } else {
            hints.push(hint("Esc", "Back"));
        }
        hints
    }

    fn key(&mut self, key: &Key, ctx: &mut Ctx) -> Flow {
        let none = LidOffers {
            turn_on: None,
            remove: false,
        };
        let offers = (ctx.store.keep_awake.as_ref())
            .map_or(none, |view| lid_offers(view, LidPlace::Settings));
        let Some(stored) = ctx.store.keep_awake.as_ref().map(|view| view.settings) else {
            if self.problem.is_some() && *key == Key::Char('r') {
                ctx.request(Request::KeepAwake(Box::new(KeepAwakeQuery::Read)));
                return Flow::Handled;
            }
            return Flow::Ignored;
        };
        if self.saving.is_some() || self.acting.is_some() {
            return Flow::Handled;
        }
        match key {
            Key::Char('t') if offers.turn_on.is_some() => {
                self.lid(KeepAwakeAction::TurnOnLidHold, ctx);
            }
            Key::Char('d') if offers.remove => self.lid(KeepAwakeAction::RemoveLidHelper, ctx),
            Key::Up => self.moved(self.selected.saturating_sub(1)),
            Key::Down => self.moved(self.selected + 1),
            Key::Home => self.moved(0),
            Key::End => self.moved(ROWS - 1),
            Key::Right => self.change(&stored, true),
            Key::Left => self.change(&stored, false),
            Key::Enter => self.save(&stored, ctx),
            Key::Esc if self.edited(&stored) => self.picks = Picks::default(),
            _ => return Flow::Ignored,
        }
        Flow::Handled
    }

    fn arrive(&mut self, arrival: &Arrival, ctx: &mut Ctx) -> Flow {
        match arrival {
            Arrival::Owner(event) => {
                if let OwnerEvent::KeepAwake(event) = event.as_ref() {
                    self.take(event, ctx);
                }
            }
            Arrival::Disconnected(_) => {
                self.saving = None;
                self.acting = None;
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
        let width = usize::from(inner.width);
        let room = usize::from(inner.height);
        let lines = match (&ctx.store.keep_awake, &self.problem) {
            (Some(view), _) => {
                let (lines, spans) = self.body(width, view, ctx);
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
            (None, Some(code)) => vec![
                Line::styled(NOT_READ, ctx.theme.warning),
                Line::styled(plain(code), ctx.theme.muted),
            ],
            (None, None) if ctx.quiet => Vec::new(),
            (None, None) => vec![Line::styled(
                format!("Loading the Keep awake settings{}", ctx.glyphs.ellipsis),
                ctx.theme.muted,
            )],
        };
        draw_lines(frame.buffer_mut(), inner, &lines);
        None
    }

    fn place(&self) -> Option<Place> {
        Some(Place::KeepAwakeSettings)
    }
}

#[cfg(all(test, unix))]
mod tests;
