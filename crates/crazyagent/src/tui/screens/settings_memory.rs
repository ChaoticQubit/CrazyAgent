use super::new_project_view::printable;
use super::project_markdown::sanitize;
use super::settings::{draw_page, step};
use super::settings_accounts::{plain, send, settings_event};
use super::{Flow, Place, Screen, SettingsSection};
use crate::memory::library::{
    AgentScope, AgentWrites, MemoryOrigin, MemoryScope, NOTE_BYTES_MAX, SavedMemory, validate_note,
};
use crate::protocol::ProjectId;
use crate::tui::client::settings::{SettingsEvent, SettingsJob, Suggestion};
use crate::tui::ctx::{Ctx, Load};
use crate::tui::frame::{Hint, Message, Modal, draw_lines, hint, sections_hint};
use crate::tui::keys::Key;
use crate::tui::seam::{Arrival, NOT_READY, Request};
use crate::tui::text::{
    cells, cut, indent, join, list_row, restyle, row, status, styled, thumb, width, with_bar, wrap,
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
const PROJECT_SUBTITLE: &str =
    "Long-term memory this project's agents read at launch, with who saved it.";
const PROJECT_EMPTY: &str = "When an agent asks to keep something for this project, it waits here for your approval, and y approves it.";
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
        "memory_not_movable" => "Only an app-wide memory an agent saved can be moved.",
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

/// The line under the subtitle of a project's memory: how many memories wait for the
/// owner's approval and the key that approves one.
/// Inputs: the waiting count, the width and the context. Output: the line.
fn waiting_line(asked: usize, limit: usize, ctx: &Ctx) -> Line<'static> {
    let text = match asked {
        0 => "Nothing waits for your approval.".to_owned(),
        1 => "1 memory waits for your approval. y approves the focused one.".to_owned(),
        _ => format!("{asked} memories wait for your approval. y approves the focused one."),
    };
    let role = if asked == 0 {
        Role::Muted
    } else {
        Role::Heading
    };
    styled(cut(&text, limit, ctx.glyphs.ellipsis), role, &ctx.theme)
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
    Move {
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
/// came from; View shows the whole text and Delete removes it for good. Opened from a
/// project's Context tab it is that project's memory only, where y approves in place.
/// Input: keys and saved memories from the engine. Output: the list, the empty
/// state or the error with the key to try again.
pub struct Memory {
    memories: Vec<SavedMemory>,
    suggestions: Vec<Suggestion>,
    load: Load<()>,
    waiting: bool,
    writes: Option<AgentWrites>,
    setting: bool,
    reach: Option<AgentScope>,
    scoping: bool,
    moving: bool,
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
    project: Option<(ProjectId, String)>,
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
            reach: None,
            scoping: false,
            moving: false,
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
            project: None,
        }
    }

    /// One project's memory, opened from its Context tab: only that project's memories, read
    /// afresh each time it shows, with y approving a memory an agent asked for right there and
    /// s opening Settings, Memory for the app-wide setting and research suggestions.
    /// Input: the project id and name. Output: the screen; `shown` asks the engine.
    pub fn for_project(id: ProjectId, name: String) -> Self {
        let mut screen = Self::new();
        screen.project = Some((id, name));
        screen
    }

    fn wanted(&self, ctx: &Ctx) -> Vec<MemoryScope> {
        match &self.project {
            Some((id, _)) => vec![MemoryScope::Project {
                project_id: id.clone(),
            }],
            None => scopes(ctx),
        }
    }

    fn title(&self) -> &'static str {
        if self.project.is_some() {
            "Project memory"
        } else {
            "Memory"
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
            self.scoped = self.wanted(ctx);
            send(
                ctx,
                SettingsJob::Memories {
                    scopes: self.scoped.clone(),
                },
            );
            if !self.setting && self.project.is_none() {
                send(ctx, SettingsJob::AgentWrites { set: None });
            }
            if !self.scoping && self.project.is_none() {
                send(ctx, SettingsJob::AgentScope { set: None });
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

    fn toggle_scope(&mut self, ctx: &mut Ctx) {
        if self.scoping {
            return;
        }
        let next = match self.reach.unwrap_or_default() {
            AgentScope::Project => AgentScope::Agent,
            AgentScope::Agent => AgentScope::Project,
        };
        self.scoping = true;
        let generation = ctx.next_generation();
        send(
            ctx,
            SettingsJob::AgentScope {
                set: Some((next, generation)),
            },
        );
    }

    fn movable(memory: &SavedMemory) -> bool {
        memory.scope == MemoryScope::Global
            && matches!(
                memory.origin,
                MemoryOrigin::Agent { .. } | MemoryOrigin::Requested { .. }
            )
    }

    fn open_mover(&mut self, ctx: &mut Ctx) {
        if self.moving
            || !self
                .focused(ctx)
                .is_some_and(|memory| Self::movable(&memory))
        {
            return;
        }
        if Self::project_choices(ctx).is_empty() {
            ctx.say(Message::error(
                "Add a project first.",
                "A moved memory belongs to one project.",
            ));
            return;
        }
        self.mode = Mode::Move { project: 0 };
    }

    fn move_key(&mut self, key: &Key, ctx: &mut Ctx) -> Flow {
        let Mode::Move { project } = &mut self.mode else {
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
            Key::Right | Key::Tab | Key::Down => *project = (*project + 1) % choices.len(),
            Key::Left | Key::BackTab | Key::Up => {
                *project = (*project + choices.len() - 1) % choices.len()
            }
            Key::Enter => {
                let target = choices[*project].0.clone();
                self.mode = Mode::List;
                if let Some(memory) = self.focused(ctx).filter(Self::movable) {
                    self.moving = true;
                    let generation = ctx.next_generation();
                    send(
                        ctx,
                        SettingsJob::MoveMemory {
                            memory: memory.id,
                            project: target,
                            generation,
                        },
                    );
                }
            }
            _ => {}
        }
        Flow::Handled
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
        let place = match self.reach {
            None => "Loading",
            Some(AgentScope::Project) => "This project only",
            Some(AgentScope::Agent) => "Agent chooses",
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
        line = join([
            line,
            styled("   Saved in  ", Role::Heading, theme),
            Line::raw(format!("{} {place} {}", ctx.glyphs.left, ctx.glyphs.right)),
        ]);
        if self.scoping {
            line = join([line, styled("  Saving", Role::Muted, theme)]);
        } else {
            line = join([line, styled("  p to change", Role::Muted, theme)]);
        }
        if line.width() <= limit {
            return line;
        }
        let (left, right) = (ctx.glyphs.left, ctx.glyphs.right);
        let compact = join([
            styled("Writes ", Role::Heading, theme),
            Line::raw(format!("{left} {chosen} {right}")),
            styled("  w", Role::Muted, theme),
            styled("   Saved in ", Role::Heading, theme),
            Line::raw(format!("{left} {place} {right}")),
            styled("  p", Role::Muted, theme),
        ]);
        if compact.width() <= limit {
            return compact;
        }
        Line::raw(cut(
            &format!("Agent memory writes: {chosen}; saved in: {place}"),
            limit,
            ctx.glyphs.ellipsis,
        ))
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
        if projects.is_empty() || self.project.is_some() {
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
            .saturating_sub(CHROME_ROWS + HEAD_ROWS + detail + ctx.extra_hint_rows())
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
                        styled(self.title(), Role::Accent, theme),
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
                    styled(self.title(), Role::Accent, theme),
                    Line::default(),
                    Line::raw("No memories yet."),
                ];
                let empty = if self.project.is_some() {
                    PROJECT_EMPTY
                } else {
                    EMPTY
                };
                lines.extend(wrap(empty, limit).into_iter().map(Line::raw));
                return (lines, None);
            }
            Load::Ready(()) => {}
            _ => {
                return (
                    vec![
                        styled(self.title(), Role::Accent, theme),
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
            styled(self.title(), Role::Accent, theme),
            styled(counts, Role::Muted, theme),
            glyphs,
        )];
        let subtitle = if self.project.is_some() {
            PROJECT_SUBTITLE
        } else {
            SUBTITLE
        };
        lines.push(styled(
            cut(subtitle, limit, glyphs.ellipsis),
            Role::Muted,
            theme,
        ));
        lines.push(match self.project {
            Some(_) => waiting_line(asked, limit, ctx),
            None => self.writes_line(limit, ctx),
        });
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

mod screen;

#[cfg(test)]
mod tests;
