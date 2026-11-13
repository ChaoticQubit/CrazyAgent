use super::app_setup::{setup_plain, setup_reason};
use super::harness_setup::{
    CANT_RUN, INSTALL_NODE, INSTALLING_NODE, NodeOffer, TRY_AGAIN, node_needed, node_offer,
};
use super::project_markdown::sanitize;
use super::settings::{draw_page, step};
use super::settings_accounts::{plain, send, settings_event};
use super::{Flow, Place, Screen, SettingsSection};
use crate::harnesses::finding::{self, Found};
use crate::harnesses::update::{HarnessVersions, Latest, Method, UpdateMode, UpdateSettings};
use crate::harnesses::{AccountKind, HarnessEntry, HarnessStatus, ToolControl};
use crate::setup::{Dependency, DependencyState, SetupStatus};
use crate::tui::client::app_setup::{AppSetupEvent, AppSetupJob};
use crate::tui::client::settings::{SettingsEvent, SettingsJob};
use crate::tui::ctx::{Ctx, Load};
use crate::tui::frame::{Hint, Message, Modal, hint, sections_hint};
use crate::tui::keys::Key;
use crate::tui::seam::Arrival;
use crate::tui::text::{
    cells, cut, join, list_row, pad, restyle, row, status, styled, thumb, width, with_bar, wrap,
};
use crate::tui::theme::Role;
use crate::tui::widgets::TextField;
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
const NODE_POLLS_MAX: usize = 240;
const UNKNOWN_INSTALL: &str = "harness_update_unknown_install";
const LOOKUP_FAILED: &str = "harness_update_lookup_failed";
const OUTCOME_UNKNOWN: &str = "The update ended in a way CrazyAgent doesn't know.";
const NOT_FOUND: &str = "harness_install_not_found";
const LOOKED_SHOWN_MAX: usize = 20;
const SHIMS: &str = "Volta, asdf and mise folders hold shims that start another program, so CrazyAgent doesn't use them.";
const PATH_COPY: [(&str, &str); 13] = [
    (
        finding::UNKNOWN_HARNESS,
        "You can only choose the file for Codex and Claude Code.",
    ),
    (finding::PATH_INVALID, "That isn't a full path to one file."),
    (finding::PATH_NOT_FOUND, "There's no file there."),
    (
        finding::PATH_NOT_EXECUTABLE,
        "That file isn't a program this Mac can run.",
    ),
    (
        finding::PATH_AGENT_SPACE,
        "That file is in CrazyAgent's own folders, where agents work, so CrazyAgent won't run it.",
    ),
    (
        finding::PATH_SHIM,
        "That file is a Volta, asdf or mise shim, which starts another program. Choose the real file instead.",
    ),
    (
        finding::PATH_CHANGEABLE,
        "Another user on this Mac can change that file or a folder above it, so CrazyAgent won't run it.",
    ),
    (
        finding::PATH_UNREADABLE,
        "CrazyAgent couldn't read or save the chosen file. Try again.",
    ),
    (
        "native_auth_host_executable_unsafe",
        "Another user on this Mac can change that file, or it isn't a plain program file, so CrazyAgent won't run it.",
    ),
    (
        "native_auth_host_executable_changed",
        "That file changed while CrazyAgent checked it. Try again.",
    ),
    (
        "native_auth_host_unavailable",
        "The engine can't add a harness right now. Restart the engine, then try again.",
    ),
    (
        "native_auth_host_registration_frozen",
        "The engine is stopping, so it can't add a harness now. Try again once it has restarted.",
    ),
    ("owner_runtime_stale", "The engine restarted. Try again."),
];
const COPY: [(&str, &str); 28] = [
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
        "Another harness update or install is running.",
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
        "harness_update_check_failed",
        "The check for a newer version didn't finish.",
    ),
    (
        "harness_update_start_failed",
        "CrazyAgent couldn't start the tool that updates or installs it.",
    ),
    (
        "harness_update_settings_unreadable",
        "CrazyAgent couldn't read the harness update settings.",
    ),
    (
        "harness_update_unavailable",
        "The engine couldn't open its update records. Try again.",
    ),
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
    (
        "harness_install_needs_node",
        "CrazyAgent installs this one with npm, and Node.js isn't on this Mac yet.",
    ),
    (
        "harness_install_list_invalid",
        "CrazyAgent couldn't install that list of harnesses. Pick them again.",
    ),
];

/// What an owner-confirmed job does to a harness: update an installed one or install a
/// missing one. Both run in the engine and are read back the same way.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Job {
    Update,
    Install,
}

/// An open confirm: the harness, the job and the exact line the dialog shows, taken from the
/// versions the engine answered right before the dialog opened.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Confirm {
    id: String,
    job: Job,
    line: String,
}

/// The sentence for a refused harness file, the same as the app's. Input: the engine's code.
/// Output: the sentence, or the plain words for a code that is not about the file.
pub fn path_copy(code: &str) -> &'static str {
    PATH_COPY
        .iter()
        .find(|(known, _)| *known == code)
        .map_or_else(|| plain(code), |(_, copy)| *copy)
}

/// The file the owner looks for when choosing a harness's file. Input: the catalog id.
/// Output: `claude` for Claude Code, else the id.
fn file_name(id: &str) -> &str {
    if id == "claude-code" { "claude" } else { id }
}

/// What a look for Codex or Claude Code found, as the owner reads it under the harness.
/// Inputs: the harness name, its file name and the answer. Output: the sentence.
fn found_line(name: &str, file: &str, found: &Found) -> String {
    let shown = |path: &std::path::Path| sanitize(&path.display().to_string());
    match found {
        Found::Registered { path } | Found::Ready { path } => {
            format!("{name} is ready at {}.", shown(path.as_path()))
        }
        Found::Missing { looked } if looked.is_empty() => {
            format!("CrazyAgent didn't find {file}. Press f to choose the {file} file.")
        }
        Found::Missing { looked } => format!(
            "CrazyAgent looked for {file} in {} folders and didn't find it. Press w to see them.",
            looked.len()
        ),
        Found::Refused { path, code } => {
            format!(
                "CrazyAgent won't use {}. {}",
                shown(path.as_path()),
                path_copy(code)
            )
        }
    }
}

/// The line a job would run for a harness. Inputs: the versions and the job. Output: the
/// line, or None when the engine offers none.
fn job_line(versions: &HarnessVersions, job: Job) -> Option<&str> {
    match job {
        Job::Update => versions.command.as_deref(),
        Job::Install => versions.install.as_deref(),
    }
}

/// The lead of the message that says a job was not started. Inputs: the job and the harness
/// name. Output: the sentence.
fn cant(job: Job, name: &str) -> String {
    match job {
        Job::Update => format!("Can't update {name}."),
        Job::Install => format!("Can't install {name}."),
    }
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
            Some(to) => {
                format!("Version {to} is installed now. New agents use it; no restart needed.")
            }
            None => "It is installed now. New agents use it; no restart needed.".to_owned(),
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

/// The resolver's line for a found harness it can't run, made safe to show, the first-run
/// step's line: the versions read's, else the catalog's until that read arrives; a running
/// update or install wins, as on the first-run step. Inputs: the catalog entry and its
/// versions, none while unread. Output: the line, or none when the harness is missing, busy
/// or runs.
fn needs_line(entry: &HarnessEntry, versions: Option<&HarnessVersions>) -> Option<String> {
    entry.executable.as_ref()?;
    let line = match (versions, &entry.status) {
        (Some(versions), _) if versions.running => return None,
        (Some(versions), _) => &versions.needs.as_ref()?.line,
        (None, HarnessStatus::CantRun { reason }) => reason,
        (None, _) => return None,
    };
    Some(sanitize(line))
}

/// The Status cell word of a harness row: Can't run, the first-run step's word, for a found
/// harness the resolver can't run, Ready when a read newer than the catalog's Can't run says
/// it runs, else the catalog's word. Inputs: the catalog entry and its versions. Output: the
/// word.
fn row_word(entry: &HarnessEntry, versions: Option<&HarnessVersions>) -> &'static str {
    if needs_line(entry, versions).is_some() {
        return CANT_RUN;
    }
    if matches!(entry.status, HarnessStatus::CantRun { .. }) && versions.is_some() {
        return "Ready";
    }
    status_word(entry)
}

/// The Status cell word of a harness.
/// Input: the catalog entry. Output: "Ready", "Not installed", "Can't start" for one that is
/// installed but has no CrazyAgent launcher, or "Can't run" for one the resolver can't start.
pub fn status_word(entry: &HarnessEntry) -> &'static str {
    match entry.status {
        HarnessStatus::Ready => "Ready",
        HarnessStatus::NotInstalled => "Not installed",
        HarnessStatus::Limited { .. } => "Can't start",
        HarnessStatus::CantRun { .. } => CANT_RUN,
    }
}

/// The Control cell word of a harness.
/// Input: the catalog entry. Output: "Full control", "Limited control" or
/// "Unknown" while it is not installed or can't run and nothing limits it.
pub fn control_word(entry: &HarnessEntry) -> &'static str {
    match (&entry.status, &entry.tools) {
        (HarnessStatus::Limited { .. }, _) | (_, ToolControl::Limited { .. }) => "Limited control",
        (HarnessStatus::Ready, ToolControl::Switchable) => "Full control",
        (HarnessStatus::NotInstalled | HarnessStatus::CantRun { .. }, ToolControl::Switchable) => {
            "Unknown"
        }
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
    confirm: Option<Confirm>,
    wants: Option<(String, Job)>,
    running: Option<(String, usize)>,
    found: BTreeMap<String, Found>,
    finding: BTreeSet<String>,
    choosing: Option<(String, TextField)>,
    looked: Option<String>,
    node: Option<DependencyState>,
    setup_busy: bool,
    setup_asked: bool,
    sending_node: bool,
    node_polls: usize,
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
            found: BTreeMap::new(),
            finding: BTreeSet::new(),
            choosing: None,
            looked: None,
            node: None,
            setup_busy: false,
            setup_asked: false,
            sending_node: false,
            node_polls: 0,
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
        self.entries.get(self.selected).filter(|entry| {
            entry.executable.is_some() && needs_line(entry, self.versions.get(&entry.id)).is_none()
        })
    }

    fn blocked_selected(&self) -> bool {
        self.entries
            .get(self.selected)
            .is_some_and(|entry| needs_line(entry, self.versions.get(&entry.id)).is_some())
    }

    /// Reads again, with no lookup, every harness whose last read the test keeps: Try again
    /// reads the found ones the resolver can't run, a settled Node.js install the ones that
    /// needed Node. Inputs: the test and the context. Output: none; each reply updates its row.
    fn read_where(
        &mut self,
        keep: impl Fn(&HarnessEntry, &HarnessVersions) -> bool,
        ctx: &mut Ctx,
    ) {
        let ids: Vec<String> = self
            .entries
            .iter()
            .filter(|entry| {
                self.versions
                    .get(&entry.id)
                    .is_some_and(|versions| keep(entry, versions))
            })
            .map(|entry| entry.id.clone())
            .collect();
        assert!(ids.len() <= self.entries.len());
        for id in ids {
            self.ask_versions(&id, false, ctx);
        }
    }

    /// Where the page's one Node.js install stands, the first-run step's kinds, from the
    /// harnesses read so far and setup's Node row. Output: the offer.
    fn node_offer(&self) -> NodeOffer {
        let needed = self.versions.values().any(node_needed);
        node_offer(needed, self.node.as_ref(), self.setup_busy)
    }

    fn read_setup(&mut self, ctx: &mut Ctx) {
        if self.setup_asked {
            return;
        }
        self.setup_asked = true;
        send(ctx, SettingsJob::AppSetup(AppSetupJob::Status));
    }

    fn install_node(&mut self, ctx: &mut Ctx) {
        let wanted = matches!(
            self.node_offer(),
            NodeOffer::Offer | NodeOffer::Failed { .. }
        );
        if !wanted || self.sending_node {
            return;
        }
        self.sending_node = true;
        let generation = ctx.next_generation();
        send(
            ctx,
            SettingsJob::AppSetup(AppSetupJob::Install {
                name: Dependency::Node,
                generation,
            }),
        );
    }

    /// Takes setup's Node row; once a Node.js install the page waited on settles, the harnesses
    /// that needed Node and the catalog are read again, so the rows, the catalog's Can't run
    /// and the Home count follow. A failed read keeps what the page knew and the next refresh
    /// reads again. Inputs: the read and the context. Output: none.
    fn arrive_setup(&mut self, result: &Result<SetupStatus, String>, ctx: &mut Ctx) {
        if !std::mem::take(&mut self.setup_asked) {
            return;
        }
        let Ok(status) = result else {
            return;
        };
        let waited = self.node_offer() == NodeOffer::Installing;
        self.setup_busy = status.running;
        self.node = status
            .dependencies
            .iter()
            .find(|tool| tool.name == Dependency::Node)
            .map(|tool| tool.state.clone());
        let settled = matches!(
            self.node,
            Some(DependencyState::Found { .. } | DependencyState::Installed { .. })
        );
        if waited && settled {
            self.read_where(|_, versions| node_needed(versions), ctx);
            self.reload(ctx);
        }
    }

    fn arrive_node(&mut self, result: &Result<(), String>, ctx: &mut Ctx) {
        if !std::mem::take(&mut self.sending_node) {
            return;
        }
        match result {
            Ok(()) => {
                self.node = Some(DependencyState::Installing);
                self.node_polls = 0;
                self.read_setup(ctx);
            }
            Err(code) => ctx.say(Message::error(
                "Could not install Node.js.",
                &setup_plain(code),
            )),
        }
    }

    /// The page's Node.js lines under the list, the first-run step's words: the install being
    /// sent, the install running, or setup's reason it failed. Inputs: the width and context.
    /// Output: the lines, none while nothing needs Node.
    fn node_lines(&self, limit: usize, ctx: &Ctx) -> Vec<Line<'static>> {
        if self.sending_node {
            return vec![ctx.loading(INSTALL_NODE)];
        }
        match self.node_offer() {
            NodeOffer::Installing => vec![Line::raw(INSTALLING_NODE)],
            NodeOffer::Failed { reason } => wrap(&setup_reason(&reason), limit)
                .into_iter()
                .map(|part| styled(part, Role::Error, &ctx.theme))
                .collect(),
            NodeOffer::None | NodeOffer::Offer => Vec::new(),
        }
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
        let off = settings.mode == UpdateMode::Off;
        let reads: Vec<(String, bool)> = self
            .entries
            .iter()
            .map(|entry| (entry.id.clone(), look && !off && entry.executable.is_some()))
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

    /// The selected harness when it is Codex or Claude Code and not installed, the two whose
    /// file the owner may choose. Output: its catalog id, or none.
    fn choosable(&self) -> Option<String> {
        self.entries
            .get(self.selected)
            .filter(|entry| {
                entry.executable.is_none() && finding::CHOOSABLE.contains(&entry.id.as_str())
            })
            .map(|entry| entry.id.clone())
    }

    /// Asks the engine to look for the selected Codex or Claude Code again; the reply lists
    /// where it looked or names the refused file. Input: the context. Output: none.
    fn find(&mut self, ctx: &mut Ctx) {
        let Some(id) = self.choosable() else {
            return;
        };
        if self.finding.insert(id.clone()) {
            assert!(self.finding.len() <= finding::CHOOSABLE.len());
            send(ctx, SettingsJob::HarnessFind { id });
        }
    }

    /// Opens the path field for the selected Codex or Claude Code. Input: none. Output: none.
    fn open_choose(&mut self) {
        let Some(id) = self.choosable() else {
            return;
        };
        let file = file_name(&id).to_owned();
        let field = TextField::new(&format!("The {file} file"), finding::PATH_BYTES_MAX)
            .placeholder(&format!("/opt/homebrew/bin/{file}"));
        self.choosing = Some((id, field));
    }

    /// Edits the path field: Enter sends the path for the engine to check and pin, Esc
    /// closes it. Inputs: the key and the context. Output: none.
    fn chose(&mut self, key: &Key, ctx: &mut Ctx) {
        let Some((id, field)) = self.choosing.as_mut() else {
            return;
        };
        match key {
            Key::Esc => self.choosing = None,
            Key::Enter => {
                let path = field.value().trim().to_owned();
                if path.is_empty() {
                    return;
                }
                let id = id.clone();
                self.choosing = None;
                send(
                    ctx,
                    SettingsJob::HarnessChoosePath {
                        id,
                        path: Some(path),
                    },
                );
            }
            other => {
                field.key(other);
            }
        }
    }

    /// Clears the file chosen for the selected harness after the engine refused it.
    /// Input: the context. Output: none; the reply shows what the engine finds now.
    fn forget(&mut self, ctx: &mut Ctx) {
        let Some(id) = self.choosable() else {
            return;
        };
        if matches!(self.found.get(&id), Some(Found::Refused { .. })) {
            send(ctx, SettingsJob::HarnessChoosePath { id, path: None });
        }
    }

    /// Takes the engine's answer to a look or a chosen file: keeps it for the detail, opens
    /// the "where we looked" list after w, reloads the list once the harness is pinned, and
    /// says why a chosen file was refused. Inputs: the id, the path sent (none for a look,
    /// `Some(None)` to forget), the answer and the context. Output: none.
    fn arrive_found(
        &mut self,
        id: &str,
        sent: Option<&Option<String>>,
        result: &Result<Found, String>,
        ctx: &mut Ctx,
    ) {
        self.finding.remove(id);
        let found = match result {
            Ok(found) => found,
            Err(code) => {
                let lead = match sent {
                    Some(Some(path)) => format!("CrazyAgent won't use {}.", sanitize(path)),
                    Some(None) => "Couldn't forget the chosen file.".to_owned(),
                    None => format!("Couldn't look for {} again.", self.name_of(id)),
                };
                ctx.say(Message::error(&lead, path_copy(code)));
                return;
            }
        };
        if sent.is_none() && matches!(found, Found::Missing { .. }) {
            self.looked = Some(id.to_owned());
        }
        let pinned = matches!(found, Found::Registered { .. } | Found::Ready { .. });
        self.found.insert(id.to_owned(), found.clone());
        assert!(self.found.len() <= finding::CHOOSABLE.len());
        if pinned {
            self.reload(ctx);
        }
    }

    fn name_of(&self, id: &str) -> String {
        self.entries
            .iter()
            .find(|entry| entry.id == id)
            .map_or(id, |entry| entry.name.as_str())
            .to_owned()
    }

    /// Says why no job can start while one runs, naming the harness that is busy: the engine
    /// runs one update or install at a time. Inputs: the lead sentence and the context.
    /// Output: whether a job runs, in which case the owner was told.
    fn job_runs(&self, lead: &str, ctx: &mut Ctx) -> bool {
        let Some((running, _)) = &self.running else {
            return false;
        };
        let name = self.name_of(running);
        ctx.say(Message::error(
            lead,
            &format!("{name} is being updated or installed. Wait until that ends."),
        ));
        true
    }

    /// Reads a harness again before its confirm opens, so the dialog shows the line the
    /// engine has now and not the one read when the page opened. Inputs: the harness id, the
    /// job and the context. Output: none; the reply opens the confirm.
    fn ask_for(&mut self, id: String, job: Job, ctx: &mut Ctx) {
        self.ask_versions(&id, false, ctx);
        self.wants = Some((id, job));
    }

    /// Opens the confirm for the job the owner asked for on a harness, from the versions
    /// just read, or says why the job is not offered. Inputs: the harness id, the job, its
    /// versions and the context. Output: none.
    fn offer(&mut self, id: &str, job: Job, versions: &HarnessVersions, ctx: &mut Ctx) {
        let lead = cant(job, &self.name_of(id));
        if self.job_runs(&lead, ctx) {
            return;
        }
        if let Some(line) = job_line(versions, job) {
            self.confirm = Some(Confirm {
                id: id.to_owned(),
                job,
                line: line.to_owned(),
            });
            return;
        }
        let unknown = "harness_update_outcome_unknown";
        let why = match job {
            Job::Update => refusal(versions).unwrap_or_else(|| update_copy(unknown).to_owned()),
            Job::Install => update_copy(versions.refusal.as_deref().unwrap_or(unknown)).to_owned(),
        };
        ctx.say(Message::error(&lead, &why));
    }

    fn update(&mut self, ctx: &mut Ctx) {
        let Some(entry) = self.chosen() else {
            return;
        };
        let (id, name) = (entry.id.clone(), entry.name.clone());
        let lead = cant(Job::Update, &name);
        let blocked = match self.mode() {
            UpdateMode::Off => Some("harness_update_off"),
            UpdateMode::CheckOnly => Some("harness_update_check_only"),
            UpdateMode::CheckAndUpdate => None,
        };
        if let Some(code) = blocked {
            ctx.say(Message::error(&lead, update_copy(code)));
            return;
        }
        if self.job_runs(&lead, ctx) {
            return;
        }
        self.ask_for(id, Job::Update, ctx);
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
        if self.job_runs(&cant(Job::Install, &name), ctx) {
            return;
        }
        self.ask_for(id, Job::Install, ctx);
    }

    fn confirmed(&mut self, key: &Key, ctx: &mut Ctx) {
        let Some(Confirm { id, job, .. }) = self.confirm.take() else {
            return;
        };
        if *key != Key::Char('y') {
            return;
        }
        if self.job_runs(&cant(job, &self.name_of(&id)), ctx) {
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
        if !self.entries.iter().any(|entry| entry.id == id) {
            return;
        }
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
                let lead = if self.mode() == UpdateMode::Off {
                    format!("Could not read {}.", self.name_of(id))
                } else {
                    "Could not check for updates.".to_owned()
                };
                ctx.say(Message::error(&lead, plain_update(code)));
                return;
            }
        };
        self.versions.insert(id.to_owned(), versions.clone());
        assert!(self.versions.len() <= self.entries.len());
        if self.node.is_none() && node_needed(versions) {
            self.read_setup(ctx);
        }
        let stale = self.confirm.take_if(|open| {
            open.id == id
                && (versions.running || job_line(versions, open.job) != Some(open.line.as_str()))
        });
        if stale.is_some() {
            let name = self.name_of(id);
            ctx.say(Message::error(
                &format!("{name} changed while you were asked."),
                "Press the key again to see what CrazyAgent would run now.",
            ));
        }
        if versions.running && self.running.is_none() {
            self.running = Some((id.to_owned(), 0));
        }
        let running = self
            .running
            .as_ref()
            .is_some_and(|(running, _)| running == id);
        if running && !versions.running {
            self.running = None;
            self.reload(ctx);
        }
        if let Some(job) = wanted {
            self.offer(id, job, versions, ctx);
        }
    }

    fn arrive_started(&mut self, id: &str, job: Job, result: &Result<(), String>, ctx: &mut Ctx) {
        let name = self.name_of(id);
        match result {
            Ok(()) => self.poll(ctx),
            Err(code) => {
                if self
                    .running
                    .as_ref()
                    .is_some_and(|(running, _)| running == id)
                {
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
            .saturating_sub(CHROME_ROWS + head + 1 + DETAIL_ROWS + 1 + ctx.extra_hint_rows())
            .saturating_sub(self.node_lines(limit, ctx).len())
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
        found: Option<&Found>,
        off: bool,
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
        let needs = needs_line(entry, versions);
        let head = if matches!(entry.status, HarnessStatus::Ready) && needs.is_none() {
            name
        } else {
            let room = (width(&entry.name) + 2).max(LABEL_WIDTH);
            join([
                pad(name, room, glyphs),
                status(row_word(entry, versions), theme, glyphs),
            ])
        };
        let mut lines = vec![head];
        if let Some(line) = &needs {
            lines.extend(
                wrap(line, limit.max(8))
                    .into_iter()
                    .map(|part| styled(part, Role::Error, theme)),
            );
        }
        if let Some(found) = found.filter(|_| entry.executable.is_none()) {
            let line = found_line(&entry.name, file_name(&entry.id), found);
            lines.extend(wrap(&line, limit.max(8)).into_iter().map(Line::raw));
        }
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
                if let Some(versions) = versions.filter(|_| needs.is_none() && !off) {
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
                    if let Some(guide) = versions
                        .guide
                        .as_deref()
                        .filter(|_| refusal(versions).is_some())
                    {
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
                        if let Some(guide) = versions
                            .guide
                            .as_deref()
                            .filter(|_| versions.install.is_none())
                        {
                            let room = limit.saturating_sub(LABEL_WIDTH).max(8);
                            lines
                                .push(label("Guide", cut(&sanitize(guide), room, glyphs.ellipsis)));
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
                    status(row_word(entry, self.versions.get(&entry.id)), theme, glyphs),
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
        lines.extend(self.node_lines(limit, ctx));
        if let Some(entry) = self.entries.get(selected) {
            lines.push(Line::default());
            lines.extend(Self::detail(
                entry,
                self.versions.get(&entry.id),
                self.found.get(&entry.id),
                self.mode() == UpdateMode::Off,
                limit,
                ctx,
            ));
        }
        self.selected = selected;
        self.offset = offset;
        lines
    }

    /// The path field for choosing a harness's file. Inputs: the id, the field and the
    /// context. Output: the modal and its hints.
    fn choose_modal(&self, id: &str, field: &TextField, ctx: &Ctx) -> (Modal, Vec<Hint>) {
        let file = file_name(id);
        let room = usize::from(MODAL_WIDTH) - 4;
        let intro = format!(
            "Type or paste the full path of the {file} file. CrazyAgent checks it before it uses it."
        );
        let mut body: Vec<Line<'static>> = wrap(&intro, room).into_iter().map(Line::raw).collect();
        body.push(Line::default());
        body.extend(field.lines(room, true, &ctx.theme, ctx.glyphs).0);
        let modal = Modal {
            width: MODAL_WIDTH,
            title: format!("Choose the {file} file"),
            body,
            top: None,
        };
        (modal, vec![hint("Enter", "Choose"), hint("Esc", "Cancel")])
    }

    /// The folders the engine looked in for a harness, at most [`LOOKED_SHOWN_MAX`] of them
    /// with a count of the rest. Inputs: the id and the context. Output: the modal and hints.
    fn looked_modal(&self, id: &str, ctx: &Ctx) -> (Modal, Vec<Hint>) {
        let file = file_name(id);
        let room = usize::from(MODAL_WIDTH) - 4;
        let looked: &[String] = match self.found.get(id) {
            Some(Found::Missing { looked }) => looked,
            _ => &[],
        };
        let mut body: Vec<Line<'static>> = wrap(
            &format!("CrazyAgent looked for {file} in these folders and didn't find it:"),
            room,
        )
        .into_iter()
        .map(Line::raw)
        .collect();
        for folder in looked.iter().take(LOOKED_SHOWN_MAX) {
            body.push(styled(
                cut(&sanitize(folder), room, ctx.glyphs.ellipsis),
                Role::Accent,
                &ctx.theme,
            ));
        }
        if looked.len() > LOOKED_SHOWN_MAX {
            body.push(Line::raw(format!(
                "and {} more.",
                looked.len() - LOOKED_SHOWN_MAX
            )));
        }
        body.push(Line::default());
        body.extend(wrap(SHIMS, room).into_iter().map(Line::raw));
        let modal = Modal {
            width: MODAL_WIDTH,
            title: format!("Where CrazyAgent looked for {file}"),
            body,
            top: None,
        };
        (modal, vec![hint("Any key", "Close")])
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
    fn wraps_hints(&self) -> bool {
        true
    }

    fn crumbs(&self) -> Vec<String> {
        vec!["Settings".into(), "Harnesses".into()]
    }

    fn hints(&self, ctx: &Ctx) -> Vec<Hint> {
        let arrows = format!("{}{}", ctx.glyphs.up, ctx.glyphs.down);
        let again = if self.blocked_selected() {
            TRY_AGAIN
        } else {
            "Check again"
        };
        let mut hints = vec![hint(&arrows, "Move"), hint("r", again)];
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
        if let Some(id) = self.choosable() {
            hints.push(hint("f", "Choose file"));
            hints.push(hint("w", "Where it looked"));
            if matches!(self.found.get(&id), Some(Found::Refused { .. })) {
                hints.push(hint("x", "Forget the chosen file"));
            }
        }
        if self.chosen().is_some() && mode != UpdateMode::Off {
            hints.push(hint("c", "Check for updates"));
            if mode == UpdateMode::CheckAndUpdate {
                hints.push(hint("u", "Update"));
            }
        }
        let offered = matches!(
            self.node_offer(),
            NodeOffer::Offer | NodeOffer::Failed { .. }
        );
        if offered && !self.sending_node {
            hints.push(hint("n", INSTALL_NODE));
        }
        hints.extend([sections_hint(ctx.glyphs), hint("Esc", "Home")]);
        hints
    }

    fn key(&mut self, key: &Key, ctx: &mut Ctx) -> Flow {
        if self.confirm.is_some() {
            self.confirmed(key, ctx);
            return Flow::Handled;
        }
        if self.choosing.is_some() {
            self.chose(key, ctx);
            return Flow::Handled;
        }
        if self.looked.take().is_some() {
            return Flow::Handled;
        }
        let page = self.window(ctx).saturating_sub(1).max(1) as isize;
        let choosable = self.choosable().is_some();
        match key {
            Key::Char('r') => {
                self.found.clear();
                self.reload(ctx);
                self.read_where(
                    |entry, versions| needs_line(entry, Some(versions)).is_some(),
                    ctx,
                );
            }
            Key::Char('n') => self.install_node(ctx),
            Key::Char('c') => self.check(ctx),
            Key::Char('u') => self.update(ctx),
            Key::Char('i') => self.install(ctx),
            Key::Char('f') if choosable => self.open_choose(),
            Key::Char('w') if choosable => self.find(ctx),
            Key::Char('x') if choosable => self.forget(ctx),
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
            Some(SettingsEvent::HarnessFound(id, result)) => {
                self.arrive_found(id, None, result, ctx);
                return Flow::Handled;
            }
            Some(SettingsEvent::HarnessChosen(id, path, result)) => {
                self.arrive_found(id, Some(path), result, ctx);
                return Flow::Handled;
            }
            Some(SettingsEvent::AppSetup(AppSetupEvent::Status(result))) => {
                self.arrive_setup(result, ctx);
                return Flow::Handled;
            }
            Some(SettingsEvent::AppSetup(AppSetupEvent::Installed(result))) => {
                self.arrive_node(result, ctx);
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
                self.versions
                    .retain(|id, _| entries.iter().any(|entry| entry.id == *id));
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
        let waiting = self.node_offer() == NodeOffer::Installing;
        if waiting && !self.setup_asked && self.node_polls < NODE_POLLS_MAX {
            self.node_polls += 1;
            self.read_setup(ctx);
        }
        assert!(self.node_polls <= NODE_POLLS_MAX);
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect, ctx: &Ctx) -> Option<Position> {
        let limit = (area.width as usize).saturating_sub(4).max(8);
        let content = self.body(limit, ctx);
        draw_page(frame, area, SettingsSection::Harnesses, content, None, ctx)
    }

    fn modal(&self, ctx: &Ctx) -> Option<(Modal, Vec<Hint>)> {
        if let Some((id, field)) = &self.choosing {
            return Some(self.choose_modal(id, field, ctx));
        }
        if let Some(id) = &self.looked {
            return Some(self.looked_modal(id, ctx));
        }
        let Confirm { id, job, line } = self.confirm.as_ref()?;
        let name = self.name_of(id);
        let how = self.versions.get(id).map_or_else(String::new, |versions| {
            format!("{} ", installed_with(&versions.method))
        });
        let (after, verb) = match job {
            Job::Update => (
                vec![format!(
                    "{how}When it finishes, new agents use the new version with no restart. Agents that are running keep their version until they start again."
                )],
                "update",
            ),
            Job::Install => (
                vec![
                    "It never uses sudo and never runs a script from a website.".to_owned(),
                    format!("When it finishes, new agents can use {name} with no engine restart."),
                ],
                "install",
            ),
        };
        let room = usize::from(MODAL_WIDTH) - 4;
        let title = match job {
            Job::Update => format!("Update {name}?"),
            Job::Install => format!("Install {name}?"),
        };
        let mut body = vec![Line::raw("CrazyAgent runs exactly this line:")];
        body.extend(
            wrap(&sanitize(line), room)
                .into_iter()
                .map(|part| styled(part, Role::Accent, &ctx.theme)),
        );
        body.push(Line::default());
        for sentence in &after {
            body.extend(wrap(sentence, room).into_iter().map(Line::raw));
        }
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
        self.waiting
            || !self.asking.is_empty()
            || self.running.is_some()
            || !self.finding.is_empty()
            || self.sending_node
    }

    fn place(&self) -> Option<Place> {
        Some(Place::Settings(SettingsSection::Harnesses))
    }
}

/// The plain sentence for a harness code, or for any other engine code the sentence the
/// Settings pages use. Input: the code. Output: one sentence.
pub(super) fn plain_update(code: &str) -> &'static str {
    if code.starts_with("harness_update_") || code.starts_with("harness_install_") {
        update_copy(code)
    } else {
        plain(code)
    }
}

#[cfg(all(test, unix))]
mod tests;
