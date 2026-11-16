use super::*;
use crate::app_plugins::PluginStatus;
use crate::connectors::approvals;
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
    assert_eq!(
        names,
        [
            "linear",
            "github",
            "atlassian",
            "notion",
            "slack",
            "asana",
            "box",
            "canva",
            "dropbox",
            "gmail",
            "google-drive",
            "google-calendar"
        ]
    );
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
    assert_eq!(
        names,
        [
            "linear",
            "github",
            "atlassian",
            "notion",
            "slack",
            "asana",
            "box",
            "canva",
            "dropbox",
            "gmail",
            "google-drive",
            "google-calendar"
        ]
    );
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

fn logged(connector: &str, started_ms: i64) -> crate::connectors::audit::CallRow {
    crate::connectors::audit::CallRow {
        launch: "l1".into(),
        connector: connector.into(),
        tool: "list_issues".into(),
        kind: crate::connectors::audit::CallKind::Read,
        sent: true,
        project: PROJECT.into(),
        agent: "Agent 1".into(),
        started_ms,
        ms: 340,
        bytes_in: 120,
        bytes_out: 2048,
        outcome: "ok".into(),
    }
}

fn activity(root: &Path, action: serde_json::Value) -> crate::connectors::audit::ActivityView {
    match send(root, request(action), None).unwrap() {
        Reply::Activity { view } => *view,
        other => panic!("expected the activity page, got {other:?}"),
    }
}

#[test]
fn the_activity_page_reads_with_its_filters_and_a_keep_save_drops_older_calls_at_once() {
    const DAY: i64 = 24 * 60 * 60 * 1000;
    let (_tmp, root) = engine();
    let db = crate::questions::open(&root).unwrap();
    let now = crate::semantic::now_ms();
    for (connector, days) in [("board", 3), ("board", 10), ("desk", 20)] {
        crate::connectors::audit::record(
            &db,
            &logged(connector, now - days * DAY),
            KeepLog::Days90,
        )
        .unwrap();
    }

    let all = activity(
        &root,
        json!({"kind": "activity", "connector": null, "project": null}),
    );
    assert_eq!(all.calls.len(), 3);
    assert_eq!(all.connectors, ["board", "desk"]);
    assert_eq!(all.projects.len(), 1);
    assert_eq!(
        (
            all.projects[0].id.as_str(),
            all.settings.keep_log,
            all.version
        ),
        (PROJECT, KeepLog::Days30, 0)
    );
    let board = activity(
        &root,
        json!({"kind": "activity", "connector": "board", "project": null}),
    );
    assert_eq!(board.calls.len(), 2);
    assert_eq!(board.connector.as_deref(), Some("board"));
    let alpha = activity(
        &root,
        json!({"kind": "activity", "connector": null, "project": PROJECT}),
    );
    assert_eq!(alpha.calls.len(), 3);
    let ghost = activity(
        &root,
        json!({"kind": "activity", "connector": null, "project": "ghost"}),
    );
    assert!(
        ghost.calls.is_empty(),
        "a project that is gone filters to nothing, never a refusal"
    );

    let saved = ask(
        &root,
        json!({"kind": "set", "settings": {
            "write": "ask_every_time", "wait": "minutes10", "native": "off",
            "size_cap": "kib256", "keep_log": "days7"
        }, "version": 0}),
    );
    assert_eq!(
        (saved.settings.keep_log, saved.version),
        (KeepLog::Days7, 1)
    );
    let left: i64 = db
        .query_row("SELECT COUNT(*) FROM connector_calls", [], |row| row.get(0))
        .unwrap();
    assert_eq!(
        left, 1,
        "the save drops the calls the new keep time no longer holds"
    );
    let after = activity(
        &root,
        json!({"kind": "activity", "connector": null, "project": null}),
    );
    assert_eq!(after.calls.len(), 1);
    assert_eq!(
        (after.settings.keep_log, after.version),
        (KeepLog::Days7, 1)
    );

    for action in [
        json!({"kind": "activity", "connector": "", "project": null}),
        json!({"kind": "activity", "connector": "a".repeat(NAME_BYTES_MAX + 1), "project": null}),
        json!({"kind": "activity", "connector": null, "project": ""}),
        json!({"kind": "activity", "connector": null, "project": "p".repeat(PROJECT_BYTES_MAX + 1)}),
        json!({"kind": "activity", "connector": null, "project": null, "arguments": true}),
    ] {
        assert_eq!(
            refused(&root, action.clone()),
            "connector_request_invalid",
            "{action}"
        );
    }
}

/// One reply of this protocol as an owner client reads it, for a request that succeeds.
fn answered(root: &Path, action: serde_json::Value) -> serde_json::Value {
    serde_json::to_value(send(root, request(action), None).unwrap()).unwrap()
}

/// DEC-168 Task 12: the owner reads a waiting write's approval, answers it once through
/// the protocol, reads the project's standing allows and takes one back; a second answer
/// and an id that names no approval are refused with their own codes.
#[test]
fn the_owner_answers_a_write_and_takes_back_a_standing_allow_through_the_protocol() {
    let (_tmp, root) = engine();
    let once = ConnectorSettings {
        write: WritePolicy::AskOncePerProject,
        ..ConnectorSettings::DEFAULT
    };
    ask(
        &root,
        json!({"kind": "set", "settings": once, "version": 0}),
    );
    let pending = approvals::PendingWrite {
        launch: "0123456789abcdef0123456789abcdef".into(),
        project: PROJECT.into(),
        agent: "Agent 1".into(),
        connector: "board".into(),
        title: "Board".into(),
        tool: "create_issue".into(),
        preview: "create_issue: title=\"Fix login\"".into(),
        arguments: "{\"title\":\"Fix login\\\\u{200B}\"}".into(),
        hidden: true,
    };
    let db = crate::questions::open(&root).unwrap();
    let now = crate::semantic::now_ms();
    let id = approvals::ask(&db, &pending, Wait::Minutes10, now).unwrap();

    let read = answered(&root, json!({"kind": "approval", "id": id}));
    assert_eq!(read["kind"], "approval", "{read}");
    assert_eq!(read["approval"]["id"], id);
    assert_eq!(read["approval"]["state"], "waiting");
    assert_eq!(read["approval"]["preview"], pending.preview.as_str());
    assert_eq!(read["approval"]["arguments"], pending.arguments.as_str());
    assert_eq!(read["approval"]["hidden"], true, "{read}");
    let allow = json!({"kind": "answer", "id": id, "allow": true});
    let done = answered(&root, allow.clone());
    assert_eq!(done["approval"]["state"], "allowed", "{done}");
    assert_eq!(refused(&root, allow), "connector_approval_closed");
    let ghost = json!({"kind": "answer", "id": id + 100, "allow": false});
    assert_eq!(refused(&root, ghost), "connector_approval_unknown");
    let ghost = json!({"kind": "approval", "id": id + 100});
    assert_eq!(refused(&root, ghost), "connector_approval_unknown");

    let writes = answered(&root, json!({"kind": "writes", "project": PROJECT}));
    assert_eq!(writes["kind"], "writes", "{writes}");
    assert_eq!(writes["writes"]["connectors"], json!(["board"]));
    let revoke = json!({"kind": "revoke_writes", "project": PROJECT, "connector": "board"});
    let left = answered(&root, revoke);
    assert_eq!(left["writes"]["connectors"], json!([]), "{left}");
    let ghost = json!({"kind": "writes", "project": "ghost"});
    assert_eq!(refused(&root, ghost), "connector_project_unknown");
    for action in [
        json!({"kind": "approval", "id": 0}),
        json!({"kind": "answer", "id": id}),
        json!({"kind": "writes", "project": ""}),
        json!({"kind": "revoke_writes", "project": PROJECT, "connector": ""}),
    ] {
        let bytes = serde_json::to_vec(&request(action.clone())).unwrap();
        let code = parse(&bytes).unwrap_err().0;
        assert_eq!(code, "connector_request_invalid", "{action}");
    }
    for code in ["connector_approval_closed", "connector_approval_unknown"] {
        assert!(CONNECTORS_CODES.contains(&code), "{code}");
    }
}
