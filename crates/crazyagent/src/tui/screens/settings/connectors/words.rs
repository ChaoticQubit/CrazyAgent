use crate::app_plugins::{
    Binding, Origin, PluginStatus, ReadOnlyTrust, RunsView, SecretView, TrustView,
};
use crate::connectors::{
    ConnectorView, NativeConnectors, SignIn, SignInWith, SizeCap, Wait, WritePolicy,
};
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
/// The write actions setting's label (WORDS r6).
pub(in crate::tui) const WRITE_LABEL: &str = "Write actions";
/// The write actions setting's help (WORDS r6).
pub(in crate::tui) const WRITE_HELP: &str = "What happens when an agent calls a connector tool that changes something: each call waits for your Allow, your first Allow in a project lets later calls run, or those tools stay hidden.";
/// The write actions choices in the desktop's order with their words (WORDS r6).
pub(in crate::tui) const WRITE_CHOICES: [(WritePolicy, &str); 3] = [
    (WritePolicy::AskEveryTime, "Ask every time"),
    (
        WritePolicy::AskOncePerProject,
        "Ask once per project and connector",
    ),
    (WritePolicy::ReadOnly, "Read-only"),
];
/// The approval wait setting's label (WORDS r6).
pub(in crate::tui) const WAIT_LABEL: &str = "Approval wait";
/// The approval wait setting's help (WORDS r6).
pub(in crate::tui) const WAIT_HELP: &str =
    "How long a call that changes something waits for your answer before it fails.";
/// The approval wait choices in the desktop's order with their words (WORDS r6).
pub(in crate::tui) const WAIT_CHOICES: [(Wait, &str); 3] = [
    (Wait::Minutes2, "2 min"),
    (Wait::Minutes10, "10 min"),
    (Wait::Minutes30, "30 min"),
];
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
/// The label of the way a connector that offers a key and a browser sign-in signs in, the
/// desktop's words (QUESTION-23:34 Q1).
pub(in crate::tui) const SIGN_IN_WITH: &str = "Sign in with";
/// The line under Sign in with, the desktop's words (QUESTION-23:34 Q1).
pub(in crate::tui) const SIGN_IN_WITH_HELP: &str =
    "Only the way you choose is used; the other stays saved.";
/// The line `k` says on a connector that signs in with the browser now (QUESTION-23:34 Q1).
pub(in crate::tui) const BROWSER_NO_KEY: &str =
    "This connector signs in with the browser now. Press w to use its key.";
/// The action that starts a sign-in through the browser, the desktop's words (Task 10).
pub(in crate::tui) const SIGN_IN: &str = "Sign in";
/// The action that ends a connector's sign-in on this computer, the desktop's words.
pub(in crate::tui) const SIGN_OUT: &str = "Sign out";
/// The line while a sign-in waits for the owner's browser, the desktop's words.
pub(in crate::tui) const WAITING: &str = "Waiting for the browser";
/// The line under a signed-in connector, the desktop's words.
pub(in crate::tui) const SIGN_OUT_HELP: &str =
    "Sign out forgets the sign-in on this computer; the vendor is not told.";
/// The line a cancelled sign-in ends with, the desktop's words.
pub(in crate::tui) const SIGN_IN_CANCELLED: &str = "The sign-in was cancelled.";
/// The lead when where a sign-in stands could not be read, the desktop's words.
pub(in crate::tui) const NOT_READ_SIGN_IN: &str = "Couldn't read the sign-in.";
/// The lead when this machine's opener did not take a sign-in's address (TUI only).
pub(in crate::tui) const NOT_OPENED: &str = "The browser did not open.";
/// The line above a waiting sign-in's address, shown for copying (TUI only).
pub(in crate::tui) const OPEN_ADDRESS: &str = "If the browser did not open, go to this address:";
/// The own app section's title, the desktop's words (addendum 30 Q4 A).
pub(in crate::tui) const OWN_APP: &str = "Your own OAuth app";
/// The line under the own app section and form, the desktop's words.
pub(in crate::tui) const OWN_APP_HELP: &str = "Use an OAuth app you registered with the vendor. Saving or removing it signs the connector out.";
/// The line while no own app is saved, the desktop's words.
pub(in crate::tui) const OWN_APP_NONE: &str = "No app saved.";
/// The own app form's client id field, the desktop's words.
pub(in crate::tui) const CLIENT_ID: &str = "Client id";
/// The own app form's secret field, the desktop's words.
pub(in crate::tui) const CLIENT_SECRET: &str = "Client secret (empty for a public app)";
/// The line under the secret field, the desktop's words.
pub(in crate::tui) const CLIENT_SECRET_HELP: &str =
    "The secret is sent to the engine once and never shown again.";
/// The form's line when Save is pressed with no client id, the desktop's words.
pub(in crate::tui) const ENTER_CLIENT_ID: &str = "Enter a client id.";
/// The form's line when Save is pressed with the secret field empty after a typed secret
/// left it, so a refusal never turns into a public app (rv-code2 F1), the desktop's words.
pub(in crate::tui) const RETYPE_SECRET: &str = "Type the client secret again.";
/// The action that opens the own app form, the desktop's words.
pub(in crate::tui) const USE_OWN_APP: &str = "Use my own app";
/// The action that removes a saved own app, the desktop's words.
pub(in crate::tui) const REMOVE_OWN_APP: &str = "Remove my app";
/// The connector page's way back to the catalog (WORDS r4).
pub(in crate::tui) const BACK: &str = "All connectors";
/// The add dialog's title (WORDS r4).
pub(in crate::tui) const ADD_TITLE: &str = "Add a connector";
/// The add dialog's manifest field (WORDS r4).
pub(in crate::tui) const ADD_FIELD: &str = "Connector manifest (YAML)";
/// The add dialog's helper line (WORDS r4).
pub(in crate::tui) const ADD_HELP: &str =
    "Paste a manifest or pick a .yaml file. Nothing is used until you confirm its trust screen.";
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

const REFUSALS: [(&str, &str); 19] = [
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
        "connector_approval_closed",
        "This write was already answered or its wait ran out. The page now shows where it stands.",
    ),
    ("connector_approval_unknown", "That approval is gone."),
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
/// desktop's order (WORDS r11): Refused (with its problem), Needs review, Unavailable (a file
/// whose only way in this build cannot use), Needs a key, Needs sign-in (the browser way
/// with no sign-in yet), then Ready. Input: the connector. Output: the word, role and line.
pub(in crate::tui) fn state(connector: &ConnectorView) -> (&'static str, Role, Option<String>) {
    match &connector.status {
        PluginStatus::Refused { problem } => ("Refused", Role::Error, Some(problem_text(problem))),
        PluginStatus::NeedsReview => (
            "Needs review",
            Role::Warning,
            Some("Review this connector before a project can use it.".into()),
        ),
        _ if connector.sign_in == SignIn::NotYet => {
            ("Unavailable", Role::Warning, Some(OAUTH_NOT_YET.into()))
        }
        PluginStatus::NeedsKey => (
            "Needs a key",
            Role::Warning,
            Some("Set a key for each secret it needs.".into()),
        ),
        PluginStatus::NeedsSignIn => (
            "Needs sign-in",
            Role::Warning,
            Some("Sign in on its page before a project can use it.".into()),
        ),
        PluginStatus::Ready => (READY, Role::Success, None),
    }
}

/// The way a connector whose file offers a key and a browser sign-in signs in now, or
/// `None` for a connector with one way, which has nothing to switch. Input: the connector.
/// Output: the way.
pub(in crate::tui) fn chosen_way(connector: &ConnectorView) -> Option<SignInWith> {
    match (&connector.sign_in, &connector.oauth) {
        (SignIn::Both, Some(oauth)) => Some(oauth.using),
        _ => None,
    }
}

/// Whether a connector's page shows and sets its keys, as on the desktop: one that signs in
/// with a key, or offers both and signs in with its key now. Input: the connector. Output:
/// the answer.
pub(in crate::tui) fn keys_shown(connector: &ConnectorView) -> bool {
    connector.sign_in == SignIn::Key || chosen_way(connector) == Some(SignInWith::Key)
}

/// The secrets a connector's page lists: every one while it signs in with a key, and under
/// Browser only those with a saved key, so a key can be cleared without making it live
/// first (rv-sec2 F1). Input: the connector. Output: the secrets in their declared order.
pub(in crate::tui) fn secrets_shown(connector: &ConnectorView) -> Vec<&SecretView> {
    let keys = keys_shown(connector);
    let browser = chosen_way(connector) == Some(SignInWith::Browser);
    connector
        .secrets
        .iter()
        .filter(|secret| keys || (browser && secret.binding != Binding::None))
        .collect()
}

/// A way's word, the desktop's choice label. Input: the way. Output: "Key" or "Browser".
pub(in crate::tui) fn way_word(way: SignInWith) -> &'static str {
    match way {
        SignInWith::Key => "Key",
        SignInWith::Browser => "Browser",
    }
}

/// The way `w` switches a connector that offers both to. Input: the way now. Output: the
/// other way.
pub(in crate::tui) fn other_way(way: SignInWith) -> SignInWith {
    match way {
        SignInWith::Key => SignInWith::Browser,
        SignInWith::Browser => SignInWith::Key,
    }
}

/// The line a chosen way is confirmed with, the desktop's `signsInWithLine`. Inputs: the
/// connector's title and the way. Output: such as "GitHub signs in with Browser.".
pub(in crate::tui) fn signs_in_with(title: &str, way: SignInWith) -> String {
    format!("{} signs in with {}.", safe(title), way_word(way))
}

/// Whether a connector signs in through the browser now: one whose file offers only that,
/// or offers both and signs in with the browser. Input: the connector. Output: the answer.
pub(in crate::tui) fn browser_now(connector: &ConnectorView) -> bool {
    connector.sign_in == SignIn::Browser || chosen_way(connector) == Some(SignInWith::Browser)
}

/// Whether a connector holds a browser sign-in, whichever way it signs in now, so Sign out
/// never needs a switch that makes it live first (rv-sec2 F1). Input: the connector. Output:
/// the answer.
pub(in crate::tui) fn signed_in(connector: &ConnectorView) -> bool {
    connector
        .oauth
        .as_ref()
        .is_some_and(|oauth| oauth.signed_in)
}

/// The line a finished sign-in says, the desktop's words. Input: the connector's title.
/// Output: "<title> is signed in.".
pub(in crate::tui) fn signed_in_line(title: &str) -> String {
    format!("{} is signed in.", safe(title))
}

/// The redirect an own app registers, with any port, the desktop's words. Input: the
/// engine's redirect. Output: "Redirect: <redirect> (any port)".
pub(in crate::tui) fn redirect_line(redirect: &str) -> String {
    format!("Redirect: {} (any port)", safe(redirect))
}

/// The own app form's heading, the desktop's words. Input: the connector's title. Output:
/// "Your own OAuth app for <title>".
pub(in crate::tui) fn own_app_heading(title: &str) -> String {
    format!("Your own OAuth app for {}", safe(title))
}

/// A saved own app's client id line, the desktop's words. Input: the client id. Output:
/// "Client id: <id>".
pub(in crate::tui) fn client_id_line(client_id: &str) -> String {
    format!("Client id: {}", safe(client_id))
}

/// Whether a saved own app holds a secret, never the secret, the desktop's words. Input:
/// whether one is saved. Output: the line.
pub(in crate::tui) fn client_secret_line(saved: bool) -> &'static str {
    if saved {
        "Client secret: saved"
    } else {
        "Client secret: none"
    }
}

/// The line Save app confirms with, the desktop's words. Input: the connector's title.
/// Output: "<title> signs in with your own app.".
pub(in crate::tui) fn own_app_saved(title: &str) -> String {
    format!("{} signs in with your own app.", safe(title))
}

/// The line Remove my app confirms with, the desktop's words. Input: the connector's title.
/// Output: "<title> no longer uses your own app.".
pub(in crate::tui) fn own_app_removed(title: &str) -> String {
    format!("{} no longer uses your own app.", safe(title))
}

/// The line on another connector's page while one sign-in waits, the desktop's words
/// (rv-code CONN-T10-PAGES-A F1). Input: the waiting connector's title. Output: the line.
pub(in crate::tui) fn sign_in_first(title: &str) -> String {
    format!("Finish or cancel the {} sign-in first.", safe(title))
}

/// The line a sign-out confirms with, the desktop's words. Input: the connector's title.
/// Output: "<title> is signed out.".
pub(in crate::tui) fn signed_out_line(title: &str) -> String {
    format!("{} is signed out.", safe(title))
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
            lines.push(format!(
                "Scopes while reading: {}",
                listed(&oauth.scopes_read)
            ));
            lines.push(format!(
                "Scopes when writing: {}",
                listed(&oauth.scopes_write)
            ));
        }
        lines.push(format!(
            "Tools it says only read: {}",
            listed(&block.read_tools)
        ));
    }
    lines.push(format!("Secrets it needs: {}", listed(&trust.secrets)));
    lines.push(format!("SHA-256 {}", safe(&trust.sha256)));
    if trust.replaces {
        lines.push(format!(
            "Replaces your connector named {}.",
            safe(&trust.name)
        ));
    }
    assert!(lines.len() >= 4, "version, reaches, secrets and the hash");
    lines
}
