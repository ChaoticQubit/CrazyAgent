use super::words::{refusal, state, trust_lines};
use super::*;
use crate::app_plugins::{
    Binding, ConnectorTrust, KeyTrust, OAuthTrust, Origin, PluginKind, PluginStatus, Preview,
    Problem, ReadOnlyTrust, RunsView, SecretView, TrustView,
};
use crate::connectors::sign_in::SignInState;
use crate::connectors::{
    ConnectorSettings, ConnectorView, ConnectorsView, NativeConnectors, OAuthView, OwnAppView,
    SignIn, SignInWith, SizeCap, Wait, WritePolicy,
};
use crate::engine::owner::ConnectorsSaved;
use crate::tui::client::OwnerEvent;
use crate::tui::client::connectors::{ConnectorsEvent, ConnectorsJob, SignInStarted};
use crate::tui::client::settings::{SettingsEvent, SettingsJob};
use crate::tui::render_tests::{check, ready, render, shell_at};
use crate::tui::screens::open;
use crate::tui::seam::{Arrival, Request};
use crate::tui::shell::{Shell, targets};
use crate::tui::theme::Role;

const SHA: &str = "c0ffee";
const TYPED_KEY: &str = "lin-secret-typed";
const LINK: &str = "https://vendor.example/authorize?state=s1";
const APP_SECRET: &str = "gh-app-secret-typed";

fn secret(name: &str, binding: Binding) -> SecretView {
    SecretView {
        name: name.into(),
        binding,
    }
}

fn connector(
    name: &str,
    title: &str,
    origin: Origin,
    status: PluginStatus,
    host: &str,
    secret: SecretView,
) -> ConnectorView {
    ConnectorView {
        name: name.into(),
        title: title.into(),
        version: if origin == Origin::User {
            "0.2.0"
        } else {
            "1.0.0"
        }
        .into(),
        description: format!("Issues and projects in {title}."),
        origin,
        status,
        sign_in: SignIn::Key,
        oauth: None,
        runs: Some(RunsView::Connector {
            url: format!("https://{host}/mcp"),
            hosts: vec![host.into()],
            oauth_hosts: Vec::new(),
            vendor_read_only: false,
        }),
        secrets: vec![secret],
        sha256: SHA.into(),
    }
}

fn view() -> ConnectorsView {
    ConnectorsView {
        connectors: vec![
            connector(
                "linear",
                "Linear",
                Origin::Bundled,
                PluginStatus::NeedsKey,
                "mcp.linear.app",
                secret("api_key", Binding::None),
            ),
            connector(
                "github",
                "GitHub",
                Origin::Bundled,
                PluginStatus::Ready,
                "api.githubcopilot.com",
                secret("token", Binding::Key),
            ),
            connector(
                "board",
                "Board",
                Origin::User,
                PluginStatus::NeedsReview,
                "board.example.com",
                secret("api_key", Binding::None),
            ),
        ],
        more: false,
        settings: ConnectorSettings::DEFAULT,
        version: 7,
        project: None,
        grants: Vec::new(),
    }
}

fn trust(oauth: bool) -> TrustView {
    TrustView {
        name: "board".into(),
        title: "Board".into(),
        version: "0.2.0".into(),
        description: "Cards on the team board.".into(),
        kind: PluginKind::Connector,
        runs: RunsView::Connector {
            url: "https://board.example.com/mcp".into(),
            hosts: vec!["board.example.com".into(), "files.example.com".into()],
            oauth_hosts: Vec::new(),
            vendor_read_only: true,
        },
        secrets: vec!["api_key".into()],
        sha256: SHA.into(),
        replaces: false,
        connector: Some(Box::new(ConnectorTrust {
            read_only: ReadOnlyTrust::Header {
                name: "X-Read-Only".into(),
                value: "true".into(),
            },
            key: Some(KeyTrust {
                header: "Authorization".into(),
                scheme: "Bearer".into(),
                secrets: vec!["api_key".into()],
            }),
            oauth: oauth.then(|| OAuthTrust {
                hosts: vec!["auth.example.com".into()],
                registration: vec!["cimd".into(), "dcr".into()],
                scopes_read: vec!["read".into()],
                scopes_write: Vec::new(),
            }),
            read_tools: vec!["list_cards".into()],
        })),
    }
}

fn requests(shell: &mut Shell) -> Vec<ConnectorsJob> {
    std::iter::from_fn(|| shell.ctx.next_request())
        .filter_map(|request| match request {
            Request::Settings(job) => match *job {
                SettingsJob::Connectors(job) => Some(*job),
                _ => panic!("the page sent another Settings request"),
            },
            _ => None,
        })
        .collect()
}

fn one(shell: &mut Shell) -> ConnectorsJob {
    let mut sent = requests(shell);
    assert_eq!(sent.len(), 1, "one request at a time");
    sent.remove(0)
}

fn arrive(shell: &mut Shell, event: ConnectorsEvent) {
    shell.arrive(Arrival::Owner(Box::new(OwnerEvent::Settings(Box::new(
        SettingsEvent::Connectors(Box::new(event)),
    )))));
}

fn read_all(job: &ConnectorsJob) -> bool {
    matches!(job, ConnectorsJob::View { project: None })
}

fn page(read: ConnectorsView) -> Shell {
    let mut shell = shell_at(ready(80, 24), Place::Connectors);
    assert!(read_all(&one(&mut shell)), "the page reads every connector");
    arrive(&mut shell, ConnectorsEvent::Page(Ok(read)));
    shell
}

fn shows(shell: &mut Shell, text: &str) -> bool {
    render(shell, 80, 24).iter().any(|row| row.contains(text))
}

fn said(shell: &Shell) -> String {
    shell
        .ctx
        .message()
        .map(|message| format!("{} {}", message.lead, message.rest))
        .unwrap_or_default()
}

fn keys(shell: &mut Shell, keys: &[Key]) {
    for key in keys {
        shell.key(key.clone());
    }
}

fn typed(shell: &mut Shell, text: &str) {
    for character in text.chars() {
        shell.key(Key::Char(character));
    }
}

fn with_secret(binding: Binding, at: usize) -> ConnectorsView {
    let mut read = view();
    read.connectors[at].secrets[0].binding = binding;
    read
}

/// The page with GitHub offering a key and a browser sign-in, signing in the way given.
fn both(way: SignInWith) -> ConnectorsView {
    let mut read = view();
    read.connectors[1].sign_in = SignIn::Both;
    if way == SignInWith::Browser {
        read.connectors[1].status = PluginStatus::NeedsSignIn;
    }
    read.connectors[1].oauth = Some(OAuthView {
        signed_in: false,
        own_app_allowed: true,
        own_app: None,
        redirect: "http://127.0.0.1/callback".into(),
        using: way,
    });
    read
}

/// The page with GitHub offering both ways and two secrets, like Atlassian, signing in the
/// way given.
fn both_two_secrets(way: SignInWith) -> ConnectorsView {
    let mut read = both(way);
    let site = secret("site", Binding::None);
    read.connectors[1].secrets.push(site);
    read
}

/// The page with Notion last, a connector that signs in only through the browser, signed in
/// or not.
fn browser(signed_in: bool) -> ConnectorsView {
    let mut read = view();
    let status = if signed_in {
        PluginStatus::Ready
    } else {
        PluginStatus::NeedsSignIn
    };
    let none = secret("none", Binding::None);
    let mut notion = connector(
        "notion",
        "Notion",
        Origin::Bundled,
        status,
        "mcp.notion.com",
        none,
    );
    notion.secrets.clear();
    notion.sign_in = SignIn::Browser;
    notion.oauth = Some(OAuthView {
        signed_in,
        own_app_allowed: false,
        own_app: None,
        redirect: "http://127.0.0.1/callback".into(),
        using: SignInWith::Browser,
    });
    read.connectors.push(notion);
    read
}

/// Notion's own page, signed in or not.
fn notion(signed_in: bool) -> Shell {
    let mut shell = page(browser(signed_in));
    keys(&mut shell, &[Key::Down, Key::Down, Key::Down, Key::Enter]);
    shell
}

fn started(opened: bool) -> ConnectorsEvent {
    let url = LINK.to_owned();
    ConnectorsEvent::SignIn {
        name: "notion".into(),
        result: Ok(SignInStarted { url, opened }),
    }
}

fn stands(result: Result<SignInState, String>) -> ConnectorsEvent {
    ConnectorsEvent::SignInState {
        name: "notion".into(),
        result,
    }
}

fn reads_state(job: &ConnectorsJob) -> bool {
    matches!(job, ConnectorsJob::SignInState { name } if name == "notion")
}

/// Notion's page after `s` started a sign-in, waiting for the browser that opened.
fn waiting() -> Shell {
    let mut shell = notion(false);
    keys(&mut shell, &[Key::Char('s')]);
    assert!(matches!(
        one(&mut shell),
        ConnectorsJob::SignIn { ref name } if name == "notion"
    ));
    arrive(&mut shell, started(true));
    shell
}

fn refused_file(name: &str, problem: Problem) -> ConnectorView {
    ConnectorView {
        name: name.into(),
        title: name.into(),
        version: String::new(),
        description: String::new(),
        origin: Origin::User,
        status: PluginStatus::Refused { problem },
        sign_in: SignIn::NotYet,
        oauth: None,
        runs: None,
        secrets: Vec::new(),
        sha256: SHA.into(),
    }
}

fn not_https() -> Problem {
    Problem {
        code: "app_plugin_url_not_https".into(),
        line: Some(7),
        field: Some("connector.url".into()),
        message: "The address must start with https://.".into(),
    }
}

fn taken() -> Problem {
    Problem {
        code: "app_plugin_name_taken".into(),
        line: None,
        field: Some("name".into()),
        message: "a plugin with this name already exists; remove it or choose replace.".into(),
    }
}

#[test]
fn the_catalog_lists_every_connector_then_the_harnesses_and_the_two_settings() {
    let mut shell = page(view());
    check("connectors-80", &render(&mut shell, 80, 24), &[]);
    keys(&mut shell, &[Key::End]);
    check("connectors-settings-80", &render(&mut shell, 80, 24), &[]);
    let mut more = view();
    more.more = true;
    let mut shell = page(more);
    keys(&mut shell, &[Key::End]);
    assert!(shows(&mut shell, "Showing the first 3 connectors."));
}

#[test]
fn enter_opens_a_connector_s_page_and_esc_goes_back_to_all_connectors() {
    let mut shell = page(view());
    keys(&mut shell, &[Key::Enter]);
    assert!(requests(&mut shell).is_empty(), "the page is already read");
    check("connector-detail-80", &render(&mut shell, 80, 24), &[]);
    keys(&mut shell, &[Key::Esc]);
    assert!(shows(&mut shell, "Catalog"));
    assert!(
        !shows(&mut shell, "/  Linear"),
        "Esc closes the connector's page"
    );
    let mut gone = view();
    keys(&mut shell, &[Key::End, Key::Home, Key::Enter]);
    gone.connectors.remove(0);
    keys(&mut shell, &[Key::Char('r')]);
    assert!(read_all(&one(&mut shell)));
    arrive(&mut shell, ConnectorsEvent::Page(Ok(gone)));
    assert!(shows(&mut shell, "No connector named linear."));
}

#[test]
fn the_native_setting_and_the_size_cap_wait_for_enter_and_save_with_the_version_read() {
    let mut shell = page(view());
    keys(&mut shell, &[Key::End, Key::Up, Key::Right]);
    assert!(requests(&mut shell).is_empty(), "a pick waits for Enter");
    assert!(shows(&mut shell, "[On]"));
    assert!(shows(&mut shell, "Enter Save changes"));
    keys(&mut shell, &[Key::Enter]);
    assert!(matches!(
        one(&mut shell),
        ConnectorsJob::SaveSettings { settings, version: 7 } if settings == ConnectorSettings {
            native: NativeConnectors::On,
            ..ConnectorSettings::DEFAULT
        }
    ));
    let mut stored = view();
    stored.settings.native = NativeConnectors::On;
    stored.version = 8;
    arrive(
        &mut shell,
        ConnectorsEvent::Saved(Ok(ConnectorsSaved::Saved(Box::new(stored)))),
    );
    assert!(said(&shell).contains("Connector settings saved."));
    assert!(
        requests(&mut shell).is_empty(),
        "the save answered with the page"
    );
    keys(&mut shell, &[Key::Down, Key::Left, Key::Enter]);
    assert!(matches!(
        one(&mut shell),
        ConnectorsJob::SaveSettings { version: 8, .. }
    ));
    arrive(
        &mut shell,
        ConnectorsEvent::Saved(Ok(ConnectorsSaved::Stale(9))),
    );
    assert!(said(&shell).contains("The connector settings changed elsewhere."));
    assert!(
        read_all(&one(&mut shell)),
        "a stale save reads the page again"
    );
    assert!(
        shows(&mut shell, "[256 KiB]"),
        "a stale save drops the pick"
    );
}

#[test]
fn write_actions_and_approval_wait_come_first_and_save_with_the_other_two() {
    let mut shell = page(view());
    keys(&mut shell, &[Key::End, Key::Up, Key::Up, Key::Up]);
    assert!(shows(&mut shell, "[Ask every time]"));
    keys(&mut shell, &[Key::Right, Key::Left, Key::Enter]);
    assert!(
        requests(&mut shell).is_empty(),
        "a pick equal to the read is no change"
    );
    keys(&mut shell, &[Key::Right]);
    assert!(requests(&mut shell).is_empty(), "a pick waits for Enter");
    assert!(shows(&mut shell, "[Ask once per project and connector]"));
    assert!(shows(&mut shell, "Enter Save changes"));
    keys(&mut shell, &[Key::Down, Key::Left]);
    assert!(shows(&mut shell, "[2 min]"));
    keys(&mut shell, &[Key::Enter]);
    assert!(matches!(
        one(&mut shell),
        ConnectorsJob::SaveSettings { settings, version: 7 } if settings == ConnectorSettings {
            write: WritePolicy::AskOncePerProject,
            wait: Wait::Minutes2,
            ..ConnectorSettings::DEFAULT
        }
    ));
    let mut read = view();
    read.settings.write = WritePolicy::ReadOnly;
    let mut shell = page(read);
    keys(
        &mut shell,
        &[Key::End, Key::Up, Key::Up, Key::Up, Key::Right, Key::Enter],
    );
    assert!(
        matches!(
            one(&mut shell),
            ConnectorsJob::SaveSettings { settings, .. }
                if settings.write == WritePolicy::AskEveryTime
        ),
        "the choices wrap from Read-only to Ask every time"
    );
}

#[test]
fn each_size_cap_is_one_left_or_right_away_and_the_choices_wrap() {
    for (seed, key, cap) in [
        (SizeCap::Kib256, Key::Left, SizeCap::Kib64),
        (SizeCap::Kib256, Key::Right, SizeCap::Mib1),
        (SizeCap::Kib64, Key::Right, SizeCap::Kib256),
        (SizeCap::Mib1, Key::Right, SizeCap::Kib64),
    ] {
        let mut read = view();
        read.settings.size_cap = seed;
        let mut shell = page(read);
        keys(&mut shell, &[Key::End, key, Key::Enter]);
        let saved = ConnectorSettings {
            size_cap: cap,
            ..ConnectorSettings::DEFAULT
        };
        assert!(
            matches!(
                one(&mut shell),
                ConnectorsJob::SaveSettings { settings, version: 7 } if settings == saved
            ),
            "{seed:?} to {cap:?}"
        );
    }
}

#[test]
fn esc_drops_an_unsaved_pick_and_a_failed_save_says_why() {
    let mut shell = page(view());
    keys(&mut shell, &[Key::End, Key::Right]);
    assert!(shows(&mut shell, "[1 MiB]"));
    assert!(shows(&mut shell, "Esc Discard changes"));
    keys(&mut shell, &[Key::Esc]);
    assert!(shows(&mut shell, "[256 KiB]"));
    assert!(
        shows(&mut shell, "Settings  /  Connectors "),
        "Esc stayed on the page"
    );
    assert!(requests(&mut shell).is_empty());
    keys(&mut shell, &[Key::Right, Key::Enter]);
    assert!(matches!(
        one(&mut shell),
        ConnectorsJob::SaveSettings { .. }
    ));
    arrive(
        &mut shell,
        ConnectorsEvent::Saved(Err("connectors_unavailable".into())),
    );
    assert!(said(&shell).contains("The connector settings were not saved."));
    assert!(said(&shell).contains("The engine could not be reached."));
}

#[test]
fn a_typed_key_is_masked_and_leaves_only_inside_the_set_key_job() {
    let mut shell = page(view());
    keys(&mut shell, &[Key::Enter, Key::Char('k')]);
    assert!(shows(&mut shell, "Key for api_key"));
    assert!(shows(
        &mut shell,
        "Linear sends this key to mcp.linear.app. Only the engine keeps it."
    ));
    assert!(shows(
        &mut shell,
        "Sent to the engine once and never shown again."
    ));
    keys(&mut shell, &[Key::Enter]);
    assert!(requests(&mut shell).is_empty(), "an empty key is not sent");
    assert!(shows(&mut shell, "Enter a key."));
    typed(&mut shell, TYPED_KEY);
    for size in [(80, 24), (120, 40)] {
        assert!(
            !render(&mut shell, size.0, size.1)
                .iter()
                .any(|row| row.contains("lin-secret") || row.contains("typed")),
            "{size:?}"
        );
    }
    keys(&mut shell, &[Key::Enter]);
    let job = one(&mut shell);
    assert_eq!(format!("{job:?}"), "ConnectorSetKey");
    assert!(matches!(
        job,
        ConnectorsJob::SetKey { ref name, ref secret, .. }
            if name == "linear" && secret == "api_key"
    ));
    arrive(
        &mut shell,
        ConnectorsEvent::Page(Ok(with_secret(Binding::Key, 0))),
    );
    assert!(said(&shell).contains("The key for api_key was saved."));
    assert!(shows(&mut shell, "api_key: Key you entered"));
    assert!(
        !render(&mut shell, 80, 24)
            .iter()
            .any(|row| row.contains("lin-secret"))
    );
}

#[test]
fn u_clears_a_saved_key_and_a_gone_connector_reads_the_page_again() {
    let mut shell = page(view());
    keys(&mut shell, &[Key::Down, Key::Enter]);
    assert!(shows(&mut shell, "token: Key you entered"));
    assert!(shows(&mut shell, "u Clear key"));
    keys(&mut shell, &[Key::Char('u')]);
    assert!(matches!(
        one(&mut shell),
        ConnectorsJob::ClearKey { ref name, ref secret } if name == "github" && secret == "token"
    ));
    arrive(
        &mut shell,
        ConnectorsEvent::Page(Ok(with_secret(Binding::None, 1))),
    );
    assert!(said(&shell).contains("token no longer has a key."));
    assert!(shows(&mut shell, "token: Not set"));
    keys(&mut shell, &[Key::Char('r')]);
    assert!(read_all(&one(&mut shell)));
    arrive(
        &mut shell,
        ConnectorsEvent::Page(Ok(with_secret(Binding::Key, 1))),
    );
    keys(&mut shell, &[Key::Char('u')]);
    assert!(matches!(one(&mut shell), ConnectorsJob::ClearKey { .. }));
    arrive(
        &mut shell,
        ConnectorsEvent::Page(Err("app_plugin_not_found".into())),
    );
    assert!(said(&shell).contains("That connector is gone. The page now shows the current ones."));
    assert!(
        read_all(&one(&mut shell)),
        "a gone connector reads the page again"
    );
}

#[test]
fn adding_shows_the_trust_screen_and_confirm_sends_the_text_and_its_sha() {
    let mut shell = page(view());
    keys(&mut shell, &[Key::Char('a')]);
    assert!(shows(&mut shell, "Add a connector"));
    assert!(shows(&mut shell, "Connector manifest (YAML)"));
    assert!(shows(
        &mut shell,
        "Nothing is used until you confirm its trust screen."
    ));
    typed(&mut shell, "name: board");
    keys(&mut shell, &[Key::Enter]);
    assert!(matches!(
        one(&mut shell),
        ConnectorsJob::Preview { ref yaml } if yaml == "name: board"
    ));
    arrive(
        &mut shell,
        ConnectorsEvent::Preview {
            read: None,
            result: Ok(Preview::Ready {
                trust: trust(false),
            }),
        },
    );
    for words in [
        "Trust Board?",
        "Version 0.2.0",
        "Address: https://board.example.com/mcp",
        "Reaches board.example.com, files.example.com",
        "Read-only header: X-Read-Only: true",
        "Key: the Authorization header with Bearer, from api_key",
        "Tools it says only read: list_cards",
        "SHA-256 c0ffee",
    ] {
        assert!(shows(&mut shell, words), "{words}");
    }
    keys(&mut shell, &[Key::Char('y')]);
    assert!(matches!(
        one(&mut shell),
        ConnectorsJob::Add { ref yaml, ref sha256, replace: false }
            if yaml == "name: board" && sha256 == SHA
    ));
    arrive(&mut shell, ConnectorsEvent::Page(Ok(view())));
    assert!(said(&shell).contains("Board was added."));
    assert!(shows(&mut shell, "Catalog"));
}

#[test]
fn a_manifest_that_is_not_a_connector_is_refused_and_e_edits_it_again() {
    let mut shell = page(view());
    keys(&mut shell, &[Key::Char('a')]);
    typed(&mut shell, "kind: embedding");
    keys(&mut shell, &[Key::Enter]);
    assert!(matches!(one(&mut shell), ConnectorsJob::Preview { .. }));
    let mut embedding = trust(false);
    embedding.kind = PluginKind::Embedding;
    embedding.connector = None;
    arrive(
        &mut shell,
        ConnectorsEvent::Preview {
            read: None,
            result: Ok(Preview::Ready { trust: embedding }),
        },
    );
    assert!(shows(&mut shell, "This manifest can't be added"));
    assert!(shows(&mut shell, "app_plugin_not_connector, kind"));
    assert!(shows(&mut shell, "e Edit manifest"));
    keys(&mut shell, &[Key::Char('y')]);
    assert!(
        requests(&mut shell).is_empty(),
        "a refused manifest has no confirm"
    );
    keys(&mut shell, &[Key::Char('e')]);
    assert!(shows(&mut shell, "Add a connector"));
    assert!(shows(&mut shell, "kind: embedding"));
    keys(&mut shell, &[Key::Enter]);
    assert!(matches!(one(&mut shell), ConnectorsJob::Preview { .. }));
    let problem = Problem {
        code: "app_plugin_manifest_invalid".into(),
        line: Some(2),
        field: Some("mcp.url".into()),
        message: "The address must start with https://.".into(),
    };
    arrive(
        &mut shell,
        ConnectorsEvent::Preview {
            read: None,
            result: Ok(Preview::Refused { problem }),
        },
    );
    assert!(shows(
        &mut shell,
        "(app_plugin_manifest_invalid, line 2, mcp.url)"
    ));
}

#[test]
fn a_connector_that_needs_review_is_trusted_from_its_page_and_the_owner_s_own_is_removed() {
    let mut shell = page(view());
    keys(&mut shell, &[Key::Enter, Key::Char('x')]);
    assert!(said(&shell).contains("Built-in connectors can't be removed or replaced."));
    assert!(requests(&mut shell).is_empty());
    keys(&mut shell, &[Key::Esc, Key::Down, Key::Down, Key::Enter]);
    assert!(shows(&mut shell, "Enter Review"));
    assert!(shows(&mut shell, "x Remove"));
    keys(&mut shell, &[Key::Enter]);
    assert!(matches!(
        one(&mut shell),
        ConnectorsJob::Review { ref name } if name == "board"
    ));
    arrive(
        &mut shell,
        ConnectorsEvent::Preview {
            read: None,
            result: Ok(Preview::Ready {
                trust: trust(false),
            }),
        },
    );
    assert!(shows(&mut shell, "Review Board"));
    keys(&mut shell, &[Key::Char('y')]);
    assert!(matches!(
        one(&mut shell),
        ConnectorsJob::Trust { ref name, ref sha256 } if name == "board" && sha256 == SHA
    ));
    let mut trusted = view();
    trusted.connectors[2].status = PluginStatus::NeedsKey;
    arrive(&mut shell, ConnectorsEvent::Page(Ok(trusted.clone())));
    assert!(said(&shell).contains("Board is trusted."));
    keys(&mut shell, &[Key::Char('x')]);
    assert!(shows(&mut shell, "Remove Board?"));
    assert!(shows(&mut shell, "keys and its switch in every project."));
    keys(&mut shell, &[Key::Char('y')]);
    assert!(matches!(
        one(&mut shell),
        ConnectorsJob::Remove { ref name } if name == "board"
    ));
    trusted.connectors.truncate(2);
    arrive(&mut shell, ConnectorsEvent::Page(Ok(trusted)));
    assert!(said(&shell).contains("Board was removed."));
    assert!(
        shows(&mut shell, "Catalog"),
        "a removed connector's page closes"
    );
}

#[test]
fn a_failed_first_read_offers_a_retry() {
    let mut shell = shell_at(ready(80, 24), Place::Connectors);
    assert!(read_all(&one(&mut shell)));
    arrive(
        &mut shell,
        ConnectorsEvent::Page(Err("app_plugins_unavailable".into())),
    );
    assert!(shows(&mut shell, "Couldn't load the connectors."));
    assert!(shows(
        &mut shell,
        "Connectors aren't available in this engine yet."
    ));
    assert!(shows(&mut shell, "r Try again"));
    keys(&mut shell, &[Key::Char('r')]);
    assert!(read_all(&one(&mut shell)));
}

#[test]
fn refusals_read_as_the_desktop_s_words() {
    for (code, words) in [
        (
            "app_plugin_bundled",
            "Built-in connectors can't be removed or replaced.",
        ),
        (
            "app_plugin_changed",
            "The manifest changed after you checked it. Check it again before you trust it.",
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
            "app_plugin_secret_not_declared",
            "This connector doesn't declare that secret.",
        ),
        (
            "connector_project_unknown",
            "That project is not set up here any more.",
        ),
        ("connectors_unavailable", "The engine could not be reached."),
        ("connector_request_bounds", "The request is too large."),
    ] {
        assert_eq!(refusal(code), words, "{code}");
    }
}

#[test]
fn a_trust_screen_lists_every_field_in_the_desktop_s_order() {
    assert_eq!(
        trust_lines(&trust(true)),
        [
            "Version 0.2.0",
            "Address: https://board.example.com/mcp",
            "Reaches board.example.com, files.example.com",
            "Read-only header: X-Read-Only: true",
            "Key: the Authorization header with Bearer, from api_key",
            "Signs in on auth.example.com",
            "Registers by cimd, then dcr",
            "Scopes while reading: read",
            "Scopes when writing: none",
            "Tools it says only read: list_cards",
            "Secrets it needs: api_key",
            "SHA-256 c0ffee",
        ]
    );
    let mut plain = trust(false);
    plain.replaces = true;
    plain.secrets.clear();
    if let Some(block) = plain.connector.as_mut() {
        block.read_only = ReadOnlyTrust::Url {
            url: "https://board.example.com/mcp/readonly".into(),
        };
        block.read_tools.clear();
        if let Some(key) = block.key.as_mut() {
            key.scheme.clear();
        }
    }
    let lines = trust_lines(&plain);
    assert_eq!(
        lines[3],
        "Read-only address: https://board.example.com/mcp/readonly"
    );
    assert_eq!(lines[4], "Key: the Authorization header, from api_key");
    assert_eq!(lines[5], "Tools it says only read: none");
    assert_eq!(lines[6], "Secrets it needs: none");
    assert_eq!(
        lines.last().map(String::as_str),
        Some("Replaces your connector named board.")
    );
    if let Some(block) = plain.connector.as_mut() {
        block.read_only = ReadOnlyTrust::None;
        block.key = None;
    }
    let lines = trust_lines(&plain);
    assert_eq!(lines[3], "Read-only mode at the vendor: none");
    assert_eq!(lines[4], "Key: none");
}

#[test]
fn go_to_lists_connectors_under_settings_and_opens_it() {
    let mut context = ready(80, 24);
    let listed = targets(&context);
    let at = listed.iter().position(|target| {
        target.label == "Settings / Connectors"
            && target.kind == "Settings"
            && target.place == Place::Connectors
    });
    let activity = listed
        .iter()
        .position(|target| target.place == Place::ConnectorActivity);
    assert!(at.is_some() && at < activity, "{at:?} {activity:?}");
    let screen = open(&Place::Connectors, &mut context);
    assert_eq!(screen.place(), Some(Place::Connectors));
    assert_eq!(screen.crumbs(), ["Settings", "Connectors"]);
}

#[test]
fn a_refused_file_is_listed_by_its_name_with_why_and_where_to_fix_it_and_never_opens() {
    let mut read = view();
    read.connectors.truncate(1);
    read.connectors.push(refused_file("broken", not_https()));
    read.connectors.push(refused_file("linear", taken()));
    let mut shell = page(read);
    let tall = render(&mut shell, 80, 48).join("\n");
    assert!(tall.contains("broken. Added by you."), "{tall}");
    assert!(tall.contains("linear. Added by you."), "{tall}");
    assert!(!tall.contains("broken, version"), "{tall}");
    assert!(
        tall.contains("The address must start with https://."),
        "{tall}"
    );
    assert!(
        tall.contains("a plugin with this name already exists;"),
        "{tall}"
    );
    let fix = "Fix its file, or remove it on the App plugins page.";
    assert_eq!(tall.matches(fix).count(), 2, "{tall}");
    assert_eq!(
        tall.matches("Reaches ").count(),
        1,
        "only Linear reaches a host"
    );
    assert!(shows(&mut shell, "Enter Open"), "Linear opens");
    keys(&mut shell, &[Key::Down]);
    assert!(
        !shows(&mut shell, "Enter Open"),
        "a refused file has no page to open"
    );
    keys(&mut shell, &[Key::Enter, Key::Down, Key::Enter]);
    assert!(!shows(&mut shell, "/  broken"));
    assert!(
        !shows(&mut shell, "/  Linear") && !shows(&mut shell, "/  linear"),
        "a refused file shadowed by Linear never opens Linear's page"
    );
    assert!(shows(&mut shell, "Catalog"));
    assert!(requests(&mut shell).is_empty());
}

#[test]
fn a_page_whose_file_turns_refused_shows_why_and_offers_no_review_no_remove_and_no_key() {
    let mut shell = page(view());
    keys(&mut shell, &[Key::Down, Key::Down, Key::Enter]);
    assert!(shows(&mut shell, "x Remove"));
    let mut broken = view();
    broken.connectors[2] = refused_file("board", not_https());
    keys(&mut shell, &[Key::Char('r')]);
    assert!(read_all(&one(&mut shell)));
    arrive(&mut shell, ConnectorsEvent::Page(Ok(broken)));
    assert!(shows(&mut shell, "Settings  /  Connectors  /  board"));
    assert!(shows(&mut shell, "board. Added by you."));
    assert!(shows(&mut shell, "The address must start with https://."));
    assert!(shows(
        &mut shell,
        "Fix its file, or remove it on the App plugins page."
    ));
    assert!(!shows(&mut shell, "Reaches ") && !shows(&mut shell, "Address:"));
    assert!(
        !shows(&mut shell, "Signs in with OAuth"),
        "a refused file is not an OAuth one"
    );
    assert!(!shows(&mut shell, "Enter Review") && !shows(&mut shell, "x Remove"));
    assert!(!shows(&mut shell, "k Set key"));
    keys(&mut shell, &[Key::Char('x'), Key::Enter, Key::Char('k')]);
    assert!(
        !shows(&mut shell, "Remove board?"),
        "x removes nothing here"
    );
    assert!(!said(&shell).contains("OAuth"), "{}", said(&shell));
    assert!(requests(&mut shell).is_empty());
}

#[test]
fn hidden_characters_in_a_title_or_description_show_as_escapes_on_the_catalog_row() {
    let mut read = view();
    read.connectors[0].title = "Lin\u{200b}ear".into();
    read.connectors[0].description = "Issues\u{202e} in Linear.".into();
    let mut shell = page(read);
    assert!(shows(&mut shell, "Lin\\u200bear"));
    assert!(shows(&mut shell, "Issues\\u202e in Linear."));
    keys(&mut shell, &[Key::Enter]);
    assert!(shows(
        &mut shell,
        "Settings  /  Connectors  /  Lin\\u200bear"
    ));
    assert!(shows(&mut shell, "Issues\\u202e in Linear."));
}

#[test]
fn an_oauth_only_connector_reads_unavailable_with_the_oauth_line_once_and_review_comes_first() {
    let oauth = "Signs in with OAuth, which this build does not have.";
    let mut read = view();
    for at in [1, 2] {
        read.connectors[at].sign_in = SignIn::NotYet;
        read.connectors[at].secrets.clear();
    }
    let mut shell = page(read);
    let tall = render(&mut shell, 80, 48);
    let reads = |title: &str, word: &str| {
        tall.iter()
            .any(|row| row.contains(title) && row.trim_end().ends_with(word))
    };
    assert!(reads("GitHub", "Unavailable"), "{tall:#?}");
    assert!(
        reads("Board", "Needs review"),
        "review comes before Unavailable"
    );
    assert_eq!(tall.join("\n").matches(oauth).count(), 1, "{tall:#?}");
    keys(&mut shell, &[Key::Down, Key::Enter]);
    assert!(shows(&mut shell, "Settings  /  Connectors  /  GitHub"));
    assert_eq!(
        render(&mut shell, 80, 24).join("\n").matches(oauth).count(),
        1
    );
    assert!(!shows(&mut shell, "k Set key"));
    keys(&mut shell, &[Key::Esc, Key::Down, Key::Enter]);
    assert!(shows(&mut shell, "Settings  /  Connectors  /  Board"));
    assert!(shows(
        &mut shell,
        "Review this connector before a project can use it."
    ));
    assert!(
        !shows(&mut shell, oauth),
        "a connector under review shows the review line only"
    );
}

fn checked(shell: &mut Shell, trust: TrustView) {
    keys(shell, &[Key::Char('a')]);
    typed(shell, "name: board");
    keys(shell, &[Key::Enter]);
    assert!(matches!(one(shell), ConnectorsJob::Preview { .. }));
    arrive(
        shell,
        ConnectorsEvent::Preview {
            read: None,
            result: Ok(Preview::Ready { trust }),
        },
    );
}

/// rv-ds241 T5 F1: a trust screen taller than the terminal scrolls inside its box above the
/// help bar, says how much is below, and y confirms only once its end was on screen.
#[test]
fn a_long_trust_screen_scrolls_to_its_end_before_y_confirms() {
    let mut long = trust(false);
    long.replaces = true;
    if let Some(block) = long.connector.as_mut() {
        block.read_tools = (1..=60).map(|at| format!("read_tool_{at:02}")).collect();
    }
    let mut shell = page(view());
    checked(&mut shell, long);
    assert!(shows(&mut shell, "Trust Board?"));
    assert!(shows(&mut shell, "more lines below"));
    assert!(!shows(&mut shell, "read_tool_60"));
    assert!(!shows(&mut shell, "Replaces your connector named board."));
    assert!(
        shows(&mut shell, "Esc Cancel"),
        "the box leaves the help bar"
    );
    assert!(
        !shows(&mut shell, "y Confirm"),
        "no confirm before the end was shown"
    );
    keys(&mut shell, &[Key::Char('y')]);
    assert!(requests(&mut shell).is_empty(), "y waits for the end");
    keys(&mut shell, &[Key::Down]);
    assert!(
        !shows(&mut shell, "Version 0.2.0"),
        "Down moves the body one line"
    );
    keys(&mut shell, &[Key::Up]);
    assert!(shows(&mut shell, "Version 0.2.0"));
    for _ in 0..40 {
        if shows(&mut shell, "Replaces your connector named board.") {
            break;
        }
        keys(&mut shell, &[Key::PageDown]);
    }
    for words in [
        "read_tool_60",
        "Secrets it needs: api_key",
        "SHA-256 c0ffee",
        "Replaces your connector named board.",
        "y Confirm",
    ] {
        assert!(shows(&mut shell, words), "{words}");
    }
    assert!(!shows(&mut shell, "more lines below"));
    keys(&mut shell, &[Key::Home]);
    assert!(shows(&mut shell, "Version 0.2.0"));
    assert!(shows(&mut shell, "y Confirm"), "the end was shown once");
    keys(&mut shell, &[Key::Char('y')]);
    assert!(matches!(
        one(&mut shell),
        ConnectorsJob::Add { ref sha256, replace: true, .. } if sha256 == SHA
    ));
}

/// rv-ds241 T5 F2: Esc does nothing while a request is on its way, as the desktop's busy
/// dialog, so a late answer never opens a screen the owner closed.
#[test]
fn esc_while_a_request_is_on_its_way_keeps_the_form_and_the_dialog() {
    let mut shell = page(view());
    keys(&mut shell, &[Key::Char('a')]);
    typed(&mut shell, "name: board");
    keys(&mut shell, &[Key::Enter]);
    assert!(matches!(one(&mut shell), ConnectorsJob::Preview { .. }));
    assert!(
        !shows(&mut shell, "Esc Cancel"),
        "no Esc is offered while it is checked"
    );
    keys(&mut shell, &[Key::Esc]);
    assert!(
        shows(&mut shell, "Add a connector"),
        "the form stays while it is checked"
    );
    arrive(
        &mut shell,
        ConnectorsEvent::Preview {
            read: None,
            result: Ok(Preview::Ready {
                trust: trust(false),
            }),
        },
    );
    assert!(shows(&mut shell, "Trust Board?"));
    keys(&mut shell, &[Key::Char('y')]);
    assert!(matches!(one(&mut shell), ConnectorsJob::Add { .. }));
    keys(&mut shell, &[Key::Esc]);
    assert!(
        shows(&mut shell, "Trust Board?"),
        "the trust screen stays while it saves"
    );
    arrive(&mut shell, ConnectorsEvent::Page(Ok(view())));
    assert!(said(&shell).contains("Board was added."));

    keys(
        &mut shell,
        &[Key::Down, Key::Down, Key::Enter, Key::Char('x')],
    );
    assert!(shows(&mut shell, "Remove Board?"));
    keys(&mut shell, &[Key::Char('y')]);
    assert!(matches!(
        one(&mut shell),
        ConnectorsJob::Remove { ref name } if name == "board"
    ));
    keys(&mut shell, &[Key::Esc]);
    assert!(
        shows(&mut shell, "Remove Board?"),
        "the dialog stays while it removes"
    );
    let mut gone = view();
    gone.connectors.truncate(2);
    arrive(&mut shell, ConnectorsEvent::Page(Ok(gone)));
    assert!(said(&shell).contains("Board was removed."));
    assert!(
        shows(&mut shell, "Catalog"),
        "the removed connector's page closes"
    );
    assert!(!shows(&mut shell, "No connector named"));
}

/// rv-ds241 T5 F4: a picked .yaml path the TUI cannot read says so in the words the App
/// plugins page uses, never that the engine could not do it.
#[test]
fn a_path_that_cannot_be_read_says_the_file_could_not_be_read() {
    let unreadable = "CrazyAgent couldn't read that file. Pick a manifest file you can open.";
    let not_text = "That file isn't text. A manifest is a UTF-8 .yaml file.";
    assert_eq!(refusal("app_plugin_file_unreadable"), unreadable);
    assert_eq!(refusal("app_plugin_file_not_text"), not_text);
    let mut shell = page(view());
    keys(&mut shell, &[Key::Char('a'), Key::Tab]);
    typed(&mut shell, "/no/such/board.yaml");
    keys(&mut shell, &[Key::Enter]);
    assert!(matches!(
        one(&mut shell),
        ConnectorsJob::ReadFile { ref path } if path.ends_with("board.yaml")
    ));
    arrive(
        &mut shell,
        ConnectorsEvent::Preview {
            read: None,
            result: Err("app_plugin_file_unreadable".into()),
        },
    );
    assert!(shows(&mut shell, "Add a connector"));
    assert!(shows(&mut shell, "CrazyAgent couldn't read that file."));
    assert!(!shows(&mut shell, "The engine could not do this."));
}

#[test]
fn the_add_forms_manifest_box_draws_hidden_characters_as_escapes() {
    let form = add_form();
    assert!(form.yaml.multiline && form.yaml.escaped);
    assert!(!form.path.escaped);
}

/// rv-code FINDINGS-01:26 F1, leader 01:31: a connector that signs in through the browser
/// reads Needs sign-in until it is signed in, with the desktop's words, and a file this
/// build cannot sign in still reads Unavailable.
#[test]
fn a_connector_waiting_for_its_browser_sign_in_reads_needs_sign_in() {
    let mut github = view().connectors[1].clone();
    github.status = PluginStatus::NeedsSignIn;
    let (word, role, line) = state(&github);
    assert_eq!((word, role), ("Needs sign-in", Role::Warning));
    let said = "Sign in on its page before a project can use it.";
    assert_eq!(line.as_deref(), Some(said));
    github.sign_in = SignIn::NotYet;
    assert_eq!(state(&github).0, "Unavailable");
}

/// QUESTION-23:34 Q1: a connector whose file offers a key and a browser sign-in shows the
/// way it signs in, and `w` switches it; its keys are shown and set only while it signs in
/// with a key (rv-code note 0: `k Set key` too), and a connector with one way has nothing
/// to switch.
#[test]
fn w_switches_a_connector_that_offers_both_between_a_key_and_the_browser() {
    let mut shell = page(both(SignInWith::Key));
    keys(&mut shell, &[Key::Down, Key::Enter]);
    assert!(shows(&mut shell, "Sign in with: Key"));
    assert!(shows(&mut shell, "w Sign in with Browser"));
    assert!(shows(&mut shell, "token: Key you entered"));
    assert!(
        shows(&mut shell, "k Set key"),
        "rv-code note 0: a key is set under Key"
    );
    keys(&mut shell, &[Key::Char('w')]);
    assert!(matches!(
        one(&mut shell),
        ConnectorsJob::SetSignInWith { ref name, way: SignInWith::Browser } if name == "github"
    ));
    let read = both(SignInWith::Browser);
    arrive(&mut shell, ConnectorsEvent::Page(Ok(read)));
    assert!(said(&shell).contains("GitHub signs in with Browser."));
    assert!(shows(&mut shell, "Sign in with: Browser"));
    assert!(shows(&mut shell, "w Sign in with Key"));
    assert!(
        shows(&mut shell, "token: Key you entered"),
        "rv-sec2 F1: a saved key stays"
    );
    assert!(shows(&mut shell, "u Clear key") && !shows(&mut shell, "k Set key"));
    keys(&mut shell, &[Key::Char('k')]);
    assert!(
        requests(&mut shell).is_empty(),
        "no key is set under Browser"
    );
    keys(&mut shell, &[Key::Char('w')]);
    assert!(matches!(
        one(&mut shell),
        ConnectorsJob::SetSignInWith {
            way: SignInWith::Key,
            ..
        }
    ));
    let read = both(SignInWith::Key);
    arrive(&mut shell, ConnectorsEvent::Page(Ok(read)));
    assert!(shows(&mut shell, "token: Key you entered") && shows(&mut shell, "k Set key"));
    keys(&mut shell, &[Key::Esc, Key::Up, Key::Enter]);
    assert!(shows(&mut shell, "api_key: Not set"));
    assert!(
        !shows(&mut shell, "Sign in with"),
        "a connector with one way has nothing to switch"
    );
    keys(&mut shell, &[Key::Char('w')]);
    assert!(requests(&mut shell).is_empty());
}

/// rv-code2 TP1-1: a connector on its second secret under Key that reads Browser next, by
/// `w` here or by a switch on the desktop read again, draws with no secret selected and no
/// panic, and Key again starts at the first secret.
#[test]
fn a_switch_to_browser_while_on_a_later_secret_moves_the_cursor_back() {
    let mut shell = page(both_two_secrets(SignInWith::Key));
    keys(&mut shell, &[Key::Down, Key::Enter, Key::Down]);
    assert!(shows(&mut shell, "site: Not set"));
    keys(&mut shell, &[Key::Char('w')]);
    assert!(matches!(
        one(&mut shell),
        ConnectorsJob::SetSignInWith {
            way: SignInWith::Browser,
            ..
        }
    ));
    let read = both_two_secrets(SignInWith::Browser);
    arrive(&mut shell, ConnectorsEvent::Page(Ok(read)));
    assert!(
        shows(&mut shell, "Sign in with: Browser"),
        "the TUI's own switch draws"
    );
    let read = both_two_secrets(SignInWith::Key);
    keys(&mut shell, &[Key::Char('w')]);
    one(&mut shell);
    arrive(&mut shell, ConnectorsEvent::Page(Ok(read)));
    keys(&mut shell, &[Key::Char('k')]);
    assert!(
        shows(&mut shell, "Key for token"),
        "Key again starts at the first secret"
    );

    let mut shell = page(both_two_secrets(SignInWith::Key));
    keys(
        &mut shell,
        &[Key::Down, Key::Enter, Key::Down, Key::Char('r')],
    );
    assert!(read_all(&one(&mut shell)));
    let read = both_two_secrets(SignInWith::Browser);
    arrive(&mut shell, ConnectorsEvent::Page(Ok(read)));
    assert!(
        shows(&mut shell, "Sign in with: Browser"),
        "a desktop switch read again draws"
    );
}

/// rv-sec2 T10 pages Q1 F1, leader 02:04: under Browser a saved key is listed with Clear
/// key, so it can be removed without making it live first; a secret with no key is not
/// listed, and no key is set.
#[test]
fn under_browser_a_saved_key_can_be_cleared_without_switching_to_key() {
    let mut shell = page(both_two_secrets(SignInWith::Browser));
    keys(&mut shell, &[Key::Down, Key::Enter]);
    assert!(shows(&mut shell, "token: Key you entered") && shows(&mut shell, "u Clear key"));
    assert!(
        !shows(&mut shell, "site: Not set"),
        "a secret with no key is not listed"
    );
    assert!(!shows(&mut shell, "k Set key"));
    keys(&mut shell, &[Key::Char('u')]);
    assert!(matches!(
        one(&mut shell),
        ConnectorsJob::ClearKey { ref name, ref secret } if name == "github" && secret == "token"
    ));
}

/// Task 10 pages slice A: `s` on a connector that signs in through the browser starts its
/// sign-in and shows the address for copying while it waits; each refresh reads where it
/// stands, one read at a time, and once it is signed in the page reads itself again.
#[test]
fn s_signs_in_through_the_browser_and_each_refresh_reads_where_it_stands() {
    let mut shell = notion(false);
    assert!(shows(&mut shell, "s Sign in"));
    let mut shell = waiting();
    assert!(shows(&mut shell, "Waiting for the browser"));
    assert!(shows(&mut shell, LINK), "the address is shown for copying");
    assert!(shows(&mut shell, "c Cancel") && !shows(&mut shell, "s Sign in"));
    assert!(requests(&mut shell).is_empty());
    shell.refresh();
    assert!(reads_state(&one(&mut shell)));
    shell.refresh();
    assert!(requests(&mut shell).is_empty(), "one read at a time");
    arrive(&mut shell, stands(Ok(SignInState::Waiting)));
    assert!(shows(&mut shell, "Waiting for the browser"));
    shell.refresh();
    assert!(reads_state(&one(&mut shell)));
    arrive(&mut shell, stands(Ok(SignInState::SignedIn)));
    assert!(said(&shell).contains("Notion is signed in."));
    assert!(read_all(&one(&mut shell)), "the page reads itself again");
    arrive(&mut shell, ConnectorsEvent::Page(Ok(browser(true))));
    assert!(shows(&mut shell, "the vendor is not told"));
    assert!(shows(&mut shell, "s Sign out"));
    assert!(!shows(&mut shell, "Waiting for the browser"));
    shell.refresh();
    assert!(
        requests(&mut shell).is_empty(),
        "an ended sign-in is not read"
    );
}

/// Task 10 pages slice A: `c` cancels a waiting sign-in once, and every way a sign-in ends
/// or is refused says why in the engine's words, the desktop's too, the finishing sentence
/// among them; an answer for another connector changes nothing.
#[test]
fn c_cancels_a_waiting_sign_in_and_each_end_says_why() {
    let mut shell = waiting();
    keys(&mut shell, &[Key::Char('c')]);
    assert!(matches!(
        one(&mut shell),
        ConnectorsJob::CancelSignIn { ref name } if name == "notion"
    ));
    assert!(!shows(&mut shell, "c Cancel"));
    keys(&mut shell, &[Key::Char('c')]);
    assert!(requests(&mut shell).is_empty(), "one cancel");
    let other = ConnectorsEvent::SignInState {
        name: "github".into(),
        result: Ok(SignInState::Cancelled),
    };
    arrive(&mut shell, other);
    assert!(shows(&mut shell, "Waiting for the browser"));
    arrive(&mut shell, stands(Ok(SignInState::Cancelled)));
    assert!(said(&shell).contains("The sign-in was cancelled."));
    assert!(read_all(&one(&mut shell)));
    assert!(!shows(&mut shell, "Waiting for the browser"));

    let expired = "connector_oauth_callback_expired";
    let words = "The browser did not come back within 5 minutes; sign in again.";
    assert_eq!(refusal(expired), words);
    let mut shell = waiting();
    let code = expired.to_owned();
    arrive(&mut shell, stands(Ok(SignInState::Failed { code })));
    assert!(said(&shell).contains(words));

    let mut shell = waiting();
    let gone = "trusted_connection_unavailable".to_owned();
    arrive(&mut shell, stands(Err(gone)));
    assert!(said(&shell).contains("Couldn't read the sign-in."));
    assert!(!shows(&mut shell, "Waiting for the browser"));

    let mut shell = notion(false);
    keys(&mut shell, &[Key::Char('s')]);
    one(&mut shell);
    arrive(&mut shell, started(false));
    assert!(said(&shell).contains("The browser did not open."));
    assert!(shows(&mut shell, LINK));

    let finishing = "connector_oauth_sign_in_finishing";
    let refused = [finishing, "connector_oauth_needs_own_app"];
    for code in refused {
        let mut shell = notion(false);
        keys(&mut shell, &[Key::Char('s')]);
        one(&mut shell);
        let answer = ConnectorsEvent::SignIn {
            name: "notion".into(),
            result: Err(code.into()),
        };
        arrive(&mut shell, answer);
        assert!(said(&shell).contains(&refusal(code)), "{code}");
        assert!(shows(&mut shell, "s Sign in"), "{code}");
    }
    let words = "This connector's last sign-in is still finishing; try again in a moment.";
    assert_eq!(refusal(finishing), words);
}

/// Task 10 pages slice A: `s` signs a signed-in connector out, whose page says the vendor
/// is not told, and a sign-out while the last sign-in still finishes says to wait; one
/// that offers both and signs in with a key now is signed out without switching (rv-sec2
/// F1: removing a sign-in never makes it live first).
#[test]
fn s_signs_a_signed_in_connector_out_and_a_finishing_sign_in_says_to_wait() {
    let mut shell = notion(true);
    assert!(shows(&mut shell, "the vendor is not told"));
    keys(&mut shell, &[Key::Char('s')]);
    assert!(matches!(
        one(&mut shell),
        ConnectorsJob::SignOut { ref name } if name == "notion"
    ));
    arrive(&mut shell, ConnectorsEvent::Page(Ok(browser(false))));
    assert!(said(&shell).contains("Notion is signed out."));
    assert!(shows(&mut shell, "s Sign in") && !shows(&mut shell, "s Sign out"));

    let finishing = "connector_oauth_sign_in_finishing";
    let mut shell = notion(true);
    keys(&mut shell, &[Key::Char('s')]);
    one(&mut shell);
    arrive(&mut shell, ConnectorsEvent::Page(Err(finishing.into())));
    assert!(said(&shell).contains(&refusal(finishing)));
    assert!(shows(&mut shell, "s Sign out"));

    let mut read = both(SignInWith::Key);
    let oauth = read.connectors[1].oauth.as_mut();
    oauth.expect("GitHub offers both").signed_in = true;
    let mut shell = page(read);
    keys(&mut shell, &[Key::Down, Key::Enter]);
    assert!(shows(&mut shell, "s Sign out"));
    keys(&mut shell, &[Key::Char('s')]);
    assert!(matches!(
        one(&mut shell),
        ConnectorsJob::SignOut { ref name } if name == "github"
    ));
}

/// Task 10 pages slice A: a connector that signs in with a key, or offers both and signs in
/// with a key now and is not signed in, has no browser sign-in, and `s` and `c` send
/// nothing; under Browser it signs in here.
#[test]
fn a_connector_signing_in_with_a_key_has_no_browser_sign_in() {
    let mut shell = page(view());
    keys(&mut shell, &[Key::Enter]);
    assert!(!shows(&mut shell, "s Sign in"));
    keys(&mut shell, &[Key::Char('s'), Key::Char('c')]);
    assert!(requests(&mut shell).is_empty());
    let mut shell = page(both(SignInWith::Key));
    keys(&mut shell, &[Key::Down, Key::Enter]);
    assert!(!shows(&mut shell, "s Sign in") && !shows(&mut shell, "s Sign out"));
    keys(&mut shell, &[Key::Char('s')]);
    assert!(requests(&mut shell).is_empty());
    let mut shell = page(both(SignInWith::Browser));
    keys(&mut shell, &[Key::Down, Key::Enter]);
    assert!(
        shows(&mut shell, "s Sign in"),
        "Both under Browser signs in"
    );
}

/// Task 10 pages slice A: a sign-in the engine keeps calling waiting is read at most
/// `SIGN_IN_READS_MAX` times, which outlasts the engine's five-minute wait, and then the
/// page reads itself again instead.
#[test]
fn a_sign_in_is_read_a_bounded_number_of_times() {
    let mut shell = waiting();
    for _ in 0..SIGN_IN_READS_MAX {
        shell.refresh();
        assert!(reads_state(&one(&mut shell)));
        arrive(&mut shell, stands(Ok(SignInState::Waiting)));
    }
    shell.refresh();
    assert!(read_all(&one(&mut shell)), "the page reads itself again");
    assert!(!shows(&mut shell, "Waiting for the browser"));
}

/// rv-code CONN-T10-PAGES-A F1, leader 02:44: while one connector's sign-in waits, every
/// other connector's Sign in is withheld with "Finish or cancel the <name> sign-in first.",
/// which `s` says too, so the page never drops the waiting one; once it ends, Sign in is back.
#[test]
fn while_one_sign_in_waits_another_connector_says_to_finish_it_first() {
    let mut read = browser(false);
    read.connectors[1].sign_in = SignIn::Both;
    read.connectors[1].status = PluginStatus::NeedsSignIn;
    read.connectors[1].oauth = browser(false).connectors[3].oauth.clone();
    let mut shell = page(read.clone());
    keys(&mut shell, &[Key::Down, Key::Down, Key::Down, Key::Enter]);
    keys(&mut shell, &[Key::Char('s')]);
    assert!(matches!(one(&mut shell), ConnectorsJob::SignIn { .. }));
    arrive(&mut shell, started(true));
    keys(&mut shell, &[Key::Esc, Key::Up, Key::Up, Key::Enter]);
    let first = "Finish or cancel the Notion sign-in first.";
    assert!(shows(&mut shell, first), "GitHub's page says why");
    assert!(!shows(&mut shell, "s Sign in"));
    keys(&mut shell, &[Key::Char('s')]);
    assert!(requests(&mut shell).is_empty(), "the waiting one is kept");
    assert!(said(&shell).contains(first), "s says why too");
    arrive(&mut shell, stands(Ok(SignInState::Cancelled)));
    assert!(read_all(&one(&mut shell)));
    arrive(&mut shell, ConnectorsEvent::Page(Ok(read)));
    assert!(shows(&mut shell, "s Sign in") && !shows(&mut shell, first));
}

fn tall(shell: &mut Shell, text: &str) -> bool {
    render(shell, 100, 48).iter().any(|row| row.contains(text))
}

/// The page with GitHub offering both ways under the way given, with the owner's own app
/// saved, and whether its file still offers one.
fn own_app_saved(way: SignInWith, allowed: bool) -> ConnectorsView {
    let mut read = both(way);
    let oauth = read.connectors[1]
        .oauth
        .as_mut()
        .expect("GitHub offers both");
    oauth.own_app_allowed = allowed;
    oauth.own_app = Some(OwnAppView {
        client_id: "Iv1.app".into(),
        secret_saved: true,
    });
    read
}

/// Task 10 pages slice B (addendum 30 Q4 A): a connector whose file offers an own app shows
/// the section with the redirect and any port; `o` opens the form, a client id is needed and
/// trimmed, the typed secret is masked and leaves only inside the job, an empty secret is a
/// public app's none, and the page confirms with words that never hold the secret.
#[test]
fn o_saves_an_own_app_and_its_secret_leaves_only_inside_the_job() {
    let mut shell = page(both(SignInWith::Browser));
    keys(&mut shell, &[Key::Down, Key::Enter]);
    assert!(tall(&mut shell, "Your own OAuth app"));
    assert!(tall(
        &mut shell,
        "Redirect: http://127.0.0.1/callback (any port)"
    ));
    assert!(tall(&mut shell, "No app saved."));
    assert!(
        tall(&mut shell, "signs the connector out"),
        "the help says what saving does"
    );
    assert!(tall(&mut shell, "o Use my own app") && !tall(&mut shell, "d Remove my app"));
    keys(&mut shell, &[Key::Char('o')]);
    assert!(tall(&mut shell, "Your own OAuth app for GitHub"));
    assert!(tall(
        &mut shell,
        "Redirect: http://127.0.0.1/callback (any port)"
    ));
    assert!(tall(&mut shell, "never shown again"));
    keys(&mut shell, &[Key::Enter]);
    assert!(
        requests(&mut shell).is_empty(),
        "no client id, nothing sent"
    );
    assert!(tall(&mut shell, "Enter a client id."));
    typed(&mut shell, "  Iv1.app  ");
    keys(&mut shell, &[Key::Tab]);
    typed(&mut shell, APP_SECRET);
    for size in [(80, 24), (100, 48)] {
        let rows = render(&mut shell, size.0, size.1);
        assert!(!rows.iter().any(|row| row.contains("gh-app")), "{size:?}");
    }
    keys(&mut shell, &[Key::Enter]);
    let job = one(&mut shell);
    assert_eq!(format!("{job:?}"), "ConnectorSetOwnApp");
    assert!(
        matches!(
            job,
            ConnectorsJob::SetOwnApp { ref name, ref client_id, secret: Some(_) }
                if name == "github" && client_id == "Iv1.app"
        ),
        "the client id is trimmed and the secret sent"
    );
    let saved = own_app_saved(SignInWith::Browser, true);
    arrive(&mut shell, ConnectorsEvent::Page(Ok(saved)));
    assert!(said(&shell).contains("GitHub signs in with your own app."));
    assert!(!said(&shell).contains("gh-app"));
    assert!(tall(&mut shell, "Client id: Iv1.app") && tall(&mut shell, "Client secret: saved"));
    assert!(tall(&mut shell, "d Remove my app") && tall(&mut shell, "o Use my own app"));
    assert!(
        !render(&mut shell, 100, 48)
            .iter()
            .any(|row| row.contains("gh-app"))
    );

    let mut shell = page(both(SignInWith::Browser));
    keys(&mut shell, &[Key::Down, Key::Enter, Key::Char('o')]);
    typed(&mut shell, "Iv1.public");
    keys(&mut shell, &[Key::Enter]);
    assert!(
        matches!(
            one(&mut shell),
            ConnectorsJob::SetOwnApp { secret: None, .. }
        ),
        "an empty secret is a public app's none"
    );
    let finishing = "connector_oauth_sign_in_finishing";
    arrive(&mut shell, ConnectorsEvent::Page(Err(finishing.into())));
    assert!(
        tall(&mut shell, &refusal(finishing)),
        "the form says to wait"
    );
    assert!(
        tall(&mut shell, "Your own OAuth app for GitHub"),
        "and stays open"
    );
}

/// Task 10 pages slice B: `d` removes a saved own app whichever way the connector signs in,
/// also where its file no longer offers one, so a stale secret can always go (addendum 30);
/// such a file sets none up, a connector with none has no section, and a finishing sign-in
/// says to wait.
#[test]
fn d_removes_the_own_app_even_where_the_file_no_longer_offers_one() {
    let mut shell = page(own_app_saved(SignInWith::Key, false));
    keys(&mut shell, &[Key::Down, Key::Enter]);
    assert!(tall(&mut shell, "Client id: Iv1.app") && tall(&mut shell, "d Remove my app"));
    assert!(!tall(&mut shell, "o Use my own app"));
    keys(&mut shell, &[Key::Char('o')]);
    assert!(requests(&mut shell).is_empty());
    assert!(
        !tall(&mut shell, "Your own OAuth app for GitHub"),
        "no form opens"
    );
    keys(&mut shell, &[Key::Char('d')]);
    assert!(matches!(
        one(&mut shell),
        ConnectorsJob::ClearOwnApp { ref name } if name == "github"
    ));
    arrive(&mut shell, ConnectorsEvent::Page(Ok(both(SignInWith::Key))));
    assert!(said(&shell).contains("GitHub no longer uses your own app."));
    assert!(!tall(&mut shell, "d Remove my app"));

    let finishing = "connector_oauth_sign_in_finishing";
    let mut shell = page(own_app_saved(SignInWith::Browser, true));
    keys(&mut shell, &[Key::Down, Key::Enter, Key::Char('d')]);
    one(&mut shell);
    arrive(&mut shell, ConnectorsEvent::Page(Err(finishing.into())));
    assert!(said(&shell).contains(&refusal(finishing)));

    let mut shell = page(view());
    keys(&mut shell, &[Key::Enter]);
    assert!(!tall(&mut shell, "Your own OAuth app"));
    keys(&mut shell, &[Key::Char('o'), Key::Char('d')]);
    assert!(
        requests(&mut shell).is_empty(),
        "a connector with no own app has none"
    );
}

/// Task 10 pages slice B: saving an own app while that connector's sign-in waits goes out,
/// and the engine's cancel of that sign-in is read and said on the next refresh.
#[test]
fn saving_an_own_app_while_its_sign_in_waits_ends_that_sign_in() {
    let mut shell = page(both(SignInWith::Browser));
    keys(&mut shell, &[Key::Down, Key::Enter, Key::Char('s')]);
    one(&mut shell);
    let url = LINK.to_owned();
    let started = SignInStarted { url, opened: true };
    let name = "github".to_owned();
    arrive(
        &mut shell,
        ConnectorsEvent::SignIn {
            name: name.clone(),
            result: Ok(started),
        },
    );
    keys(&mut shell, &[Key::Char('o')]);
    typed(&mut shell, "Iv1.app");
    keys(&mut shell, &[Key::Enter]);
    assert!(matches!(one(&mut shell), ConnectorsJob::SetOwnApp { .. }));
    let saved = own_app_saved(SignInWith::Browser, true);
    arrive(&mut shell, ConnectorsEvent::Page(Ok(saved)));
    shell.refresh();
    assert!(matches!(one(&mut shell), ConnectorsJob::SignInState { .. }));
    let cancelled = ConnectorsEvent::SignInState {
        name,
        result: Ok(SignInState::Cancelled),
    };
    arrive(&mut shell, cancelled);
    assert!(said(&shell).contains("The sign-in was cancelled."));
}
