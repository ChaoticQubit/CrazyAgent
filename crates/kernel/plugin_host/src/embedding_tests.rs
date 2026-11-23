use crate::embedding::{
    BATCH_DEFAULT, Manifest, Piece, Purpose, SCHEMA_VERSION, Step, declared_kind, parse, valid_name,
};
use crate::problem::Problem;
use crate::views::{PluginKind, RunsView};

const HTTP: &str = r#"plugin = "acme"
tier = "t2"
kind = "embedding"
version = 1
permissions = []
title = "Acme embeddings"
release = "2.1.0"
description = "Acme test plugin."

[http]
url = "https://api.acme.example/v1/embed"
hosts = ["api.acme.example"]
secrets = ["api_key"]
vectors = "data.*.embedding"
dimensions = 256
batch = 16

[http.headers]
Authorization = "Bearer {{secret.api_key}}"
X-Client = "crazyagent"

[http.body]
model = "acme-small"
input = "{{inputs}}"
kind = "{{purpose}}"

[http.body.options]
truncate = true
"#;

/// The bundled plugin.toml texts, in screen order, each with its plugin id.
const BUNDLED: [(&str, &str); 3] = [
    ("openai", include_str!("../bundled/openai/plugin.toml")),
    ("voyage", include_str!("../bundled/voyage/plugin.toml")),
    ("ollama", include_str!("../bundled/ollama/plugin.toml")),
];

/// The code and field a manifest is refused with. Input: the text. Output: both.
fn refused(text: &str) -> (String, Option<String>) {
    let problem = parse(text).expect_err("the manifest is refused");
    (problem.code, problem.field)
}

/// The code and line a manifest is refused with. Input: the text. Output: both.
fn refused_at(text: &str) -> (String, Option<u32>) {
    let problem = parse(text).expect_err("the manifest is refused");
    (problem.code, problem.line)
}

/// The fixture with its first `old` replaced by `new`. Inputs: both texts. Output: the text.
fn with(old: &str, new: &str) -> String {
    assert!(HTTP.contains(old), "{old}");
    HTTP.replacen(old, new, 1)
}

/// The runs view of a manifest that runs over http. Input: the manifest. Output: hosts,
/// whether it is plain http to this computer, and the vector size.
fn http_view(manifest: &Manifest) -> (Vec<String>, bool, u16) {
    let RunsView::Http {
        hosts,
        local_unencrypted,
        dimensions,
        ..
    } = manifest.runs_view()
    else {
        panic!("{} runs over http", manifest.name);
    };
    (hosts, local_unencrypted, dimensions)
}

#[test]
fn the_three_bundled_manifests_read_as_plugin_toml() {
    let read: Vec<Manifest> = BUNDLED
        .iter()
        .map(|(name, text)| {
            assert!(!text.contains("Mac"), "{name} names one platform");
            let manifest = parse(text).unwrap_or_else(|problem| panic!("{name}: {problem:?}"));
            assert_eq!(manifest.name, *name);
            assert_eq!(manifest.kind, PluginKind::Embedding, "{name}");
            assert_eq!(manifest.version, "1.0.0", "{name}");
            let base = crate::manifest::parse(text).expect("the five keys read");
            assert_eq!(
                (base.tier.as_str(), base.version, base.permissions.len()),
                ("t2", 1, 0),
                "{name}"
            );
            manifest
        })
        .collect();
    assert_eq!(read[0].title, "OpenAI embeddings");
    assert_eq!(read[1].title, "Voyage AI embeddings");
    assert_eq!(read[2].title, "Ollama on this computer");
    assert_eq!(
        http_view(&read[0]),
        (vec!["api.openai.com".to_owned()], false, 256)
    );
    assert_eq!(
        http_view(&read[1]),
        (vec!["api.voyageai.com".to_owned()], false, 256)
    );
    assert_eq!(
        http_view(&read[2]),
        (vec!["127.0.0.1".to_owned()], true, 768)
    );
    assert_eq!(read[0].secrets(), ["api_key"]);
    assert_eq!(read[1].secrets(), ["api_key"]);
    assert!(read[2].secrets().is_empty());
    let urls: Vec<&str> = read
        .iter()
        .map(|manifest| manifest.http.url.as_str())
        .collect();
    assert_eq!(
        urls,
        [
            "https://api.openai.com/v1/embeddings",
            "https://api.voyageai.com/v1/embeddings",
            "http://127.0.0.1:11434/api/embed",
        ]
    );
    let batches: Vec<u16> = read.iter().map(|manifest| manifest.http.batch).collect();
    assert_eq!(batches, [64, 64, 32]);
    let vectors: Vec<&[Step]> = read
        .iter()
        .map(|manifest| manifest.http.vectors.as_slice())
        .collect();
    assert_eq!(
        vectors[0],
        [
            Step::Key("data".into()),
            Step::Each,
            Step::Key("embedding".into())
        ]
    );
    assert_eq!(vectors[1], vectors[0]);
    assert_eq!(vectors[2], [Step::Key("embeddings".into()), Step::Each]);
    assert_eq!(
        read[0].http.headers[0].value,
        [Piece::Text("Bearer ".into()), Piece::Secret(0)]
    );
    assert_eq!(read[1].http.headers, read[0].http.headers);
    assert!(read[2].http.headers.is_empty());
    assert_eq!(
        read[0].http.body(&["a"], Purpose::Query),
        serde_json::json!({"model": "text-embedding-3-small", "input": ["a"], "dimensions": 256, "encoding_format": "float"})
    );
    assert_eq!(
        read[1].http.body(&["a"], Purpose::Document),
        serde_json::json!({"model": "voyage-3.5-lite", "input": ["a"], "input_type": "document", "output_dimension": 256})
    );
    assert_eq!(
        read[2].http.body(&["a"], Purpose::Query),
        serde_json::json!({"model": "nomic-embed-text", "input": ["a"]})
    );
}

#[test]
fn only_manifest_version_1_is_read_and_a_later_one_is_refused_before_its_keys() {
    assert_eq!(SCHEMA_VERSION, 1);
    for version in ["0", "2", "99", "-1"] {
        let text = with("version = 1", &format!("version = {version}"));
        assert_eq!(
            refused(&text),
            ("app_plugin_schema_unknown".into(), Some("version".into())),
            "{version}"
        );
    }
    let later = format!("color = \"red\"\n{}", with("version = 1", "version = 2"));
    assert_eq!(
        refused(&later),
        ("app_plugin_schema_unknown".into(), Some("version".into())),
        "a later version's new keys never hide that the version itself is unknown"
    );
    let later_kind = with(
        "kind = \"embedding\"\nversion = 1",
        "kind = \"tide-gauge\"\nversion = 3",
    );
    assert_eq!(refused(&later_kind).0, "app_plugin_schema_unknown");
    for typed in ["\"1\"", "1.0", "true", "[1]"] {
        let text = with("version = 1", &format!("version = {typed}"));
        assert_eq!(
            refused(&text),
            ("app_plugin_field_invalid".into(), Some("version".into())),
            "{typed}"
        );
    }
    let missing = with("version = 1\n", "");
    assert_eq!(
        refused(&missing),
        ("app_plugin_field_missing".into(), Some("version".into()))
    );
    let unknown = Problem::new("app_plugin_schema_unknown", None, Some("version"));
    assert_eq!(
        unknown.message,
        "version: this app reads version = 1 plugin files only."
    );
}

#[test]
fn a_kind_this_app_does_not_read_is_refused_before_its_keys() {
    let connector = "plugin = \"tide\"\ntier = \"t2\"\nkind = \"connector\"\nversion = 1\npermissions = []\ntitle = \"Tide\"\n\n[mcp]\nurl = \"https://mcp.tide.example/mcp\"\n";
    assert_eq!(
        refused(connector),
        ("app_plugin_kind_unknown".into(), Some("kind".into()))
    );
    assert_eq!(
        refused(&with("kind = \"embedding\"", "kind = [\"embedding\"]")),
        ("app_plugin_field_invalid".into(), Some("kind".into()))
    );
    assert_eq!(declared_kind(HTTP), Some(PluginKind::Embedding));
    assert_eq!(declared_kind(connector), None);
    let broken = format!("{HTTP}color = \n");
    assert_eq!(declared_kind(&broken), Some(PluginKind::Embedding));
}

#[test]
fn an_http_manifest_reads_hosts_secrets_headers_body_and_vectors() {
    let manifest = parse(HTTP).unwrap();
    assert_eq!(manifest.name, "acme");
    assert_eq!(manifest.title, "Acme embeddings");
    assert_eq!(manifest.version, "2.1.0");
    assert_eq!(manifest.description, "Acme test plugin.");
    assert_eq!(manifest.sha256, kernel_common::hash(HTTP.as_bytes()));
    let changed = parse(&with("release = \"2.1.0\"", "release = \"2.1.1\"")).unwrap();
    assert_ne!(changed.sha256, manifest.sha256);
    let http = &manifest.http;
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
    let bare = with(
        "release = \"2.1.0\"\ndescription = \"Acme test plugin.\"\n",
        "",
    );
    let bare = parse(&bare).expect("release and description are optional");
    assert_eq!((bare.version.as_str(), bare.description.as_str()), ("", ""));
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
        "https://api.acme.example/v1/embed",
        "http://api.acme.example/v1/embed",
    );
    assert_eq!(
        refused(&plain),
        ("app_plugin_url_not_https".into(), Some("http.url".into()))
    );
    let ftp = with(
        "https://api.acme.example/v1/embed",
        "ftp://api.acme.example/v1/embed",
    );
    assert_eq!(refused(&ftp).0, "app_plugin_url_not_https");
}

/// A loopback plugin text. Inputs: the URL host with port, the declared host and any
/// extra `[http]` lines. Output: the text.
fn near(url_host: &str, host: &str, extra: &str) -> String {
    format!(
        "plugin = \"near\"\ntier = \"t2\"\nkind = \"embedding\"\nversion = 1\npermissions = []\ntitle = \"Near\"\n\n[http]\nurl = \"http://{url_host}/api/embed\"\nhosts = [\"{host}\"]\nvectors = \"embeddings.*\"\ndimensions = 768\n{extra}\n[http.body]\ninput = \"{{{{inputs}}}}\"\n"
    )
}

#[test]
fn plain_http_is_allowed_only_to_a_literal_loopback_host_without_secrets() {
    for (host, url_host) in [
        ("127.0.0.1", "127.0.0.1:11434"),
        ("localhost", "localhost:11434"),
        ("[::1]", "[::1]:11434"),
    ] {
        let manifest = parse(&near(url_host, host, ""))
            .unwrap_or_else(|problem| panic!("{host}: {problem:?}"));
        assert!(manifest.http.local_unencrypted, "{host}");
    }
    let keyed = near(
        "127.0.0.1:11434",
        "127.0.0.1",
        "secrets = [\"token\"]\n\n[http.headers]\nAuthorization = \"Bearer {{secret.token}}\"\n",
    );
    assert_eq!(refused(&keyed).0, "app_plugin_secret_needs_https");
    let lan = near("192.168.1.4:11434", "192.168.1.4", "");
    assert_eq!(refused(&lan).0, "app_plugin_url_not_https");
}

#[test]
fn hosts_are_exact_declared_and_used() {
    let hosts = "hosts = [\"api.acme.example\"]";
    let undeclared = with(hosts, "hosts = [\"other.acme.example\"]");
    assert_eq!(refused(&undeclared).0, "app_plugin_host_undeclared");
    let unused = with(
        hosts,
        "hosts = [\"api.acme.example\", \"cdn.acme.example\"]",
    );
    assert_eq!(
        refused(&unused),
        ("app_plugin_host_unused".into(), Some("http.hosts".into()))
    );
    for bad in [
        "*.acme.example",
        "API.acme.example",
        "api.acme.example:443",
        "-api.acme.example",
    ] {
        let text = with(hosts, &format!("hosts = [\"{bad}\"]"));
        assert_eq!(refused(&text).0, "app_plugin_host_invalid", "{bad}");
    }
    let many = with(
        hosts,
        "hosts = [\"api.acme.example\", \"a.example\", \"b.example\", \"c.example\", \"d.example\"]",
    );
    assert_eq!(refused(&many).0, "app_plugin_hosts_limit");
    let userinfo = with(
        "https://api.acme.example",
        "https://user:pw@api.acme.example",
    );
    assert_eq!(refused(&userinfo).0, "app_plugin_url_invalid");
    let fragment = with("v1/embed\"", "v1/embed#x\"");
    assert_eq!(refused(&fragment).0, "app_plugin_url_invalid");
}

/// V250-H4 sec N4: a plugin URL whose host is a cloud metadata address, in any spelling the
/// URL reader turns into one, is refused before any call can dial it, while a private
/// address that is not one still reads.
#[test]
fn a_metadata_address_is_refused_as_a_plugin_host() {
    let at = |url_host: &str, declared: &str| {
        with(
            "https://api.acme.example/v1/embed",
            &format!("https://{url_host}/v1/embed"),
        )
        .replacen(
            "hosts = [\"api.acme.example\"]",
            &format!("hosts = [\"{declared}\"]"),
            1,
        )
    };
    for (url_host, declared) in [
        ("169.254.169.254", "169.254.169.254"),
        ("0xa9fea9fe", "169.254.169.254"),
        ("100.100.100.200", "100.100.100.200"),
        ("168.63.129.16", "168.63.129.16"),
        ("[fd00:ec2::254]", "[fd00:ec2::254]"),
        ("[::ffff:169.254.169.254]", "[::ffff:a9fe:a9fe]"),
    ] {
        assert_eq!(
            refused(&at(url_host, declared)),
            ("app_plugin_host_metadata".into(), Some("http.url".into())),
            "{url_host}"
        );
    }
    let private = parse(&at("10.0.0.5", "10.0.0.5")).expect("a private address reads");
    assert_eq!(private.http.url.as_str(), "https://10.0.0.5/v1/embed");
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
    let secrets = "secrets = [\"api_key\"]";
    let unused = with(secrets, "secrets = [\"api_key\", \"spare\"]");
    assert_eq!(
        refused(&unused),
        (
            "app_plugin_secret_unused".into(),
            Some("http.secrets".into())
        )
    );
    let in_body = with("model = \"acme-small\"", "model = \"{{secret.api_key}}\"");
    assert_eq!(
        refused(&in_body),
        (
            "app_plugin_secret_outside_headers".into(),
            Some("http.body.model".into())
        )
    );
    let in_url = with("v1/embed\"", "v1/embed?key={{secret.api_key}}\"");
    assert_eq!(refused(&in_url).0, "app_plugin_secret_outside_headers");
    let twice = with(secrets, "secrets = [\"api_key\", \"api_key\"]");
    assert_eq!(refused(&twice).0, "app_plugin_secret_duplicate");
    let bad = with(secrets, "secrets = [\"Api-Key\"]");
    assert_eq!(refused(&bad).0, "app_plugin_secret_invalid");
    let many = with(
        secrets,
        "secrets = [\"api_key\", \"b\", \"c\", \"d\", \"e\"]",
    );
    assert_eq!(refused(&many).0, "app_plugin_secrets_limit");
}

#[test]
fn placeholders_are_known_and_inputs_appear_once() {
    let unknown = with("kind = \"{{purpose}}\"", "kind = \"{{whoami}}\"");
    assert_eq!(refused(&unknown).0, "app_plugin_placeholder_unknown");
    let header = with("X-Client = \"crazyagent\"", "X-Client = \"{{inputs}}\"");
    assert_eq!(refused(&header).0, "app_plugin_placeholder_unknown");
    let unclosed = with("Bearer {{secret.api_key}}", "Bearer {{secret.api_key");
    assert_eq!(refused(&unclosed).0, "app_plugin_placeholder_unknown");
    let missing = with("input = \"{{inputs}}\"", "input = \"fixed\"");
    assert_eq!(refused(&missing).0, "app_plugin_inputs_missing");
    let repeated = with("model = \"acme-small\"", "model = \"{{inputs}}\"");
    assert_eq!(refused(&repeated).0, "app_plugin_inputs_repeated");
    let listed = with("input = \"{{inputs}}\"", "input = [\"{{inputs}}\"]");
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
        let text = with("X-Client = \"crazyagent\"", &format!("{reserved} = \"x\""));
        assert_eq!(refused(&text).0, "app_plugin_header_reserved", "{reserved}");
    }
    let bad_name = with("X-Client = \"crazyagent\"", "X_Client = \"crazyagent\"");
    assert_eq!(refused(&bad_name).0, "app_plugin_header_invalid");
    let twice = with("X-Client = \"crazyagent\"", "authorization = \"x\"");
    assert_eq!(refused(&twice).0, "app_plugin_header_invalid");
    let wide = with("X-Client = \"crazyagent\"", "X-Client = \"caf\\u00e9\"");
    assert_eq!(refused(&wide).0, "app_plugin_header_invalid");
    let number = parse(&with("X-Client = \"crazyagent\"", "X-Client = 7")).unwrap();
    assert_eq!(number.http.headers[1].value, [Piece::Text("7".into())]);
    let float = with("X-Client = \"crazyagent\"", "X-Client = 7.5");
    assert_eq!(refused(&float).0, "app_plugin_header_invalid");
}

#[test]
fn keys_are_known_present_and_typed() {
    let extra = with("batch = 16", "batch = 16\ntimeout = 5");
    assert_eq!(
        refused(&extra),
        (
            "app_plugin_field_unknown".into(),
            Some("http.timeout".into())
        )
    );
    let top = format!("color = \"red\"\n{HTTP}");
    assert_eq!(
        refused_at(&top),
        ("plugin_manifest_invalid".into(), Some(1))
    );
    let no_title = with("title = \"Acme embeddings\"\n", "");
    assert_eq!(
        refused(&no_title),
        ("app_plugin_field_missing".into(), Some("title".into()))
    );
    let no_url = with("url = \"https://api.acme.example/v1/embed\"\n", "");
    assert_eq!(
        refused(&no_url),
        ("app_plugin_field_missing".into(), Some("http.url".into()))
    );
    let typed = with("title = \"Acme embeddings\"", "title = [\"a\"]");
    assert_eq!(
        refused(&typed),
        ("app_plugin_field_invalid".into(), Some("title".into()))
    );
    let blank = with("title = \"Acme embeddings\"", "title = \"  \"");
    assert_eq!(refused(&blank).0, "app_plugin_field_invalid");
    for release in ["\"\"", "\"1 0\"", "1"] {
        let text = with("release = \"2.1.0\"", &format!("release = {release}"));
        assert_eq!(
            refused(&text),
            ("app_plugin_field_invalid".into(), Some("release".into())),
            "{release}"
        );
    }
    for name in ["9acme", "a-very-long-name-that-goes-past-forty-bytes"] {
        assert_eq!(
            refused(&with("plugin = \"acme\"", &format!("plugin = \"{name}\""))),
            ("app_plugin_name_invalid".into(), Some("plugin".into())),
            "{name}"
        );
    }
    for id in ["Acme", "acme_x"] {
        assert_eq!(
            refused_at(&with("plugin = \"acme\"", &format!("plugin = \"{id}\""))),
            ("plugin_id_invalid".into(), Some(1)),
            "{id}"
        );
    }
    assert!(valid_name("acme") && !valid_name("9acme"));
    let neither = HTTP.split("\n[http]").next().unwrap().to_owned();
    assert_eq!(
        refused(&neither),
        ("app_plugin_field_missing".into(), Some("http".into()))
    );
    let broken = format!("{HTTP}color = \n");
    let line = u32::try_from(broken.lines().count()).unwrap();
    assert_eq!(
        refused_at(&broken),
        ("plugin_manifest_invalid".into(), Some(line))
    );
}

#[test]
fn an_http_table_outside_the_embedding_kind_is_refused() {
    let setting = with("kind = \"embedding\"", "kind = \"setting\"");
    let read = crate::manifest::parse(&setting).expect_err("the five keys refuse it");
    assert_eq!(read.code, "plugin_manifest_invalid");
    let text = "plugin = \"quit\"\ntier = \"t2\"\nkind = \"setting\"\nversion = 1\npermissions = [\"settings.write\"]\n\n[http]\nurl = \"https://a.example\"\n";
    let read = crate::manifest::parse(text).expect_err("http is the embedding kind's");
    assert_eq!((read.code, read.line), ("plugin_manifest_invalid", 7));
    let title = "plugin = \"quit\"\ntier = \"t2\"\nkind = \"setting\"\nversion = 1\npermissions = []\ntitle = \"Quit\"\n";
    assert_eq!(
        crate::manifest::parse(title)
            .expect_err("title is the embedding kind's")
            .line,
        6
    );
    let base =
        crate::manifest::parse(HTTP).expect("the embedding kind's keys pass the five-key check");
    assert_eq!(base.kind, "embedding");
}

#[test]
fn a_manifest_with_http_and_net_hosts_is_refused() {
    let both = format!("{HTTP}\n[net]\nhosts = [\"api.acme.example\"]\n");
    let line = u32::try_from(both.lines().position(|line| line == "[net]").unwrap() + 1).unwrap();
    assert_eq!(
        refused_at(&both),
        ("plugin_manifest_invalid".into(), Some(line))
    );
    let read = crate::manifest::parse(&both).expect_err("net without net.request is refused");
    assert_eq!(read.code, "plugin_net_without_permission");
}

#[test]
fn vectors_dimensions_and_batch_are_bounded() {
    for path in [
        "data.embedding",
        "data.*.*",
        "data..embedding",
        "data.*.emb edding",
    ] {
        let text = with(
            "vectors = \"data.*.embedding\"",
            &format!("vectors = \"{path}\""),
        );
        assert_eq!(refused(&text).0, "app_plugin_vectors_invalid", "{path}");
    }
    for dimensions in ["63", "1025", "256.5", "\"x\""] {
        let text = with("dimensions = 256", &format!("dimensions = {dimensions}"));
        assert_eq!(
            refused(&text).0,
            "app_plugin_dimensions_invalid",
            "{dimensions}"
        );
    }
    for batch in ["0", "129"] {
        let text = with("batch = 16", &format!("batch = {batch}"));
        assert_eq!(refused(&text).0, "app_plugin_batch_invalid", "{batch}");
    }
    let default_batch = parse(&with("batch = 16\n", "")).unwrap();
    assert_eq!(default_batch.http.batch, BATCH_DEFAULT);
}

#[test]
fn a_body_value_toml_has_and_json_does_not_is_refused() {
    let date = with("model = \"acme-small\"", "model = 1979-05-27");
    assert_eq!(
        refused(&date),
        (
            "app_plugin_field_invalid".into(),
            Some("http.body.model".into())
        )
    );
    let float = parse(&with("model = \"acme-small\"", "model = 0.5")).unwrap();
    assert_eq!(
        float.http.body(&["a"], Purpose::Query)["model"],
        serde_json::json!(0.5)
    );
    let nan = with("model = \"acme-small\"", "model = nan");
    assert_eq!(refused(&nan).0, "app_plugin_field_invalid");
}

/// A key outside the plain key rule under `[http]`, its headers or its
/// body, at any depth, is refused naming only the table it sits in, so no key text reaches
/// the refusal shown on the trust screen.
#[test]
fn a_key_outside_the_plain_rule_is_refused_naming_only_its_table() {
    let escape = "\"\\u001b[2J\" = 1\n";
    for (table, code, field) in [
        ("[http]\n", "app_plugin_field_unknown", "http"),
        (
            "[http.headers]\n",
            "app_plugin_header_invalid",
            "http.headers",
        ),
        ("[http.body]\n", "app_plugin_field_invalid", "http.body"),
        (
            "[http.body.options]\n",
            "app_plugin_field_invalid",
            "http.body.options",
        ),
    ] {
        let text = with(table, &format!("{table}{escape}"));
        let problem = parse(&text).expect_err("the key is refused");
        assert_eq!(
            (problem.code.as_str(), problem.field.as_deref()),
            (code, Some(field)),
            "{table}"
        );
        assert!(!problem.message.chars().any(char::is_control), "{table}");
        assert!(problem.message.len() <= field.len() + 200, "{table}");
    }
    let long = format!("[http.body]\n{} = 1\n", "k".repeat(65));
    assert_eq!(
        refused(&with("[http.body]\n", &long)),
        (
            "app_plugin_field_invalid".to_owned(),
            Some("http.body".to_owned())
        )
    );
    let plain = with("[http.body]\n", "[http.body]\nk.v-1_x = 1\n");
    assert!(
        parse(&plain).is_ok(),
        "a dotted plain key is a nested table"
    );
}

/// The title and description refuse every Unicode mark that reorders or
/// hides text on the trust screen.
#[test]
fn a_title_or_description_with_a_bidi_or_zero_width_mark_is_refused() {
    let marks = [
        '\u{202A}', '\u{202B}', '\u{202C}', '\u{202D}', '\u{202E}', '\u{2066}', '\u{2067}',
        '\u{2068}', '\u{2069}', '\u{200B}', '\u{200C}', '\u{200D}', '\u{2060}', '\u{FEFF}',
    ];
    for mark in marks {
        for (old, field) in [
            ("title = \"Acme embeddings\"", "title"),
            ("description = \"Acme test plugin.\"", "description"),
        ] {
            let (key, rest) = old.split_once(" = \"").expect("a key line");
            let marked = format!("{key} = \"{mark}{rest}");
            assert_eq!(
                refused(&with(old, &marked)),
                (
                    "app_plugin_field_invalid".to_owned(),
                    Some(field.to_owned())
                ),
                "{field} U+{:04X}",
                u32::from(mark)
            );
        }
    }
}

/// Asserts that a title holding any mark of the range is refused on `title`. Input: the
/// inclusive range of marks.
fn refuses_title_marks(marks: core::ops::RangeInclusive<char>) {
    let old = "title = \"Acme embeddings\"";
    for mark in marks {
        let marked = format!("title = \"{mark}Acme embeddings\"");
        assert_eq!(
            refused(&with(old, &marked)),
            (
                "app_plugin_field_invalid".to_owned(),
                Some("title".to_owned())
            ),
            "title U+{:04X}",
            u32::from(mark)
        );
    }
}

#[test]
fn a_title_with_a_left_to_right_mark_is_refused() {
    refuses_title_marks('\u{200E}'..='\u{200E}');
}

#[test]
fn a_title_with_a_right_to_left_mark_is_refused() {
    refuses_title_marks('\u{200F}'..='\u{200F}');
}

#[test]
fn a_title_with_an_arabic_letter_mark_is_refused() {
    refuses_title_marks('\u{061C}'..='\u{061C}');
}

#[test]
fn a_title_with_a_line_separator_is_refused() {
    refuses_title_marks('\u{2028}'..='\u{2028}');
}

#[test]
fn a_title_with_a_paragraph_separator_is_refused() {
    refuses_title_marks('\u{2029}'..='\u{2029}');
}

#[test]
fn a_title_with_a_soft_hyphen_is_refused() {
    refuses_title_marks('\u{00AD}'..='\u{00AD}');
}

#[test]
fn a_title_with_a_mongolian_vowel_separator_is_refused() {
    refuses_title_marks('\u{180E}'..='\u{180E}');
}

#[test]
fn a_title_with_an_interlinear_annotation_mark_is_refused() {
    refuses_title_marks('\u{FFF9}'..='\u{FFFB}');
}

#[test]
fn a_title_with_a_tag_character_is_refused() {
    refuses_title_marks('\u{E0000}'..='\u{E007F}');
}

/// An embedding plugin asks the kernel for nothing, since the engine's HTTP
/// runner makes its call, so a permission is refused.
#[test]
fn an_embedding_plugin_with_permissions_is_refused() {
    assert_eq!(
        refused(&with(
            "permissions = []",
            "permissions = [\"net.http.get\"]"
        )),
        (
            "app_plugin_field_invalid".to_owned(),
            Some("permissions".to_owned())
        )
    );
}

/// An embedding source is T2, so any other tier is refused.
#[test]
fn an_embedding_plugin_outside_t2_is_refused() {
    for tier in ["t1", "t3"] {
        assert_eq!(
            refused(&with("tier = \"t2\"", &format!("tier = \"{tier}\""))),
            (
                "app_plugin_field_invalid".to_owned(),
                Some("tier".to_owned())
            ),
            "{tier}"
        );
    }
}

/// The declared kind is read from at most `MANIFEST_BYTES_MAX` bytes.
#[test]
fn the_declared_kind_of_a_text_over_the_bound_is_none() {
    let head = "kind = \"embedding\"\n";
    let fits = format!(
        "{head}{}",
        " ".repeat(crate::manifest::MANIFEST_BYTES_MAX - head.len())
    );
    assert_eq!(declared_kind(&fits), Some(PluginKind::Embedding));
    assert_eq!(declared_kind(&format!("{fits} ")), None);
}

/// A name Windows opens as a device is no plugin name on any system, so no plugin folder
/// and no left-over `.yaml` file is ever listed under one, while longer names that only
/// start like a device stay valid.
#[test]
fn a_windows_device_name_is_no_plugin_name() {
    for name in ["con", "nul", "aux", "prn", "com1", "lpt9"] {
        assert!(!valid_name(name), "{name}");
    }
    let legacy = "con.yaml".strip_suffix(".yaml").unwrap();
    assert!(!valid_name(legacy));
    for name in ["console", "com10", "nul-check", "auxiliary"] {
        assert!(valid_name(name), "{name}");
    }
}
