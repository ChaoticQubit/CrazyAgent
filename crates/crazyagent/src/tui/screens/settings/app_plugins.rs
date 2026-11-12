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

/// Settings, App plugins, not built yet: it draws nothing and sends nothing.
pub(in crate::tui) struct AppPluginsPage;

impl AppPluginsPage {
    /// The page. Output: the screen.
    pub fn new() -> Self {
        Self
    }
}

impl Screen for AppPluginsPage {
    fn crumbs(&self) -> Vec<String> {
        vec!["Settings".into(), "App plugins".into()]
    }

    fn hints(&self, _ctx: &Ctx) -> Vec<Hint> {
        Vec::new()
    }

    fn key(&mut self, _key: &Key, _ctx: &mut Ctx) -> Flow {
        Flow::Ignored
    }

    fn draw(&mut self, _frame: &mut Frame, _area: Rect, _ctx: &Ctx) -> Option<Position> {
        None
    }

    fn place(&self) -> Option<Place> {
        Some(Place::AppPlugins)
    }
}
