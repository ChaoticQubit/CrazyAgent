use super::*;

const SMALL: &str = "schema: 1\nkind: harness\nid: \"sample\"\nname: \"Sample\"\nbinaries:\n  - \"sample\"\nheadless: \"sample --json\"\nmodel_selection: true\naccount_kinds:\n  - \"api_key\"\n";

fn with(extra: &str) -> String {
    format!("{SMALL}{extra}")
}

fn refused(text: &str) -> (&'static str, String) {
    let refused = parse(text.as_bytes()).unwrap_err();
    (refused.code, refused.message)
}

fn nested(levels: usize, last: &str) -> String {
    let mut text = with("home:\n");
    for level in 1..=levels {
        text.push_str(&" ".repeat(level * 2));
        text.push_str("a:\n");
    }
    text.push_str(&" ".repeat((levels + 1) * 2));
    text.push_str(last);
    text
}

#[test]
fn the_smallest_manifest_reads_with_plain_defaults() {
    let manifest = parse(SMALL.as_bytes()).unwrap();
    assert_eq!(manifest.id, "sample");
    assert_eq!(manifest.name, "Sample");
    assert_eq!(manifest.binaries, ["sample"]);
    assert_eq!(manifest.headless, "sample --json");
    assert!(manifest.model_selection);
    assert_eq!(manifest.account_kinds, [AccountKind::ApiKey]);
    assert!(manifest.aliases.is_empty() && manifest.locations.is_empty());
    assert_eq!(manifest.tools_limited, None);
    assert_eq!(manifest.cannot_start, None);
    assert!(manifest.roles.is_empty());
    assert_eq!(manifest.sub_agents, SubAgents::App);
    assert_eq!(manifest.piped, None);
    assert_eq!(manifest.record, RecordKind::Unavailable);
    assert!(manifest.owner_folders.is_empty());
    assert_eq!(manifest.minimum_version, None);
    assert_eq!(manifest.sign_in_minimum, None);
    assert_eq!(manifest.limits, None);
    assert!(manifest.account_protocols.is_empty());
    for key in BLOCKS {
        assert_eq!(manifest.block(key), None, "{key}");
    }
}

#[test]
fn a_full_manifest_reads_its_typed_settings_and_keeps_each_block_as_written() {
    let text = with(concat!(
        "aliases:\n  - \"smp\"\n",
        "locations:\n  - \".sample/bin\"\n",
        "tools_limited: \"tools cannot be turned off\"\n",
        "cannot_start: \"not wired yet\"\n",
        "roles:\n  - \"agent\"\n  - \"master\"\n  - \"research\"\n",
        "sub_agents: \"teams\"\n",
        "piped:\n  driver: \"jsonl-turn\"\n  argv:\n    - \"--model\"\n    - \"{model}\"\n",
        "terminal:\n  submit: \"enter\"\n  record:\n    reader: \"jsonl-append\"\n",
        "    path: \"{home}/sessions/{session}.jsonl\"\n",
        "version:\n  minimum: \"1.5.2\"\n",
        "home:\n  env:\n    SAMPLE_HOME: \"{home}/agent\"\n  owner_folders:\n    - \".sample\"\n",
        "accounts:\n  openai-chat:\n    render: \"template\"\n",
        "rules:\n  events:\n    started:\n      when:\n        type: \"start\"\n",
        "      emit: \"ready\"\n",
    ));
    let manifest = parse(text.as_bytes()).unwrap();
    assert_eq!(manifest.aliases, ["smp"]);
    assert_eq!(manifest.locations, [".sample/bin"]);
    assert_eq!(
        manifest.tools_limited.as_deref(),
        Some("tools cannot be turned off")
    );
    assert_eq!(manifest.cannot_start.as_deref(), Some("not wired yet"));
    assert_eq!(manifest.roles, [Role::Agent, Role::Master, Role::Research]);
    assert_eq!(manifest.sub_agents, SubAgents::Teams);
    assert_eq!(manifest.piped, Some(DriverKind::JsonlTurn));
    assert_eq!(manifest.record, RecordKind::JsonlAppend);
    assert_eq!(manifest.minimum_version.as_deref(), Some("1.5.2"));
    assert_eq!(manifest.owner_folders, [".sample"]);
    let render = manifest
        .block("accounts")
        .and_then(|accounts| accounts.get("openai-chat"))
        .and_then(|account| account.get("render"));
    assert_eq!(render, Some(&Value::Str("template".to_owned())));
    let argv = manifest.block("piped").and_then(|piped| piped.get("argv"));
    assert_eq!(
        argv,
        Some(&Value::List(vec![
            Value::Str("--model".to_owned()),
            Value::Str("{model}".to_owned()),
        ]))
    );
    let emit = manifest
        .block("rules")
        .and_then(|rules| rules.get("events"))
        .and_then(|events| events.get("started"))
        .and_then(|rule| rule.get("emit"));
    assert_eq!(emit, Some(&Value::Str("ready".to_owned())));
}

#[test]
fn every_account_kind_reads_by_the_name_the_apps_use() {
    let text = SMALL.replace(
        "  - \"api_key\"\n",
        "  - \"subscription\"\n  - \"api_key\"\n  - \"open_ai_compatible_url\"\n  - \"local\"\n",
    );
    assert_eq!(
        parse(text.as_bytes()).unwrap().account_kinds,
        [
            AccountKind::Subscription,
            AccountKind::ApiKey,
            AccountKind::OpenAiCompatibleUrl,
            AccountKind::Local,
        ]
    );
}

#[test]
fn every_driver_and_record_reader_kind_has_one_manifest_name() {
    let drivers = [
        ("codex-app-server", DriverKind::CodexAppServer),
        ("claude-stream-json", DriverKind::ClaudeStreamJson),
        ("rcli", DriverKind::Rcli),
        ("acp", DriverKind::Acp),
        ("jsonl-turn", DriverKind::JsonlTurn),
        ("jsonl-rpc", DriverKind::JsonlRpc),
        ("text-turn", DriverKind::TextTurn),
    ];
    for (name, kind) in drivers {
        assert_eq!(DriverKind::of(name), Some(kind), "{name}");
        let text = with(&format!("piped:\n  driver: \"{name}\"\n"));
        assert_eq!(parse(text.as_bytes()).unwrap().piped, Some(kind), "{name}");
    }
    let readers = [
        ("codex-rollouts", RecordKind::CodexRollouts),
        ("claude-session", RecordKind::ClaudeSession),
        ("jsonl-append", RecordKind::JsonlAppend),
        ("json-rewrite", RecordKind::JsonRewrite),
        ("sqlite-poll", RecordKind::SqlitePoll),
        ("markdown-history", RecordKind::MarkdownHistory),
        ("screen", RecordKind::Screen),
        ("unavailable", RecordKind::Unavailable),
    ];
    for (name, kind) in readers {
        assert_eq!(RecordKind::of(name), Some(kind), "{name}");
        let text = with(&format!("terminal:\n  record:\n    reader: \"{name}\"\n"));
        assert_eq!(parse(text.as_bytes()).unwrap().record, kind, "{name}");
    }
    assert_eq!(DriverKind::of("jsonl_turn"), None);
    assert_eq!(RecordKind::of(""), None);
}

#[test]
fn an_unknown_key_and_a_wrong_schema_or_kind_are_refused_in_plain_words() {
    assert_eq!(
        refused(&with("colour: \"red\"\n")),
        (
            UNKNOWN_KEY,
            "colour: this key is not one a harness manifest has.".to_owned()
        )
    );
    assert_eq!(
        refused(&SMALL.replace("schema: 1", "schema: 2")),
        (
            SCHEMA,
            "schema: this app reads schema: 1 harness manifests only.".to_owned()
        )
    );
    assert_eq!(
        refused(&SMALL.replace("kind: harness", "kind: plugin")),
        (SCHEMA, "kind: the only kind is harness.".to_owned())
    );
}

#[test]
fn a_manifest_over_16_kib_or_nested_too_deep_is_refused_by_the_strict_reader() {
    let large = with(&format!(
        "tools_limited: \"{}\"\n",
        "x".repeat(yaml::BYTES_MAX)
    ));
    assert_eq!(
        refused(&large),
        (YAML, "The file is larger than 16 KiB.".to_owned())
    );
    let (code, message) = refused(&nested(yaml::DEPTH_MAX, "b: \"c\"\n"));
    assert_eq!(code, YAML);
    assert!(
        message.starts_with("Line ") && message.ends_with("the settings are nested too deep."),
        "{message}"
    );
    let deepest = nested(yaml::DEPTH_MAX - 1, "b: \"{home}\"\n");
    assert!(parse(deepest.as_bytes()).is_ok());
    assert_eq!(
        refused(&with("accounts:\n  - protocol: \"openai-chat\"\n")),
        (
            YAML,
            "Line 12: list items are single values; lists of maps are not supported.".to_owned()
        )
    );
}

#[test]
fn a_driver_or_record_reader_the_engine_does_not_have_is_refused() {
    assert_eq!(
        refused(&with("piped:\n  driver: \"telepathy\"\n")),
        (
            KIND,
            "piped.driver: CrazyAgent has no driver named telepathy.".to_owned()
        )
    );
    assert_eq!(
        refused(&with(
            "terminal:\n  record:\n    reader: \"crystal-ball\"\n"
        )),
        (
            KIND,
            "terminal.record.reader: CrazyAgent has no record reader named crystal-ball."
                .to_owned()
        )
    );
    assert_eq!(
        refused(&with("piped:\n  argv:\n    - \"--json\"\n")),
        (MISSING, "piped.driver: this setting is needed.".to_owned())
    );
}

#[test]
fn only_the_listed_placeholders_are_filled_and_any_other_is_refused() {
    let mut text = with("piped:\n  driver: \"jsonl-turn\"\n  argv:\n");
    for name in PLACEHOLDERS {
        text.push_str(&format!("    - \"{{{name}}}\"\n"));
    }
    text.push_str("  template: \"{\\\"model\\\": \\\"{model}\\\", \\\"n\\\": {1}}\"\n");
    assert!(parse(text.as_bytes()).is_ok());
    assert_eq!(
        refused(&with(
            "piped:\n  driver: \"jsonl-turn\"\n  argv:\n    - \"{secret}\"\n"
        )),
        (
            PLACEHOLDER,
            "piped.argv: {secret} is not a value CrazyAgent fills in.".to_owned()
        )
    );
    assert_eq!(
        refused(&with(
            "home:\n  env:\n    SAMPLE_KEY: \"x{home}y{api_key}\"\n"
        )),
        (
            PLACEHOLDER,
            "home.env.SAMPLE_KEY: {api_key} is not a value CrazyAgent fills in.".to_owned()
        )
    );
}

#[test]
fn a_missing_or_mistyped_setting_names_itself() {
    let cases: [(String, &str, &str); 8] = [
        (
            SMALL.replace("id: \"sample\"\n", ""),
            MISSING,
            "id: this setting is needed.",
        ),
        (
            SMALL.replace("binaries:\n  - \"sample\"\n", ""),
            MISSING,
            "binaries: this setting is needed.",
        ),
        (
            SMALL.replace("account_kinds:\n  - \"api_key\"\n", ""),
            MISSING,
            "account_kinds: this setting is needed.",
        ),
        (
            SMALL.replace("model_selection: true\n", ""),
            MISSING,
            "model_selection: this setting is needed.",
        ),
        (
            SMALL.replace("name: \"Sample\"", "name: 7"),
            TYPE,
            "name: this must be text in quotes.",
        ),
        (
            SMALL.replace("model_selection: true", "model_selection: \"yes\""),
            TYPE,
            "model_selection: this must be true or false.",
        ),
        (
            with("version:\n  minimum: 1.5\n"),
            TYPE,
            "version.minimum: this must be text in quotes.",
        ),
        (
            with("home: \"here\"\n"),
            TYPE,
            "home: this must be a block of settings.",
        ),
    ];
    for (text, code, message) in cases {
        assert_eq!(refused(&text), (code, message.to_owned()));
    }
}

#[test]
fn a_value_outside_what_a_manifest_allows_is_refused_with_the_value_named() {
    let cases: [(String, &str); 9] = [
        (
            SMALL.replace("id: \"sample\"", "id: \"Sample\""),
            "id: Sample is not lower-case letters, digits and dashes.",
        ),
        (
            SMALL.replace("  - \"sample\"", "  - \"bin/sample\""),
            "binaries: bin/sample is not a program name without a folder.",
        ),
        (
            with("locations:\n  - \"../etc\"\n"),
            "locations: ../etc is not a folder under the home folder.",
        ),
        (
            with("locations:\n  - \"/etc\"\n"),
            "locations: /etc is not a folder under the home folder.",
        ),
        (
            SMALL.replace("api_key", "gold"),
            "account_kinds: gold is not a kind of account.",
        ),
        (
            with("aliases:\n  - \"sample\"\n"),
            "aliases: sample is the id itself.",
        ),
        (
            with("roles:\n  - \"pilot\"\n"),
            "roles: pilot is not agent, master or research.",
        ),
        (
            with("sub_agents: \"swarm\"\n"),
            "sub_agents: swarm is not teams, threads or app.",
        ),
        (
            with("roles:\n  - \"agent\"\n  - \"agent\"\n"),
            "roles: agent is listed twice.",
        ),
    ];
    for (text, message) in cases {
        assert_eq!(refused(&text), (VALUE, message.to_owned()));
    }
    let long = "a".repeat(NAME_BYTES_MAX + 1);
    assert_eq!(
        refused(&SMALL.replace("id: \"sample\"", &format!("id: \"{long}\""))),
        (VALUE, "id: this must be 1 to 64 bytes of text.".to_owned())
    );
    let mut many = with("aliases:\n");
    for index in 0..=LIST_MAX {
        many.push_str(&format!("  - \"alias-{index}\"\n"));
    }
    assert_eq!(
        refused(&many),
        (
            VALUE,
            "aliases: a list holds at most 16 entries.".to_owned()
        )
    );
}

#[test]
fn the_wire_formats_the_limits_reader_and_the_sign_in_minimum_read_as_written() {
    let manifest = parse(
        with(concat!(
            "account_protocols:\n  - \"openai*\"\n  - \"anthropic-messages\"\n  - \"*\"\n",
            "limits: \"claude\"\n",
            "version:\n  minimum: \"1.5.2\"\n  sign_in_minimum: \"2.1.268\"\n",
        ))
        .as_bytes(),
    )
    .unwrap();
    assert_eq!(
        manifest.account_protocols,
        ["openai*", "anthropic-messages", "*"]
    );
    assert_eq!(manifest.limits, Some(LimitsKind::Claude));
    assert_eq!(manifest.sign_in_minimum.as_deref(), Some("2.1.268"));
    assert_eq!(manifest.minimum_version.as_deref(), Some("1.5.2"));
    assert_eq!(
        parse(with("limits: \"codex\"\n").as_bytes())
            .unwrap()
            .limits,
        Some(LimitsKind::Codex)
    );
    assert_eq!(LimitsKind::of("moonshot"), None);
}

#[test]
fn a_wire_format_a_limits_reader_or_a_sign_in_minimum_that_is_not_allowed_is_refused() {
    for name in ["Open AI", "open*ai", "**", "openai_chat"] {
        assert_eq!(
            refused(&with(&format!("account_protocols:\n  - \"{name}\"\n"))),
            (
                VALUE,
                format!(
                    "account_protocols: {name} is not a wire format name, with * allowed only as its last character."
                )
            ),
            "{name}"
        );
    }
    assert_eq!(
        refused(&with("account_protocols: \"openai*\"\n")),
        (
            TYPE,
            "account_protocols: this must be a list of texts.".to_owned()
        )
    );
    assert_eq!(
        refused(&with("limits: \"moonshot\"\n")),
        (
            KIND,
            "limits: CrazyAgent has no limits reader named moonshot.".to_owned()
        )
    );
    for minimum in ["v2.1", "2.1-beta", ".5"] {
        assert_eq!(
            refused(&with(&format!(
                "version:\n  sign_in_minimum: \"{minimum}\"\n"
            ))),
            (
                VALUE,
                format!(
                    "version.sign_in_minimum: {minimum} is not a version of two to six numbers joined by dots."
                )
            ),
            "{minimum}"
        );
    }
    assert_eq!(
        refused(&with("version:\n  sign_in_minimum: 2.1\n")),
        (
            TYPE,
            "version.sign_in_minimum: this must be text in quotes.".to_owned()
        )
    );
    assert_eq!(
        refused(&with("usage:\n  limits: \"codex\"\n")),
        (
            UNKNOWN_KEY,
            "usage: this key is not one a harness manifest has.".to_owned()
        )
    );
}

#[test]
fn a_version_floor_the_version_check_cannot_compare_is_refused_when_the_manifest_is_read() {
    for floor in ["2", "1..2", "1.", "1.2.3.4.5.6.7", "99999999999999999999.1"] {
        for key in ["sign_in_minimum", "minimum"] {
            let text = with(&format!("version:\n  {key}: \"{floor}\"\n"));
            assert!(parse(text.as_bytes()).is_err(), "{key} {floor}");
        }
    }
    for floor in ["0.56.0", "2.1.268", "1.5", "1.2.3.4.5.6"] {
        let text = with(&format!(
            "version:\n  sign_in_minimum: \"{floor}\"\n  minimum: \"{floor}\"\n"
        ));
        let manifest = parse(text.as_bytes()).unwrap();
        assert_eq!(manifest.sign_in_minimum.as_deref(), Some(floor));
        assert!(!crate::harnesses::update::below("9999.0", floor), "{floor}");
        assert!(crate::harnesses::update::below("0.0", floor), "{floor}");
    }
}
