use crate::accounts::{AccountView, SignInMethod};
use crate::app_plugins::{
    Binding, Listing, Origin, PluginKind, PluginStatus, PluginView, Preview, Problem, RunsView,
    SecretView, TrustView,
};
use crate::engine::owner::AccountSecret;
use crate::tui::client::app_plugins::{AppPluginsEvent, AppPluginsJob, MANIFEST_BYTES_MAX};
use crate::tui::client::settings::{SettingsEvent, SettingsJob};
use crate::tui::ctx::Ctx;
use crate::tui::frame::{Hint, Message, Modal, draw_lines, hint};
use crate::tui::keys::Key;
use crate::tui::screens::new_project_view::printable;
use crate::tui::screens::settings_accounts::{send, settings_event};
use crate::tui::screens::{Flow, Place, Screen};
use crate::tui::seam::Arrival;
use crate::tui::text::{coded, cut, indent, list_row, row, styled, wrap};
use crate::tui::theme::Role;
use crate::tui::widgets::TextField;
use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::text::Line;
use std::path::PathBuf;

#[cfg(all(test, unix))]
pub(in crate::tui) mod tests;

const MODAL_WIDTH: u16 = 66;
const TEXT_CHARS_MAX: usize = 300;
const KEY_CHARS_MAX: usize = 1024;
const PATH_CHARS_MAX: usize = 1024;
const INTRO: &str = "Plugins CrazyAgent itself runs, such as the embedding models search can rank with. Each one shows what it reaches before you trust it.";
const ABSOLUTE: &str = "Type the full path of a .yaml file, starting with /.";

/// One plain sentence for a refused App plugins or Search request, the words the desktop
/// uses. Input: the code. Output: the sentence, or the plain fallback naming the code.
pub(in crate::tui) fn sentence(code: &str) -> String {
    let known = match code {
        "app_plugin_outcome_unknown" => {
            "CrazyAgent couldn't confirm what happened. Read the list again before you try again."
        }
        "app_plugin_request_bounds" => "A manifest can be at most 16 KB.",
        "app_plugin_request_invalid" => "The engine didn't accept this plugin request.",
        "app_plugins_unavailable" => "App plugins aren't available in this engine yet.",
        "app_plugin_not_found" => "That plugin is gone. The list now shows the current ones.",
        "app_plugin_name_taken" => {
            "A built-in plugin already has that name. Give your plugin another name."
        }
        "app_plugin_bundled" => "Built-in plugins can be turned off but not removed or replaced.",
        "app_plugin_changed" => {
            "The manifest changed after you checked it. Check it again before you trust it."
        }
        "app_plugin_needs_review" => {
            "This plugin's file changed or is new. Review it before it runs."
        }
        "app_plugin_refused" => "CrazyAgent refused this manifest. Its reason is shown with it.",
        "app_plugin_folder_unsafe" => {
            "Your plugins folder isn't private to you, so CrazyAgent won't read it."
        }
        "app_plugin_folder_unavailable" => "CrazyAgent couldn't open your plugins folder.",
        "app_plugin_file_unsafe" => {
            "That plugin file isn't a plain file private to you, so CrazyAgent won't read it."
        }
        "app_plugin_secret_not_declared" => "This plugin doesn't declare that secret.",
        "app_plugin_account_not_found" => "That account is gone. Pick another one or enter a key.",
        "app_plugin_account_has_no_key" => {
            "That account has no saved API key to share. Pick another one or enter a key."
        }
        "app_plugin_key_invalid" => {
            "That key can't be used: it is empty, too long or has spaces or control characters."
        }
        "app_plugin_key_store_unavailable" => {
            "CrazyAgent couldn't save the key. Nothing was saved."
        }
        "app_plugin_file_unreadable" => {
            "CrazyAgent couldn't read that file. Type the path of a manifest you can open."
        }
        "app_plugin_file_not_text" => "That file isn't text. A manifest is a UTF-8 .yaml file.",
        "owner_entropy_unavailable" => {
            "This computer couldn't make a random value for the request. Nothing changed."
        }
        "search_choice_unavailable" => "That plugin can't rank search right now.",
        "search_confirm_required" => {
            "This plugin sends search text to a host you haven't confirmed. Choose it again to confirm."
        }
        "search_model_confirm" => {
            "The download size changed since you confirmed it. Read the page again and confirm."
        }
        "search_model_downloading" => "The built-in model is still downloading.",
        "search_model_store_unavailable" => {
            "CrazyAgent couldn't use the folder the built-in model lives in. Nothing changed."
        }
        _ => return coded("CrazyAgent couldn't do that.", code),
    };
    known.to_owned()
}

/// Manifest-derived text made safe for one terminal row and bounded. Input: the text.
/// Output: the text without control or hidden characters, at most `TEXT_CHARS_MAX` chars.
pub(in crate::tui) fn safe(text: &str) -> String {
    cut(&printable(text.to_owned()), TEXT_CHARS_MAX, "...")
}

/// One plain line for a plugin or manifest problem: the engine's sentence, then its code
/// and, when known, the manifest line and field. Input: the problem. Output: the line.
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

/// A size in whole decimal megabytes. Input: bytes. Output: such as "126 MB".
pub(in crate::tui) fn megabytes(bytes: u64) -> String {
    format!("{} MB", (bytes + 500_000) / 1_000_000)
}

/// How a plugin runs, in the line the row and the trust screen share. Input: how it runs.
/// Output: the line, naming the download or every host it reaches.
pub(in crate::tui) fn runs_line(runs: &RunsView) -> String {
    match runs {
        RunsView::Local { download_bytes, .. } => format!(
            "Runs on this computer with the built-in model, a {} download",
            megabytes(*download_bytes)
        ),
        RunsView::Http {
            hosts,
            local_unencrypted,
            ..
        } => format!(
            "Reaches {}{}",
            hosts
                .iter()
                .map(|host| safe(host))
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

/// The one label a plugin row shows, in a fixed order: Refused (with its problem), Needs
/// review, Needs a key, then Enabled or Off. Input: the plugin. Output: the word, its role
/// and the line under it, if any.
pub(in crate::tui) fn plugin_state(plugin: &PluginView) -> (&'static str, Role, Option<String>) {
    match &plugin.status {
        PluginStatus::Refused { problem } => ("Refused", Role::Error, Some(problem_text(problem))),
        PluginStatus::NeedsReview => (
            "Needs review",
            Role::Warning,
            Some("Review this plugin before it runs.".into()),
        ),
        PluginStatus::NeedsKey => (
            "Needs a key",
            Role::Warning,
            Some("Set a key for each secret it needs.".into()),
        ),
        PluginStatus::Ready if plugin.enabled => ("Enabled", Role::Success, None),
        PluginStatus::Ready => ("Off", Role::Muted, None),
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

#[derive(Debug, PartialEq)]
enum State {
    Loading,
    Failed(String),
    Ready(Listing),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pending {
    List,
    Check,
    Save,
    Accounts,
}

/// The paste box and the typed path, with the one that has focus.
struct AddForm {
    yaml: TextField,
    path: TextField,
    on_path: bool,
}

/// The key dialog for one plugin: its secrets and the one being set, the API-key accounts
/// read for it (None while reading), the chosen source (an account index, or one past the
/// accounts for a typed key) and the masked key field.
struct KeyForm {
    plugin: String,
    title: String,
    secrets: Vec<String>,
    secret: usize,
    accounts: Option<Vec<(String, String)>>,
    choice: usize,
    key: TextField,
}

enum Open {
    None,
    Add(AddForm),
    Trust {
        yaml: Option<String>,
        trust: Box<TrustView>,
        review: bool,
    },
    Refused {
        yaml: Option<String>,
        problem: Problem,
    },
    Remove {
        name: String,
        title: String,
    },
    Key(Box<KeyForm>),
}

/// Settings, App plugins: every bundled and user plugin with its one status label, how it
/// runs and where each secret's key comes from. `a` adds a pasted or typed-path manifest
/// only after its trust screen is confirmed with `y`; Enter reviews an unreviewed plugin or
/// turns a usable one on or off; `x` removes the owner's own after `y`; `k` binds a secret
/// to an API-key account or a key typed once; `u` unbinds. One request runs at a time.
pub(in crate::tui) struct AppPluginsPage {
    state: State,
    selected: usize,
    open: Open,
    pending: Option<Pending>,
    failure: Option<String>,
    sent: Option<String>,
}

impl AppPluginsPage {
    /// The page, loading until the engine answers. Output: the screen; `shown` reads.
    pub fn new() -> Self {
        Self {
            state: State::Loading,
            selected: 0,
            open: Open::None,
            pending: None,
            failure: None,
            sent: None,
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
            self.ask(AppPluginsJob::List, Pending::List, ctx);
        }
    }

    fn plugins(&self) -> &[PluginView] {
        match &self.state {
            State::Ready(listing) => &listing.plugins,
            _ => &[],
        }
    }

    fn current(&self) -> Option<&PluginView> {
        self.plugins().get(self.selected)
    }

    fn activate(&mut self, ctx: &mut Ctx) {
        let Some(plugin) = self.current().cloned() else {
            return;
        };
        match plugin.status {
            PluginStatus::Refused { .. } => ctx.say(Message::error(
                "Refused plugins can't run.",
                "Its reason is shown under it.",
            )),
            PluginStatus::NeedsReview => {
                self.ask(
                    AppPluginsJob::Review { name: plugin.name },
                    Pending::Check,
                    ctx,
                );
            }
            _ => self.ask(
                AppPluginsJob::SetEnabled {
                    name: plugin.name,
                    enabled: !plugin.enabled,
                },
                Pending::Save,
                ctx,
            ),
        }
    }

    fn remove(&mut self, ctx: &mut Ctx) {
        let Some(plugin) = self.current() else {
            return;
        };
        if plugin.origin == Origin::Bundled {
            ctx.say(Message::error(
                "Not removed.",
                &sentence("app_plugin_bundled"),
            ));
            return;
        }
        self.open = Open::Remove {
            name: plugin.name.clone(),
            title: safe(&plugin.title),
        };
    }

    fn key_form(&mut self, ctx: &mut Ctx) {
        let Some(plugin) = self.current() else {
            return;
        };
        if plugin.secrets.is_empty() {
            ctx.say(Message::warning(
                "No key needed.",
                "This plugin declares no secrets.",
            ));
            return;
        }
        let secret = plugin
            .secrets
            .iter()
            .position(|secret| secret.binding == Binding::None)
            .unwrap_or(0);
        self.open = Open::Key(Box::new(KeyForm {
            plugin: plugin.name.clone(),
            title: safe(&plugin.title),
            secrets: plugin
                .secrets
                .iter()
                .map(|secret| secret.name.clone())
                .collect(),
            secret,
            accounts: None,
            choice: 0,
            key: TextField::secret("Key", KEY_CHARS_MAX),
        }));
        self.pending = Some(Pending::Accounts);
        send(ctx, SettingsJob::AllAccounts);
    }

    fn unbind(&mut self, ctx: &mut Ctx) {
        let Some(plugin) = self.current() else {
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
        let Open::Add(form) = &self.open else {
            return;
        };
        if form.on_path {
            let typed = form.path.value().trim().to_owned();
            let path = PathBuf::from(&typed);
            if typed.is_empty() || !path.is_absolute() {
                self.failure = Some(ABSOLUTE.into());
                return;
            }
            self.ask(AppPluginsJob::ReadFile { path }, Pending::Check, ctx);
            return;
        }
        let yaml = form.yaml.value().to_owned();
        if yaml.trim().is_empty() {
            self.failure = Some("Paste a manifest first.".into());
            return;
        }
        if yaml.len() > MANIFEST_BYTES_MAX {
            self.failure = Some(sentence("app_plugin_request_bounds"));
            return;
        }
        self.sent = Some(yaml.clone());
        self.ask(AppPluginsJob::Preview { yaml }, Pending::Check, ctx);
    }

    fn confirm(&mut self, ctx: &mut Ctx) {
        let job = match &self.open {
            Open::Trust {
                yaml: Some(yaml),
                trust,
                ..
            } => AppPluginsJob::Add {
                yaml: yaml.clone(),
                sha256: trust.sha256.clone(),
                replace: trust.replaces,
            },
            Open::Trust {
                yaml: None, trust, ..
            } => AppPluginsJob::Trust {
                name: trust.name.clone(),
                sha256: trust.sha256.clone(),
            },
            Open::Remove { name, .. } => AppPluginsJob::Remove { name: name.clone() },
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
        let Open::Add(form) = &mut self.open else {
            return Flow::Ignored;
        };
        match key {
            Key::Esc => self.open = Open::None,
            Key::Tab | Key::BackTab => form.on_path = !form.on_path,
            Key::Enter if self.pending.is_none() => self.check(ctx),
            other => {
                let field = if form.on_path {
                    &mut form.path
                } else {
                    &mut form.yaml
                };
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
            Key::Esc => self.open = Open::None,
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

    fn apply(&mut self, event: &AppPluginsEvent, ctx: &mut Ctx) {
        let pending = self.pending.take();
        match event {
            AppPluginsEvent::Listing(Ok(listing)) => {
                let was = std::mem::replace(&mut self.open, Open::None);
                if pending == Some(Pending::Save) {
                    ctx.say(Message::success("Saved.", saved_line(&was)));
                }
                self.selected = self.selected.min(listing.plugins.len().saturating_sub(1));
                self.state = State::Ready(listing.clone());
            }
            AppPluginsEvent::Listing(Err(code)) if pending == Some(Pending::List) => {
                self.state = State::Failed(code.clone());
            }
            AppPluginsEvent::Listing(Err(code)) => {
                if let Open::Remove { .. } | Open::Trust { .. } = self.open {
                    self.open = Open::None;
                }
                if matches!(self.open, Open::None) {
                    ctx.say(Message::error("Not saved.", &sentence(code)));
                } else {
                    self.failure = Some(sentence(code));
                }
            }
            AppPluginsEvent::Preview { read, result } => {
                let yaml = match &self.open {
                    Open::Add(_) => read.clone().or_else(|| self.sent.take()),
                    _ => None,
                };
                match result {
                    Ok(Preview::Ready { trust }) => {
                        self.open = Open::Trust {
                            review: yaml.is_none(),
                            yaml,
                            trust: Box::new(trust.clone()),
                        };
                    }
                    Ok(Preview::Refused { problem }) => {
                        self.open = Open::Refused {
                            yaml,
                            problem: problem.clone(),
                        };
                    }
                    Err(code) if matches!(self.open, Open::Add(_)) => {
                        self.failure = Some(sentence(code));
                    }
                    Err(code) => ctx.say(Message::error("Not reviewed.", &sentence(code))),
                }
            }
            AppPluginsEvent::Search(_) | AppPluginsEvent::SearchSaved(_) => {
                self.pending = pending;
            }
        }
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

    fn body(
        &self,
        limit: usize,
        height: usize,
        ctx: &Ctx,
    ) -> (Vec<Line<'static>>, Option<Position>) {
        let theme = &ctx.theme;
        let mut lines: Vec<Line<'static>> = wrap(INTRO, limit)
            .into_iter()
            .map(|part| styled(part, Role::Muted, theme))
            .collect();
        lines.push(Line::default());
        let listing = match &self.state {
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
            State::Ready(listing) => listing,
        };
        let mut cursor = None;
        match &self.open {
            Open::Add(form) => {
                lines.push(styled("Add an app plugin", Role::Heading, theme));
                lines.extend(
                    wrap(
                        "Paste a manifest, or Tab to type the path of a .yaml file. Nothing runs until you confirm its trust screen.",
                        limit,
                    )
                    .into_iter()
                    .map(|part| styled(part, Role::Muted, theme)),
                );
                self.failure_lines(&mut lines, limit, ctx);
                let top = lines.len();
                let (yaml, at) = form.yaml.lines(limit, !form.on_path, theme, ctx.glyphs);
                if let Some(at) = at {
                    cursor = Some(Position::new(at.x, at.y + top as u16));
                }
                lines.extend(yaml);
                let top = lines.len();
                let (path, at) = form.path.lines(limit, form.on_path, theme, ctx.glyphs);
                if let Some(at) = at {
                    cursor = Some(Position::new(at.x, at.y + top as u16));
                }
                lines.extend(path);
                return (lines, cursor);
            }
            Open::Key(form) => {
                lines.push(styled(
                    format!("Key for {}", safe(&form.secrets[form.secret])),
                    Role::Heading,
                    theme,
                ));
                lines.push(styled(
                    format!(
                        "Where {} gets its {}.{}",
                        form.title,
                        safe(&form.secrets[form.secret]),
                        if form.secrets.len() > 1 {
                            " Left and right pick another secret."
                        } else {
                            ""
                        }
                    ),
                    Role::Muted,
                    theme,
                ));
                self.failure_lines(&mut lines, limit, ctx);
                let Some(accounts) = &form.accounts else {
                    lines.push(styled("Reading API-key accounts...", Role::Muted, theme));
                    return (lines, None);
                };
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
                if form.choice == accounts.len() {
                    let top = lines.len();
                    let (key, at) = form.key.lines(limit, true, theme, ctx.glyphs);
                    if let Some(at) = at {
                        cursor = Some(Position::new(at.x, at.y + top as u16));
                    }
                    lines.extend(key);
                    lines.push(styled(
                        "Sent to the engine once and never shown again.",
                        Role::Muted,
                        theme,
                    ));
                }
                return (lines, cursor);
            }
            _ => {}
        }
        if matches!(self.open, Open::None) {
            self.failure_lines(&mut lines, limit, ctx);
        }
        let mut selected_rows = (0, 0);
        for (index, plugin) in listing.plugins.iter().enumerate() {
            let start = lines.len();
            let (word, role, detail) = plugin_state(plugin);
            let selected = index == self.selected;
            lines.push(list_row(
                row(
                    limit.saturating_sub(2),
                    Line::raw(safe(&plugin.title)),
                    styled(word, role, theme),
                    ctx.glyphs,
                ),
                limit,
                selected,
                true,
                theme,
                ctx.glyphs,
            ));
            let origin = match plugin.origin {
                Origin::Bundled => "Built in",
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
                under.push(runs_line(runs));
            }
            under.extend(detail);
            under.extend(plugin.secrets.iter().map(secret_line));
            for text in under {
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
        if listing.more {
            lines.push(styled(
                format!("Showing the first {} plugins.", listing.plugins.len()),
                Role::Muted,
                theme,
            ));
        }
        if selected_rows.1 > height {
            let skip = selected_rows.0.min(lines.len().saturating_sub(height));
            lines.drain(..skip);
        }
        (lines, None)
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
}

impl Default for AppPluginsPage {
    fn default() -> Self {
        Self::new()
    }
}

/// The success line for the change a listing answered. Input: what was open. Output: the
/// line.
fn saved_line(open: &Open) -> &'static str {
    match open {
        Open::Trust { yaml: Some(_), .. } => "The plugin is added.",
        Open::Trust { yaml: None, .. } => "The plugin is trusted.",
        Open::Remove { .. } => "The plugin is removed.",
        Open::Key(_) => "The key is set.",
        _ => "The plugin is updated.",
    }
}

/// The lines of a trust screen: what the plugin is, how it runs, the address it calls, the
/// secrets it needs, its SHA-256 and whether it replaces the owner's plugin. Inputs: the
/// trust screen and the width. Output: the lines.
fn trust_lines(trust: &TrustView, room: usize) -> Vec<Line<'static>> {
    let mut texts = Vec::new();
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
    texts.push(runs_line(&trust.runs));
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
    texts
        .iter()
        .flat_map(|text| wrap(text, room))
        .map(Line::raw)
        .collect()
}

impl Screen for AppPluginsPage {
    fn crumbs(&self) -> Vec<String> {
        vec!["Settings".into(), "App plugins".into()]
    }

    fn hints(&self, ctx: &Ctx) -> Vec<Hint> {
        let vertical = format!("{}{}", ctx.glyphs.up, ctx.glyphs.down);
        match &self.open {
            Open::Add(_) => {
                return vec![
                    hint("Enter", "Check"),
                    hint("Ctrl-J", "New line"),
                    hint("Tab", "Paste or path"),
                    hint("Esc", "Cancel"),
                ];
            }
            Open::Key(_) => {
                return vec![
                    hint(&vertical, "Source"),
                    hint("Enter", "Save"),
                    hint("Esc", "Cancel"),
                ];
            }
            _ => {}
        }
        match &self.state {
            State::Loading => return vec![hint("Esc", "Back")],
            State::Failed(_) => return vec![hint("r", "Try again"), hint("Esc", "Back")],
            State::Ready(_) => {}
        }
        let mut hints = Vec::new();
        if let Some(plugin) = self.current() {
            match plugin.status {
                PluginStatus::NeedsReview => hints.push(hint("Enter", "Review")),
                PluginStatus::Refused { .. } => {}
                _ if plugin.enabled => hints.push(hint("Enter", "Turn off")),
                _ => hints.push(hint("Enter", "Turn on")),
            }
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
        hints.push(hint("a", "Add plugin"));
        hints.push(hint(&vertical, "Move"));
        hints.push(hint("r", "Read again"));
        hints.push(hint("Esc", "Back"));
        hints
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
            Open::Trust { .. } | Open::Remove { .. } => {
                match key {
                    Key::Esc => {
                        self.open = Open::None;
                        self.failure = None;
                    }
                    Key::Char('y') if self.pending.is_none() => self.confirm(ctx),
                    _ => {}
                }
                return Flow::Handled;
            }
            Open::Refused { yaml, .. } => {
                match key {
                    Key::Esc => self.open = Open::None,
                    Key::Char('e') => {
                        if let Some(yaml) = yaml.clone() {
                            let mut form = add_form();
                            form.yaml.set(&yaml);
                            self.open = Open::Add(form);
                        }
                    }
                    _ => {}
                }
                return Flow::Handled;
            }
            Open::None => {}
        }
        let total = self.plugins().len();
        let idle = total > 0 && self.pending.is_none();
        match key {
            Key::Char('r') => self.read(ctx),
            Key::Up => self.selected = self.selected.saturating_sub(1),
            Key::Down => self.selected = (self.selected + 1).min(total.saturating_sub(1)),
            Key::Enter | Key::Char(' ') if idle => self.activate(ctx),
            Key::Char('a') if matches!(self.state, State::Ready(_)) && self.pending.is_none() => {
                self.failure = None;
                self.open = Open::Add(add_form());
            }
            Key::Char('x') if idle => self.remove(ctx),
            Key::Char('k') if idle => self.key_form(ctx),
            Key::Char('u') if idle => self.unbind(ctx),
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
        let mut body = Vec::new();
        if let Some(failure) = &self.failure {
            body.extend(
                wrap(failure, room)
                    .into_iter()
                    .map(|part| styled(part, Role::Error, &ctx.theme)),
            );
        }
        let (title, hints) = match &self.open {
            Open::Trust { trust, review, .. } => {
                body.extend(trust_lines(trust, room));
                (
                    if *review {
                        format!("Review {}", safe(&trust.title))
                    } else {
                        format!("Trust {}?", safe(&trust.title))
                    },
                    vec![hint("y", "Confirm"), hint("Esc", "Cancel")],
                )
            }
            Open::Refused { yaml, problem } => {
                body.extend(
                    wrap(&problem_text(problem), room)
                        .into_iter()
                        .map(|part| styled(part, Role::Error, &ctx.theme)),
                );
                let mut hints = Vec::new();
                if yaml.is_some() {
                    hints.push(hint("e", "Edit manifest"));
                }
                hints.push(hint("Esc", "Cancel"));
                (
                    if yaml.is_some() {
                        "This manifest can't be added".to_owned()
                    } else {
                        "This plugin can't be trusted".to_owned()
                    },
                    hints,
                )
            }
            Open::Remove { title, .. } => {
                body.push(Line::raw(format!("Remove {title}?")));
                body.extend(
                    wrap("Its manifest is deleted from your plugins folder.", room)
                        .into_iter()
                        .map(Line::raw),
                );
                (
                    "Remove".to_owned(),
                    vec![hint("y", "Remove"), hint("Esc", "Cancel")],
                )
            }
            _ => return None,
        };
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
            Open::Add(form) => !form.yaml.is_empty() || !form.path.is_empty(),
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

/// A fresh Add form: a multi-line paste box one character past the engine's bound, so an
/// over-long paste stays whole and is refused, and a path field.
fn add_form() -> AddForm {
    AddForm {
        yaml: TextField::new("Plugin manifest (YAML)", MANIFEST_BYTES_MAX + 1).multiline(),
        path: TextField::new("Or the path of a .yaml file", PATH_CHARS_MAX),
        on_path: false,
    }
}
