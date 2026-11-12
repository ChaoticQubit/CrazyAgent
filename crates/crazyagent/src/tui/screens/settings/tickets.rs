use crate::engine::owner::{SettingChoices, SettingsSaved, TicketSettingsView};
use crate::protocol::ProjectId;
use crate::tracker::settings::{TicketOverride, TicketSettings};
use crate::tui::client::settings::{SettingsEvent, SettingsJob};
use crate::tui::client::tracker::{TrackerEvent, TrackerJob};
use crate::tui::ctx::Ctx;
use crate::tui::frame::{Hint, Message, draw_lines, hint};
use crate::tui::keys::Key;
use crate::tui::screens::process::Scope;
use crate::tui::screens::settings::top_for;
use crate::tui::screens::settings_accounts::{send, settings_event};
use crate::tui::screens::tickets::sentence;
use crate::tui::screens::{Flow, Place, Screen};
use crate::tui::seam::Arrival;
use crate::tui::text::{cells, indent, join, list_row, restyle, row, styled, wrap};
use crate::tui::theme::Role;
use crate::tui::widgets::TextField;
use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::text::Line;
use serde_json::{Map, Value};

#[cfg(all(test, unix))]
mod tests;

const DAYS_CHARS_MAX: usize = 2;
const LABEL_CELLS: usize = 22;
const NAME_CELLS: usize = 20;

/// The page's words, the desktop's (Settings, Tickets and Project Settings, Tickets on
/// feat/v3-screens): the title, the app page's purpose, the Ticket rules section with when a
/// change applies and the project's line, and the line for a project without tickets. The app
/// purpose names the Tickets tab, where a TUI project sets its own values.
const TITLE: &str = "Tickets";
const PURPOSE: &str = "The ticket settings every project with tickets starts from, until a project sets its own from its Tickets tab.";
const RULES: &str = "Ticket rules";
const EFFECT: &str = "Changes apply to the next ticket action. Tickets already closed stay closed.";
const PROJECT_EFFECT: &str = "A value set here wins over the app value for this project only.";
const NOT_ATTACHED: &str = "Tickets aren't set up for this project yet. Set them up on the Tickets tab, then its settings show here.";

#[derive(Debug, PartialEq)]
enum State {
    Loading,
    Failed(String),
    NotAttached,
    Ready(TicketSettingsView),
}

/// A change the owner is making to the selected key: one of its options (the index one
/// past the options is "Custom" on a key with custom bounds), or a typed number of days.
enum Choosing {
    Option(usize),
    Days(TextField),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pending {
    Read,
    Save,
}

/// The ticket settings page: every ticket setting for the app, or one project's values over
/// the app values, with each option's one-line meaning. A change saves the scope's values
/// with that key set, at the version read; `u` clears the key so it follows the scope
/// below. One request runs at a time.
pub(in crate::tui) struct TicketSettingsPage {
    scope: Scope,
    state: State,
    selected: usize,
    choosing: Option<Choosing>,
    pending: Option<Pending>,
}

impl TicketSettingsPage {
    /// The page for one scope, loading until the engine answers. Input: the scope.
    /// Output: the screen; `shown` sends the first read.
    pub fn new(scope: Scope) -> Self {
        Self {
            scope,
            state: State::Loading,
            selected: 0,
            choosing: None,
            pending: None,
        }
    }

    fn project(&self) -> Option<ProjectId> {
        match &self.scope {
            Scope::App => None,
            Scope::Project { id, .. } => Some(id.clone()),
        }
    }

    fn ask(&mut self, job: TrackerJob, pending: Pending, ctx: &mut Ctx) {
        assert!(self.pending.is_none());
        self.pending = Some(pending);
        send(ctx, SettingsJob::Tracker(Box::new(job)));
    }

    fn read(&mut self, ctx: &mut Ctx) {
        if self.pending.is_some() {
            return;
        }
        let project = self.project();
        self.ask(TrackerJob::Settings { project }, Pending::Read, ctx);
    }

    fn current(&self) -> Option<(&TicketSettingsView, &SettingChoices)> {
        match &self.state {
            State::Ready(view) => view.choices.get(self.selected).map(|choice| (view, choice)),
            _ => None,
        }
    }

    fn save(&mut self, key: &str, value: Option<Value>, ctx: &mut Ctx) {
        let Some((view, _)) = self.current() else {
            return;
        };
        let version = view.version;
        let Some(values) = with(&view.values, key, value) else {
            ctx.say(Message::error(
                "Could not save the ticket settings.",
                &sentence("policy_value_invalid"),
            ));
            return;
        };
        let project = self.project();
        self.ask(
            TrackerJob::SaveSettings {
                project,
                values,
                version,
            },
            Pending::Save,
            ctx,
        );
    }

    fn start(&mut self) {
        let Some((view, choice)) = self.current() else {
            return;
        };
        if count(choice) == 0 {
            return;
        }
        let effective = setting(&view.effective, &choice.key);
        let at = match choice
            .options
            .iter()
            .position(|option| Some(option) == effective.as_ref())
        {
            Some(at) => at,
            None if choice.custom.is_some() => choice.options.len(),
            None => 0,
        };
        self.choosing = Some(Choosing::Option(at));
    }

    fn reset(&mut self, ctx: &mut Ctx) {
        let Some((view, choice)) = self.current() else {
            return;
        };
        let key = choice.key.clone();
        if here(&view.values, &key).is_none() {
            ctx.say(Message::warning(
                "Nothing to reset.",
                match self.scope {
                    Scope::App => "This setting already uses the proposed default.",
                    Scope::Project { .. } => "This setting already uses the app value.",
                },
            ));
            return;
        }
        self.save(&key, None, ctx);
    }

    fn choose_key(&mut self, key: &Key, ctx: &mut Ctx) -> Flow {
        let Some((view, choice)) = self.current() else {
            self.choosing = None;
            return Flow::Handled;
        };
        let stored = here(&view.values, &choice.key);
        let choice = choice.clone();
        let total = count(&choice);
        if *key == Key::Esc || total == 0 {
            self.choosing = None;
            return Flow::Handled;
        }
        let mut open_days = false;
        let value = match self.choosing.as_mut() {
            Some(Choosing::Option(at)) => match key {
                Key::Left => {
                    *at = (*at + total - 1) % total;
                    None
                }
                Key::Right => {
                    *at = (*at + 1) % total;
                    None
                }
                Key::Enter => {
                    let option = choice.options.get(*at).cloned();
                    open_days = option.is_none();
                    option
                }
                _ => None,
            },
            Some(Choosing::Days(field)) => match key {
                Key::Enter => match days(&choice, field.value()) {
                    Ok(days) => Some(Value::from(days)),
                    Err(problem) => {
                        field.error = Some(problem);
                        None
                    }
                },
                key => {
                    if field.key(key) {
                        field.error = None;
                    }
                    None
                }
            },
            None => None,
        };
        if open_days {
            let mut field = TextField::new("Days", DAYS_CHARS_MAX);
            if let Some(days) = stored.as_ref().and_then(Value::as_u64) {
                field.set(&days.to_string());
            }
            self.choosing = Some(Choosing::Days(field));
        }
        if let Some(value) = value {
            self.choosing = None;
            if stored.as_ref() != Some(&value) {
                self.save(&choice.key, Some(value), ctx);
            }
        }
        Flow::Handled
    }

    fn apply(&mut self, event: &TrackerEvent, ctx: &mut Ctx) {
        let project = self.project();
        match (event, self.pending) {
            (
                TrackerEvent::Settings {
                    project: scope,
                    result,
                },
                Some(Pending::Read),
            ) if *scope == project => {
                self.pending = None;
                match result {
                    Ok(view) => {
                        self.selected = self.selected.min(view.choices.len().saturating_sub(1));
                        self.state = State::Ready(view.clone());
                    }
                    Err(code) if matches!(self.state, State::Ready(_)) => ctx.say(Message::error(
                        "Could not read the ticket settings.",
                        &sentence(code),
                    )),
                    Err(code) if code == "project_not_attached" => {
                        self.state = State::NotAttached;
                    }
                    Err(code) => self.state = State::Failed(sentence(code)),
                }
            }
            (
                TrackerEvent::Saved {
                    project: scope,
                    result,
                },
                Some(Pending::Save),
            ) if *scope == project => {
                self.pending = None;
                match result {
                    Ok(SettingsSaved::Saved { .. }) => ctx.say(Message::success(
                        "Saved.",
                        "It applies from the next change on.",
                    )),
                    Ok(SettingsSaved::Stale { .. }) => ctx.say(Message::warning(
                        "Changed somewhere else.",
                        "These are the current values; change it again if you still want to.",
                    )),
                    Err(code) => ctx.say(Message::error(
                        "Could not save the ticket settings.",
                        &sentence(code),
                    )),
                }
                self.read(ctx);
            }
            _ => {}
        }
    }

    /// The page's lines. Inputs: the content width and the context. Output: the lines, the
    /// cursor of an open days field relative to the first line, and the first and last line
    /// of the part to keep in view: the selected setting through its choices or its line.
    fn body(
        &self,
        limit: usize,
        ctx: &Ctx,
    ) -> (Vec<Line<'static>>, Option<Position>, (usize, usize)) {
        let theme = &ctx.theme;
        let glyphs = ctx.glyphs;
        let muted = |text: &str| {
            wrap(text, limit)
                .into_iter()
                .map(|part| styled(part, Role::Muted, theme))
                .collect::<Vec<_>>()
        };
        let mut lines = vec![styled(TITLE, Role::Heading, theme)];
        let effect = match &self.scope {
            Scope::App => {
                lines.extend(muted(PURPOSE));
                EFFECT.to_owned()
            }
            Scope::Project { .. } => format!("{EFFECT} {PROJECT_EFFECT}"),
        };
        lines.push(Line::default());
        lines.push(styled(RULES, Role::Heading, theme));
        lines.extend(muted(&effect));
        lines.push(Line::default());
        let view = match &self.state {
            State::Loading => {
                lines.push(ctx.loading("Reading the ticket settings"));
                return (lines, None, (0, 0));
            }
            State::NotAttached => {
                lines.extend(muted(NOT_ATTACHED));
                return (lines, None, (0, 0));
            }
            State::Failed(problem) => {
                lines.extend(
                    wrap(
                        &format!("Could not read the ticket settings. {problem}"),
                        limit,
                    )
                    .into_iter()
                    .map(|part| styled(part, Role::Error, theme)),
                );
                lines.push(styled("Press r to try again.", Role::Muted, theme));
                return (lines, None, (0, 0));
            }
            State::Ready(view) => view,
        };
        let widths = [
            LABEL_CELLS,
            limit.saturating_sub(2 + LABEL_CELLS + 2).max(1),
        ];
        lines.push(join([
            Line::raw("  "),
            restyle(
                cells(
                    &widths,
                    vec![Line::raw("Setting"), Line::raw("Value")],
                    2,
                    glyphs,
                ),
                theme.muted,
            ),
        ]));
        let mut first = lines.len();
        for (index, choice) in view.choices.iter().enumerate() {
            let shown = self.value_text(view, choice, index);
            if index == self.selected {
                first = lines.len();
            }
            lines.push(list_row(
                cells(
                    &widths,
                    vec![Line::raw(label(&choice.key)), Line::raw(shown)],
                    2,
                    glyphs,
                ),
                limit,
                index == self.selected,
                true,
                theme,
                glyphs,
            ));
        }
        lines.push(Line::default());
        let Some(choice) = view.choices.get(self.selected) else {
            let last = lines.len() - 1;
            return (lines, None, (first.min(last), last));
        };
        let mut cursor = None;
        match &self.choosing {
            Some(Choosing::Option(at)) => {
                for (index, option) in choice.options.iter().enumerate() {
                    let (name, meaning) = option_text(&choice.key, option);
                    lines.push(option_line(&name, &meaning, index == *at, limit, ctx));
                }
                if let Some(bounds) = &choice.custom {
                    lines.push(option_line(
                        "Custom",
                        &format!("Type a length from {} to {} days.", bounds.min, bounds.max),
                        *at == choice.options.len(),
                        limit,
                        ctx,
                    ));
                }
            }
            Some(Choosing::Days(field)) => {
                let (field_lines, at) = field.lines(limit.max(6), true, theme, glyphs);
                let first = u16::try_from(lines.len()).unwrap_or(u16::MAX);
                cursor = at.map(|at| Position::new(at.x, first.saturating_add(at.y)));
                lines.extend(field_lines);
            }
            None => {
                let effective = setting(&view.effective, &choice.key).unwrap_or(Value::Null);
                let (_, meaning) = option_text(&choice.key, &effective);
                lines.extend(
                    wrap(&meaning, limit)
                        .into_iter()
                        .map(|part| styled(part, Role::Muted, theme)),
                );
            }
        }
        let last = lines.len() - 1;
        assert!(first <= last);
        (lines, cursor, (first, last))
    }

    fn value_text(
        &self,
        view: &TicketSettingsView,
        choice: &SettingChoices,
        index: usize,
    ) -> String {
        if let Some(Choosing::Option(at)) = &self.choosing
            && index == self.selected
        {
            let name = match choice.options.get(*at) {
                Some(option) => option_text(&choice.key, option).0,
                None => "Custom".to_owned(),
            };
            return format!("{name} \u{2190}\u{2192}");
        }
        match here(&view.values, &choice.key) {
            Some(value) => option_text(&choice.key, &value).0,
            None => {
                let below = setting(&view.below, &choice.key).unwrap_or(Value::Null);
                let name = option_text(&choice.key, &below).0;
                match self.scope {
                    Scope::App => format!("Default ({name})"),
                    Scope::Project { .. } => format!("App value ({name})"),
                }
            }
        }
    }
}

impl Screen for TicketSettingsPage {
    fn wraps_hints(&self) -> bool {
        true
    }

    fn crumbs(&self) -> Vec<String> {
        match &self.scope {
            Scope::App => vec!["Settings".into(), "Tickets".into()],
            Scope::Project { name, .. } => {
                vec![name.clone(), "Tickets".into(), "Settings".into()]
            }
        }
    }

    fn hints(&self, ctx: &Ctx) -> Vec<Hint> {
        match &self.choosing {
            Some(Choosing::Option(_)) => {
                return vec![
                    hint(&format!("{}{}", ctx.glyphs.left, ctx.glyphs.right), "Pick"),
                    hint("Enter", "Save"),
                    hint("Esc", "Cancel"),
                ];
            }
            Some(Choosing::Days(_)) => return vec![hint("Enter", "Save"), hint("Esc", "Cancel")],
            None => {}
        }
        match &self.state {
            State::Loading => return vec![hint("Esc", "Back")],
            State::Failed(_) => return vec![hint("r", "Try again"), hint("Esc", "Back")],
            State::NotAttached => return vec![hint("r", "Read again"), hint("Esc", "Back")],
            State::Ready(_) => {}
        }
        let mut hints = vec![hint("Enter", "Change")];
        if let Some((view, choice)) = self.current()
            && here(&view.values, &choice.key).is_some()
        {
            hints.push(match self.scope {
                Scope::App => hint("u", "Use the default"),
                Scope::Project { .. } => hint("u", "Use the app value"),
            });
        }
        if matches!(self.scope, Scope::Project { .. }) {
            hints.push(hint("a", "App values"));
        }
        hints.push(hint(
            &format!("{}{}", ctx.glyphs.up, ctx.glyphs.down),
            "Move",
        ));
        hints.push(hint("r", "Read again"));
        hints.push(hint("Esc", "Back"));
        hints
    }

    fn typing(&self) -> bool {
        matches!(self.choosing, Some(Choosing::Days(_)))
    }

    fn key(&mut self, key: &Key, ctx: &mut Ctx) -> Flow {
        if self.choosing.is_some() {
            return self.choose_key(key, ctx);
        }
        let total = match &self.state {
            State::Ready(view) => view.choices.len(),
            _ => 0,
        };
        let idle = total > 0 && self.pending.is_none();
        match key {
            Key::Char('r') => self.read(ctx),
            Key::Up => self.selected = self.selected.saturating_sub(1),
            Key::Down => self.selected = (self.selected + 1).min(total.saturating_sub(1)),
            Key::Enter | Key::Char(' ') | Key::Left | Key::Right if idle => self.start(),
            Key::Char('u') if idle => self.reset(ctx),
            Key::Char('a') if matches!(self.scope, Scope::Project { .. }) => {
                return Flow::Open(Place::TicketSettings { project: None });
            }
            _ => return Flow::Ignored,
        }
        Flow::Handled
    }

    fn arrive(&mut self, arrival: &Arrival, ctx: &mut Ctx) -> Flow {
        if let Some(SettingsEvent::Tracker(event)) = settings_event(arrival) {
            self.apply(event, ctx);
        }
        Flow::Handled
    }

    fn shown(&mut self, ctx: &mut Ctx) {
        if self.state == State::Loading {
            self.read(ctx);
        }
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect, ctx: &Ctx) -> Option<Position> {
        let limit = usize::from(area.width).saturating_sub(4).max(8);
        let (content, cursor, (first, last)) = self.body(limit, ctx);
        let top = top_for(first, last, usize::from(area.height));
        let shown: Vec<Line<'static>> = content.into_iter().skip(top).collect();
        draw_lines(frame.buffer_mut(), area, &indent(shown, 2));
        let top = u16::try_from(top).unwrap_or(u16::MAX);
        cursor.and_then(|at| {
            at.y.checked_sub(top)
                .map(|y| Position::new(area.x + 2 + at.x, area.y + y))
        })
    }

    fn busy(&self, _ctx: &Ctx) -> bool {
        self.pending.is_some()
    }

    fn draft(&self) -> bool {
        matches!(self.choosing, Some(Choosing::Days(_)))
    }

    fn loss(&self) -> &'static str {
        "The number you typed will be lost."
    }

    fn place(&self) -> Option<Place> {
        Some(Place::TicketSettings {
            project: match &self.scope {
                Scope::App => None,
                Scope::Project { id, name } => Some((id.clone(), name.clone())),
            },
        })
    }
}

fn count(choice: &SettingChoices) -> usize {
    choice.options.len() + usize::from(choice.custom.is_some())
}

fn option_line(name: &str, meaning: &str, chosen: bool, limit: usize, ctx: &Ctx) -> Line<'static> {
    let line = row(
        limit.saturating_sub(2),
        Line::raw(format!("{name:<NAME_CELLS$}{meaning}")),
        Line::default(),
        ctx.glyphs,
    );
    list_row(line, limit, chosen, true, &ctx.theme, ctx.glyphs)
}

/// The value one scope sets for a key, if it sets it. Inputs: the scope's values and the
/// key. Output: the value, or None when the key follows the scope below.
fn here(values: &TicketOverride, key: &str) -> Option<Value> {
    serde_json::to_value(values)
        .ok()?
        .get(key)
        .filter(|value| !value.is_null())
        .cloned()
}

/// The value a key has in a full set of ticket settings. Inputs: the settings and the key.
/// Output: the value, or None for a key the settings do not have.
fn setting(settings: &TicketSettings, key: &str) -> Option<Value> {
    let document = serde_json::to_value(settings).ok()?;
    document
        .get("policy")
        .and_then(|policy| policy.get(key))
        .or_else(|| document.get(key))
        .cloned()
}

/// A scope's values with one key set or cleared. Inputs: the values, the key and the new
/// value, None to clear it. Output: the values, or None for a value the settings refuse.
fn with(values: &TicketOverride, key: &str, value: Option<Value>) -> Option<TicketOverride> {
    let mut document = match serde_json::to_value(values) {
        Ok(Value::Object(document)) => document,
        _ => Map::new(),
    };
    match value {
        Some(value) => document.insert(key.to_owned(), value),
        None => document.remove(key),
    };
    serde_json::from_value(Value::Object(document)).ok()
}

/// Checks a typed cycle length against the key's custom bounds. Inputs: the key's choices
/// and the text. Output: the days, or the sentence saying what to type.
fn days(choice: &SettingChoices, text: &str) -> Result<u8, String> {
    let Some(bounds) = &choice.custom else {
        return Err("This setting takes one of its options only.".into());
    };
    match text.trim().parse::<u8>() {
        Ok(days) if (bounds.min..=bounds.max).contains(&days) => Ok(days),
        _ => Err(format!(
            "Use a whole number from {} to {}.",
            bounds.min, bounds.max
        )),
    }
}

/// The name of a ticket setting. Input: the key. Output: the name, or the key itself for
/// one this version does not know.
fn label(key: &str) -> String {
    match key {
        "close" => "Who closes tickets",
        "create" => "New tickets",
        "agent_edit" => "Agent edits",
        "cycle_days" => "Cycle length",
        "rollover" => "Unfinished work",
        "import" => "Roadmap import",
        "manual_evidence" => "Manual checks",
        "ci_evidence" => "CI checks",
        other => other,
    }
    .to_owned()
}

/// One option of a ticket setting: its short name and the line saying what it does.
/// Inputs: the key and the option's value. Output: the name and the line; an option this
/// version does not know shows its value and no line.
fn option_text(key: &str, value: &Value) -> (String, String) {
    if key == "cycle_days"
        && let Some(days) = value.as_u64()
    {
        return (
            format!("{days} days"),
            format!("Each cycle runs {days} days."),
        );
    }
    let (name, meaning) = match (key, value.as_str().unwrap_or("")) {
        ("close", "agents") => ("Agents", "Any agent may close a ticket."),
        ("close", "evidence") => (
            "Evidence",
            "A ticket closes once every criterion is met and every check has passing evidence.",
        ),
        ("close", "owner") => ("Owner", "Only you close tickets."),
        ("create", "direct") => ("Direct", "New tickets start where their creator puts them."),
        ("create", "triage") => (
            "Triage",
            "Tickets agents create wait in triage until you accept them.",
        ),
        ("create", "owner_only") => ("Owner only", "Only you create tickets."),
        ("agent_edit", "any") => ("Anything", "Agents may change any field of any ticket."),
        ("agent_edit", "status_and_criteria") => (
            "Status, criteria",
            "On tickets they did not create, agents change only the status and criteria.",
        ),
        ("agent_edit", "none") => (
            "Nothing",
            "Agents may not change tickets they did not create.",
        ),
        ("rollover", "auto") => (
            "Moves on",
            "Unfinished tickets move to the next cycle when a cycle closes.",
        ),
        ("rollover", "ask") => ("Ask", "You choose what moves on when a cycle closes."),
        ("rollover", "off") => ("Stays", "Unfinished tickets stay in the closed cycle."),
        ("import", "auto_once") => (
            "Once, on its own",
            "The project roadmap is imported once, when planning writes it.",
        ),
        ("import", "on_request") => ("On request", "The roadmap is imported only when you ask."),
        ("manual_evidence", "owner") => ("Owner", "Only you record evidence on a manual check."),
        ("manual_evidence", "agents") => (
            "Agents too",
            "Agents may record evidence on a manual check.",
        ),
        ("ci_evidence", "engine") => (
            "Engine",
            "CI evidence comes from CrazyAgent's own runs, or from you.",
        ),
        ("ci_evidence", "owner") => ("Owner", "Only you record CI evidence."),
        ("ci_evidence", "agents") => ("Agents too", "Agents may record CI evidence."),
        _ => return (value.to_string(), String::new()),
    };
    (name.to_owned(), meaning.to_owned())
}
