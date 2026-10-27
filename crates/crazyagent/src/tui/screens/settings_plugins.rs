use super::settings::{draw_page, step};
use super::settings_accounts::{plain, send, settings_event};
use super::{Flow, Place, ProjectTab, Screen, SettingsSection};
use crate::engine::owner::AccountSecret;
use crate::plugins::{
    ARG_BYTES_MAX, ARGS_MAX, COMMAND_BYTES_MAX, SECRET_NAME_BYTES_MAX, SECRETS_PER_SERVER_MAX,
    SERVER_NAME_BYTES_MAX, Scope, ServerSpec, ServerStatus, ServerView, SkillView, URL_BYTES_MAX,
    validate_secret_name, validate_server_name,
};
use crate::protocol::ProjectId;
use crate::tui::client::settings::{PluginChange, SettingsEvent, SettingsJob};
use crate::tui::ctx::{Ctx, Load};
use crate::tui::frame::{Hint, Message, Modal, draw_lines, hint, sections_hint};
use crate::tui::keys::Key;
use crate::tui::seam::Arrival;
use crate::tui::text::{
    cells, cut, indent, join, list_row, pad, row, status, styled, thumb, with_bar, wrap,
};
use crate::tui::theme::Role;
use crate::tui::widgets::{TextField, buttons, radio};
use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::text::{Line, Span};

const COMMAND_HINT: &str = if cfg!(target_os = "windows") {
    "Use the full path of the command, such as C:\\Users\\you\\.local\\bin\\uvx.exe."
} else if cfg!(target_os = "linux") {
    "Use the full path of the command, such as /usr/local/bin/uvx."
} else {
    "Use the full path of the command, such as /opt/homebrew/bin/uvx."
};

const NAME_WIDTH: usize = 22;
const SKILL_STATE_WIDTH: usize = 5;
const SERVER_STATE_WIDTH: usize = 22;
const LABEL_WIDTH: usize = 11;
const MODAL_WIDTH: u16 = 62;
const FIELD_WIDTH: usize = 64;
const SECRET_NAME_BOX: usize = 17;
const SECRET_VALUE_CHARS_MAX: usize = 4096;
const RUNS_CHARS_MAX: usize = COMMAND_BYTES_MAX + ARGS_MAX * (ARG_BYTES_MAX * 4 + 3);
const FOLDER_CHARS_MAX: usize = 1024;
const SUBTITLE: &str = "Skills teach agents how to do a task. MCP servers give agents new tools. A project can add its own in its settings.";

/// The design's status word for an MCP server.
/// Input: the engine status. Output: "Needs approval", "Approved", "Turned off" or "Can't start".
pub fn server_word(status: ServerStatus) -> &'static str {
    match status {
        ServerStatus::WaitingForApproval => "Needs approval",
        ServerStatus::Approved => "Approved",
        ServerStatus::Off => "Turned off",
        ServerStatus::CantStart => "Can't start",
    }
}

/// What a server runs, as the list shows it: the command without its folder,
/// or the URL.
/// Input: the server. Output: the short line; details show the whole line.
pub fn short_runs(server: &ServerView) -> String {
    match &server.spec {
        ServerSpec::Stdio { command, .. } => match command.rfind('/') {
            Some(slash) => server
                .runs
                .strip_prefix(&command[..=slash])
                .unwrap_or(server.runs.as_str())
                .to_owned(),
            None => server.runs.clone(),
        },
        ServerSpec::Http { .. } => server.runs.clone(),
    }
}

/// A path with the home folder written as `~`.
/// Inputs: the path and the context. Output: the shorter path.
pub fn tilde(path: &str, ctx: &Ctx) -> String {
    match ctx.home.as_deref() {
        Some(home) if !home.is_empty() && path.starts_with(&format!("{home}/")) => {
            format!("~{}", &path[home.len()..])
        }
        _ => path.to_owned(),
    }
}

/// A plain sentence for an engine problem code about skills and MCP servers.
/// Input: the code. Output: the sentence.
pub fn plugin_plain(code: &str) -> &'static str {
    match code {
        "skill_folder_not_found" => "That folder does not exist.",
        "skill_not_a_folder" => "That is a file. Choose the folder that holds SKILL.md.",
        "skill_missing_skill_md" => "That folder has no SKILL.md.",
        "skill_frontmatter_missing" => "SKILL.md needs a name and a description at the top.",
        "skill_name_invalid" | "skill_description_invalid" => {
            "SKILL.md has a name or description CrazyAgent cannot use."
        }
        "skill_name_taken" | "server_name_taken" => "Another one already uses this name.",
        "skill_too_large" | "skill_too_many_files" | "skill_too_deep" => {
            "This skill folder is too large to copy."
        }
        "skill_link_outside" | "skill_file_unsupported" | "skill_unreadable" => {
            "The folder has links or files CrazyAgent cannot copy."
        }
        "skill_limit" | "server_limit" | "server_secret_limit" => {
            "You reached the limit. Remove one first."
        }
        "server_command_invalid" => COMMAND_HINT,
        "server_args_invalid" => "An argument is too long or has control characters.",
        "server_url_invalid" => "Use an https address. Plain http works only for this Mac.",
        "server_name_invalid" => {
            "Use 1 to 64 lowercase letters, digits, - or _, starting with a letter or digit."
        }
        "server_secret_name_invalid" => {
            "Secret names use letters, digits, _ or -, and start with a letter or _."
        }
        "server_secret_invalid" => "This secret cannot be saved. Paste it on one line.",
        "server_secret_not_found" | "plugin_not_found" => "It was removed already.",
        "server_command_not_found" | "server_command_not_runnable" => {
            "That command is not on this Mac or cannot run."
        }
        "server_cant_start"
        | "server_exited"
        | "server_refused"
        | "server_reply_invalid"
        | "server_output_too_large" => {
            "The server did not start or did not answer as an MCP server."
        }
        "server_check_timeout" => "The server took too long to answer.",
        "server_check_busy" => "A check is already running.",
        "server_check_command_only" => "Only command servers can be checked from here.",
        "plugin_secret_store_unavailable"
        | "plugin_store_unavailable"
        | "plugin_store_unsafe"
        | "skill_store_unavailable" => "The engine cannot save plugins right now.",
        other => crate::plugins::plain_problem(other).unwrap_or_else(|| plain(other)),
    }
}

/// Splits "command and arguments" as typed into a server spec.
/// Input: the typed line. Output: the spec, or the plain reason it cannot be used.
pub fn command_spec(line: &str) -> Result<ServerSpec, &'static str> {
    let mut words = words(line)?.into_iter();
    let command = words
        .next()
        .ok_or("Type the command that starts the server.")?;
    if !std::path::Path::new(&command).is_absolute() {
        return Err(COMMAND_HINT);
    }
    let spec = ServerSpec::Stdio {
        command,
        args: words.collect(),
    };
    spec.validate().map_err(|error| plugin_plain(&error.0))?;
    Ok(spec)
}

/// The server spec typed into a command or URL field.
/// Inputs: whether the field holds a URL, and the typed text. Output: the
/// spec, or the plain reason it cannot be used.
pub fn target_spec(url: bool, typed: &str) -> Result<ServerSpec, &'static str> {
    if !url {
        return command_spec(typed);
    }
    let spec = ServerSpec::Http {
        url: typed.trim().to_owned(),
    };
    spec.validate()
        .map(|()| spec)
        .map_err(|error| plugin_plain(&error.0))
}

/// Splits a typed command line into words. Single quotes keep spaces and a
/// backslash keeps the next character, so a server's `runs` line reads back
/// as the same command.
/// Input: the line. Output: the words, or the plain reason it cannot be read.
pub fn words(line: &str) -> Result<Vec<String>, &'static str> {
    let mut words = Vec::new();
    let mut word: Option<String> = None;
    let mut characters = line.chars();
    while let Some(character) = characters.next() {
        match character {
            '\'' => {
                let text = word.get_or_insert_with(String::new);
                loop {
                    match characters.next() {
                        Some('\'') => break,
                        Some(inner) => text.push(inner),
                        None => return Err("A quote is not closed."),
                    }
                }
            }
            '\\' => {
                let kept = characters.next().ok_or("A backslash ends the line.")?;
                word.get_or_insert_with(String::new).push(kept);
            }
            space if space.is_whitespace() => words.extend(word.take()),
            other => word.get_or_insert_with(String::new).push(other),
        }
    }
    words.extend(word);
    assert!(words.len() <= line.len());
    Ok(words)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Picked {
    Skill(usize),
    Server(usize),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Action {
    Read,
    Approve,
    NotNow,
    Toggle,
    Check,
    Edit,
    Remove,
    Close,
}

enum Open {
    None,
    Details { focus: usize },
    Remove { remove_focused: bool },
}

/// Settings, Plugins and skills: skills and MCP servers for every project, with
/// On or off, details, approval and remove.
/// Input: keys and the engine's skill and server lists. Output: both lists.
pub struct Plugins {
    skills: Vec<SkillView>,
    servers: Vec<ServerView>,
    load: Load<()>,
    waiting: bool,
    changing: Option<String>,
    selected: usize,
    skill_offset: usize,
    server_offset: usize,
    open: Open,
}

impl Default for Plugins {
    fn default() -> Self {
        Self::new()
    }
}

impl Plugins {
    /// The page with nothing loaded yet.
    /// Input: none. Output: the screen; `shown` asks the engine for both lists.
    pub fn new() -> Self {
        Self {
            skills: Vec::new(),
            servers: Vec::new(),
            load: Load::Idle,
            waiting: false,
            changing: None,
            selected: 0,
            skill_offset: 0,
            server_offset: 0,
            open: Open::None,
        }
    }

    /// The page with both lists already known.
    /// Inputs: skills and servers. Output: the screen.
    #[cfg(test)]
    pub fn with(skills: Vec<SkillView>, servers: Vec<ServerView>) -> Self {
        let mut screen = Self::new();
        screen.skills = skills;
        screen.servers = servers;
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
        send(ctx, SettingsJob::Plugins);
    }

    fn count(&self) -> usize {
        self.skills.len() + self.servers.len()
    }

    fn picked(&self) -> Option<Picked> {
        if self.selected < self.skills.len() {
            Some(Picked::Skill(self.selected))
        } else if self.selected < self.count() {
            Some(Picked::Server(self.selected - self.skills.len()))
        } else {
            None
        }
    }

    fn change(&mut self, change: PluginChange, done: String, ctx: &mut Ctx) {
        if self.changing.is_some() {
            return;
        }
        self.changing = Some(done);
        send(ctx, SettingsJob::Plugin(change));
    }

    fn actions(&self) -> Vec<Action> {
        match self.picked() {
            Some(Picked::Skill(_)) => {
                vec![Action::Read, Action::Toggle, Action::Remove, Action::Close]
            }
            Some(Picked::Server(index)) => {
                let server = &self.servers[index];
                let mut actions = Vec::new();
                if server.status == ServerStatus::WaitingForApproval {
                    actions.extend([Action::Approve, Action::NotNow, Action::Toggle]);
                    return actions;
                }
                if matches!(server.spec, ServerSpec::Stdio { .. }) {
                    actions.push(Action::Check);
                }
                actions.extend([Action::Edit, Action::Toggle, Action::Remove]);
                actions
            }
            None => Vec::new(),
        }
    }

    fn label(&self, action: Action) -> &'static str {
        let on = match self.picked() {
            Some(Picked::Skill(index)) => self.skills[index].enabled,
            Some(Picked::Server(index)) => self.servers[index].enabled,
            None => false,
        };
        match action {
            Action::Read => "Read SKILL.md",
            Action::Approve => "Approve",
            Action::NotNow => "Not now",
            Action::Toggle if on => "Turn off",
            Action::Toggle => "Turn on",
            Action::Check => "Check again",
            Action::Edit => "Edit",
            Action::Remove => "Remove",
            Action::Close => "Close",
        }
    }

    fn toggle(&mut self, ctx: &mut Ctx) {
        match self.picked() {
            Some(Picked::Skill(index)) => {
                let skill = &self.skills[index];
                let enabled = !skill.enabled;
                let done = format!("{} is {}.", skill.name, if enabled { "on" } else { "off" });
                let id = skill.id.clone();
                self.change(PluginChange::SetSkill { id, enabled }, done, ctx);
            }
            Some(Picked::Server(index)) => {
                let server = &self.servers[index];
                let enabled = !server.enabled;
                let done = format!("{} is {}.", server.name, if enabled { "on" } else { "off" });
                let id = server.id.clone();
                self.change(PluginChange::SetServer { id, enabled }, done, ctx);
            }
            None => {}
        }
    }

    fn run(&mut self, action: Action, ctx: &mut Ctx) {
        self.open = Open::None;
        match (action, self.picked()) {
            (Action::Approve, Some(Picked::Server(index))) => {
                let server = &self.servers[index];
                let done = format!("Approved. Agents can use {} now.", server.name);
                let change = PluginChange::Approve {
                    id: server.id.clone(),
                    spec: server.spec.clone(),
                };
                self.change(change, done, ctx);
            }
            (Action::Check, Some(Picked::Server(index))) => {
                let server = &self.servers[index];
                let done = format!("Checked {}.", server.name);
                let id = server.id.clone();
                self.change(PluginChange::CheckServer { id }, done, ctx);
            }
            (Action::Toggle, _) => self.toggle(ctx),
            (Action::Remove, Some(_)) => {
                self.open = Open::Remove {
                    remove_focused: false,
                }
            }
            _ => {}
        }
    }

    fn remove(&mut self, ctx: &mut Ctx) {
        match self.picked() {
            Some(Picked::Skill(index)) => {
                let skill = &self.skills[index];
                let done = format!("Removed {}.", skill.name);
                let id = skill.id.clone();
                self.change(PluginChange::RemoveSkill { id }, done, ctx);
            }
            Some(Picked::Server(index)) => {
                let server = &self.servers[index];
                let done = format!("Removed {} and its secrets.", server.name);
                let id = server.id.clone();
                self.change(PluginChange::RemoveServer { id }, done, ctx);
            }
            None => {}
        }
    }

    fn windows(&self, ctx: &Ctx, subtitle_rows: usize) -> (usize, usize) {
        let content = (ctx.height as usize).saturating_sub(4 + 1);
        let avail = content.saturating_sub(1 + subtitle_rows + 1 + 3).max(2);
        let servers_wanted = self.servers.len().max(1);
        let skills = self
            .skills
            .len()
            .max(1)
            .min(avail.saturating_sub(servers_wanted.min(avail / 2)).max(1));
        let servers = servers_wanted.min(avail.saturating_sub(skills).max(1));
        (skills, servers)
    }

    fn section(
        rows: Vec<Line<'static>>,
        total: usize,
        window: usize,
        offset: usize,
        limit: usize,
        ctx: &Ctx,
    ) -> Vec<Line<'static>> {
        if total > window {
            let (top, size) = thumb(window, offset, total);
            with_bar(rows, limit, top, size, &ctx.theme, ctx.glyphs)
        } else {
            rows
        }
    }

    fn follow(selected: Option<usize>, offset: usize, window: usize, total: usize) -> usize {
        let offset = offset.min(total.saturating_sub(window));
        match selected {
            Some(index) if index < offset => index,
            Some(index) if index >= offset + window => index + 1 - window,
            _ => offset,
        }
    }

    /// The body lines of the page.
    /// Inputs: the content width and context. Output: the lines below the tab strip.
    pub fn body(&mut self, limit: usize, ctx: &Ctx) -> Vec<Line<'static>> {
        let theme = &ctx.theme;
        let glyphs = ctx.glyphs;
        let title = || styled("Plugins and skills", Role::Accent, theme);
        match &self.load {
            Load::Failed(code) => {
                return vec![
                    title(),
                    Line::default(),
                    status(
                        &format!("Failed to load plugins. {}", plugin_plain(code)),
                        theme,
                        glyphs,
                    ),
                    Line::raw("Press r to try again."),
                ];
            }
            Load::Ready(()) => {}
            _ => {
                return vec![
                    title(),
                    Line::default(),
                    Line::from(vec![
                        Span::styled(ctx.spinner(), theme.running),
                        Span::raw(" Loading skills and MCP servers"),
                    ]),
                ];
            }
        }
        let mut lines = vec![row(
            limit,
            title(),
            styled("For every project", Role::Muted, theme),
            glyphs,
        )];
        let subtitle = wrap(SUBTITLE, limit);
        let subtitle_rows = subtitle.len();
        lines.extend(
            subtitle
                .into_iter()
                .map(|part| styled(part, Role::Muted, theme)),
        );
        lines.push(Line::default());
        let (skill_window, server_window) = self.windows(ctx, subtitle_rows);
        let picked = self.picked();
        let skill_pick = match picked {
            Some(Picked::Skill(index)) => Some(index),
            _ => None,
        };
        let server_pick = match picked {
            Some(Picked::Server(index)) => Some(index),
            _ => None,
        };
        self.skill_offset = Self::follow(
            skill_pick,
            self.skill_offset,
            skill_window,
            self.skills.len(),
        );
        self.server_offset = Self::follow(
            server_pick,
            self.server_offset,
            server_window,
            self.servers.len(),
        );
        let focused = matches!(self.open, Open::None);
        let heading = |name: &str, count: usize| {
            join([
                styled(name, Role::Heading, theme),
                styled(format!("  {count}"), Role::Muted, theme),
            ])
        };
        lines.push(heading("Skills", self.skills.len()));
        if self.skills.is_empty() {
            lines.push(styled(
                "No skills yet. Press a to add a skill folder.",
                Role::Muted,
                theme,
            ));
        } else {
            let row_limit = if self.skills.len() > skill_window {
                limit - 1
            } else {
                limit
            };
            let widths = [
                NAME_WIDTH,
                SKILL_STATE_WIDTH,
                row_limit
                    .saturating_sub(2 + NAME_WIDTH + SKILL_STATE_WIDTH + 4)
                    .max(1),
            ];
            let rows = self
                .skills
                .iter()
                .enumerate()
                .skip(self.skill_offset)
                .take(skill_window)
                .map(|(index, skill)| {
                    let values = vec![
                        Line::raw(skill.name.clone()),
                        Line::raw(if skill.enabled { "On" } else { "Off" }),
                        Line::raw(tilde(&skill.source, ctx)),
                    ];
                    list_row(
                        cells(&widths, values, 2, glyphs),
                        row_limit,
                        skill_pick == Some(index),
                        focused,
                        theme,
                        glyphs,
                    )
                })
                .collect();
            lines.extend(Self::section(
                rows,
                self.skills.len(),
                skill_window,
                self.skill_offset,
                limit,
                ctx,
            ));
        }
        lines.push(Line::default());
        lines.push(heading("MCP servers", self.servers.len()));
        if self.servers.is_empty() {
            lines.push(styled(
                "No MCP servers yet. Press m to add one.",
                Role::Muted,
                theme,
            ));
        } else {
            let row_limit = if self.servers.len() > server_window {
                limit - 1
            } else {
                limit
            };
            let widths = [
                NAME_WIDTH,
                SERVER_STATE_WIDTH,
                row_limit
                    .saturating_sub(2 + NAME_WIDTH + SERVER_STATE_WIDTH + 4)
                    .max(1),
            ];
            let rows = self
                .servers
                .iter()
                .enumerate()
                .skip(self.server_offset)
                .take(server_window)
                .map(|(index, server)| {
                    let values = vec![
                        Line::raw(server.name.clone()),
                        status(server_word(server.status), theme, glyphs),
                        Line::raw(short_runs(server)),
                    ];
                    list_row(
                        cells(&widths, values, 2, glyphs),
                        row_limit,
                        server_pick == Some(index),
                        focused,
                        theme,
                        glyphs,
                    )
                })
                .collect();
            lines.extend(Self::section(
                rows,
                self.servers.len(),
                server_window,
                self.server_offset,
                limit,
                ctx,
            ));
        }
        lines
    }

    fn details(&self, focus: usize, ctx: &Ctx) -> Option<(Modal, Vec<Hint>)> {
        let theme = &ctx.theme;
        let glyphs = ctx.glyphs;
        let room = usize::from(MODAL_WIDTH) - 4;
        let value_room = room - LABEL_WIDTH;
        let label = |name: &str, value: Line<'static>| {
            join([
                pad(styled(name, Role::Heading, theme), LABEL_WIDTH, glyphs),
                value,
            ])
        };
        let wrapped = |name: &str, text: &str, body: &mut Vec<Line<'static>>| {
            for (index, part) in wrap(text, value_room).into_iter().enumerate() {
                body.push(if index == 0 {
                    label(name, Line::raw(part))
                } else {
                    Line::raw(format!("{}{part}", " ".repeat(LABEL_WIDTH)))
                });
            }
        };
        let actions = self.actions();
        let labels: Vec<&str> = actions.iter().map(|action| self.label(*action)).collect();
        let mut body = Vec::new();
        let title = match self.picked()? {
            Picked::Skill(index) => {
                let skill = &self.skills[index];
                body.push(join([
                    styled(skill.name.clone(), Role::Accent, theme),
                    Line::raw("  "),
                    Line::raw(if skill.enabled { "On" } else { "Off" }),
                ]));
                body.push(Line::default());
                body.extend(wrap(&skill.description, room).into_iter().map(Line::raw));
                body.push(Line::default());
                wrapped("Folder", &tilde(&skill.source, ctx), &mut body);
                body.push(label("Projects", Line::raw("All projects")));
                "Skill"
            }
            Picked::Server(index) => {
                let server = &self.servers[index];
                let asking = server.status == ServerStatus::WaitingForApproval;
                if asking {
                    body.extend(
                        wrap(
                            &format!("{} wants to give agents new tools.", server.name),
                            room,
                        )
                        .into_iter()
                        .map(Line::raw),
                    );
                } else {
                    body.push(join([
                        styled(server.name.clone(), Role::Accent, theme),
                        Line::raw("  "),
                        status(server_word(server.status), theme, glyphs),
                    ]));
                }
                body.push(Line::default());
                wrapped("Runs", &server.runs, &mut body);
                for (index, secret) in server.secrets.iter().enumerate() {
                    let state = if secret.saved {
                        status("Key saved", theme, glyphs)
                    } else {
                        Line::raw("Not saved")
                    };
                    let value = join([
                        pad(
                            Line::raw(cut(&secret.name, 18, glyphs.ellipsis)),
                            20,
                            glyphs,
                        ),
                        state,
                    ]);
                    body.push(if index == 0 {
                        label("Secrets", value)
                    } else {
                        join([Line::raw(" ".repeat(LABEL_WIDTH)), value])
                    });
                }
                body.push(label("Projects", Line::raw("All projects")));
                if let Some(problem) = &server.problem {
                    wrapped("Problem", problem, &mut body);
                } else if !server.tools.is_empty() {
                    let tools =
                        format!("{} tools: {}", server.tools.len(), server.tools.join(", "));
                    body.push(label(
                        "Tools",
                        Line::raw(cut(&tools, value_room, glyphs.ellipsis)),
                    ));
                }
                if asking {
                    body.push(Line::default());
                    body.push(Line::raw(
                        "Agents can call its tools only after you approve.",
                    ));
                }
                if asking {
                    "Approve MCP server"
                } else {
                    "MCP server"
                }
            }
        };
        body.push(Line::default());
        body.push(buttons(
            &labels,
            Some(focus.min(labels.len().saturating_sub(1))),
            theme,
        ));
        let arrows = format!("{}{}", glyphs.left, glyphs.right);
        let close = if title == "Approve MCP server" {
            "Not now"
        } else {
            "Close"
        };
        Some((
            Modal {
                width: MODAL_WIDTH,
                title: title.into(),
                body,
                top: None,
            },
            vec![
                hint(&arrows, "Choose"),
                hint("Enter", "Confirm"),
                hint("Esc", close),
            ],
        ))
    }

    fn modal_key(&mut self, key: &Key, ctx: &mut Ctx) -> Flow {
        let count = self.actions().len().max(1);
        match self.open {
            Open::None => return Flow::Ignored,
            Open::Remove { remove_focused } => match key {
                Key::Esc => self.open = Open::None,
                Key::Left | Key::Right | Key::Tab | Key::BackTab => {
                    self.open = Open::Remove {
                        remove_focused: !remove_focused,
                    }
                }
                Key::Enter if remove_focused => {
                    self.open = Open::None;
                    self.remove(ctx);
                }
                Key::Enter => self.open = Open::None,
                _ => {}
            },
            Open::Details { focus } => match key {
                Key::Esc => self.open = Open::None,
                Key::Left | Key::BackTab => {
                    self.open = Open::Details {
                        focus: (focus + count - 1) % count,
                    }
                }
                Key::Right | Key::Tab => {
                    self.open = Open::Details {
                        focus: (focus + 1) % count,
                    }
                }
                Key::Enter => {
                    let action = self.actions().get(focus).copied().unwrap_or(Action::Close);
                    if let (Action::Read, Some(Picked::Skill(index))) = (action, self.picked()) {
                        self.open = Open::None;
                        let skill = &self.skills[index];
                        return Flow::Replace(Box::new(SkillDetailPage::new(
                            skill.id.clone(),
                            skill.name.clone(),
                        )));
                    }
                    if let (Action::Edit, Some(Picked::Server(index))) = (action, self.picked()) {
                        self.open = Open::None;
                        let server = self.servers[index].clone();
                        return Flow::Replace(Box::new(EditServer::new(server)));
                    }
                    self.run(action, ctx);
                }
                _ => {}
            },
        }
        Flow::Handled
    }
}

impl Screen for Plugins {
    fn crumbs(&self) -> Vec<String> {
        vec!["Settings".into(), "Plugins and skills".into()]
    }

    fn hints(&self, ctx: &Ctx) -> Vec<Hint> {
        vec![
            hint("a", "Add skill"),
            hint("m", "Add MCP server"),
            hint("Space", "On or off"),
            hint("Enter", "Details"),
            sections_hint(ctx.glyphs),
            hint("Esc", "Home"),
        ]
    }

    fn key(&mut self, key: &Key, ctx: &mut Ctx) -> Flow {
        if !matches!(self.open, Open::None) {
            return self.modal_key(key, ctx);
        }
        let count = self.count();
        match key {
            Key::Char('a') => return Flow::Replace(Box::new(AddPlugin::skill())),
            Key::Char('m') => return Flow::Replace(Box::new(AddPlugin::server())),
            Key::Char('r') if matches!(self.load, Load::Failed(_)) => self.reload(ctx),
            Key::Char(' ') if self.changing.is_none() => self.toggle(ctx),
            Key::Enter if self.picked().is_some() && self.changing.is_none() => {
                self.open = Open::Details { focus: 0 }
            }
            Key::Up => self.selected = self.selected.saturating_sub(1),
            Key::Down => self.selected = (self.selected + 1).min(count.saturating_sub(1)),
            Key::Home => self.selected = 0,
            Key::End => self.selected = count.saturating_sub(1),
            Key::Char(' ') | Key::Enter => {}
            _ => return Flow::Ignored,
        }
        Flow::Handled
    }

    fn step(&mut self, forward: bool, ctx: &mut Ctx) -> Flow {
        step(SettingsSection::Plugins, forward, ctx)
    }

    fn arrive(&mut self, arrival: &Arrival, ctx: &mut Ctx) -> Flow {
        match settings_event(arrival) {
            Some(SettingsEvent::Plugins(result)) => {
                let asked = std::mem::take(&mut self.waiting);
                match result {
                    Ok((skills, servers)) => {
                        self.skills = skills.clone();
                        self.servers = servers.clone();
                        self.load = Load::Ready(());
                        self.selected = self.selected.min(self.count().saturating_sub(1));
                    }
                    Err(code) if self.count() == 0 => self.load = Load::Failed(code.clone()),
                    Err(code) if asked => ctx.say(Message::error(
                        "Could not refresh plugins.",
                        plugin_plain(code),
                    )),
                    Err(_) => {}
                }
            }
            Some(SettingsEvent::PluginChanged(result)) => {
                let Some(done) = self.changing.take() else {
                    return Flow::Handled;
                };
                match result {
                    Ok(Some(server)) if server.problem.is_some() => ctx.say(Message::error(
                        &format!("{} can't start.", server.name),
                        server.problem.as_deref().unwrap_or_default(),
                    )),
                    Ok(Some(server)) if !server.tools.is_empty() && done.starts_with("Checked") => {
                        ctx.say(Message::success(
                            &format!("{} has {} tools.", server.name, server.tools.len()),
                            "",
                        ))
                    }
                    Ok(_) => ctx.say(Message::success(&done, "")),
                    Err(code) => ctx.say(Message::error(plugin_plain(code), "")),
                }
                self.reload(ctx);
            }
            _ => {}
        }
        Flow::Handled
    }

    fn modal(&self, ctx: &Ctx) -> Option<(Modal, Vec<Hint>)> {
        match self.open {
            Open::None => None,
            Open::Details { focus } => self.details(focus, ctx),
            Open::Remove { remove_focused } => {
                let (name, reach) = match self.picked()? {
                    Picked::Skill(index) => (
                        self.skills[index].name.clone(),
                        "Agents stop getting this skill in every project. Your folder stays.",
                    ),
                    Picked::Server(index) => (
                        self.servers[index].name.clone(),
                        "Agents lose its tools in every project. Its saved secrets are deleted.",
                    ),
                };
                let room = usize::from(MODAL_WIDTH) - 4;
                let mut body = vec![Line::raw(format!("Remove {name}?")), Line::default()];
                body.extend(wrap(reach, room).into_iter().map(Line::raw));
                body.push(Line::default());
                body.push(buttons(
                    &["Cancel", "Remove"],
                    Some(usize::from(remove_focused)),
                    &ctx.theme,
                ));
                let arrows = format!("{}{}", ctx.glyphs.left, ctx.glyphs.right);
                Some((
                    Modal {
                        width: MODAL_WIDTH,
                        title: "Remove".into(),
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
        if matches!(self.load, Load::Idle) {
            self.reload(ctx);
        }
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect, ctx: &Ctx) -> Option<Position> {
        let limit = (area.width as usize).saturating_sub(4).max(8);
        let content = self.body(limit, ctx);
        draw_page(frame, area, SettingsSection::Plugins, content, None, ctx)
    }

    fn busy(&self, _ctx: &Ctx) -> bool {
        self.waiting || self.changing.is_some()
    }

    fn place(&self) -> Option<Place> {
        Some(Place::Settings(SettingsSection::Plugins))
    }
}

fn place_of(project: Option<&(ProjectId, String)>) -> Place {
    match project {
        Some((id, name)) => Place::Project {
            id: id.clone(),
            name: name.clone(),
            tab: ProjectTab::Settings,
        },
        None => Place::Settings(SettingsSection::Plugins),
    }
}

/// Leaves a plugin form for the page it replaced.
/// Takes the form's project, if any, and the context to open that page with.
/// Returns a replace with the project's Settings tab or the Plugins page; the
/// form already reports the project's place, so opening that place would be a no-op.
fn back_to(project: Option<&(ProjectId, String)>, ctx: &mut Ctx) -> Flow {
    match project {
        Some(_) => Flow::Replace(super::open(&place_of(project), ctx)),
        None => Flow::Replace(Box::new(Plugins::new())),
    }
}

fn crumbs_of(project: Option<&(ProjectId, String)>, page: &str) -> Vec<String> {
    match project {
        Some((_, name)) => vec![name.clone(), "Settings".into(), page.into()],
        None => vec!["Settings".into(), "Plugins and skills".into(), page.into()],
    }
}

fn draw_form(
    frame: &mut Frame,
    area: Rect,
    for_project: bool,
    content: Vec<Line<'static>>,
    cursor: Option<Position>,
    ctx: &Ctx,
) -> Option<Position> {
    if !for_project {
        return draw_page(frame, area, SettingsSection::Plugins, content, cursor, ctx);
    }
    draw_lines(frame.buffer_mut(), area, &indent(content, 2));
    cursor.map(|at| Position::new(area.x + 2 + at.x, area.y + at.y))
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Skill,
    Server,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Focus {
    Folder,
    Name,
    Type,
    Target,
    SecretName,
    SecretValue,
    Submit,
}

/// Add skill and Add MCP server: a skill folder, or a server's name, command
/// or URL and hidden secrets, for every project or for one project. Secret
/// values go to the engine and are never shown.
/// Input: keys. Output: one add request; the list it came from when it is done.
pub struct AddPlugin {
    kind: Kind,
    project: Option<(ProjectId, String)>,
    folder: TextField,
    name: TextField,
    url: bool,
    target: TextField,
    secret_name: TextField,
    secret_value: TextField,
    secrets: Vec<(String, AccountSecret)>,
    focus: Focus,
    waiting: bool,
}

impl AddPlugin {
    /// The Add skill form.
    /// Input: none. Output: the screen with the folder field focused.
    pub fn skill() -> Self {
        Self::new(Kind::Skill)
    }

    /// The Add MCP server form.
    /// Input: none. Output: the screen with the name field focused.
    pub fn server() -> Self {
        Self::new(Kind::Server)
    }

    /// The same form adding only for one project, returning to that project's
    /// Settings tab.
    /// Inputs: the form, the project and its name. Output: the form.
    pub fn for_project(mut self, project: ProjectId, name: String) -> Self {
        self.project = Some((project, name));
        self
    }

    fn scope(&self) -> Scope {
        match &self.project {
            Some((project_id, _)) => Scope::Project {
                project_id: project_id.clone(),
            },
            None => Scope::Global,
        }
    }

    fn back(&self, ctx: &mut Ctx) -> Flow {
        back_to(self.project.as_ref(), ctx)
    }

    fn new(kind: Kind) -> Self {
        Self {
            kind,
            project: None,
            folder: TextField::new("Skill folder", FOLDER_CHARS_MAX)
                .placeholder("~/Skills/frontend-design")
                .note("The folder that holds SKILL.md. CrazyAgent keeps its own copy."),
            name: TextField::new("Name", SERVER_NAME_BYTES_MAX),
            url: false,
            target: TextField::new(
                "Command and arguments",
                COMMAND_BYTES_MAX.max(URL_BYTES_MAX),
            ),
            secret_name: TextField::new("", SECRET_NAME_BYTES_MAX),
            secret_value: TextField::secret("", SECRET_VALUE_CHARS_MAX),
            secrets: Vec::new(),
            focus: match kind {
                Kind::Skill => Focus::Folder,
                Kind::Server => Focus::Name,
            },
            waiting: false,
        }
    }

    fn order(&self) -> &'static [Focus] {
        match self.kind {
            Kind::Skill => &[Focus::Folder, Focus::Submit],
            Kind::Server => &[
                Focus::Name,
                Focus::Type,
                Focus::Target,
                Focus::SecretName,
                Focus::SecretValue,
                Focus::Submit,
            ],
        }
    }

    fn move_focus(&mut self, forward: bool) {
        let order = self.order();
        let at = order
            .iter()
            .position(|focus| *focus == self.focus)
            .unwrap_or(0);
        let next = if forward {
            (at + 1) % order.len()
        } else {
            (at + order.len() - 1) % order.len()
        };
        self.focus = order[next];
    }

    fn field(&mut self) -> Option<&mut TextField> {
        match self.focus {
            Focus::Folder => Some(&mut self.folder),
            Focus::Name => Some(&mut self.name),
            Focus::Target => Some(&mut self.target),
            Focus::SecretName => Some(&mut self.secret_name),
            Focus::SecretValue => Some(&mut self.secret_value),
            Focus::Type | Focus::Submit => None,
        }
    }

    fn set_type(&mut self, url: bool) {
        self.url = url;
        self.target = TextField::new(
            if url { "URL" } else { "Command and arguments" },
            COMMAND_BYTES_MAX.max(URL_BYTES_MAX),
        );
    }

    fn add_secret(&mut self) {
        let name = self.secret_name.value().trim().to_owned();
        if validate_secret_name(&name).is_err() {
            self.secret_name.error = Some(plugin_plain("server_secret_name_invalid").into());
            return;
        }
        if self.secrets.iter().any(|(saved, _)| saved == &name) {
            self.secret_name.error = Some("This secret is added already.".into());
            return;
        }
        if self.secrets.len() >= SECRETS_PER_SERVER_MAX {
            self.secret_name.error = Some(plugin_plain("server_secret_limit").into());
            return;
        }
        if self.secret_value.is_empty() {
            self.focus = Focus::SecretValue;
            return;
        }
        match AccountSecret::new(self.secret_value.take_secret()) {
            Ok(secret) => {
                self.secrets.push((name, secret));
                self.secret_name.clear();
                self.secret_name.error = None;
                self.focus = Focus::SecretName;
            }
            Err(error) => self.secret_name.error = Some(plugin_plain(&error.0).into()),
        }
        assert!(self.secret_value.is_empty());
        assert!(self.secrets.len() <= SECRETS_PER_SERVER_MAX);
    }

    fn submit(&mut self, ctx: &mut Ctx) {
        if self.waiting {
            return;
        }
        match self.kind {
            Kind::Skill => {
                let typed = self.folder.value().trim();
                let folder = match (typed.strip_prefix("~/"), ctx.home.as_deref()) {
                    (Some(rest), Some(home)) => format!("{home}/{rest}"),
                    _ => typed.to_owned(),
                };
                if !folder.starts_with('/') {
                    self.folder.error = Some(
                        "Use the full path of the folder, such as ~/Skills/frontend-design.".into(),
                    );
                    self.focus = Focus::Folder;
                    return;
                }
                self.waiting = true;
                let scope = self.scope();
                send(
                    ctx,
                    SettingsJob::Plugin(PluginChange::AddSkill { scope, folder }),
                );
            }
            Kind::Server => {
                let name = self.name.value().trim().to_owned();
                if validate_server_name(&name).is_err() {
                    self.name.error = Some(plugin_plain("server_name_invalid").into());
                    self.focus = Focus::Name;
                    return;
                }
                let spec = target_spec(self.url, self.target.value());
                let spec = match spec {
                    Ok(spec) => spec,
                    Err(problem) => {
                        self.target.error = Some(problem.into());
                        self.focus = Focus::Target;
                        return;
                    }
                };
                if !self.secret_name.value().trim().is_empty() || !self.secret_value.is_empty() {
                    self.secret_name.error =
                        Some("Press Enter to add this secret, or clear it.".into());
                    self.focus = Focus::SecretName;
                    return;
                }
                self.waiting = true;
                let secrets = std::mem::take(&mut self.secrets);
                send(
                    ctx,
                    SettingsJob::Plugin(PluginChange::AddServer {
                        scope: self.scope(),
                        name,
                        spec,
                        secrets,
                    }),
                );
            }
        }
    }

    /// The body lines of the form and the cursor.
    /// Inputs: the content width and context. Output: the lines and the cursor
    /// relative to them.
    pub fn body(&self, limit: usize, ctx: &Ctx) -> (Vec<Line<'static>>, Option<Position>) {
        let theme = &ctx.theme;
        let glyphs = ctx.glyphs;
        let width = limit.min(FIELD_WIDTH);
        let mut lines = Vec::new();
        let mut cursor = None;
        let mut place = |lines: &mut Vec<Line<'static>>, field: &TextField, focused: bool| {
            let top = lines.len() as u16;
            let (field_lines, at) = field.lines(width, focused, theme, glyphs);
            if let Some(at) = at {
                cursor = Some(Position::new(at.x, top + at.y));
            }
            lines.extend(field_lines);
        };
        match self.kind {
            Kind::Skill => {
                lines.push(styled("Add skill", Role::Accent, theme));
                lines.push(Line::raw(if self.project.is_some() {
                    "Only this project can use it. Turn it off any time."
                } else {
                    "Every project can use it. Turn it off any time."
                }));
                lines.push(Line::default());
                place(&mut lines, &self.folder, self.focus == Focus::Folder);
                lines.push(Line::default());
                lines.push(buttons(
                    &["Add skill"],
                    (self.focus == Focus::Submit).then_some(0),
                    theme,
                ));
            }
            Kind::Server => {
                lines.push(styled("Add MCP server", Role::Accent, theme));
                lines.push(Line::raw(if self.project.is_some() {
                    "Only this project's agents can use it, after you approve it."
                } else {
                    "Agents can use it only after you approve it."
                }));
                lines.push(Line::default());
                place(&mut lines, &self.name, self.focus == Focus::Name);
                let kinds = format!(
                    "{}   {}",
                    radio(!self.url, "Command on this Mac"),
                    radio(self.url, "URL")
                );
                let kinds = if self.focus == Focus::Type {
                    styled(kinds, Role::Accent, theme)
                } else {
                    Line::raw(kinds)
                };
                lines.push(join([
                    pad(styled("Type", Role::Heading, theme), 14, glyphs),
                    kinds,
                ]));
                place(&mut lines, &self.target, self.focus == Focus::Target);
                lines.push(Line::default());
                lines.push(row(
                    width,
                    styled("Secrets", Role::Heading, theme),
                    styled("Optional", Role::Muted, theme),
                    glyphs,
                ));
                for (name, _) in &self.secrets {
                    lines.push(join([
                        Line::raw("  "),
                        pad(Line::raw(cut(name, 16, glyphs.ellipsis)), 18, glyphs),
                        status("Key saved", theme, glyphs),
                    ]));
                }
                let top = lines.len() as u16;
                let value_width = width.saturating_sub(SECRET_NAME_BOX + 2).max(6);
                let (name_lines, name_at) = self.secret_name.lines(
                    SECRET_NAME_BOX,
                    self.focus == Focus::SecretName,
                    theme,
                    glyphs,
                );
                let (value_lines, value_at) = self.secret_value.lines(
                    value_width,
                    self.focus == Focus::SecretValue,
                    theme,
                    glyphs,
                );
                let boxed = |lines: &[Line<'static>]| lines.get(1).cloned().unwrap_or_default();
                lines.push(join([
                    boxed(&name_lines),
                    Line::raw("  "),
                    boxed(&value_lines),
                ]));
                if let Some(at) = name_at {
                    cursor = Some(Position::new(at.x, top + at.y - 1));
                }
                if let Some(at) = value_at {
                    cursor = Some(Position::new(
                        at.x + (SECRET_NAME_BOX + 2) as u16,
                        top + at.y - 1,
                    ));
                }
                match &self.secret_name.error {
                    Some(error) => lines.push(styled(error.clone(), Role::Error, theme)),
                    None => lines.push(styled(
                        "Type the name, then the value. Values stay hidden.",
                        Role::Muted,
                        theme,
                    )),
                }
                lines.push(Line::default());
                lines.push(buttons(
                    &["Add server"],
                    (self.focus == Focus::Submit).then_some(0),
                    theme,
                ));
            }
        }
        (lines, cursor)
    }
}

impl Screen for AddPlugin {
    fn crumbs(&self) -> Vec<String> {
        let page = match self.kind {
            Kind::Skill => "Add skill",
            Kind::Server => "Add MCP server",
        };
        crumbs_of(self.project.as_ref(), page)
    }

    fn hints(&self, _ctx: &Ctx) -> Vec<Hint> {
        let enter = match (self.kind, self.focus) {
            (_, Focus::SecretName | Focus::SecretValue) => "Add secret",
            (_, Focus::Type) => "Next",
            (Kind::Skill, _) => "Add skill",
            (Kind::Server, _) => "Add server",
        };
        vec![
            hint("Tab", "Next field"),
            hint("Enter", enter),
            hint("Ctrl-U", "Clear"),
            hint("Esc", "Cancel"),
        ]
    }

    fn typing(&self) -> bool {
        !matches!(self.focus, Focus::Type | Focus::Submit)
    }

    fn key(&mut self, key: &Key, ctx: &mut Ctx) -> Flow {
        match key {
            Key::Esc => return self.back(ctx),
            Key::Tab | Key::Down => self.move_focus(true),
            Key::BackTab | Key::Up => self.move_focus(false),
            Key::Enter => match self.focus {
                Focus::SecretName | Focus::SecretValue => self.add_secret(),
                Focus::Type => self.move_focus(true),
                _ => self.submit(ctx),
            },
            Key::Left | Key::Right | Key::Char(' ') if self.focus == Focus::Type => {
                self.set_type(!self.url)
            }
            other => {
                if let Some(field) = self.field() {
                    field.error = None;
                    field.key(other);
                }
            }
        }
        Flow::Handled
    }

    fn arrive(&mut self, arrival: &Arrival, ctx: &mut Ctx) -> Flow {
        let Some(SettingsEvent::PluginChanged(result)) = settings_event(arrival) else {
            return Flow::Handled;
        };
        if !std::mem::take(&mut self.waiting) {
            return Flow::Handled;
        }
        match result {
            Ok(Some(server)) => {
                ctx.say(Message::success(
                    &format!(
                        "Added {}. Approve it before agents can use it.",
                        server.name
                    ),
                    "",
                ));
                self.back(ctx)
            }
            Ok(None) => {
                ctx.say(Message::success(
                    if self.project.is_some() {
                        "Skill added for this project."
                    } else {
                        "Skill added for every project."
                    },
                    "",
                ));
                self.back(ctx)
            }
            Err(code) => {
                let problem = plugin_plain(code);
                match self.kind {
                    Kind::Skill => self.folder.error = Some(problem.into()),
                    Kind::Server => ctx.say(Message::error(
                        problem,
                        "Type any secrets again; they were not kept.",
                    )),
                }
                Flow::Handled
            }
        }
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect, ctx: &Ctx) -> Option<Position> {
        let limit = (area.width as usize).saturating_sub(4).max(8);
        let (content, cursor) = self.body(limit, ctx);
        draw_form(frame, area, self.project.is_some(), content, cursor, ctx)
    }

    fn busy(&self, _ctx: &Ctx) -> bool {
        self.waiting
    }

    fn draft(&self) -> bool {
        !self.name.is_empty()
            || !self.target.is_empty()
            || !self.folder.is_empty()
            || !self.secrets.is_empty()
    }

    fn place(&self) -> Option<Place> {
        Some(place_of(self.project.as_ref()))
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum EditFocus {
    Target,
    List,
    Name,
    Value,
}

/// One MCP server's page: what it runs and its secrets. Changing what it runs
/// needs approval again. Secret values are never shown.
/// Input: the server and keys. Output: one change at a time.
pub struct EditServer {
    server: ServerView,
    project: Option<(ProjectId, String)>,
    focus: EditFocus,
    selected: usize,
    target: TextField,
    name: TextField,
    value: TextField,
    confirm: Option<bool>,
    changing: Option<String>,
}

impl EditServer {
    /// The page of one server, on its command or URL.
    /// Input: the server as last read. Output: the screen.
    pub fn new(server: ServerView) -> Self {
        assert!(server.runs.chars().count() <= RUNS_CHARS_MAX);
        let label = match server.spec {
            ServerSpec::Stdio { .. } => "Command and arguments",
            ServerSpec::Http { .. } => "URL",
        };
        let mut target =
            TextField::new(label, RUNS_CHARS_MAX).right("A change needs approval again");
        target.set(&server.runs);
        Self {
            server,
            project: None,
            focus: EditFocus::Target,
            selected: 0,
            target,
            name: TextField::new("", SECRET_NAME_BYTES_MAX),
            value: TextField::secret("", SECRET_VALUE_CHARS_MAX),
            confirm: None,
            changing: None,
        }
    }

    /// The same page for a server added for one project, returning to that
    /// project's Settings tab.
    /// Inputs: the page, the project and its name. Output: the page.
    pub fn for_project(mut self, project: ProjectId, name: String) -> Self {
        self.project = Some((project, name));
        self
    }

    fn order(&self) -> Vec<EditFocus> {
        let mut order = vec![EditFocus::Target];
        if !self.server.secrets.is_empty() {
            order.push(EditFocus::List);
        }
        order.extend([EditFocus::Name, EditFocus::Value]);
        order
    }

    fn move_focus(&mut self, forward: bool) {
        let order = self.order();
        let at = order
            .iter()
            .position(|focus| *focus == self.focus)
            .unwrap_or(0);
        let next = if forward {
            (at + 1) % order.len()
        } else {
            (at + order.len() - 1) % order.len()
        };
        self.focus = order[next];
    }

    fn update(&mut self, ctx: &mut Ctx) {
        if self.changing.is_some() {
            return;
        }
        if self.target.value() == self.server.runs {
            ctx.say(Message::success("Nothing changed.", ""));
            return;
        }
        let url = matches!(self.server.spec, ServerSpec::Http { .. });
        match target_spec(url, self.target.value()) {
            Err(reason) => self.target.error = Some(reason.into()),
            Ok(spec) if spec == self.server.spec => {
                ctx.say(Message::success("Nothing changed.", ""));
            }
            Ok(spec) => {
                self.changing = Some(format!(
                    "Changed {}. Approve it before agents can use it.",
                    self.server.name
                ));
                let id = self.server.id.clone();
                send(ctx, SettingsJob::Plugin(PluginChange::Update { id, spec }));
            }
        }
    }

    fn save(&mut self, ctx: &mut Ctx) {
        if self.changing.is_some() {
            return;
        }
        let name = self.name.value().trim().to_owned();
        if validate_secret_name(&name).is_err() {
            self.name.error = Some(plugin_plain("server_secret_name_invalid").into());
            self.focus = EditFocus::Name;
            return;
        }
        let known = self.server.secrets.iter().any(|secret| secret.name == name);
        if !known && self.server.secrets.len() >= SECRETS_PER_SERVER_MAX {
            self.name.error = Some(plugin_plain("server_secret_limit").into());
            return;
        }
        if self.value.is_empty() {
            self.focus = EditFocus::Value;
            return;
        }
        match AccountSecret::new(self.value.take_secret()) {
            Ok(value) => {
                self.changing = Some(format!("Saved {name}."));
                self.name.clear();
                self.name.error = None;
                self.focus = EditFocus::Name;
                let id = self.server.id.clone();
                send(
                    ctx,
                    SettingsJob::Plugin(PluginChange::SetSecret { id, name, value }),
                );
            }
            Err(error) => self.name.error = Some(plugin_plain(&error.0).into()),
        }
        assert!(self.value.is_empty());
    }

    /// The body lines of the page and the cursor.
    /// Inputs: the content width and context. Output: the lines and the cursor
    /// relative to them.
    pub fn body(&self, limit: usize, ctx: &Ctx) -> (Vec<Line<'static>>, Option<Position>) {
        let theme = &ctx.theme;
        let glyphs = ctx.glyphs;
        let width = limit.min(FIELD_WIDTH);
        let mut lines = vec![
            row(
                width,
                styled(self.server.name.clone(), Role::Accent, theme),
                status(server_word(self.server.status), theme, glyphs),
                glyphs,
            ),
            styled(
                "Change what it runs or its secrets. Secret values stay hidden.",
                Role::Muted,
                theme,
            ),
            Line::default(),
        ];
        let mut cursor = None;
        let top = lines.len() as u16;
        let (target_lines, target_at) =
            self.target
                .lines(width, self.focus == EditFocus::Target, theme, glyphs);
        if let Some(at) = target_at {
            cursor = Some(Position::new(at.x, top + at.y));
        }
        lines.extend(target_lines);
        lines.push(Line::default());
        lines.push(row(
            width,
            styled(
                format!("Secrets  {}", self.server.secrets.len()),
                Role::Heading,
                theme,
            ),
            styled("Same name replaces it", Role::Muted, theme),
            glyphs,
        ));
        let listing = self.focus == EditFocus::List && self.confirm.is_none();
        for (index, secret) in self.server.secrets.iter().enumerate() {
            let state = if secret.saved {
                status("Key saved", theme, glyphs)
            } else {
                Line::raw("Not saved")
            };
            let content = join([
                pad(
                    Line::raw(cut(&secret.name, 16, glyphs.ellipsis)),
                    18,
                    glyphs,
                ),
                state,
            ]);
            lines.push(list_row(
                content,
                width,
                index == self.selected,
                listing,
                theme,
                glyphs,
            ));
        }
        let top = lines.len() as u16;
        let value_width = width.saturating_sub(SECRET_NAME_BOX + 2).max(6);
        let (name_lines, name_at) = self.name.lines(
            SECRET_NAME_BOX,
            self.focus == EditFocus::Name,
            theme,
            glyphs,
        );
        let (value_lines, value_at) =
            self.value
                .lines(value_width, self.focus == EditFocus::Value, theme, glyphs);
        let boxed = |field: &[Line<'static>]| field.get(1).cloned().unwrap_or_default();
        lines.push(join([
            boxed(&name_lines),
            Line::raw("  "),
            boxed(&value_lines),
        ]));
        if let Some(at) = name_at {
            cursor = Some(Position::new(at.x, top + at.y - 1));
        }
        if let Some(at) = value_at {
            cursor = Some(Position::new(
                at.x + (SECRET_NAME_BOX + 2) as u16,
                top + at.y - 1,
            ));
        }
        match &self.name.error {
            Some(error) => lines.push(styled(error.clone(), Role::Error, theme)),
            None => lines.push(styled(
                "Type a secret name, then its value, then press Enter.",
                Role::Muted,
                theme,
            )),
        }
        (lines, cursor)
    }
}

impl Screen for EditServer {
    fn crumbs(&self) -> Vec<String> {
        crumbs_of(self.project.as_ref(), &self.server.name)
    }

    fn hints(&self, ctx: &Ctx) -> Vec<Hint> {
        match self.focus {
            EditFocus::List => {
                let arrows = format!("{}{}", ctx.glyphs.up, ctx.glyphs.down);
                vec![
                    hint(&arrows, "Move"),
                    hint("Enter", "Replace"),
                    hint("d", "Remove"),
                    hint("Tab", "Next field"),
                    hint("Esc", "Back"),
                ]
            }
            EditFocus::Target => vec![
                hint("Tab", "Next field"),
                hint("Enter", "Save change"),
                hint("Esc", "Back"),
            ],
            EditFocus::Name | EditFocus::Value => vec![
                hint("Tab", "Next field"),
                hint("Enter", "Save secret"),
                hint("Ctrl-U", "Clear"),
                hint("Esc", "Back"),
            ],
        }
    }

    fn typing(&self) -> bool {
        self.focus != EditFocus::List && self.confirm.is_none()
    }

    fn key(&mut self, key: &Key, ctx: &mut Ctx) -> Flow {
        if let Some(remove_focused) = self.confirm {
            match key {
                Key::Esc => self.confirm = None,
                Key::Left | Key::Right | Key::Tab | Key::BackTab => {
                    self.confirm = Some(!remove_focused)
                }
                Key::Enter => {
                    self.confirm = None;
                    let secret = self.server.secrets.get(self.selected);
                    let idle = self.changing.is_none();
                    if let (true, Some(secret), true) = (remove_focused, secret, idle) {
                        let change = PluginChange::RemoveSecret {
                            id: self.server.id.clone(),
                            name: secret.name.clone(),
                        };
                        self.changing = Some(format!("Removed {}.", secret.name));
                        send(ctx, SettingsJob::Plugin(change));
                    }
                }
                _ => {}
            }
            return Flow::Handled;
        }
        let count = self.server.secrets.len();
        match (self.focus, key) {
            (_, Key::Esc) => return back_to(self.project.as_ref(), ctx),
            (_, Key::Tab) => self.move_focus(true),
            (_, Key::BackTab) => self.move_focus(false),
            (EditFocus::List, Key::Up) => self.selected = self.selected.saturating_sub(1),
            (EditFocus::List, Key::Down) => {
                self.selected = (self.selected + 1).min(count.saturating_sub(1))
            }
            (EditFocus::List, Key::Char('d')) if count > 0 && self.changing.is_none() => {
                self.confirm = Some(false)
            }
            (EditFocus::List, Key::Enter) => {
                if let Some(secret) = self.server.secrets.get(self.selected) {
                    self.name.set(&secret.name);
                    self.name.error = None;
                    self.focus = EditFocus::Value;
                }
            }
            (EditFocus::List, _) => return Flow::Ignored,
            (EditFocus::Target, Key::Enter) => self.update(ctx),
            (EditFocus::Target, other) => {
                self.target.error = None;
                self.target.key(other);
            }
            (_, Key::Enter) => self.save(ctx),
            (EditFocus::Name, other) => {
                self.name.error = None;
                self.name.key(other);
            }
            (EditFocus::Value, other) => {
                self.value.key(other);
            }
        }
        Flow::Handled
    }

    fn arrive(&mut self, arrival: &Arrival, ctx: &mut Ctx) -> Flow {
        let Some(SettingsEvent::PluginChanged(result)) = settings_event(arrival) else {
            return Flow::Handled;
        };
        let Some(done) = self.changing.take() else {
            return Flow::Handled;
        };
        match result {
            Ok(Some(server)) => {
                if server.spec != self.server.spec {
                    self.target.set(&server.runs);
                }
                self.server = server.clone();
                self.selected = self
                    .selected
                    .min(self.server.secrets.len().saturating_sub(1));
                if self.server.secrets.is_empty() && self.focus == EditFocus::List {
                    self.focus = EditFocus::Name;
                }
                ctx.say(Message::success(&done, ""));
            }
            Ok(None) => {}
            Err(code) => ctx.say(Message::error(plugin_plain(code), "")),
        }
        Flow::Handled
    }

    fn modal(&self, ctx: &Ctx) -> Option<(Modal, Vec<Hint>)> {
        let remove_focused = self.confirm?;
        let secret = self.server.secrets.get(self.selected)?;
        let room = usize::from(MODAL_WIDTH) - 4;
        let mut body = vec![
            Line::raw(format!("Remove {} from {}?", secret.name, self.server.name)),
            Line::default(),
        ];
        body.extend(
            wrap("The server may stop working until you save it again.", room)
                .into_iter()
                .map(Line::raw),
        );
        body.push(Line::default());
        body.push(buttons(
            &["Cancel", "Remove"],
            Some(usize::from(remove_focused)),
            &ctx.theme,
        ));
        let arrows = format!("{}{}", ctx.glyphs.left, ctx.glyphs.right);
        Some((
            Modal {
                width: MODAL_WIDTH,
                title: "Remove secret".into(),
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

    fn draw(&mut self, frame: &mut Frame, area: Rect, ctx: &Ctx) -> Option<Position> {
        let limit = (area.width as usize).saturating_sub(4).max(8);
        let (content, cursor) = self.body(limit, ctx);
        draw_form(frame, area, self.project.is_some(), content, cursor, ctx)
    }

    fn busy(&self, _ctx: &Ctx) -> bool {
        self.changing.is_some()
    }

    fn place(&self) -> Option<Place> {
        Some(place_of(self.project.as_ref()))
    }
}

mod skill_detail;
pub use skill_detail::SkillDetailPage;

#[cfg(test)]
mod tests;
