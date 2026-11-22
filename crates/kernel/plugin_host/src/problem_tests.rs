use crate::problem::{PLAIN, PORT_HANDED_OUT, Problem, plain_problem};

#[test]
fn every_problem_code_has_one_plain_sentence() {
    for (index, (code, plain)) in PLAIN.iter().enumerate() {
        assert!(
            code.starts_with("app_plugin_") || code.starts_with("plugin_"),
            "{code}"
        );
        assert!(
            !code.starts_with("app_plugin_yaml_"),
            "{code} is the harness reader's"
        );
        assert!(!plain.is_empty() && !plain.contains('\u{2014}'), "{code}");
        assert!(
            PLAIN[index + 1..].iter().all(|(other, _)| other != code),
            "{code} twice"
        );
    }
}

#[test]
fn every_code_the_kernel_readers_refuse_with_has_its_plain_sentence() {
    let sources = [include_str!("manifest.rs"), include_str!("embedding.rs")];
    let codes: Vec<&str> = sources
        .iter()
        .flat_map(|source| source.split('"'))
        .filter(|part| part.starts_with("app_plugin_") || part.starts_with("plugin_"))
        .filter(|part| {
            part.bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte == b'_')
        })
        .collect();
    assert!(codes.len() > 30, "the codes are read: {codes:?}");
    for code in codes {
        assert!(
            plain_problem(code).is_some(),
            "{code} has no plain sentence"
        );
    }
}

#[test]
fn a_problem_names_its_line_or_field_or_stands_alone() {
    let line = Problem::new("app_plugin_field_invalid", Some(3), None);
    assert_eq!(
        line.message,
        "Line 3: this value has the wrong type or length."
    );
    let field = Problem::new("app_plugin_host_undeclared", None, Some("http.url"));
    assert_eq!(field.message, "http.url: the URL's host is not in hosts.");
    let whole = Problem::new("app_plugin_off", None, None);
    assert_eq!(whole.message, "This plugin is turned off.");
}

#[test]
fn a_handed_out_port_names_the_port_and_the_range() {
    let problem = Problem::port_handed_out(40_000, &(32_768..=60_999));
    assert_eq!(problem.code, PORT_HANDED_OUT);
    assert_eq!(problem.field.as_deref(), Some("http.url"));
    assert!(
        problem.message.starts_with(
            "http.url: port 40000 is one this Linux hands out by itself (ports 32768 to 60999)"
        ),
        "{}",
        problem.message
    );
}

#[test]
fn a_problem_round_trips_through_serde() {
    let problem = Problem::new("app_plugin_schema_unknown", None, Some("version"));
    let shown = serde_json::to_value(&problem).expect("serializes");
    assert_eq!(
        shown,
        serde_json::json!({
            "code": "app_plugin_schema_unknown",
            "line": null,
            "field": "version",
            "message": problem.message,
        })
    );
    assert_eq!(
        serde_json::from_value::<Problem>(shown).expect("reads back"),
        problem
    );
}
