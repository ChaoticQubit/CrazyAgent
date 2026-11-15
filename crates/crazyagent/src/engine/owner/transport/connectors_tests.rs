use super::*;
use crate::app_plugins::PluginStatus;
use crate::connectors::grants::{self, Role};
use crate::connectors::{
    ConnectorView, Grant, KeepLog, NativeConnectors, SignIn, SizeCap, Wait, WritePolicy,
};
use serde_json::json;

const PROJECT: &str = "alpha";
const SECRET: &[u8] = b"sk-test-key-0001";

const BOARD: &str = "schema: 1
kind: connector
name: board
title: Board
version: 1
mcp:
  url: https://mcp.board.example/mcp
  hosts: [mcp.board.example]
key:
  secret: api_key
  header: Authorization
  scheme: Bearer
";

const NEAR: &str = "schema: 1
kind: embedding
name: near
title: Near
version: 1
http:
  url: http://127.0.0.1:9000/embed
  hosts: [127.0.0.1]
  body:
    input: \"{{inputs}}\"
  vectors: embeddings.*
  dimensions: 128
";

/// An engine root whose database holds the authority schemas and one set-up project.
/// Output: the temporary folder and the engine root.
fn engine() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::Builder::new()
        .prefix("ca-owner-connectors-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    let base = tmp.path().canonicalize().unwrap();
    let root = base.join("engine");
    std::fs::create_dir(&root).unwrap();
    let folder = base.join("project");
    std::fs::create_dir(&folder).unwrap();
    let db = crate::questions::open(&root).unwrap();
    crate::storage::initialize_authority_schemas(&db).unwrap();
    crate::ide::workspace::tests::add_project(&db, PROJECT, &folder);
    (tmp, root)
}

/// Sends one request through this protocol's selector, parsing and handler, as an owner
/// client's frame body plus the key frame a `set_key` announces. Inputs: the engine root,
/// the body and the key. Output: the reply, or the refusal.
fn send(root: &Path, body: serde_json::Value, key: Option<&[u8]>) -> Result<Reply, EngineError> {
    let bytes = serde_json::to_vec(&body).unwrap();
    assert!(matches!(protocol(&bytes).unwrap(), Protocol::Connectors));
    let (action, key_present) = parse(&bytes)?;
    assert_eq!(key_present, key.is_some());
    run(
        root,
        action,
        key.map(|key| AccountSecret::new(key.to_vec()).unwrap()),
    )
}

fn request(action: serde_json::Value) -> serde_json::Value {
    json!({"connectors": action, "secret_present": false})
}

fn page(reply: Reply) -> ConnectorsView {
    match reply {
        Reply::Connectors { view } => *view,
        other => panic!("expected the page, got {other:?}"),
    }
}

fn ask(root: &Path, action: serde_json::Value) -> ConnectorsView {
    page(send(root, request(action), None).unwrap())
}

fn refused(root: &Path, action: serde_json::Value) -> String {
    send(root, request(action), None).unwrap_err().0
}

fn named<'a>(view: &'a ConnectorsView, name: &str) -> &'a ConnectorView {
    view.connectors
        .iter()
        .find(|connector| connector.name == name)
        .unwrap_or_else(|| panic!("no connector {name}"))
}

fn sha(text: &str) -> String {
    crate::semantic::sha256_hex(text.as_bytes())
}

fn set_key(root: &Path, name: &str) -> Result<Reply, EngineError> {
    send(
        root,
        json!({
            "connectors": {"kind": "set_key", "name": name, "secret": "api_key"},
            "secret_present": true
        }),
        Some(SECRET),
    )
}

fn reach(root: &Path, role: Role) -> Vec<String> {
    let db = crate::questions::open(root).unwrap();
    grants::for_launch(&db, root, PROJECT, role)
        .unwrap()
        .into_iter()
        .map(|granted| granted.name)
        .collect()
}

#[test]
fn every_setting_option_round_trips_through_the_owner_protocol() {
    let (_tmp, root) = engine();
    let first = ask(&root, json!({"kind": "view", "project": null}));
    assert_eq!(first.settings, ConnectorSettings::DEFAULT);
    assert_eq!(first.version, 0);

    let mut options = Vec::new();
    for write in WritePolicy::ALL {
        options.push(ConnectorSettings {
            write,
            ..ConnectorSettings::DEFAULT
        });
    }
    for wait in Wait::ALL {
        options.push(ConnectorSettings {
            wait,
            ..ConnectorSettings::DEFAULT
        });
    }
    for native in NativeConnectors::ALL {
        options.push(ConnectorSettings {
            native,
            ..ConnectorSettings::DEFAULT
        });
    }
    for size_cap in SizeCap::ALL {
        options.push(ConnectorSettings {
            size_cap,
            ..ConnectorSettings::DEFAULT
        });
    }
    for keep_log in KeepLog::ALL {
        options.push(ConnectorSettings {
            keep_log,
            ..ConnectorSettings::DEFAULT
        });
    }
    assert_eq!(options.len(), 14);
    let mut version = 0;
    for settings in options {
        let saved = ask(
            &root,
            json!({"kind": "set", "settings": settings, "version": version}),
        );
        assert_eq!(saved.settings, settings);
        assert_eq!(saved.version, version + 1);
        version = saved.version;
        let read = ask(&root, json!({"kind": "view", "project": null}));
        assert_eq!((read.settings, read.version), (settings, version));
    }

    let wire = json!({
        "write": "ask_once_per_project",
        "wait": "minutes30",
        "native": "on",
        "size_cap": "mib1",
        "keep_log": "days90"
    });
    let saved = ask(
        &root,
        json!({"kind": "set", "settings": wire, "version": version}),
    );
    assert_eq!(
        saved.settings,
        ConnectorSettings {
            write: WritePolicy::AskOncePerProject,
            wait: Wait::Minutes30,
            native: NativeConnectors::On,
            size_cap: SizeCap::Mib1,
            keep_log: KeepLog::Days90,
        }
    );
    assert_eq!(
        serde_json::to_value(ConnectorSettings::DEFAULT).unwrap(),
        json!({
            "write": "ask_every_time",
            "wait": "minutes10",
            "native": "off",
            "size_cap": "kib256",
            "keep_log": "days30"
        })
    );

    match send(
        &root,
        request(json!({"kind": "set", "settings": wire, "version": 0})),
        None,
    )
    .unwrap()
    {
        Reply::Stale { current } => assert_eq!(current, version + 1),
        other => panic!("expected stale, got {other:?}"),
    }
    let mut unknown_option = wire.clone();
    unknown_option["wait"] = json!("minutes5");
    assert_eq!(
        refused(
            &root,
            json!({"kind": "set", "settings": unknown_option, "version": version + 1})
        ),
        "connector_request_invalid"
    );
    assert_eq!(
        ask(&root, json!({"kind": "view", "project": null})).settings,
        saved.settings
    );
}

#[test]
fn the_owner_adds_keys_grants_and_removes_a_connector_through_the_protocol() {
    let (_tmp, root) = engine();
    let first = ask(&root, json!({"kind": "view", "project": PROJECT}));
    let names: Vec<&str> = first
        .connectors
        .iter()
        .map(|connector| connector.name.as_str())
        .collect();
    assert_eq!(names, ["linear", "github", "atlassian"]);
    assert!(first.grants.is_empty());
    assert_eq!(first.project.as_deref(), Some(PROJECT));
    assert!(
        serde_json::to_value(&first.connectors[0])
            .unwrap()
            .get("enabled")
            .is_none(),
        "a connector has no app-wide switch; its switch is per project"
    );

    let sha256 = match send(
        &root,
        request(json!({"kind": "preview", "yaml": BOARD})),
        None,
    )
    .unwrap()
    {
        Reply::Preview { preview } => match *preview {
            Preview::Ready { trust } => {
                assert_eq!(trust.kind, PluginKind::Connector);
                assert!(!trust.replaces);
                trust.sha256
            }
            other => panic!("expected a trust screen, got {other:?}"),
        },
        other => panic!("expected a preview, got {other:?}"),
    };
    assert_eq!(sha256, sha(BOARD));
    let added = ask(
        &root,
        json!({"kind": "add", "yaml": BOARD, "sha256": sha256, "replace": false}),
    );
    assert_eq!(named(&added, "board").status, PluginStatus::NeedsKey);
    assert_eq!(named(&added, "board").sign_in, SignIn::Key);
    assert!(added.grants.is_empty() && added.project.is_none());
    for role in [Role::Master, Role::Agent { tools_on: true }] {
        assert!(reach(&root, role).is_empty(), "added is not turned on");
    }

    let grant = json!({
        "kind": "grant",
        "project": PROJECT,
        "connector": "board",
        "enabled": true,
        "who": "master"
    });
    assert_eq!(refused(&root, grant.clone()), "app_plugin_needs_key");

    let keyed = set_key(&root, "board").unwrap();
    let text = serde_json::to_string(&keyed).unwrap();
    assert!(
        !text.contains("sk-test-key-0001"),
        "the page never holds a key"
    );
    assert_eq!(named(&page(keyed), "board").status, PluginStatus::Ready);

    let granted = ask(&root, grant);
    assert_eq!(granted.project.as_deref(), Some(PROJECT));
    assert_eq!(
        granted.grants,
        [Grant {
            connector: "board".into(),
            enabled: true,
            who: Who::Master,
        }]
    );
    assert_eq!(reach(&root, Role::Master), ["board"]);
    assert!(reach(&root, Role::Agent { tools_on: true }).is_empty());
    assert!(
        ask(&root, json!({"kind": "view", "project": null}))
            .grants
            .is_empty()
    );

    let cleared = ask(
        &root,
        json!({"kind": "clear_key", "name": "board", "secret": "api_key"}),
    );
    assert_eq!(named(&cleared, "board").status, PluginStatus::NeedsKey);
    assert!(reach(&root, Role::Master).is_empty());

    let removed = ask(&root, json!({"kind": "remove", "name": "board"}));
    assert!(
        removed
            .connectors
            .iter()
            .all(|connector| connector.name != "board")
    );
    assert!(
        ask(&root, json!({"kind": "view", "project": PROJECT}))
            .grants
            .is_empty(),
        "a removed connector's switches go with it"
    );
    assert_eq!(
        refused(&root, json!({"kind": "remove", "name": "linear"})),
        "app_plugin_bundled"
    );
}

#[test]
fn the_connectors_page_never_changes_an_embedding_plugin() {
    let (_tmp, root) = engine();
    let db = crate::questions::open(&root).unwrap();
    store::add(&db, &root, NEAR, &sha(NEAR), false, 1).unwrap();
    match send(
        &root,
        request(json!({"kind": "preview", "yaml": NEAR.replacen("near", "nearer", 1)})),
        None,
    )
    .unwrap()
    {
        Reply::Preview { preview } => match *preview {
            Preview::Refused { problem } => assert_eq!(
                (problem.code.as_str(), problem.field.as_deref()),
                ("app_plugin_not_connector", Some("kind"))
            ),
            other => panic!("expected a refusal, got {other:?}"),
        },
        other => panic!("expected a preview, got {other:?}"),
    }
    let other = NEAR.replacen("near", "nearer", 1);
    assert_eq!(
        refused(
            &root,
            json!({"kind": "add", "yaml": other, "sha256": sha(&other), "replace": false})
        ),
        "app_plugin_refused"
    );
    assert_eq!(
        refused(
            &root,
            json!({"kind": "add", "yaml": NEAR, "sha256": sha(NEAR), "replace": true})
        ),
        "app_plugin_refused"
    );
    for name in ["near", "openai", "nothing"] {
        for action in [
            json!({"kind": "review", "name": name}),
            json!({"kind": "trust", "name": name, "sha256": sha(NEAR)}),
            json!({"kind": "remove", "name": name}),
            json!({"kind": "clear_key", "name": name, "secret": "api_key"}),
            json!({
                "kind": "grant",
                "project": PROJECT,
                "connector": name,
                "enabled": true,
                "who": "both"
            }),
        ] {
            assert_eq!(
                refused(&root, action.clone()),
                "app_plugin_not_found",
                "{action}"
            );
        }
        assert_eq!(
            set_key(&root, name).unwrap_err().0,
            "app_plugin_not_found",
            "{name}"
        );
    }
    let listed = ask(&root, json!({"kind": "view", "project": null}));
    let names: Vec<&str> = listed
        .connectors
        .iter()
        .map(|connector| connector.name.as_str())
        .collect();
    assert_eq!(names, ["linear", "github", "atlassian"]);
    let all = store::list(&db, &root).unwrap();
    assert!(all.plugins.iter().any(|plugin| plugin.name == "near"));
    assert!(!root.join(crate::storage::accounts::PLUGIN_KEYS).exists());
}

#[test]
fn a_connectors_request_is_bounded_and_every_code_has_a_plain_sentence() {
    let (_tmp, root) = engine();
    let long = "a".repeat(NAME_BYTES_MAX + 1);
    for action in [
        json!({"kind": "review", "name": long}),
        json!({"kind": "review", "name": ""}),
        json!({"kind": "clear_key", "name": "board", "secret": ""}),
        json!({"kind": "view", "project": ""}),
        json!({"kind": "view", "project": "p".repeat(PROJECT_BYTES_MAX + 1)}),
        json!({"kind": "grant", "project": PROJECT, "connector": "", "enabled": false, "who": "both"}),
        json!({"kind": "grant", "project": PROJECT, "connector": "board", "enabled": true, "who": "everyone"}),
        json!({"kind": "set_key", "name": "board", "secret": "api_key"}),
        json!({"kind": "bind_account", "name": "board", "secret": "api_key", "account": "a"}),
        json!({"kind": "set_enabled", "name": "board", "enabled": true}),
    ] {
        assert_eq!(
            parse(&serde_json::to_vec(&request(action.clone())).unwrap())
                .unwrap_err()
                .0,
            "connector_request_invalid",
            "{action}"
        );
    }
    assert_eq!(
        parse(&vec![b' '; REQUEST_BYTES_MAX + 1]).unwrap_err().0,
        "connector_request_bounds"
    );
    for action in [
        json!({"kind": "view", "project": "ghost"}),
        json!({
            "kind": "grant",
            "project": "ghost",
            "connector": "linear",
            "enabled": true,
            "who": "both"
        }),
    ] {
        assert_eq!(refused(&root, action), "connector_project_unknown");
    }

    let mut sorted = CONNECTORS_CODES.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted, CONNECTORS_CODES);
    for code in [
        "connector_request_bounds",
        "connector_request_invalid",
        "connector_outcome_unknown",
        "connector_project_unknown",
        "connectors_unavailable",
        "app_plugin_not_found",
        "app_plugin_bundled",
        "app_plugin_changed",
        "app_plugin_refused",
        "app_plugin_needs_key",
        "app_plugin_needs_review",
        "app_plugin_connector_oauth_not_yet",
        "app_plugin_key_invalid",
        "app_plugin_key_store_unavailable",
        "app_plugin_secret_not_declared",
        "owner_grant_stale_or_revoked",
    ] {
        assert!(CONNECTORS_CODES.contains(&code), "{code}");
    }
    for code in CONNECTORS_CODES {
        if code.starts_with("app_plugin") || code.starts_with("connector") {
            assert!(crate::connectors::plain_problem(code).is_some(), "{code}");
        }
    }
    assert!(crate::app_plugins::plain_problem("app_plugin_not_connector").is_some());
}

#[test]
fn a_connector_add_never_replaces_an_embedding_plugin_of_that_name() {
    let (_tmp, root) = engine();
    let db = crate::questions::open(&root).unwrap();
    store::add(&db, &root, NEAR, &sha(NEAR), false, 1).unwrap();
    let taking = BOARD.replacen("name: board", "name: near", 1);
    match send(
        &root,
        request(json!({"kind": "preview", "yaml": taking})),
        None,
    )
    .unwrap()
    {
        Reply::Preview { preview } => match *preview {
            Preview::Refused { problem } => assert_eq!(
                (problem.code.as_str(), problem.field.as_deref()),
                ("app_plugin_name_taken", Some("name"))
            ),
            other => panic!("expected a refusal, got {other:?}"),
        },
        other => panic!("expected a preview, got {other:?}"),
    }
    for replace in [false, true] {
        assert_eq!(
            refused(
                &root,
                json!({"kind": "add", "yaml": taking, "sha256": sha(&taking), "replace": replace})
            ),
            "app_plugin_name_taken",
            "replace {replace}"
        );
    }
    assert_eq!(
        store::kind_of(&root, "near").unwrap(),
        Some(PluginKind::Embedding)
    );
    let kept = store::list(&db, &root).unwrap();
    let near = kept
        .plugins
        .iter()
        .find(|plugin| plugin.name == "near")
        .unwrap();
    assert_eq!(near.sha256, sha(NEAR));
}
