use super::settings::{draw_page, step};
use super::settings_accounts::{plain, send, settings_event};
use super::{Flow, Place, Screen, SettingsSection};
use crate::memory::library::{MemoryOrigin, MemoryScope, SavedMemory};
use crate::protocol::ProjectId;
use crate::tui::client::settings::{SettingsEvent, SettingsJob};
use crate::tui::ctx::{Ctx, Load};
use crate::tui::frame::{Hint, Message, Modal, hint, sections_hint};
use crate::tui::keys::Key;
use crate::tui::seam::{Arrival, Request};
use crate::tui::text::{
    cells, cut, join, list_row, restyle, row, status, styled, thumb, width, with_bar, wrap,
};
use crate::tui::theme::Role;
use crate::tui::widgets::{TextField, buttons};
use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::text::{Line, Span};

const FILTER_CHARS_MAX: usize = 96;
const HEAD_ROWS: usize = 5;
const CHROME_ROWS: usize = 4 + 1;
const DETAIL_ROWS: usize = 3;
const MODAL_WIDTH: u16 = 66;
const SUBTITLE: &str =
    "Long-term memory every project can use. Nothing is saved without your click.";
const EMPTY: &str = "When agents find something worth keeping, the project shows it under \"Save to memory\". One click saves it here, and every project can use it.";

fn project_name(id: &ProjectId, ctx: &Ctx) -> String {
    ctx.store
        .projects
        .ready()
        .and_then(|cards| cards.iter().find(|card| &card.id == id))
        .map_or_else(|| "A project".to_owned(), |card| card.name.clone())
}

/// Where a saved memory came from, as the From column shows it.
/// Inputs: the memory and context. Output: the project name, or "You" for a
/// note the owner wrote for every project.
pub fn source(memory: &SavedMemory, ctx: &Ctx) -> String {
    match (&memory.origin, &memory.scope) {
        (MemoryOrigin::Promoted { project_id, .. }, _)
        | (_, MemoryScope::Project { project_id }) => project_name(project_id, ctx),
        (MemoryOrigin::Written, MemoryScope::Global) => "You".into(),
    }
}

/// One sentence on how a memory was saved and where agents get it.
/// Inputs: the memory and context. Output: "Saved by you for every project." and so on.
pub fn saved_line(memory: &SavedMemory, ctx: &Ctx) -> String {
    match (&memory.origin, &memory.scope) {
        (_, MemoryScope::Project { project_id }) => {
            format!("Saved for {} only.", project_name(project_id, ctx))
        }
        (MemoryOrigin::Promoted { project_id, .. }, MemoryScope::Global) => {
            format!(
                "Saved from {} for every project.",
                project_name(project_id, ctx)
            )
        }
        (MemoryOrigin::Written, MemoryScope::Global) => "Saved by you for every project.".into(),
    }
}

/// The memory scopes the page reads: global memory, then each known project.
/// Input: the context. Output: the scopes.
pub fn scopes(ctx: &Ctx) -> Vec<MemoryScope> {
    let projects = ctx
        .store
        .projects
        .ready()
        .map(Vec::as_slice)
        .unwrap_or_default();
    std::iter::once(MemoryScope::Global)
        .chain(projects.iter().map(|card| MemoryScope::Project {
            project_id: card.id.clone(),
        }))
        .collect()
}

enum Mode {
    List,
    View,
    Delete { delete_focused: bool },
}

/// Settings, Memory: every saved memory, global and per project, with where it
/// came from; View shows the whole text and Delete removes it for good.
/// Input: keys and saved memories from the engine. Output: the list, the empty
/// state or the error with the key to try again.
pub struct Memory {
    memories: Vec<SavedMemory>,
    load: Load<()>,
    waiting: bool,
    deleting: bool,
    scoped: Vec<MemoryScope>,
    selected: usize,
    offset: usize,
    filter: TextField,
    filtering: bool,
    mode: Mode,
}

impl Default for Memory {
    fn default() -> Self {
        Self::new()
    }
}

impl Memory {
    /// The memory page with nothing loaded yet.
    /// Input: none. Output: the screen; `shown` asks the engine for memories.
    pub fn new() -> Self {
        Self {
            memories: Vec::new(),
            load: Load::Idle,
            waiting: false,
            deleting: false,
            scoped: Vec::new(),
            selected: 0,
            offset: 0,
            filter: TextField::new("Filter", FILTER_CHARS_MAX),
            filtering: false,
            mode: Mode::List,
        }
    }

    /// The memory page with memories already known.
    /// Input: the memories, newest first. Output: the screen.
    #[cfg(test)]
    pub fn with(memories: Vec<SavedMemory>) -> Self {
        let mut screen = Self::new();
        screen.memories = memories;
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
        self.scoped = scopes(ctx);
        send(
            ctx,
            SettingsJob::Memories {
                scopes: self.scoped.clone(),
            },
        );
    }

    fn visible<'a>(&'a self, ctx: &Ctx) -> Vec<&'a SavedMemory> {
        let words: Vec<String> = self
            .filter
            .value()
            .split_whitespace()
            .map(str::to_lowercase)
            .collect();
        self.memories
            .iter()
            .filter(|memory| {
                let text = format!("{} {}", memory.text, source(memory, ctx)).to_lowercase();
                words.iter().all(|word| text.contains(word.as_str()))
            })
            .collect()
    }

    fn focused(&self, ctx: &Ctx) -> Option<SavedMemory> {
        self.visible(ctx)
            .get(self.selected)
            .map(|memory| (*memory).clone())
    }

    fn window(ctx: &Ctx) -> usize {
        let detail = if ctx.wide() { DETAIL_ROWS } else { 0 };
        (ctx.height as usize)
            .saturating_sub(CHROME_ROWS + HEAD_ROWS + detail)
            .max(1)
    }

    fn widths(wide: bool) -> [usize; 2] {
        if wide { [64, 20] } else { [44, 14] }
    }

    fn move_by(&mut self, delta: isize, ctx: &Ctx) {
        let count = self.visible(ctx).len();
        if count == 0 {
            self.selected = 0;
            return;
        }
        let target = self.selected as isize + delta;
        self.selected = target.clamp(0, count as isize - 1) as usize;
    }

    /// The body lines of the page and the filter cursor.
    /// Inputs: the content width and context. Output: the lines below the tab
    /// strip and the cursor relative to them while the filter has focus.
    pub fn body(&mut self, limit: usize, ctx: &Ctx) -> (Vec<Line<'static>>, Option<Position>) {
        let theme = &ctx.theme;
        let glyphs = ctx.glyphs;
        match &self.load {
            Load::Failed(code) => {
                return (
                    vec![
                        styled("Memory", Role::Accent, theme),
                        Line::default(),
                        status(
                            &format!("Failed to load memories. {}", plain(code)),
                            theme,
                            glyphs,
                        ),
                        Line::raw("Press r to try again."),
                    ],
                    None,
                );
            }
            Load::Ready(()) if self.memories.is_empty() => {
                let mut lines = vec![
                    styled("Memory", Role::Accent, theme),
                    Line::default(),
                    Line::raw("No memories yet."),
                ];
                lines.extend(wrap(EMPTY, limit).into_iter().map(Line::raw));
                return (lines, None);
            }
            Load::Ready(()) => {}
            _ => {
                return (
                    vec![
                        styled("Memory", Role::Accent, theme),
                        Line::default(),
                        Line::from(vec![
                            Span::styled(ctx.spinner(), theme.running),
                            Span::raw(" Loading memories"),
                        ]),
                    ],
                    None,
                );
            }
        }
        let mut lines = vec![row(
            limit,
            styled("Memory", Role::Accent, theme),
            styled(format!("{} saved", self.memories.len()), Role::Muted, theme),
            glyphs,
        )];
        lines.push(styled(
            cut(SUBTITLE, limit, glyphs.ellipsis),
            Role::Muted,
            theme,
        ));
        let mut cursor = None;
        if self.filtering {
            let shown = cut(
                self.filter.value(),
                limit.saturating_sub(3),
                glyphs.ellipsis,
            );
            cursor = Some(Position::new(2 + width(&shown) as u16, 2));
            lines.push(join([styled("/ ", Role::Accent, theme), Line::raw(shown)]));
        } else if self.filter.is_empty() {
            lines.push(join([
                Line::raw("/  "),
                styled("Filter", Role::Muted, theme),
            ]));
        } else {
            lines.push(Line::raw(format!("/ {}", self.filter.value())));
        }
        lines.push(Line::default());
        let widths = Self::widths(ctx.wide());
        lines.push(join([
            Line::raw("  "),
            restyle(
                cells(
                    &widths,
                    vec![Line::raw("Memory"), Line::raw("From")],
                    2,
                    glyphs,
                ),
                theme.muted,
            ),
        ]));
        let visible = self.visible(ctx);
        let count = visible.len();
        let window = Self::window(ctx);
        let selected = self.selected.min(count.saturating_sub(1));
        let mut offset = self.offset.min(count.saturating_sub(window));
        if selected < offset {
            offset = selected;
        } else if selected >= offset + window {
            offset = selected + 1 - window;
        }
        let row_limit = if count > window { limit - 1 } else { limit };
        let rows: Vec<Line<'static>> = visible
            .iter()
            .enumerate()
            .skip(offset)
            .take(window)
            .map(|(index, memory)| {
                let values = vec![
                    Line::raw(memory.text.clone()),
                    Line::raw(source(memory, ctx)),
                ];
                list_row(
                    cells(&widths, values, 2, glyphs),
                    row_limit,
                    index == selected,
                    !self.filtering,
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
        if count == 0 {
            lines.push(Line::raw("No memory matches this filter."));
        }
        if let (true, Some(memory)) = (ctx.wide(), visible.get(selected)) {
            lines.push(Line::default());
            lines.push(Line::raw(cut(&memory.text, limit, glyphs.ellipsis)));
            lines.push(status(&saved_line(memory, ctx), theme, glyphs));
        }
        self.selected = selected;
        self.offset = offset;
        (lines, cursor)
    }

    fn filter_key(&mut self, key: &Key, ctx: &Ctx) -> Flow {
        match key {
            Key::Esc => {
                self.filter.clear();
                self.filtering = false;
            }
            Key::Enter | Key::Down | Key::Tab => self.filtering = false,
            other => {
                self.filter.key(other);
            }
        }
        let count = self.visible(ctx).len();
        self.selected = self.selected.min(count.saturating_sub(1));
        Flow::Handled
    }

    fn delete_key(&mut self, key: &Key, ctx: &mut Ctx) -> Flow {
        let Mode::Delete { delete_focused } = &mut self.mode else {
            return Flow::Ignored;
        };
        match key {
            Key::Esc => self.mode = Mode::List,
            Key::Left | Key::Right | Key::Tab | Key::BackTab => *delete_focused = !*delete_focused,
            Key::Enter if *delete_focused => {
                self.mode = Mode::List;
                if let Some(memory) = self.focused(ctx) {
                    self.deleting = true;
                    let generation = ctx.next_generation();
                    send(
                        ctx,
                        SettingsJob::DeleteMemory {
                            memory: memory.id,
                            generation,
                        },
                    );
                }
            }
            Key::Enter => self.mode = Mode::List,
            _ => {}
        }
        Flow::Handled
    }
}

impl Screen for Memory {
    fn crumbs(&self) -> Vec<String> {
        vec!["Settings".into(), "Memory".into()]
    }

    fn hints(&self, ctx: &Ctx) -> Vec<Hint> {
        if self.filtering {
            return vec![
                hint("Enter", "Keep filter"),
                hint(ctx.glyphs.down, "To the list"),
                hint("Esc", "Clear filter"),
            ];
        }
        if matches!(self.load, Load::Ready(())) && !self.memories.is_empty() {
            return vec![
                hint("Enter", "View"),
                hint("d", "Delete"),
                hint("/", "Filter"),
                sections_hint(ctx.glyphs),
                hint("Esc", "Home"),
            ];
        }
        vec![sections_hint(ctx.glyphs), hint("Esc", "Home")]
    }

    fn typing(&self) -> bool {
        self.filtering
    }

    fn key(&mut self, key: &Key, ctx: &mut Ctx) -> Flow {
        match self.mode {
            Mode::Delete { .. } => return self.delete_key(key, ctx),
            Mode::View => {
                match key {
                    Key::Char('d') if !self.deleting => {
                        self.mode = Mode::Delete {
                            delete_focused: false,
                        }
                    }
                    Key::Esc | Key::Enter => self.mode = Mode::List,
                    _ => {}
                }
                return Flow::Handled;
            }
            Mode::List => {}
        }
        if self.filtering {
            return self.filter_key(key, ctx);
        }
        let any = !self.visible(ctx).is_empty();
        let page = Self::window(ctx).saturating_sub(1).max(1) as isize;
        match key {
            Key::Char('r') if matches!(self.load, Load::Failed(_)) => self.reload(ctx),
            Key::Char('/') if !self.memories.is_empty() => self.filtering = true,
            Key::Enter if any => self.mode = Mode::View,
            Key::Char('d') if any && !self.deleting => {
                self.mode = Mode::Delete {
                    delete_focused: false,
                }
            }
            Key::Esc if !self.filter.is_empty() => self.filter.clear(),
            Key::Up => self.move_by(-1, ctx),
            Key::Down => self.move_by(1, ctx),
            Key::PageUp => self.move_by(-page, ctx),
            Key::PageDown => self.move_by(page, ctx),
            Key::Home => self.selected = 0,
            Key::End => self.selected = self.visible(ctx).len().saturating_sub(1),
            _ => return Flow::Ignored,
        }
        Flow::Handled
    }

    fn step(&mut self, forward: bool, ctx: &mut Ctx) -> Flow {
        step(SettingsSection::Memory, forward, ctx)
    }

    fn arrive(&mut self, arrival: &Arrival, ctx: &mut Ctx) -> Flow {
        if let Arrival::Projects(Ok(_)) = arrival {
            if !matches!(self.load, Load::Idle) && scopes(ctx) != self.scoped {
                self.reload(ctx);
            }
            return Flow::Handled;
        }
        match settings_event(arrival) {
            Some(SettingsEvent::Memories(result)) => {
                let asked = std::mem::take(&mut self.waiting);
                match result {
                    Ok(memories) => {
                        self.memories = memories.clone();
                        self.load = Load::Ready(());
                        let count = self.visible(ctx).len();
                        self.selected = self.selected.min(count.saturating_sub(1));
                        ctx.store.memory = Load::Ready(self.memories.len());
                    }
                    Err(code) if self.memories.is_empty() => self.load = Load::Failed(code.clone()),
                    Err(code) if asked => {
                        ctx.say(Message::error("Could not refresh memories.", plain(code)))
                    }
                    Err(_) => {}
                }
            }
            Some(SettingsEvent::MemoryDeleted(result)) if self.deleting => {
                self.deleting = false;
                match result {
                    Ok(()) => {
                        ctx.say(Message::success("Memory deleted.", ""));
                        self.reload(ctx);
                    }
                    Err(code) => {
                        ctx.say(Message::error("Could not delete this memory.", plain(code)))
                    }
                }
            }
            _ => {}
        }
        Flow::Handled
    }

    fn modal(&self, ctx: &Ctx) -> Option<(Modal, Vec<Hint>)> {
        let memory = self.focused(ctx)?;
        let theme = &ctx.theme;
        let room = usize::from(MODAL_WIDTH) - 4;
        match self.mode {
            Mode::List => None,
            Mode::View => {
                let mut body: Vec<Line<'static>> = wrap(&memory.text, room)
                    .into_iter()
                    .map(Line::raw)
                    .collect();
                body.push(Line::default());
                body.push(status(&saved_line(&memory, ctx), theme, ctx.glyphs));
                Some((
                    Modal {
                        width: MODAL_WIDTH,
                        title: "Memory".into(),
                        body,
                        top: None,
                    },
                    vec![hint("d", "Delete"), hint("Esc", "Close")],
                ))
            }
            Mode::Delete { delete_focused } => {
                let reach = match &memory.scope {
                    MemoryScope::Global => "Agents in every project stop getting it.".to_owned(),
                    MemoryScope::Project { project_id } => {
                        format!(
                            "Agents in {} stop getting it.",
                            project_name(project_id, ctx)
                        )
                    }
                };
                let mut body = vec![Line::raw("Delete this memory for good?"), Line::default()];
                body.push(styled(
                    cut(&memory.text, room, ctx.glyphs.ellipsis),
                    Role::Muted,
                    theme,
                ));
                body.extend(wrap(&reach, room).into_iter().map(Line::raw));
                body.push(Line::default());
                body.push(buttons(
                    &["Cancel", "Delete"],
                    Some(usize::from(delete_focused)),
                    theme,
                ));
                let arrows = format!("{}{}", ctx.glyphs.left, ctx.glyphs.right);
                Some((
                    Modal {
                        width: MODAL_WIDTH,
                        title: "Delete memory".into(),
                        body,
                        top: None,
                    },
                    vec![
                        hint(&arrows, "Choose"),
                        hint("Enter", "Confirm"),
                        hint("Esc", "Cancel"),
                    ],
                ))
            }
        }
    }

    fn shown(&mut self, ctx: &mut Ctx) {
        ctx.request(Request::Projects);
        if matches!(self.load, Load::Idle) {
            self.reload(ctx);
        }
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect, ctx: &Ctx) -> Option<Position> {
        let limit = (area.width as usize).saturating_sub(4).max(8);
        let (content, cursor) = self.body(limit, ctx);
        draw_page(frame, area, SettingsSection::Memory, content, cursor, ctx)
    }

    fn busy(&self, _ctx: &Ctx) -> bool {
        self.waiting || self.deleting
    }

    fn place(&self) -> Option<Place> {
        Some(Place::Settings(SettingsSection::Memory))
    }
}

#[cfg(test)]
mod tests;
