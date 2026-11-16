use super::words::{refusal, trust_lines};
use super::*;
use crate::app_plugins::{
    Binding, ConnectorTrust, KeyTrust, OAuthTrust, Origin, PluginKind, PluginStatus, Preview,
    Problem, ReadOnlyTrust, RunsView, SecretView, TrustView,
};
use crate::connectors::{
    ConnectorSettings, ConnectorView, ConnectorsView, NativeConnectors, SignIn, SizeCap, Wait,
    WritePolicy,
};
use crate::engine::owner::ConnectorsSaved;
use crate::tui::client::OwnerEvent;
use crate::tui::client::connectors::{ConnectorsEvent, ConnectorsJob};
use crate::tui::client::settings::{SettingsEvent, SettingsJob};
use crate::tui::render_tests::{check, ready, render, shell_at};
use crate::tui::screens::open;
use crate::tui::seam::{Arrival, Request};
use crate::tui::shell::{Shell, targets};

const SHA: &str = "c0ffee";
const TYPED_KEY: &str = "lin-secret-typed";

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

fn refused_file(name: &str, problem: Problem) -> ConnectorView {
    ConnectorView {
        name: name.into(),
        title: name.into(),
        version: String::new(),
        description: String::new(),
        origin: Origin::User,
        status: PluginStatus::Refused { problem },
        sign_in: SignIn::NotYet,
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
    assert!(!shows(&mut shell, "/  Linear"), "Esc closes the connector's page");
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
    assert!(requests(&mut shell).is_empty(), "the save answered with the page");
    keys(&mut shell, &[Key::Down, Key::Left, Key::Enter]);
    assert!(matches!(one(&mut shell), ConnectorsJob::SaveSettings { version: 8, .. }));
    arrive(
        &mut shell,
        ConnectorsEvent::Saved(Ok(ConnectorsSaved::Stale(9))),
    );
    assert!(said(&shell).contains("The connector settings changed elsewhere."));
    assert!(read_all(&one(&mut shell)), "a stale save reads the page again");
    assert!(shows(&mut shell, "[256 KiB]"), "a stale save drops the pick");
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
    assert!(shows(&mut shell, "Settings  /  Connectors "), "Esc stayed on the page");
    assert!(requests(&mut shell).is_empty());
    keys(&mut shell, &[Key::Right, Key::Enter]);
    assert!(matches!(one(&mut shell), ConnectorsJob::SaveSettings { .. }));
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
    assert!(shows(&mut shell, "Sent to the engine once and never shown again."));
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
    assert!(!render(&mut shell, 80, 24).iter().any(|row| row.contains("lin-secret")));
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
    arrive(&mut shell, ConnectorsEvent::Page(Ok(with_secret(Binding::Key, 1))));
    keys(&mut shell, &[Key::Char('u')]);
    assert!(matches!(one(&mut shell), ConnectorsJob::ClearKey { .. }));
    arrive(
        &mut shell,
        ConnectorsEvent::Page(Err("app_plugin_not_found".into())),
    );
    assert!(said(&shell).contains("That connector is gone. The page now shows the current ones."));
    assert!(read_all(&one(&mut shell)), "a gone connector reads the page again");
}

#[test]
fn adding_shows_the_trust_screen_and_confirm_sends_the_text_and_its_sha() {
    let mut shell = page(view());
    keys(&mut shell, &[Key::Char('a')]);
    assert!(shows(&mut shell, "Add a connector"));
    assert!(shows(&mut shell, "Connector manifest (YAML)"));
    assert!(shows(&mut shell, "Nothing is used until you confirm its trust screen."));
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
            result: Ok(Preview::Ready { trust: trust(false) }),
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
    assert!(requests(&mut shell).is_empty(), "a refused manifest has no confirm");
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
    assert!(shows(&mut shell, "(app_plugin_manifest_invalid, line 2, mcp.url)"));
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
            result: Ok(Preview::Ready { trust: trust(false) }),
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
    assert!(shows(&mut shell, "Catalog"), "a removed connector's page closes");
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
    assert!(shows(&mut shell, "Connectors aren't available in this engine yet."));
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
    assert_eq!(lines[3], "Read-only address: https://board.example.com/mcp/readonly");
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
    assert!(tall.contains("The address must start with https://."), "{tall}");
    assert!(tall.contains("a plugin with this name already exists;"), "{tall}");
    let fix = "Fix its file, or remove it on the App plugins page.";
    assert_eq!(tall.matches(fix).count(), 2, "{tall}");
    assert_eq!(tall.matches("Reaches ").count(), 1, "only Linear reaches a host");
    assert!(shows(&mut shell, "Enter Open"), "Linear opens");
    keys(&mut shell, &[Key::Down]);
    assert!(!shows(&mut shell, "Enter Open"), "a refused file has no page to open");
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
    assert!(shows(&mut shell, "Fix its file, or remove it on the App plugins page."));
    assert!(!shows(&mut shell, "Reaches ") && !shows(&mut shell, "Address:"));
    assert!(!shows(&mut shell, "Signs in with OAuth"), "a refused file is not an OAuth one");
    assert!(!shows(&mut shell, "Enter Review") && !shows(&mut shell, "x Remove"));
    assert!(!shows(&mut shell, "k Set key"));
    keys(&mut shell, &[Key::Char('x'), Key::Enter, Key::Char('k')]);
    assert!(!shows(&mut shell, "Remove board?"), "x removes nothing here");
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
    assert!(shows(&mut shell, "Settings  /  Connectors  /  Lin\\u200bear"));
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
        tall.iter().any(|row| row.contains(title) && row.trim_end().ends_with(word))
    };
    assert!(reads("GitHub", "Unavailable"), "{tall:#?}");
    assert!(reads("Board", "Needs review"), "review comes before Unavailable");
    assert_eq!(tall.join("\n").matches(oauth).count(), 1, "{tall:#?}");
    keys(&mut shell, &[Key::Down, Key::Enter]);
    assert!(shows(&mut shell, "Settings  /  Connectors  /  GitHub"));
    assert_eq!(render(&mut shell, 80, 24).join("\n").matches(oauth).count(), 1);
    assert!(!shows(&mut shell, "k Set key"));
    keys(&mut shell, &[Key::Esc, Key::Down, Key::Enter]);
    assert!(shows(&mut shell, "Settings  /  Connectors  /  Board"));
    assert!(shows(&mut shell, "Review this connector before a project can use it."));
    assert!(!shows(&mut shell, oauth), "a connector under review shows the review line only");
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
    assert!(shows(&mut shell, "Esc Cancel"), "the box leaves the help bar");
    assert!(!shows(&mut shell, "y Confirm"), "no confirm before the end was shown");
    keys(&mut shell, &[Key::Char('y')]);
    assert!(requests(&mut shell).is_empty(), "y waits for the end");
    keys(&mut shell, &[Key::Down]);
    assert!(!shows(&mut shell, "Version 0.2.0"), "Down moves the body one line");
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
    assert!(!shows(&mut shell, "Esc Cancel"), "no Esc is offered while it is checked");
    keys(&mut shell, &[Key::Esc]);
    assert!(shows(&mut shell, "Add a connector"), "the form stays while it is checked");
    arrive(
        &mut shell,
        ConnectorsEvent::Preview {
            read: None,
            result: Ok(Preview::Ready { trust: trust(false) }),
        },
    );
    assert!(shows(&mut shell, "Trust Board?"));
    keys(&mut shell, &[Key::Char('y')]);
    assert!(matches!(one(&mut shell), ConnectorsJob::Add { .. }));
    keys(&mut shell, &[Key::Esc]);
    assert!(shows(&mut shell, "Trust Board?"), "the trust screen stays while it saves");
    arrive(&mut shell, ConnectorsEvent::Page(Ok(view())));
    assert!(said(&shell).contains("Board was added."));

    keys(&mut shell, &[Key::Down, Key::Down, Key::Enter, Key::Char('x')]);
    assert!(shows(&mut shell, "Remove Board?"));
    keys(&mut shell, &[Key::Char('y')]);
    assert!(matches!(
        one(&mut shell),
        ConnectorsJob::Remove { ref name } if name == "board"
    ));
    keys(&mut shell, &[Key::Esc]);
    assert!(shows(&mut shell, "Remove Board?"), "the dialog stays while it removes");
    let mut gone = view();
    gone.connectors.truncate(2);
    arrive(&mut shell, ConnectorsEvent::Page(Ok(gone)));
    assert!(said(&shell).contains("Board was removed."));
    assert!(shows(&mut shell, "Catalog"), "the removed connector's page closes");
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
