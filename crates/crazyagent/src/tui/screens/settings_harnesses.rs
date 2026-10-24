use super::settings::{draw_page, step};
use super::settings_accounts::{plain, send, settings_event};
use super::{Flow, Place, Screen, SettingsSection};
use crate::harnesses::{AccountKind, HarnessEntry, HarnessStatus, ToolControl};
use crate::tui::client::settings::{SettingsEvent, SettingsJob};
use crate::tui::ctx::{Ctx, Load};
use crate::tui::frame::{Hint, Message, hint, sections_hint};
use crate::tui::keys::Key;
use crate::tui::seam::Arrival;
use crate::tui::text::{
    cells, cut, join, list_row, pad, restyle, row, status, styled, thumb, width, with_bar, wrap,
};
use crate::tui::theme::Role;
use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::text::{Line, Span};

const LABEL_WIDTH: usize = 13;
const DETAIL_ROWS: usize = 6;
const CHROME_ROWS: usize = 4 + 1;
const SUBTITLE: &str =
    "Agents run inside these tools. CrazyAgent finds them on this Mac and never installs them.";

/// The Status cell word of a harness.
/// Input: the catalog entry. Output: "Ready", "Not installed", or "Can't start"
/// for one that is installed but has no CrazyAgent launcher.
pub fn status_word(entry: &HarnessEntry) -> &'static str {
    match entry.status {
        HarnessStatus::Ready => "Ready",
        HarnessStatus::NotInstalled => "Not installed",
        HarnessStatus::Limited { .. } => "Can't start",
    }
}

/// The Control cell word of a harness.
/// Input: the catalog entry. Output: "Full control", "Limited control" or
/// "Unknown" while it is not installed and nothing limits it.
pub fn control_word(entry: &HarnessEntry) -> &'static str {
    match (&entry.status, &entry.tools) {
        (HarnessStatus::Limited { .. }, _) | (_, ToolControl::Limited { .. }) => "Limited control",
        (HarnessStatus::Ready, ToolControl::Switchable) => "Full control",
        (HarnessStatus::NotInstalled, ToolControl::Switchable) => "Unknown",
    }
}

/// The kinds of account a harness runs on, in plain words.
/// Input: the account kinds. Output: "Any account", "Any API key or URL", a
/// short list, or "Not known yet" when the engine names none.
pub fn works_with(kinds: &[AccountKind]) -> String {
    let has = |kind: AccountKind| kinds.contains(&kind);
    let keyed = has(AccountKind::ApiKey)
        && has(AccountKind::OpenAiCompatibleUrl)
        && has(AccountKind::Local);
    match (has(AccountKind::Subscription), keyed) {
        (true, true) => "Any account".into(),
        (false, true) => "Any API key or URL".into(),
        _ => {
            let words: Vec<&str> = [
                (AccountKind::Subscription, "Subscription"),
                (AccountKind::ApiKey, "API key"),
                (AccountKind::OpenAiCompatibleUrl, "URL"),
                (AccountKind::Local, "Local model"),
            ]
            .into_iter()
            .filter(|(kind, _)| has(*kind))
            .map(|(_, word)| word)
            .collect();
            if words.is_empty() {
                "Not known yet".into()
            } else {
                words.join(", ")
            }
        }
    }
}

/// One sentence on how much CrazyAgent controls a harness.
/// Input: the catalog entry. Output: the sentence with the engine's reasons.
pub fn control_sentence(entry: &HarnessEntry) -> String {
    let mut reasons: Vec<&str> = Vec::new();
    if let HarnessStatus::Limited { reason } = &entry.status {
        reasons.push(reason.trim_end_matches('.'));
    }
    if let ToolControl::Limited { reason } = &entry.tools {
        reasons.push(reason.trim_end_matches('.'));
    }
    if !reasons.is_empty() {
        return format!("Limited control: {}.", reasons.join("; "));
    }
    match entry.status {
        HarnessStatus::Ready => {
            "Full control. Runs without a window, streams progress, and tools can be turned off."
                .into()
        }
        _ => "Unknown until it is installed and CrazyAgent can start it.".into(),
    }
}

/// Settings, Harnesses: every harness the engine knows, whether it is on this
/// Mac, how much CrazyAgent controls it and which accounts it runs on.
/// Input: keys and the engine's harness catalog. Output: the table and the
/// selected harness's details.
pub struct Harnesses {
    entries: Vec<HarnessEntry>,
    load: Load<()>,
    waiting: bool,
    selected: usize,
    offset: usize,
}

impl Default for Harnesses {
    fn default() -> Self {
        Self::new()
    }
}

impl Harnesses {
    /// The harness list with nothing loaded yet.
    /// Input: none. Output: the screen; `shown` asks the engine for the catalog.
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            load: Load::Idle,
            waiting: false,
            selected: 0,
            offset: 0,
        }
    }

    /// The harness list with a catalog already known.
    /// Input: the entries. Output: the screen.
    #[cfg(all(test, unix))]
    pub fn with(entries: Vec<HarnessEntry>) -> Self {
        let mut screen = Self::new();
        screen.entries = entries;
        screen.load = Load::Ready(());
        screen
    }

    fn reload(&mut self, ctx: &mut Ctx) {
        if self.waiting {
            return;
        }
        if matches!(self.load, Load::Idle | Load::Failed(_)) {
            self.load = Load::Loading;
        }
        self.waiting = true;
        send(ctx, SettingsJob::Harnesses);
    }

    fn installed(&self) -> usize {
        self.entries
            .iter()
            .filter(|entry| entry.executable.is_some())
            .count()
    }

    fn window(&self, ctx: &Ctx) -> usize {
        let head = if ctx.wide() { 4 } else { 2 };
        (ctx.height as usize)
            .saturating_sub(CHROME_ROWS + head + 1 + DETAIL_ROWS + 1)
            .max(1)
            .min(self.entries.len().max(1))
    }

    fn widths(limit: usize, wide: bool) -> Vec<usize> {
        let first = if wide { 14 } else { 12 };
        let fixed = 2 + first + 15 + 17 + 3 * 2;
        vec![first, 15, 17, limit.saturating_sub(fixed).max(1)]
    }

    fn detail(entry: &HarnessEntry, limit: usize, ctx: &Ctx) -> Vec<Line<'static>> {
        let theme = &ctx.theme;
        let glyphs = ctx.glyphs;
        let label = |name: &str, value: String| {
            join([
                pad(styled(name, Role::Heading, theme), LABEL_WIDTH, glyphs),
                Line::raw(value),
            ])
        };
        let name = styled(entry.name.clone(), Role::Accent, theme);
        let head = if matches!(entry.status, HarnessStatus::Ready) {
            name
        } else {
            let room = (width(&entry.name) + 2).max(LABEL_WIDTH);
            join([
                pad(name, room, glyphs),
                status(status_word(entry), theme, glyphs),
            ])
        };
        let mut lines = vec![head];
        let sentence = control_sentence(entry);
        match &entry.executable {
            Some(path) => {
                let room = limit.saturating_sub(LABEL_WIDTH).max(8);
                lines.push(label(
                    "Found at",
                    cut(&path.display().to_string(), room, glyphs.ellipsis),
                ));
                for (index, part) in wrap(&sentence, room).into_iter().enumerate() {
                    if index == 0 {
                        lines.push(label("Control", part));
                    } else {
                        lines.push(Line::raw(format!("{}{part}", " ".repeat(LABEL_WIDTH))));
                    }
                }
            }
            None => {
                lines.extend(wrap(&sentence, limit.max(8)).into_iter().map(Line::raw));
                lines.push(Line::raw(
                    "Install it yourself, then press r to check again.",
                ));
            }
        }
        lines.truncate(DETAIL_ROWS);
        lines
    }

    /// The body lines of the page.
    /// Inputs: the content width and context. Output: the lines below the tab strip.
    pub fn body(&mut self, limit: usize, ctx: &Ctx) -> Vec<Line<'static>> {
        let theme = &ctx.theme;
        let glyphs = ctx.glyphs;
        match &self.load {
            Load::Failed(code) => {
                return vec![
                    styled("Harnesses", Role::Accent, theme),
                    Line::default(),
                    status(
                        &format!("Failed to load harnesses. {}", plain(code)),
                        theme,
                        glyphs,
                    ),
                    Line::raw("Press r to try again."),
                ];
            }
            Load::Ready(()) => {}
            _ => {
                return vec![
                    styled("Harnesses", Role::Accent, theme),
                    Line::default(),
                    Line::from(vec![
                        Span::styled(ctx.spinner(), theme.running),
                        Span::raw(" Looking for harnesses on this Mac"),
                    ]),
                ];
            }
        }
        let wide = ctx.wide();
        let mut lines = vec![row(
            limit,
            styled("Harnesses", Role::Accent, theme),
            styled(
                format!("{} of {} installed", self.installed(), self.entries.len()),
                Role::Muted,
                theme,
            ),
            glyphs,
        )];
        if wide {
            lines.extend(
                wrap(SUBTITLE, limit)
                    .into_iter()
                    .map(|part| styled(part, Role::Muted, theme)),
            );
            lines.push(Line::default());
        }
        let count = self.entries.len();
        let window = self.window(ctx);
        let row_limit = if count > window { limit - 1 } else { limit };
        let widths = Self::widths(row_limit, wide);
        let heads = ["Harness", "Status", "Control", "Works with"];
        lines.push(join([
            Line::raw("  "),
            restyle(
                cells(
                    &widths,
                    heads.into_iter().map(Line::raw).collect(),
                    2,
                    glyphs,
                ),
                theme.muted,
            ),
        ]));
        let selected = self.selected.min(count.saturating_sub(1));
        let mut offset = self.offset.min(count.saturating_sub(window));
        if selected < offset {
            offset = selected;
        } else if selected >= offset + window {
            offset = selected + 1 - window;
        }
        let rows: Vec<Line<'static>> = self
            .entries
            .iter()
            .enumerate()
            .skip(offset)
            .take(window)
            .map(|(index, entry)| {
                let values = vec![
                    Line::raw(entry.name.clone()),
                    status(status_word(entry), theme, glyphs),
                    status(control_word(entry), theme, glyphs),
                    Line::raw(works_with(&entry.account_kinds)),
                ];
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
        if let Some(entry) = self.entries.get(selected) {
            lines.push(Line::default());
            lines.extend(Self::detail(entry, limit, ctx));
        }
        self.selected = selected;
        self.offset = offset;
        lines
    }

    fn move_by(&mut self, delta: isize) {
        let count = self.entries.len();
        if count == 0 {
            self.selected = 0;
            return;
        }
        let target = self.selected as isize + delta;
        self.selected = target.clamp(0, count as isize - 1) as usize;
    }
}

impl Screen for Harnesses {
    fn crumbs(&self) -> Vec<String> {
        vec!["Settings".into(), "Harnesses".into()]
    }

    fn hints(&self, ctx: &Ctx) -> Vec<Hint> {
        let arrows = format!("{}{}", ctx.glyphs.up, ctx.glyphs.down);
        vec![
            hint(&arrows, "Move"),
            hint("r", "Check again"),
            sections_hint(ctx.glyphs),
            hint("Esc", "Home"),
        ]
    }

    fn key(&mut self, key: &Key, ctx: &mut Ctx) -> Flow {
        let page = self.window(ctx).saturating_sub(1).max(1) as isize;
        match key {
            Key::Char('r') => self.reload(ctx),
            Key::Up => self.move_by(-1),
            Key::Down => self.move_by(1),
            Key::PageUp => self.move_by(-page),
            Key::PageDown => self.move_by(page),
            Key::Home => self.selected = 0,
            Key::End => self.selected = self.entries.len().saturating_sub(1),
            _ => return Flow::Ignored,
        }
        Flow::Handled
    }

    fn step(&mut self, forward: bool, ctx: &mut Ctx) -> Flow {
        step(SettingsSection::Harnesses, forward, ctx)
    }

    fn arrive(&mut self, arrival: &Arrival, ctx: &mut Ctx) -> Flow {
        let Some(SettingsEvent::Harnesses(result)) = settings_event(arrival) else {
            return Flow::Handled;
        };
        let asked = std::mem::take(&mut self.waiting);
        match result {
            Ok(entries) => {
                self.entries = entries.clone();
                self.load = Load::Ready(());
                self.selected = self.selected.min(self.entries.len().saturating_sub(1));
                let ready = self
                    .entries
                    .iter()
                    .filter(|entry| entry.status == HarnessStatus::Ready)
                    .count();
                ctx.store.harnesses = Load::Ready((ready, self.entries.len()));
            }
            Err(code) if self.entries.is_empty() => self.load = Load::Failed(code.clone()),
            Err(code) if asked => {
                ctx.say(Message::error("Could not check harnesses.", plain(code)))
            }
            Err(_) => {}
        }
        Flow::Handled
    }

    fn shown(&mut self, ctx: &mut Ctx) {
        if matches!(self.load, Load::Idle) {
            self.reload(ctx);
        }
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect, ctx: &Ctx) -> Option<Position> {
        let limit = (area.width as usize).saturating_sub(4).max(8);
        let content = self.body(limit, ctx);
        draw_page(frame, area, SettingsSection::Harnesses, content, None, ctx)
    }

    fn busy(&self, _ctx: &Ctx) -> bool {
        self.waiting
    }

    fn place(&self) -> Option<Place> {
        Some(Place::Settings(SettingsSection::Harnesses))
    }
}

#[cfg(all(test, unix))]
mod tests;
