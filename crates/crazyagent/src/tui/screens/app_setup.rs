use super::settings_accounts::{send, settings_event};
use super::settings_plugins::tilde;
use super::{Flow, Place, Screen, open};
use crate::github::{GitHubMethod, GitHubStatus};
use crate::projects_home::{PATH_BYTES_MAX, ProjectsHome, path_text};
use crate::setup::pack::entry_title;
use crate::setup::{DependencyState, PackEntry, PackKind, PackState, SetupStatus, plain_reason};
use crate::tui::client::app_setup::{AppSetupEvent, AppSetupJob, PackChange};
use crate::tui::client::settings::{SettingsEvent, SettingsJob};
use crate::tui::ctx::{Ctx, Load};
use crate::tui::frame::{Hint, Message, draw_lines, hint};
use crate::tui::keys::Key;
use crate::tui::seam::{Arrival, NOT_READY};
use crate::tui::text::{
    cells, indent, join, list_row, pad, row, status, styled, thumb, with_bar, wrap,
};
use crate::tui::theme::Role;
use crate::tui::widgets::{TextField, checkbox};
use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::text::Line;
use std::collections::BTreeMap;

#[cfg(test)]
mod tests;

const TOOL_WIDTH: usize = 16;
const NAME_WIDTH: usize = 26;
const KIND_WIDTH: usize = 10;
const LICENSE_WIDTH: usize = 12;
const LABEL_WIDTH: usize = 15;
const FIELD_WIDTH: usize = 72;
const WINDOW_MIN: usize = 3;
const NOT_READY_TEXT: &str =
    "Not ready yet. Setup comes with the setup and default skills and plugins update.";
const FIRST_RUN_TEXT: &str = "CrazyAgent needs a few tools on this computer and starts every project with the skills and plugins below. Untick anything you don't want.";
const SETTINGS_TEXT: &str = "The tools CrazyAgent needs on this computer, the skills and plugins every project starts with, the projects home folder and your GitHub account.";

/// A plain sentence for a setup page problem code.
/// Input: the code. Output: "Not ready yet." until the engine serves the
/// request, otherwise the setup sentence for the code.
pub fn setup_plain(code: &str) -> &'static str {
    match code {
        NOT_READY => "Not ready yet.",
        "trusted_connection_unavailable" | "controller_disconnected" => {
            "CrazyAgent is not connected to the engine right now."
        }
        "home_path_invalid" => "Use a full folder path, such as /Users/you/Projects.",
        "owner_outcome_unknown_reconcile_same_identity" => {
            "CrazyAgent did not answer in time. Try again."
        }
        _ => plain_reason(code),
    }
}

/// A folder size in plain units.
/// Input: bytes. Output: such as "820 MB" or "2.4 GB".
pub fn size(bytes: u64) -> String {
    const KIB: u64 = 1024;
    match bytes {
        0..KIB => format!("{bytes} B"),
        KIB..0x10_0000 => format!("{} KB", bytes / KIB),
        0x10_0000..0x4000_0000 => format!("{} MB", bytes / 0x10_0000),
        _ => format!("{:.1} GB", bytes as f64 / 0x4000_0000 as f64),
    }
}

fn kind_word(kind: PackKind) -> &'static str {
    match kind {
        PackKind::Skill => "Skill",
        PackKind::McpServer => "MCP server",
    }
}

fn pack_word(state: &PackState) -> &'static str {
    match state {
        PackState::Available => "Not installed",
        PackState::Installing => "Installing",
        PackState::On => "Installed",
        PackState::Off => "Turned off",
        PackState::Failed { .. } => "Failed",
    }
}

fn tool_word(state: &DependencyState) -> String {
    match state {
        DependencyState::Checking => "Checking".into(),
        DependencyState::Missing => "Not installed".into(),
        DependencyState::Found { version } => format!("Found {}", version.as_str()),
        DependencyState::Installing => "Installing".into(),
        DependencyState::Installed { version } => format!("Installed {}", version.as_str()),
        DependencyState::Failed { .. } => "Failed".into(),
    }
}

fn method_words(method: GitHubMethod) -> &'static str {
    match method {
        GitHubMethod::Device => "with GitHub sign-in",
        GitHubMethod::Token => "with a pasted token",
        GitHubMethod::Cli => "through the gh command",
        GitHubMethod::None => "",
    }
}

fn loaded<T: Clone>(result: &Result<T, String>) -> Load<T> {
    match result {
        Ok(value) => Load::Ready(value.clone()),
        Err(code) if code == NOT_READY => Load::NotReady,
        Err(code) => Load::Failed(code.clone()),
    }
}

fn expand(typed: &str, ctx: &Ctx) -> String {
    let typed = typed.trim();
    match (typed.strip_prefix('~'), ctx.home.as_deref()) {
        (Some(rest), Some(home)) if rest.is_empty() || rest.starts_with('/') => {
            format!("{home}{rest}")
        }
        _ => typed.to_owned(),
    }
}

enum Ask {
    Remove {
        id: String,
    },
    SignOut,
    HomePath(TextField),
    HomeChoice {
        path: String,
        size: Option<Result<u64, String>>,
    },
}

enum Sent {
    Run,
    Done(String),
}

/// The setup page: the tools CrazyAgent needs on this machine and the default
/// skills and plugins pack, with ticks. First run shows it after the projects
/// home folder: Enter installs and continues to Home, and continuing past the
/// ticked MCP servers approves them. Settings shows it with on and off,
/// remove, the projects home folder and the GitHub account.
pub struct AppSetup {
    first_run: bool,
    setup: Load<SetupStatus>,
    home: Load<ProjectsHome>,
    github: Load<GitHubStatus>,
    chosen: BTreeMap<String, bool>,
    selected: usize,
    offset: usize,
    reading: [bool; 3],
    sent: Option<Sent>,
    started: bool,
    finished_with_problems: bool,
    ask: Option<Ask>,
}

impl AppSetup {
    /// A setup page with nothing loaded yet.
    /// Input: true during first run. Output: the screen; `shown` asks the
    /// engine for the setup status, and in Settings also for the projects
    /// home and the GitHub account.
    pub fn new(first_run: bool) -> Self {
        Self {
            first_run,
            setup: Load::Idle,
            home: Load::Idle,
            github: Load::Idle,
            chosen: BTreeMap::new(),
            selected: 0,
            offset: 0,
            reading: [false; 3],
            sent: None,
            started: false,
            finished_with_problems: false,
            ask: None,
        }
    }

    fn entries(&self) -> &[PackEntry] {
        match &self.setup {
            Load::Ready(status) => &status.pack,
            _ => &[],
        }
    }

    fn picked(&self) -> Option<&PackEntry> {
        self.entries().get(self.selected)
    }

    fn installed(state: &PackState) -> bool {
        matches!(
            state,
            PackState::On | PackState::Off | PackState::Installing
        )
    }

    fn ticked(&self, entry: &PackEntry) -> bool {
        if Self::installed(&entry.state) && !self.first_run {
            return entry.state != PackState::Off;
        }
        self.chosen
            .get(&entry.id)
            .copied()
            .unwrap_or(match entry.state {
                PackState::On | PackState::Installing => true,
                PackState::Off => false,
                PackState::Available | PackState::Failed { .. } => entry.on_by_default,
            })
    }

    fn read(&mut self, index: usize, job: AppSetupJob, ctx: &mut Ctx) {
        assert!(index < self.reading.len());
        if self.reading[index] {
            return;
        }
        self.reading[index] = true;
        send(ctx, SettingsJob::AppSetup(job));
    }

    fn reload(&mut self, ctx: &mut Ctx) {
        if matches!(self.setup, Load::Idle) {
            self.setup = Load::Loading;
        }
        self.read(0, AppSetupJob::Status, ctx);
        if self.first_run {
            return;
        }
        if matches!(self.home, Load::Idle) {
            self.home = Load::Loading;
        }
        self.read(1, AppSetupJob::Home, ctx);
        if matches!(self.github, Load::Idle) {
            self.github = Load::Loading;
        }
        self.read(2, AppSetupJob::GitHub, ctx);
    }

    fn change(&mut self, job: AppSetupJob, sent: Sent, ctx: &mut Ctx) {
        if self.sent.is_some() {
            ctx.say(Message::warning("Wait for the last change to finish.", ""));
            return;
        }
        self.sent = Some(sent);
        send(ctx, SettingsJob::AppSetup(job));
    }

    fn toggle(&mut self, ctx: &mut Ctx) {
        let Some(entry) = self.picked().cloned() else {
            return;
        };
        let title = entry_title(&entry.id);
        if entry.state == PackState::Installing {
            ctx.say(Message::warning(&format!("{title} is installing now."), ""));
            return;
        }
        if Self::installed(&entry.state) && self.first_run {
            ctx.say(Message::warning(
                &format!("{title} is already installed."),
                "Switch it on or off in Settings, Setup.",
            ));
            return;
        }
        if Self::installed(&entry.state) {
            let on = entry.state == PackState::Off;
            let done = format!("{title} is {}.", if on { "on" } else { "off" });
            let job = AppSetupJob::Pack {
                change: PackChange::Switch { id: entry.id, on },
                generation: ctx.next_generation(),
            };
            self.change(job, Sent::Done(done), ctx);
            return;
        }
        let ticked = self.ticked(&entry);
        self.chosen.insert(entry.id, !ticked);
    }

    fn run(&mut self, ctx: &mut Ctx) -> Flow {
        let running = match &self.setup {
            Load::NotReady if self.first_run => return Flow::Replace(open(&Place::Home, ctx)),
            Load::Failed(_) => {
                self.reload(ctx);
                return Flow::Handled;
            }
            Load::Ready(status) => status.running,
            _ => return Flow::Handled,
        };
        if running || self.started || self.sent.is_some() {
            ctx.say(Message::warning(setup_plain("setup_busy"), ""));
            return Flow::Handled;
        }
        let pack: Vec<String> = self
            .entries()
            .iter()
            .filter(|entry| self.ticked(entry) && !Self::installed(&entry.state))
            .map(|entry| entry.id.clone())
            .collect();
        self.finished_with_problems = false;
        let job = AppSetupJob::Run {
            pack,
            generation: ctx.next_generation(),
        };
        self.change(job, Sent::Run, ctx);
        Flow::Handled
    }

    fn problems(&self) -> usize {
        let Load::Ready(status) = &self.setup else {
            return 0;
        };
        let tools = status
            .dependencies
            .iter()
            .filter(|tool| {
                matches!(
                    tool.state,
                    DependencyState::Missing | DependencyState::Failed { .. }
                )
            })
            .count();
        let pack = status
            .pack
            .iter()
            .filter(|entry| {
                self.ticked(entry)
                    && matches!(entry.state, PackState::Available | PackState::Failed { .. })
            })
            .count();
        tools + pack
    }

    fn read_setup(&mut self, result: &Result<SetupStatus, String>, ctx: &mut Ctx) -> Flow {
        self.reading[0] = false;
        let status = match result {
            Ok(status) => status,
            Err(code) if code != NOT_READY && matches!(self.setup, Load::Ready(_)) => {
                ctx.say(Message::error(
                    &format!("Failed to check setup. {}", setup_plain(code)),
                    "",
                ));
                return Flow::Handled;
            }
            Err(_) => {
                self.setup = loaded(result);
                return Flow::Handled;
            }
        };
        self.setup = Load::Ready(status.clone());
        self.selected = self.selected.min(status.pack.len().saturating_sub(1));
        if status.running {
            self.started = true;
            return Flow::Handled;
        }
        if !self.started {
            return Flow::Handled;
        }
        self.started = false;
        let problems = self.problems();
        if problems == 0 {
            if self.first_run {
                return Flow::Replace(open(&Place::Home, ctx));
            }
            ctx.say(Message::success(
                "Setup finished.",
                "Everything is installed.",
            ));
            return Flow::Handled;
        }
        self.finished_with_problems = true;
        let items = if problems == 1 { "item" } else { "items" };
        ctx.say(Message::warning(
            &format!("Setup finished, but {problems} {items} did not install."),
            "See the reasons below.",
        ));
        Flow::Handled
    }

    fn ran(&mut self, result: &Result<(), String>, ctx: &mut Ctx) {
        if !matches!(self.sent, Some(Sent::Run)) {
            return;
        }
        self.sent = None;
        match result {
            Ok(()) => {
                self.started = true;
                self.read(0, AppSetupJob::Status, ctx);
            }
            Err(code) => ctx.say(Message::error(setup_plain(code), "")),
        }
    }

    fn changed(&mut self, result: &Result<(), String>, ctx: &mut Ctx) {
        let done = match self.sent.take() {
            Some(Sent::Done(done)) => done,
            other => {
                self.sent = other;
                return;
            }
        };
        match result {
            Ok(()) => {
                ctx.say(Message::success(&done, ""));
                self.reload(ctx);
            }
            Err(code) => ctx.say(Message::error(setup_plain(code), "")),
        }
    }

    fn ask_remove(&mut self, ctx: &mut Ctx) {
        let Some(entry) = self.picked() else {
            return;
        };
        let id = entry.id.clone();
        let title = entry_title(&id);
        let problem = if entry.state == PackState::Installing {
            Some(format!("{title} is installing now."))
        } else if !Self::installed(&entry.state) {
            Some(format!("{title} is not installed."))
        } else {
            None
        };
        match problem {
            Some(problem) => ctx.say(Message::warning(&problem, "")),
            None => self.ask = Some(Ask::Remove { id }),
        }
    }

    fn ask_home(&mut self, ctx: &mut Ctx) {
        let Load::Ready(home) = &self.home else {
            ctx.say(Message::warning(
                "The projects home folder is not known yet.",
                "",
            ));
            return;
        };
        let mut field =
            TextField::new("New projects home folder", PATH_BYTES_MAX).placeholder("~/Projects");
        if let Some(path) = &home.path {
            field.set(&tilde(path, ctx));
        }
        self.ask = Some(Ask::HomePath(field));
    }

    fn ask_sign_out(&mut self, ctx: &mut Ctx) {
        match &self.github {
            Load::Ready(github) if github.signed_in => self.ask = Some(Ask::SignOut),
            Load::Ready(_) => ctx.say(Message::warning(
                "CrazyAgent is not signed in to GitHub.",
                "",
            )),
            _ => {}
        }
    }

    fn submit_home(&mut self, typed: &str, field: TextField, ctx: &mut Ctx) {
        let path = expand(typed, ctx);
        if path_text(&path, "home_path_invalid").is_err() {
            ctx.say(Message::error(setup_plain("home_path_invalid"), ""));
            self.ask = Some(Ask::HomePath(field));
            return;
        }
        let current = match &self.home {
            Load::Ready(home) => home.path.clone(),
            _ => None,
        };
        match current {
            Some(current) if current == path => {
                ctx.say(Message::warning(
                    "That folder is already the projects home.",
                    "",
                ));
            }
            Some(current) => {
                self.ask = Some(Ask::HomeChoice { path, size: None });
                send(
                    ctx,
                    SettingsJob::AppSetup(AppSetupJob::FolderSize { path: current }),
                );
            }
            None => self.set_home(path, false, ctx),
        }
    }

    fn set_home(&mut self, path: String, move_projects: bool, ctx: &mut Ctx) {
        let shown = tilde(&path, ctx);
        let done = if move_projects {
            format!("Moved every project to {shown}.")
        } else {
            format!("The projects home is now {shown}. Nothing was moved.")
        };
        let job = AppSetupJob::SetHome {
            path,
            move_projects,
            generation: ctx.next_generation(),
        };
        self.change(job, Sent::Done(done), ctx);
    }

    fn ask_key(&mut self, ask: Ask, key: &Key, ctx: &mut Ctx) {
        match ask {
            Ask::HomePath(mut field) => match key {
                Key::Esc => {}
                Key::Enter => {
                    let typed = field.value().to_owned();
                    self.submit_home(&typed, field, ctx);
                }
                other => {
                    field.key(other);
                    self.ask = Some(Ask::HomePath(field));
                }
            },
            Ask::HomeChoice { path, size } => match key {
                Key::Enter => self.set_home(path, false, ctx),
                Key::Char('m') => self.set_home(path, true, ctx),
                Key::Esc | Key::Char('n') => {}
                _ => self.ask = Some(Ask::HomeChoice { path, size }),
            },
            Ask::Remove { id } => match key {
                Key::Char('y') => {
                    let done = format!("Removed {id}.");
                    let job = AppSetupJob::Pack {
                        change: PackChange::Remove { id },
                        generation: ctx.next_generation(),
                    };
                    self.change(job, Sent::Done(done), ctx);
                }
                Key::Char('n') | Key::Esc => {}
                _ => self.ask = Some(Ask::Remove { id }),
            },
            Ask::SignOut => match key {
                Key::Char('y') => {
                    let done = "Signed out of GitHub.".to_owned();
                    self.change(AppSetupJob::GitHubSignOut, Sent::Done(done), ctx);
                }
                Key::Char('n') | Key::Esc => {}
                _ => self.ask = Some(Ask::SignOut),
            },
        }
    }

    /// True when no setup run is going and no change waits for the engine.
    /// Input: none. Output: whether first run may leave for Home.
    fn quiet(&self) -> bool {
        !self.started && self.sent.is_none()
    }

    fn space_label(&self) -> &'static str {
        match self.picked() {
            Some(entry) if Self::installed(&entry.state) && !self.first_run => {
                if entry.state == PackState::Off {
                    "Turn on"
                } else {
                    "Turn off"
                }
            }
            Some(entry) if self.ticked(entry) => "Untick",
            _ => "Tick",
        }
    }

    fn tools(&self, setup: &SetupStatus, limit: usize, ctx: &Ctx) -> Vec<Line<'static>> {
        let theme = &ctx.theme;
        let glyphs = ctx.glyphs;
        let mut lines = vec![styled("Tools", Role::Heading, theme)];
        let rest = limit.saturating_sub(2 + TOOL_WIDTH + 2).max(1);
        for tool in &setup.dependencies {
            lines.push(join([
                Line::raw("  "),
                cells(
                    &[TOOL_WIDTH, rest],
                    vec![
                        Line::raw(tool.label.clone()),
                        status(&tool_word(&tool.state), theme, glyphs),
                    ],
                    2,
                    glyphs,
                ),
            ]));
            if let DependencyState::Failed { reason } = &tool.state {
                let gap = " ".repeat(2 + TOOL_WIDTH + 2);
                lines.extend(
                    wrap(plain_reason(reason), rest).into_iter().map(|part| {
                        join([Line::raw(gap.clone()), styled(part, Role::Muted, theme)])
                    }),
                );
            }
        }
        lines
    }

    fn pack_heading(&self, limit: usize, ctx: &Ctx) -> Line<'static> {
        let theme = &ctx.theme;
        let total = self.entries().len();
        let on = self
            .entries()
            .iter()
            .filter(|entry| self.ticked(entry))
            .count();
        let word = if self.first_run { "ticked" } else { "on" };
        row(
            limit,
            styled("Skills and plugins", Role::Heading, theme),
            styled(format!("{on} of {total} {word}"), Role::Muted, theme),
            ctx.glyphs,
        )
    }

    fn pack_rows(&mut self, limit: usize, window: usize, ctx: &Ctx) -> Vec<Line<'static>> {
        let theme = &ctx.theme;
        let glyphs = ctx.glyphs;
        let total = self.entries().len();
        if total == 0 {
            return vec![styled(
                "The default pack is empty in this build.",
                Role::Muted,
                theme,
            )];
        }
        assert!(window > 0);
        self.offset = self.offset.min(total.saturating_sub(window));
        if self.selected < self.offset {
            self.offset = self.selected;
        } else if self.selected >= self.offset + window {
            self.offset = self.selected + 1 - window;
        }
        let scrolls = total > window;
        let row_limit = if scrolls { limit - 1 } else { limit };
        let rest = row_limit
            .saturating_sub(2 + 4 + NAME_WIDTH + KIND_WIDTH + LICENSE_WIDTH + 3 * 2)
            .max(1);
        let widths = [4 + NAME_WIDTH, KIND_WIDTH, LICENSE_WIDTH, rest];
        let focused = self.ask.is_none();
        let rows: Vec<Line<'static>> = self
            .entries()
            .iter()
            .enumerate()
            .skip(self.offset)
            .take(window)
            .map(|(index, entry)| {
                let values = vec![
                    Line::raw(checkbox(self.ticked(entry), entry_title(&entry.id))),
                    Line::raw(kind_word(entry.kind)),
                    Line::raw(entry.license.clone()),
                    status(pack_word(&entry.state), theme, glyphs),
                ];
                list_row(
                    cells(&widths, values, 2, glyphs),
                    row_limit,
                    index == self.selected,
                    focused,
                    theme,
                    glyphs,
                )
            })
            .collect();
        if scrolls {
            let (top, length) = thumb(window, self.offset, total);
            with_bar(rows, limit, top, length, theme, glyphs)
        } else {
            rows
        }
    }

    fn pack_notes(&self, limit: usize, ctx: &Ctx) -> Vec<Line<'static>> {
        let theme = &ctx.theme;
        let mut lines = Vec::new();
        if let Some(PackEntry {
            id,
            state: PackState::Failed { reason },
            ..
        }) = self.picked()
        {
            lines.extend(
                wrap(
                    &format!("{}: {}", entry_title(id), plain_reason(reason)),
                    limit,
                )
                .into_iter()
                .map(|part| styled(part, Role::Muted, theme)),
            );
        }
        let servers: Vec<&str> = self
            .entries()
            .iter()
            .filter(|entry| {
                entry.kind == PackKind::McpServer
                    && self.ticked(entry)
                    && !Self::installed(&entry.state)
            })
            .map(|entry| entry.id.as_str())
            .collect();
        if !servers.is_empty() {
            let lead = if self.first_run {
                "Continuing installs these MCP servers and approves them for your agents:"
            } else {
                "Installing approves these MCP servers for your agents:"
            };
            lines.push(Line::default());
            lines.extend(
                wrap(&format!("{lead} {}.", servers.join(", ")), limit)
                    .into_iter()
                    .map(Line::raw),
            );
        }
        lines
    }

    fn account_rows(&self, limit: usize, ctx: &Ctx) -> Vec<Line<'static>> {
        let theme = &ctx.theme;
        let glyphs = ctx.glyphs;
        let label = |text: &str, value: Line<'static>| {
            join([
                pad(styled(text, Role::Heading, theme), LABEL_WIDTH, glyphs),
                value,
            ])
        };
        let mut lines = vec![Line::default()];
        let home = match &self.home {
            Load::Ready(home) => match &home.path {
                Some(path) => Line::raw(tilde(path, ctx)),
                None => styled("Not chosen yet", Role::Muted, theme),
            },
            Load::NotReady => styled("Not ready yet", Role::Muted, theme),
            Load::Failed(code) => status(
                &format!("Failed to read it. {}", setup_plain(code)),
                theme,
                glyphs,
            ),
            Load::Idle | Load::Loading => ctx.loading("Loading"),
        };
        lines.push(label("Projects home", home));
        let mut problem = None;
        let github = match &self.github {
            Load::Ready(github) => {
                problem = github.problem.clone();
                match (&github.login, github.signed_in) {
                    (Some(login), true) => join([
                        status(&format!("Signed in as {login}"), theme, glyphs),
                        styled(
                            format!("  {}", method_words(github.method)),
                            Role::Muted,
                            theme,
                        ),
                    ]),
                    (None, true) => status("Signed in", theme, glyphs),
                    _ if github.waiting => {
                        Line::raw("Waiting for you to enter the code on github.com")
                    }
                    _ => join([
                        status("Not signed in", theme, glyphs),
                        styled("  Sign in when you clone from GitHub.", Role::Muted, theme),
                    ]),
                }
            }
            Load::NotReady => styled("Not ready yet", Role::Muted, theme),
            Load::Failed(code) => status(
                &format!("Failed to read it. {}", setup_plain(code)),
                theme,
                glyphs,
            ),
            Load::Idle | Load::Loading => ctx.loading("Loading"),
        };
        lines.push(label("GitHub", github));
        if let Some(problem) = problem {
            let gap = " ".repeat(LABEL_WIDTH);
            lines.extend(
                wrap(&problem, limit.saturating_sub(LABEL_WIDTH).max(1))
                    .into_iter()
                    .map(|part| join([Line::raw(gap.clone()), styled(part, Role::Muted, theme)])),
            );
        }
        lines
    }

    fn ask_rows(&self, limit: usize, ctx: &Ctx) -> (Vec<Line<'static>>, Option<Position>) {
        let theme = &ctx.theme;
        let sentence = |text: &str| -> Vec<Line<'static>> {
            wrap(text, limit).into_iter().map(Line::raw).collect()
        };
        let mut lines = vec![Line::default()];
        let mut cursor = None;
        match &self.ask {
            None => return (Vec::new(), None),
            Some(Ask::Remove { id }) => lines.extend(sentence(&format!(
                "Remove {}? Agents in every project stop getting it.",
                entry_title(id)
            ))),
            Some(Ask::SignOut) => lines.extend(sentence(
                "Sign out of GitHub? CrazyAgent deletes the GitHub token it keeps. Cloned projects stay.",
            )),
            Some(Ask::HomePath(field)) => {
                let (field_lines, at) =
                    field.lines(limit.min(FIELD_WIDTH), true, theme, ctx.glyphs);
                let top = lines.len() as u16;
                cursor = at.map(|at| Position::new(at.x, at.y + top));
                lines.extend(field_lines);
                lines.extend(sentence(
                    "Changing the home moves nothing by itself; you choose next.",
                ));
            }
            Some(Ask::HomeChoice {
                path,
                size: measured,
            }) => {
                lines.extend(sentence(&format!(
                    "Use {} as the projects home? Nothing moves unless you choose Move all projects.",
                    tilde(path, ctx)
                )));
                lines.push(match measured {
                    None => ctx.loading("Measuring the projects in the current home"),
                    Some(Ok(bytes)) => Line::raw(format!(
                        "Move all projects takes {} with it.",
                        size(*bytes)
                    )),
                    Some(Err(_)) => styled(
                        "The size of the current home is not known.",
                        Role::Muted,
                        theme,
                    ),
                });
            }
        }
        (lines, cursor)
    }

    fn body(
        &mut self,
        limit: usize,
        height: usize,
        ctx: &Ctx,
    ) -> (Vec<Line<'static>>, Option<Position>) {
        let theme = &ctx.theme;
        let glyphs = ctx.glyphs;
        let mut top = vec![styled("Setup", Role::Accent, theme)];
        let intro = if self.first_run {
            FIRST_RUN_TEXT
        } else {
            SETTINGS_TEXT
        };
        top.extend(
            wrap(intro, limit)
                .into_iter()
                .map(|part| styled(part, Role::Muted, theme)),
        );
        top.push(Line::default());
        let mut bottom = Vec::new();
        let ready = match &self.setup {
            Load::Ready(status) => Some(status.clone()),
            Load::NotReady => {
                top.extend(
                    wrap(NOT_READY_TEXT, limit)
                        .into_iter()
                        .map(|part| styled(part, Role::Muted, theme)),
                );
                None
            }
            Load::Failed(code) => {
                top.push(status(
                    &format!("Failed to check setup. {}", setup_plain(code)),
                    theme,
                    glyphs,
                ));
                top.push(Line::raw("Press r to try again."));
                None
            }
            Load::Idle | Load::Loading => {
                top.push(ctx.loading("Checking this computer"));
                None
            }
        };
        let mut rows = Vec::new();
        if let Some(setup) = &ready {
            top.extend(self.tools(setup, limit, ctx));
            top.push(Line::default());
            top.push(self.pack_heading(limit, ctx));
            bottom.extend(self.pack_notes(limit, ctx));
        }
        if !self.first_run {
            bottom.extend(self.account_rows(limit, ctx));
        }
        let (asked, at) = self.ask_rows(limit, ctx);
        let asked_from = bottom.len();
        bottom.extend(asked);
        if ready.is_some() {
            let window = height
                .saturating_sub(top.len() + bottom.len())
                .max(WINDOW_MIN);
            rows = self.pack_rows(limit, window, ctx);
        }
        let cursor =
            at.map(|at| Position::new(at.x, at.y + (top.len() + rows.len() + asked_from) as u16));
        let mut lines = top;
        lines.extend(rows);
        lines.extend(bottom);
        (lines, cursor)
    }
}

impl Screen for AppSetup {
    fn crumbs(&self) -> Vec<String> {
        if self.first_run {
            vec!["Setup".into()]
        } else {
            vec!["Settings".into(), "Setup".into()]
        }
    }

    fn hints(&self, _ctx: &Ctx) -> Vec<Hint> {
        match &self.ask {
            Some(Ask::HomePath(_)) => return vec![hint("Enter", "Next"), hint("Esc", "Cancel")],
            Some(Ask::HomeChoice { .. }) => {
                return vec![
                    hint("Enter", "Use it"),
                    hint("m", "Move all projects"),
                    hint("Esc", "Cancel"),
                ];
            }
            Some(Ask::Remove { .. }) => return vec![hint("y", "Remove"), hint("n", "Keep")],
            Some(Ask::SignOut) => return vec![hint("y", "Sign out"), hint("n", "Stay signed in")],
            None => {}
        }
        let mut hints = Vec::new();
        match &self.setup {
            Load::Ready(_) if self.first_run => {
                hints.push(hint("Enter", "Install and continue"));
                if self.quiet() {
                    let skip = if self.finished_with_problems {
                        "Continue without it"
                    } else {
                        "Continue without installing"
                    };
                    hints.push(hint("c", skip));
                }
                hints.push(hint("Space", self.space_label()));
            }
            Load::Ready(_) => {
                hints.push(hint("Space", self.space_label()));
                hints.push(hint("Enter", "Install missing"));
                hints.push(hint("d", "Remove"));
            }
            Load::NotReady if self.first_run => hints.push(hint("Enter", "Continue")),
            Load::Failed(_) => {
                hints.push(hint("r", "Try again"));
                if self.first_run {
                    hints.push(hint("c", "Continue"));
                }
            }
            _ => {}
        }
        if !self.first_run {
            hints.push(hint("h", "Change home"));
            if matches!(&self.github, Load::Ready(github) if github.signed_in) {
                hints.push(hint("g", "Sign out of GitHub"));
            }
            hints.push(hint("Esc", "Back"));
        } else {
            hints.push(hint("Ctrl-Q", "Quit"));
        }
        hints
    }

    fn typing(&self) -> bool {
        matches!(self.ask, Some(Ask::HomePath(_)))
    }

    fn key(&mut self, key: &Key, ctx: &mut Ctx) -> Flow {
        if let Some(ask) = self.ask.take() {
            self.ask_key(ask, key, ctx);
            return Flow::Handled;
        }
        let checked = matches!(self.setup, Load::Ready(_) | Load::Failed(_));
        match key {
            Key::Up => {
                self.selected = self.selected.saturating_sub(1);
                Flow::Handled
            }
            Key::Down => {
                if self.selected + 1 < self.entries().len() {
                    self.selected += 1;
                }
                Flow::Handled
            }
            Key::Char(' ') => {
                self.toggle(ctx);
                Flow::Handled
            }
            Key::Enter => self.run(ctx),
            Key::Char('r') => {
                self.reload(ctx);
                Flow::Handled
            }
            Key::Char('c') if self.first_run && self.quiet() && checked => {
                Flow::Replace(open(&Place::Home, ctx))
            }
            Key::Char('d') if !self.first_run => {
                self.ask_remove(ctx);
                Flow::Handled
            }
            Key::Char('h') if !self.first_run => {
                self.ask_home(ctx);
                Flow::Handled
            }
            Key::Char('g') if !self.first_run => {
                self.ask_sign_out(ctx);
                Flow::Handled
            }
            _ => Flow::Ignored,
        }
    }

    fn arrive(&mut self, arrival: &Arrival, ctx: &mut Ctx) -> Flow {
        if matches!(arrival, Arrival::Disconnected(_)) {
            self.reading = [false; 3];
            self.sent = None;
            return Flow::Handled;
        }
        let Some(SettingsEvent::AppSetup(event)) = settings_event(arrival) else {
            return Flow::Handled;
        };
        match event {
            AppSetupEvent::Status(result) => return self.read_setup(result, ctx),
            AppSetupEvent::Ran(result) => self.ran(result, ctx),
            AppSetupEvent::PackChanged(result) | AppSetupEvent::HomeSet(result) => {
                self.changed(result, ctx)
            }
            AppSetupEvent::GitHubSignedOut(result) => {
                let result = result.as_ref().map(|github| {
                    self.github = Load::Ready(github.clone());
                });
                self.changed(&result.map_err(Clone::clone), ctx);
            }
            AppSetupEvent::Home(result) => {
                self.reading[1] = false;
                self.home = loaded(result);
            }
            AppSetupEvent::GitHub(result) => {
                self.reading[2] = false;
                self.github = loaded(result);
            }
            AppSetupEvent::FolderSize(measured, result) => {
                let current = match &self.home {
                    Load::Ready(home) => home.path.as_deref(),
                    _ => None,
                };
                if current == Some(measured.as_str())
                    && let Some(Ask::HomeChoice { size, .. }) = &mut self.ask
                {
                    *size = Some(result.clone());
                }
            }
        }
        Flow::Handled
    }

    fn shown(&mut self, ctx: &mut Ctx) {
        self.reload(ctx);
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect, ctx: &Ctx) -> Option<Position> {
        let limit = (area.width as usize).saturating_sub(4).max(8);
        let (lines, cursor) = self.body(limit, area.height as usize, ctx);
        draw_lines(frame.buffer_mut(), area, &indent(lines, 2));
        cursor.map(|at| Position::new(area.x + 2 + at.x, area.y + at.y))
    }

    fn busy(&self, _ctx: &Ctx) -> bool {
        self.started
            || self.sent.is_some()
            || matches!(self.setup, Load::Loading)
            || matches!(self.home, Load::Loading)
            || matches!(self.github, Load::Loading)
            || matches!(self.ask, Some(Ask::HomeChoice { size: None, .. }))
    }

    fn place(&self) -> Option<Place> {
        Some(Place::Setup {
            first_run: self.first_run,
        })
    }
}
