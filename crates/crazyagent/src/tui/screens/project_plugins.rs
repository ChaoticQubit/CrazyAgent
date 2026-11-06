use super::new_project_input::char_cells;
use super::new_project_view::{Look, Row, columns, list_row, spread, wrap_words};
use super::project_team::{Help, Notice, TABS, window};
use super::settings_plugins::{hook_runs, plugin_plain, short_runs};
use crate::plugins::{HookView, Scope, ServerStatus, ServerView, SkillView};
use crate::protocol::ProjectId;
use crate::tui::client::settings::{PluginChange, SettingsEvent, SettingsJob};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::Style;
use std::cell::{Cell, RefCell};

const WIDE: usize = 96;
const NAME_WIDTH: usize = 22;
const STATE_WIDTH: usize = 18;
const WHERE_WIDTH: usize = 17;
const PANEL_TEXT_LINES: usize = 2;

/// What the project plugins view asks its host to do after a key.
/// Ignored lets the host's own keys run; Send is one Settings job for the host
/// to queue, whose reply comes back through `apply`; AddSkill, AddServer,
/// AddHooks, Edit, OpenSkill and Tools ask the host to open the add form, the
/// server's edit page, the skill's page or the tool switches for this project.
#[derive(Debug)]
pub(in crate::tui) enum PluginsReply {
    Ignored,
    Handled,
    Send(SettingsJob),
    AddSkill,
    AddServer,
    AddHooks,
    Edit(Box<ServerView>),
    OpenSkill {
        id: String,
        name: String,
    },
    Tools {
        servers: Vec<(String, String)>,
        at: usize,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Item {
    Skill(usize),
    Server(usize),
    Hook(usize),
}

/// The skills, MCP servers and hooks one project's agents get: every-project
/// items with whether they are on here, and this project's own items.
/// Takes engine reads through `apply` and keys through `key`; never invents data.
/// Drawing the approval question keeps its first command line in range and
/// notes the server whose command showed through its last line; `y` approves
/// only that server with that command.
pub(in crate::tui) struct ProjectPlugins {
    pub project: ProjectId,
    pub skills: Vec<SkillView>,
    pub servers: Vec<ServerView>,
    pub hooks: Vec<HookView>,
    pub loaded: bool,
    pub cursor: usize,
    pub confirm: Option<String>,
    pub removing: Option<String>,
    pub notice: Option<Notice>,
    waiting: bool,
    stale: bool,
    hooks_stale: bool,
    changing: Option<String>,
    command_top: Cell<usize>,
    read_through: RefCell<Option<(String, String)>>,
}

impl ProjectPlugins {
    /// Opens one project's plugins with nothing read yet.
    /// Takes the project.
    /// Returns the view; reads start with `next_job`.
    pub fn new(project: ProjectId) -> Self {
        Self {
            project,
            skills: Vec::new(),
            servers: Vec::new(),
            hooks: Vec::new(),
            loaded: false,
            cursor: 0,
            confirm: None,
            removing: None,
            notice: None,
            waiting: false,
            stale: true,
            hooks_stale: false,
            changing: None,
            command_top: Cell::new(0),
            read_through: RefCell::new(None),
        }
    }

    /// Gives the next read: skills and servers at the start and after every
    /// change, then that project's hooks, never two at once.
    /// Takes the view. Returns `SettingsJob::ProjectPlugins`, `SettingsJob::Hooks`
    /// or none.
    pub fn next_job(&mut self) -> Option<SettingsJob> {
        if self.waiting {
            return None;
        }
        if self.stale {
            self.waiting = true;
            self.stale = false;
            self.hooks_stale = true;
            return Some(SettingsJob::ProjectPlugins {
                project: self.project.clone(),
            });
        }
        if self.hooks_stale {
            self.waiting = true;
            self.hooks_stale = false;
            return Some(SettingsJob::Hooks {
                project: Some(self.project.clone()),
            });
        }
        None
    }

    /// Forgets the read and change in flight after the engine connection dropped.
    /// Takes the view; the last lists stay on screen.
    /// Returns nothing; the next `next_job` reads again.
    pub fn forget_reads(&mut self) {
        if self.waiting {
            self.stale = true;
        }
        self.waiting = false;
        self.changing = None;
    }

    /// Applies one Settings reply for this project.
    /// Takes the event; reads for other projects and changes this view did not
    /// ask for are left alone.
    /// Returns nothing.
    pub fn apply(&mut self, event: &SettingsEvent) {
        match event {
            SettingsEvent::ProjectPlugins(project, result) if project == &self.project => {
                self.waiting = false;
                match result {
                    Ok((skills, servers)) => {
                        for (scope, on_here) in skills
                            .iter()
                            .map(|skill| (&skill.scope, skill.on_here))
                            .chain(servers.iter().map(|server| (&server.scope, server.on_here)))
                        {
                            assert!(visible(scope, &self.project));
                            assert_eq!(on_here.is_some(), *scope == Scope::Global);
                        }
                        let asked = self.asking().map(|server| server.runs.clone());
                        self.skills = skills.clone();
                        self.servers = servers.clone();
                        self.loaded = true;
                        if self.asking().map(|server| &server.runs) != asked.as_ref() {
                            self.command_top.set(0);
                            *self.read_through.borrow_mut() = None;
                        }
                    }
                    Err(code) => {
                        self.notice = Some(Notice::problem(format!(
                            "Could not read this project's plugins. {}",
                            plugin_plain(code)
                        )));
                    }
                }
            }
            SettingsEvent::Hooks(Some(project), result) if project == &self.project => {
                self.waiting = false;
                match result {
                    Ok(hooks) => {
                        for hook in hooks {
                            assert!(visible(&hook.scope, &self.project));
                            assert_eq!(hook.on_here.is_some(), hook.scope == Scope::Global);
                        }
                        self.hooks = hooks.clone();
                    }
                    Err(code) => {
                        self.notice = Some(Notice::problem(format!(
                            "Could not read this project's hooks. {}",
                            plugin_plain(code)
                        )));
                    }
                }
            }
            SettingsEvent::PluginChanged(result) => {
                let Some(done) = self.changing.take() else {
                    return;
                };
                self.stale = true;
                self.notice = Some(match result {
                    Ok(Some(server)) if server.problem.is_some() => Notice::problem(format!(
                        "{} can't start. {}",
                        server.name,
                        server.problem.as_deref().unwrap_or_default()
                    )),
                    Ok(_) => Notice::plain(done),
                    Err(code) => Notice::problem(plugin_plain(code)),
                });
            }
            _ => {}
        }
        self.clamp();
    }

    /// Says whether a read or a change is waiting for the engine.
    /// Takes the view.
    /// Returns true until its reply arrives.
    pub fn busy(&self) -> bool {
        self.waiting || self.changing.is_some()
    }

    fn count(&self) -> usize {
        self.skills.len() + self.servers.len() + self.hooks.len()
    }

    fn item(&self) -> Option<Item> {
        let listed = self.skills.len() + self.servers.len();
        if self.cursor < self.skills.len() {
            Some(Item::Skill(self.cursor))
        } else if self.cursor < listed {
            Some(Item::Server(self.cursor - self.skills.len()))
        } else if self.cursor < self.count() {
            Some(Item::Hook(self.cursor - listed))
        } else {
            None
        }
    }

    fn asking(&self) -> Option<&ServerView> {
        let id = self.confirm.as_deref()?;
        self.servers
            .iter()
            .find(|server| server.id == id && server.status == ServerStatus::WaitingForApproval)
    }

    fn chosen(&self) -> Option<(String, String, bool)> {
        let (id, name, scope) = match self.item()? {
            Item::Skill(index) => {
                let skill = &self.skills[index];
                (&skill.id, &skill.name, &skill.scope)
            }
            Item::Server(index) => {
                let server = &self.servers[index];
                (&server.id, &server.name, &server.scope)
            }
            Item::Hook(index) => {
                let hook = &self.hooks[index];
                (&hook.id, &hook.name, &hook.scope)
            }
        };
        Some((id.clone(), name.clone(), *scope == Scope::Global))
    }

    fn removed(&self) -> Option<(&str, bool)> {
        self.local(self.removing.as_deref()?)
            .map(|(name, skill, _)| (name, skill))
    }

    fn local(&self, id: &str) -> Option<(&str, bool, u64)> {
        let local = |scope: &Scope| *scope != Scope::Global;
        self.skills
            .iter()
            .find(|skill| skill.id == id && local(&skill.scope))
            .map(|skill| (skill.name.as_str(), true, skill.version))
            .or_else(|| {
                self.servers
                    .iter()
                    .find(|server| server.id == id && local(&server.scope))
                    .map(|server| (server.name.as_str(), false, server.version))
            })
    }

    fn clamp(&mut self) {
        self.cursor = self.cursor.min(self.count().saturating_sub(1));
        if self.confirm.is_some() && self.asking().is_none() {
            self.confirm = None;
        }
        if self.removing.is_some() && self.removed().is_none() {
            self.removing = None;
        }
    }

    /// Handles one key: move, switch the chosen item on or off for this project,
    /// approve a server after showing exactly what it runs, or edit or remove
    /// one of this project's own items.
    /// Takes the key event. Returns what the host should do next; Enter on an
    /// item that needs no approval is left to the host.
    pub fn key(&mut self, key: KeyEvent) -> PluginsReply {
        if key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SHIFT)
        {
            return PluginsReply::Ignored;
        }
        if let Some(id) = self.removing.clone() {
            return match key.code {
                KeyCode::Char('y') => {
                    self.removing = None;
                    self.remove(&id)
                }
                KeyCode::Char('n') | KeyCode::Esc => {
                    self.removing = None;
                    PluginsReply::Handled
                }
                _ => PluginsReply::Handled,
            };
        }
        if let Some(id) = self.confirm.clone() {
            return match key.code {
                KeyCode::Char('y') if self.read_whole_command() => {
                    self.confirm = None;
                    self.approve(&id)
                }
                KeyCode::Down => {
                    self.command_top
                        .set(self.command_top.get().saturating_add(1));
                    PluginsReply::Handled
                }
                KeyCode::Up => {
                    self.command_top
                        .set(self.command_top.get().saturating_sub(1));
                    PluginsReply::Handled
                }
                KeyCode::Char('n') | KeyCode::Esc => {
                    self.confirm = None;
                    PluginsReply::Handled
                }
                _ => PluginsReply::Handled,
            };
        }
        let reply = match key.code {
            KeyCode::Up => {
                self.cursor = self.cursor.saturating_sub(1);
                PluginsReply::Handled
            }
            KeyCode::Down => {
                self.cursor = self.cursor.saturating_add(1);
                PluginsReply::Handled
            }
            KeyCode::Char(' ') => self.toggle(),
            KeyCode::Char('a') => PluginsReply::AddSkill,
            KeyCode::Char('m') => PluginsReply::AddServer,
            KeyCode::Char('h') => PluginsReply::AddHooks,
            KeyCode::Char('d') => self.ask_remove(),
            KeyCode::Char('e') => self.edit(),
            KeyCode::Char('l') => PluginsReply::Tools {
                servers: self
                    .servers
                    .iter()
                    .map(|server| (server.id.clone(), server.name.clone()))
                    .collect(),
                at: match self.item() {
                    Some(Item::Server(index)) => index + 1,
                    _ => 0,
                },
            },
            KeyCode::Enter => match self.item() {
                Some(Item::Skill(index)) => PluginsReply::OpenSkill {
                    id: self.skills[index].id.clone(),
                    name: self.skills[index].name.clone(),
                },
                Some(Item::Server(index))
                    if self.servers[index].status == ServerStatus::WaitingForApproval =>
                {
                    self.confirm = Some(self.servers[index].id.clone());
                    self.command_top.set(0);
                    *self.read_through.borrow_mut() = None;
                    PluginsReply::Handled
                }
                _ => PluginsReply::Ignored,
            },
            _ => PluginsReply::Ignored,
        };
        self.clamp();
        reply
    }

    fn still_saving(&mut self) -> bool {
        if self.changing.is_some() {
            self.notice = Some(Notice::problem(
                "Still saving the last change. Try again in a moment.",
            ));
        }
        self.changing.is_some()
    }

    fn toggle(&mut self) -> PluginsReply {
        let Some(item) = self.item() else {
            return PluginsReply::Handled;
        };
        if self.still_saving() {
            return PluginsReply::Handled;
        }
        let (id, name, enabled, on_here, version) = match item {
            Item::Skill(index) => {
                let skill = &self.skills[index];
                (
                    skill.id.clone(),
                    skill.name.clone(),
                    skill.enabled,
                    skill.on_here,
                    skill.version,
                )
            }
            Item::Server(index) => {
                let server = &self.servers[index];
                (
                    server.id.clone(),
                    server.name.clone(),
                    server.enabled,
                    server.on_here,
                    server.version,
                )
            }
            Item::Hook(index) => {
                let hook = &self.hooks[index];
                (
                    hook.id.clone(),
                    hook.name.clone(),
                    hook.enabled,
                    hook.on_here,
                    hook.version,
                )
            }
        };
        let (change, done) = match (on_here, item) {
            (Some(_), _) if !enabled => {
                self.notice = Some(Notice::plain(format!(
                    "{name} is off in Settings for every project. Turn it on there first."
                )));
                return PluginsReply::Handled;
            }
            (Some(on), _) => (
                PluginChange::Switch {
                    project: self.project.clone(),
                    id,
                    on: !on,
                    version,
                },
                if on {
                    format!("{name} is off for this project.")
                } else {
                    format!("{name} is on for this project.")
                },
            ),
            (None, Item::Skill(_)) => (
                PluginChange::SetSkill {
                    id,
                    enabled: !enabled,
                    version,
                },
                format!("{name} is {}.", if enabled { "off" } else { "on" }),
            ),
            (None, Item::Server(_)) => (
                PluginChange::SetServer {
                    id,
                    enabled: !enabled,
                    version,
                },
                format!("{name} is {}.", if enabled { "off" } else { "on" }),
            ),
            (None, Item::Hook(_)) => (
                PluginChange::SetHook {
                    id,
                    enabled: !enabled,
                },
                format!("{name} is {}.", if enabled { "off" } else { "on" }),
            ),
        };
        self.changing = Some(done);
        PluginsReply::Send(SettingsJob::Plugin(change))
    }

    fn ask_remove(&mut self) -> PluginsReply {
        let Some((id, name, global)) = self.chosen() else {
            return PluginsReply::Ignored;
        };
        if matches!(self.item(), Some(Item::Hook(_))) {
            self.notice = Some(Notice::plain(format!(
                "{name} is a hook. Press Space to turn it off."
            )));
        } else if global {
            self.notice = Some(Notice::plain(format!(
                "{name} is for every project. Switch it off here, or remove it in Settings."
            )));
        } else {
            self.removing = Some(id);
        }
        PluginsReply::Handled
    }

    fn remove(&mut self, id: &str) -> PluginsReply {
        if self.still_saving() {
            return PluginsReply::Handled;
        }
        let Some((name, skill, version)) = self
            .local(id)
            .map(|(name, skill, version)| (name.to_owned(), skill, version))
        else {
            return PluginsReply::Handled;
        };
        let id = id.to_owned();
        let (change, done) = if skill {
            (
                PluginChange::RemoveSkill { id, version },
                format!("Removed {name}."),
            )
        } else {
            (
                PluginChange::RemoveServer { id, version },
                format!("Removed {name} and its secrets."),
            )
        };
        self.changing = Some(done);
        PluginsReply::Send(SettingsJob::Plugin(change))
    }

    fn edit(&mut self) -> PluginsReply {
        let Some(Item::Server(index)) = self.item() else {
            return PluginsReply::Ignored;
        };
        let server = &self.servers[index];
        if server.scope == Scope::Global {
            self.notice = Some(Notice::plain(format!(
                "{} is for every project. Change it in Settings, Plugins and skills.",
                server.name
            )));
            return PluginsReply::Handled;
        }
        PluginsReply::Edit(Box::new(server.clone()))
    }

    fn read_whole_command(&self) -> bool {
        self.asking().is_some_and(|server| {
            matches!(
                &*self.read_through.borrow(),
                Some((id, runs)) if *id == server.id && *runs == server.runs
            )
        })
    }

    fn approve(&mut self, id: &str) -> PluginsReply {
        if self.still_saving() {
            return PluginsReply::Handled;
        }
        let Some(server) = self.servers.iter().find(|server| server.id == id) else {
            return PluginsReply::Handled;
        };
        let change = PluginChange::Approve {
            id: server.id.clone(),
            spec: server.spec.clone(),
        };
        self.changing = Some(format!("Approved. Agents can use {} now.", server.name));
        PluginsReply::Send(SettingsJob::Plugin(change))
    }

    /// Draws the Skills, MCP servers and Hooks sections of the project's Settings tab; a short window
    /// cuts the details panel first, so the notice and any question always show. An
    /// approval question takes the whole section and never more than `room` rows.
    /// Takes the content width, the rows available and the look.
    /// Returns the rows and the help keys.
    pub fn rows(&self, content: usize, room: usize, look: &Look) -> (Vec<Row>, Help) {
        let wide = content >= WIDE;
        let mut lines = vec![self.heading("Skills", self.skills.len(), content, look)];
        if !self.loaded {
            lines.push(Row::of("  Loading...", look.muted));
            lines.extend(self.notice.iter().map(|notice| notice.row(look)));
            return (lines, self.help());
        }
        if let Some(server) = self.asking() {
            return (self.approval(server, content, room, look), self.help());
        }
        let footer = self.footer(look);
        let floor = 6 + footer.len();
        let spaced = room > 9 + footer.len();
        let reserve = if spaced { 9 } else { 6 };
        let mut panel = self.panel(content, look);
        panel.truncate(room.saturating_sub(reserve + footer.len()));
        let fixed = reserve + panel.len() + footer.len();
        let free = room.saturating_sub(fixed).max(3);
        let hook_rows = self.hooks.len().max(1).min((free / 3).max(1));
        let free = free.saturating_sub(hook_rows).max(2);
        let wanted_servers = self.servers.len().max(1).min(free / 2);
        let skill_rows = self
            .skills
            .len()
            .max(1)
            .min(free.saturating_sub(wanted_servers))
            .max(1);
        let server_rows = free.saturating_sub(skill_rows).max(1);
        let widths = if wide {
            vec![
                NAME_WIDTH,
                STATE_WIDTH,
                WHERE_WIDTH,
                content.saturating_sub(2 + NAME_WIDTH + STATE_WIDTH + WHERE_WIDTH + 6),
            ]
        } else {
            vec![
                NAME_WIDTH,
                STATE_WIDTH,
                content.saturating_sub(2 + NAME_WIDTH + STATE_WIDTH + 4),
            ]
        };
        let selected = self.item();

        if self.skills.is_empty() {
            lines.push(Row::of(
                "  No skills yet. Press a to add one for this project.",
                look.muted,
            ));
        }
        let picked = match selected {
            Some(Item::Skill(index)) => index,
            _ => 0,
        };
        let first = window(picked, self.skills.len(), skill_rows);
        for (index, skill) in self.skills.iter().enumerate().skip(first).take(skill_rows) {
            let mut cells = vec![
                Row::of(skill.name.clone(), look.accent),
                state(skill.enabled, skill.on_here, None, look),
                Row::of(place(&skill.scope), look.fg),
            ];
            if wide {
                cells.push(Row::of(skill.description.clone(), look.muted));
            }
            lines.push(list_row(
                columns(&widths, cells),
                content,
                selected == Some(Item::Skill(index)),
                true,
                look,
            ));
        }
        if spaced {
            lines.push(Row::default());
        }

        lines.push(self.heading("MCP servers", self.servers.len(), content, look));
        if self.servers.is_empty() {
            lines.push(Row::of(
                "  No MCP servers yet. Press m to add one for this project.",
                look.muted,
            ));
        }
        let picked = match selected {
            Some(Item::Server(index)) => index,
            _ => 0,
        };
        let first = window(picked, self.servers.len(), server_rows);
        for (index, server) in self
            .servers
            .iter()
            .enumerate()
            .skip(first)
            .take(server_rows)
        {
            let mut cells = vec![
                Row::of(server.name.clone(), look.fg),
                state(server.enabled, server.on_here, Some(server.status), look),
                Row::of(place(&server.scope), look.fg),
            ];
            if wide {
                cells.push(Row::of(short_runs(server), look.muted));
            }
            lines.push(list_row(
                columns(&widths, cells),
                content,
                selected == Some(Item::Server(index)),
                true,
                look,
            ));
        }
        if spaced {
            lines.push(Row::default());
        }

        lines.push(self.heading("Hooks", self.hooks.len(), content, look));
        if self.hooks.is_empty() {
            lines.push(Row::of(
                "  No hooks yet. Press h to add a plugin folder's hooks for this project.",
                look.muted,
            ));
        }
        let picked = match selected {
            Some(Item::Hook(index)) => index,
            _ => 0,
        };
        let first = window(picked, self.hooks.len(), hook_rows);
        for (index, hook) in self.hooks.iter().enumerate().skip(first).take(hook_rows) {
            let mut cells = vec![
                Row::of(hook.plugin.clone(), look.fg),
                state(hook.enabled, hook.on_here, None, look),
                Row::of(place(&hook.scope), look.fg),
            ];
            if wide {
                cells.push(Row::of(hook_runs(hook), look.muted));
            }
            lines.push(list_row(
                columns(&widths, cells),
                content,
                selected == Some(Item::Hook(index)),
                true,
                look,
            ));
        }
        if spaced {
            lines.push(Row::default());
        }
        lines.extend(panel);
        if !footer.is_empty() {
            lines.truncate(room.saturating_sub(footer.len()));
        }
        lines.extend(footer);
        assert!(lines.len() <= room.max(floor));
        (lines, self.help())
    }

    fn heading(&self, name: &str, count: usize, content: usize, look: &Look) -> Row {
        let left = Row::of(name, look.heading).push(format!("  {count}"), look.muted);
        if name == "Skills" {
            spread(
                content,
                left,
                Row::of("From Settings, plus this project", look.muted),
            )
        } else {
            left
        }
    }

    fn panel(&self, content: usize, look: &Look) -> Vec<Row> {
        let entry = |label: &str, value: String, style: Style| {
            Row::of(label, look.muted).pad(10).push(value, style)
        };
        let mut rows = Vec::new();
        match self.item() {
            None => {}
            Some(Item::Skill(index)) => {
                let skill = &self.skills[index];
                rows.push(spread(
                    content,
                    Row::of(skill.name.clone(), look.heading),
                    Row::of(skill.scope.label(), look.muted),
                ));
                rows.extend(
                    wrap_words(skill.description.trim(), content)
                        .into_iter()
                        .take(PANEL_TEXT_LINES)
                        .map(|line| Row::of(line, look.fg)),
                );
            }
            Some(Item::Server(index)) => {
                let server = &self.servers[index];
                rows.push(spread(
                    content,
                    Row::of(server.name.clone(), look.heading),
                    Row::of(server.scope.label(), look.muted),
                ));
                rows.push(entry("Runs", short_runs(server), look.fg));
                match &server.problem {
                    Some(problem) => rows.extend(
                        wrap_words(problem, content)
                            .into_iter()
                            .take(PANEL_TEXT_LINES)
                            .map(|line| Row::of(line, look.error)),
                    ),
                    None if !server.tools.is_empty() => rows.push(entry(
                        "Tools",
                        format!("{} tools: {}", server.tools.len(), server.tools.join(", ")),
                        look.fg,
                    )),
                    None => {}
                }
            }
            Some(Item::Hook(index)) => {
                let hook = &self.hooks[index];
                rows.push(spread(
                    content,
                    Row::of(hook.name.clone(), look.heading),
                    Row::of(hook.scope.label(), look.muted),
                ));
                rows.push(entry("Runs", hook_runs(hook), look.fg));
                if let Some(follows) = &hook.follows {
                    rows.push(entry(
                        "Follows",
                        format!("Runs while {follows} is on."),
                        look.fg,
                    ));
                }
            }
        }
        rows
    }

    /// The fewest body rows the plugins section needs: a heading, a row of content and
    /// its footer. The footer is the two-row remove question while one is open, otherwise
    /// the one-row notice, otherwise nothing. Callers reserve this many rows so the footer
    /// stays on screen.
    pub fn floor(&self) -> usize {
        6 + self.footer_len()
    }

    fn footer_len(&self) -> usize {
        if self.removed().is_some() {
            2
        } else {
            usize::from(self.notice.is_some())
        }
    }

    fn footer(&self, look: &Look) -> Vec<Row> {
        if let Some((name, skill)) = self.removed() {
            return vec![
                Row::of(format!("Remove {name}?"), look.warning)
                    .push("   y", look.heading)
                    .push(" Remove   ", look.fg)
                    .push("n", look.heading)
                    .push(" Keep", look.fg),
                Row::of(
                    if skill {
                        "Agents in this project stop getting it. Your folder stays."
                    } else {
                        "Agents in this project lose its tools. Its saved secrets are deleted."
                    },
                    look.muted,
                ),
            ];
        }
        self.notice.iter().map(|notice| notice.row(look)).collect()
    }

    /// Draws the approval question: the server, the question and as many lines
    /// of exactly what it runs as fit from the first line Up and Down chose, with
    /// a line marker when some do not fit. Records the server and command once
    /// its last line has shown, which `y` needs before it approves.
    /// Takes the server asked about, the content width, the rows and the look.
    /// Returns at most `room` rows.
    fn approval(&self, server: &ServerView, content: usize, room: usize, look: &Look) -> Vec<Row> {
        let mut rows = vec![
            spread(
                content,
                Row::of(server.name.clone(), look.heading),
                Row::of(server.scope.label(), look.muted),
            ),
            Row::of(format!("Approve {}?", server.name), look.warning)
                .push("   y", look.heading)
                .push(" Approve   ", look.fg)
                .push("n", look.heading)
                .push(" Not now", look.fg),
            Row::of(
                "Agents can call its tools only after you approve. It runs:",
                look.muted,
            ),
        ];
        let command = exact_lines(&server.runs, content.saturating_sub(2));
        let fits = rows.len() + command.len() <= room;
        let shown = if fits {
            command.len()
        } else {
            room.saturating_sub(rows.len() + 1)
        };
        assert!(shown <= command.len());
        let top = self.command_top.get().min(command.len() - shown);
        self.command_top.set(top);
        let end = top + shown;
        rows.extend(
            command[top..end]
                .iter()
                .map(|line| Row::of("  ", Style::new()).push(line.clone(), look.fg)),
        );
        if shown > 0 && end == command.len() {
            *self.read_through.borrow_mut() = Some((server.id.clone(), server.runs.clone()));
        }
        if !fits {
            let marker = match shown {
                0 => "  Make the window taller to read what it runs.".to_owned(),
                _ if end == command.len() => format!(
                    "  Lines {}-{end} of {}, the end. Up reads back.",
                    top + 1,
                    command.len()
                ),
                _ => format!(
                    "  Lines {}-{end} of {}. Down reads on; y waits for the end.",
                    top + 1,
                    command.len()
                ),
            };
            rows.push(Row::of(marker, look.warning));
        }
        rows.truncate(room);
        rows
    }

    fn help(&self) -> Help {
        if self.confirm.is_some() {
            return vec![
                ("\u{2191}\u{2193}", "Read"),
                ("y", "Approve"),
                ("n", "Not now"),
            ];
        }
        if self.removing.is_some() {
            return vec![("y", "Remove"), ("n", "Keep")];
        }
        let mut help = vec![("\u{2191}\u{2193}", "Move"), ("Space", "On or off here")];
        if let Some(Item::Skill(_)) = self.item() {
            help.push(("Enter", "Details"));
        }
        if let Some(Item::Server(index)) = self.item()
            && self.servers[index].status == ServerStatus::WaitingForApproval
        {
            help.push(("Enter", "Approve"));
        }
        if let Some((_, _, false)) = self.chosen()
            && !matches!(self.item(), Some(Item::Hook(_)))
        {
            if matches!(self.item(), Some(Item::Server(_))) {
                help.push(("e", "Edit"));
            }
            help.push(("d", "Remove"));
        }
        help.extend([
            ("a", "Add skill"),
            ("m", "Add MCP server"),
            ("h", "Add hooks"),
            ("l", "Tools"),
            TABS,
            ("Esc", "Home"),
        ]);
        help
    }
}

fn visible(scope: &Scope, project: &ProjectId) -> bool {
    match scope {
        Scope::Global => true,
        Scope::Project { project_id } => project_id == project,
    }
}

fn place(scope: &Scope) -> &'static str {
    match scope {
        Scope::Global => "Every project",
        Scope::Project { .. } => "This project only",
    }
}

fn state(enabled: bool, on_here: Option<bool>, status: Option<ServerStatus>, look: &Look) -> Row {
    let off = |word: &str| Row::of(format!("{} {word}", look.glyph("\u{2298}")), look.muted);
    match on_here {
        Some(_) if !enabled => return off("Off in Settings"),
        Some(false) => return off("Off here"),
        None if !enabled => return off("Off"),
        _ => {}
    }
    match status {
        Some(ServerStatus::WaitingForApproval) => Row::of("! Needs approval", look.warning),
        Some(ServerStatus::CantStart) => Row::of(
            format!("{} Can't start", look.glyph("\u{2717}")),
            look.error,
        ),
        Some(ServerStatus::Off) => off("Off"),
        Some(ServerStatus::Approved) | None => {
            Row::of(format!("{} On", look.glyph("\u{2713}")), look.success)
        }
    }
}

/// Splits text into lines of at most a width without dropping or merging any
/// character, so a command shown for approval reads exactly as it will run.
/// Takes the text and the width in cells. Returns at least one line.
pub(in crate::tui) fn exact_lines(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut lines = vec![String::new()];
    let mut used = 0;
    for character in text.chars() {
        let size = char_cells(character);
        if used + size > width && used > 0 {
            lines.push(String::new());
            used = 0;
        }
        let last = lines.len() - 1;
        lines[last].push(character);
        used += size;
    }
    assert_eq!(lines.concat(), text);
    lines
}

#[cfg(all(test, unix))]
mod tests;
