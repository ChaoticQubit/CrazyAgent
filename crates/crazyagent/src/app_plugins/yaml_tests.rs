use super::{Value, parse};

fn map(value: &Value) -> &[(String, Value)] {
    match value {
        Value::Map(entries) => entries,
        other => panic!("expected a map, got {other:?}"),
    }
}

fn get<'a>(value: &'a Value, key: &str) -> &'a Value {
    map(value)
        .iter()
        .find(|(name, _)| name == key)
        .map(|(_, value)| value)
        .unwrap_or_else(|| panic!("no key {key}"))
}

fn refused(text: &str) -> (&'static str, u32) {
    let error = parse(text).expect_err("the text is refused");
    (error.code, error.line)
}

#[test]
fn block_maps_lists_and_typed_scalars_parse_in_order() {
    let value = parse(
        "schema: 1\nname: voyage\nhttp:\n  url: https://api.voyageai.com/v1/embeddings\n  hosts:\n    - api.voyageai.com\n  body:\n    truncation: true\n    scale: 0.5\n    empty: null\n    minus: -3\n",
    )
    .unwrap();
    let keys: Vec<&str> = map(&value).iter().map(|(key, _)| key.as_str()).collect();
    assert_eq!(keys, ["schema", "name", "http"]);
    assert_eq!(get(&value, "schema"), &Value::Int(1));
    assert_eq!(get(&value, "name"), &Value::Str("voyage".into()));
    let http = get(&value, "http");
    assert_eq!(
        get(http, "url"),
        &Value::Str("https://api.voyageai.com/v1/embeddings".into())
    );
    assert_eq!(
        get(http, "hosts"),
        &Value::List(vec![Value::Str("api.voyageai.com".into())])
    );
    let body = get(http, "body");
    assert_eq!(get(body, "truncation"), &Value::Bool(true));
    assert_eq!(get(body, "scale"), &Value::Float(0.5));
    assert_eq!(get(body, "empty"), &Value::Null);
    assert_eq!(get(body, "minus"), &Value::Int(-3));
}

#[test]
fn a_list_may_sit_at_its_key_s_indent() {
    let value = parse("hosts:\n- a.example.com\n- b.example.com\nname: x\n").unwrap();
    assert_eq!(
        get(&value, "hosts"),
        &Value::List(vec![
            Value::Str("a.example.com".into()),
            Value::Str("b.example.com".into())
        ])
    );
    assert_eq!(get(&value, "name"), &Value::Str("x".into()));
}

#[test]
fn flow_lists_hold_plain_and_quoted_scalars() {
    let value = parse("hosts: [api.openai.com, \"a, b\", 'it''s']\nnone: []\n").unwrap();
    assert_eq!(
        get(&value, "hosts"),
        &Value::List(vec![
            Value::Str("api.openai.com".into()),
            Value::Str("a, b".into()),
            Value::Str("it's".into()),
        ])
    );
    assert_eq!(get(&value, "none"), &Value::List(Vec::new()));
}

#[test]
fn comments_and_blank_lines_are_skipped_and_hash_inside_quotes_is_kept() {
    let value = parse(
        "# a plugin\n---\n\nname: x   # trailing\ntitle: \"A # B\"\nlabel: a#b\n  # indented comment\n",
    )
    .unwrap();
    assert_eq!(get(&value, "name"), &Value::Str("x".into()));
    assert_eq!(get(&value, "title"), &Value::Str("A # B".into()));
    assert_eq!(get(&value, "label"), &Value::Str("a#b".into()));
}

#[test]
fn double_quoted_escapes_decode_and_other_escapes_are_refused() {
    let value = parse("a: \"q\\\"b\\\\c\\n\\t\\/\\u00e9\"\n").unwrap();
    assert_eq!(get(&value, "a"), &Value::Str("q\"b\\c\n\t/\u{e9}".into()));
    assert_eq!(refused("a: \"\\x41\"\n"), ("app_plugin_yaml_escape", 1));
    assert_eq!(refused("a: \"\\ud800\"\n"), ("app_plugin_yaml_escape", 1));
}

#[test]
fn each_refused_construct_has_its_own_code_and_line() {
    assert_eq!(refused("a: 1\n\tb: 2\n"), ("app_plugin_yaml_tab", 2));
    assert_eq!(refused("a: &x 1\n"), ("app_plugin_yaml_anchor", 1));
    assert_eq!(refused("a: 1\nb: *x\n"), ("app_plugin_yaml_alias", 2));
    assert_eq!(refused("a: !!str 1\n"), ("app_plugin_yaml_tag", 1));
    assert_eq!(
        refused("a: |\n  text\n"),
        ("app_plugin_yaml_block_scalar", 1)
    );
    assert_eq!(
        refused("a: >\n  text\n"),
        ("app_plugin_yaml_block_scalar", 1)
    );
    assert_eq!(refused("a: {b: 1}\n"), ("app_plugin_yaml_flow_map", 1));
    assert_eq!(
        refused("%YAML 1.2\na: 1\n"),
        ("app_plugin_yaml_directive", 1)
    );
    assert_eq!(
        refused("a: 1\n---\nb: 2\n"),
        ("app_plugin_yaml_documents", 2)
    );
    assert_eq!(refused("a: 1\n...\n"), ("app_plugin_yaml_documents", 2));
    assert_eq!(
        refused("a: 1\na: 2\n"),
        ("app_plugin_yaml_duplicate_key", 2)
    );
    assert_eq!(
        refused("a:\n  b: 1\n   c: 2\n"),
        ("app_plugin_yaml_indent", 3)
    );
    assert_eq!(refused("a: 1\n  b: 2\n"), ("app_plugin_yaml_indent", 2));
    assert_eq!(refused("a:\n  - b: 1\n"), ("app_plugin_yaml_list_map", 2));
    assert_eq!(
        refused("a:\n  -\n    - b\n"),
        ("app_plugin_yaml_list_item", 2)
    );
    assert_eq!(refused("a: \"open\n"), ("app_plugin_yaml_quote", 1));
    assert_eq!(refused("a: 'open\n"), ("app_plugin_yaml_quote", 1));
    assert_eq!(refused("a: \"x\" y\n"), ("app_plugin_yaml_quote", 1));
    assert_eq!(refused("\"a\": 1\n"), ("app_plugin_yaml_key", 1));
    assert_eq!(refused("? a\n: 1\n"), ("app_plugin_yaml_complex_key", 1));
    assert_eq!(refused("a: @x\n"), ("app_plugin_yaml_reserved", 1));
    assert_eq!(refused("a: `x`\n"), ("app_plugin_yaml_reserved", 1));
    assert_eq!(refused("a: b: c\n"), ("app_plugin_yaml_syntax", 1));
    assert_eq!(refused("just text\n"), ("app_plugin_yaml_syntax", 1));
    assert_eq!(refused("- a\n- b\n"), ("app_plugin_yaml_top_level", 1));
    assert_eq!(refused("a: [b, [c]]\n"), ("app_plugin_yaml_flow_list", 1));
    assert_eq!(refused("a: [b, c\n"), ("app_plugin_yaml_flow_list", 1));
}

#[test]
fn a_placeholder_must_be_quoted_or_it_reads_as_a_flow_map() {
    assert_eq!(
        refused("input: {{inputs}}\n"),
        ("app_plugin_yaml_flow_map", 1)
    );
    let value = parse("input: \"{{inputs}}\"\n").unwrap();
    assert_eq!(get(&value, "input"), &Value::Str("{{inputs}}".into()));
}

#[test]
fn size_depth_encoding_and_emptiness_are_bounded() {
    let large = format!("a: \"{}\"\n", "x".repeat(super::BYTES_MAX));
    assert_eq!(refused(&large), ("app_plugin_yaml_too_large", 0));
    let mut deep = String::new();
    for level in 0..=super::DEPTH_MAX {
        deep.push_str(&" ".repeat(level * 2));
        deep.push_str("k:\n");
    }
    deep.push_str(&" ".repeat((super::DEPTH_MAX + 1) * 2));
    deep.push_str("v: 1\n");
    assert_eq!(refused(&deep).0, "app_plugin_yaml_depth");
    assert_eq!(
        super::parse_bytes(b"a: \xff\n").unwrap_err().code,
        "app_plugin_yaml_encoding"
    );
    assert_eq!(refused("# only a comment\n"), ("app_plugin_yaml_empty", 0));
    assert_eq!(refused(""), ("app_plugin_yaml_empty", 0));
}

#[test]
fn crlf_lines_and_a_leading_byte_order_mark_are_read() {
    let value = super::parse_bytes(b"\xef\xbb\xbfname: x\r\ntitle: y\r\n").unwrap();
    assert_eq!(get(&value, "name"), &Value::Str("x".into()));
    assert_eq!(get(&value, "title"), &Value::Str("y".into()));
}

#[test]
fn the_error_message_names_the_line_in_plain_words() {
    let error = parse("a: 1\n\tb: 2\n").unwrap_err();
    assert_eq!(error.message(), "Line 2: use spaces, not tabs, to indent.");
    let whole = parse("").unwrap_err();
    assert_eq!(whole.message(), "The file has no settings.");
}

#[test]
fn values_convert_to_json_with_their_types() {
    let value = parse("a: 1\nb: [x, 2]\nc:\n  d: false\n  e: 1.5\n  f: null\n").unwrap();
    assert_eq!(
        value.to_json(),
        serde_json::json!({"a": 1, "b": ["x", 2], "c": {"d": false, "e": 1.5, "f": null}})
    );
}
