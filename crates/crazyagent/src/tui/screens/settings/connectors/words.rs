use crate::app_plugins::{
    Binding, Origin, PluginStatus, ReadOnlyTrust, RunsView, SecretView, TrustView,
};
use crate::connectors::{ConnectorView, NativeConnectors, SignIn, SizeCap};
use crate::tui::screens::settings::app_plugins::{problem_text, safe};
use crate::tui::screens::settings::connector_activity::problem_words;
use crate::tui::theme::Role;

/// The page's purpose line under its title (WORDS r1).
pub(in crate::tui) const PURPOSE: &str = "Vendor MCP servers your projects can use through the engine, which keeps each key. A connector reaches no project until you turn it on in that project's Settings, Connectors.";
/// The catalog section's title (WORDS r4).
pub(in crate::tui) const CATALOG: &str = "Catalog";
/// The line under the catalog's title (WORDS r4).
pub(in crate::tui) const CATALOG_ABOUT: &str = "Built-in connectors ship with CrazyAgent. Yours live in your plugins folder and are used only while their file matches what you trusted.";
/// The catalog's add action (WORDS r4).
pub(in crate::tui) const ADD: &str = "Add connector";
/// The harness section's title (WORDS r5); its rows are connectors::launch::reach's words.
pub(in crate::tui) const HARNESSES: &str = "Harnesses";
/// The line under the harness section's title (WORDS r5).
pub(in crate::tui) const HARNESSES_ABOUT: &str =
    "Which harnesses reach connectors through the engine.";
/// The settings section's title (WORDS r4).
pub(in crate::tui) const SETTINGS: &str = "Connector settings";
/// The harness-native setting's label (WORDS r1).
pub(in crate::tui) const NATIVE_LABEL: &str = "Harness-native connectors";
/// The harness-native setting's help (WORDS r1).
pub(in crate::tui) const NATIVE_HELP: &str = "Connectors from the harness's own account (claude.ai connectors in Claude Code, ChatGPT apps in Codex). The engine neither filters nor logs them.";
/// The harness-native choices in the desktop's order with their words (WORDS r1).
pub(in crate::tui) const NATIVE_CHOICES: [(NativeConnectors, &str); 2] =
    [(NativeConnectors::Off, "Off"), (NativeConnectors::On, "On")];
/// The size cap setting's label (WORDS r1).
pub(in crate::tui) const SIZE_LABEL: &str = "Tool result size cap";
/// The size cap setting's help (WORDS r1).
pub(in crate::tui) const SIZE_HELP: &str =
    "A longer tool result is cut to this size, with a note, before an agent reads it.";
/// The size cap choices in the desktop's order with their words (WORDS r1).
pub(in crate::tui) const SIZE_CHOICES: [(SizeCap, &str); 3] = [
    (SizeCap::Kib64, "64 KiB"),
    (SizeCap::Kib256, "256 KiB"),
    (SizeCap::Mib1, "1 MiB"),
];
/// The locked line's label, value and reason (WORDS r1).
pub(in crate::tui) const LOCKED_LABEL: &str = "Tool list check and third-party label";
/// The locked line's value (WORDS r1).
pub(in crate::tui) const LOCKED_VALUE: &str = "Always on";
/// Why the locked line cannot be turned off (WORDS r1).
pub(in crate::tui) const LOCKED_REASON: &str = "A connector whose vendor tools changed stops and shows a notice, and every vendor result starts with a line saying it is content, not instructions.";
/// The save action of an unsaved settings pick (WORDS r1).
pub(in crate::tui) const SAVE: &str = "Save changes";
/// The toast of a saved settings pick (WORDS r1).
pub(in crate::tui) const SAVED: &str = "Connector settings saved.";
/// The line of a save that met newer settings (WORDS r1).
pub(in crate::tui) const STALE: &str =
    "The connector settings changed elsewhere. Check them and try again.";
/// The line a connector that only signs in with OAuth shows in place of its keys.
pub(in crate::tui) const OAUTH_NOT_YET: &str =
    "Signs in with OAuth, which this build does not have.";
/// The connector page's way back to the catalog (WORDS r4).
pub(in crate::tui) const BACK: &str = "All connectors";
/// The add dialog's title (WORDS r4).
pub(in crate::tui) const ADD_TITLE: &str = "Add a connector";
/// The add dialog's manifest field (WORDS r4).
pub(in crate::tui) const ADD_FIELD: &str = "Connector manifest (YAML)";
/// The add dialog's helper line (WORDS r4).
pub(in crate::tui) const ADD_HELP: &str = "Paste a manifest or pick a .yaml file. Nothing is used until you confirm its trust screen.";
/// The add dialog's check action and its busy line (WORDS r4).
pub(in crate::tui) const CHECK: &str = "Check";
/// The status line while a manifest is checked (WORDS r4).
pub(in crate::tui) const CHECKING: &str = "Checking...";
/// The key dialog's field (WORDS r4).
pub(in crate::tui) const KEY_FIELD: &str = "Key";
/// The key dialog's helper line (WORDS r4).
pub(in crate::tui) const KEY_HELP: &str = "Sent to the engine once and never shown again.";
/// The key dialog's line for an empty key (WORDS r4).
pub(in crate::tui) const KEY_EMPTY: &str = "Enter a key.";
/// The remove dialog's body (WORDS r4).
pub(in crate::tui) const REMOVE_BODY: &str = "Its manifest is deleted from your plugins folder, with its keys and its switch in every project.";
/// The refused dialog's title for a pasted or picked manifest (WORDS r4).
pub(in crate::tui) const REFUSED_PASTED: &str = "This manifest can't be added";
/// The refused dialog's title for a listed connector under review (WORDS r4).
pub(in crate::tui) const REFUSED_LISTED: &str = "This connector can't be trusted";
/// The code a ready trust screen with no connector block is refused under (WORDS r4).
pub(in crate::tui) const NOT_CONNECTOR: &str = "app_plugin_not_connector";
/// The state word of a connector a project can turn on (WORDS r1).
pub(in crate::tui) const READY: &str = "Ready";
/// The line under a refused connector file on its row and its page (WORDS r7).
pub(in crate::tui) const REFUSED_FILE: &str = "Fix its file, or remove it on the App plugins page.";

/// The line a y on a trust screen not yet read to its end says (rv-ds241 T5 F1).
pub(in crate::tui) const READ_TO_THE_END: &str =
    "Read the trust screen to its end first, with Down or PgDn.";

const REFUSALS: [(&str, &str); 17] = [
    (
        "app_plugin_bundled",
        "Built-in connectors can't be removed or replaced.",
    ),
    (
        "app_plugin_changed",
        "The manifest changed after you checked it. Check it again before you trust it.",
    ),
    (
        "app_plugin_file_not_text",
        "That file isn't text. A manifest is a UTF-8 .yaml file.",
    ),
    (
        "app_plugin_file_unreadable",
        "CrazyAgent couldn't read that file. Pick a manifest file you can open.",
    ),
    (
        "app_plugin_folder_unavailable",
        "CrazyAgent couldn't open your plugins folder. Try again.",
    ),
    (
        "app_plugin_folder_unsafe",
        "Your plugins folder isn't private to you, so CrazyAgent won't read it. Make it yours alone and try again.",
    ),
    (
        "app_plugin_key_invalid",
        "That key can't be used: it is empty, too long or has spaces or control characters.",
    ),
    (
        "app_plugin_key_store_unavailable",
        "CrazyAgent couldn't save the key. Nothing was saved; try again.",
    ),
    (
        "app_plugin_name_taken",
        "A built-in plugin or connector already has that name. Give yours another name.",
    ),
    (
        "app_plugin_needs_key",
        "This connector needs a key first. Set one on its page in Settings, Connectors.",
    ),
    (
        "app_plugin_needs_review",
        "This connector's file changed or is new. Review it in Settings, Connectors first.",
    ),
    (
        "app_plugin_not_found",
        "That connector is gone. The page now shows the current ones.",
    ),
    (
        "app_plugin_refused",
        "CrazyAgent refused this manifest. Its reason is shown with it.",
    ),
    (
        "app_plugin_secret_not_declared",
        "This connector doesn't declare that secret.",
    ),
    (
        "app_plugins_unavailable",
        "Connectors aren't available in this engine yet.",
    ),
    (
        "connector_project_unknown",
        "That project is not set up here any more.",
    ),
    (
        "owner_entropy_unavailable",
        "This computer couldn't make a random value for the request. Nothing changed; try again.",
    ),
];

/// The plain sentence for a refused Connectors request: the desktop's words for the
/// connector protocol codes (WORDS r4), else the engine's connector sentence. Input: the
/// code. Output: the sentence.
pub(in crate::tui) fn refusal(code: &str) -> String {
    let words = match REFUSALS.iter().find(|(known, _)| *known == code) {
        Some((_, words)) => (*words).to_owned(),
        None => problem_words(code),
    };
    assert!(!words.is_empty(), "every refusal has words");
    words
}

/// The hosts a connector reaches, made safe for one row. Input: how it runs. Output: the
/// hosts in their declared order; none for a plugin that runs on this computer.
pub(in crate::tui) fn hosts(runs: &RunsView) -> Vec<String> {
    match runs {
        RunsView::Connector { hosts, .. } | RunsView::Http { hosts, .. } => {
            hosts.iter().map(|host| safe(host)).collect()
        }
        RunsView::Local { .. } => Vec::new(),
    }
}

/// The line naming every host a connector reaches, the same on its row, its page and its
/// trust screen. Input: how it runs. Output: "Reaches <host>, <host>".
pub(in crate::tui) fn reaches_line(runs: &RunsView) -> String {
    format!("Reaches {}", hosts(runs).join(", "))
}

/// A connector's name, version and where it came from, as its row and page show them; a
/// refused file read no version, so its line has none (WORDS r7). Input: the connector.
/// Output: such as "linear, version 1.0.0. Built in." or "broken. Added by you.".
pub(in crate::tui) fn origin_line(connector: &ConnectorView) -> String {
    let origin = match connector.origin {
        Origin::Bundled => "Built in",
        Origin::User => "Added by you",
    };
    let name = safe(&connector.name);
    let line = if connector.version.is_empty() {
        format!("{name}. {origin}.")
    } else {
        format!("{name}, version {}. {origin}.", safe(&connector.version))
    };
    assert!(line.starts_with(&name) && line.ends_with('.'));
    line
}

/// A connector's one state word with its role and the line under it, checked in the
/// desktop's order (WORDS r11): Refused (with its problem), Needs review, Unavailable (it
/// signs in with OAuth, which the engine lists Ready with no key), Needs a key, then Ready.
/// Input: the connector. Output: the word, its role and the line, if any.
pub(in crate::tui) fn state(connector: &ConnectorView) -> (&'static str, Role, Option<String>) {
    match &connector.status {
        PluginStatus::Refused { problem } => ("Refused", Role::Error, Some(problem_text(problem))),
        PluginStatus::NeedsReview => (
            "Needs review",
            Role::Warning,
            Some("Review this connector before a project can use it.".into()),
        ),
        _ if connector.sign_in == SignIn::NotYet => (
            "Unavailable",
            Role::Warning,
            Some(OAUTH_NOT_YET.into()),
        ),
        PluginStatus::NeedsKey => (
            "Needs a key",
            Role::Warning,
            Some("Set a key for each secret it needs.".into()),
        ),
        PluginStatus::Ready => (READY, Role::Success, None),
    }
}

/// Where one secret's key comes from, never the key. Input: the secret. Output: the line.
pub(in crate::tui) fn secret_line(secret: &SecretView) -> String {
    let name = safe(&secret.name);
    match &secret.binding {
        Binding::Account { name: account, .. } => format!("{name}: Key from {}", safe(account)),
        Binding::Key => format!("{name}: Key you entered"),
        Binding::None => format!("{name}: Not set"),
    }
}

fn listed(items: &[String]) -> String {
    if items.is_empty() {
        return "none".to_owned();
    }
    items
        .iter()
        .map(|item| safe(item))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The lines of a connector's trust screen in the desktop's order (WORDS r4). Input: the
/// trust screen. Output: one sentence per line, before wrapping; a screen with no
/// connector block shows only the lines every plugin has.
pub(in crate::tui) fn trust_lines(trust: &TrustView) -> Vec<String> {
    assert!(!trust.sha256.is_empty());
    let mut lines = vec![format!("Version {}", safe(&trust.version))];
    if let RunsView::Connector { url, .. } | RunsView::Http { url, .. } = &trust.runs {
        lines.push(format!("Address: {}", safe(url)));
    }
    lines.push(reaches_line(&trust.runs));
    if let Some(block) = &trust.connector {
        lines.push(match &block.read_only {
            ReadOnlyTrust::Url { url } => format!("Read-only address: {}", safe(url)),
            ReadOnlyTrust::Header { name, value } => {
                format!("Read-only header: {}: {}", safe(name), safe(value))
            }
            ReadOnlyTrust::None => "Read-only mode at the vendor: none".to_owned(),
        });
        lines.push(match &block.key {
            Some(key) if key.scheme.is_empty() => format!(
                "Key: the {} header, from {}",
                safe(&key.header),
                listed(&key.secrets)
            ),
            Some(key) => format!(
                "Key: the {} header with {}, from {}",
                safe(&key.header),
                safe(&key.scheme),
                listed(&key.secrets)
            ),
            None => "Key: none".to_owned(),
        });
        if let Some(oauth) = &block.oauth {
            lines.push(format!("Signs in on {}", listed(&oauth.hosts)));
            let ways: Vec<String> = oauth.registration.iter().map(|way| safe(way)).collect();
            lines.push(format!("Registers by {}", ways.join(", then ")));
            lines.push(format!("Scopes while reading: {}", listed(&oauth.scopes_read)));
            lines.push(format!("Scopes when writing: {}", listed(&oauth.scopes_write)));
        }
        lines.push(format!("Tools it says only read: {}", listed(&block.read_tools)));
    }
    lines.push(format!("Secrets it needs: {}", listed(&trust.secrets)));
    lines.push(format!("SHA-256 {}", safe(&trust.sha256)));
    if trust.replaces {
        lines.push(format!("Replaces your connector named {}.", safe(&trust.name)));
    }
    assert!(lines.len() >= 4, "version, reaches, secrets and the hash");
    lines
}
