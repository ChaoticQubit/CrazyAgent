use crate::accounts::{AccountView, SignInMethod};
use crate::engine::owner::{AccountSecret, AppPluginsPage as PageReply, PluginUser};
use crate::tui::client::settings::app_plugins::{AppPluginsEvent, AppPluginsJob};
use crate::tui::client::settings::{SettingsEvent, SettingsJob};
use crate::tui::ctx::Ctx;
use crate::tui::frame::{Hint, Message, Modal, draw_lines, hint};
use crate::tui::keys::Key;
use crate::tui::screens::new_project_view::printable;
use crate::tui::screens::settings_accounts::{send, settings_event};
use crate::tui::screens::{Flow, Place, Screen};
use crate::tui::seam::Arrival;
use crate::tui::text::{cut, indent, list_row, row, styled, wrap};
use crate::tui::theme::Role;
use crate::tui::widgets::TextField;
use kernel_plugin_host::grants::{GrantState, PluginGrants};
use kernel_plugin_host::problem::Problem;
use kernel_plugin_host::usage::TurnOff;
use kernel_plugin_host::views::{
    Binding, Origin, PluginKind, PluginStatus, PluginView, Preview, RunsView, SecretView, TrustView,
};
use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::text::Line;
use std::path::PathBuf;

#[cfg(all(test, unix))]
mod tests;

#[cfg(all(test, windows))]
mod path_hint_tests;

mod sentence;
pub(in crate::tui) use sentence::sentence;

const MODAL_WIDTH: u16 = 66;
const TEXT_CHARS_MAX: usize = 300;
const KEY_CHARS_MAX: usize = 1024;
const PATH_CHARS_MAX: usize = 1024;
const USERS_SHOWN_MAX: usize = 8;
const NOTICES_MAX: usize = 64;
const INTRO: &str = "Plugins CrazyAgent itself runs, such as the embedding models search ranks with. Each one shows what it reaches before you approve it.";
#[cfg(not(windows))]
const ABSOLUTE: &str = "Type the full path of the plugin folder, starting with /.";
#[cfg(windows)]
const ABSOLUTE: &str =
    "Type the full path of the plugin folder, starting with its drive, such as C:\\.";
const PATH_LONG: &str = "A folder path can be at most 1024 characters.";
const TURNING_OFF: &str = "Turning it off...";
const PATH_CHANGED: &str =
    "The path changed while it was being checked. Press Enter to check it again.";

/// Plugin text made safe for one terminal row and bounded. Input: the text. Output: the text
/// without control or hidden characters, at most `TEXT_CHARS_MAX` characters.
pub(in crate::tui) fn safe(text: &str) -> String {
    cut(&printable(text.to_owned()), TEXT_CHARS_MAX, "...")
}

/// One plain line for a plugin problem: the engine's sentence, then its code and, when
/// known, the plugin.toml line and field. Input: the problem. Output: the line.
pub(in crate::tui) fn problem_text(problem: &Problem) -> String {
    let mut parts = vec![safe(&problem.code)];
    if let Some(line) = problem.line {
        parts.push(format!("line {line}"));
    }
    if let Some(field) = &problem.field {
        parts.push(safe(field));
    }
    format!("{} ({})", safe(&problem.message), parts.join(", "))
}

/// How a plugin runs, in the line the row and the review share: the download, or every host
/// it reaches with each host it did not reach when approved marked new. Inputs: how it runs
/// and the grown hosts. Output: the line.
pub(in crate::tui) fn runs_line(runs: &RunsView, grown: &[String]) -> String {
    match runs {
        RunsView::Local { download_bytes, .. } => format!(
            "Runs on this computer with the built-in model, a {} MB download",
            (download_bytes + 500_000) / 1_000_000
        ),
        RunsView::Http {
            hosts,
            local_unencrypted,
            ..
        } => format!(
            "Reaches {}{}",
            hosts
                .iter()
                .map(|host| if grown.contains(host) {
                    format!("{} (new)", safe(host))
                } else {
                    safe(host)
                })
                .collect::<Vec<_>>()
                .join(", "),
            if *local_unencrypted {
                " (local, not encrypted)"
            } else {
                ""
            }
        ),
    }
}

/// The short words of a turn-off choice. Input: the choice. Output: the words.
fn choice_words(choice: TurnOff) -> &'static str {
    match choice {
        TurnOff::RefuseNewCalls => "Refuse new calls",
        TurnOff::StopRuns => "Stop runs using it",
    }
}

/// The other turn-off choice. Input: the choice. Output: the one Tab switches to.
fn other(choice: TurnOff) -> TurnOff {
    match choice {
        TurnOff::RefuseNewCalls => TurnOff::StopRuns,
        TurnOff::StopRuns => TurnOff::RefuseNewCalls,
    }
}

/// Where one secret's key comes from, never the key. Input: the secret. Output: the line.
fn secret_line(secret: &SecretView) -> String {
    let name = safe(&secret.name);
    match &secret.binding {
        Binding::Account { name: account, .. } => format!("{name}: Key from {}", safe(account)),
        Binding::Key => format!("{name}: Key you entered"),
        Binding::None => format!("{name}: Not set"),
    }
}

/// One row of the page: one of the plugins the listing carries, or a bundled code plugin
/// only the grants read names.
#[derive(Clone, Copy)]
enum Entry<'a> {
    App(&'a PluginView),
    Code(&'a PluginGrants),
}

/// What Enter does on a row: review it (approvable or read-only), switch it, say why it
/// can't run, or nothing for an essential plugin.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Action {
    Review,
    Look,
    Switch,
    Refused,
    Nothing,
}

impl Entry<'_> {
    fn name(&self) -> &str {
        match self {
            Self::App(plugin) => &plugin.name,
            Self::Code(grants) => &grants.plugin,
        }
    }

    fn title(&self) -> String {
        match self {
            Self::App(plugin) => safe(&plugin.title),
            Self::Code(grants) => safe(&grants.plugin),
        }
    }

    fn on(&self) -> bool {
        match self {
            Self::App(plugin) => plugin.enabled,
            Self::Code(grants) => grants.state == GrantState::On,
        }
    }

    fn enter(&self) -> Action {
        match self {
            Self::App(plugin) => match (&plugin.status, plugin.origin) {
                (PluginStatus::Refused { .. }, _) => Action::Refused,
                (PluginStatus::NeedsReview | PluginStatus::Changed { .. }, _) => Action::Review,
                (_, Origin::Bundled) => Action::Look,
                (_, Origin::User) => Action::Switch,
            },
            Self::Code(grants) if grants.essential => Action::Nothing,
            Self::Code(_) => Action::Switch,
        }
    }

    fn switchable(&self) -> bool {
        match self {
            Self::App(plugin) => {
                matches!(plugin.status, PluginStatus::Ready | PluginStatus::NeedsKey)
            }
            Self::Code(grants) => !grants.essential,
        }
    }

    /// The one label a row shows and its role: the trust state of an added plugin,
    /// Bundled or Essential, then whether it is on.
    fn label(&self) -> (String, Role) {
        let switch = if self.on() { "On" } else { "Off" };
        let role = if self.on() {
            Role::Success
        } else {
            Role::Muted
        };
        match self {
            Self::App(plugin) => match (&plugin.status, plugin.origin) {
                (PluginStatus::Refused { .. }, _) => ("Refused".into(), Role::Error),
                (PluginStatus::NeedsReview, _) => ("Not approved".into(), Role::Warning),
                (PluginStatus::Changed { .. }, _) => ("Changed - review".into(), Role::Warning),
                (_, Origin::Bundled) => (format!("Bundled  {switch}"), role),
                (_, Origin::User) => (format!("Approved  {switch}"), role),
            },
            Self::Code(grants) if grants.essential => (format!("Essential  {switch}"), role),
            Self::Code(_) => (format!("Bundled  {switch}"), role),
        }
    }
}

#[derive(Debug, PartialEq)]
enum State {
    Loading,
    Failed(String),
    Ready(PageReply),
}

#[derive(Debug, PartialEq)]
enum Grants {
    Loading,
    Failed(String),
    Ready(Vec<PluginGrants>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pending {
    List,
    Grants,
    Check,
    Save,
    Users,
    Setting,
    Accounts,
}

/// The key dialog for one plugin: its secrets and the one being set, the hosts the key goes
/// to, the API-key accounts read for it (None while reading), the chosen source (an account
/// index, or one past the accounts for a typed key) and the masked key field.
struct KeyForm {
    plugin: String,
    title: String,
    hosts: Vec<String>,
    secrets: Vec<String>,
    secret: usize,
    accounts: Option<Vec<(String, String)>>,
    choice: usize,
    key: TextField,
}

/// The turn-off confirm: the plugin, the runs holding it, the stored choice and the one
/// this call sends.
struct TurnOffForm {
    name: String,
    title: String,
    users: Vec<PluginUser>,
    stored: TurnOff,
    choice: TurnOff,
}

enum Open {
    None,
    Add(Box<TextField>),
    Trust {
        folder: Option<PathBuf>,
        trust: Box<TrustView>,
        approve: bool,
        scroll: usize,
    },
    Refused {
        folder: Option<PathBuf>,
        problem: Problem,
    },
    Remove {
        name: String,
        title: String,
        keyed: bool,
    },
    TurnOff(Box<TurnOffForm>),
    Key(Box<KeyForm>),
}

/// Settings, App plugins: the plugins the listing carries, then each bundled code plugin
/// the grants read names, with one label each, how it runs, where each secret's key comes
/// from and, for a code plugin, its permissions in plain words. `a` adds a plugin folder
/// by path only after its review is approved with `y`, and only the path that review
/// checked; Enter reviews an unapproved or changed plugin, shows a bundled one read-only,
/// or switches an approved or code plugin;
/// turning off always confirms, listing the runs holding it; `w` switches the stored
/// choice for runs holding a plugin turned off. One request runs at a time.
pub(in crate::tui) struct AppPluginsPage {
    state: State,
    grants: Grants,
    selected: usize,
    open: Open,
    pending: Option<Pending>,
    regrant: bool,
    reviewing: Option<bool>,
    checking: Option<PathBuf>,
    failure: Option<String>,
    notices: Vec<String>,
}

impl AppPluginsPage {
    /// The page, loading until the engine answers. Output: the screen; `shown` reads.
    pub fn new() -> Self {
        Self {
            state: State::Loading,
            grants: Grants::Loading,
            selected: 0,
            open: Open::None,
            pending: None,
            regrant: false,
            reviewing: None,
            checking: None,
            failure: None,
            notices: Vec::new(),
        }
    }

    fn ask(&mut self, job: AppPluginsJob, pending: Pending, ctx: &mut Ctx) {
        assert!(self.pending.is_none(), "one request at a time");
        self.pending = Some(pending);
        self.failure = None;
        send(ctx, SettingsJob::AppPlugins(Box::new(job)));
    }

    fn read(&mut self, ctx: &mut Ctx) {
        if self.pending.is_none() {
            self.regrant = true;
            self.ask(AppPluginsJob::List, Pending::List, ctx);
        }
    }

    fn entries(&self) -> Vec<Entry<'_>> {
        let State::Ready(page) = &self.state else {
            return Vec::new();
        };
        let mut entries: Vec<Entry<'_>> = page.listing.plugins.iter().map(Entry::App).collect();
        if let Grants::Ready(grants) = &self.grants {
            entries.extend(
                grants
                    .iter()
                    .filter(|grant| grant.origin == Origin::Bundled)
                    .filter(|grant| {
                        !page
                            .listing
                            .plugins
                            .iter()
                            .any(|plugin| plugin.name == grant.plugin)
                    })
                    .map(Entry::Code),
            );
        }
        entries
    }

    fn stored(&self) -> TurnOff {
        match &self.state {
            State::Ready(page) => page.when_turned_off.value,
            _ => TurnOff::default(),
        }
    }

    fn current(&self) -> Option<Entry<'_>> {
        self.entries().get(self.selected).copied()
    }

    fn current_app(&self) -> Option<PluginView> {
        match self.current() {
            Some(Entry::App(plugin)) => Some(plugin.clone()),
            _ => None,
        }
    }

    fn activate(&mut self, ctx: &mut Ctx) {
        let Some(entry) = self.current() else {
            return;
        };
        let (name, on) = (entry.name().to_owned(), entry.on());
        match entry.enter() {
            Action::Refused => ctx.say(Message::error(
                "Refused plugins can't run.",
                "Its reason is shown under it.",
            )),
            Action::Review => self.review(name, true, ctx),
            Action::Look => self.review(name, false, ctx),
            Action::Switch => self.switch(name, on, ctx),
            Action::Nothing => {}
        }
    }

    fn review(&mut self, name: String, approve: bool, ctx: &mut Ctx) {
        self.reviewing = Some(approve);
        self.ask(AppPluginsJob::Review { name }, Pending::Check, ctx);
    }

    fn switch(&mut self, name: String, on: bool, ctx: &mut Ctx) {
        if on {
            self.ask(AppPluginsJob::Users { name }, Pending::Users, ctx);
        } else {
            self.regrant = true;
            let job = AppPluginsJob::SetEnabled {
                name,
                enabled: true,
                when_off: None,
            };
            self.ask(job, Pending::Save, ctx);
        }
    }

    fn space(&mut self, ctx: &mut Ctx) {
        let Some(entry) = self.current() else {
            return;
        };
        if entry.switchable() {
            let (name, on) = (entry.name().to_owned(), entry.on());
            self.switch(name, on, ctx);
        }
    }

    fn remove(&mut self, ctx: &mut Ctx) {
        let Some(plugin) = self.current_app().filter(|one| one.origin == Origin::User) else {
            if self.current().is_some() {
                ctx.say(Message::error(
                    "Not removed.",
                    &sentence("app_plugin_bundled"),
                ));
            }
            return;
        };
        self.open = Open::Remove {
            keyed: plugin
                .secrets
                .iter()
                .any(|secret| secret.binding == Binding::Key),
            name: plugin.name,
            title: safe(&plugin.title),
        };
    }

    fn key_form(&mut self, ctx: &mut Ctx) {
        let Some(plugin) = self.current_app().filter(|one| !one.secrets.is_empty()) else {
            if self.current().is_some() {
                ctx.say(Message::warning(
                    "No key needed.",
                    "This plugin declares no secrets.",
                ));
            }
            return;
        };
        let secret = plugin
            .secrets
            .iter()
            .position(|secret| secret.binding == Binding::None)
            .unwrap_or(0);
        let hosts = match &plugin.runs {
            Some(RunsView::Http { hosts, .. }) => hosts.iter().map(|host| safe(host)).collect(),
            _ => Vec::new(),
        };
        self.open = Open::Key(Box::new(KeyForm {
            plugin: plugin.name.clone(),
            title: safe(&plugin.title),
            hosts,
            secrets: plugin.secrets.into_iter().map(|one| one.name).collect(),
            secret,
            accounts: None,
            choice: 0,
            key: TextField::secret("Key", KEY_CHARS_MAX),
        }));
        self.pending = Some(Pending::Accounts);
        self.failure = None;
        send(ctx, SettingsJob::AllAccounts);
    }

    fn unbind(&mut self, ctx: &mut Ctx) {
        let Some(plugin) = self.current_app() else {
            return;
        };
        let Some(secret) = plugin
            .secrets
            .iter()
            .find(|secret| secret.binding != Binding::None)
        else {
            ctx.say(Message::warning(
                "Nothing to unbind.",
                "No secret of this plugin has a key.",
            ));
            return;
        };
        let job = AppPluginsJob::Unbind {
            name: plugin.name.clone(),
            secret: secret.name.clone(),
        };
        self.ask(job, Pending::Save, ctx);
    }

    fn check(&mut self, ctx: &mut Ctx) {
        let Open::Add(field) = &self.open else {
            return;
        };
        let typed = field.value().trim().to_owned();
        if typed.chars().count() > PATH_CHARS_MAX {
            self.failure = Some(PATH_LONG.into());
            return;
        }
        let folder = PathBuf::from(&typed);
        if typed.is_empty() || !folder.is_absolute() {
            self.failure = Some(ABSOLUTE.into());
            return;
        }
        self.checking = Some(folder.clone());
        self.ask(AppPluginsJob::Preview { folder }, Pending::Check, ctx);
    }

    fn confirm(&mut self, ctx: &mut Ctx) {
        let job = match &self.open {
            Open::Trust {
                folder: Some(folder),
                trust,
                approve: true,
                ..
            } => AppPluginsJob::Add {
                folder: folder.clone(),
                sha256: trust.sha256.clone(),
                replace: trust.replaces,
            },
            Open::Trust {
                folder: None,
                trust,
                approve: true,
                ..
            } => AppPluginsJob::Trust {
                name: trust.name.clone(),
                sha256: trust.sha256.clone(),
            },
            Open::Remove { name, .. } => AppPluginsJob::Remove { name: name.clone() },
            Open::TurnOff(form) => {
                self.regrant = true;
                AppPluginsJob::SetEnabled {
                    name: form.name.clone(),
                    enabled: false,
                    when_off: (form.choice != form.stored).then_some(form.choice),
                }
            }
            _ => return,
        };
        self.ask(job, Pending::Save, ctx);
    }

    fn save_key(&mut self, ctx: &mut Ctx) {
        let Open::Key(form) = &mut self.open else {
            return;
        };
        let Some(accounts) = &form.accounts else {
            return;
        };
        let account = accounts.get(form.choice).map(|(id, _)| id.clone());
        let (name, secret) = (form.plugin.clone(), form.secrets[form.secret].clone());
        let job = if let Some(account) = account {
            AppPluginsJob::BindAccount {
                name,
                secret,
                account,
            }
        } else {
            match AccountSecret::new(form.key.take_secret()) {
                Ok(key) => AppPluginsJob::SetKey { name, secret, key },
                Err(_) => {
                    self.failure = Some(sentence("app_plugin_key_invalid"));
                    return;
                }
            }
        };
        self.ask(job, Pending::Save, ctx);
    }

    fn add_key(&mut self, key: &Key, ctx: &mut Ctx) -> Flow {
        let Open::Add(field) = &mut self.open else {
            return Flow::Ignored;
        };
        match key {
            Key::Esc => {
                self.open = Open::None;
                self.failure = None;
            }
            Key::Enter if self.pending.is_none() => self.check(ctx),
            other => {
                field.key(other);
            }
        }
        Flow::Handled
    }

    fn key_key(&mut self, key: &Key, ctx: &mut Ctx) -> Flow {
        let Open::Key(form) = &mut self.open else {
            return Flow::Ignored;
        };
        let sources = form
            .accounts
            .as_ref()
            .map_or(0, |accounts| accounts.len() + 1);
        let typed = form
            .accounts
            .as_ref()
            .is_some_and(|accounts| form.choice == accounts.len());
        match key {
            Key::Esc => {
                self.open = Open::None;
                self.failure = None;
            }
            Key::Up => form.choice = form.choice.saturating_sub(1),
            Key::Down if sources > 0 => form.choice = (form.choice + 1).min(sources - 1),
            Key::Left if form.secrets.len() > 1 => {
                form.secret = (form.secret + form.secrets.len() - 1) % form.secrets.len();
            }
            Key::Right if form.secrets.len() > 1 => {
                form.secret = (form.secret + 1) % form.secrets.len();
            }
            Key::Enter if self.pending.is_none() => self.save_key(ctx),
            other if typed => {
                form.key.key(other);
            }
            _ => {}
        }
        Flow::Handled
    }

    fn modal_key(&mut self, key: &Key, ctx: &mut Ctx) -> Flow {
        let idle = self.pending.is_none();
        match (&mut self.open, key) {
            (_, Key::Esc) => {
                self.open = Open::None;
                self.failure = None;
            }
            (Open::Trust { scroll, .. }, Key::Up) => *scroll = scroll.saturating_sub(1),
            (Open::Trust { scroll, trust, .. }, Key::Down) => {
                let last = trust_lines(trust, usize::from(MODAL_WIDTH) - 4).len();
                *scroll = (*scroll + 1).min(last.saturating_sub(1));
            }
            (Open::TurnOff(form), Key::Tab | Key::BackTab) if idle => {
                form.choice = other(form.choice);
            }
            (Open::Trust { approve: true, .. }, Key::Char('y')) if idle => self.confirm(ctx),
            (Open::Remove { .. } | Open::TurnOff(_), Key::Char('y')) if idle => {
                self.confirm(ctx);
            }
            (Open::Refused { folder, .. }, Key::Char('e')) => {
                if let Some(folder) = folder.clone() {
                    let mut field = path_field();
                    field.set(&folder.to_string_lossy());
                    self.open = Open::Add(Box::new(field));
                }
            }
            _ => {}
        }
        Flow::Handled
    }

    fn apply(&mut self, event: &AppPluginsEvent, ctx: &mut Ctx) {
        let pending = self.pending.take();
        match event {
            AppPluginsEvent::Page(Ok(page)) => {
                let was = std::mem::replace(&mut self.open, Open::None);
                if pending == Some(Pending::Save) {
                    ctx.say(Message::success("Saved.", saved_line(&was)));
                }
                if pending == Some(Pending::Setting) {
                    let now = choice_words(page.when_turned_off.value);
                    ctx.say(Message::success(
                        "Saved.",
                        &format!("When turned off: {now}."),
                    ));
                }
                self.note(page);
                self.state = State::Ready(page.clone());
                self.selected = self.selected.min(self.entries().len().saturating_sub(1));
                if std::mem::take(&mut self.regrant) {
                    self.ask(AppPluginsJob::Grants, Pending::Grants, ctx);
                }
            }
            AppPluginsEvent::Page(Err(code)) if pending == Some(Pending::List) => {
                self.regrant = false;
                self.state = State::Failed(code.clone());
            }
            AppPluginsEvent::Page(Err(code)) => {
                self.regrant = false;
                if let Open::Remove { .. } | Open::Trust { .. } | Open::TurnOff(_) = self.open {
                    self.open = Open::None;
                }
                if matches!(self.open, Open::None) {
                    ctx.say(Message::error("Not saved.", &sentence(code)));
                } else {
                    self.failure = Some(sentence(code));
                }
            }
            AppPluginsEvent::Grants(result) => {
                self.grants = match result {
                    Ok(grants) => Grants::Ready(grants.clone()),
                    Err(code) => Grants::Failed(code.clone()),
                };
                self.selected = self.selected.min(self.entries().len().saturating_sub(1));
            }
            AppPluginsEvent::Preview(result) => self.previewed(result, ctx),
            AppPluginsEvent::Users { name, result } => match result {
                Ok(users) => {
                    let stored = self.stored();
                    let title = self
                        .entries()
                        .iter()
                        .find(|entry| entry.name() == name)
                        .map_or_else(|| safe(name), Entry::title);
                    self.open = Open::TurnOff(Box::new(TurnOffForm {
                        name: name.clone(),
                        title,
                        users: users.clone(),
                        stored,
                        choice: stored,
                    }));
                }
                Err(code) => ctx.say(Message::error("Not turned off.", &sentence(code))),
            },
        }
    }

    fn previewed(&mut self, result: &Result<Preview, String>, ctx: &mut Ctx) {
        let checked = self.checking.take();
        let (folder, approve) = match (&self.open, self.reviewing.take()) {
            (Open::Add(field), _) => match checked {
                Some(sent) if PathBuf::from(field.value().trim()) == sent => (Some(sent), true),
                _ => {
                    self.failure = Some(PATH_CHANGED.into());
                    return;
                }
            },
            (_, Some(approve)) => (None, approve),
            _ => return,
        };
        match result {
            Ok(Preview::Ready { trust }) => {
                self.open = Open::Trust {
                    folder,
                    trust: Box::new(trust.clone()),
                    approve,
                    scroll: 0,
                };
            }
            Ok(Preview::Refused { problem }) => {
                self.open = Open::Refused {
                    folder,
                    problem: problem.clone(),
                };
            }
            Err(code) if folder.is_some() => self.failure = Some(sentence(code)),
            Err(code) => ctx.say(Message::error("Not reviewed.", &sentence(code))),
        }
    }

    fn note(&mut self, page: &PageReply) {
        for notice in &page.notices {
            let title = page
                .listing
                .plugins
                .iter()
                .find(|plugin| plugin.name == notice.plugin)
                .map_or_else(|| safe(&notice.plugin), |plugin| safe(&plugin.title));
            if self.notices.len() == NOTICES_MAX {
                self.notices.remove(0);
            }
            self.notices.push(format!(
                "{title} changed since you approved it. Review it to use it again."
            ));
        }
        assert!(self.notices.len() <= NOTICES_MAX);
    }

    fn accounts(&mut self, result: &Result<Vec<AccountView>, String>) {
        if self.pending != Some(Pending::Accounts) {
            return;
        }
        self.pending = None;
        let Open::Key(form) = &mut self.open else {
            return;
        };
        let keyed = match result {
            Ok(accounts) => accounts
                .iter()
                .filter(|account| account.method == SignInMethod::ApiKey)
                .map(|account| (account.id.as_str().to_owned(), safe(&account.name)))
                .collect(),
            Err(code) => {
                self.failure = Some(sentence(code));
                Vec::new()
            }
        };
        form.accounts = Some(keyed);
        form.choice = 0;
    }

    fn words(&self, capability: &str) -> String {
        match &self.state {
            State::Ready(page) => page
                .words
                .iter()
                .find(|row| row.name == capability)
                .map_or_else(|| safe(capability), |row| safe(&row.words)),
            _ => safe(capability),
        }
    }

    fn under(&self, entry: Entry<'_>, selected: bool) -> Vec<String> {
        match entry {
            Entry::App(plugin) => {
                let origin = match plugin.origin {
                    Origin::Bundled => "Bundled",
                    Origin::User => "Added by you",
                };
                let mut under = vec![format!(
                    "{}, version {}. {origin}.",
                    safe(&plugin.name),
                    safe(&plugin.version)
                )];
                if !plugin.description.is_empty() {
                    under.push(safe(&plugin.description));
                }
                if let Some(runs) = &plugin.runs {
                    under.push(runs_line(runs, &[]));
                }
                match &plugin.status {
                    PluginStatus::Refused { problem } => under.push(problem_text(problem)),
                    PluginStatus::NeedsReview => {
                        under.push("Review this plugin before it runs.".into());
                    }
                    PluginStatus::Changed { .. } => under.push(
                        "Its plugin.toml changed since you approved it. Review it to use it again."
                            .into(),
                    ),
                    PluginStatus::NeedsKey => {
                        under.push("Set a key for each secret it needs.".into());
                    }
                    PluginStatus::Ready => {}
                }
                under.extend(plugin.secrets.iter().map(secret_line));
                under
            }
            Entry::Code(grants) => {
                let count = grants.capabilities.len();
                let mut under = vec![format!(
                    "{}, bundled code plugin. {count} permission{}.",
                    safe(&grants.plugin),
                    if count == 1 { "" } else { "s" }
                )];
                if grants.essential {
                    under.push("Essential, stays on.".into());
                }
                if selected {
                    under.extend(
                        grants
                            .capabilities
                            .iter()
                            .map(|capability| format!("- {}", self.words(&capability.name))),
                    );
                }
                under
            }
        }
    }

    fn body(
        &self,
        limit: usize,
        height: usize,
        ctx: &Ctx,
    ) -> (Vec<Line<'static>>, Option<Position>) {
        let theme = &ctx.theme;
        let muted = |text: &str| -> Vec<Line<'static>> {
            wrap(text, limit)
                .into_iter()
                .map(|part| styled(part, Role::Muted, theme))
                .collect()
        };
        let mut lines = muted(INTRO);
        if let State::Ready(page) = &self.state {
            let setting = &page.when_turned_off;
            lines.extend(muted(&format!(
                "When turned off: {}",
                choice_words(setting.value)
            )));
            if setting.project_override {
                lines.extend(
                    wrap(
                        "A project settings file sets this, and it wins over what you save here.",
                        limit,
                    )
                    .into_iter()
                    .map(|part| styled(part, Role::Warning, theme)),
                );
            }
        }
        lines.push(Line::default());
        let page = match &self.state {
            State::Loading => {
                lines.push(styled("Reading app plugins...", Role::Muted, theme));
                return (lines, None);
            }
            State::Failed(code) => {
                lines.extend(
                    wrap(&sentence(code), limit)
                        .into_iter()
                        .map(|part| styled(part, Role::Error, theme)),
                );
                return (lines, None);
            }
            State::Ready(page) => page,
        };
        match &self.open {
            Open::Add(field) => {
                lines.push(styled("Add an app plugin", Role::Heading, theme));
                lines.extend(muted(
                    "Type the full path of a plugin folder holding its plugin.toml. Nothing runs until you approve its review.",
                ));
                self.failure_lines(&mut lines, limit, ctx);
                let top = lines.len();
                let (field_lines, at) = field.lines(limit, true, theme, ctx.glyphs);
                lines.extend(field_lines);
                let cursor = at.map(|at| Position::new(at.x, at.y + top as u16));
                return (lines, cursor);
            }
            Open::Key(form) => return self.key_body(form, lines, (limit, height), ctx),
            _ => {}
        }
        for notice in &self.notices {
            lines.extend(
                wrap(notice, limit)
                    .into_iter()
                    .map(|part| styled(part, Role::Warning, theme)),
            );
        }
        if matches!(self.open, Open::None) {
            self.failure_lines(&mut lines, limit, ctx);
        }
        let mut selected_rows = (0, 0);
        for (index, entry) in self.entries().into_iter().enumerate() {
            let start = lines.len();
            let selected = index == self.selected;
            let (word, role) = entry.label();
            lines.push(list_row(
                row(
                    limit.saturating_sub(2),
                    Line::raw(entry.title()),
                    styled(word, role, theme),
                    ctx.glyphs,
                ),
                limit,
                selected,
                true,
                theme,
                ctx.glyphs,
            ));
            for text in self.under(entry, selected) {
                lines.extend(
                    wrap(&text, limit.saturating_sub(4))
                        .into_iter()
                        .map(|part| styled(format!("    {part}"), Role::Muted, theme)),
                );
            }
            if selected {
                selected_rows = (start, lines.len());
            }
        }
        match &self.grants {
            Grants::Loading => lines.extend(muted("Reading bundled code plugins...")),
            Grants::Failed(code) => lines.extend(
                wrap(
                    &format!(
                        "Bundled code plugins couldn't be read. {} Press r to read again.",
                        sentence(code)
                    ),
                    limit,
                )
                .into_iter()
                .map(|part| styled(part, Role::Error, theme)),
            ),
            Grants::Ready(_) => {}
        }
        if page.listing.more {
            lines.extend(muted(&format!(
                "Showing the first {} plugins.",
                page.listing.plugins.len()
            )));
        }
        if selected_rows.1 > height {
            let skip = selected_rows.0.min(lines.len().saturating_sub(height));
            lines.drain(..skip);
        }
        (lines, None)
    }

    /// The key dialog under the page's lines, scrolled so the chosen source, and the typed
    /// key's input when that source is chosen, stay in view. Inputs: the dialog, the lines
    /// above it, the width and height, and the context. Output: the lines and the cursor.
    fn key_body(
        &self,
        form: &KeyForm,
        mut lines: Vec<Line<'static>>,
        (limit, height): (usize, usize),
        ctx: &Ctx,
    ) -> (Vec<Line<'static>>, Option<Position>) {
        let theme = &ctx.theme;
        let secret = safe(&form.secrets[form.secret]);
        lines.push(styled(format!("Key for {secret}"), Role::Heading, theme));
        let mut about = format!("Where {} gets its {secret}.", form.title);
        if !form.hosts.is_empty() {
            about.push_str(&format!(" The key goes only to {}.", form.hosts.join(", ")));
        }
        if form.secrets.len() > 1 {
            about.push_str(" Left and right pick another secret.");
        }
        lines.extend(
            wrap(&about, limit)
                .into_iter()
                .map(|part| styled(part, Role::Muted, theme)),
        );
        self.failure_lines(&mut lines, limit, ctx);
        let Some(accounts) = &form.accounts else {
            lines.push(styled("Reading API-key accounts...", Role::Muted, theme));
            return (lines, None);
        };
        let mut shown_to = lines.len() + form.choice + 1;
        for (index, (_, name)) in accounts.iter().enumerate() {
            lines.push(list_row(
                Line::raw(format!("Key from {name}")),
                limit,
                form.choice == index,
                true,
                theme,
                ctx.glyphs,
            ));
        }
        lines.push(list_row(
            Line::raw("Enter a key for this plugin"),
            limit,
            form.choice == accounts.len(),
            true,
            theme,
            ctx.glyphs,
        ));
        let mut cursor = None;
        if form.choice == accounts.len() {
            let top = lines.len();
            let (key, at) = form.key.lines(limit, true, theme, ctx.glyphs);
            cursor = at.map(|at| Position::new(at.x, at.y + top as u16));
            lines.extend(key);
            lines.push(styled(
                "Sent to the engine once and never shown again.",
                Role::Muted,
                theme,
            ));
            shown_to = lines.len();
        }
        if shown_to > height {
            let skip = shown_to - height;
            lines.drain(..skip);
            cursor = cursor.map(|at| Position::new(at.x, at.y.saturating_sub(skip as u16)));
        }
        (lines, cursor)
    }

    fn failure_lines(&self, lines: &mut Vec<Line<'static>>, limit: usize, ctx: &Ctx) {
        if let Some(failure) = &self.failure {
            lines.extend(
                wrap(failure, limit)
                    .into_iter()
                    .map(|part| styled(part, Role::Error, &ctx.theme)),
            );
        }
    }

    fn list_hints(&self, vertical: &str) -> Vec<Hint> {
        match &self.state {
            State::Loading => return vec![hint("Esc", "Back")],
            State::Failed(_) => return vec![hint("r", "Try again"), hint("Esc", "Back")],
            State::Ready(_) => {}
        }
        let mut hints = Vec::new();
        if let Some(entry) = self.current() {
            let turn = if entry.on() { "Turn off" } else { "Turn on" };
            match entry.enter() {
                Action::Review => hints.push(hint("Enter", "Review")),
                Action::Look => {
                    hints.push(hint("Enter", "Review"));
                    hints.push(hint("Space", turn));
                }
                Action::Switch => hints.push(hint("Enter", turn)),
                Action::Refused | Action::Nothing => {}
            }
            if let Entry::App(plugin) = entry {
                if !plugin.secrets.is_empty() {
                    hints.push(hint("k", "Set key"));
                }
                if plugin
                    .secrets
                    .iter()
                    .any(|secret| secret.binding != Binding::None)
                {
                    hints.push(hint("u", "Unbind"));
                }
                if plugin.origin == Origin::User {
                    hints.push(hint("x", "Remove"));
                }
            }
        }
        hints.push(hint("a", "Add plugin"));
        hints.push(hint("w", "When turned off"));
        hints.push(hint(vertical, "Move"));
        hints.push(hint("r", "Read again"));
        hints.push(hint("Esc", "Back"));
        hints
    }
}

impl Default for AppPluginsPage {
    fn default() -> Self {
        Self::new()
    }
}

/// A fresh Add path field, one character past the bound so an over-long path stays whole
/// and is refused rather than cut.
fn path_field() -> TextField {
    TextField::new("Plugin folder", PATH_CHARS_MAX + 1)
}

/// The success line for the change a page answered. Input: what was open. Output: the line.
fn saved_line(open: &Open) -> &'static str {
    match open {
        Open::Trust {
            folder: Some(_), ..
        } => "The plugin is added.",
        Open::Trust { folder: None, .. } => "The plugin is approved.",
        Open::Remove { .. } => "The plugin is removed.",
        Open::TurnOff(_) => "The plugin is off.",
        Open::Key(_) => "The key is set.",
        _ => "The plugin is updated.",
    }
}

/// The lines of a review: what changed first, then what the plugin is, how it runs with each
/// new host marked, the address it calls, the secrets it needs, its SHA-256 and whether it
/// replaces the owner's plugin. Inputs: the review and the width. Output: the lines.
fn trust_lines(trust: &TrustView, room: usize) -> Vec<String> {
    let mut texts = Vec::new();
    if trust.changed {
        texts.push("Changed since you approved it.".to_owned());
        if !trust.grew_hosts.is_empty() {
            texts.push(format!(
                "New hosts: {}",
                trust
                    .grew_hosts
                    .iter()
                    .map(|host| safe(host))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
    }
    if !trust.description.is_empty() {
        texts.push(safe(&trust.description));
    }
    texts.push(format!(
        "Kind: {}",
        match trust.kind {
            PluginKind::Embedding => "Embedding",
        }
    ));
    texts.push(format!("Version {}", safe(&trust.version)));
    texts.push(runs_line(&trust.runs, &trust.grew_hosts));
    if let RunsView::Http { url, .. } = &trust.runs {
        texts.push(format!("Address: {}", safe(url)));
    }
    texts.push(format!(
        "Secrets it needs: {}",
        if trust.secrets.is_empty() {
            "none".to_owned()
        } else {
            trust
                .secrets
                .iter()
                .map(|name| safe(name))
                .collect::<Vec<_>>()
                .join(", ")
        }
    ));
    texts.push(format!("SHA-256 {}", safe(&trust.sha256)));
    if trust.replaces {
        texts.push(format!("Replaces your plugin named {}.", safe(&trust.name)));
    }
    texts.iter().flat_map(|text| wrap(text, room)).collect()
}

/// The lines of the turn-off confirm: the runs holding the plugin, then what this call does
/// to them and whether that differs from the setting. Inputs: the confirm and the width.
/// Output: the lines.
fn turn_off_lines(form: &TurnOffForm, room: usize) -> Vec<String> {
    let mut texts = Vec::new();
    if form.users.is_empty() {
        texts.push("Nothing uses it now.".to_owned());
    } else {
        texts.push("In use by:".to_owned());
        texts.extend(
            form.users
                .iter()
                .take(USERS_SHOWN_MAX)
                .map(|user| format!("- {}", safe(&user.label))),
        );
        if form.users.len() > USERS_SHOWN_MAX {
            texts.push(format!("and {} more", form.users.len() - USERS_SHOWN_MAX));
        }
    }
    texts.push(match form.choice {
        TurnOff::RefuseNewCalls => {
            "Refuse new calls: running work goes on without it, and each new call is refused."
                .to_owned()
        }
        TurnOff::StopRuns => "Stop runs using it: the runs listed above stop.".to_owned(),
    });
    texts.push(if form.choice == form.stored {
        "This is your setting.".to_owned()
    } else {
        format!(
            "This time only. Your setting stays {}.",
            choice_words(form.stored)
        )
    });
    texts.iter().flat_map(|text| wrap(text, room)).collect()
}

impl Screen for AppPluginsPage {
    fn crumbs(&self) -> Vec<String> {
        vec!["Settings".into(), "App plugins".into()]
    }

    fn hints(&self, ctx: &Ctx) -> Vec<Hint> {
        let vertical = format!("{}{}", ctx.glyphs.up, ctx.glyphs.down);
        match &self.open {
            Open::Add(_) => vec![hint("Enter", "Check"), hint("Esc", "Cancel")],
            Open::Key(_) => vec![
                hint(&vertical, "Source"),
                hint("Enter", "Save"),
                hint("Esc", "Cancel"),
            ],
            _ => self.list_hints(&vertical),
        }
    }

    fn wraps_hints(&self) -> bool {
        true
    }

    fn typing(&self) -> bool {
        match &self.open {
            Open::Add(_) => true,
            Open::Key(form) => form
                .accounts
                .as_ref()
                .is_some_and(|accounts| form.choice == accounts.len()),
            _ => false,
        }
    }

    fn key(&mut self, key: &Key, ctx: &mut Ctx) -> Flow {
        match &self.open {
            Open::Add(_) => return self.add_key(key, ctx),
            Open::Key(_) => return self.key_key(key, ctx),
            Open::None => {}
            _ => return self.modal_key(key, ctx),
        }
        let total = self.entries().len();
        let idle = total > 0 && self.pending.is_none();
        match key {
            Key::Char('r') => self.read(ctx),
            Key::Up => self.selected = self.selected.saturating_sub(1),
            Key::Down => self.selected = (self.selected + 1).min(total.saturating_sub(1)),
            Key::Enter if idle => self.activate(ctx),
            Key::Char(' ') if idle => self.space(ctx),
            Key::Char('a') if matches!(self.state, State::Ready(_)) && self.pending.is_none() => {
                self.failure = None;
                self.open = Open::Add(Box::new(path_field()));
            }
            Key::Char('x') if idle => self.remove(ctx),
            Key::Char('k') if idle => self.key_form(ctx),
            Key::Char('u') if idle => self.unbind(ctx),
            Key::Char('w') if matches!(self.state, State::Ready(_)) && self.pending.is_none() => {
                let when_off = other(self.stored());
                self.ask(
                    AppPluginsJob::SetWhenOff { when_off },
                    Pending::Setting,
                    ctx,
                );
            }
            _ => return Flow::Ignored,
        }
        Flow::Handled
    }

    fn arrive(&mut self, arrival: &Arrival, ctx: &mut Ctx) -> Flow {
        match settings_event(arrival) {
            Some(SettingsEvent::AppPlugins(event)) => self.apply(event, ctx),
            Some(SettingsEvent::AllAccounts(result)) => self.accounts(result),
            _ => {}
        }
        Flow::Handled
    }

    fn shown(&mut self, ctx: &mut Ctx) {
        if self.state == State::Loading {
            self.read(ctx);
        }
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect, ctx: &Ctx) -> Option<Position> {
        let limit = usize::from(area.width).saturating_sub(4).max(8);
        let (content, cursor) = self.body(limit, usize::from(area.height), ctx);
        draw_lines(frame.buffer_mut(), area, &indent(content, 2));
        cursor.map(|at| Position::new(area.x + 2 + at.x, area.y + at.y))
    }

    fn modal(&self, ctx: &Ctx) -> Option<(Modal, Vec<Hint>)> {
        let room = usize::from(MODAL_WIDTH) - 4;
        let vertical = format!("{}{}", ctx.glyphs.up, ctx.glyphs.down);
        let mut body: Vec<Line<'static>> = self
            .failure
            .iter()
            .flat_map(|failure| wrap(failure, room))
            .map(|part| styled(part, Role::Error, &ctx.theme))
            .collect();
        let (title, texts, hints) = match &self.open {
            Open::Trust {
                trust,
                approve,
                scroll,
                ..
            } => {
                let texts: Vec<String> = trust_lines(trust, room);
                let skip = (*scroll).min(texts.len().saturating_sub(1));
                let mut hints = Vec::new();
                if *approve {
                    hints.push(hint("y", "Approve"));
                }
                hints.push(hint(&vertical, "Scroll"));
                hints.push(hint("Esc", "Close"));
                let title = if *approve {
                    format!("Review {}", safe(&trust.title))
                } else {
                    format!("{} (bundled)", safe(&trust.title))
                };
                (title, texts[skip..].to_vec(), hints)
            }
            Open::Refused { folder, problem } => {
                body.extend(
                    wrap(&problem_text(problem), room)
                        .into_iter()
                        .map(|part| styled(part, Role::Error, &ctx.theme)),
                );
                let mut hints = Vec::new();
                if folder.is_some() {
                    hints.push(hint("e", "Edit path"));
                }
                hints.push(hint("Esc", "Close"));
                let title = if folder.is_some() {
                    "This plugin can't be added"
                } else {
                    "This plugin can't be approved"
                };
                (title.to_owned(), Vec::new(), hints)
            }
            Open::Remove { title, keyed, .. } => {
                let mut texts = vec![
                    format!("Remove {title}?"),
                    "Its folder is deleted from your plugins folder.".to_owned(),
                ];
                if *keyed {
                    texts.push("Removing it also deletes the key you entered for it.".into());
                }
                (
                    "Remove".to_owned(),
                    texts.iter().flat_map(|text| wrap(text, room)).collect(),
                    vec![hint("y", "Remove"), hint("Esc", "Cancel")],
                )
            }
            Open::TurnOff(form) if self.pending.is_some() => (
                format!("Turn off {}?", form.title),
                [vec![TURNING_OFF.to_owned()], turn_off_lines(form, room)].concat(),
                vec![hint("Esc", "Close")],
            ),
            Open::TurnOff(form) => (
                format!("Turn off {}?", form.title),
                turn_off_lines(form, room),
                vec![
                    hint("y", "Turn off"),
                    hint(
                        "Tab",
                        match form.choice {
                            TurnOff::RefuseNewCalls => "Stop runs instead",
                            TurnOff::StopRuns => "Refuse new calls instead",
                        },
                    ),
                    hint("Esc", "Keep on"),
                ],
            ),
            Open::None | Open::Add(_) | Open::Key(_) => return None,
        };
        body.extend(texts.into_iter().map(Line::raw));
        Some((
            Modal {
                width: MODAL_WIDTH,
                title,
                body,
                top: None,
            },
            hints,
        ))
    }

    fn busy(&self, _ctx: &Ctx) -> bool {
        self.pending.is_some()
    }

    fn draft(&self) -> bool {
        match &self.open {
            Open::Add(field) => !field.is_empty(),
            Open::Key(form) => !form.key.is_empty(),
            _ => false,
        }
    }

    fn loss(&self) -> &'static str {
        "What you typed will be lost."
    }

    fn place(&self) -> Option<Place> {
        Some(Place::AppPlugins)
    }
}
