use super::*;
use crate::app_plugins::{Binding, Origin, PluginStatus};
use serde_json::json;

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

/// An engine root whose database holds the authority schemas. Output: the temporary
/// folder and the engine root.
fn engine() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::Builder::new()
        .prefix("ca-owner-app-plugins-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    let root = tmp.path().canonicalize().unwrap().join("engine");
    std::fs::create_dir(&root).unwrap();
    let db = crate::questions::open(&root).unwrap();
    crate::storage::initialize_authority_schemas(&db).unwrap();
    (tmp, root)
}

/// Sends one request through this protocol's selector, parsing and handler, as an owner
/// client's frame body plus the key frame a `set_key` announces. Inputs: the engine root,
/// the body and the key. Output: the reply, or the refusal.
fn send(root: &Path, body: serde_json::Value, key: Option<&[u8]>) -> Result<Reply, EngineError> {
    let bytes = serde_json::to_vec(&body).unwrap();
    assert!(matches!(protocol(&bytes).unwrap(), Protocol::AppPlugins));
    let (action, key_present) = parse(&bytes)?;
    assert_eq!(key_present, key.is_some());
    run(
        root,
        action,
        key.map(|key| AccountSecret::new(key.to_vec()).unwrap()),
    )
}

fn request(action: serde_json::Value) -> serde_json::Value {
    json!({"app_plugins": action, "secret_present": false})
}

fn listing(reply: Reply) -> Listing {
    match reply {
        Reply::Plugins { listing } => *listing,
        other => panic!("expected plugins, got {other:?}"),
    }
}

fn search(reply: Reply) -> SearchView {
    match reply {
        Reply::Search { search } => *search,
        other => panic!("expected search, got {other:?}"),
    }
}

#[test]
fn the_owner_previews_adds_and_removes_a_plugin_through_the_protocol() {
    let (_tmp, root) = engine();
    let first = listing(send(&root, request(json!({"kind": "list"})), None).unwrap());
    let names: Vec<&str> = first.plugins.iter().map(|plugin| plugin.name.as_str()).collect();
    assert_eq!(names, ["local", "openai", "voyage", "ollama"]);
    let sha256 = match send(&root, request(json!({"kind": "preview", "yaml": NEAR})), None).unwrap() {
        Reply::Preview { preview } => match *preview {
            Preview::Ready { trust } => trust.sha256,
            other => panic!("expected a trust screen, got {other:?}"),
        },
        other => panic!("expected a preview, got {other:?}"),
    };
    let added = listing(
        send(
            &root,
            request(json!({"kind": "add", "yaml": NEAR, "sha256": sha256, "replace": false})),
            None,
        )
        .unwrap(),
    );
    let near = added.plugins.iter().find(|plugin| plugin.name == "near").unwrap();
    assert_eq!(
        (near.origin, &near.status, near.enabled),
        (Origin::User, &PluginStatus::Ready, true)
    );
    let removed = listing(send(&root, request(json!({"kind": "remove", "name": "near"})), None).unwrap());
    assert!(removed.plugins.iter().all(|plugin| plugin.name != "near"));
    assert_eq!(
        send(&root, request(json!({"kind": "remove", "name": "openai"})), None)
            .unwrap_err()
            .0,
        "app_plugin_bundled"
    );
}

#[test]
fn a_plugin_key_travels_only_in_its_own_frame_and_never_comes_back() {
    let (_tmp, root) = engine();
    let key = b"pa-secret-key-000001";
    let body = json!({
        "app_plugins": {"kind": "set_key", "name": "voyage", "secret": "api_key"},
        "secret_present": true,
    });
    let reply = send(&root, body.clone(), Some(key)).unwrap();
    let text = serde_json::to_string(&reply).unwrap();
    assert!(!text.contains("pa-secret-key"));
    let voyage = listing(reply)
        .plugins
        .into_iter()
        .find(|plugin| plugin.name == "voyage")
        .unwrap();
    assert_eq!(voyage.secrets[0].binding, Binding::Key);
    let unannounced = json!({
        "app_plugins": {"kind": "set_key", "name": "voyage", "secret": "api_key"},
        "secret_present": false,
    });
    assert_eq!(
        parse(&serde_json::to_vec(&unannounced).unwrap()).unwrap_err().0,
        "app_plugin_request_invalid"
    );
    let stray = json!({"app_plugins": {"kind": "list"}, "secret_present": true});
    assert_eq!(
        parse(&serde_json::to_vec(&stray).unwrap()).unwrap_err().0,
        "app_plugin_request_invalid"
    );
    let unbound = listing(
        send(
            &root,
            request(json!({"kind": "unbind", "name": "voyage", "secret": "api_key"})),
            None,
        )
        .unwrap(),
    );
    let voyage = unbound.plugins.iter().find(|plugin| plugin.name == "voyage").unwrap();
    assert_eq!(voyage.status, PluginStatus::NeedsKey);
}

#[test]
fn requests_are_bounded_and_strict() {
    let invalid = |body: serde_json::Value| parse(&serde_json::to_vec(&body).unwrap()).unwrap_err().0;
    assert_eq!(
        invalid(json!({"app_plugins": {"kind": "list"}})),
        "app_plugin_request_invalid"
    );
    assert_eq!(
        invalid(request(json!({"kind": "list", "extra": 1}))),
        "app_plugin_request_invalid"
    );
    assert_eq!(
        invalid(request(json!({"kind": "run_plugin"}))),
        "app_plugin_request_invalid"
    );
    assert_eq!(
        invalid(request(json!({"kind": "remove", "name": ""}))),
        "app_plugin_request_invalid"
    );
    assert_eq!(
        invalid(request(json!({"kind": "remove", "name": "n".repeat(NAME_BYTES_MAX + 1)}))),
        "app_plugin_request_invalid"
    );
    assert_eq!(
        invalid(request(json!({
            "kind": "bind_account",
            "name": "openai",
            "secret": "api_key",
            "account": "a".repeat(NAME_BYTES_MAX + 1),
        }))),
        "app_plugin_request_invalid"
    );
    let padded = json!({
        "app_plugins": {"kind": "list"},
        "secret_present": false,
        "pad": "x".repeat(REQUEST_BYTES_MAX),
    });
    assert_eq!(invalid(padded), "app_plugin_request_bounds");
}

#[test]
fn the_search_choice_saves_by_version_and_an_http_plugin_needs_its_host_confirmed() {
    let (_tmp, root) = engine();
    let first = search(send(&root, request(json!({"kind": "search"})), None).unwrap());
    assert_eq!(first.version, 0);
    let save = |choice: serde_json::Value, host: Option<&str>, version: u64| {
        send(
            &root,
            request(json!({
                "kind": "set_search",
                "choice": choice,
                "confirm_host": host,
                "version": version,
            })),
            None,
        )
    };
    let saved = search(save(json!({"kind": "full_text"}), None, 0).unwrap());
    assert_eq!(saved.version, 1);
    assert!(matches!(
        save(json!({"kind": "full_text"}), None, 0).unwrap(),
        Reply::Stale { current: 1 }
    ));
    send(
        &root,
        request(json!({"kind": "set_enabled", "name": "ollama", "enabled": true})),
        None,
    )
    .unwrap();
    let ollama = json!({"kind": "plugin", "name": "ollama"});
    assert_eq!(
        save(ollama.clone(), None, 1).unwrap_err().0,
        "search_confirm_required"
    );
    let chosen = search(save(ollama, Some("127.0.0.1"), 1).unwrap());
    assert_eq!(
        chosen.choice,
        SearchChoice::Plugin {
            name: "ollama".into()
        }
    );
}

#[test]
fn every_app_plugins_code_is_listed_for_the_clients_with_a_plain_sentence() {
    for code in [
        "app_plugin_request_bounds",
        "app_plugin_request_invalid",
        "app_plugin_outcome_unknown",
        "app_plugins_unavailable",
        "app_plugin_not_found",
        "app_plugin_bundled",
        "app_plugin_changed",
        "app_plugin_needs_review",
        "app_plugin_key_invalid",
        "app_plugin_key_store_unavailable",
        "search_confirm_required",
        "search_choice_unavailable",
        "owner_grant_stale_or_revoked",
    ] {
        assert!(APP_PLUGINS_CODES.contains(&code), "{code}");
    }
    let mut sorted = APP_PLUGINS_CODES.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted, APP_PLUGINS_CODES);
    for code in APP_PLUGINS_CODES {
        if code.starts_with("app_plugin") || code.starts_with("search_") {
            assert!(crate::app_plugins::plain_problem(code).is_some(), "{code}");
        }
    }
}
