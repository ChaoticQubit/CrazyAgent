use crate::problem::Problem;
use crate::views::{
    Binding, Listing, Origin, PluginKind, PluginStatus, PluginView, Preview, RunsView, SecretView,
    TrustView,
};

/// One HTTP plugin's runs view. Output: the view.
fn http() -> RunsView {
    RunsView::Http {
        url: "https://api.openai.com/v1/embeddings".into(),
        hosts: vec!["api.openai.com".into()],
        local_unencrypted: false,
        dimensions: 256,
    }
}

#[test]
fn the_kind_and_the_views_serialize_as_the_bridge_reads_them() {
    assert_eq!(
        serde_json::to_value(PluginKind::Embedding).unwrap(),
        serde_json::json!("embedding")
    );
    assert!(serde_json::from_value::<PluginKind>(serde_json::json!("connector")).is_err());
    let runs = RunsView::Local {
        runtime: "static-retrieval-mrl-en-v1".into(),
        download_bytes: 7,
    };
    assert_eq!(
        serde_json::to_value(&runs).unwrap(),
        serde_json::json!({"kind": "local", "runtime": "static-retrieval-mrl-en-v1", "download_bytes": 7})
    );
}

#[test]
fn a_listing_keeps_the_shape_the_bridge_reads() {
    let listing = Listing {
        plugins: vec![PluginView {
            name: "openai".into(),
            title: "OpenAI embeddings".into(),
            version: "1.0.0".into(),
            description: "d".into(),
            kind: PluginKind::Embedding,
            origin: Origin::Bundled,
            status: PluginStatus::NeedsKey,
            enabled: false,
            runs: Some(http()),
            secrets: vec![SecretView {
                name: "api_key".into(),
                binding: Binding::Account {
                    account: "acct".into(),
                    name: "Work".into(),
                },
            }],
            sha256: "ab".into(),
        }],
        more: false,
    };
    let shown = serde_json::to_value(&listing).unwrap();
    assert_eq!(
        shown,
        serde_json::json!({
            "plugins": [{
                "name": "openai",
                "title": "OpenAI embeddings",
                "version": "1.0.0",
                "description": "d",
                "kind": "embedding",
                "origin": "bundled",
                "status": {"kind": "needs_key"},
                "enabled": false,
                "runs": {"kind": "http", "url": "https://api.openai.com/v1/embeddings", "hosts": ["api.openai.com"], "local_unencrypted": false, "dimensions": 256},
                "secrets": [{"name": "api_key", "binding": {"kind": "account", "account": "acct", "name": "Work"}}],
                "sha256": "ab",
            }],
            "more": false,
        })
    );
    assert_eq!(serde_json::from_value::<Listing>(shown).unwrap(), listing);
}

#[test]
fn a_preview_keeps_the_shape_the_bridge_reads() {
    let ready = Preview::Ready {
        trust: TrustView {
            name: "openai".into(),
            title: "t".into(),
            version: "1".into(),
            description: String::new(),
            kind: PluginKind::Embedding,
            runs: http(),
            secrets: vec!["api_key".into()],
            sha256: "ab".into(),
            replaces: true,
        },
    };
    let shown = serde_json::to_value(&ready).unwrap();
    assert_eq!(shown["kind"], "ready");
    assert_eq!(shown["trust"]["replaces"], true);
    assert_eq!(serde_json::from_value::<Preview>(shown).unwrap(), ready);
    let refused = Preview::Refused {
        problem: Problem::new("app_plugin_field_missing", None, Some("http.url")),
    };
    let shown = serde_json::to_value(&refused).unwrap();
    assert_eq!(shown["kind"], "refused");
    assert_eq!(shown["problem"]["field"], "http.url");
    assert!(
        serde_json::from_value::<Listing>(
            serde_json::json!({"plugins": [], "more": false, "extra": 1})
        )
        .is_err()
    );
}
