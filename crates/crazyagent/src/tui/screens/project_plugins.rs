use super::new_project_input::char_cells;
use super::new_project_view::{Look, Row, columns, list_row, spread, wrap_words};
use super::project_team::{Help, Notice, TABS, window};
use super::settings_plugins::{plugin_plain, short_runs};
use crate::plugins::{Scope, ServerStatus, ServerView, SkillView};
use crate::protocol::ProjectId;
use crate::tui::client::settings::{PluginChange, SettingsEvent, SettingsJob};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::Style;

const WIDE: usize = 96;
const NAME_WIDTH: usize = 22;
const STATE_WIDTH: usize = 18;
const WHERE_WIDTH: usize = 17;
const PANEL_TEXT_LINES: usize = 2;

/// What the project plugins view asks its host to do after a key.
/// Ignored lets the host's own keys run; Send is one Settings job for the host
/// to queue, whose reply comes back through `apply`; AddSkill, AddServer and
/// Edit ask the host to open the add form or the server's edit page for this
/// project.
#[derive(Debug)]
pub(in crate::tui) enum PluginsReply {
    Ignored,
    Handled,
    Send(SettingsJob),
    AddSkill,
    AddServer,
    Edit(Box<ServerView>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Item {
    Skill(usize),
    Server(usize),
}

/// The skills and MCP servers one project's agents get: every-project items
/// with whether they are on here, and this project's own items.
/// Takes engine reads through `apply` and keys through `key`; never invents data.
pub(in crate::tui) struct ProjectPlugins {
    pub project: ProjectId,
    pub skills: Vec<SkillView>,
    pub servers: Vec<ServerView>,
    pub loaded: bool,
    pub cursor: usize,
    pub confirm: Option<String>,
    pub removing: Option<String>,
    pub notice: Option<Notice>,
    waiting: bool,
    stale: bool,
    changing: Option<String>,
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
            loaded: false,
            cursor: 0,
            confirm: None,
            removing: None,
            notice: None,
            waiting: false,
            stale: true,
            changing: None,
        }
    }

    /// Gives the next read: one at the start and one after every change, never
    /// two at once.
    /// Takes the view. Returns `SettingsJob::ProjectPlugins`, or none.
    pub fn next_job(&mut self) -> Option<SettingsJob> {
        if self.waiting || !self.stale {
            return None;
        }
        self.waiting = true;
        self.stale = false;
        Some(SettingsJob::ProjectPlugins {
            project: self.project.clone(),
        })
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
                        self.skills = skills.clone();
                        self.servers = servers.clone();
                        self.loaded = true;
                    }
                    Err(code) => {
                        self.notice = Some(Notice::problem(format!(
                            "Could not read this project's plugins. {}",
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
        self.skills.len() + self.servers.len()
    }

    fn item(&self) -> Option<Item> {
        if self.cursor < self.skills.len() {
            Some(Item::Skill(self.cursor))
        } else if self.cursor < self.count() {
            Some(Item::Server(self.cursor - self.skills.len()))
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
        };
        Some((id.clone(), name.clone(), *scope == Scope::Global))
    }

    fn removed(&self) -> Option<(&str, bool)> {
        self.local(self.removing.as_deref()?)
    }

    fn local(&self, id: &str) -> Option<(&str, bool)> {
        let local = |scope: &Scope| *scope != Scope::Global;
        self.skills
            .iter()
            .find(|skill| skill.id == id && local(&skill.scope))
            .map(|skill| (skill.name.as_str(), true))
            .or_else(|| {
                self.servers
                    .iter()
                    .find(|server| server.id == id && local(&server.scope))
                    .map(|server| (server.name.as_str(), false))
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
                KeyCode::Char('y') => {
                    self.confirm = None;
                    self.approve(&id)
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
            KeyCode::Char('d') => self.ask_remove(),
            KeyCode::Char('e') => self.edit(),
            KeyCode::Enter => match self.item() {
                Some(Item::Server(index))
                    if self.servers[index].status == ServerStatus::WaitingForApproval =>
                {
                    self.confirm = Some(self.servers[index].id.clone());
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
        let (id, name, enabled, on_here) = match item {
            Item::Skill(index) => {
                let skill = &self.skills[index];
                (
                    skill.id.clone(),
                    skill.name.clone(),
                    skill.enabled,
                    skill.on_here,
                )
            }
            Item::Server(index) => {
                let server = &self.servers[index];
                (
                    server.id.clone(),
                    server.name.clone(),
                    server.enabled,
                    server.on_here,
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
                },
                format!("{name} is {}.", if enabled { "off" } else { "on" }),
            ),
            (None, Item::Server(_)) => (
                PluginChange::SetServer {
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
            return PluginsReply::Handled;
        };
        if global {
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
        let Some((name, skill)) = self.local(id).map(|(name, skill)| (name.to_owned(), skill))
        else {
            return PluginsReply::Handled;
        };
        let id = id.to_owned();
        let (change, done) = if skill {
            (PluginChange::RemoveSkill { id }, format!("Removed {name}."))
        } else {
            (
                PluginChange::RemoveServer { id },
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

    /// Draws the Skills and MCP servers sections of the project's Settings tab.
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
        let panel = self.panel(content, look);
        let footer = self.footer(content, look);
        let fixed = 6 + panel.len() + footer.len();
        let free = room.saturating_sub(fixed).max(2);
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
                Row::of(skill.name.clone(), look.fg),
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
        lines.push(Row::default());

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
        lines.push(Row::default());
        lines.extend(panel);
        lines.extend(footer);
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
                if self.confirm.is_some() {
                    return rows;
                }
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
        }
        rows
    }

    fn footer(&self, content: usize, look: &Look) -> Vec<Row> {
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
        let Some(server) = self.asking() else {
            return self.notice.iter().map(|notice| notice.row(look)).collect();
        };
        let mut rows = vec![
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
        rows.extend(
            exact_lines(&server.runs, content.saturating_sub(2))
                .into_iter()
                .map(|line| Row::of("  ", Style::new()).push(line, look.fg)),
        );
        rows
    }

    fn help(&self) -> Help {
        if self.confirm.is_some() {
            return vec![("y", "Approve"), ("n", "Not now")];
        }
        if self.removing.is_some() {
            return vec![("y", "Remove"), ("n", "Keep")];
        }
        let mut help = vec![("\u{2191}\u{2193}", "Move"), ("Space", "On or off here")];
        if let Some(Item::Server(index)) = self.item()
            && self.servers[index].status == ServerStatus::WaitingForApproval
        {
            help.push(("Enter", "Approve"));
        }
        if let Some((_, _, false)) = self.chosen() {
            if matches!(self.item(), Some(Item::Server(_))) {
                help.push(("e", "Edit"));
            }
            help.push(("d", "Remove"));
        }
        help.extend([
            ("a", "Add skill"),
            ("m", "Add MCP server"),
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
