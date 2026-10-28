use super::new_project_view::printable;
use super::project_markdown::sanitize;
use super::settings::{draw_page, step};
use super::settings_accounts::{plain, send, settings_event};
use super::{Flow, Place, Screen, SettingsSection};
use crate::memory::library::{
    AgentWrites, MemoryOrigin, MemoryScope, NOTE_BYTES_MAX, SavedMemory, validate_note,
};
use crate::protocol::ProjectId;
use crate::tui::client::settings::{SettingsEvent, SettingsJob, Suggestion};
use crate::tui::ctx::{Ctx, Load};
use crate::tui::frame::{Hint, Message, Modal, hint, sections_hint};
use crate::tui::keys::Key;
use crate::tui::seam::{Arrival, NOT_READY, Request};
use crate::tui::text::{
    cells, cut, join, list_row, restyle, row, status, styled, thumb, width, with_bar, wrap,
};
use crate::tui::theme::Role;
use crate::tui::widgets::{TextField, buttons};
use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::text::{Line, Span};

const FILTER_CHARS_MAX: usize = 96;
const HEAD_ROWS: usize = 6;
const CHROME_ROWS: usize = 4 + 1;
const DETAIL_ROWS: usize = 3;
const MODAL_WIDTH: u16 = 66;
const COMPOSER_WIDTH: usize = 72;
const FOLDER_CHARS_MAX: usize = 1024;
const SUBTITLE: &str = "Long-term memory every agent reads at launch, with who saved it.";
const EMPTY: &str = "When agents find something worth keeping, the project shows it under \"Save to memory\". One click saves it here, and every project can use it.";

/// A plain sentence for an engine problem code about memory.
/// Input: the code. Output: the sentence; other codes use the accounts sentences.
pub fn memory_plain(code: &str) -> &'static str {
    match code {
        "memory_note_blank" => "Write something to remember first.",
        "memory_note_too_long" => "Keep a memory under 2000 characters.",
        "memory_note_invalid" => "A memory can't hold control characters.",
        "memory_duplicate" => "This memory is already saved.",
        "memory_full" => "Memory is full. Delete an old memory first.",
        "memory_project_missing" => "That project no longer exists.",
        "memory_import_folder_invalid" => {
            "Choose a folder that exists. A link to a folder can't be imported."
        }
        "memory_import_invalid" => "A file in this folder can't be imported.",
        "memory_missing" | "knowledge_entry_missing" => "This memory no longer exists.",
        "memory_already_global" => "This memory is already saved for every project.",
        "memory_not_requested" => "This memory is already approved.",
        "memory_agent_invalid" => "The agent that saved this memory could not be named.",
        "knowledge_source_revoked" => "Its source was removed, so it can't be saved.",
        "knowledge_version_conflict"
        | "memory_changed"
        | "memory_not_kept"
        | "memory_version_exhausted" => "This suggestion changed. The list now shows the latest.",
        _ => plain(code),
    }
}

fn project_name(id: &ProjectId, ctx: &Ctx) -> String {
    ctx.store
        .projects
        .ready()
        .and_then(|cards| cards.iter().find(|card| &card.id == id))
        .map_or_else(|| "A project".to_owned(), |card| card.name.clone())
}

/// Where a saved memory came from, as the From column shows it.
/// Inputs: the memory and context. Output: the agent that saved it, the project
/// name, or "You" for a note the owner wrote for every project.
pub fn source(memory: &SavedMemory, ctx: &Ctx) -> String {
    match (&memory.origin, &memory.scope) {
        (MemoryOrigin::Agent { agent } | MemoryOrigin::Requested { agent }, _) => {
            printable(agent.clone())
        }
        (MemoryOrigin::Promoted { project_id, .. }, _)
        | (_, MemoryScope::Project { project_id }) => project_name(project_id, ctx),
        (
            MemoryOrigin::Written | MemoryOrigin::Selected { .. } | MemoryOrigin::Imported { .. },
            MemoryScope::Global,
        ) => "You".into(),
    }
}

/// One sentence on how a memory was saved and where agents get it.
/// Inputs: the memory and context. Output: "Saved by you for every project." and so on.
pub fn saved_line(memory: &SavedMemory, ctx: &Ctx) -> String {
    let reach = |scope: &MemoryScope| match scope {
        MemoryScope::Global => "every project".to_owned(),
        MemoryScope::Project { project_id } => format!("{} only", project_name(project_id, ctx)),
    };
    match (&memory.origin, &memory.scope) {
        (MemoryOrigin::Requested { agent }, scope) => format!(
            "Needs approval. {} asked to save this for {}; agents get it once you approve it.",
            printable(agent.clone()),
            reach(scope)
        ),
        (MemoryOrigin::Agent { agent }, scope) => {
            format!(
                "Saved by {} for {}.",
                printable(agent.clone()),
                reach(scope)
            )
        }
        (MemoryOrigin::Imported { file }, MemoryScope::Project { project_id }) => format!(
            "Imported from {} for {} only.",
            printable(file.clone()),
            project_name(project_id, ctx)
        ),
        (_, MemoryScope::Project { project_id }) => {
            format!("Saved for {} only.", project_name(project_id, ctx))
        }
        (MemoryOrigin::Promoted { project_id, .. }, MemoryScope::Global) => {
            format!(
                "Saved from {} for every project.",
                project_name(project_id, ctx)
            )
        }
        (
            MemoryOrigin::Written | MemoryOrigin::Selected { .. } | MemoryOrigin::Imported { .. },
            MemoryScope::Global,
        ) => "Saved by you for every project.".into(),
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
    Delete {
        delete_focused: bool,
    },
    Add {
        field: Box<TextField>,
        scope: usize,
    },
    Import {
        field: Box<TextField>,
        project: usize,
    },
    Suggestion {
        save_focused: bool,
    },
}

enum Picked<'a> {
    Suggested(&'a Suggestion),
    Saved(&'a SavedMemory),
}

/// Settings, Memory: every saved memory, global and per project, with where it
/// came from; View shows the whole text and Delete removes it for good.
/// Input: keys and saved memories from the engine. Output: the list, the empty
/// state or the error with the key to try again.
pub struct Memory {
    memories: Vec<SavedMemory>,
    suggestions: Vec<Suggestion>,
    load: Load<()>,
    waiting: bool,
    writes: Option<AgentWrites>,
    setting: bool,
    approving: bool,
    deleting: bool,
    suggesting: bool,
    adding: bool,
    importing: bool,
    deciding: Option<(Suggestion, bool)>,
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
            suggestions: Vec::new(),
            load: Load::Idle,
            waiting: false,
            writes: None,
            setting: false,
            approving: false,
            deleting: false,
            suggesting: false,
            adding: false,
            importing: false,
            deciding: None,
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
        if !self.waiting {
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
            if !self.setting {
                send(ctx, SettingsJob::AgentWrites { set: None });
            }
        }
        self.reload_suggestions(ctx);
    }

    fn toggle_writes(&mut self, ctx: &mut Ctx) {
        if self.setting {
            return;
        }
        let next = match self.writes.unwrap_or_default() {
            AgentWrites::Save => AgentWrites::Ask,
            AgentWrites::Ask => AgentWrites::Save,
        };
        self.setting = true;
        let generation = ctx.next_generation();
        send(
            ctx,
            SettingsJob::AgentWrites {
                set: Some((next, generation)),
            },
        );
    }

    fn approve(&mut self, ctx: &mut Ctx) {
        let Some(memory) = self.focused(ctx).filter(|memory| memory.origin.waiting()) else {
            return;
        };
        if self.approving {
            return;
        }
        self.approving = true;
        let generation = ctx.next_generation();
        send(
            ctx,
            SettingsJob::ApproveMemory {
                memory: memory.id,
                generation,
            },
        );
    }

    fn writes_line(&self, limit: usize, ctx: &Ctx) -> Line<'static> {
        let theme = &ctx.theme;
        let chosen = match self.writes {
            None => "Loading",
            Some(AgentWrites::Save) => "Save directly",
            Some(AgentWrites::Ask) => "Ask me first",
        };
        let mut line = join([
            styled("Agent memory writes  ", Role::Heading, theme),
            Line::raw(format!("{} {chosen} {}", ctx.glyphs.left, ctx.glyphs.right)),
        ]);
        if self.setting {
            line = join([line, styled("  Saving", Role::Muted, theme)]);
        } else {
            line = join([line, styled("  w to change", Role::Muted, theme)]);
        }
        if line.width() > limit {
            return Line::raw(cut(
                &format!("Agent memory writes: {chosen}"),
                limit,
                ctx.glyphs.ellipsis,
            ));
        }
        line
    }

    fn reload_suggestions(&mut self, ctx: &mut Ctx) {
        let projects: Vec<ProjectId> = self
            .scoped
            .iter()
            .filter_map(|scope| match scope {
                MemoryScope::Project { project_id } => Some(project_id.clone()),
                MemoryScope::Global => None,
            })
            .collect();
        if projects.is_empty() {
            self.suggestions.clear();
            return;
        }
        if self.suggesting {
            return;
        }
        self.suggesting = true;
        send(ctx, SettingsJob::Suggestions { projects });
    }

    fn matches(&self, text: &str) -> bool {
        let text = text.to_lowercase();
        self.filter
            .value()
            .split_whitespace()
            .all(|word| text.contains(word.to_lowercase().as_str()))
    }

    fn promoted(&self, item: &Suggestion) -> bool {
        self.memories.iter().any(|memory| {
            matches!(
                &memory.origin,
                MemoryOrigin::Promoted { project_id, entry_id, .. }
                    if *project_id == item.project && *entry_id == item.entry
            )
        })
    }

    fn open(&self) -> impl Iterator<Item = &Suggestion> {
        self.suggestions
            .iter()
            .filter(move |item| !(item.kept && self.promoted(item)))
    }

    fn suggested<'a>(&'a self, ctx: &Ctx) -> Vec<&'a Suggestion> {
        self.open()
            .filter(|item| {
                self.matches(&format!(
                    "{} {}",
                    item.text,
                    project_name(&item.project, ctx)
                ))
            })
            .collect()
    }

    fn count(&self, ctx: &Ctx) -> usize {
        self.suggested(ctx).len() + self.visible(ctx).len()
    }

    fn picked<'a>(&'a self, ctx: &Ctx) -> Option<Picked<'a>> {
        let suggested = self.suggested(ctx);
        match suggested.get(self.selected).copied() {
            Some(item) => Some(Picked::Suggested(item)),
            None => self
                .visible(ctx)
                .get(self.selected - suggested.len())
                .copied()
                .map(Picked::Saved),
        }
    }

    fn scope_choices(ctx: &Ctx) -> Vec<(MemoryScope, String)> {
        std::iter::once((MemoryScope::Global, "Every project".to_owned()))
            .chain(
                ctx.store
                    .projects
                    .ready()
                    .into_iter()
                    .flatten()
                    .map(|card| {
                        (
                            MemoryScope::Project {
                                project_id: card.id.clone(),
                            },
                            format!("{} only", card.name),
                        )
                    }),
            )
            .collect()
    }

    fn add_key(&mut self, key: &Key, ctx: &mut Ctx) -> Flow {
        let Mode::Add { field, scope } = &mut self.mode else {
            return Flow::Ignored;
        };
        let choices = Self::scope_choices(ctx);
        assert!(!choices.is_empty());
        *scope = (*scope).min(choices.len() - 1);
        match key {
            Key::Esc => self.mode = Mode::List,
            Key::Tab => *scope = (*scope + 1) % choices.len(),
            Key::BackTab => *scope = (*scope + choices.len() - 1) % choices.len(),
            Key::Enter if self.adding => {}
            Key::Enter => {
                let text = field.value().trim().to_owned();
                if let Err(error) = validate_note(&text) {
                    ctx.say(Message::error(memory_plain(&error.0), ""));
                    return Flow::Handled;
                }
                self.adding = true;
                let generation = ctx.next_generation();
                send(
                    ctx,
                    SettingsJob::AddMemory {
                        scope: choices[*scope].0.clone(),
                        text,
                        generation,
                    },
                );
            }
            other => {
                field.key(other);
            }
        }
        Flow::Handled
    }

    fn project_choices(ctx: &Ctx) -> Vec<(ProjectId, String)> {
        ctx.store
            .projects
            .ready()
            .into_iter()
            .flatten()
            .map(|card| (card.id.clone(), card.name.clone()))
            .collect()
    }

    fn open_importer(&mut self, ctx: &mut Ctx) {
        if Self::project_choices(ctx).is_empty() {
            ctx.say(Message::error(
                "Add a project first.",
                "Imported memories belong to one project.",
            ));
            return;
        }
        self.mode = Mode::Import {
            field: Box::new(TextField::new("Folder", FOLDER_CHARS_MAX)),
            project: 0,
        };
    }

    fn import_key(&mut self, key: &Key, ctx: &mut Ctx) -> Flow {
        let Mode::Import { field, project } = &mut self.mode else {
            return Flow::Ignored;
        };
        let choices = Self::project_choices(ctx);
        if choices.is_empty() {
            self.mode = Mode::List;
            return Flow::Handled;
        }
        *project = (*project).min(choices.len() - 1);
        match key {
            Key::Esc => self.mode = Mode::List,
            Key::Tab => *project = (*project + 1) % choices.len(),
            Key::BackTab => *project = (*project + choices.len() - 1) % choices.len(),
            Key::Enter if self.importing => {}
            Key::Enter => {
                let typed = field.value().trim();
                let folder = match (typed.strip_prefix("~/"), ctx.home.as_deref()) {
                    (Some(rest), Some(home)) => format!("{home}/{rest}"),
                    _ => typed.to_owned(),
                };
                if !folder.starts_with('/') {
                    field.error = Some(
                        "Use the full path of the folder, such as ~/.claude/projects/my-app/memory."
                            .into(),
                    );
                    return Flow::Handled;
                }
                field.error = None;
                self.importing = true;
                let generation = ctx.next_generation();
                send(
                    ctx,
                    SettingsJob::ImportMemory {
                        project: choices[*project].0.clone(),
                        folder,
                        generation,
                    },
                );
            }
            other => {
                field.key(other);
            }
        }
        Flow::Handled
    }

    fn importer(&self, limit: usize, ctx: &Ctx) -> (Vec<Line<'static>>, Option<Position>) {
        let Mode::Import { field, project } = &self.mode else {
            return (Vec::new(), None);
        };
        let theme = &ctx.theme;
        let choices = Self::project_choices(ctx);
        let chosen = choices
            .get((*project).min(choices.len().saturating_sub(1)))
            .map_or("No project", |choice| choice.1.as_str());
        let mut lines = vec![
            styled("Import memory", Role::Accent, theme),
            styled(
                "Each markdown file in the folder becomes one memory of the chosen project. MEMORY.md, links and other files are left out.",
                Role::Muted,
                theme,
            ),
            Line::default(),
        ];
        let top = lines.len() as u16;
        let (field_lines, at) = field.lines(limit.min(COMPOSER_WIDTH), true, theme, ctx.glyphs);
        lines.extend(field_lines);
        lines.push(Line::default());
        lines.push(join([
            styled("For  ", Role::Heading, theme),
            Line::raw(format!("{} {chosen} {}", ctx.glyphs.left, ctx.glyphs.right)),
        ]));
        if self.importing {
            lines.push(ctx.loading("Importing"));
        }
        (lines, at.map(|at| Position::new(at.x, at.y + top)))
    }

    fn decide(&mut self, keep: bool, ctx: &mut Ctx) {
        self.mode = Mode::List;
        if self.deciding.is_some() {
            return;
        }
        let Some(Picked::Suggested(item)) = self.picked(ctx) else {
            return;
        };
        let suggestion = item.clone();
        self.deciding = Some((suggestion.clone(), keep));
        let generations = [ctx.next_generation(), ctx.next_generation()];
        send(
            ctx,
            SettingsJob::Suggestion {
                suggestion,
                keep,
                generations,
            },
        );
    }

    fn suggestion_key(&mut self, key: &Key, ctx: &mut Ctx) -> Flow {
        let kept = matches!(self.picked(ctx), Some(Picked::Suggested(item)) if item.kept);
        let Mode::Suggestion { save_focused } = &mut self.mode else {
            return Flow::Ignored;
        };
        match key {
            Key::Esc => self.mode = Mode::List,
            Key::Left | Key::Right | Key::Tab | Key::BackTab => *save_focused = !*save_focused,
            Key::Char('y') => self.decide(true, ctx),
            Key::Char('n') if kept => self.mode = Mode::List,
            Key::Char('n') => self.decide(false, ctx),
            Key::Enter if kept && !*save_focused => self.mode = Mode::List,
            Key::Enter => {
                let keep = *save_focused;
                self.decide(keep, ctx);
            }
            _ => {}
        }
        Flow::Handled
    }

    fn composer(&self, limit: usize, ctx: &Ctx) -> (Vec<Line<'static>>, Option<Position>) {
        let Mode::Add { field, scope } = &self.mode else {
            return (Vec::new(), None);
        };
        let theme = &ctx.theme;
        let choices = Self::scope_choices(ctx);
        let chosen = &choices[(*scope).min(choices.len() - 1)].1;
        let mut lines = vec![
            styled("Add memory", Role::Accent, theme),
            styled(
                "Every agent in the chosen projects reads this before it starts.",
                Role::Muted,
                theme,
            ),
            Line::default(),
        ];
        let top = lines.len() as u16;
        let (field_lines, at) = field.lines(limit.min(COMPOSER_WIDTH), true, theme, ctx.glyphs);
        lines.extend(field_lines);
        lines.push(Line::default());
        lines.push(join([
            styled("For  ", Role::Heading, theme),
            Line::raw(format!("{} {chosen} {}", ctx.glyphs.left, ctx.glyphs.right)),
        ]));
        if self.adding {
            lines.push(ctx.loading("Saving"));
        }
        (lines, at.map(|at| Position::new(at.x, at.y + top)))
    }

    fn visible<'a>(&'a self, ctx: &Ctx) -> Vec<&'a SavedMemory> {
        self.memories
            .iter()
            .filter(|memory| self.matches(&format!("{} {}", memory.text, source(memory, ctx))))
            .collect()
    }

    fn focused(&self, ctx: &Ctx) -> Option<SavedMemory> {
        match self.picked(ctx)? {
            Picked::Saved(memory) => Some(memory.clone()),
            Picked::Suggested(_) => None,
        }
    }

    fn clamp(&mut self, ctx: &Ctx) {
        let count = self.count(ctx);
        self.selected = self.selected.min(count.saturating_sub(1));
    }

    fn open_composer(&mut self) {
        self.mode = Mode::Add {
            field: Box::new(TextField::new("Memory", NOTE_BYTES_MAX).multiline()),
            scope: 0,
        };
    }

    fn suggestion_modal(item: &Suggestion, save_focused: bool, ctx: &Ctx) -> (Modal, Vec<Hint>) {
        let theme = &ctx.theme;
        let room = usize::from(MODAL_WIDTH) - 4;
        let mut body: Vec<Line<'static>> = wrap(&sanitize(&item.text), room)
            .into_iter()
            .map(Line::raw)
            .collect();
        body.push(Line::default());
        let project = project_name(&item.project, ctx);
        let (reach, choices) = if item.kept {
            (
                format!("Kept for {project} only. Use it in every project?"),
                ["Cancel", "Use in all projects"],
            )
        } else {
            (
                format!("From research in {project}. Save it for every project?"),
                ["Dismiss", "Save to memory"],
            )
        };
        body.extend(wrap(&reach, room).into_iter().map(Line::raw));
        body.push(Line::default());
        body.push(buttons(&choices, Some(usize::from(save_focused)), theme));
        let arrows = format!("{}{}", ctx.glyphs.left, ctx.glyphs.right);
        (
            Modal {
                width: MODAL_WIDTH,
                title: if item.kept {
                    "Use in all projects"
                } else {
                    "Suggested memory"
                }
                .into(),
                body,
                top: None,
            },
            vec![
                hint(&arrows, "Choose"),
                hint("y", "Save"),
                hint("n", "Dismiss"),
                hint("Esc", "Close"),
            ],
        )
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
        let count = self.count(ctx);
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
        if matches!(self.mode, Mode::Add { .. }) {
            return self.composer(limit, ctx);
        }
        if matches!(self.mode, Mode::Import { .. }) {
            return self.importer(limit, ctx);
        }
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
            Load::Ready(()) if self.memories.is_empty() && self.open().next().is_none() => {
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
        let kept = self.open().filter(|item| item.kept).count();
        let waiting = self.open().count() - kept;
        let mut counts = format!("{} saved", self.memories.len());
        let asked = self
            .memories
            .iter()
            .filter(|memory| memory.origin.waiting())
            .count();
        if asked > 0 {
            counts.push_str(&format!("   {asked} need approval"));
        }
        if waiting > 0 {
            counts.push_str(&format!("   {waiting} suggested"));
        }
        if kept > 0 {
            counts.push_str(&format!("   {kept} kept in a project"));
        }
        let mut lines = vec![row(
            limit,
            styled("Memory", Role::Accent, theme),
            styled(counts, Role::Muted, theme),
            glyphs,
        )];
        lines.push(styled(
            cut(SUBTITLE, limit, glyphs.ellipsis),
            Role::Muted,
            theme,
        ));
        lines.push(self.writes_line(limit, ctx));
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
        let suggested = self.suggested(ctx);
        let visible = self.visible(ctx);
        let count = suggested.len() + visible.len();
        let window = Self::window(ctx);
        let selected = self.selected.min(count.saturating_sub(1));
        let mut offset = self.offset.min(count.saturating_sub(window));
        if selected < offset {
            offset = selected;
        } else if selected >= offset + window {
            offset = selected + 1 - window;
        }
        let row_limit = if count > window { limit - 1 } else { limit };
        let entries: Vec<(Line<'static>, Line<'static>)> = suggested
            .iter()
            .map(|item| {
                let from = if item.kept {
                    Line::raw(project_name(&item.project, ctx))
                } else {
                    status("Suggested", theme, glyphs)
                };
                (Line::raw(printable(item.text.clone())), from)
            })
            .chain(visible.iter().map(|memory| {
                let from = if memory.origin.waiting() {
                    status("Needs approval", theme, glyphs)
                } else {
                    Line::raw(source(memory, ctx))
                };
                (Line::raw(printable(memory.text.clone())), from)
            }))
            .collect();
        let rows: Vec<Line<'static>> = entries
            .into_iter()
            .enumerate()
            .skip(offset)
            .take(window)
            .map(|(index, (text, from))| {
                let values = vec![text, from];
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
        self.selected = selected;
        match (ctx.wide(), self.picked(ctx)) {
            (true, Some(Picked::Saved(memory))) => {
                lines.push(Line::default());
                lines.push(Line::raw(cut(
                    &printable(memory.text.clone()),
                    limit,
                    glyphs.ellipsis,
                )));
                lines.push(status(&saved_line(memory, ctx), theme, glyphs));
            }
            (true, Some(Picked::Suggested(item))) => {
                lines.push(Line::default());
                lines.push(Line::raw(cut(
                    &printable(item.text.clone()),
                    limit,
                    glyphs.ellipsis,
                )));
                let project = project_name(&item.project, ctx);
                let from = if item.kept {
                    format!("Kept for {project} only. Enter to use it in all projects.")
                } else {
                    format!(
                        "From research in {project}. Enter to save it for every project or dismiss it."
                    )
                };
                lines.push(Line::raw(cut(&from, limit, glyphs.ellipsis)));
            }
            _ => {}
        }
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
        let count = self.count(ctx);
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
        if matches!(self.mode, Mode::Add { .. }) {
            return vec![
                hint("Enter", "Save"),
                hint("Ctrl-J", "New line"),
                hint("Tab", "Projects"),
                hint("Esc", "Cancel"),
            ];
        }
        if matches!(self.mode, Mode::Import { .. }) {
            return vec![
                hint("Enter", "Import"),
                hint("Tab", "Projects"),
                hint("Esc", "Cancel"),
            ];
        }
        if !matches!(self.load, Load::Ready(())) {
            return vec![sections_hint(ctx.glyphs), hint("Esc", "Home")];
        }
        let mut hints = match self.picked(ctx) {
            Some(Picked::Suggested(item)) if item.kept => vec![hint("Enter", "Use everywhere")],
            Some(Picked::Suggested(_)) => vec![hint("Enter", "Save or dismiss")],
            Some(Picked::Saved(memory)) if memory.origin.waiting() => vec![
                hint("y", "Approve"),
                hint("Enter", "View"),
                hint("d", "Delete"),
            ],
            Some(Picked::Saved(_)) => vec![hint("Enter", "View"), hint("d", "Delete")],
            None => Vec::new(),
        };
        hints.push(hint("a", "Add"));
        hints.push(hint("w", "Agent memory writes"));
        hints.push(hint("i", "Import"));
        if !self.memories.is_empty() || self.open().next().is_some() {
            hints.push(hint("/", "Filter"));
        }
        hints.push(sections_hint(ctx.glyphs));
        hints.push(hint("Esc", "Home"));
        hints
    }

    fn typing(&self) -> bool {
        self.filtering || matches!(self.mode, Mode::Add { .. } | Mode::Import { .. })
    }

    fn key(&mut self, key: &Key, ctx: &mut Ctx) -> Flow {
        match self.mode {
            Mode::Delete { .. } => return self.delete_key(key, ctx),
            Mode::Add { .. } => return self.add_key(key, ctx),
            Mode::Import { .. } => return self.import_key(key, ctx),
            Mode::Suggestion { .. } => return self.suggestion_key(key, ctx),
            Mode::View => {
                match key {
                    Key::Char('d') if !self.deleting => {
                        self.mode = Mode::Delete {
                            delete_focused: false,
                        }
                    }
                    Key::Char('y') => {
                        self.approve(ctx);
                        self.mode = Mode::List;
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
        let (suggested, saved) = match self.picked(ctx) {
            Some(Picked::Suggested(_)) => (true, false),
            Some(Picked::Saved(_)) => (false, true),
            None => (false, false),
        };
        let ready = matches!(self.load, Load::Ready(()));
        let page = Self::window(ctx).saturating_sub(1).max(1) as isize;
        match key {
            Key::Char('r') if matches!(self.load, Load::Failed(_)) => self.reload(ctx),
            Key::Char('a') if ready && !self.adding => self.open_composer(),
            Key::Char('w') if ready => self.toggle_writes(ctx),
            Key::Char('y') if saved => self.approve(ctx),
            Key::Char('i') if ready && !self.importing => self.open_importer(ctx),
            Key::Char('/') if !self.memories.is_empty() || self.open().next().is_some() => {
                self.filtering = true
            }
            Key::Enter if suggested && self.deciding.is_none() => {
                self.mode = Mode::Suggestion { save_focused: true }
            }
            Key::Enter if saved => self.mode = Mode::View,
            Key::Char('d') if saved && !self.deleting => {
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
            Key::End => self.selected = self.count(ctx).saturating_sub(1),
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
                        self.clamp(ctx);
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
            Some(SettingsEvent::AgentWrites(result)) => {
                let asked = std::mem::take(&mut self.setting);
                match result {
                    Ok(writes) => {
                        self.writes = Some(*writes);
                        if asked {
                            let lead = match writes {
                                AgentWrites::Save => "Agents now save memories directly.",
                                AgentWrites::Ask => "Agents now ask you before a memory is saved.",
                            };
                            ctx.say(Message::success(lead, ""));
                        }
                    }
                    Err(code) if asked => ctx.say(Message::error(
                        "Could not change Agent memory writes.",
                        memory_plain(code),
                    )),
                    Err(_) => {}
                }
            }
            Some(SettingsEvent::MemoryApproved(result)) if self.approving => {
                self.approving = false;
                match result {
                    Ok(()) => {
                        ctx.say(Message::success(
                            "Memory approved. Agents get it from their next launch.",
                            "",
                        ));
                        self.reload(ctx);
                    }
                    Err(code) => ctx.say(Message::error(
                        "Could not approve this memory.",
                        memory_plain(code),
                    )),
                }
            }
            Some(SettingsEvent::MemoryAdded(result)) if self.adding => {
                self.adding = false;
                match result {
                    Ok(()) => {
                        self.mode = Mode::List;
                        ctx.say(Message::success("Memory saved.", ""));
                        self.reload(ctx);
                    }
                    Err(code) => ctx.say(Message::error(
                        "Could not save this memory.",
                        memory_plain(code),
                    )),
                }
            }
            Some(SettingsEvent::MemoryImported(result)) if self.importing => {
                self.importing = false;
                match result {
                    Ok(report) => {
                        self.mode = Mode::List;
                        ctx.say(Message::success(
                            &format!("Imported {}, skipped {}.", report.imported, report.skipped),
                            "",
                        ));
                    }
                    Err(code) => ctx.say(Message::error(
                        "Could not import memories.",
                        memory_plain(code),
                    )),
                }
                self.reload(ctx);
            }
            Some(SettingsEvent::Suggestions(result)) if self.suggesting => {
                self.suggesting = false;
                match result {
                    Ok(found) => {
                        self.suggestions = found.clone();
                        self.clamp(ctx);
                    }
                    Err(code) if code == NOT_READY => {}
                    Err(code) => ctx.say(Message::warning(
                        "Could not read research suggestions.",
                        plain(code),
                    )),
                }
            }
            Some(SettingsEvent::SuggestionDone(done, result))
                if self
                    .deciding
                    .as_ref()
                    .is_some_and(|(asked, _)| asked == done) =>
            {
                let keep = self.deciding.take().is_some_and(|(_, keep)| keep);
                match result {
                    Ok(()) => {
                        let lead = match (done.kept, keep) {
                            (true, _) => "Now used in all projects.",
                            (false, true) => "Saved to memory.",
                            (false, false) => "Suggestion dismissed.",
                        };
                        ctx.say(Message::success(lead, ""));
                        self.suggestions.retain(|item| item != done);
                        self.clamp(ctx);
                        self.reload(ctx);
                    }
                    Err(code) => {
                        ctx.say(Message::error(
                            "Could not finish with this suggestion.",
                            memory_plain(code),
                        ));
                        self.reload(ctx);
                    }
                }
            }
            _ => {}
        }
        Flow::Handled
    }

    fn modal(&self, ctx: &Ctx) -> Option<(Modal, Vec<Hint>)> {
        if let (Mode::Suggestion { save_focused }, Some(Picked::Suggested(item))) =
            (&self.mode, self.picked(ctx))
        {
            return Some(Self::suggestion_modal(item, *save_focused, ctx));
        }
        let memory = self.focused(ctx)?;
        let theme = &ctx.theme;
        let room = usize::from(MODAL_WIDTH) - 4;
        match self.mode {
            Mode::List | Mode::Add { .. } | Mode::Import { .. } | Mode::Suggestion { .. } => None,
            Mode::View => {
                let mut body: Vec<Line<'static>> = wrap(&sanitize(&memory.text), room)
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
                    if memory.origin.waiting() {
                        vec![
                            hint("y", "Approve"),
                            hint("d", "Delete"),
                            hint("Esc", "Close"),
                        ]
                    } else {
                        vec![hint("d", "Delete"), hint("Esc", "Close")]
                    },
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
                    cut(&printable(memory.text.clone()), room, ctx.glyphs.ellipsis),
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
        self.waiting
            || self.setting
            || self.approving
            || self.deleting
            || self.adding
            || self.importing
            || self.suggesting
            || self.deciding.is_some()
    }

    fn place(&self) -> Option<Place> {
        Some(Place::Settings(SettingsSection::Memory))
    }
}

#[cfg(test)]
mod tests;
