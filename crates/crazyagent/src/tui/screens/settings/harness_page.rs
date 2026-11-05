use super::draw_page;
use crate::harness_settings::{self, Access, Kind, Setting, Source, Written};
use crate::protocol::ProjectId;
use crate::tui::client::harness_settings::{
    HarnessRead, HarnessSettingsEvent, HarnessSettingsQuery, HarnessSettingsReply,
};
use crate::tui::client::settings::{SettingsEvent, SettingsJob};
use crate::tui::ctx::Ctx;
use crate::tui::frame::{Hint, Message, draw_lines, hint};
use crate::tui::keys::Key;
use crate::tui::screens::process::Scope;
use crate::tui::screens::settings_accounts::{plain, send, settings_event};
use crate::tui::screens::{Flow, Place, Screen, SettingsSection};
use crate::tui::seam::Arrival;
use crate::tui::text::{
    cells, indent, join, list_row, restyle, row, status, styled, thumb, with_bar, wrap,
};
use crate::tui::theme::Role;
use crate::tui::widgets::TextField;
use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::text::Line;
use serde_json::Value;

#[cfg(all(test, unix))]
mod tests;

const DETAIL_ROWS: usize = 6;
const HELP_ROWS: usize = 2;
const NUMBER_CHARS_MAX: usize = 20;
const PAGE_STEP: isize = 10;

/// What a key left to the harness shows as its value: the harness decides until the owner
/// sets one.
const HARNESS_DEFAULT: &str = "Harness default";

#[derive(Debug, PartialEq)]
enum State {
    Loading,
    Failed(String),
    Ready(HarnessRead),
}

/// A change the owner is making to the selected row: an option of a choice key, or the
/// typed text of a number or text key.
#[derive(Debug)]
enum Edit {
    Choice(usize),
    Field(TextField),
}

/// What one key does to an open change: keep it open, close it, or save a value.
enum Step {
    Stay,
    Close,
    Save(Value),
}

/// The plain sentence for a harness settings problem code. Input: the code. Output: the
/// sentence; codes without one fall back to the shared settings sentences.
pub(in crate::tui) fn message(code: &str) -> String {
    match code {
        "harness_settings_key_locked" => "CrazyAgent sets this one itself.".into(),
        "harness_settings_key_not_qualified" => {
            "This setting is read-only for the installed version.".into()
        }
        "harness_settings_key_app_only" => "This setting is set once for the whole app.".into(),
        "harness_settings_value_invalid" => "This value is not allowed for this setting.".into(),
        "harness_settings_project_unknown" => "This project no longer exists.".into(),
        "harness_settings_no_catalog" => {
            "CrazyAgent has no settings page for this harness yet.".into()
        }
        "harness_settings_outcome_unknown" => {
            "The engine did not say whether it saved; press r to read the current value.".into()
        }
        "harness_settings_unavailable" => {
            "Harness settings are not available in this engine.".into()
        }
        other => plain(other).into(),
    }
}

/// A value as the page shows it: On or Off, the number, the text, "Empty" for empty
/// text and "Not set" for no value; control characters show as `?`. Input: the value.
/// Output: one line of text.
pub(in crate::tui) fn shown(value: &Value) -> String {
    let printable = |text: &str| -> String {
        text.chars()
            .map(|character| {
                if character.is_control() {
                    '?'
                } else {
                    character
                }
            })
            .collect()
    };
    match value {
        Value::Bool(true) => "On".into(),
        Value::Bool(false) => "Off".into(),
        Value::Null => "Not set".into(),
        Value::String(text) if text.is_empty() => "Empty".into(),
        Value::String(text) => printable(text),
        other => printable(&other.to_string()),
    }
}

/// What one key accepts and its default, in a sentence; a null default reads as the
/// harness default. Input: the catalog key. Output: the sentence.
pub(in crate::tui) fn kind_sentence(key: &harness_settings::Key) -> String {
    let default = if key.default.is_null() {
        HARNESS_DEFAULT.to_owned()
    } else {
        shown(&key.default)
    };
    match &key.kind {
        Kind::Bool => format!("On or off. Default: {default}."),
        Kind::Enum => format!("One of {}. Default: {default}.", key.options.join(", ")),
        Kind::Integer => match (key.min, key.max) {
            (Some(min), Some(max)) => {
                format!("A whole number from {min} to {max}. Default: {default}.")
            }
            _ => format!("A whole number. Default: {default}."),
        },
        Kind::String => match key.max_bytes {
            Some(max) => format!("One line of text, at most {max} bytes. Default: {default}."),
            None => format!("One line of text. Default: {default}."),
        },
    }
}

/// Checks typed text against one number or text key before it is sent; the engine checks
/// it again. Inputs: the key and the typed text. Output: the value, or the sentence the
/// field shows.
pub(in crate::tui) fn typed_value(
    key: &harness_settings::Key,
    text: &str,
) -> Result<Value, String> {
    match &key.kind {
        Kind::Integer => {
            let number: i64 = text
                .trim()
                .parse()
                .map_err(|_| "Type a whole number.".to_owned())?;
            match (key.min, key.max) {
                (Some(min), Some(max)) if !(min..=max).contains(&number) => {
                    Err(format!("Use a number from {min} to {max}."))
                }
                _ => Ok(Value::from(number)),
            }
        }
        Kind::String => match key.max_bytes {
            Some(max) if text.len() > usize::try_from(max).unwrap_or(usize::MAX) => {
                Err(format!("Use at most {max} bytes."))
            }
            _ => Ok(Value::String(text.to_owned())),
        },
        Kind::Bool | Kind::Enum => Err("Pick a value from the list.".to_owned()),
    }
}

fn sentence(reason: &str) -> String {
    if reason.ends_with('.') {
        reason.to_owned()
    } else {
        format!("{reason}.")
    }
}

/// One harness's settings page: every key the engine lists for it, with the value the next
/// start uses and where it comes from, at app scope from Settings > Harnesses or as one
/// project's overrides from that project's settings. Editable keys change with Space,
/// Enter and x, naming the version read; locked and read-only keys show the engine's
/// reason and send nothing. One request runs at a time.
pub(in crate::tui) struct HarnessPage {
    harness: String,
    name: String,
    scope: Scope,
    state: State,
    selected: usize,
    offset: usize,
    edit: Option<Edit>,
    pending: Option<HarnessSettingsQuery>,
}

impl HarnessPage {
    /// The page for one harness at one scope, loading until the engine answers. Inputs:
    /// the engine's harness id, its name and the scope. Output: the screen; `shown` reads.
    pub fn new(harness: String, name: String, scope: Scope) -> Self {
        assert!(!harness.is_empty());
        Self {
            harness,
            name,
            scope,
            state: State::Loading,
            selected: 0,
            offset: 0,
            edit: None,
            pending: None,
        }
    }

    fn project(&self) -> Option<ProjectId> {
        match &self.scope {
            Scope::App => None,
            Scope::Project { id, .. } => Some(id.clone()),
        }
    }

    fn ask(&mut self, query: HarnessSettingsQuery, ctx: &mut Ctx) {
        if self.pending.is_some() {
            return;
        }
        self.pending = Some(query.clone());
        send(ctx, SettingsJob::HarnessSettings(Box::new(query)));
    }

    fn read_again(&mut self, ctx: &mut Ctx) {
        if self.pending.is_some() {
            return;
        }
        if matches!(self.state, State::Failed(_)) {
            self.state = State::Loading;
        }
        let query = HarnessSettingsQuery::Read {
            harness: self.harness.clone(),
            project: self.project(),
        };
        self.ask(query, ctx);
    }

    fn current(&self) -> Option<(Option<&harness_settings::Key>, &Setting)> {
        let State::Ready(read) = &self.state else {
            return None;
        };
        let setting = read.settings.get(self.selected)?;
        Some((read.key(&setting.key), setting))
    }

    fn refuse(&self, access: &Access, ctx: &mut Ctx) -> bool {
        match access {
            Access::Editable => false,
            Access::Locked { reason } => {
                ctx.say(Message::warning(
                    "CrazyAgent sets this one itself.",
                    &sentence(reason),
                ));
                true
            }
            Access::ReadOnly { reason } => {
                ctx.say(Message::warning(
                    "This setting is read-only here.",
                    &sentence(reason),
                ));
                true
            }
        }
    }

    fn write(&mut self, setting: &Setting, value: Value, ctx: &mut Ctx) {
        let query = HarnessSettingsQuery::Set {
            harness: self.harness.clone(),
            project: self.project(),
            key: setting.key.clone(),
            value,
            version: setting.version,
        };
        self.ask(query, ctx);
    }

    fn change(&mut self, only_switch: bool, ctx: &mut Ctx) {
        if self.pending.is_some() {
            return;
        }
        let Some((Some(key), setting)) = self.current() else {
            if let Some((None, setting)) = self.current() {
                let access = setting.access.clone();
                self.refuse(&access, ctx);
            }
            return;
        };
        let (key, setting) = (key.clone(), setting.clone());
        if self.refuse(&setting.access, ctx) {
            return;
        }
        match &key.kind {
            Kind::Bool => {
                let on = setting.effective.as_bool() == Some(true);
                self.write(&setting, Value::Bool(!on), ctx);
            }
            _ if only_switch => {}
            Kind::Enum => {
                let at = key
                    .options
                    .iter()
                    .position(|option| setting.effective.as_str() == Some(option.as_str()))
                    .unwrap_or(0);
                self.edit = Some(Edit::Choice(at));
            }
            Kind::Integer | Kind::String => {
                let limit = match (&key.kind, key.max_bytes) {
                    (Kind::String, Some(max)) => usize::try_from(max).unwrap_or(usize::MAX),
                    _ => NUMBER_CHARS_MAX,
                };
                let mut field = TextField::new("New value", limit.max(1));
                if let Some(text) = setting.effective.as_str() {
                    field.set(text);
                } else if setting.effective.is_number() {
                    field.set(&setting.effective.to_string());
                }
                self.edit = Some(Edit::Field(field));
            }
        }
    }

    fn reset(&mut self, ctx: &mut Ctx) {
        if self.pending.is_some() {
            return;
        }
        let Some((_, setting)) = self.current() else {
            return;
        };
        let setting = setting.clone();
        if self.refuse(&setting.access, ctx) {
            return;
        }
        if setting.here.is_none() {
            let follows = match setting.source {
                Source::App => "the app value",
                _ => "the default",
            };
            ctx.say(Message::warning(
                "Nothing to reset.",
                &format!("This setting already uses {follows}."),
            ));
            return;
        }
        let query = HarnessSettingsQuery::Reset {
            harness: self.harness.clone(),
            project: self.project(),
            key: setting.key.clone(),
            version: setting.version,
        };
        self.ask(query, ctx);
    }

    fn edit_key(&mut self, key: &Key, ctx: &mut Ctx) -> Flow {
        let Some((Some(spec), setting)) = self.current() else {
            self.edit = None;
            return Flow::Handled;
        };
        let (spec, setting) = (spec.clone(), setting.clone());
        let count = spec.options.len().max(1);
        let step = match (&mut self.edit, key) {
            (_, Key::Esc) => Step::Close,
            (Some(Edit::Choice(at)), Key::Left) => {
                *at = (*at + count - 1) % count;
                Step::Stay
            }
            (Some(Edit::Choice(at)), Key::Right) => {
                *at = (*at + 1) % count;
                Step::Stay
            }
            (Some(Edit::Choice(at)), Key::Enter) => {
                spec.options.get(*at).map_or(Step::Close, |option| {
                    Step::Save(Value::String(option.clone()))
                })
            }
            (Some(Edit::Field(field)), Key::Enter) => match typed_value(&spec, field.value()) {
                Ok(value) => Step::Save(value),
                Err(problem) => {
                    field.error = Some(problem);
                    Step::Stay
                }
            },
            (Some(Edit::Field(field)), key) => {
                if field.key(key) {
                    field.error = None;
                }
                Step::Stay
            }
            _ => Step::Stay,
        };
        match step {
            Step::Stay => {}
            Step::Close => self.edit = None,
            Step::Save(value) => {
                self.edit = None;
                if value != setting.effective {
                    self.write(&setting, value, ctx);
                }
            }
        }
        Flow::Handled
    }

    fn move_by(&mut self, delta: isize) {
        let count = match &self.state {
            State::Ready(read) => read.settings.len(),
            _ => 0,
        };
        if count == 0 {
            self.selected = 0;
            return;
        }
        let target = self.selected as isize + delta;
        self.selected = target.clamp(0, count as isize - 1) as usize;
    }

    fn replace(&mut self, setting: &Setting) {
        if let State::Ready(read) = &mut self.state
            && let Some(row) = read.settings.iter_mut().find(|row| row.key == setting.key)
        {
            *row = setting.clone();
        }
    }

    fn apply(&mut self, event: &HarnessSettingsEvent, ctx: &mut Ctx) {
        if self.pending.as_ref() != Some(&event.query) {
            return;
        }
        self.pending = None;
        let name = self.name.clone();
        match (&event.query, &event.result) {
            (HarnessSettingsQuery::Read { .. }, Ok(HarnessSettingsReply::Read(read))) => {
                let count = read.settings.len();
                self.state = State::Ready(read.clone());
                self.selected = self.selected.min(count.saturating_sub(1));
                self.edit = None;
            }
            (HarnessSettingsQuery::Read { .. }, Err(code)) => match self.state {
                State::Ready(_) => ctx.say(Message::error(
                    &format!("Could not read the {name} settings."),
                    &message(code),
                )),
                _ => self.state = State::Failed(message(code)),
            },
            (_, Ok(HarnessSettingsReply::Written(Written::Saved { setting }))) => {
                self.replace(setting);
                ctx.say(Message::success(
                    "Saved.",
                    &format!("{name} gets it the next time it starts."),
                ));
            }
            (_, Ok(HarnessSettingsReply::Written(Written::Stale { setting }))) => {
                self.replace(setting);
                ctx.say(Message::warning(
                    "Changed somewhere else.",
                    "This is the current value; change it again if you still want to.",
                ));
            }
            (_, Ok(HarnessSettingsReply::Written(Written::Refused { code, setting }))) => {
                self.replace(setting);
                let reason = match &setting.access {
                    Access::Locked { reason } | Access::ReadOnly { reason } => sentence(reason),
                    Access::Editable => String::new(),
                };
                ctx.say(Message::warning(&message(code), &reason));
            }
            (_, Err(code)) => ctx.say(Message::error(
                "Could not change the setting.",
                &message(code),
            )),
            (_, Ok(_)) => {}
        }
    }

    fn widths(limit: usize, wide: bool) -> [usize; 3] {
        let (first, second) = if wide { (32, 32) } else { (24, 22) };
        [
            first,
            second,
            limit.saturating_sub(2 + first + second + 2 * 2).max(1),
        ]
    }

    fn from(&self, setting: &Setting, ctx: &Ctx) -> Line<'static> {
        match &setting.access {
            Access::Locked { .. } => status("Locked", &ctx.theme, ctx.glyphs),
            Access::ReadOnly { .. } => status("Read-only", &ctx.theme, ctx.glyphs),
            Access::Editable => Line::raw(match setting.source {
                Source::Default => "Default",
                Source::App => "App",
                Source::Project => "Project",
                Source::Engine => "Engine",
                Source::NotApplied => "Not applied",
            }),
        }
    }

    fn detail(
        &self,
        read: &HarnessRead,
        setting: &Setting,
        limit: usize,
        ctx: &Ctx,
    ) -> (Vec<Line<'static>>, Option<Position>) {
        let theme = &ctx.theme;
        let key = read.key(&setting.key);
        let label = key.map_or_else(|| setting.key.clone(), |key| key.label.clone());
        let mut out = vec![row(
            limit,
            styled(label, Role::Heading, theme),
            styled(setting.key.clone(), Role::Muted, theme),
            ctx.glyphs,
        )];
        if let Some(key) = key {
            out.extend(
                wrap(&key.help, limit)
                    .into_iter()
                    .take(HELP_ROWS)
                    .map(Line::raw),
            );
        }
        let warning = |text: String| -> Vec<Line<'static>> {
            wrap(&text, limit)
                .into_iter()
                .map(|part| styled(part, Role::Warning, theme))
                .collect()
        };
        let mut cursor = None;
        match (&setting.access, &self.edit, key) {
            (Access::Locked { reason }, _, _) => {
                out.extend(warning(format!("Locked: {}", sentence(reason))));
            }
            (Access::ReadOnly { reason }, _, _) => {
                out.extend(warning(format!("Read-only: {}", sentence(reason))));
            }
            (Access::Editable, Some(Edit::Field(field)), _) => {
                let (lines, at) = field.lines(limit.max(6), true, theme, ctx.glyphs);
                let first = out.len() as u16;
                cursor = at.map(|at| Position::new(at.x, first + at.y));
                out.extend(lines);
            }
            (Access::Editable, _, Some(key)) => out.extend(
                wrap(&kind_sentence(key), limit)
                    .into_iter()
                    .map(|part| styled(part, Role::Muted, theme)),
            ),
            (Access::Editable, _, None) => {}
        }
        out.truncate(DETAIL_ROWS);
        (out, cursor)
    }

    /// The page's lines below the tab strip. Inputs: the content width, the rows it may
    /// fill and the context. Output: the lines and the cursor of an open field.
    fn body(
        &mut self,
        limit: usize,
        height: usize,
        ctx: &Ctx,
    ) -> (Vec<Line<'static>>, Option<Position>) {
        let theme = &ctx.theme;
        let glyphs = ctx.glyphs;
        let name = self.name.clone();
        let title = match &self.scope {
            Scope::App => format!("{name} settings"),
            Scope::Project { name: project, .. } => format!("{name} settings for {project}"),
        };
        let version = match &self.state {
            State::Ready(read) => match (&read.installed, read.qualified) {
                (Some(version), true) => {
                    styled(format!("{name} {version}, qualified"), Role::Muted, theme)
                }
                (Some(version), false) => styled(
                    format!("{name} {version}, not qualified"),
                    Role::Warning,
                    theme,
                ),
                (None, _) => styled("Version unknown", Role::Warning, theme),
            },
            _ => Line::default(),
        };
        let mut lines = vec![row(
            limit,
            styled(title, Role::Accent, theme),
            version,
            glyphs,
        )];
        let intro = match &self.scope {
            Scope::App => format!(
                "Every project gets these unless the project changes them. A change applies the next time {name} starts."
            ),
            Scope::Project { name: project, .. } => format!(
                "Only {project} gets the values set here; App and Default rows follow Settings > Harnesses. A change applies the next time {name} starts."
            ),
        };
        lines.extend(
            wrap(&intro, limit)
                .into_iter()
                .map(|part| styled(part, Role::Muted, theme)),
        );
        let read = match &self.state {
            State::Loading => {
                lines.push(Line::default());
                lines.push(ctx.loading(&format!("Reading the {name} settings")));
                return (lines, None);
            }
            State::Failed(problem) => {
                lines.push(Line::default());
                lines.extend(
                    wrap(
                        &format!("Could not read the {name} settings. {problem}"),
                        limit,
                    )
                    .into_iter()
                    .map(|part| styled(part, Role::Error, theme)),
                );
                lines.push(styled("Press r to try again.", Role::Muted, theme));
                return (lines, None);
            }
            State::Ready(read) => read,
        };
        if !read.qualified {
            lines.extend(
                wrap(
                    &format!(
                        "This version is not qualified yet: every setting is read-only and {name} starts with CrazyAgent's locked values only."
                    ),
                    limit,
                )
                .into_iter()
                .map(|part| styled(part, Role::Warning, theme)),
            );
        }
        lines.push(Line::default());
        let count = read.settings.len();
        if count == 0 {
            lines.push(Line::raw(format!(
                "CrazyAgent lists no settings for {name} yet."
            )));
            return (lines, None);
        }
        let window = height
            .saturating_sub(lines.len() + 2 + DETAIL_ROWS)
            .max(1)
            .min(count);
        let row_limit = if count > window { limit - 1 } else { limit };
        let widths = Self::widths(row_limit, ctx.wide());
        lines.push(join([
            Line::raw("  "),
            restyle(
                cells(
                    &widths,
                    ["Setting", "Value", "From"]
                        .into_iter()
                        .map(Line::raw)
                        .collect(),
                    2,
                    glyphs,
                ),
                theme.muted,
            ),
        ]));
        let selected = self.selected.min(count - 1);
        let mut offset = self.offset.min(count - window);
        if selected < offset {
            offset = selected;
        } else if selected >= offset + window {
            offset = selected + 1 - window;
        }
        let rows: Vec<Line<'static>> = read
            .settings
            .iter()
            .enumerate()
            .skip(offset)
            .take(window)
            .map(|(index, setting)| {
                let key = read.key(&setting.key);
                let label = key.map_or_else(|| setting.key.clone(), |key| key.label.clone());
                let value = match (&self.edit, key) {
                    (Some(Edit::Choice(at)), Some(key)) if index == selected => format!(
                        "{} {} {}",
                        glyphs.left,
                        key.options.get(*at).map_or("", String::as_str),
                        glyphs.right
                    ),
                    _ if setting.source == Source::Default && setting.effective.is_null() => {
                        HARNESS_DEFAULT.into()
                    }
                    _ => shown(&setting.effective),
                };
                let values = vec![Line::raw(label), Line::raw(value), self.from(setting, ctx)];
                list_row(
                    cells(&widths, values, 2, glyphs),
                    row_limit,
                    index == selected,
                    true,
                    theme,
                    glyphs,
                )
            })
            .collect();
        if count > window {
            let (top, size) = thumb(window, offset, count);
            lines.extend(with_bar(rows, limit, top, size, theme, glyphs));
        } else {
            lines.extend(rows);
        }
        lines.push(Line::default());
        let start = lines.len() as u16;
        let (detail, cursor) = self.detail(read, &read.settings[selected], limit, ctx);
        lines.extend(detail);
        self.selected = selected;
        self.offset = offset;
        (lines, cursor.map(|at| Position::new(at.x, start + at.y)))
    }
}

impl Screen for HarnessPage {
    fn crumbs(&self) -> Vec<String> {
        match &self.scope {
            Scope::App => vec!["Settings".into(), "Harnesses".into(), self.name.clone()],
            Scope::Project { name, .. } => {
                vec![name.clone(), "Harnesses".into(), self.name.clone()]
            }
        }
    }

    fn hints(&self, ctx: &Ctx) -> Vec<Hint> {
        match &self.edit {
            Some(Edit::Choice(_)) => {
                return vec![
                    hint(&format!("{}{}", ctx.glyphs.left, ctx.glyphs.right), "Pick"),
                    hint("Enter", "Save"),
                    hint("Esc", "Cancel"),
                ];
            }
            Some(Edit::Field(_)) => return vec![hint("Enter", "Save"), hint("Esc", "Cancel")],
            None => {}
        }
        match self.state {
            State::Loading => return vec![hint("Esc", "Back")],
            State::Failed(_) => return vec![hint("r", "Try again"), hint("Esc", "Back")],
            State::Ready(_) => {}
        }
        let mut hints = Vec::new();
        if let Some((Some(key), setting)) = self.current()
            && setting.access == Access::Editable
        {
            hints.push(match key.kind {
                Kind::Bool => hint("Space", "Turn on or off"),
                Kind::Enum => hint("Enter", "Choose"),
                Kind::Integer | Kind::String => hint("Enter", "Change"),
            });
            if setting.here.is_some() {
                hints.push(hint("x", "Reset"));
            }
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
        matches!(self.edit, Some(Edit::Field(_)))
    }

    fn key(&mut self, key: &Key, ctx: &mut Ctx) -> Flow {
        if self.edit.is_some() {
            return self.edit_key(key, ctx);
        }
        match key {
            Key::Char('r') => self.read_again(ctx),
            Key::Up => self.move_by(-1),
            Key::Down => self.move_by(1),
            Key::PageUp => self.move_by(-PAGE_STEP),
            Key::PageDown => self.move_by(PAGE_STEP),
            Key::Home => self.selected = 0,
            Key::End => self.move_by(isize::MAX / 2),
            Key::Char(' ') => self.change(true, ctx),
            Key::Enter => self.change(false, ctx),
            Key::Char('x') => self.reset(ctx),
            _ => return Flow::Ignored,
        }
        Flow::Handled
    }

    fn arrive(&mut self, arrival: &Arrival, ctx: &mut Ctx) -> Flow {
        if let Some(SettingsEvent::HarnessSettings(event)) = settings_event(arrival) {
            self.apply(event, ctx);
        }
        Flow::Handled
    }

    fn shown(&mut self, ctx: &mut Ctx) {
        if self.state == State::Loading {
            self.read_again(ctx);
        }
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect, ctx: &Ctx) -> Option<Position> {
        let limit = (area.width as usize).saturating_sub(4).max(8);
        match self.scope {
            Scope::App => {
                let height = (area.height as usize).saturating_sub(2);
                let (content, cursor) = self.body(limit, height, ctx);
                draw_page(
                    frame,
                    area,
                    SettingsSection::Harnesses,
                    content,
                    cursor,
                    ctx,
                )
            }
            Scope::Project { .. } => {
                let (content, cursor) = self.body(limit, area.height as usize, ctx);
                draw_lines(frame.buffer_mut(), area, &indent(content, 2));
                cursor.map(|at| Position::new(area.x + 2 + at.x, area.y + at.y))
            }
        }
    }

    fn busy(&self, _ctx: &Ctx) -> bool {
        self.pending.is_some()
    }

    fn draft(&self) -> bool {
        matches!(self.edit, Some(Edit::Field(_)))
    }

    fn loss(&self) -> &'static str {
        "The value you typed will be lost."
    }

    fn place(&self) -> Option<Place> {
        Some(Place::HarnessSettings {
            harness: self.harness.clone(),
            name: self.name.clone(),
            project: match &self.scope {
                Scope::App => None,
                Scope::Project { id, name } => Some((id.clone(), name.clone())),
            },
        })
    }
}
