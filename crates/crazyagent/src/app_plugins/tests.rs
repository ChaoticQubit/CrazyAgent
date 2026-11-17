use super::*;

#[test]
fn every_problem_code_has_one_plain_sentence() {
    for (index, (code, plain)) in PLAIN.iter().enumerate() {
        assert!(code.starts_with("app_plugin"), "{code}");
        assert!(!plain.is_empty() && !plain.contains('\u{2014}'), "{code}");
        assert!(
            PLAIN[index + 1..].iter().all(|(other, _)| other != code),
            "{code} twice"
        );
    }
}

#[test]
fn every_code_the_reader_and_the_manifest_refuse_with_has_its_plain_sentence() {
    let sources = [include_str!("yaml.rs"), include_str!("manifest.rs")];
    let codes: Vec<&str> = sources
        .iter()
        .flat_map(|source| source.split('"'))
        .filter(|part| part.starts_with("app_plugin_"))
        .collect();
    assert!(codes.len() > 40, "the codes are read: {codes:?}");
    for code in codes {
        assert!(plain_problem(code).is_some(), "{code} has no plain sentence");
    }
    assert!(
        PLAIN
            .iter()
            .all(|(code, _)| !code.starts_with("app_plugin_connector")),
        "connectors re-land on the 2.5 host later"
    );
}

#[test]
fn a_problem_names_its_line_or_field_or_stands_alone() {
    let line = Problem::new("app_plugin_yaml_tab", Some(3), None);
    assert_eq!(line.message, "Line 3: use spaces, not tabs, to indent.");
    let field = Problem::new("app_plugin_host_undeclared", None, Some("http.url"));
    assert_eq!(field.message, "http.url: the URL's host is not in hosts.");
    let whole = Problem::new("app_plugin_yaml_empty", None, None);
    assert_eq!(whole.message, "The file has no settings.");
}

#[test]
fn the_content_hash_is_the_lowercase_hex_sha256_of_the_exact_bytes() {
    assert_eq!(
        sha256_hex(b"abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert_eq!(
        sha256_hex(b""),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
    assert_ne!(sha256_hex(b"abc\n"), sha256_hex(b"abc"));
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
    let problem = Problem::new("app_plugin_schema_unknown", None, Some("schema"));
    let shown = serde_json::to_value(&problem).unwrap();
    assert_eq!(shown["code"], "app_plugin_schema_unknown");
    assert_eq!(serde_json::from_value::<Problem>(shown).unwrap(), problem);
}
