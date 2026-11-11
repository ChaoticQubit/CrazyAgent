use super::project_markdown::sanitize;
use super::settings::{draw_page, step};
use super::settings_accounts::{plain, send, settings_event};
use super::{Flow, Place, Screen, SettingsSection};
use crate::harnesses::update::{HarnessVersions, Latest, Method, UpdateMode, UpdateSettings};
use crate::harnesses::{AccountKind, HarnessEntry, HarnessStatus, ToolControl};
use crate::tui::client::settings::{SettingsEvent, SettingsJob};
use crate::tui::ctx::{Ctx, Load};
use crate::tui::frame::{Hint, Message, Modal, hint, sections_hint};
use crate::tui::keys::Key;
use crate::tui::seam::Arrival;
use crate::tui::text::{
    cells, cut, join, list_row, pad, restyle, row, status, styled, thumb, width, with_bar, wrap,
};
use crate::tui::theme::Role;
use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::text::{Line, Span};
use std::collections::{BTreeMap, BTreeSet};

const LABEL_WIDTH: usize = 13;
const DETAIL_ROWS: usize = 6;
const CHROME_ROWS: usize = 4 + 1;
const SUBTITLE: &str = "Agents run inside these tools. CrazyAgent finds them on this Mac. It can install one with Homebrew or npm and update one with that harness's own updater, each only when you choose it.";
const MODAL_WIDTH: u16 = 72;
const UPDATE_POLLS_MAX: usize = 240;
const UNKNOWN_INSTALL: &str = "harness_update_unknown_install";
const LOOKUP_FAILED: &str = "harness_update_lookup_failed";
const OUTCOME_UNKNOWN: &str = "The update ended in a way CrazyAgent doesn't know.";
const NOT_FOUND: &str = "harness_install_not_found";
const COPY: [(&str, &str); 22] = [
    (
        UNKNOWN_INSTALL,
        "CrazyAgent can't tell how this was installed, so it won't update it.",
    ),
    (
        "harness_update_with_app",
        "Comes with CrazyAgent and updates with it.",
    ),
    (
        "harness_update_tool_missing",
        "The tool that updates it is no longer on this Mac.",
    ),
    (
        "harness_update_tool_unsafe",
        "The tool that updates it can be changed by another user, so CrazyAgent won't run it.",
    ),
    (
        "harness_update_needs_admin",
        "Updating it needs an administrator. Update it yourself.",
    ),
    (
        "harness_update_as_root",
        "CrazyAgent is running as the root user, so it won't run an update.",
    ),
    ("harness_update_off", "Harness updates are off in settings."),
    (
        "harness_update_check_only",
        "Harness updates are set to Check only.",
    ),
    (
        "harness_update_busy",
        "An update for this harness is already running.",
    ),
    (
        "harness_update_not_installed",
        "It isn't installed on this Mac.",
    ),
    (
        "harness_update_timeout",
        "The update took too long and was stopped.",
    ),
    ("harness_update_failed", "The update didn't finish."),
    (
        "harness_update_unsupported_here",
        "CrazyAgent can't update it on this computer.",
    ),
    (LOOKUP_FAILED, "Couldn't check for a newer version."),
    (
        "harness_update_check_unreadable",
        "Couldn't read the newer version from the vendor's site.",
    ),
    (
        "harness_update_unknown_harness",
        "CrazyAgent doesn't know this harness.",
    ),
    ("harness_update_outcome_unknown", OUTCOME_UNKNOWN),
    (
        "harness_install_no_tool",
        "CrazyAgent installs with Homebrew or npm and found no way to install this one.",
    ),
    (
        "harness_install_off",
        "Install from the app is off in settings.",
    ),
    (
        "harness_install_already",
        "It's already installed on this Mac.",
    ),
    (
        NOT_FOUND,
        "The install finished, but CrazyAgent can't find the harness. Choose Check again.",
    ),
    ("harness_install_failed", "The install didn't finish."),
];

/// What an owner-confirmed job does to a harness: update an installed one or install a
/// missing one. Both run in the engine and are read back the same way.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Job {
    Update,
    Install,
}

/// The plain sentence for an update or check code, in the same words as the desktop's
/// `updateCopy`. Input: the code. Output: one sentence, the same for no two known codes;
/// a code this build does not know reads as an outcome CrazyAgent doesn't know.
pub fn update_copy(code: &str) -> &'static str {
    COPY.iter()
        .find(|(known, _)| *known == code)
        .map_or(OUTCOME_UNKNOWN, |(_, sentence)| sentence)
}

/// The line under a harness: its version and what a check found, in the same words as the
/// desktop's `versionLine`. A failed check reads the same whatever went wrong.
/// Input: the versions. Output: one or two sentences.
pub fn version_line(versions: &HarnessVersions) -> String {
    let installed = match &versions.installed {
        Some(version) => format!("Version {version}."),
        None => "Version unknown.".to_owned(),
    };
    match &versions.latest {
        Latest::Newer { version } => format!("{installed} Version {version} is available."),
        Latest::UpToDate => format!("{installed} Up to date."),
        Latest::NoCheck => {
            format!("{installed} This install updates itself. Update asks it to look now.")
        }
        Latest::Failed { .. } => format!("{installed} {}", update_copy(LOOKUP_FAILED)),
        Latest::NotChecked => installed,
    }
}

/// How a harness was installed, as the Update confirm says it, in the desktop's words.
/// Input: the install method. Output: one sentence naming the tool and package.
pub fn installed_with(method: &Method) -> String {
    match method {
        Method::Homebrew { cask } => format!("Installed with Homebrew ({cask})."),
        Method::HomebrewFormula { formula } => format!("Installed with Homebrew ({formula})."),
        Method::Npm { package } => format!("Installed with npm ({package})."),
        Method::SelfUpdate => {
            "Installed with its own installer, which Update asks to update it.".to_owned()
        }
        Method::WithApp => update_copy("harness_update_with_app").to_owned(),
        Method::Unknown => "Installed some way CrazyAgent can't tell.".to_owned(),
    }
}

/// How the last update or install of a harness ended, in the desktop's words.
/// Inputs: the versions and the harness name. Output: the sentence, or None before any job.
fn last_line(versions: &HarnessVersions, name: &str) -> Option<String> {
    let last = versions.last.as_ref()?;
    let unknown = "an unknown version";
    Some(match (last.ok, &last.from, last.code.as_deref()) {
        (true, None, _) => match &last.to {
            Some(to) => format!("Installed version {to}. No restart needed."),
            None => "Installed. No restart needed.".to_owned(),
        },
        (true, Some(from), _) => format!(
            "Updated from {from} to {}. New agents use it now; no restart needed.",
            last.to.as_deref().unwrap_or(unknown)
        ),
        (false, _, Some(NOT_FOUND)) => {
            format!("The install finished, but CrazyAgent can't find {name}. Choose Check again.")
        }
        (false, _, code) => update_copy(code.unwrap_or("harness_update_failed")).to_owned(),
    })
}

/// The sentence under the version line of an installed harness: a running update, how the
/// last job ended, or why Update is not offered. Inputs: the versions and the harness name.
/// Output: the sentence, or None.
fn update_state(versions: &HarnessVersions, name: &str) -> Option<String> {
    if versions.running {
        return Some("Updating...".to_owned());
    }
    last_line(versions, name).or_else(|| refusal(versions))
}

/// The sentence for a harness that is not installed: a running install, how the last one
/// ended, the key that installs it, or why it can't be installed from here.
/// Inputs: the versions and the harness name. Output: the sentence.
fn install_state(versions: &HarnessVersions, name: &str) -> String {
    if versions.running {
        return "Installing...".to_owned();
    }
    if let Some(line) = last_line(versions, name) {
        return line;
    }
    match (&versions.install, versions.refusal.as_deref()) {
        (Some(_), _) => "Press i to install it; CrazyAgent shows the exact line first.".to_owned(),
        (None, Some(code)) => update_copy(code).to_owned(),
        (None, None) => "Install it yourself, then press r to check again.".to_owned(),
    }
}

fn refusal(versions: &HarnessVersions) -> Option<String> {
    if versions.method == Method::WithApp {
        return Some(update_copy("harness_update_with_app").to_owned());
    }
    if versions.command.is_some() {
        return None;
    }
    let code = versions.refusal.as_deref()?;
    let line = update_copy(code);
    Some(if code == UNKNOWN_INSTALL {
        format!("{line} Update it with its own installer.")
    } else {
        line.to_owned()
    })
}

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
    settings: Load<UpdateSettings>,
    opened: bool,
    versions: BTreeMap<String, HarnessVersions>,
    asking: BTreeSet<String>,
    confirm: Option<(String, Job)>,
    wants: Option<(String, Job)>,
    running: Option<(String, usize)>,
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
            settings: Load::Idle,
            opened: false,
            versions: BTreeMap::new(),
            asking: BTreeSet::new(),
            confirm: None,
            wants: None,
            running: None,
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

    fn mode(&self) -> UpdateMode {
        match &self.settings {
            Load::Ready(settings) => settings.mode,
            _ => UpdateMode::CheckAndUpdate,
        }
    }

    fn chosen(&self) -> Option<&HarnessEntry> {
        self.entries
            .get(self.selected)
            .filter(|entry| entry.executable.is_some())
    }

    fn ask_versions(&mut self, id: &str, look: bool, ctx: &mut Ctx) {
        if self.asking.insert(id.to_owned()) {
            assert!(self.asking.len() <= self.entries.len().max(1));
            send(
                ctx,
                SettingsJob::HarnessVersions {
                    id: id.to_owned(),
                    look,
                },
            );
        }
    }

    fn open_versions(&mut self, ctx: &mut Ctx) {
        let Load::Ready(settings) = &self.settings else {
            return;
        };
        if self.opened || !matches!(self.load, Load::Ready(())) {
            return;
        }
        self.opened = true;
        let look = settings.when == crate::harnesses::update::CheckWhen::OnOpen;
        let reads: Vec<(String, bool)> = self
            .entries
            .iter()
            .filter(|entry| entry.executable.is_none() || settings.mode != UpdateMode::Off)
            .map(|entry| (entry.id.clone(), look && entry.executable.is_some()))
            .collect();
        for (id, look) in reads {
            self.ask_versions(&id, look, ctx);
        }
    }

    fn check(&mut self, ctx: &mut Ctx) {
        let Some(entry) = self.chosen() else {
            return;
        };
        let id = entry.id.clone();
        if self.mode() == UpdateMode::Off {
            ctx.say(Message::error(
                "Can't check for updates.",
                update_copy("harness_update_off"),
            ));
            return;
        }
        self.ask_versions(&id, true, ctx);
    }

    fn update(&mut self, ctx: &mut Ctx) {
        let Some(entry) = self.chosen() else {
            return;
        };
        let (id, name) = (entry.id.clone(), entry.name.clone());
        let lead = format!("Can't update {name}.");
        let blocked = match self.mode() {
            UpdateMode::Off => Some("harness_update_off"),
            UpdateMode::CheckOnly => Some("harness_update_check_only"),
            UpdateMode::CheckAndUpdate => None,
        };
        if let Some(code) = blocked {
            ctx.say(Message::error(&lead, update_copy(code)));
            return;
        }
        if self.running.as_ref().is_some_and(|(running, _)| *running == id) {
            ctx.say(Message::error(&lead, update_copy("harness_update_busy")));
            return;
        }
        match self.versions.get(&id) {
            Some(versions) if versions.command.is_some() => self.confirm = Some((id, Job::Update)),
            Some(versions) => {
                let why = refusal(versions)
                    .unwrap_or_else(|| update_copy("harness_update_outcome_unknown").to_owned());
                ctx.say(Message::error(&lead, &why));
            }
            None => {
                self.wants = Some((id.clone(), Job::Update));
                self.ask_versions(&id, false, ctx);
            }
        }
    }

    fn install(&mut self, ctx: &mut Ctx) {
        let Some(entry) = self
            .entries
            .get(self.selected)
            .filter(|entry| entry.executable.is_none())
        else {
            return;
        };
        let (id, name) = (entry.id.clone(), entry.name.clone());
        let lead = format!("Can't install {name}.");
        if self.running.as_ref().is_some_and(|(running, _)| *running == id) {
            ctx.say(Message::error(&lead, update_copy("harness_update_busy")));
            return;
        }
        match self.versions.get(&id) {
            Some(versions) if versions.install.is_some() => {
                self.confirm = Some((id, Job::Install))
            }
            Some(versions) => {
                let code = versions
                    .refusal
                    .as_deref()
                    .unwrap_or("harness_update_outcome_unknown");
                ctx.say(Message::error(&lead, update_copy(code)));
            }
            None => {
                self.wants = Some((id.clone(), Job::Install));
                self.ask_versions(&id, false, ctx);
            }
        }
    }

    fn confirmed(&mut self, key: &Key, ctx: &mut Ctx) {
        let Some((id, job)) = self.confirm.take() else {
            return;
        };
        if *key != Key::Char('y') {
            return;
        }
        let request = match job {
            Job::Update => SettingsJob::HarnessUpdate { id: id.clone() },
            Job::Install => SettingsJob::HarnessInstall { id: id.clone() },
        };
        send(ctx, request);
        self.running = Some((id, 0));
    }

    fn poll(&mut self, ctx: &mut Ctx) {
        let Some((id, polls)) = self.running.clone() else {
            return;
        };
        if self.asking.contains(&id) {
            return;
        }
        if polls >= UPDATE_POLLS_MAX {
            self.running = None;
            return;
        }
        self.running = Some((id.clone(), polls + 1));
        self.ask_versions(&id, false, ctx);
    }

    fn arrive_versions(
        &mut self,
        id: &str,
        result: &Result<HarnessVersions, String>,
        ctx: &mut Ctx,
    ) {
        self.asking.remove(id);
        let wanted = self
            .wants
            .take_if(|(wanted, _)| wanted == id)
            .map(|(_, job)| job);
        let versions = match result {
            Ok(versions) if versions.id == id => versions,
            Ok(_) => {
                ctx.say(Message::error(
                    "Could not check for updates.",
                    update_copy("harness_update_outcome_unknown"),
                ));
                return;
            }
            Err(code) => {
                ctx.say(Message::error(
                    "Could not check for updates.",
                    plain_update(code),
                ));
                return;
            }
        };
        self.versions.insert(id.to_owned(), versions.clone());
        assert!(self.versions.len() <= self.entries.len().max(1));
        let running = self.running.as_ref().is_some_and(|(running, _)| running == id);
        if running && !versions.running {
            self.running = None;
            self.reload(ctx);
        }
        match wanted {
            Some(Job::Update) => self.update(ctx),
            Some(Job::Install) => self.install(ctx),
            None => {}
        }
    }

    fn arrive_started(
        &mut self,
        id: &str,
        job: Job,
        result: &Result<(), String>,
        ctx: &mut Ctx,
    ) {
        let name = self
            .entries
            .iter()
            .find(|entry| entry.id == id)
            .map_or(id, |entry| entry.name.as_str())
            .to_owned();
        match result {
            Ok(()) => self.poll(ctx),
            Err(code) => {
                if self.running.as_ref().is_some_and(|(running, _)| running == id) {
                    self.running = None;
                }
                let verb = match job {
                    Job::Update => "update",
                    Job::Install => "install",
                };
                ctx.say(Message::error(
                    &format!("Could not {verb} {name}."),
                    plain_update(code),
                ));
            }
        }
    }

    fn installed(&self) -> usize {
        self.entries
            .iter()
            .filter(|entry| entry.executable.is_some())
            .count()
    }

    fn window(&self, ctx: &Ctx) -> usize {
        let limit = (ctx.width as usize).saturating_sub(4).max(8);
        let head = if ctx.wide() {
            3 + wrap(SUBTITLE, limit).len()
        } else {
            2
        };
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

    fn detail(
        entry: &HarnessEntry,
        versions: Option<&HarnessVersions>,
        limit: usize,
        ctx: &Ctx,
    ) -> Vec<Line<'static>> {
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
                if let Some(versions) = versions {
                    let mut text = version_line(versions);
                    if let Some(state) = update_state(versions, &entry.name) {
                        text = format!("{text} {state}");
                    }
                    for (index, part) in wrap(&text, room).into_iter().enumerate() {
                        if index == 0 {
                            lines.push(label("Updates", part));
                        } else {
                            lines.push(Line::raw(format!("{}{part}", " ".repeat(LABEL_WIDTH))));
                        }
                    }
                    if let Some(guide) = versions.guide.as_deref().filter(|_| refusal(versions).is_some()) {
                        lines.push(label("Guide", cut(&sanitize(guide), room, glyphs.ellipsis)));
                    }
                }
            }
            None => {
                lines.extend(wrap(&sentence, limit.max(8)).into_iter().map(Line::raw));
                match versions {
                    None => lines.push(Line::raw(
                        "Install it yourself, then press r to check again.",
                    )),
                    Some(versions) => {
                        let state = install_state(versions, &entry.name);
                        lines.extend(wrap(&state, limit.max(8)).into_iter().map(Line::raw));
                        if let Some(guide) = versions.guide.as_deref().filter(|_| versions.install.is_none()) {
                            let room = limit.saturating_sub(LABEL_WIDTH).max(8);
                            lines.push(label("Guide", cut(&sanitize(guide), room, glyphs.ellipsis)));
                        }
                    }
                }
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
            lines.extend(Self::detail(entry, self.versions.get(&entry.id), limit, ctx));
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
        let mut hints = vec![hint(&arrows, "Move"), hint("r", "Check again")];
        let mode = self.mode();
        let missing = self
            .entries
            .get(self.selected)
            .filter(|entry| entry.executable.is_none());
        if let Some(entry) = missing {
            let offered = self
                .versions
                .get(&entry.id)
                .is_none_or(|versions| versions.install.is_some());
            if offered {
                hints.push(hint("i", "Install"));
            }
        }
        if self.chosen().is_some() && mode != UpdateMode::Off {
            hints.push(hint("c", "Check for updates"));
            if mode == UpdateMode::CheckAndUpdate {
                hints.push(hint("u", "Update"));
            }
        }
        hints.extend([sections_hint(ctx.glyphs), hint("Esc", "Home")]);
        hints
    }

    fn key(&mut self, key: &Key, ctx: &mut Ctx) -> Flow {
        if self.confirm.is_some() {
            self.confirmed(key, ctx);
            return Flow::Handled;
        }
        let page = self.window(ctx).saturating_sub(1).max(1) as isize;
        match key {
            Key::Char('r') => self.reload(ctx),
            Key::Char('c') => self.check(ctx),
            Key::Char('u') => self.update(ctx),
            Key::Char('i') => self.install(ctx),
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
        let result = match settings_event(arrival) {
            Some(SettingsEvent::Harnesses(result)) => result,
            Some(SettingsEvent::HarnessVersions(id, result)) => {
                self.arrive_versions(id, result, ctx);
                return Flow::Handled;
            }
            Some(SettingsEvent::HarnessUpdated(id, result)) => {
                self.arrive_started(id, Job::Update, result, ctx);
                return Flow::Handled;
            }
            Some(SettingsEvent::HarnessInstalled(id, result)) => {
                self.arrive_started(id, Job::Install, result, ctx);
                return Flow::Handled;
            }
            Some(SettingsEvent::HarnessUpdateSettings(result)) => {
                self.settings = match result {
                    Ok(settings) => Load::Ready(*settings),
                    Err(code) => Load::Failed(code.clone()),
                };
                self.open_versions(ctx);
                return Flow::Handled;
            }
            _ => return Flow::Handled,
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
                self.open_versions(ctx);
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
        if matches!(self.settings, Load::Idle) {
            self.settings = Load::Loading;
            send(ctx, SettingsJob::HarnessUpdateSettings);
        }
        self.poll(ctx);
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect, ctx: &Ctx) -> Option<Position> {
        let limit = (area.width as usize).saturating_sub(4).max(8);
        let content = self.body(limit, ctx);
        draw_page(frame, area, SettingsSection::Harnesses, content, None, ctx)
    }

    fn modal(&self, ctx: &Ctx) -> Option<(Modal, Vec<Hint>)> {
        let (id, job) = self.confirm.as_ref()?;
        let versions = self.versions.get(id)?;
        let name = self
            .entries
            .iter()
            .find(|entry| entry.id == *id)
            .map_or(id.as_str(), |entry| entry.name.as_str());
        let (line, opening, after, verb) = match job {
            Job::Update => (
                versions.command.as_deref()?,
                "CrazyAgent runs exactly this line:".to_owned(),
                format!(
                    "{} When it finishes, new agents use the new version with no restart. Agents that are running keep their version until they start again.",
                    installed_with(&versions.method)
                ),
                "update",
            ),
            Job::Install => (
                versions.install.as_deref()?,
                "CrazyAgent runs exactly this line. It never uses sudo and never runs a script from a website.".to_owned(),
                format!("When it finishes, new agents can use {name} with no engine restart."),
                "install",
            ),
        };
        let room = usize::from(MODAL_WIDTH) - 4;
        let title = match job {
            Job::Update => format!("Update {name}?"),
            Job::Install => format!("Install {name}?"),
        };
        let mut body: Vec<Line<'static>> = wrap(&opening, room).into_iter().map(Line::raw).collect();
        body.extend(
            wrap(&sanitize(line), room)
                .into_iter()
                .map(|part| styled(part, Role::Accent, &ctx.theme)),
        );
        body.push(Line::default());
        body.extend(wrap(&after, room).into_iter().map(Line::raw));
        body.push(Line::default());
        body.extend(
            wrap(
                &format!("{title} y to {verb}, any other key to cancel."),
                room,
            )
            .into_iter()
            .map(Line::raw),
        );
        let label = match job {
            Job::Update => "Update",
            Job::Install => "Install",
        };
        Some((
            Modal {
                width: MODAL_WIDTH,
                title,
                body,
                top: None,
            },
            vec![hint("y", label), hint("Any key", "Cancel")],
        ))
    }

    fn busy(&self, _ctx: &Ctx) -> bool {
        self.waiting || !self.asking.is_empty() || self.running.is_some()
    }

    fn place(&self) -> Option<Place> {
        Some(Place::Settings(SettingsSection::Harnesses))
    }
}

fn plain_update(code: &str) -> &'static str {
    if code.starts_with("harness_update_") || code.starts_with("harness_install_") {
        update_copy(code)
    } else {
        plain(code)
    }
}

#[cfg(all(test, unix))]
mod tests;
