use crate::app_plugins::{manifest, plain_problem};
use serde_json::json;

const HEAD: &str = "schema: 1
kind: connector
name: acme-issues
title: Acme Issues
version: 1.0.0
description: Issues in Acme.
mcp:
  url: https://mcp.acme.example/mcp
  read_only_url: https://mcp.acme.example/mcp/readonly
  hosts: [mcp.acme.example]
";

const KEY: &str = "key:
  secret: api_key
  header: Authorization
  scheme: Bearer
  help: Create a read-only API key in Acme.
";

const OAUTH: &str = "oauth:
  hosts: [acme.example, api.acme.example]
  registration: [cimd, dcr]
  scopes_read: [read]
  scopes_write: [write]
";

const TOOLS: &str = "tools:
  read: [list_issues, get_issue]
";

const URL_LINE: &str = "  url: https://mcp.acme.example/mcp\n";
const READ_ONLY_LINE: &str = "  read_only_url: https://mcp.acme.example/mcp/readonly\n";
const HOSTS_LINE: &str = "  hosts: [mcp.acme.example]\n";
const READ_ONLY_HEADER: &str = "  read_only_header:
    name: X-MCP-Readonly
    value: \"true\"
";

fn full() -> String {
    [HEAD, KEY, OAUTH, TOOLS].concat()
}

fn keyed() -> String {
    [HEAD, KEY].concat()
}

fn oauth_only() -> String {
    [HEAD, OAUTH].concat()
}

fn swap(text: &str, old: &str, new: &str) -> String {
    assert!(text.contains(old), "{old}");
    text.replacen(old, new, 1)
}

fn refused(text: &str) -> (String, Option<String>) {
    let problem = manifest::parse(text).expect_err("the manifest is refused");
    (problem.code, problem.field)
}

fn refusal(code: &str, field: &str) -> (String, Option<String>) {
    (code.to_owned(), Some(field.to_owned()))
}

fn trust(text: &str) -> serde_json::Value {
    let manifest = manifest::parse(text).unwrap_or_else(|problem| panic!("{problem:?}"));
    serde_json::to_value(manifest.trust_view(false)).unwrap()
}

#[test]
fn a_connector_naming_a_stdio_command_or_an_http_host_is_refused() {
    assert_eq!(
        refused(&swap(
            &keyed(),
            URL_LINE,
            "  command: npx\n  url: https://mcp.acme.example/mcp\n"
        )),
        refusal("app_plugin_field_unknown", "mcp.command")
    );
    assert_eq!(
        refused(
            &[
                keyed().as_str(),
                "local:\n  runtime: static-retrieval-mrl-en-v1\n"
            ]
            .concat()
        ),
        refusal("app_plugin_field_unknown", "local")
    );
    assert_eq!(
        refused(
            &[
                keyed().as_str(),
                "http:\n  url: https://mcp.acme.example/mcp\n"
            ]
            .concat()
        ),
        refusal("app_plugin_field_unknown", "http")
    );
    assert_eq!(
        refused(&swap(
            &keyed(),
            URL_LINE,
            "  url: http://mcp.acme.example/mcp\n"
        )),
        refusal("app_plugin_connector_url_not_https", "mcp.url")
    );
    let loopback = swap(
        &swap(&keyed(), URL_LINE, "  url: http://127.0.0.1:9000/mcp\n"),
        HOSTS_LINE,
        "  hosts: [127.0.0.1]\n",
    );
    assert_eq!(
        refused(&swap(&loopback, READ_ONLY_LINE, "")),
        refusal("app_plugin_connector_url_not_https", "mcp.url")
    );
    for host in ["127.0.0.1", "localhost", "[::1]", "10.0.0.8"] {
        let private = swap(
            &swap(&keyed(), URL_LINE, &format!("  url: https://{host}/mcp\n")),
            HOSTS_LINE,
            &format!("  hosts: [\"{host}\"]\n"),
        );
        assert_eq!(
            refused(&swap(&private, READ_ONLY_LINE, "")),
            refusal("app_plugin_connector_host_public", "mcp.url"),
            "{host}"
        );
    }
    assert_eq!(
        refused(&swap(
            &keyed(),
            READ_ONLY_LINE,
            "  read_only_url: http://mcp.acme.example/mcp/readonly\n"
        )),
        refusal("app_plugin_connector_url_not_https", "mcp.read_only_url")
    );
}

#[test]
fn a_connector_url_on_an_undeclared_host_is_refused() {
    assert_eq!(
        refused(&swap(&keyed(), HOSTS_LINE, "  hosts: [other.example]\n")),
        refusal("app_plugin_host_undeclared", "mcp.url")
    );
    assert_eq!(
        refused(&swap(
            &keyed(),
            READ_ONLY_LINE,
            "  read_only_url: https://ro.acme.example/mcp\n"
        )),
        refusal("app_plugin_host_undeclared", "mcp.read_only_url")
    );
    assert_eq!(
        refused(&swap(
            &keyed(),
            HOSTS_LINE,
            "  hosts: [mcp.acme.example, extra.example]\n"
        )),
        refusal("app_plugin_host_unused", "mcp.hosts")
    );
    assert_eq!(
        refused(&swap(
            &keyed(),
            URL_LINE,
            "  url: https://user:pass@mcp.acme.example/mcp\n"
        )),
        refusal("app_plugin_url_invalid", "mcp.url")
    );
    let second_host = swap(
        &swap(
            &keyed(),
            READ_ONLY_LINE,
            "  read_only_url: https://ro.acme.example/mcp\n",
        ),
        HOSTS_LINE,
        "  hosts: [mcp.acme.example, ro.acme.example]\n",
    );
    assert_eq!(
        trust(&second_host)["runs"]["hosts"],
        json!(["mcp.acme.example", "ro.acme.example"])
    );
}

#[test]
fn the_trust_screen_lists_the_mcp_host_and_every_oauth_host() {
    let view = trust(&full());
    assert_eq!(view["kind"], "connector");
    assert_eq!(view["name"], "acme-issues");
    assert_eq!(view["runs"]["kind"], "connector");
    assert_eq!(view["runs"]["url"], "https://mcp.acme.example/mcp");
    assert_eq!(view["runs"]["hosts"], json!(["mcp.acme.example"]));
    assert_eq!(
        view["runs"]["oauth_hosts"],
        json!(["acme.example", "api.acme.example"])
    );
    assert_eq!(view["secrets"], json!(["api_key"]));
    assert_eq!(trust(&keyed())["runs"]["oauth_hosts"], json!([]));
    let basic = swap(
        &keyed(),
        "  scheme: Bearer\n",
        "  scheme: Basic\n  user_secret: email\n",
    );
    assert_eq!(trust(&basic)["secrets"], json!(["api_key", "email"]));
    assert_eq!(trust(&oauth_only())["secrets"], json!([]));
}

#[test]
fn a_connector_with_neither_key_nor_oauth_is_refused() {
    assert_eq!(
        refused(HEAD),
        refusal("app_plugin_connector_auth_missing", "key")
    );
    assert_eq!(
        refused(&[HEAD, TOOLS].concat()),
        refusal("app_plugin_connector_auth_missing", "key")
    );
    assert!(manifest::parse(&keyed()).is_ok());
    assert!(manifest::parse(&oauth_only()).is_ok());
}

#[test]
fn a_connector_with_no_read_only_url_or_header_parses_and_has_read_only_none() {
    assert_eq!(trust(&keyed())["runs"]["vendor_read_only"], true);
    let none = swap(&keyed(), READ_ONLY_LINE, "");
    assert_eq!(trust(&none)["runs"]["vendor_read_only"], false);
    let header = swap(&keyed(), READ_ONLY_LINE, READ_ONLY_HEADER);
    assert_eq!(trust(&header)["runs"]["vendor_read_only"], true);
    let both = swap(
        &keyed(),
        READ_ONLY_LINE,
        &[READ_ONLY_LINE, READ_ONLY_HEADER].concat(),
    );
    assert_eq!(
        refused(&both),
        refusal(
            "app_plugin_connector_read_only_both",
            "mcp.read_only_header"
        )
    );
    assert_eq!(
        refused(&swap(
            &keyed(),
            READ_ONLY_LINE,
            "  read_only_url: https://mcp.acme.example/mcp\n"
        )),
        refusal("app_plugin_field_invalid", "mcp.read_only_url")
    );
    for name in [
        "Authorization",
        "authorization",
        "Mcp-Session-Id",
        "MCP-Protocol-Version",
        "Accept",
        "Host",
        "Cookie",
    ] {
        assert_eq!(
            refused(&swap(&header, "X-MCP-Readonly", name)),
            refusal("app_plugin_header_reserved", "mcp.read_only_header.name"),
            "{name}"
        );
    }
    assert_eq!(
        refused(&swap(&header, "X-MCP-Readonly", "X MCP")),
        refusal("app_plugin_header_invalid", "mcp.read_only_header.name")
    );
    for value in ["\"caf\\u00e9\"", "\"a\\tb\"", "\"{{secret.api_key}}\""] {
        assert_eq!(
            refused(&swap(&header, "\"true\"", value)),
            refusal("app_plugin_header_invalid", "mcp.read_only_header.value"),
            "{value}"
        );
    }
    let custom = swap(
        &header,
        "  header: Authorization\n",
        "  header: X-MCP-Readonly\n",
    );
    assert_eq!(
        refused(&custom),
        refusal("app_plugin_header_reserved", "mcp.read_only_header.name")
    );
}

#[test]
fn a_connector_key_names_its_header_scheme_and_secrets_strictly() {
    let scheme = "  scheme: Bearer\n";
    assert_eq!(
        refused(&swap(&keyed(), scheme, "  scheme: Basic\n")),
        refusal("app_plugin_field_missing", "key.user_secret")
    );
    assert_eq!(
        refused(&swap(
            &keyed(),
            scheme,
            "  scheme: Bearer\n  user_secret: email\n"
        )),
        refusal("app_plugin_connector_user_secret", "key.user_secret")
    );
    assert_eq!(
        refused(&swap(
            &keyed(),
            scheme,
            "  scheme: Basic\n  user_secret: api_key\n"
        )),
        refusal("app_plugin_secret_duplicate", "key.user_secret")
    );
    for word in ["Digest", "bearer", "Token"] {
        assert_eq!(
            refused(&swap(&keyed(), scheme, &format!("  scheme: {word}\n"))),
            refusal("app_plugin_connector_scheme_unknown", "key.scheme"),
            "{word}"
        );
    }
    let header = "  header: Authorization\n";
    for name in ["Host", "Content-Length", "Mcp-Session-Id", "Accept"] {
        assert_eq!(
            refused(&swap(&keyed(), header, &format!("  header: {name}\n"))),
            refusal("app_plugin_header_reserved", "key.header"),
            "{name}"
        );
    }
    assert_eq!(
        refused(&swap(&keyed(), header, "  header: \"X Key\"\n")),
        refusal("app_plugin_header_invalid", "key.header")
    );
    assert_eq!(
        refused(&swap(
            &keyed(),
            "  secret: api_key\n",
            "  secret: API-KEY\n"
        )),
        refusal("app_plugin_secret_invalid", "key.secret")
    );
    assert_eq!(
        refused(&[keyed().as_str(), "  value: sk-test-key-0001\n"].concat()),
        refusal("app_plugin_field_unknown", "key.value")
    );
    assert!(manifest::parse(&swap(&keyed(), header, "  header: X-Api-Key\n")).is_ok());
}

#[test]
fn connector_hosts_scopes_registration_and_read_tools_are_bounded() {
    let five = "[a.example, b.example, c.example, d.example, e.example]";
    assert_eq!(
        refused(&swap(
            &keyed(),
            HOSTS_LINE,
            &format!("  hosts: [mcp.acme.example, {}\n", &five[1..])
        )),
        refusal("app_plugin_hosts_limit", "mcp.hosts")
    );
    let oauth_hosts = "  hosts: [acme.example, api.acme.example]\n";
    assert_eq!(
        refused(&swap(&full(), oauth_hosts, &format!("  hosts: {five}\n"))),
        refusal("app_plugin_hosts_limit", "oauth.hosts")
    );
    assert_eq!(
        refused(&swap(
            &full(),
            oauth_hosts,
            "  hosts: [\"*.acme.example\"]\n"
        )),
        refusal("app_plugin_host_invalid", "oauth.hosts")
    );
    assert_eq!(
        refused(&swap(&full(), oauth_hosts, "  hosts: [127.0.0.1]\n")),
        refusal("app_plugin_connector_host_public", "oauth.hosts")
    );
    assert_eq!(
        refused(&swap(&full(), oauth_hosts, "")),
        refusal("app_plugin_field_missing", "oauth.hosts")
    );
    let registration = "  registration: [cimd, dcr]\n";
    for list in ["[cimd, cimd]", "[magic]", "[]"] {
        assert_eq!(
            refused(&swap(
                &full(),
                registration,
                &format!("  registration: {list}\n")
            )),
            refusal(
                "app_plugin_connector_registration_invalid",
                "oauth.registration"
            ),
            "{list}"
        );
    }
    assert_eq!(
        refused(&swap(&full(), registration, "")),
        refusal("app_plugin_field_missing", "oauth.registration")
    );
    assert!(manifest::parse(&swap(&full(), registration, "  registration: [own]\n")).is_ok());
    let scopes = "  scopes_read: [read]\n";
    let many: Vec<String> = (0..17).map(|index| format!("s{index}")).collect();
    for list in [
        "[\"read all\"]".to_owned(),
        "[read, read]".to_owned(),
        format!("[{}]", many.join(", ")),
    ] {
        assert_eq!(
            refused(&swap(&full(), scopes, &format!("  scopes_read: {list}\n"))),
            refusal("app_plugin_connector_scope_invalid", "oauth.scopes_read"),
            "{list}"
        );
    }
    let read = "  read: [list_issues, get_issue]\n";
    let names = |count: usize| -> String {
        (0..count)
            .map(|index| format!("t{index}"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    assert!(manifest::parse(&swap(&full(), read, &format!("  read: [{}]\n", names(256)))).is_ok());
    assert_eq!(
        refused(&swap(&full(), read, &format!("  read: [{}]\n", names(257)))),
        refusal("app_plugin_connector_tools_limit", "tools.read")
    );
    for list in ["[\"bad name\"]", "[get_issue, get_issue]"] {
        assert_eq!(
            refused(&swap(&full(), read, &format!("  read: {list}\n"))),
            refusal("app_plugin_connector_tool_invalid", "tools.read"),
            "{list}"
        );
    }
    assert_eq!(
        refused(&swap(&full(), read, "  write: [create_issue]\n")),
        refusal("app_plugin_field_unknown", "tools.write")
    );
}

#[test]
fn every_connector_refusal_has_a_plain_sentence() {
    for code in [
        "app_plugin_connector_url_not_https",
        "app_plugin_connector_host_public",
        "app_plugin_connector_auth_missing",
        "app_plugin_connector_read_only_both",
        "app_plugin_connector_user_secret",
        "app_plugin_connector_scheme_unknown",
        "app_plugin_connector_registration_invalid",
        "app_plugin_connector_scope_invalid",
        "app_plugin_connector_tools_limit",
        "app_plugin_connector_tool_invalid",
        "app_plugin_connector_oauth_not_yet",
        "app_plugin_connector_elsewhere",
    ] {
        assert!(plain_problem(code).is_some(), "{code}");
    }
    assert_eq!(
        plain_problem("app_plugin_kind_unknown"),
        Some("the kinds are embedding and connector.")
    );
}

#[test]
fn the_trust_screen_shows_every_field_the_gateway_acts_on() {
    let view = trust(&full());
    assert_eq!(
        view["connector"],
        json!({
            "read_only": {"kind": "url", "url": "https://mcp.acme.example/mcp/readonly"},
            "key": {"header": "Authorization", "scheme": "Bearer", "secrets": ["api_key"]},
            "oauth": {
                "hosts": ["acme.example", "api.acme.example"],
                "registration": ["cimd", "dcr"],
                "scopes_read": ["read"],
                "scopes_write": ["write"]
            },
            "read_tools": ["list_issues", "get_issue"]
        })
    );
    let header = trust(&swap(&keyed(), READ_ONLY_LINE, READ_ONLY_HEADER));
    assert_eq!(
        header["connector"]["read_only"],
        json!({"kind": "header", "name": "X-MCP-Readonly", "value": "true"})
    );
    assert_eq!(header["connector"]["oauth"], json!(null));
    assert_eq!(header["connector"]["read_tools"], json!([]));
    let plain = trust(&swap(&keyed(), READ_ONLY_LINE, ""));
    assert_eq!(plain["connector"]["read_only"], json!({"kind": "none"}));
    let basic = trust(&swap(
        &keyed(),
        "  scheme: Bearer\n",
        "  scheme: Basic\n  user_secret: email\n",
    ));
    assert_eq!(
        basic["connector"]["key"],
        json!({"header": "Authorization", "scheme": "Basic", "secrets": ["api_key", "email"]})
    );
    assert_eq!(trust(&oauth_only())["connector"]["key"], json!(null));
    let own = trust(&swap(
        &oauth_only(),
        "  registration: [cimd, dcr]\n",
        "  registration: [own]\n",
    ));
    assert_eq!(own["connector"]["oauth"]["registration"], json!(["own"]));

    let embedding = manifest::parse(crate::app_plugins::store::bundled()[1].1).unwrap();
    assert!(
        serde_json::to_value(embedding.trust_view(false))
            .unwrap()
            .get("connector")
            .is_none(),
        "an embedding plugin's trust screen is unchanged"
    );
    let linear = manifest::parse(crate::app_plugins::store::bundled()[4].1).unwrap();
    assert_eq!(
        serde_json::to_value(linear.trust_view(false)).unwrap()["connector"]["read_only"],
        json!({"kind": "url", "url": "https://mcp.linear.app/mcp/readonly"})
    );
}

#[test]
fn a_connector_address_with_a_query_is_refused() {
    assert_eq!(
        refused(&swap(
            &keyed(),
            URL_LINE,
            "  url: https://mcp.acme.example/mcp?token=abc\n"
        )),
        refusal("app_plugin_url_invalid", "mcp.url")
    );
    assert_eq!(
        refused(&swap(
            &keyed(),
            READ_ONLY_LINE,
            "  read_only_url: https://mcp.acme.example/mcp/readonly?mode=ro\n"
        )),
        refusal("app_plugin_url_invalid", "mcp.read_only_url")
    );
    assert_eq!(
        refused(&swap(
            &keyed(),
            URL_LINE,
            "  url: https://mcp.acme.example/mcp?\n"
        )),
        refusal("app_plugin_url_invalid", "mcp.url")
    );
}

#[cfg(unix)]
mod stored {
    use super::{HOSTS_LINE, READ_ONLY_LINE, URL_LINE, full, keyed, oauth_only, swap};
    use crate::app_plugins::store::{self, Loaded};
    use crate::app_plugins::{Binding, Listing, Origin, PluginStatus, PluginView, Preview};
    use crate::engine::EngineError;
    use serde_json::json;
    use std::os::unix::fs::PermissionsExt;

    struct Case {
        _temp: tempfile::TempDir,
        root: std::path::PathBuf,
        db: rusqlite::Connection,
    }

    fn case() -> Case {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        Case {
            _temp: temp,
            root,
            db: rusqlite::Connection::open_in_memory().unwrap(),
        }
    }

    fn sha(text: &str) -> String {
        crate::semantic::sha256_hex(text.as_bytes())
    }

    fn plugin<'a>(listing: &'a Listing, name: &str) -> &'a PluginView {
        listing
            .plugins
            .iter()
            .find(|plugin| plugin.name == name)
            .unwrap_or_else(|| panic!("no plugin {name}"))
    }

    fn code(result: Result<Loaded, EngineError>) -> String {
        result.expect_err("refused").0
    }

    fn put(case: &Case, text: &str, replace: bool, now_ms: i64) {
        store::add(&case.db, &case.root, text, &sha(text), replace, now_ms).unwrap();
    }

    fn key(case: &Case, secret: &str) {
        store::set_key(
            &case.db,
            &case.root,
            "acme-issues",
            secret,
            b"sk-test-key-0001".to_vec(),
        )
        .unwrap();
    }

    fn stored_keys(case: &Case) -> usize {
        std::fs::read_dir(case.root.join(crate::storage::accounts::PLUGIN_KEYS))
            .unwrap()
            .count()
    }

    #[test]
    fn every_bundled_connector_parses_and_is_trusted_by_shipping() {
        let names: Vec<&str> = store::bundled().iter().map(|(name, _)| *name).collect();
        assert_eq!(
            names,
            [
                "local",
                "openai",
                "voyage",
                "ollama",
                "linear",
                "github",
                "atlassian"
            ]
        );
        let case = case();
        let listing = store::list(&case.db, &case.root).unwrap();
        let facts = [
            (
                "linear",
                "https://mcp.linear.app/mcp",
                "mcp.linear.app",
                true,
                json!(["api_key"]),
            ),
            (
                "github",
                "https://api.githubcopilot.com/mcp/",
                "api.githubcopilot.com",
                true,
                json!(["token"]),
            ),
            (
                "atlassian",
                "https://mcp.atlassian.com/v2/mcp",
                "mcp.atlassian.com",
                false,
                json!(["api_token", "email"]),
            ),
        ];
        for (name, url, host, vendor_read_only, secrets) in facts {
            let connector = plugin(&listing, name);
            assert_eq!(
                (connector.origin, &connector.status, connector.enabled),
                (Origin::Bundled, &PluginStatus::NeedsKey, false),
                "{name}"
            );
            let view = serde_json::to_value(connector).unwrap();
            assert_eq!(view["kind"], "connector", "{name}");
            assert_eq!(view["runs"]["kind"], "connector", "{name}");
            assert_eq!(view["runs"]["url"], url, "{name}");
            assert_eq!(view["runs"]["hosts"], json!([host]), "{name}");
            assert_eq!(view["runs"]["oauth_hosts"], json!([]), "{name}");
            assert_eq!(view["runs"]["vendor_read_only"], vendor_read_only, "{name}");
            let declared: Vec<&str> = connector
                .secrets
                .iter()
                .map(|secret| secret.name.as_str())
                .collect();
            assert_eq!(json!(declared), secrets, "{name}");
            assert!(matches!(
                store::review(&case.root, name).unwrap(),
                Preview::Ready { .. }
            ));
            store::set_enabled(&case.db, &case.root, name, true).unwrap();
            assert_eq!(
                code(store::ready(&case.db, &case.root, name)),
                "app_plugin_needs_key",
                "{name}"
            );
            assert_eq!(
                store::remove(&case.db, &case.root, name).unwrap_err().0,
                "app_plugin_bundled",
                "{name}"
            );
        }
        assert!(!case.root.join(crate::app_plugins::FOLDER).exists());
    }

    #[test]
    fn oauth_sign_in_is_refused_until_slice_3_with_a_plain_reason() {
        let case = case();
        put(&case, &oauth_only(), false, 1);
        assert_eq!(
            code(store::ready(&case.db, &case.root, "acme-issues")),
            "app_plugin_connector_oauth_not_yet"
        );
        let plain = crate::app_plugins::plain_problem("app_plugin_connector_oauth_not_yet")
            .expect("a plain sentence");
        assert!(
            plain.contains("sign") && !plain.contains('\u{2014}'),
            "{plain}"
        );
        put(&case, &full(), true, 2);
        assert_eq!(
            code(store::ready(&case.db, &case.root, "acme-issues")),
            "app_plugin_needs_key"
        );
        key(&case, "api_key");
        let loaded = store::ready(&case.db, &case.root, "acme-issues").unwrap();
        assert_eq!(loaded.bindings.len(), 1);
    }

    #[test]
    fn a_replaced_connector_on_other_hosts_loses_its_key() {
        let case = case();
        put(&case, &full(), false, 1);
        key(&case, "api_key");
        let bumped = swap(&full(), "version: 1.0.0", "version: 1.0.1");
        put(&case, &bumped, true, 2);
        let listing = store::list(&case.db, &case.root).unwrap();
        assert_eq!(
            plugin(&listing, "acme-issues").secrets[0].binding,
            Binding::Key
        );
        let oauth_moved = swap(
            &bumped,
            "  hosts: [acme.example, api.acme.example]\n",
            "  hosts: [acme.example, login.other.example]\n",
        );
        put(&case, &oauth_moved, true, 3);
        let listing = store::list(&case.db, &case.root).unwrap();
        assert_eq!(
            plugin(&listing, "acme-issues").secrets[0].binding,
            Binding::None
        );
        assert_eq!(stored_keys(&case), 0);
        key(&case, "api_key");
        let mcp_moved = swap(
            &swap(
                &swap(
                    &oauth_moved,
                    URL_LINE,
                    "  url: https://mcp.other.example/mcp\n",
                ),
                READ_ONLY_LINE,
                "",
            ),
            HOSTS_LINE,
            "  hosts: [mcp.other.example]\n",
        );
        put(&case, &mcp_moved, true, 4);
        let listing = store::list(&case.db, &case.root).unwrap();
        assert_eq!(
            plugin(&listing, "acme-issues").secrets[0].binding,
            Binding::None
        );
        assert_eq!(stored_keys(&case), 0);
        let basic = swap(
            &keyed(),
            "  scheme: Bearer\n",
            "  scheme: Basic\n  user_secret: email\n",
        );
        put(&case, &basic, true, 5);
        key(&case, "api_key");
        assert_eq!(
            code(store::ready(&case.db, &case.root, "acme-issues")),
            "app_plugin_needs_key"
        );
        key(&case, "email");
        let loaded = store::ready(&case.db, &case.root, "acme-issues").unwrap();
        assert_eq!(loaded.manifest.secrets(), ["api_key", "email"]);
        assert_eq!(loaded.bindings.len(), 2);
    }
}
