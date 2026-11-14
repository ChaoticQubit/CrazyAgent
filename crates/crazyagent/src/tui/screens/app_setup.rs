use super::settings_accounts::{send, settings_event};
use super::settings_plugins::tilde;
use super::{Flow, Place, Screen, open};
use crate::cleanup_settings::{CleanupMode, CleanupSettings, DAYS_MAX, DAYS_MIN};
use crate::github::{GitHubMethod, GitHubStatus};
use crate::master_frame_limit::{FrameSize, MasterFrameSettings};
use crate::master_step_limit::{MasterStepSettings, StepLimit};
use crate::master_time_limit::{MasterTimeSettings, TimeLimit};
use crate::projects_home::{PATH_BYTES_MAX, ProjectsHome, path_text};
use crate::provider_retry::{ProviderRetry, ProviderRetryView};
use crate::quit_settings::{
    HOURS_DEFAULT, HOURS_MAX, HOURS_MIN, LeftRunning, OnQuit, QuitSettings,
};
use crate::sandbox::LinuxSockets;
use crate::sandbox_settings::SandboxSettings;
use crate::setup::pack::entry_title;
use crate::setup::{DependencyState, PackEntry, PackKind, PackState, SetupStatus, known_reason};
use crate::tui::client::app_setup::{AppSetupEvent, AppSetupJob, PackChange};
use crate::tui::client::settings::{SettingsEvent, SettingsJob};
use crate::tui::ctx::{Ctx, Load};
use crate::tui::frame::{Hint, Message, draw_lines, hint};
use crate::tui::keys::Key;
use crate::tui::seam::{Arrival, NOT_READY};
use crate::tui::text::{
    cells, coded, indent, join, list_row, pad, plain_code, row, status, styled, thumb, with_bar,
    wrap,
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
const LABEL_WIDTH: usize = 17;
const RETRY_TEXT: &str = "When a model provider can't be reached: what harnesses do.";
const RETRY_HELP: &str = "New runs take the choice; runs already going keep theirs. Each harness line is proven on read-only runs.";
const FIELD_WIDTH: usize = 72;
const WINDOW_MIN: usize = 3;
const NOT_READY_TEXT: &str =
    "Not ready yet. Setup comes with the setup and default skills and plugins update.";
const FIRST_RUN_TEXT: &str = "CrazyAgent needs a few tools on this computer and starts every project with the skills and plugins below. Untick anything you don't want.";
const SETTINGS_TEXT: &str = "The tools CrazyAgent needs on this computer, the skills and plugins every project starts with, the projects home folder and your GitHub account.";
const SOCKETS_TEXT: &str = "Linux limited-network sockets: how an agent limited to its model reaches the network when CrazyAgent runs on Linux.";
const SOCKETS_CHOICES: [LinuxSockets; 3] = [
    LinuxSockets::TcpOnly,
    LinuxSockets::Refuse,
    LinuxSockets::TcpPorts,
];

const CLEANUP_TEXT: &str =
    "Finished agents: when CrazyAgent removes a finished agent's worktree on its own.";
const CLEANUP_NOTE: &str = "Only the worktree goes: the code checkout and its build files. The agent's conversations, history, logs and messages stay. An agent whose work has not landed stays, and Clean up shows why.";
const CLEANUP_CHOICES: [CleanupMode; 3] = [
    CleanupMode::AfterDays,
    CleanupMode::OnceLanded,
    CleanupMode::Manual,
];
const DAYS_TEXT: &str = "Enter a whole number of days from 1 to 365.";
const DAYS_DIGITS_MAX: usize = 3;
const QUIT_TEXT: &str = "When CrazyAgent quits: what happens to team agents that are running.";
const QUIT_CHOICES: [OnQuit; 2] = [OnQuit::KeepRunning, OnQuit::Stop];
const LEFT_TEXT: &str =
    "Left running: how long agents left running wait for CrazyAgent to open again.";
const LEFT_RUNNING_NOTE: &str = "An agent still running when the time is up ends with its terminal. Opening CrazyAgent before then takes it over. The limit also covers agents left behind when CrazyAgent did not quit cleanly.";
const LEFT_CHOICES: [LeftRunning; 2] = [LeftRunning::AfterHours, LeftRunning::Never];
const HOURS_TEXT: &str = "Enter a whole number of hours from 1 to 168.";
const STEPS_TEXT: &str = "Master step limit: the most steps a project's master takes in one turn.";
const STEPS_NOTE: &str = "A step is one round of the master's work in a turn: it uses its tools, reads the results and decides what to do next. A turn that reaches the limit stops and says: Stopped after N steps; reply with 'continue' to go on. Its edits so far are kept. The limit covers Claude and Codex masters with tools on. A master with tools off, and an rcli master, which has no tools, answers each turn in one step, so the limit never stops it.";
const TIME_TEXT: &str = "Master time limit: the longest a project's master works on one turn.";
const TIME_NOTE: &str = "A turn that reaches the limit stops and says: Stopped after N minutes; reply with 'continue' to go on. Its edits so far are kept. With no limit a turn runs until the master replies or you stop it. The limit covers every master: Claude, Codex and rcli.";
const FRAMES_TEXT: &str =
    "Master frame size limit: the largest one message the engine sends a project's master.";
const FRAMES_NOTE: &str = "A turn whose message to the master is larger than the limit stops and says: One message was larger than the master frame size limit. Raise it here, or at 1 MiB shorten the message or the instructions, then send again. The limit covers Claude and Codex masters; rcli keeps 64 KiB.";
const HOURS_DIGITS_MAX: usize = 3;

/// The title of one finished agents option, the same words as the desktop Settings.
/// Inputs: the mode and the days the first option waits. Output: the title, such as
/// "3 days after the agent finishes".
pub(in crate::tui) fn cleanup_title(mode: CleanupMode, days: u16) -> String {
    match mode {
        CleanupMode::AfterDays if days == 1 => "1 day after the agent finishes".to_owned(),
        CleanupMode::AfterDays => format!("{days} days after the agent finishes"),
        CleanupMode::OnceLanded => "Once its work lands".to_owned(),
        CleanupMode::Manual => "Only when I click Clean up".to_owned(),
    }
}

/// Reads the typed days as the engine takes them. Input: the typed text. Output: the whole
/// number of days from 1 to 365, or None for anything else.
fn cleanup_days(typed: &str) -> Option<u16> {
    whole_number(typed, DAYS_DIGITS_MAX, DAYS_MIN..=DAYS_MAX)
}

/// Reads the typed hours as the engine takes them. Input: the typed text. Output: the whole
/// number of hours from 1 to 168, or None for anything else.
fn quit_hours(typed: &str) -> Option<u16> {
    whole_number(typed, HOURS_DIGITS_MAX, HOURS_MIN..=HOURS_MAX)
}

/// Reads a typed whole number in a range. Inputs: the typed text, the most digits and the
/// range. Output: the number, or None for blank, signed, decimal or out of range text.
fn whole_number(typed: &str, digits: usize, range: std::ops::RangeInclusive<u16>) -> Option<u16> {
    let typed = typed.trim();
    if typed.is_empty() || typed.len() > digits || !typed.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    let value: u16 = typed.parse().ok()?;
    range.contains(&value).then_some(value)
}

/// The title of one quit option and what it does, the same words as the desktop Settings.
/// Input: the option. Output: the title and its sentence.
pub(in crate::tui) fn on_quit_words(on_quit: OnQuit) -> (&'static str, &'static str) {
    match on_quit {
        OnQuit::KeepRunning => (
            "Keep team agents running",
            "They go on with their work, and CrazyAgent takes them over when it opens again.",
        ),
        OnQuit::Stop => (
            "Stop team agents",
            "Each agent stops as Stop would, and its task goes back to the queue.",
        ),
    }
}

/// The title of one time limit option for agents left running, the same words as the
/// desktop Settings. Inputs: the option and the hours the first one waits. Output: the
/// title, such as "24 hours after CrazyAgent quits".
pub(in crate::tui) fn left_running_title(left_running: LeftRunning, hours: u16) -> String {
    match left_running {
        LeftRunning::AfterHours if hours == 1 => "1 hour after CrazyAgent quits".to_owned(),
        LeftRunning::AfterHours => format!("{hours} hours after CrazyAgent quits"),
        LeftRunning::Never => "No limit, until CrazyAgent opens again".to_owned(),
    }
}

/// The title of one master step limit option and what it does, the same words as the
/// desktop Settings. Input: the option. Output: the title and the sentence.
pub(in crate::tui) fn step_limit_words(limit: StepLimit) -> (&'static str, &'static str) {
    match limit {
        StepLimit::NoLimit => (
            "No limit",
            "A turn runs until the master replies or its time is up.",
        ),
        StepLimit::Steps50 => ("50 steps", "A turn stops after 50 steps."),
        StepLimit::Steps100 => ("100 steps", "A turn stops after 100 steps."),
        StepLimit::Steps200 => ("200 steps", "A turn stops after 200 steps."),
    }
}

/// The title of one master time limit option and what it does, the same words as the
/// desktop Settings. Input: the option. Output: the title and the sentence.
pub(in crate::tui) fn time_limit_words(limit: TimeLimit) -> (&'static str, &'static str) {
    match limit {
        TimeLimit::NoLimit => (
            "No limit",
            "A turn runs until the master replies or you stop it.",
        ),
        TimeLimit::Minutes10 => ("10 min", "A turn stops after 10 minutes."),
        TimeLimit::Minutes30 => ("30 min", "A turn stops after 30 minutes."),
        TimeLimit::Minutes60 => ("60 min", "A turn stops after 60 minutes."),
    }
}

/// The title of one master frame size limit option and what it is for, the same words as
/// the desktop Settings. Input: the option. Output: the title and the sentence.
pub(in crate::tui) fn frame_size_words(limit: FrameSize) -> (&'static str, &'static str) {
    match limit {
        FrameSize::Kib64 => (
            "64 KiB",
            "The engine's own message size; enough for most turns.",
        ),
        FrameSize::Kib256 => (
            "256 KiB",
            "Room for long instructions and large pasted text.",
        ),
        FrameSize::Mib1 => (
            "1 MiB",
            "The largest; for a turn that still stops at 256 KiB.",
        ),
    }
}

/// The title of one provider retry option and what it does, the same words as the desktop
/// Settings. Input: the option. Output: the title and the sentence.
pub(in crate::tui) fn provider_retry_words(choice: ProviderRetry) -> (&'static str, &'static str) {
    match choice {
        ProviderRetry::StopAtOnce => (
            "Stop at once",
            "Runs fail with the provider's error instead of retrying, where the harness allows it.",
        ),
        ProviderRetry::RetryBriefly => (
            "Retry briefly",
            "Harnesses keep their own retries, but none waits for the network without end.",
        ),
        ProviderRetry::HarnessDefault => (
            "Harness default",
            "CrazyAgent sets nothing, so each harness does what it does on its own.",
        ),
    }
}

/// The engine's line for every harness under `choice`, in the view's harness order.
/// Inputs: the view and the option. Output: the lines.
fn retry_lines(view: &ProviderRetryView, choice: ProviderRetry) -> Vec<String> {
    view.notes
        .iter()
        .filter(|note| note.option == choice)
        .map(|note| note.line.clone())
        .collect()
}

/// The plain title and explanation of one Linux sockets option, as Settings shows it.
/// Input: the option. Output: the title and the sentence naming what it allows and risks.
pub(in crate::tui) fn sockets_words(sockets: LinuxSockets) -> (&'static str, &'static str) {
    match sockets {
        LinuxSockets::TcpOnly => (
            "TCP only, names looked up over TCP (recommended)",
            "Only TCP to the model's ports; names are looked up over TCP. A harness whose resolver ignores RES_OPTIONS, such as a musl build, cannot look names up, so it does not start. Wrappers are judged by their first file, a script by its interpreter, so one that starts a musl program still starts, then cannot look names up.",
        ),
        LinuxSockets::Refuse => (
            "Do not start limited-network agents",
            "No agent limited to its model starts on Linux, so the master is unavailable there.",
        ),
        LinuxSockets::TcpPorts => (
            "TCP ports only",
            "Only the model's TCP ports are limited. Hosts and UDP are not limited: UDP, ping and other sockets reach any host.",
        ),
    }
}

/// A plain sentence for a setup page problem code.
/// Input: the code. Output: "Not ready yet." until the engine serves the
/// request, otherwise the setup, projects home or GitHub sentence for the code,
/// or a general setup sentence naming a code none of them knows.
pub fn setup_plain(code: &str) -> String {
    match code {
        NOT_READY => "Not ready yet.",
        "trusted_connection_unavailable" | "controller_disconnected" => {
            "CrazyAgent is not connected to the engine right now."
        }
        "home_path_invalid" => "Use a full folder path, such as /Users/you/Projects.",
        "cleanup_settings_invalid" => {
            "The stored finished agents setting is not one this version knows."
        }
        "quit_settings_invalid" => "The stored quitting setting is not one this version knows.",
        "master_step_settings_invalid" => {
            "The stored master step limit is not one this version knows."
        }
        "master_time_settings_invalid" => {
            "The stored master time limit is not one this version knows."
        }
        "master_frame_settings_invalid" => {
            "The stored master frame size limit is not one this version knows."
        }
        "provider_retry_invalid" => {
            "The stored provider retry setting is not one this version knows."
        }
        crate::storage::versioned::STALE_CODE => {
            "The pack changed in another window, so this was not saved. The list shows it now."
        }
        "owner_outcome_unknown_reconcile_same_identity" => {
            "CrazyAgent did not answer in time. Try again."
        }
        _ => {
            return known_reason(code)
                .or_else(|| super::intake::known(code))
                .map_or_else(
                    || coded("Something went wrong during setup.", code),
                    str::to_owned,
                );
        }
    }
    .to_owned()
}

/// The sentence for why a tool or default skill did not install.
/// Input: the reason the engine sent, which is the setup sentence it already chose.
/// Output: that sentence; a plain code instead gets the setup sentence for the code.
fn setup_reason(reason: &str) -> String {
    if plain_code(reason) {
        setup_plain(reason)
    } else {
        reason.to_owned()
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
        DependencyState::Missing { .. } => "Not installed".into(),
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
    Sockets {
        choice: usize,
    },
    Cleanup {
        choice: usize,
    },
    CleanupDays(TextField),
    QuitChoice {
        choice: usize,
    },
    QuitLimit {
        on_quit: OnQuit,
        choice: usize,
    },
    QuitHours {
        on_quit: OnQuit,
        field: TextField,
    },
    Steps {
        choice: usize,
    },
    Time {
        choice: usize,
    },
    Frames {
        choice: usize,
    },
    Retry {
        choice: usize,
    },
}

enum Sent {
    Run,
    Done(String),
}

/// The setup page: the tools CrazyAgent needs on this machine and the default
/// skills and plugins pack, with ticks. First run shows it after the projects
/// home folder: Enter installs and continues to the Accounts step, and continuing past the
/// ticked MCP servers approves them. Settings shows it with on and off,
/// remove, the projects home folder, the GitHub account, the Linux sockets
/// setting, when finished agents' worktrees go, what quitting does to team agents, the
/// master step, time and frame size limits and what harnesses do when a model provider
/// can't be reached.
pub struct AppSetup {
    first_run: bool,
    setup: Load<SetupStatus>,
    home: Load<ProjectsHome>,
    github: Load<GitHubStatus>,
    sandbox: Load<SandboxSettings>,
    cleanup: Load<CleanupSettings>,
    quit: Load<QuitSettings>,
    steps: Load<MasterStepSettings>,
    time: Load<MasterTimeSettings>,
    frames: Load<MasterFrameSettings>,
    retry: Load<ProviderRetryView>,
    chosen: BTreeMap<String, bool>,
    selected: usize,
    offset: usize,
    reading: [bool; 10],
    sent: Option<Sent>,
    started: bool,
    finished_with_problems: bool,
    ask: Option<Ask>,
}

impl AppSetup {
    /// A setup page with nothing loaded yet. Input: true during first run. Output: the
    /// screen; `shown` asks the engine for the setup status, and in Settings also for the
    /// projects home, the GitHub account, the sandbox settings, the finished agents setting,
    /// the quitting setting, the master step, time and frame size limits and the provider
    /// retry setting.
    pub fn new(first_run: bool) -> Self {
        Self {
            first_run,
            setup: Load::Idle,
            home: Load::Idle,
            github: Load::Idle,
            sandbox: Load::Idle,
            cleanup: Load::Idle,
            quit: Load::Idle,
            steps: Load::Idle,
            time: Load::Idle,
            frames: Load::Idle,
            retry: Load::Idle,
            chosen: BTreeMap::new(),
            selected: 0,
            offset: 0,
            reading: [false; 10],
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

    fn pack_version(&self) -> u64 {
        match &self.setup {
            Load::Ready(status) => status.version,
            _ => 0,
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
        if matches!(self.sandbox, Load::Idle) {
            self.sandbox = Load::Loading;
        }
        self.read(3, AppSetupJob::Sandbox, ctx);
        if matches!(self.cleanup, Load::Idle) {
            self.cleanup = Load::Loading;
        }
        self.read(4, AppSetupJob::Cleanup, ctx);
        if matches!(self.quit, Load::Idle) {
            self.quit = Load::Loading;
        }
        self.read(5, AppSetupJob::Quit, ctx);
        if matches!(self.steps, Load::Idle) {
            self.steps = Load::Loading;
        }
        self.read(6, AppSetupJob::MasterSteps, ctx);
        if matches!(self.time, Load::Idle) {
            self.time = Load::Loading;
        }
        self.read(7, AppSetupJob::MasterTime, ctx);
        if matches!(self.frames, Load::Idle) {
            self.frames = Load::Loading;
        }
        self.read(8, AppSetupJob::MasterFrames, ctx);
        if matches!(self.retry, Load::Idle) {
            self.retry = Load::Loading;
        }
        self.read(9, AppSetupJob::ProviderRetry, ctx);
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
                change: PackChange::Switch {
                    id: entry.id,
                    on,
                    version: self.pack_version(),
                },
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
            Load::NotReady if self.first_run => return Self::next_step(ctx),
            Load::Failed(_) => {
                self.reload(ctx);
                return Flow::Handled;
            }
            Load::Ready(status) => status.running,
            _ => return Flow::Handled,
        };
        if running || self.started || self.sent.is_some() {
            ctx.say(Message::warning(&setup_plain("setup_busy"), ""));
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
                    DependencyState::Missing { .. } | DependencyState::Failed { .. }
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
                return Self::next_step(ctx);
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
            Err(code) => ctx.say(Message::error(&setup_plain(code), "")),
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
            Err(code) => {
                ctx.say(Message::error(&setup_plain(code), ""));
                if code == crate::storage::versioned::STALE_CODE {
                    self.reload(ctx);
                }
            }
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

    fn ask_sockets(&mut self, ctx: &mut Ctx) {
        let Load::Ready(settings) = &self.sandbox else {
            ctx.say(Message::warning(
                "The Linux sockets setting is not known yet.",
                "",
            ));
            return;
        };
        let choice = SOCKETS_CHOICES
            .iter()
            .position(|sockets| *sockets == settings.linux_sockets)
            .expect("every option is listed");
        self.ask = Some(Ask::Sockets { choice });
    }

    fn set_sockets(&mut self, choice: usize, ctx: &mut Ctx) {
        assert!(choice < SOCKETS_CHOICES.len());
        let linux_sockets = SOCKETS_CHOICES[choice];
        if matches!(&self.sandbox, Load::Ready(settings) if settings.linux_sockets == linux_sockets)
        {
            ctx.say(Message::warning("That is already the setting.", ""));
            return;
        }
        let done = format!("Linux sockets: {}.", sockets_words(linux_sockets).0);
        let job = AppSetupJob::SetSandbox {
            settings: SandboxSettings { linux_sockets },
            generation: ctx.next_generation(),
        };
        self.change(job, Sent::Done(done), ctx);
    }

    fn ask_cleanup(&mut self, ctx: &mut Ctx) {
        let Load::Ready(settings) = &self.cleanup else {
            ctx.say(Message::warning(
                "The finished agents setting is not known yet.",
                "",
            ));
            return;
        };
        let choice = CLEANUP_CHOICES
            .iter()
            .position(|mode| *mode == settings.mode)
            .expect("every option is listed");
        self.ask = Some(Ask::Cleanup { choice });
    }

    fn pick_cleanup(&mut self, choice: usize, ctx: &mut Ctx) {
        assert!(choice < CLEANUP_CHOICES.len());
        let Load::Ready(current) = &self.cleanup else {
            ctx.say(Message::warning(
                "The finished agents setting is not known yet.",
                "",
            ));
            return;
        };
        let (mode, days) = (CLEANUP_CHOICES[choice], current.days);
        if mode != CleanupMode::AfterDays {
            self.set_cleanup(CleanupSettings { mode, days }, ctx);
            return;
        }
        let mut field =
            TextField::new("Days after the agent finishes", DAYS_DIGITS_MAX).placeholder("1");
        field.set(&days.to_string());
        self.ask = Some(Ask::CleanupDays(field));
    }

    fn submit_days(&mut self, field: TextField, ctx: &mut Ctx) {
        let Some(days) = cleanup_days(field.value()) else {
            ctx.say(Message::error(DAYS_TEXT, ""));
            self.ask = Some(Ask::CleanupDays(field));
            return;
        };
        let mode = CleanupMode::AfterDays;
        self.set_cleanup(CleanupSettings { mode, days }, ctx);
    }

    fn set_cleanup(&mut self, settings: CleanupSettings, ctx: &mut Ctx) {
        assert!((DAYS_MIN..=DAYS_MAX).contains(&settings.days));
        if matches!(&self.cleanup, Load::Ready(current) if *current == settings) {
            ctx.say(Message::warning("That is already the setting.", ""));
            return;
        }
        let title = cleanup_title(settings.mode, settings.days);
        let job = AppSetupJob::SetCleanup {
            settings,
            generation: ctx.next_generation(),
        };
        self.change(job, Sent::Done(format!("Finished agents: {title}.")), ctx);
    }

    fn cleanup_value(&self, ctx: &Ctx) -> Line<'static> {
        match &self.cleanup {
            Load::Ready(settings) => Line::raw(cleanup_title(settings.mode, settings.days)),
            Load::NotReady => styled("Not ready yet", Role::Muted, &ctx.theme),
            Load::Failed(code) => status(
                &format!("Failed to read it. {}", setup_plain(code)),
                &ctx.theme,
                ctx.glyphs,
            ),
            Load::Idle | Load::Loading => ctx.loading("Loading"),
        }
    }

    fn cleanup_rows(&self, choice: usize, limit: usize, ctx: &Ctx) -> Vec<Line<'static>> {
        let theme = &ctx.theme;
        let muted = |text: &str| -> Vec<Line<'static>> {
            wrap(text, limit)
                .into_iter()
                .map(|part| styled(part, Role::Muted, theme))
                .collect()
        };
        let days = match &self.cleanup {
            Load::Ready(settings) => settings.days,
            _ => DAYS_MIN,
        };
        let mut lines = muted(CLEANUP_TEXT);
        for (index, mode) in CLEANUP_CHOICES.iter().enumerate() {
            let mark = if index == choice { "> " } else { "  " };
            lines.push(join([
                Line::raw(mark),
                styled(cleanup_title(*mode, days), Role::Heading, theme),
            ]));
        }
        lines.extend(muted(CLEANUP_NOTE));
        lines
    }

    fn ask_quit(&mut self, ctx: &mut Ctx) {
        let Load::Ready(settings) = &self.quit else {
            ctx.say(Message::warning(
                "The quitting setting is not known yet.",
                "",
            ));
            return;
        };
        let choice = QUIT_CHOICES
            .iter()
            .position(|on_quit| *on_quit == settings.on_quit)
            .expect("every option is listed");
        self.ask = Some(Ask::QuitChoice { choice });
    }

    fn pick_quit(&mut self, choice: usize, ctx: &mut Ctx) {
        assert!(choice < QUIT_CHOICES.len());
        let Load::Ready(current) = &self.quit else {
            ctx.say(Message::warning(
                "The quitting setting is not known yet.",
                "",
            ));
            return;
        };
        let limit = LEFT_CHOICES
            .iter()
            .position(|left_running| *left_running == current.left_running)
            .expect("every option is listed");
        self.ask = Some(Ask::QuitLimit {
            on_quit: QUIT_CHOICES[choice],
            choice: limit,
        });
    }

    fn pick_limit(&mut self, on_quit: OnQuit, choice: usize, ctx: &mut Ctx) {
        assert!(choice < LEFT_CHOICES.len());
        let Load::Ready(current) = &self.quit else {
            ctx.say(Message::warning(
                "The quitting setting is not known yet.",
                "",
            ));
            return;
        };
        let (left_running, hours) = (LEFT_CHOICES[choice], current.hours);
        if left_running != LeftRunning::AfterHours {
            let settings = QuitSettings {
                on_quit,
                left_running,
                hours,
            };
            self.set_quit(settings, ctx);
            return;
        }
        let mut field = TextField::new("Hours after CrazyAgent quits", HOURS_DIGITS_MAX)
            .placeholder(&HOURS_DEFAULT.to_string());
        field.set(&hours.to_string());
        self.ask = Some(Ask::QuitHours { on_quit, field });
    }

    fn submit_hours(&mut self, on_quit: OnQuit, field: TextField, ctx: &mut Ctx) {
        let Some(hours) = quit_hours(field.value()) else {
            ctx.say(Message::error(HOURS_TEXT, ""));
            self.ask = Some(Ask::QuitHours { on_quit, field });
            return;
        };
        let settings = QuitSettings {
            on_quit,
            left_running: LeftRunning::AfterHours,
            hours,
        };
        self.set_quit(settings, ctx);
    }

    fn set_quit(&mut self, settings: QuitSettings, ctx: &mut Ctx) {
        assert!((HOURS_MIN..=HOURS_MAX).contains(&settings.hours));
        if matches!(&self.quit, Load::Ready(current) if *current == settings) {
            ctx.say(Message::warning("That is already the setting.", ""));
            return;
        }
        let done = format!(
            "When quitting: {}. Left running: {}.",
            on_quit_words(settings.on_quit).0,
            left_running_title(settings.left_running, settings.hours)
        );
        let job = AppSetupJob::SetQuit {
            settings,
            generation: ctx.next_generation(),
        };
        self.change(job, Sent::Done(done), ctx);
    }

    fn quit_value(&self, ctx: &Ctx) -> Line<'static> {
        match &self.quit {
            Load::Ready(settings) => Line::raw(on_quit_words(settings.on_quit).0),
            Load::NotReady => styled("Not ready yet", Role::Muted, &ctx.theme),
            Load::Failed(code) => status(
                &format!("Failed to read it. {}", setup_plain(code)),
                &ctx.theme,
                ctx.glyphs,
            ),
            Load::Idle | Load::Loading => ctx.loading("Loading"),
        }
    }

    fn quit_rows(&self, ask: &Ask, limit: usize, ctx: &Ctx) -> Vec<Line<'static>> {
        let theme = &ctx.theme;
        let muted = |text: &str, inset: usize| -> Vec<Line<'static>> {
            wrap(text, limit.saturating_sub(inset).max(1))
                .into_iter()
                .map(|part| styled(format!("{}{part}", " ".repeat(inset)), Role::Muted, theme))
                .collect()
        };
        let choice_line = |picked: bool, title: String| {
            let mark = if picked { "> " } else { "  " };
            join([Line::raw(mark), styled(title, Role::Heading, theme)])
        };
        let mut lines = Vec::new();
        match ask {
            Ask::QuitChoice { choice } => {
                lines.extend(muted(QUIT_TEXT, 0));
                for (index, on_quit) in QUIT_CHOICES.iter().enumerate() {
                    let (title, detail) = on_quit_words(*on_quit);
                    lines.push(choice_line(index == *choice, title.to_owned()));
                    lines.extend(muted(detail, 4));
                }
            }
            Ask::QuitLimit { choice, .. } => {
                let hours = match &self.quit {
                    Load::Ready(settings) => settings.hours,
                    _ => HOURS_DEFAULT,
                };
                lines.extend(muted(LEFT_TEXT, 0));
                for (index, left_running) in LEFT_CHOICES.iter().enumerate() {
                    let title = left_running_title(*left_running, hours);
                    lines.push(choice_line(index == *choice, title));
                }
                lines.extend(muted(LEFT_RUNNING_NOTE, 0));
            }
            _ => {}
        }
        lines
    }

    fn ask_retry(&mut self, ctx: &mut Ctx) {
        let Load::Ready(view) = &self.retry else {
            ctx.say(Message::warning(
                "The provider retry setting is not known yet.",
                "",
            ));
            return;
        };
        let choice = ProviderRetry::ALL
            .iter()
            .position(|option| *option == view.choice)
            .expect("every option is listed");
        assert!(choice < ProviderRetry::ALL.len());
        self.ask = Some(Ask::Retry { choice });
    }

    fn set_retry(&mut self, choice: usize, ctx: &mut Ctx) {
        assert!(choice < ProviderRetry::ALL.len());
        let picked = ProviderRetry::ALL[choice];
        let Load::Ready(current) = &self.retry else {
            ctx.say(Message::warning(
                "The provider retry setting is not known yet.",
                "",
            ));
            return;
        };
        if current.choice == picked {
            ctx.say(Message::warning("That is already the setting.", ""));
            return;
        }
        let done = format!("Provider retry: {}.", provider_retry_words(picked).0);
        let job = AppSetupJob::SetProviderRetry {
            choice: picked,
            generation: ctx.next_generation(),
        };
        self.change(job, Sent::Done(done), ctx);
    }

    fn retry_value(&self, ctx: &Ctx) -> Line<'static> {
        match &self.retry {
            Load::Ready(view) => Line::raw(provider_retry_words(view.choice).0),
            Load::NotReady => styled("Not ready yet", Role::Muted, &ctx.theme),
            Load::Failed(code) => status(
                &format!("Failed to read it. {}", setup_plain(code)),
                &ctx.theme,
                ctx.glyphs,
            ),
            Load::Idle | Load::Loading => ctx.loading("Loading"),
        }
    }

    fn retry_rows(&self, choice: usize, limit: usize, ctx: &Ctx) -> Vec<Line<'static>> {
        assert!(choice < ProviderRetry::ALL.len());
        let theme = &ctx.theme;
        let muted = |text: &str, inset: usize| -> Vec<Line<'static>> {
            wrap(text, limit.saturating_sub(inset).max(1))
                .into_iter()
                .map(|part| styled(format!("{}{part}", " ".repeat(inset)), Role::Muted, theme))
                .collect()
        };
        let Load::Ready(view) = &self.retry else {
            return muted(RETRY_TEXT, 0);
        };
        let mut lines = muted(RETRY_TEXT, 0);
        for (index, option) in ProviderRetry::ALL.iter().enumerate() {
            let (title, detail) = provider_retry_words(*option);
            let mark = if index == choice { "> " } else { "  " };
            let title = styled(title, Role::Heading, theme);
            lines.push(join([Line::raw(mark), title]));
            lines.extend(muted(detail, 4));
            for line in retry_lines(view, *option) {
                lines.extend(muted(&line, 4));
            }
        }
        lines.extend(muted(RETRY_HELP, 0));
        assert!(lines.len() > 2 * ProviderRetry::ALL.len());
        lines
    }

    fn ask_steps(&mut self, ctx: &mut Ctx) {
        let Load::Ready(settings) = &self.steps else {
            ctx.say(Message::warning(
                "The master step limit is not known yet.",
                "",
            ));
            return;
        };
        let choice = StepLimit::ALL
            .iter()
            .position(|limit| *limit == settings.limit)
            .expect("every option is listed");
        assert!(choice < StepLimit::ALL.len());
        self.ask = Some(Ask::Steps { choice });
    }

    fn set_steps(&mut self, choice: usize, ctx: &mut Ctx) {
        assert!(choice < StepLimit::ALL.len());
        let settings = MasterStepSettings {
            limit: StepLimit::ALL[choice],
        };
        let Load::Ready(current) = &self.steps else {
            ctx.say(Message::warning(
                "The master step limit is not known yet.",
                "",
            ));
            return;
        };
        if *current == settings {
            ctx.say(Message::warning("That is already the setting.", ""));
            return;
        }
        let done = format!("Master step limit: {}.", step_limit_words(settings.limit).0);
        let job = AppSetupJob::SetMasterSteps {
            settings,
            generation: ctx.next_generation(),
        };
        self.change(job, Sent::Done(done), ctx);
    }

    fn steps_value(&self, ctx: &Ctx) -> Line<'static> {
        match &self.steps {
            Load::Ready(settings) => Line::raw(step_limit_words(settings.limit).0),
            Load::NotReady => styled("Not ready yet", Role::Muted, &ctx.theme),
            Load::Failed(code) => status(
                &format!("Failed to read it. {}", setup_plain(code)),
                &ctx.theme,
                ctx.glyphs,
            ),
            Load::Idle | Load::Loading => ctx.loading("Loading"),
        }
    }

    fn steps_rows(&self, choice: usize, limit: usize, ctx: &Ctx) -> Vec<Line<'static>> {
        assert!(choice < StepLimit::ALL.len());
        let theme = &ctx.theme;
        let muted = |text: &str, inset: usize| -> Vec<Line<'static>> {
            wrap(text, limit.saturating_sub(inset).max(1))
                .into_iter()
                .map(|part| styled(format!("{}{part}", " ".repeat(inset)), Role::Muted, theme))
                .collect()
        };
        let mut lines = muted(STEPS_TEXT, 0);
        for (index, option) in StepLimit::ALL.iter().enumerate() {
            let (title, detail) = step_limit_words(*option);
            let mark = if index == choice { "> " } else { "  " };
            let title = styled(title, Role::Heading, theme);
            lines.push(join([Line::raw(mark), title]));
            lines.extend(muted(detail, 4));
        }
        lines.extend(muted(STEPS_NOTE, 0));
        assert!(lines.len() > 2 * StepLimit::ALL.len());
        lines
    }

    fn ask_time(&mut self, ctx: &mut Ctx) {
        let Load::Ready(settings) = &self.time else {
            ctx.say(Message::warning(
                "The master time limit is not known yet.",
                "",
            ));
            return;
        };
        let choice = TimeLimit::ALL
            .iter()
            .position(|limit| *limit == settings.limit)
            .expect("every option is listed");
        assert!(choice < TimeLimit::ALL.len());
        self.ask = Some(Ask::Time { choice });
    }

    fn set_time(&mut self, choice: usize, ctx: &mut Ctx) {
        assert!(choice < TimeLimit::ALL.len());
        let settings = MasterTimeSettings {
            limit: TimeLimit::ALL[choice],
        };
        let Load::Ready(current) = &self.time else {
            ctx.say(Message::warning(
                "The master time limit is not known yet.",
                "",
            ));
            return;
        };
        if *current == settings {
            ctx.say(Message::warning("That is already the setting.", ""));
            return;
        }
        let done = format!("Master time limit: {}.", time_limit_words(settings.limit).0);
        let job = AppSetupJob::SetMasterTime {
            settings,
            generation: ctx.next_generation(),
        };
        self.change(job, Sent::Done(done), ctx);
    }

    fn time_value(&self, ctx: &Ctx) -> Line<'static> {
        match &self.time {
            Load::Ready(settings) => Line::raw(time_limit_words(settings.limit).0),
            Load::NotReady => styled("Not ready yet", Role::Muted, &ctx.theme),
            Load::Failed(code) => status(
                &format!("Failed to read it. {}", setup_plain(code)),
                &ctx.theme,
                ctx.glyphs,
            ),
            Load::Idle | Load::Loading => ctx.loading("Loading"),
        }
    }

    fn time_rows(&self, choice: usize, limit: usize, ctx: &Ctx) -> Vec<Line<'static>> {
        assert!(choice < TimeLimit::ALL.len());
        let theme = &ctx.theme;
        let muted = |text: &str, inset: usize| -> Vec<Line<'static>> {
            wrap(text, limit.saturating_sub(inset).max(1))
                .into_iter()
                .map(|part| styled(format!("{}{part}", " ".repeat(inset)), Role::Muted, theme))
                .collect()
        };
        let mut lines = muted(TIME_TEXT, 0);
        for (index, option) in TimeLimit::ALL.iter().enumerate() {
            let (title, detail) = time_limit_words(*option);
            let mark = if index == choice { "> " } else { "  " };
            let title = styled(title, Role::Heading, theme);
            lines.push(join([Line::raw(mark), title]));
            lines.extend(muted(detail, 4));
        }
        lines.extend(muted(TIME_NOTE, 0));
        assert!(lines.len() > 2 * TimeLimit::ALL.len());
        lines
    }

    fn ask_frames(&mut self, ctx: &mut Ctx) {
        let Load::Ready(settings) = &self.frames else {
            ctx.say(Message::warning(
                "The master frame size limit is not known yet.",
                "",
            ));
            return;
        };
        let choice = FrameSize::ALL
            .iter()
            .position(|limit| *limit == settings.limit)
            .expect("every option is listed");
        assert!(choice < FrameSize::ALL.len());
        self.ask = Some(Ask::Frames { choice });
    }

    fn set_frames(&mut self, choice: usize, ctx: &mut Ctx) {
        assert!(choice < FrameSize::ALL.len());
        let settings = MasterFrameSettings {
            limit: FrameSize::ALL[choice],
        };
        let Load::Ready(current) = &self.frames else {
            ctx.say(Message::warning(
                "The master frame size limit is not known yet.",
                "",
            ));
            return;
        };
        if *current == settings {
            ctx.say(Message::warning("That is already the setting.", ""));
            return;
        }
        let done = format!(
            "Master frame size limit: {}.",
            frame_size_words(settings.limit).0
        );
        let job = AppSetupJob::SetMasterFrames {
            settings,
            generation: ctx.next_generation(),
        };
        self.change(job, Sent::Done(done), ctx);
    }

    fn frames_value(&self, ctx: &Ctx) -> Line<'static> {
        match &self.frames {
            Load::Ready(settings) => Line::raw(frame_size_words(settings.limit).0),
            Load::NotReady => styled("Not ready yet", Role::Muted, &ctx.theme),
            Load::Failed(code) => status(
                &format!("Failed to read it. {}", setup_plain(code)),
                &ctx.theme,
                ctx.glyphs,
            ),
            Load::Idle | Load::Loading => ctx.loading("Loading"),
        }
    }

    fn frames_rows(&self, choice: usize, limit: usize, ctx: &Ctx) -> Vec<Line<'static>> {
        assert!(choice < FrameSize::ALL.len());
        let theme = &ctx.theme;
        let muted = |text: &str, inset: usize| -> Vec<Line<'static>> {
            wrap(text, limit.saturating_sub(inset).max(1))
                .into_iter()
                .map(|part| styled(format!("{}{part}", " ".repeat(inset)), Role::Muted, theme))
                .collect()
        };
        let mut lines = muted(FRAMES_TEXT, 0);
        for (index, option) in FrameSize::ALL.iter().enumerate() {
            let (title, detail) = frame_size_words(*option);
            let mark = if index == choice { "> " } else { "  " };
            let title = styled(title, Role::Heading, theme);
            lines.push(join([Line::raw(mark), title]));
            lines.extend(muted(detail, 4));
        }
        lines.extend(muted(FRAMES_NOTE, 0));
        assert!(lines.len() > 2 * FrameSize::ALL.len());
        lines
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
            ctx.say(Message::error(&setup_plain("home_path_invalid"), ""));
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
                        change: PackChange::Remove {
                            id,
                            version: self.pack_version(),
                        },
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
            Ask::Sockets { choice } => match key {
                Key::Up => {
                    self.ask = Some(Ask::Sockets {
                        choice: choice.saturating_sub(1),
                    })
                }
                Key::Down => {
                    self.ask = Some(Ask::Sockets {
                        choice: (choice + 1).min(SOCKETS_CHOICES.len() - 1),
                    })
                }
                Key::Enter => self.set_sockets(choice, ctx),
                Key::Esc => {}
                _ => self.ask = Some(Ask::Sockets { choice }),
            },
            Ask::Cleanup { choice } => match key {
                Key::Up => {
                    self.ask = Some(Ask::Cleanup {
                        choice: choice.saturating_sub(1),
                    })
                }
                Key::Down => {
                    self.ask = Some(Ask::Cleanup {
                        choice: (choice + 1).min(CLEANUP_CHOICES.len() - 1),
                    })
                }
                Key::Enter => self.pick_cleanup(choice, ctx),
                Key::Esc => {}
                _ => self.ask = Some(Ask::Cleanup { choice }),
            },
            Ask::Retry { choice } => match key {
                Key::Up => {
                    self.ask = Some(Ask::Retry {
                        choice: choice.saturating_sub(1),
                    })
                }
                Key::Down => {
                    self.ask = Some(Ask::Retry {
                        choice: (choice + 1).min(ProviderRetry::ALL.len() - 1),
                    })
                }
                Key::Enter => self.set_retry(choice, ctx),
                Key::Esc => {}
                _ => self.ask = Some(Ask::Retry { choice }),
            },
            Ask::Steps { choice } => match key {
                Key::Up => {
                    self.ask = Some(Ask::Steps {
                        choice: choice.saturating_sub(1),
                    })
                }
                Key::Down => {
                    self.ask = Some(Ask::Steps {
                        choice: (choice + 1).min(StepLimit::ALL.len() - 1),
                    })
                }
                Key::Enter => self.set_steps(choice, ctx),
                Key::Esc => {}
                _ => self.ask = Some(Ask::Steps { choice }),
            },
            Ask::Time { choice } => match key {
                Key::Up => {
                    self.ask = Some(Ask::Time {
                        choice: choice.saturating_sub(1),
                    })
                }
                Key::Down => {
                    self.ask = Some(Ask::Time {
                        choice: (choice + 1).min(TimeLimit::ALL.len() - 1),
                    })
                }
                Key::Enter => self.set_time(choice, ctx),
                Key::Esc => {}
                _ => self.ask = Some(Ask::Time { choice }),
            },
            Ask::Frames { choice } => match key {
                Key::Up => {
                    self.ask = Some(Ask::Frames {
                        choice: choice.saturating_sub(1),
                    })
                }
                Key::Down => {
                    self.ask = Some(Ask::Frames {
                        choice: (choice + 1).min(FrameSize::ALL.len() - 1),
                    })
                }
                Key::Enter => self.set_frames(choice, ctx),
                Key::Esc => {}
                _ => self.ask = Some(Ask::Frames { choice }),
            },
            Ask::CleanupDays(mut field) => match key {
                Key::Esc => {}
                Key::Enter => self.submit_days(field, ctx),
                other => {
                    field.key(other);
                    self.ask = Some(Ask::CleanupDays(field));
                }
            },
            Ask::QuitChoice { choice } => match key {
                Key::Up => {
                    self.ask = Some(Ask::QuitChoice {
                        choice: choice.saturating_sub(1),
                    })
                }
                Key::Down => {
                    self.ask = Some(Ask::QuitChoice {
                        choice: (choice + 1).min(QUIT_CHOICES.len() - 1),
                    })
                }
                Key::Enter => self.pick_quit(choice, ctx),
                Key::Esc => {}
                _ => self.ask = Some(Ask::QuitChoice { choice }),
            },
            Ask::QuitLimit { on_quit, choice } => match key {
                Key::Up => {
                    self.ask = Some(Ask::QuitLimit {
                        on_quit,
                        choice: choice.saturating_sub(1),
                    })
                }
                Key::Down => {
                    self.ask = Some(Ask::QuitLimit {
                        on_quit,
                        choice: (choice + 1).min(LEFT_CHOICES.len() - 1),
                    })
                }
                Key::Enter => self.pick_limit(on_quit, choice, ctx),
                Key::Esc => {}
                _ => self.ask = Some(Ask::QuitLimit { on_quit, choice }),
            },
            Ask::QuitHours { on_quit, mut field } => match key {
                Key::Esc => {}
                Key::Enter => self.submit_hours(on_quit, field, ctx),
                other => {
                    field.key(other);
                    self.ask = Some(Ask::QuitHours { on_quit, field });
                }
            },
        }
    }

    /// Leaves first run's Setup for the Accounts step, the only way first run
    /// leaves this page. Input: the context. Output: the flow that replaces Setup.
    fn next_step(ctx: &mut Ctx) -> Flow {
        Flow::Replace(open(&Place::FirstRunAccounts, ctx))
    }

    /// True when no setup run is going and no change waits for the engine.
    /// Input: none. Output: whether first run may leave for its Accounts step.
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
            let detail = match &tool.state {
                DependencyState::Failed { reason } => Some(setup_reason(reason)),
                DependencyState::Missing { line } if !line.is_empty() => {
                    Some(super::new_project_view::printable(line.clone()))
                }
                _ => None,
            };
            if let Some(detail) = detail {
                let gap = " ".repeat(2 + TOOL_WIDTH + 2);
                lines.extend(
                    wrap(&detail, rest).into_iter().map(|part| {
                        join([Line::raw(gap.clone()), styled(part, Role::Muted, theme)])
                    }),
                );
            }
        }
        for note in &setup.notes {
            lines.extend(
                wrap(&setup_reason(note), limit.saturating_sub(2).max(1))
                    .into_iter()
                    .map(|part| join([Line::raw("  "), styled(part, Role::Warning, theme)])),
            );
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
                    &format!("{}: {}", entry_title(id), setup_reason(reason)),
                    limit,
                )
                .into_iter()
                .map(|part| styled(part, Role::Muted, theme)),
            );
        }
        for entry in self.entries() {
            if let Some(reason) = &entry.hooks_refused {
                lines.extend(
                    wrap(
                        &format!(
                            "{} hooks were not added: {}",
                            entry_title(&entry.id),
                            setup_reason(reason)
                        ),
                        limit,
                    )
                    .into_iter()
                    .map(|part| styled(part, Role::Warning, theme)),
                );
            }
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

    /// Lists the projects home, GitHub and app-wide setting rows of Settings. Takes the
    /// line width, whether each setting's explaining note goes under it, and the context.
    /// Returns the rows; a GitHub problem is listed with or without notes.
    fn account_rows(&self, limit: usize, notes: bool, ctx: &Ctx) -> Vec<Line<'static>> {
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
        let gap = " ".repeat(LABEL_WIDTH);
        let under = |text: &str| -> Vec<Line<'static>> {
            wrap(text, limit.saturating_sub(LABEL_WIDTH).max(1))
                .into_iter()
                .map(|part| join([Line::raw(gap.clone()), styled(part, Role::Muted, theme)]))
                .collect()
        };
        if let Some(problem) = problem {
            lines.extend(under(&problem));
        }
        let note =
            |text: &str| -> Vec<Line<'static>> { if notes { under(text) } else { Vec::new() } };
        let (sockets, detail) = match &self.sandbox {
            Load::Ready(settings) => {
                let (title, detail) = sockets_words(settings.linux_sockets);
                (Line::raw(title), Some(detail))
            }
            Load::NotReady => (styled("Not ready yet", Role::Muted, theme), None),
            Load::Failed(code) => (
                status(
                    &format!("Failed to read it. {}", setup_plain(code)),
                    theme,
                    glyphs,
                ),
                None,
            ),
            Load::Idle | Load::Loading => (ctx.loading("Loading"), None),
        };
        lines.push(label("Linux sockets", sockets));
        if let Some(detail) = detail {
            lines.extend(note(detail));
        }
        lines.push(label("Finished agents", self.cleanup_value(ctx)));
        if matches!(self.cleanup, Load::Ready(_)) {
            lines.extend(note(CLEANUP_NOTE));
        }
        lines.push(label("When quitting", self.quit_value(ctx)));
        if let Load::Ready(settings) = &self.quit {
            let title = left_running_title(settings.left_running, settings.hours);
            lines.push(label("Left running", Line::raw(title)));
            lines.extend(note(LEFT_RUNNING_NOTE));
        }
        lines.push(label("Master steps", self.steps_value(ctx)));
        if matches!(self.steps, Load::Ready(_)) {
            lines.extend(note(STEPS_NOTE));
        }
        lines.push(label("Master time", self.time_value(ctx)));
        if matches!(self.time, Load::Ready(_)) {
            lines.extend(note(TIME_NOTE));
        }
        lines.push(label("Master frame", self.frames_value(ctx)));
        if matches!(self.frames, Load::Ready(_)) {
            lines.extend(note(FRAMES_NOTE));
        }
        lines.push(label("Provider retry", self.retry_value(ctx)));
        if let Load::Ready(view) = &self.retry {
            for line in retry_lines(view, view.choice) {
                lines.extend(note(&line));
            }
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
            Some(Ask::Sockets { choice }) => {
                lines.extend(
                    wrap(SOCKETS_TEXT, limit)
                        .into_iter()
                        .map(|part| styled(part, Role::Muted, theme)),
                );
                for (index, sockets) in SOCKETS_CHOICES.iter().enumerate() {
                    let (title, detail) = sockets_words(*sockets);
                    let mark = if index == *choice { "> " } else { "  " };
                    lines.push(join([
                        Line::raw(mark),
                        styled(title, Role::Heading, theme),
                    ]));
                    lines.extend(
                        wrap(detail, limit.saturating_sub(4).max(1))
                            .into_iter()
                            .map(|part| styled(format!("    {part}"), Role::Muted, theme)),
                    );
                }
            }
            Some(Ask::Cleanup { choice }) => lines.extend(self.cleanup_rows(*choice, limit, ctx)),
            Some(Ask::Steps { choice }) => lines.extend(self.steps_rows(*choice, limit, ctx)),
            Some(Ask::Time { choice }) => lines.extend(self.time_rows(*choice, limit, ctx)),
            Some(Ask::Frames { choice }) => lines.extend(self.frames_rows(*choice, limit, ctx)),
            Some(Ask::Retry { choice }) => lines.extend(self.retry_rows(*choice, limit, ctx)),
            Some(Ask::CleanupDays(field)) => {
                let (field_lines, at) =
                    field.lines(limit.min(FIELD_WIDTH), true, theme, ctx.glyphs);
                let top = lines.len() as u16;
                cursor = at.map(|at| Position::new(at.x, at.y + top));
                lines.extend(field_lines);
                lines.extend(sentence(
                    "Days from 1 to 365. CrazyAgent removes a finished agent's worktree that many days after it finishes.",
                ));
            }
            Some(ask @ (Ask::QuitChoice { .. } | Ask::QuitLimit { .. })) => {
                lines.extend(self.quit_rows(ask, limit, ctx))
            }
            Some(Ask::QuitHours { field, .. }) => {
                let (field_lines, at) =
                    field.lines(limit.min(FIELD_WIDTH), true, theme, ctx.glyphs);
                let top = lines.len() as u16;
                cursor = at.map(|at| Position::new(at.x, at.y + top));
                lines.extend(field_lines);
                lines.extend(sentence(
                    "Hours from 1 to 168. An agent still running that many hours after CrazyAgent quits ends with its terminal.",
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
        let (asked, at) = self.ask_rows(limit, ctx);
        let asking = !asked.is_empty();
        let window_min = if ready.is_some() && !asking {
            WINDOW_MIN
        } else {
            0
        };
        if !self.first_run {
            let full = self.account_rows(limit, true, ctx);
            let used = top.len() + bottom.len() + asked.len() + window_min;
            if used + full.len() <= height {
                bottom.extend(full);
            } else {
                let lean = self.account_rows(limit, false, ctx);
                assert!(lean.len() <= full.len());
                bottom.extend(lean);
            }
        }
        let asked_from = bottom.len();
        bottom.extend(asked);
        if ready.is_some() {
            let window = height
                .saturating_sub(top.len() + bottom.len())
                .max(window_min);
            if window > 0 {
                rows = self.pack_rows(limit, window, ctx);
            }
        }
        let ask_top = top.len() + rows.len() + asked_from;
        let mut lines = top;
        lines.extend(rows);
        lines.extend(bottom);
        let skip = if asking {
            ask_skip(lines.len(), ask_top, height)
        } else {
            0
        };
        lines.drain(..skip);
        let cursor = at.map(|at| Position::new(at.x, at.y + (ask_top - skip) as u16));
        (lines, cursor)
    }
}

/// Counts the body lines left out above an open ask, so the ask is drawn whole at the bottom
/// of the body, or from its first line when it alone is taller than the body. Takes the
/// body's line count, the index of the ask's first line and the body height. Returns the
/// count, never past the ask's first line.
fn ask_skip(lines: usize, ask_top: usize, height: usize) -> usize {
    assert!(ask_top < lines);
    let skip = lines.saturating_sub(height).min(ask_top);
    assert!(skip <= ask_top);
    assert!(lines - skip <= height || skip == ask_top);
    skip
}

impl Screen for AppSetup {
    fn wraps_hints(&self) -> bool {
        true
    }

    fn crumbs(&self) -> Vec<String> {
        if self.first_run {
            vec!["Setup".into()]
        } else {
            vec!["Settings".into(), "Setup".into()]
        }
    }

    fn hints(&self, ctx: &Ctx) -> Vec<Hint> {
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
            Some(Ask::CleanupDays(_) | Ask::QuitHours { .. }) => {
                return vec![hint("Enter", "Save"), hint("Esc", "Cancel")];
            }
            Some(
                ask @ (Ask::Sockets { .. }
                | Ask::Cleanup { .. }
                | Ask::QuitChoice { .. }
                | Ask::QuitLimit { .. }
                | Ask::Steps { .. }
                | Ask::Time { .. }
                | Ask::Frames { .. }
                | Ask::Retry { .. }),
            ) => {
                let arrows = format!("{}{}", ctx.glyphs.up, ctx.glyphs.down);
                let enter = if matches!(ask, Ask::QuitChoice { .. }) {
                    "Next"
                } else {
                    "Save"
                };
                return vec![
                    hint(&arrows, "Choose"),
                    hint("Enter", enter),
                    hint("Esc", "Cancel"),
                ];
            }
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
            hints.push(hint("l", "Linux sockets"));
            hints.push(hint("f", "Finished agents"));
            hints.push(hint("q", "Quitting"));
            hints.push(hint("m", "Master steps"));
            hints.push(hint("t", "Master time"));
            hints.push(hint("z", "Master frame"));
            hints.push(hint("p", "Retries"));
            hints.push(hint("Esc", "Back"));
        } else {
            hints.push(hint("Ctrl-Q", "Quit"));
        }
        hints
    }

    fn typing(&self) -> bool {
        matches!(
            self.ask,
            Some(Ask::HomePath(_) | Ask::CleanupDays(_) | Ask::QuitHours { .. })
        )
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
            Key::Char('c') if self.first_run && self.quiet() && checked => Self::next_step(ctx),
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
            Key::Char('l') if !self.first_run => {
                self.ask_sockets(ctx);
                Flow::Handled
            }
            Key::Char('f') if !self.first_run => {
                self.ask_cleanup(ctx);
                Flow::Handled
            }
            Key::Char('q') if !self.first_run => {
                self.ask_quit(ctx);
                Flow::Handled
            }
            Key::Char('m') if !self.first_run => {
                self.ask_steps(ctx);
                Flow::Handled
            }
            Key::Char('t') if !self.first_run => {
                self.ask_time(ctx);
                Flow::Handled
            }
            Key::Char('z') if !self.first_run => {
                self.ask_frames(ctx);
                Flow::Handled
            }
            Key::Char('p') if !self.first_run => {
                self.ask_retry(ctx);
                Flow::Handled
            }
            _ => Flow::Ignored,
        }
    }

    fn arrive(&mut self, arrival: &Arrival, ctx: &mut Ctx) -> Flow {
        if matches!(arrival, Arrival::Disconnected(_)) {
            self.reading = [false; 10];
            self.sent = None;
            return Flow::Handled;
        }
        let Some(SettingsEvent::AppSetup(event)) = settings_event(arrival) else {
            return Flow::Handled;
        };
        match event {
            AppSetupEvent::Status(result) => return self.read_setup(result, ctx),
            AppSetupEvent::Ran(result) => self.ran(result, ctx),
            AppSetupEvent::PackChanged(result)
            | AppSetupEvent::HomeSet(result)
            | AppSetupEvent::SandboxSet(result)
            | AppSetupEvent::CleanupSet(result)
            | AppSetupEvent::QuitSet(result)
            | AppSetupEvent::MasterStepsSet(result)
            | AppSetupEvent::MasterTimeSet(result)
            | AppSetupEvent::MasterFramesSet(result)
            | AppSetupEvent::ProviderRetrySet(result) => self.changed(result, ctx),
            AppSetupEvent::Sandbox(result) => {
                self.reading[3] = false;
                self.sandbox = loaded(result);
            }
            AppSetupEvent::Cleanup(result) => {
                self.reading[4] = false;
                self.cleanup = loaded(result);
            }
            AppSetupEvent::Quit(result) => {
                self.reading[5] = false;
                self.quit = loaded(result);
            }
            AppSetupEvent::MasterSteps(result) => {
                self.reading[6] = false;
                self.steps = loaded(result);
            }
            AppSetupEvent::MasterTime(result) => {
                self.reading[7] = false;
                self.time = loaded(result);
            }
            AppSetupEvent::MasterFrames(result) => {
                self.reading[8] = false;
                self.frames = loaded(result);
            }
            AppSetupEvent::ProviderRetry(result) => {
                self.reading[9] = false;
                self.retry = loaded(result);
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
            || matches!(self.sandbox, Load::Loading)
            || matches!(self.cleanup, Load::Loading)
            || matches!(self.quit, Load::Loading)
            || matches!(self.steps, Load::Loading)
            || matches!(self.time, Load::Loading)
            || matches!(self.frames, Load::Loading)
            || matches!(self.retry, Load::Loading)
            || matches!(self.ask, Some(Ask::HomeChoice { size: None, .. }))
    }

    fn place(&self) -> Option<Place> {
        Some(Place::Setup {
            first_run: self.first_run,
        })
    }
}
