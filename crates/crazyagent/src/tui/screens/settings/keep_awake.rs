use crate::keep_awake::freeze::APPS_MAX;
use crate::keep_awake::{
    BatteryFloor, Cap, DisplayAwake, Freeze, FreezeApp, Grace, HeatEnd, HeatNotice,
    KeepAwakeAction, KeepAwakeSettings, KeepAwakeView, LidDisplay, LidRule, Mode, Mute,
};
use crate::paths::Platform;
use crate::tui::client::OwnerEvent;
use crate::tui::client::keep_awake::{KeepAwakeEvent, KeepAwakeQuery, KeepAwakeReply, Picks};
use crate::tui::ctx::Ctx;
use crate::tui::frame::{Hint, Message, Modal, draw_lines, hint};
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
const ROWS: usize = 11;
const OPTIONS_MAX: usize = 4;
const DISPLAY_AWAKE_HELP: &str =
    "Whether a hold also keeps the display on. While it does, the screen stays unlocked.";
const FREEZE_HELP: &str = "The listed apps pause while a hold runs with the lid closed and go on when the lid opens. Not while an external display is in use. If one stays paused, run crazyagent keep-awake restore; the next start also puts it back.";
const MUTE_HELP: &str = "The sound goes off while a hold runs with the lid closed and comes back as it was when the lid opens. Not while an external display is in use.";
const CHOOSER_ROWS: usize = 12;
const CHOOSER_WIDTH: u16 = 64;
const SECTIONS: [(&str, &str, &[Setting]); 5] = [
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
    (
        "While the lid is closed",
        "What a hold quiets while the lid is closed.",
        &[Setting::Freeze, Setting::Mute],
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
const FREEZE: [(Freeze, &str); 2] = [(Freeze::Off, "Off"), (Freeze::On, "On for the listed apps")];
const MUTE: [(Mute, &str); 2] = [(Mute::Off, "Off"), (Mute::On, "On")];
const SAVED: &str = "Keep awake settings saved.";
const NOT_SAVED: &str = "The Keep awake settings were not saved.";
const NOT_READ: &str = "Couldn't load the Keep awake settings.";
const LID_NOT_CHANGED: &str = "The lid helper was not changed.";
const LID_NOT_PUT_BACK: &str = "The lid setting was not put back.";
const LID_CRASH_GAP: &str = "If the engine stops by a crash during a hold, the lid stays at Do \
    nothing until the engine starts again.";
const PUT_BACK: &str = "Put the lid setting back now";
const APPS_NOT_CHANGED: &str = "The apps to freeze were not changed.";
const APPS_NOT_READ: &str = "Couldn't list the running apps.";
const APPS_TOO_MANY: &str = "Pick at most 16 apps, each once.";
const NO_APPS: &str = "None listed. Press a to choose apps.";

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
        Setting::Freeze => ("Freeze apps while the lid is closed", FREEZE_HELP),
        Setting::Mute => ("Mute audio while the lid is closed", MUTE_HELP),
    }
}

/// The settings in page order: the five sections' settings, one after the other. Input:
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
        Setting::Freeze => names(&FREEZE),
        Setting::Mute => names(&MUTE),
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
        Setting::Freeze => index(&FREEZE, settings.freeze),
        Setting::Mute => index(&MUTE, settings.mute),
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
        Setting::Freeze => picks.freeze = Some(FREEZE[at].0),
        Setting::Mute => picks.mute = Some(MUTE[at].0),
    }
}

/// The app chooser that a opens on the Freeze row (DEC-167 Task 17): the apps already
/// listed, then the running apps the engine offers, whether each is ticked and the line
/// the cursor is on. Space ticks, Enter saves the ticked apps as the list, Esc drops it.
struct Chooser {
    offered: Vec<FreezeApp>,
    ticked: Vec<bool>,
    at: usize,
}

impl Chooser {
    /// Opens the chooser on the listed apps, all ticked, then every running app not
    /// listed yet. Inputs: the listed apps and the running ones. Output: the chooser.
    fn new(listed: &[FreezeApp], running: Vec<FreezeApp>) -> Self {
        assert!(listed.len() <= APPS_MAX);
        let mut offered = listed.to_vec();
        for app in running {
            if !offered.iter().any(|known| known.id == app.id) {
                offered.push(app);
            }
        }
        let ticked = (0..offered.len()).map(|at| at < listed.len()).collect();
        Self {
            offered,
            ticked,
            at: 0,
        }
    }

    /// The ticked apps in the order they are offered. Output: at most `APPS_MAX` apps.
    fn chosen(&self) -> Vec<FreezeApp> {
        assert_eq!(self.offered.len(), self.ticked.len());
        let apps: Vec<FreezeApp> = (self.offered.iter().zip(&self.ticked))
            .filter(|(_, ticked)| **ticked)
            .map(|(app, _)| app.clone())
            .collect();
        assert!(apps.len() <= APPS_MAX);
        apps
    }

    /// Ticks or unticks the app under the cursor, unless that would tick more than
    /// `APPS_MAX`. Output: false when the tick was refused.
    fn toggle(&mut self) -> bool {
        let Some(ticked) = self.ticked.get(self.at).copied() else {
            return true;
        };
        if !ticked && self.chosen().len() == APPS_MAX {
            return false;
        }
        self.ticked[self.at] = !ticked;
        true
    }

    fn moved(&mut self, down: bool) {
        let last = self.offered.len().saturating_sub(1);
        self.at = if down {
            (self.at + 1).min(last)
        } else {
            self.at.saturating_sub(1)
        };
    }
}

/// Settings, Keep awake (DEC-167 addendum 7): the eleven settings in the desktop's five
/// sections, with its labels, help, options and reasons, on macOS the lid helper's line
/// under the lid rule with t and d as it offers them, whatever the rule (QUESTION 36c), on
/// Windows the lid setting's line with p (DEC-167 addendum 32), and the listed apps under
/// the Freeze row with a to choose them. Left and Right pick; the picks stay unsaved until
/// Enter saves them over the settings the engine holds at that moment, and Esc drops them
/// first. Opened by Go to and by s on the Keep awake screen.
pub(in crate::tui) struct KeepAwakeSettingsPage {
    picks: Picks,
    selected: usize,
    top: usize,
    saving: Option<KeepAwakeQuery>,
    acting: Option<KeepAwakeQuery>,
    asking: bool,
    choosing: Option<Chooser>,
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
            asking: false,
            choosing: None,
            problem: None,
        }
    }

    fn waiting(&self) -> bool {
        self.saving.is_some() || self.acting.is_some() || self.asking
    }

    /// Sends one lid command or the chosen apps, unless a save, another command or the app
    /// list is on its way. Inputs: the command and the context. Output: none.
    fn lid(&mut self, action: KeepAwakeAction, ctx: &mut Ctx) {
        assert!(matches!(
            action,
            KeepAwakeAction::TurnOnLidHold
                | KeepAwakeAction::RemoveLidHelper
                | KeepAwakeAction::PutLidBack
                | KeepAwakeAction::FreezeApps { .. }
        ));
        if self.waiting() {
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
        if !self.edited(stored) || self.waiting() {
            return;
        }
        let query = KeepAwakeQuery::Save {
            picks: self.picks,
            generation: ctx.next_generation(),
        };
        self.saving = Some(query.clone());
        ctx.request(Request::KeepAwake(Box::new(query)));
    }

    /// Asks the engine for the running apps, so the chooser can open on them. Input: the
    /// context. Output: none.
    fn ask_apps(&mut self, ctx: &mut Ctx) {
        if self.waiting() || self.choosing.is_some() {
            return;
        }
        self.asking = true;
        ctx.request(Request::KeepAwake(Box::new(KeepAwakeQuery::Apps)));
    }

    /// Opens the chooser on the running apps the engine listed, or says why they could
    /// not be listed. Inputs: the reply and the context. Output: none.
    fn take_apps(&mut self, result: &Result<KeepAwakeReply, String>, ctx: &mut Ctx) {
        if !self.asking {
            return;
        }
        self.asking = false;
        match result {
            Ok(KeepAwakeReply::Apps(running)) => {
                let listed = (ctx.store.keep_awake.as_ref())
                    .map(|view| view.freeze_apps.clone())
                    .unwrap_or_default();
                self.choosing = Some(Chooser::new(&listed, running.clone()));
            }
            Ok(_) => ctx.say(Message::error(APPS_NOT_READ, "")),
            Err(code) => ctx.say(Message::error(APPS_NOT_READ, refusal(code))),
        }
    }

    /// Handles one key while the chooser is open: move, tick, save the ticked apps or
    /// drop the chooser. Inputs: the key and the context. Output: always handled.
    fn choose_key(&mut self, key: &Key, ctx: &mut Ctx) -> Flow {
        let Some(chooser) = self.choosing.as_mut() else {
            return Flow::Ignored;
        };
        match key {
            Key::Up => chooser.moved(false),
            Key::Down => chooser.moved(true),
            Key::Char(' ') => {
                if !chooser.toggle() {
                    ctx.say(Message::error(APPS_NOT_CHANGED, APPS_TOO_MANY));
                }
            }
            Key::Enter => {
                let apps = chooser.chosen();
                self.choosing = None;
                self.lid(KeepAwakeAction::FreezeApps { apps }, ctx);
            }
            Key::Esc => self.choosing = None,
            _ => {}
        }
        Flow::Handled
    }

    fn take(&mut self, event: &KeepAwakeEvent, ctx: &mut Ctx) {
        if event.query == KeepAwakeQuery::Apps {
            self.take_apps(&event.result, ctx);
            return;
        }
        if event.query == KeepAwakeQuery::Read {
            self.problem = match &event.result {
                Ok(KeepAwakeReply::View(_)) => None,
                Ok(KeepAwakeReply::Done | KeepAwakeReply::Apps(_)) => self.problem.take(),
                Err(code) => Some(code.clone()),
            };
            return;
        }
        if self.acting.as_ref() == Some(&event.query) {
            let title = match self.acting.take() {
                Some(KeepAwakeQuery::Act {
                    action: KeepAwakeAction::FreezeApps { .. },
                    ..
                }) => APPS_NOT_CHANGED,
                Some(KeepAwakeQuery::Act {
                    action: KeepAwakeAction::PutLidBack,
                    ..
                }) => LID_NOT_PUT_BACK,
                _ => LID_NOT_CHANGED,
            };
            if let Err(code) = &event.result {
                ctx.say(Message::error(title, refusal(code)));
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
        let lid_line = match view.platform {
            Platform::MacOs => format!("Lid helper  {}", lid_helper_line(view)),
            Platform::Windows => format!("Lid setting  {LID_CRASH_GAP}"),
            Platform::Linux => String::new(),
        };
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
                if *setting == Setting::Freeze && unavailable(*setting, view).is_none() {
                    let names: Vec<&str> = view
                        .freeze_apps
                        .iter()
                        .map(|app| app.name.as_str())
                        .collect();
                    let listed = if names.is_empty() {
                        NO_APPS.to_owned()
                    } else {
                        names.join(", ")
                    };
                    for part in wrap(&format!("Apps  {listed}"), width.saturating_sub(2).max(8)) {
                        lines.push(Line::raw(format!("  {part}")));
                    }
                }
                if *setting == Setting::Lid && !lid_line.is_empty() {
                    let room = width.saturating_sub(2).max(8);
                    if lid_line.chars().count() <= room {
                        lines.push(Line::raw(format!("  {lid_line}")));
                    } else {
                        for part in wrap(&lid_line, room) {
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

    /// Whether a may open the chooser: the Freeze row is selected and this computer can
    /// freeze apps. Input: the view. Output: the answer.
    fn may_choose(&self, view: &KeepAwakeView) -> bool {
        rows()[self.selected] == Setting::Freeze && unavailable(Setting::Freeze, view).is_none()
    }
}

impl Screen for KeepAwakeSettingsPage {
    fn crumbs(&self) -> Vec<String> {
        vec!["Settings".into(), "Keep awake".into()]
    }

    fn status(&self, ctx: &Ctx) -> Line<'static> {
        if self.asking {
            return Line::styled(
                format!("Listing the running apps{}", ctx.glyphs.ellipsis),
                ctx.theme.muted,
            );
        }
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
        if self.waiting() {
            return Vec::new();
        }
        let vertical = format!("{}{}", ctx.glyphs.up, ctx.glyphs.down);
        let sideways = format!("{}{}", ctx.glyphs.left, ctx.glyphs.right);
        let mut hints = vec![hint(&vertical, "Move"), hint(&sideways, "Change")];
        if self.may_choose(view) {
            hints.push(hint("a", "Choose apps"));
        }
        let offers = lid_offers(view, LidPlace::Settings);
        if let Some(turn_on) = offers.turn_on {
            hints.push(hint("t", turn_on));
        }
        if offers.remove {
            hints.push(hint("d", "Remove lid helper"));
        }
        if view.platform == Platform::Windows {
            hints.push(hint("p", PUT_BACK));
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
        let windows = (ctx.store.keep_awake.as_ref())
            .is_some_and(|view| view.platform == Platform::Windows);
        if self.choosing.is_some() {
            return self.choose_key(key, ctx);
        }
        let Some(stored) = ctx.store.keep_awake.as_ref().map(|view| view.settings) else {
            if self.problem.is_some() && *key == Key::Char('r') {
                ctx.request(Request::KeepAwake(Box::new(KeepAwakeQuery::Read)));
                return Flow::Handled;
            }
            return Flow::Ignored;
        };
        if self.waiting() {
            return Flow::Handled;
        }
        let may_choose = (ctx.store.keep_awake.as_ref()).is_some_and(|view| self.may_choose(view));
        match key {
            Key::Char('a') if may_choose => self.ask_apps(ctx),
            Key::Char('t') if offers.turn_on.is_some() => {
                self.lid(KeepAwakeAction::TurnOnLidHold, ctx);
            }
            Key::Char('d') if offers.remove => self.lid(KeepAwakeAction::RemoveLidHelper, ctx),
            Key::Char('p') if windows => self.lid(KeepAwakeAction::PutLidBack, ctx),
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
                self.asking = false;
                self.choosing = None;
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

    fn modal(&self, ctx: &Ctx) -> Option<(Modal, Vec<Hint>)> {
        let chooser = self.choosing.as_ref()?;
        let room = usize::from(CHOOSER_WIDTH) - 6;
        let mut body: Vec<Line<'static>> = wrap(
            "Ticked apps pause while a hold runs with the lid closed. CrazyAgent, its agents and the system are never listed.",
            room,
        )
        .into_iter()
        .map(|part| Line::styled(part, ctx.theme.muted))
        .collect();
        body.push(Line::default());
        if chooser.offered.is_empty() {
            body.push(Line::raw(
                "No running app can be frozen. Open the app first.",
            ));
        }
        let first = chooser.at.saturating_sub(CHOOSER_ROWS - 1);
        for (at, app) in chooser
            .offered
            .iter()
            .enumerate()
            .skip(first)
            .take(CHOOSER_ROWS)
        {
            let tick = if chooser.ticked[at] { "[x]" } else { "[ ]" };
            let name: String = app.name.chars().take(room.saturating_sub(4)).collect();
            let text = format!("{tick} {name}");
            body.push(if at == chooser.at {
                Line::styled(text, ctx.theme.heading)
            } else {
                Line::raw(text)
            });
        }
        let vertical = format!("{}{}", ctx.glyphs.up, ctx.glyphs.down);
        Some((
            Modal {
                width: CHOOSER_WIDTH,
                title: "Apps to freeze".into(),
                body,
                top: None,
            },
            vec![
                hint(&vertical, "Move"),
                hint("Space", "Tick"),
                hint("Enter", "Save apps"),
                hint("Esc", "Cancel"),
            ],
        ))
    }

    fn place(&self) -> Option<Place> {
        Some(Place::KeepAwakeSettings)
    }
}

#[cfg(all(test, unix))]
mod tests;
