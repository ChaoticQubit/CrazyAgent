use crate::checks::settings::{ChecksChoice, ChecksSettings, SettingChoice, choices};
use crate::checks::{ProfileId, ProfileState, ProfileSummary};
use crate::engine::owner::{ChecksSettingsView, SettingsSaved};
use crate::protocol::ProjectId;
use crate::tui::client::checks::{ChecksEvent, ChecksJob};
use crate::tui::client::settings::{SettingsEvent, SettingsJob};
use crate::tui::ctx::Ctx;
use crate::tui::frame::{Hint, Message, Modal, draw_lines, hint};
use crate::tui::keys::Key;
use crate::tui::screens::process::Scope;
use crate::tui::screens::settings_accounts::{send, settings_event};
use crate::tui::screens::tests::sentence;
use crate::tui::screens::{Flow, Place, Screen};
use crate::tui::seam::Arrival;
use crate::tui::text::{cells, indent, join, list_row, restyle, row, styled, wrap};
use crate::tui::theme::Role;
use crate::tui::widgets::buttons;
use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::text::Line;
use serde_json::{Map, Value};

#[cfg(all(test, unix))]
mod tests;

const LABEL_CELLS: usize = 23;
const NAME_CELLS: usize = 16;
const PROFILE_SHOWN: usize = 12;
const STATE_CELLS: usize = 10;
const MODAL_WIDTH: u16 = 58;

#[derive(Debug, PartialEq)]
enum State {
    Loading,
    Failed(String),
    Ready(ChecksSettingsView),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pending {
    Read,
    Save,
    Profiles,
    Profile,
}

/// A profile action the owner asked for and has not confirmed yet: approve or revoke one
/// stored profile.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Confirm {
    approve: bool,
    profile: ProfileSummary,
}

/// The check settings page: every checks setting for the app, or one project's values over
/// the app values, with each option's one-line meaning; a project's page also lists its
/// stored check profiles, where the owner approves or revokes one after confirming it. A
/// change saves the scope's values with that key set, at the version read; `u` clears the
/// key so it follows the scope below. One request runs at a time.
pub(in crate::tui) struct CheckSettingsPage {
    scope: Scope,
    state: State,
    profiles: Vec<ProfileSummary>,
    profiles_problem: Option<String>,
    selected: usize,
    choosing: Option<usize>,
    confirm: Option<Confirm>,
    pending: Option<Pending>,
    profiles_due: bool,
}

impl CheckSettingsPage {
    /// The page for one scope, loading until the engine answers. Input: the scope.
    /// Output: the screen; `shown` sends the first read.
    pub fn new(scope: Scope) -> Self {
        let profiles_due = matches!(scope, Scope::Project { .. });
        Self {
            scope,
            state: State::Loading,
            profiles: Vec::new(),
            profiles_problem: None,
            selected: 0,
            choosing: None,
            confirm: None,
            pending: None,
            profiles_due,
        }
    }

    fn project(&self) -> Option<ProjectId> {
        match &self.scope {
            Scope::App => None,
            Scope::Project { id, .. } => Some(id.clone()),
        }
    }

    fn ask(&mut self, job: ChecksJob, pending: Pending, ctx: &mut Ctx) {
        assert!(self.pending.is_none());
        self.pending = Some(pending);
        send(ctx, SettingsJob::Checks(Box::new(job)));
    }

    fn read(&mut self, ctx: &mut Ctx) {
        if self.pending.is_some() {
            return;
        }
        let project = self.project();
        self.profiles_due = project.is_some();
        self.ask(ChecksJob::Settings { project }, Pending::Read, ctx);
    }

    fn read_profiles(&mut self, ctx: &mut Ctx) {
        let Some(project) = self.project() else {
            self.profiles_due = false;
            return;
        };
        if self.pending.is_some() {
            return;
        }
        self.profiles_due = false;
        self.ask(ChecksJob::Profiles { project }, Pending::Profiles, ctx);
    }

    fn rows(&self) -> usize {
        match &self.state {
            State::Ready(_) => choices().len() + self.profiles.len(),
            _ => 0,
        }
    }

    fn current(&self) -> Option<(&ChecksSettingsView, SettingChoice)> {
        match &self.state {
            State::Ready(view) => choices()
                .into_iter()
                .nth(self.selected)
                .map(|choice| (view, choice)),
            _ => None,
        }
    }

    fn chosen_profile(&self) -> Option<&ProfileSummary> {
        self.selected
            .checked_sub(choices().len())
            .and_then(|at| self.profiles.get(at))
    }

    fn save(&mut self, key: &str, value: Option<Value>, ctx: &mut Ctx) {
        let Some((view, _)) = self.current() else {
            return;
        };
        let version = view.version;
        let Some(values) = with(&view.values, key, value) else {
            ctx.say(Message::error(
                "Could not save the check settings.",
                &sentence("checks_setting_invalid"),
            ));
            return;
        };
        let project = self.project();
        self.ask(
            ChecksJob::SaveSettings {
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
        let effective = setting(&view.effective, choice.key);
        let at = choice
            .options
            .iter()
            .position(|option| Some(option) == effective.as_ref())
            .unwrap_or(0);
        self.choosing = Some(at);
    }

    fn reset(&mut self, ctx: &mut Ctx) {
        let Some((view, choice)) = self.current() else {
            return;
        };
        if here(&view.values, choice.key).is_none() {
            ctx.say(Message::warning(
                "Nothing to reset.",
                match self.scope {
                    Scope::App => "This setting already uses the proposed default.",
                    Scope::Project { .. } => "This setting already uses the app value.",
                },
            ));
            return;
        }
        self.save(choice.key, None, ctx);
    }

    fn choose_key(&mut self, key: &Key, ctx: &mut Ctx) -> Flow {
        let Some((view, choice)) = self.current() else {
            self.choosing = None;
            return Flow::Handled;
        };
        let stored = here(&view.values, choice.key);
        let total = choice.options.len();
        let Some(at) = self.choosing.as_mut() else {
            return Flow::Handled;
        };
        if *key == Key::Esc || total == 0 {
            self.choosing = None;
            return Flow::Handled;
        }
        match key {
            Key::Left => *at = (*at + total - 1) % total,
            Key::Right => *at = (*at + 1) % total,
            Key::Enter => {
                let value = choice.options.get(*at).cloned();
                self.choosing = None;
                if let Some(value) = value
                    && stored.as_ref() != Some(&value)
                {
                    self.save(choice.key, Some(value), ctx);
                }
            }
            _ => {}
        }
        Flow::Handled
    }

    fn ask_profile(&mut self, approve: bool, ctx: &mut Ctx) {
        let Some(profile) = self.chosen_profile().cloned() else {
            return;
        };
        let allowed = if approve {
            profile.state != ProfileState::Current
        } else {
            profile.state != ProfileState::Revoked
        };
        if !allowed {
            ctx.say(Message::warning(
                "Nothing to change.",
                if approve {
                    "This profile is already the current one."
                } else {
                    "This profile is already revoked."
                },
            ));
            return;
        }
        self.confirm = Some(Confirm { approve, profile });
    }

    fn confirm_key(&mut self, key: &Key, ctx: &mut Ctx) -> Flow {
        match key {
            Key::Esc => self.confirm = None,
            Key::Enter => {
                let Some(confirm) = self.confirm.take() else {
                    return Flow::Handled;
                };
                let Some(project) = self.project() else {
                    return Flow::Handled;
                };
                if self.pending.is_some() {
                    self.confirm = Some(confirm);
                    return Flow::Handled;
                }
                let profile = confirm.profile.id;
                let job = if confirm.approve {
                    ChecksJob::Approve { project, profile }
                } else {
                    ChecksJob::Revoke { project, profile }
                };
                self.ask(job, Pending::Profile, ctx);
            }
            _ => {}
        }
        Flow::Handled
    }

    fn apply(&mut self, event: &ChecksEvent, ctx: &mut Ctx) {
        let project = self.project();
        match (event, self.pending) {
            (
                ChecksEvent::Settings {
                    project: scope,
                    result,
                },
                Some(Pending::Read),
            ) if *scope == project => {
                self.pending = None;
                match result {
                    Ok(view) => self.state = State::Ready(view.clone()),
                    Err(code) if matches!(self.state, State::Ready(_)) => ctx.say(Message::error(
                        "Could not read the check settings.",
                        &sentence(code),
                    )),
                    Err(code) => self.state = State::Failed(sentence(code)),
                }
                if self.profiles_due {
                    self.read_profiles(ctx);
                }
            }
            (
                ChecksEvent::Saved {
                    project: scope,
                    result,
                },
                Some(Pending::Save),
            ) if *scope == project => {
                self.pending = None;
                match result {
                    Ok(SettingsSaved::Saved { .. }) => ctx.say(Message::success(
                        "Saved.",
                        "Check runs and Accept follow it from now on.",
                    )),
                    Ok(SettingsSaved::Stale { .. }) => ctx.say(Message::warning(
                        "Changed somewhere else.",
                        "These are the current values; change it again if you still want to.",
                    )),
                    Err(code) => ctx.say(Message::error(
                        "Could not save the check settings.",
                        &sentence(code),
                    )),
                }
                self.read(ctx);
            }
            (
                ChecksEvent::Profiles {
                    project: scope,
                    result,
                },
                Some(Pending::Profiles),
            ) if Some(scope) == project.as_ref() => {
                self.pending = None;
                match result {
                    Ok(rows) => {
                        self.profiles = rows.clone();
                        self.profiles_problem = None;
                    }
                    Err(code) => self.profiles_problem = Some(sentence(code)),
                }
            }
            (
                ChecksEvent::Profile {
                    project: scope,
                    approve,
                    result,
                    ..
                },
                Some(Pending::Profile),
            ) if Some(scope) == project.as_ref() => {
                self.pending = None;
                match (result, approve) {
                    (Ok(()), true) => ctx.say(Message::success(
                        "Approved.",
                        "Checks run under this profile from now on.",
                    )),
                    (Ok(()), false) => ctx.say(Message::success(
                        "Revoked.",
                        "No check runs under this profile from now on.",
                    )),
                    (Err(code), _) => ctx.say(Message::error(
                        "Could not change the check profile.",
                        &sentence(code),
                    )),
                }
                self.read_profiles(ctx);
            }
            _ => {}
        }
        self.selected = self.selected.min(self.rows().saturating_sub(1));
    }

    /// The page's lines. Inputs: the content width and the context. Output: the lines.
    fn body(&self, limit: usize, ctx: &Ctx) -> Vec<Line<'static>> {
        let theme = &ctx.theme;
        let glyphs = ctx.glyphs;
        let (title, intro) = match &self.scope {
            Scope::App => (
                "Check settings".to_owned(),
                "Every project follows these unless it sets its own.".to_owned(),
            ),
            Scope::Project { name, .. } => (
                format!("Check settings for {name}"),
                format!(
                    "Only {name} follows the values set here; the rest follow Settings / Checks."
                ),
            ),
        };
        let mut lines = vec![styled(title, Role::Accent, theme)];
        lines.extend(
            wrap(&intro, limit)
                .into_iter()
                .map(|part| styled(part, Role::Muted, theme)),
        );
        lines.push(Line::default());
        let view = match &self.state {
            State::Loading => {
                lines.push(ctx.loading("Reading the check settings"));
                return lines;
            }
            State::Failed(problem) => {
                lines.extend(
                    wrap(
                        &format!("Could not read the check settings. {problem}"),
                        limit,
                    )
                    .into_iter()
                    .map(|part| styled(part, Role::Error, theme)),
                );
                lines.push(styled("Press r to try again.", Role::Muted, theme));
                return lines;
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
        let keys = choices();
        for (index, choice) in keys.iter().enumerate() {
            let shown = self.value_text(view, choice, index);
            lines.push(list_row(
                cells(
                    &widths,
                    vec![Line::raw(label(choice.key)), Line::raw(shown)],
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
        if let Some(choice) = keys.get(self.selected) {
            match self.choosing {
                Some(at) => {
                    for (index, option) in choice.options.iter().enumerate() {
                        let (name, meaning) = option_text(choice.key, option);
                        lines.push(option_line(&name, &meaning, index == at, limit, ctx));
                    }
                }
                None => {
                    let effective = setting(&view.effective, choice.key).unwrap_or(Value::Null);
                    let (_, meaning) = option_text(choice.key, &effective);
                    lines.extend(
                        wrap(&meaning, limit)
                            .into_iter()
                            .map(|part| styled(part, Role::Muted, theme)),
                    );
                }
            }
        }
        if matches!(self.scope, Scope::Project { .. }) {
            lines.push(Line::default());
            lines.extend(self.profile_lines(keys.len(), limit, ctx));
        }
        lines
    }

    fn profile_lines(&self, first: usize, limit: usize, ctx: &Ctx) -> Vec<Line<'static>> {
        let theme = &ctx.theme;
        let mut lines = vec![styled("Check profiles", Role::Accent, theme)];
        if let Some(problem) = &self.profiles_problem {
            lines.extend(
                wrap(&format!("Could not read the profiles. {problem}"), limit)
                    .into_iter()
                    .map(|part| styled(part, Role::Error, theme)),
            );
            return lines;
        }
        if self.profiles.is_empty() {
            lines.extend(
                wrap(
                    "No profile yet. A master or agent proposes one, or you propose one with crazyagent checks; it asks you before it runs anything.",
                    limit,
                )
                .into_iter()
                .map(|part| styled(part, Role::Muted, theme)),
            );
            return lines;
        }
        for (offset, profile) in self.profiles.iter().enumerate() {
            let line = row(
                limit.saturating_sub(2),
                Line::raw(profile_text(profile)),
                Line::default(),
                ctx.glyphs,
            );
            lines.push(list_row(
                line,
                limit,
                first + offset == self.selected,
                true,
                theme,
                ctx.glyphs,
            ));
        }
        lines
    }

    fn value_text(
        &self,
        view: &ChecksSettingsView,
        choice: &SettingChoice,
        index: usize,
    ) -> String {
        if let Some(at) = self.choosing
            && index == self.selected
        {
            let name = choice
                .options
                .get(at)
                .map_or_else(String::new, |option| option_text(choice.key, option).0);
            return format!("{name} \u{2190}\u{2192}");
        }
        match here(&view.values, choice.key) {
            Some(value) => option_text(choice.key, &value).0,
            None => {
                let below = setting(&view.below, choice.key).unwrap_or(Value::Null);
                let name = option_text(choice.key, &below).0;
                match self.scope {
                    Scope::App => format!("Default ({name})"),
                    Scope::Project { .. } => format!("App value ({name})"),
                }
            }
        }
    }
}

impl Screen for CheckSettingsPage {
    fn crumbs(&self) -> Vec<String> {
        match &self.scope {
            Scope::App => vec!["Settings".into(), "Checks".into()],
            Scope::Project { name, .. } => {
                vec![name.clone(), "Tests".into(), "Settings".into()]
            }
        }
    }

    fn hints(&self, ctx: &Ctx) -> Vec<Hint> {
        if self.choosing.is_some() {
            return vec![
                hint(&format!("{}{}", ctx.glyphs.left, ctx.glyphs.right), "Pick"),
                hint("Enter", "Save"),
                hint("Esc", "Cancel"),
            ];
        }
        match &self.state {
            State::Loading => return vec![hint("Esc", "Back")],
            State::Failed(_) => return vec![hint("r", "Try again"), hint("Esc", "Back")],
            State::Ready(_) => {}
        }
        let mut hints = Vec::new();
        match self.chosen_profile() {
            Some(profile) => {
                if profile.state != ProfileState::Current {
                    hints.push(hint("p", "Approve"));
                }
                if profile.state != ProfileState::Revoked {
                    hints.push(hint("v", "Revoke"));
                }
            }
            None => {
                hints.push(hint("Enter", "Change"));
                if let Some((view, choice)) = self.current()
                    && here(&view.values, choice.key).is_some()
                {
                    hints.push(match self.scope {
                        Scope::App => hint("u", "Use the default"),
                        Scope::Project { .. } => hint("u", "Use the app value"),
                    });
                }
            }
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

    fn key(&mut self, key: &Key, ctx: &mut Ctx) -> Flow {
        if self.confirm.is_some() {
            return self.confirm_key(key, ctx);
        }
        if self.choosing.is_some() {
            return self.choose_key(key, ctx);
        }
        let total = self.rows();
        let idle = total > 0 && self.pending.is_none();
        let on_profile = self.chosen_profile().is_some();
        match key {
            Key::Char('r') => self.read(ctx),
            Key::Up => self.selected = self.selected.saturating_sub(1),
            Key::Down => self.selected = (self.selected + 1).min(total.saturating_sub(1)),
            Key::Enter | Key::Char(' ') | Key::Left | Key::Right if idle && !on_profile => {
                self.start()
            }
            Key::Char('u') if idle && !on_profile => self.reset(ctx),
            Key::Char('p') if on_profile => self.ask_profile(true, ctx),
            Key::Char('v') if on_profile => self.ask_profile(false, ctx),
            Key::Char('a') if matches!(self.scope, Scope::Project { .. }) => {
                return Flow::Open(Place::CheckSettings { project: None });
            }
            _ => return Flow::Ignored,
        }
        Flow::Handled
    }

    fn arrive(&mut self, arrival: &Arrival, ctx: &mut Ctx) -> Flow {
        if let Some(SettingsEvent::Checks(event)) = settings_event(arrival) {
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
        let content = self.body(limit, ctx);
        draw_lines(frame.buffer_mut(), area, &indent(content, 2));
        None
    }

    fn modal(&self, ctx: &Ctx) -> Option<(Modal, Vec<Hint>)> {
        let confirm = self.confirm.as_ref()?;
        let short = short(&confirm.profile.id);
        let (title, question, note, action) = if confirm.approve {
            (
                "Approve check profile",
                format!("Approve check profile {short}?"),
                format!(
                    "Checks then run as it says: {}. Runs under any other profile no longer count for Accept.",
                    counts(&confirm.profile)
                ),
                "Approve",
            )
        } else {
            (
                "Revoke check profile",
                format!("Revoke check profile {short}?"),
                "No check runs under it from now on. If it is the current profile, the project has none until you approve one.".to_owned(),
                "Revoke",
            )
        };
        let mut body = vec![Line::raw(question), Line::default()];
        body.extend(
            wrap(&note, usize::from(MODAL_WIDTH) - 6)
                .into_iter()
                .map(Line::raw),
        );
        body.push(Line::default());
        body.push(buttons(&["Keep it", action], Some(1), &ctx.theme));
        Some((
            Modal {
                width: MODAL_WIDTH,
                title: title.into(),
                body,
                top: None,
            },
            vec![hint("Enter", action), hint("Esc", "Keep it")],
        ))
    }

    fn busy(&self, _ctx: &Ctx) -> bool {
        self.pending.is_some()
    }

    fn place(&self) -> Option<Place> {
        Some(Place::CheckSettings {
            project: match &self.scope {
                Scope::App => None,
                Scope::Project { id, name } => Some((id.clone(), name.clone())),
            },
        })
    }
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

fn short(id: &ProfileId) -> String {
    id.as_str().chars().take(PROFILE_SHOWN).collect()
}

fn counts(profile: &ProfileSummary) -> String {
    let checks = match profile.checks {
        1 => "1 check".to_owned(),
        many => format!("{many} checks"),
    };
    match profile.holdouts {
        0 => checks,
        1 => format!("{checks} and 1 holdout"),
        many => format!("{checks} and {many} holdouts"),
    }
}

/// One profile row: its short id, state, counts and who proposed it. Input: the row.
/// Output: the text.
fn profile_text(profile: &ProfileSummary) -> String {
    let state = match profile.state {
        ProfileState::Proposed => "Proposed",
        ProfileState::Current => "Current",
        ProfileState::Approved => "Replaced",
        ProfileState::Revoked => "Revoked",
    };
    let by = trackwright_cli::views::text::safe(&profile.proposed_by);
    format!(
        "{:<width$}{state:<STATE_CELLS$}{}, proposed by {by}",
        short(&profile.id),
        counts(profile),
        width = PROFILE_SHOWN + 2,
    )
}

/// The value one scope sets for a key, if it sets it. Inputs: the scope's values and the
/// key. Output: the value, or None when the key follows the scope below.
fn here(values: &ChecksChoice, key: &str) -> Option<Value> {
    serde_json::to_value(values)
        .ok()?
        .get(key)
        .filter(|value| !value.is_null())
        .cloned()
}

/// The value a key has in a full set of check settings. Inputs: the settings and the key.
/// Output: the value, or None for a key the settings do not have.
fn setting(settings: &ChecksSettings, key: &str) -> Option<Value> {
    serde_json::to_value(settings).ok()?.get(key).cloned()
}

/// A scope's values with one key set or cleared. Inputs: the values, the key and the new
/// value, None to clear it. Output: the values, or None for a value the settings refuse.
fn with(values: &ChecksChoice, key: &str, value: Option<Value>) -> Option<ChecksChoice> {
    let mut document = match serde_json::to_value(values) {
        Ok(Value::Object(document)) => document,
        _ => Map::new(),
    };
    match value {
        Some(value) => document.insert(key.to_owned(), value),
        None => document.remove(key),
    };
    let choice: ChecksChoice = serde_json::from_value(Value::Object(document)).ok()?;
    choice.validate().ok()?;
    Some(choice)
}

/// The name of a checks setting. Input: the key. Output: the name, or the key itself for
/// one this version does not know.
fn label(key: &str) -> String {
    match key {
        "gate" => "Accept gate",
        "parallel" => "Runs at once",
        "keep_failed_snapshots" => "Failed snapshots kept",
        "agent_requests" => "Agent run requests",
        "inventory" => "Test inventory",
        "skip_markers" => "Added skip markers",
        "protected_change" => "Protected paths",
        "removed_tests" => "Removed tests",
        other => other,
    }
    .to_owned()
}

/// One option of a checks setting: its short name and the line saying what it does.
/// Inputs: the key and the option's value. Output: the name and the line; an option this
/// version does not know shows its value and no line.
fn option_text(key: &str, value: &Value) -> (String, String) {
    if let Some(number) = value.as_u64() {
        let text = match (key, number) {
            ("parallel", 1) => (
                "1 at a time".to_owned(),
                "One check run goes at a time.".to_owned(),
            ),
            ("parallel", n) => (
                format!("{n} at once"),
                format!("Up to {n} check runs go at the same time."),
            ),
            ("keep_failed_snapshots", 0) => (
                "None".to_owned(),
                "No failed run keeps its snapshot folder.".to_owned(),
            ),
            ("keep_failed_snapshots", 1) => (
                "1".to_owned(),
                "The last failed run keeps its snapshot folder for you to look at.".to_owned(),
            ),
            ("keep_failed_snapshots", n) => (
                n.to_string(),
                format!("The last {n} failed runs keep their snapshot folders for you to look at."),
            ),
            ("agent_requests", 0) => (
                "Off".to_owned(),
                "Agents cannot ask for check runs; checks run at their triggers only.".to_owned(),
            ),
            ("agent_requests", n) => (
                format!("{n} an hour"),
                format!("Each agent may ask for {n} check runs of its own changes an hour."),
            ),
            _ => return (value.to_string(), String::new()),
        };
        return text;
    }
    let (name, meaning) = match (key, value.as_str().unwrap_or("")) {
        ("gate", "off") => ("Off", "Accept never looks at the checks."),
        ("gate", "warn") => (
            "Warn",
            "Accept goes ahead; the change set and its tickets record what the checks miss.",
        ),
        ("gate", "require") => (
            "Require",
            "Accept waits until every required check passed on the reviewed head under the current profile.",
        ),
        ("inventory", "last_known") => (
            "Last known",
            "A run's tests are compared with the last passed run of the check on the base commit.",
        ),
        ("inventory", "run_base") => (
            "Run the base",
            "The check also runs on the base commit, and the two runs' tests are compared.",
        ),
        ("inventory", "off") => ("Off", "The tests a run executed are not compared."),
        ("skip_markers", "flag") => (
            "Flag",
            "A skip marker the change set adds is shown on the run.",
        ),
        ("skip_markers", "refuse") => (
            "Refuse",
            "A skip marker the change set adds keeps the change set from passing the gate.",
        ),
        ("skip_markers", "off") => ("Off", "Added skip markers are not looked for."),
        ("protected_change", "refuse") => (
            "Refuse",
            "A change set that touches a protected path cannot pass the gate.",
        ),
        ("protected_change", "flag") => (
            "Flag",
            "A change set that touches a protected path is shown and can still pass.",
        ),
        ("removed_tests", "off") => (
            "Off",
            "A test a run no longer reports is not looked at by the gate.",
        ),
        ("removed_tests", "flag") => (
            "Flag",
            "A test a run no longer reports is recorded under Warn and blocks Accept under Require.",
        ),
        ("removed_tests", "refuse") => (
            "Refuse",
            "A test a run no longer reports blocks Accept under Warn and Require.",
        ),
        _ => return (value.to_string(), String::new()),
    };
    (name.to_owned(), meaning.to_owned())
}
