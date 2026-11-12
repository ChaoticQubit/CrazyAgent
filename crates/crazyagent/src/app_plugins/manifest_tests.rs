use super::*;
use crate::app_plugins::RunsView;

const HTTP: &str = "schema: 1
kind: embedding
name: acme
title: Acme embeddings
version: 2.1.0
description: Acme test plugin.
http:
  url: https://api.acme.example/v1/embed
  hosts: [api.acme.example]
  secrets: [api_key]
  headers:
    Authorization: \"Bearer {{secret.api_key}}\"
    X-Client: crazyagent
  body:
    model: acme-small
    input: \"{{inputs}}\"
    kind: \"{{purpose}}\"
    options:
      truncate: true
  vectors: data.*.embedding
  dimensions: 256
  batch: 16
";

fn refused(text: &str) -> (String, Option<String>) {
    let problem = parse(text).expect_err("the manifest is refused");
    (problem.code, problem.field)
}

fn with(old: &str, new: &str) -> String {
    assert!(HTTP.contains(old), "{old}");
    HTTP.replacen(old, new, 1)
}

#[test]
fn every_bundled_manifest_is_valid_and_named_as_its_entry() {
    let bundled = super::super::store::bundled();
    let names: Vec<&str> = bundled.iter().map(|(name, _)| *name).collect();
    assert_eq!(names, ["local", "openai", "voyage", "ollama"]);
    for (name, text) in bundled {
        let manifest = parse(text).unwrap_or_else(|problem| panic!("{name}: {problem:?}"));
        assert_eq!(manifest.name, *name);
        assert_eq!(manifest.kind, PluginKind::Embedding);
    }
    let local = parse(bundled[0].1).unwrap();
    assert_eq!(
        local.runs_view(),
        RunsView::Local {
            runtime: "static-retrieval-mrl-en-v1".into(),
            download_bytes: 125_018_208 + 711_396,
        }
    );
    let ollama = parse(bundled[3].1).unwrap();
    assert!(matches!(
        ollama.runs_view(),
        RunsView::Http { local_unencrypted: true, ref hosts, .. } if hosts == &["127.0.0.1".to_owned()]
    ));
    let voyage = parse(bundled[2].1).unwrap();
    assert!(matches!(
        voyage.runs_view(),
        RunsView::Http {
            local_unencrypted: false,
            dimensions: 256,
            ..
        }
    ));
}

#[test]
fn an_http_manifest_reads_hosts_secrets_headers_body_and_vectors() {
    let manifest = parse(HTTP).unwrap();
    assert_eq!(manifest.name, "acme");
    assert_eq!(manifest.title, "Acme embeddings");
    assert_eq!(manifest.version, "2.1.0");
    assert_eq!(
        manifest.sha256,
        crate::semantic::sha256_hex(HTTP.as_bytes())
    );
    let Runs::Http(http) = &manifest.runs else {
        panic!("an http plugin");
    };
    assert_eq!(http.url.as_str(), "https://api.acme.example/v1/embed");
    assert_eq!(http.hosts, ["api.acme.example"]);
    assert_eq!(http.secrets, ["api_key"]);
    assert_eq!(http.headers.len(), 2);
    assert_eq!(http.headers[0].name, "Authorization");
    assert_eq!(
        http.headers[0].value,
        [Piece::Text("Bearer ".into()), Piece::Secret(0)]
    );
    assert_eq!(http.headers[1].value, [Piece::Text("crazyagent".into())]);
    assert_eq!(
        http.vectors,
        [
            Step::Key("data".into()),
            Step::Each,
            Step::Key("embedding".into())
        ]
    );
    assert_eq!((http.dimensions, http.batch), (256, 16));
    assert!(!http.local_unencrypted);
    assert_eq!(
        http.body(&["one", "two"], Purpose::Query),
        serde_json::json!({
            "model": "acme-small",
            "input": ["one", "two"],
            "kind": "query",
            "options": {"truncate": true}
        })
    );
    assert_eq!(
        http.body(&["one"], Purpose::Document)["kind"],
        serde_json::json!("document")
    );
}

#[test]
fn the_trust_view_lists_what_the_plugin_touches() {
    let trust = parse(HTTP).unwrap().trust_view(true);
    assert_eq!(trust.name, "acme");
    assert_eq!(trust.secrets, ["api_key"]);
    assert!(trust.replaces);
    assert_eq!(
        trust.runs,
        RunsView::Http {
            url: "https://api.acme.example/v1/embed".into(),
            hosts: vec!["api.acme.example".into()],
            local_unencrypted: false,
            dimensions: 256,
        }
    );
}

#[test]
fn a_non_https_host_is_refused() {
    let plain = with(
        "url: https://api.acme.example/v1/embed",
        "url: http://api.acme.example/v1/embed",
    );
    assert_eq!(
        refused(&plain),
        ("app_plugin_url_not_https".into(), Some("http.url".into()))
    );
    let ftp = with(
        "url: https://api.acme.example/v1/embed",
        "url: ftp://api.acme.example/v1/embed",
    );
    assert_eq!(refused(&ftp).0, "app_plugin_url_not_https");
}

#[test]
fn plain_http_is_allowed_only_to_a_literal_loopback_host_without_secrets() {
    for (host, url_host) in [
        ("127.0.0.1", "127.0.0.1:11434"),
        ("localhost", "localhost:11434"),
        ("\"[::1]\"", "[::1]:11434"),
    ] {
        let text = format!(
            "schema: 1\nkind: embedding\nname: near\ntitle: Near\nversion: 1\nhttp:\n  url: http://{url_host}/api/embed\n  hosts: [{host}]\n  body:\n    input: \"{{{{inputs}}}}\"\n  vectors: embeddings.*\n  dimensions: 768\n"
        );
        let manifest = parse(&text).unwrap_or_else(|problem| panic!("{host}: {problem:?}"));
        let Runs::Http(http) = &manifest.runs else {
            panic!("http");
        };
        assert!(http.local_unencrypted, "{host}");
        assert_eq!(manifest.version, "1");
    }
    let keyed = "schema: 1\nkind: embedding\nname: near\ntitle: Near\nversion: 1\nhttp:\n  url: http://127.0.0.1:11434/api/embed\n  hosts: [127.0.0.1]\n  secrets: [token]\n  headers:\n    Authorization: \"Bearer {{secret.token}}\"\n  body:\n    input: \"{{inputs}}\"\n  vectors: embeddings.*\n  dimensions: 768\n";
    assert_eq!(refused(keyed).0, "app_plugin_secret_needs_https");
    let lan = "schema: 1\nkind: embedding\nname: near\ntitle: Near\nversion: 1\nhttp:\n  url: http://192.168.1.4:11434/api/embed\n  hosts: [192.168.1.4]\n  body:\n    input: \"{{inputs}}\"\n  vectors: embeddings.*\n  dimensions: 768\n";
    assert_eq!(refused(lan).0, "app_plugin_url_not_https");
}

#[test]
fn hosts_are_exact_declared_and_used() {
    let undeclared = with("hosts: [api.acme.example]", "hosts: [other.acme.example]");
    assert_eq!(refused(&undeclared).0, "app_plugin_host_undeclared");
    let unused = with(
        "hosts: [api.acme.example]",
        "hosts: [api.acme.example, cdn.acme.example]",
    );
    assert_eq!(
        refused(&unused),
        ("app_plugin_host_unused".into(), Some("http.hosts".into()))
    );
    for bad in [
        "\"*.acme.example\"",
        "API.acme.example",
        "api.acme.example:443",
        "-api.acme.example",
    ] {
        let text = with("hosts: [api.acme.example]", &format!("hosts: [{bad}]"));
        assert_eq!(refused(&text).0, "app_plugin_host_invalid", "{bad}");
    }
    let many = with(
        "hosts: [api.acme.example]",
        "hosts: [api.acme.example, a.example, b.example, c.example, d.example]",
    );
    assert_eq!(refused(&many).0, "app_plugin_hosts_limit");
    let userinfo = with(
        "url: https://api.acme.example/v1/embed",
        "url: https://user:pw@api.acme.example/v1/embed",
    );
    assert_eq!(refused(&userinfo).0, "app_plugin_url_invalid");
    let fragment = with(
        "url: https://api.acme.example/v1/embed",
        "url: https://api.acme.example/v1/embed#x",
    );
    assert_eq!(refused(&fragment).0, "app_plugin_url_invalid");
}

#[test]
fn secrets_are_declared_used_and_only_in_headers() {
    let undeclared = with("{{secret.api_key}}", "{{secret.other}}");
    assert_eq!(
        refused(&undeclared),
        (
            "app_plugin_secret_undeclared".into(),
            Some("http.headers.Authorization".into())
        )
    );
    let unused = with("secrets: [api_key]", "secrets: [api_key, spare]");
    assert_eq!(
        refused(&unused),
        (
            "app_plugin_secret_unused".into(),
            Some("http.secrets".into())
        )
    );
    let in_body = with("model: acme-small", "model: \"{{secret.api_key}}\"");
    assert_eq!(
        refused(&in_body),
        (
            "app_plugin_secret_outside_headers".into(),
            Some("http.body.model".into())
        )
    );
    let in_url = with(
        "url: https://api.acme.example/v1/embed",
        "url: \"https://api.acme.example/v1/embed?key={{secret.api_key}}\"",
    );
    assert_eq!(refused(&in_url).0, "app_plugin_secret_outside_headers");
    let twice = with("secrets: [api_key]", "secrets: [api_key, api_key]");
    assert_eq!(refused(&twice).0, "app_plugin_secret_duplicate");
    let bad = with("secrets: [api_key]", "secrets: [Api-Key]");
    assert_eq!(refused(&bad).0, "app_plugin_secret_invalid");
    let many = with("secrets: [api_key]", "secrets: [api_key, b, c, d, e]");
    assert_eq!(refused(&many).0, "app_plugin_secrets_limit");
}

#[test]
fn placeholders_are_known_and_inputs_appear_once() {
    let unknown = with("kind: \"{{purpose}}\"", "kind: \"{{whoami}}\"");
    assert_eq!(refused(&unknown).0, "app_plugin_placeholder_unknown");
    let header = with("X-Client: crazyagent", "X-Client: \"{{inputs}}\"");
    assert_eq!(refused(&header).0, "app_plugin_placeholder_unknown");
    let unclosed = with("Bearer {{secret.api_key}}", "Bearer {{secret.api_key");
    assert_eq!(refused(&unclosed).0, "app_plugin_placeholder_unknown");
    let missing = with("input: \"{{inputs}}\"", "input: fixed");
    assert_eq!(refused(&missing).0, "app_plugin_inputs_missing");
    let repeated = with("model: acme-small", "model: \"{{inputs}}\"");
    assert_eq!(refused(&repeated).0, "app_plugin_inputs_repeated");
    let listed = with("input: \"{{inputs}}\"", "input: [\"{{inputs}}\"]");
    assert_eq!(refused(&listed).0, "app_plugin_placeholder_unknown");
}

#[test]
fn headers_are_bounded_and_the_app_s_own_headers_are_reserved() {
    for reserved in [
        "Host",
        "content-length",
        "Content-Type",
        "Transfer-Encoding",
        "Connection",
    ] {
        let text = with("X-Client: crazyagent", &format!("{reserved}: x"));
        assert_eq!(refused(&text).0, "app_plugin_header_reserved", "{reserved}");
    }
    let bad_name = with("X-Client: crazyagent", "X_Client: crazyagent");
    assert_eq!(refused(&bad_name).0, "app_plugin_header_invalid");
    let twice = with("X-Client: crazyagent", "authorization: x");
    assert_eq!(refused(&twice).0, "app_plugin_header_invalid");
    let wide = with("X-Client: crazyagent", "X-Client: \"caf\\u00e9\"");
    assert_eq!(refused(&wide).0, "app_plugin_header_invalid");
}

#[test]
fn fields_are_known_present_and_typed() {
    let extra = with("batch: 16", "batch: 16\n  timeout: 5");
    assert_eq!(
        refused(&extra),
        (
            "app_plugin_field_unknown".into(),
            Some("http.timeout".into())
        )
    );
    let top = format!("{HTTP}color: red\n");
    assert_eq!(
        refused(&top),
        ("app_plugin_field_unknown".into(), Some("color".into()))
    );
    let no_title = with("title: Acme embeddings\n", "");
    assert_eq!(
        refused(&no_title),
        ("app_plugin_field_missing".into(), Some("title".into()))
    );
    let no_url = with("  url: https://api.acme.example/v1/embed\n", "");
    assert_eq!(
        refused(&no_url),
        ("app_plugin_field_missing".into(), Some("http.url".into()))
    );
    let typed = with("title: Acme embeddings", "title: [a]");
    assert_eq!(
        refused(&typed),
        ("app_plugin_field_invalid".into(), Some("title".into()))
    );
    assert_eq!(
        refused(&with("schema: 1", "schema: 2")).0,
        "app_plugin_schema_unknown"
    );
    assert_eq!(
        refused(&with("kind: embedding", "kind: chat")).0,
        "app_plugin_kind_unknown"
    );
    for name in [
        "Acme",
        "9acme",
        "acme_x",
        "a-very-long-name-that-goes-past-forty-bytes",
    ] {
        assert_eq!(
            refused(&with("name: acme", &format!("name: {name}"))).0,
            "app_plugin_name_invalid",
            "{name}"
        );
    }
    let both = format!("{HTTP}local:\n  runtime: static-retrieval-mrl-en-v1\n");
    assert_eq!(refused(&both).0, "app_plugin_runs_invalid");
    let neither = "schema: 1\nkind: embedding\nname: x\ntitle: X\nversion: 1\n";
    assert_eq!(refused(neither).0, "app_plugin_runs_invalid");
    let runtime =
        "schema: 1\nkind: embedding\nname: x\ntitle: X\nversion: 1\nlocal:\n  runtime: llama\n";
    assert_eq!(refused(runtime).0, "app_plugin_runtime_unknown");
}

#[test]
fn vectors_dimensions_and_batch_are_bounded() {
    for path in [
        "data.embedding",
        "data.*.*",
        "data..embedding",
        "data.*.emb edding",
    ] {
        let text = with("vectors: data.*.embedding", &format!("vectors: \"{path}\""));
        assert_eq!(refused(&text).0, "app_plugin_vectors_invalid", "{path}");
    }
    for dimensions in ["63", "1025", "256.5", "x"] {
        let text = with("dimensions: 256", &format!("dimensions: {dimensions}"));
        assert_eq!(
            refused(&text).0,
            "app_plugin_dimensions_invalid",
            "{dimensions}"
        );
    }
    for batch in ["0", "129"] {
        let text = with("batch: 16", &format!("batch: {batch}"));
        assert_eq!(refused(&text).0, "app_plugin_batch_invalid", "{batch}");
    }
    let default_batch = parse(&with("  batch: 16\n", "")).unwrap();
    let Runs::Http(http) = default_batch.runs else {
        panic!("http");
    };
    assert_eq!(http.batch, BATCH_DEFAULT);
}

#[test]
fn a_yaml_refusal_keeps_its_line() {
    let problem = parse("schema: 1\n\tkind: embedding\n").unwrap_err();
    assert_eq!(problem.code, "app_plugin_yaml_tab");
    assert_eq!(problem.line, Some(2));
}
