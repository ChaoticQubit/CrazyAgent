use crate::checks::settings::{ChecksChoice, ChecksSettings, SettingChoice, choices};
use crate::checks::{
    CheckKind, CheckSpec, NetworkMode, ProfileId, ProfileState, ProfileSummary, ReportFormat,
    StoredProfile, Trigger,
};
use crate::engine::owner::{ChecksSettingsView, SettingsSaved};
use crate::protocol::ProjectId;
use crate::sandbox::toolchains::Toolchain;
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
use std::collections::BTreeMap;
use trackwright_cli::views::text::safe;

#[cfg(all(test, unix))]
mod tests;

const LABEL_CELLS: usize = 23;
const NAME_CELLS: usize = 16;
const PROFILE_SHOWN: usize = 12;
const STATE_CELLS: usize = 10;
const MODAL_WIDTH: u16 = 58;
const FIELD_INDENT: usize = 4;
const FIELD_CELLS: usize = 11;

/// The page's words, the desktop's (Settings, Checks and Project Settings, Checks on
/// feat/v3-screens): the title, the app page's purpose, the Check rules section with when a
/// change applies and the project's line, and the profiles' notice and empty line. The app
/// purpose names the Tests tab, where a TUI project sets its own values.
const TITLE: &str = "Checks";
const PURPOSE: &str = "The check settings every project starts from, until a project sets its own from its Tests tab.";
const RULES: &str = "Check rules";
const EFFECT: &str = "Changes apply to check runs queued after you save and to the next Accept.";
const PROJECT_EFFECT: &str = "A value set here wins over the app value for this project only.";
const PROFILES: &str = "Check profiles";
const PROFILES_NOTICE: &str = "A check runs only under a profile you approved. Agents can propose one; nothing runs until you approve it here.";
const NO_PROFILE: &str = "No check profile yet. An agent can propose one for you to approve, or you propose one with crazyagent checks.";

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
    Show,
    Profile,
}

/// A stored profile the owner reads before approving it: its whole body, the first line
/// drawn, how many rows the last draw had, how many lines from the top draws have shown
/// without a gap, and whether that reached the last line.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Review {
    profile: Box<StoredProfile>,
    scroll: usize,
    page: usize,
    read: usize,
    seen: bool,
}

/// The check settings page: every checks setting for the app, or one project's values over
/// the app values, with each option's one-line meaning; a project's page also lists its
/// stored check profiles. The owner approves one from a review of its whole body once every
/// line of it was shown, and revokes one after confirming it. A change saves the scope's
/// values with that key set, at the version read; `u` clears the key so it follows the
/// scope below. One request runs at a time.
pub(in crate::tui) struct CheckSettingsPage {
    scope: Scope,
    state: State,
    profiles: Vec<ProfileSummary>,
    profiles_problem: Option<String>,
    selected: usize,
    choosing: Option<usize>,
    confirm: Option<ProfileSummary>,
    review: Option<Review>,
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
            review: None,
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

    /// Starts approving or revoking the selected profile: approval reads the whole profile
    /// for its review, revocation opens its confirm. Nothing starts while a request waits,
    /// so no confirm can sit under a review that a reply opens. Inputs: approve or revoke
    /// and the context.
    fn ask_profile(&mut self, approve: bool, ctx: &mut Ctx) {
        if self.pending.is_some() {
            return;
        }
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
        if !approve {
            self.confirm = Some(profile);
            return;
        }
        let Some(project) = self.project() else {
            return;
        };
        self.ask(
            ChecksJob::Show {
                project,
                profile: profile.id,
            },
            Pending::Show,
            ctx,
        );
    }

    fn confirm_key(&mut self, key: &Key, ctx: &mut Ctx) -> Flow {
        match key {
            Key::Esc => self.confirm = None,
            Key::Enter => {
                let (Some(project), None) = (self.project(), self.pending) else {
                    return Flow::Handled;
                };
                let Some(profile) = self.confirm.take() else {
                    return Flow::Handled;
                };
                self.ask(
                    ChecksJob::Revoke {
                        project,
                        profile: profile.id,
                    },
                    Pending::Profile,
                    ctx,
                );
            }
            _ => {}
        }
        Flow::Handled
    }

    /// Keys while the owner reads a profile: arrows and pages scroll, Esc keeps it, and
    /// Enter approves it once draws have shown every line of it. Inputs: the key and the
    /// context. Output: handled; no key reaches the table under the review.
    fn review_key(&mut self, key: &Key, ctx: &mut Ctx) -> Flow {
        let project = self.project();
        let idle = self.pending.is_none();
        let Some(review) = self.review.as_mut() else {
            return Flow::Handled;
        };
        let page = review.page.max(1);
        match key {
            Key::Esc => self.review = None,
            Key::Up => review.scroll = review.scroll.saturating_sub(1),
            Key::Down => review.scroll = review.scroll.saturating_add(1),
            Key::PageUp => review.scroll = review.scroll.saturating_sub(page),
            Key::PageDown => review.scroll = review.scroll.saturating_add(page),
            Key::Home => review.scroll = 0,
            Key::End => review.scroll = usize::MAX,
            Key::Enter if !review.seen => ctx.say(Message::warning(
                "Read the whole profile first.",
                "Press PgDn until its end is shown, then Enter to approve it.",
            )),
            Key::Enter => {
                let (Some(project), true) = (project, idle) else {
                    return Flow::Handled;
                };
                let profile = review.profile.id.clone();
                self.review = None;
                self.ask(
                    ChecksJob::Approve { project, profile },
                    Pending::Profile,
                    ctx,
                );
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
                ChecksEvent::Shown {
                    project: scope,
                    result,
                    ..
                },
                Some(Pending::Show),
            ) if Some(scope) == project.as_ref() => {
                self.pending = None;
                match result {
                    Ok(profile) => {
                        self.review = Some(Review {
                            profile: profile.clone(),
                            scroll: 0,
                            page: 1,
                            read: 0,
                            seen: false,
                        })
                    }
                    Err(code) => ctx.say(Message::error(
                        "Could not read the check profile.",
                        &sentence(code),
                    )),
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
        let mut lines = vec![styled(PROFILES, Role::Heading, theme)];
        lines.extend(
            wrap(PROFILES_NOTICE, limit)
                .into_iter()
                .map(|part| styled(part, Role::Muted, theme)),
        );
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
                wrap(NO_PROFILE, limit)
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
    fn wraps_hints(&self) -> bool {
        true
    }

    fn crumbs(&self) -> Vec<String> {
        match &self.scope {
            Scope::App => vec!["Settings".into(), "Checks".into()],
            Scope::Project { name, .. } => {
                vec![name.clone(), "Tests".into(), "Settings".into()]
            }
        }
    }

    fn hints(&self, ctx: &Ctx) -> Vec<Hint> {
        if let Some(review) = &self.review {
            return vec![
                if review.seen {
                    hint("Enter", "Approve")
                } else {
                    hint("PgDn", "Read on")
                },
                hint(&format!("{}{}", ctx.glyphs.up, ctx.glyphs.down), "Scroll"),
                hint("Esc", "Keep it"),
            ];
        }
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
        if self.review.is_some() {
            return self.review_key(key, ctx);
        }
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
        let content = match &self.review {
            Some(review) => review_lines(&review.profile, limit, ctx),
            None => self.body(limit, ctx),
        };
        let rows = usize::from(area.height);
        let first = match self.review.as_mut() {
            Some(review) => {
                review.scroll = review.scroll.min(content.len().saturating_sub(rows));
                review.page = rows;
                if rows > 0 && review.scroll <= review.read {
                    review.read = review.read.max(review.scroll + rows);
                }
                review.seen = review.seen || review.read >= content.len();
                review.scroll
            }
            None => 0,
        };
        let shown = content.into_iter().skip(first).collect();
        draw_lines(frame.buffer_mut(), area, &indent(shown, 2));
        None
    }

    fn modal(&self, ctx: &Ctx) -> Option<(Modal, Vec<Hint>)> {
        let profile = self.confirm.as_ref()?;
        let mut body = vec![
            Line::raw(format!("Revoke check profile {}?", short(&profile.id))),
            Line::default(),
        ];
        body.extend(
            wrap(
                "No check runs under it from now on. If it is the current profile, the project has none until you approve one.",
                usize::from(MODAL_WIDTH) - 6,
            )
            .into_iter()
            .map(Line::raw),
        );
        body.push(Line::default());
        body.push(buttons(&["Keep it", "Revoke"], Some(1), &ctx.theme));
        Some((
            Modal {
                width: MODAL_WIDTH,
                title: "Revoke check profile".into(),
                body,
                top: None,
            },
            vec![hint("Enter", "Revoke"), hint("Esc", "Keep it")],
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
    let by = safe(&profile.proposed_by);
    format!(
        "{:<width$}{state:<STATE_CELLS$}{}, proposed by {by}",
        short(&profile.id),
        counts(profile),
        width = PROFILE_SHOWN + 2,
    )
}

/// A whole stored profile as the owner reads it before approving it: who proposed it, each
/// check and holdout with every field that decides what runs, each holdout's files by path
/// (their bytes stay with the engine) and the protected paths, all stored text made safe to
/// draw. Inputs: the profile, the content width and the context. Output: the lines.
fn review_lines(stored: &StoredProfile, limit: usize, ctx: &Ctx) -> Vec<Line<'static>> {
    let theme = &ctx.theme;
    let body = &stored.profile;
    let mut lines = vec![styled(
        format!("Approve check profile {}?", short(&stored.id)),
        Role::Accent,
        theme,
    )];
    let note = format!(
        "Proposed by {}. Approving it lets every check below run exactly as written, with the network it names; runs under any other profile no longer count for Accept.",
        safe(&stored.proposed_by)
    );
    lines.extend(
        wrap(&note, limit)
            .into_iter()
            .map(|part| styled(part, Role::Muted, theme)),
    );
    lines.push(Line::default());
    lines.push(styled(
        format!("Checks ({})", body.checks.len()),
        Role::Accent,
        theme,
    ));
    for spec in &body.checks {
        lines.extend(check_lines(spec, None, limit, ctx));
    }
    if !body.holdouts.is_empty() {
        lines.push(Line::default());
        lines.push(styled(
            format!("Holdouts ({})", body.holdouts.len()),
            Role::Accent,
            theme,
        ));
        for holdout in &body.holdouts {
            lines.extend(check_lines(
                &holdout.check,
                Some(&holdout.files),
                limit,
                ctx,
            ));
        }
    }
    lines.push(Line::default());
    lines.push(styled("Protected paths", Role::Accent, theme));
    if body.protected_paths.is_empty() {
        lines.push(styled(
            "  None. A change set may touch any path.",
            Role::Muted,
            theme,
        ));
    }
    for path in &body.protected_paths {
        lines.extend(
            wrap(&safe(path), limit.saturating_sub(2).max(1))
                .into_iter()
                .map(|part| Line::raw(format!("  {part}"))),
        );
    }
    lines.push(Line::default());
    lines.push(styled(
        "Enter approves this profile. Esc keeps it as it is.",
        Role::Muted,
        theme,
    ));
    lines
}

/// One check as the review shows it: its id, kind, whether the gate requires it and whether
/// its cases are protected, then its command, folder, network, time limit, triggers,
/// toolchain and report, and a holdout's files by path. A network beyond none is drawn as a
/// warning. Inputs: the check, a holdout's files, the content width and the context.
/// Output: the lines.
fn check_lines(
    spec: &CheckSpec,
    files: Option<&BTreeMap<String, String>>,
    limit: usize,
    ctx: &Ctx,
) -> Vec<Line<'static>> {
    let mut traits = vec![
        kind_text(spec.kind),
        if spec.required {
            "required"
        } else {
            "optional"
        },
    ];
    if spec.protected {
        traits.push("protected");
    }
    let mut lines = vec![Line::raw(format!(
        "  {}  {}",
        safe(&spec.id),
        traits.join(", ")
    ))];
    let (network, reach) = match spec.network {
        NetworkMode::Off => ("Off", None),
        NetworkMode::Localhost => ("Localhost only", Some(Role::Warning)),
        NetworkMode::Remotes => ("The project's sandbox remotes", Some(Role::Warning)),
    };
    let report = match spec.format {
        ReportFormat::Libtest => "libtest output",
        ReportFormat::Junit => "JUnit",
        ReportFormat::Allure => "Allure results",
        ReportFormat::ExitCode => "exit status only",
    };
    let report = match &spec.report {
        Some(path) => format!("{report} at {}", safe(path)),
        None => report.to_owned(),
    };
    let when: Vec<&str> = spec
        .when
        .iter()
        .map(|trigger| trigger_text(*trigger))
        .collect();
    let mut fields = vec![
        (
            "Command",
            safe(&serde_json::to_string(&spec.argv).unwrap_or_default()),
            None,
        ),
        ("Folder", safe(&spec.cwd), None),
        ("Network", network.to_owned(), reach),
        ("Timeout", format!("{} s", spec.timeout_s), None),
        ("Runs at", when.join(", "), None),
        (
            "Toolchain",
            spec.toolchain.map_or("None", Toolchain::label).to_owned(),
            None,
        ),
        ("Reports", report, None),
    ];
    if let Some(files) = files {
        let paths: Vec<String> = files.keys().map(|path| safe(path)).collect();
        fields.push(("Files", paths.join(", "), None));
    }
    for (label, value, role) in fields {
        lines.extend(field_lines(label, &value, role, limit, ctx));
    }
    lines
}

/// One labelled field of a check, its value wrapped under the value column. Inputs: the
/// label, the value, the value's role (None for plain), the content width and the context.
/// Output: at least one line.
fn field_lines(
    label: &str,
    value: &str,
    role: Option<Role>,
    limit: usize,
    ctx: &Ctx,
) -> Vec<Line<'static>> {
    let mut parts = wrap(
        value,
        limit.saturating_sub(FIELD_INDENT + FIELD_CELLS).max(1),
    );
    if parts.is_empty() {
        parts.push(String::new());
    }
    parts
        .into_iter()
        .enumerate()
        .map(|(index, part)| {
            let head = if index == 0 {
                format!("{}{label:<FIELD_CELLS$}", " ".repeat(FIELD_INDENT))
            } else {
                " ".repeat(FIELD_INDENT + FIELD_CELLS)
            };
            let value = match role {
                Some(role) => styled(part, role, &ctx.theme),
                None => Line::raw(part),
            };
            join([Line::raw(head), value])
        })
        .collect()
}

/// The word for what a check proves. Input: the kind. Output: the word.
fn kind_text(kind: CheckKind) -> &'static str {
    match kind {
        CheckKind::Unit => "unit",
        CheckKind::Integration => "integration",
        CheckKind::E2e => "e2e",
        CheckKind::Smoke => "smoke",
        CheckKind::Scenario => "scenario",
    }
}

/// The words for when the engine runs a check. Input: the trigger. Output: the words.
fn trigger_text(trigger: Trigger) -> &'static str {
    match trigger {
        Trigger::TurnEnd => "turn end",
        Trigger::BeforeAccept => "before Accept",
        Trigger::OnRequest => "on request",
    }
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
