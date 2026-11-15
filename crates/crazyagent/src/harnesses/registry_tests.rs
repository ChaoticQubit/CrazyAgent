use super::*;
use crate::harnesses::{spec, specs};

fn sample(id: &str, alias: Option<&str>, binary: &str) -> String {
    let aliases = alias.map_or_else(String::new, |alias| format!("aliases:\n  - \"{alias}\"\n"));
    format!(
        "schema: 1\nkind: harness\nid: \"{id}\"\nname: \"Sample\"\n{aliases}binaries:\n  - \"{binary}\"\nheadless: \"sample --json\"\nmodel_selection: true\naccount_kinds:\n  - \"api_key\"\n"
    )
}

fn refusal(files: &[(&str, &str)]) -> (&'static str, String) {
    let refused = Registry::build(files).unwrap_err();
    (refused.code, refused.message)
}

#[test]
fn every_bundled_manifest_loads_in_catalog_order_under_its_own_file_name() {
    let ids: Vec<&str> = registry().all().iter().map(Harness::id).collect();
    assert_eq!(
        ids,
        [
            "codex",
            "claude-code",
            "opencode",
            "gemini-cli",
            "fx",
            "openclaw",
            "pi",
            "rcli",
            "goose",
            "aider",
            "qwen-code",
            "crush",
            "droid",
            "copilot",
            "cline",
            "continue",
            "openhands",
            "kimi",
        ]
    );
    assert_eq!(BUNDLED.len(), 18);
    for (file, text) in BUNDLED {
        let manifest = manifest::parse(text.as_bytes()).unwrap();
        assert_eq!(manifest.id, file);
        assert!(
            text.lines().all(|line| !line.trim_start().starts_with('#')),
            "{file} holds no comment"
        );
    }
    assert_eq!(specs().len(), ids.len());
    for (harness, listed) in registry().all().iter().zip(specs()) {
        assert_eq!(harness.id(), listed.id);
        let again = harness.spec();
        assert_eq!(again.binaries, listed.binaries, "{}", listed.id);
        assert_eq!(again.locations, listed.locations, "{}", listed.id);
        assert_eq!(again.launch, listed.launch, "{}", listed.id);
    }
}

const ANSWERS: [(&str, bool, bool, bool, bool); 18] = [
    ("codex", true, true, true, true),
    ("claude-code", true, true, true, true),
    ("opencode", false, false, false, true),
    ("gemini-cli", false, false, false, true),
    ("fx", false, false, false, false),
    ("openclaw", false, false, false, false),
    ("pi", false, false, false, true),
    ("rcli", false, true, false, false),
    ("goose", false, false, false, true),
    ("aider", false, false, false, false),
    ("qwen-code", false, false, false, true),
    ("crush", false, false, false, true),
    ("droid", false, false, false, true),
    ("copilot", false, false, false, true),
    ("cline", false, false, false, true),
    ("continue", false, false, false, true),
    ("openhands", false, false, false, false),
    ("kimi", false, false, false, true),
];

#[test]
fn every_harness_runs_an_agent_a_master_and_research_and_switches_tools_off_as_listed_here() {
    assert_eq!(registry().all().len(), ANSWERS.len());
    for (harness, (id, agent, master, research, tools_off)) in registry().all().iter().zip(ANSWERS)
    {
        assert_eq!(harness.id(), id);
        assert_eq!(harness.runnable(), agent, "{id}");
        assert_eq!(harness.master(), master, "{id}");
        assert_eq!(harness.research(), research, "{id}");
        assert_eq!(harness.tools_off(), tools_off, "{id}");
        assert_eq!(harness.piped().is_some(), master, "{id}");
        assert_eq!(harness.record() != RecordKind::Unavailable, agent, "{id}");
        assert_eq!(spec(id).unwrap().launch.is_none(), agent, "{id}");
        assert!(harness.owner_folders().is_empty(), "{id}");
        assert_eq!(harness.minimum_version(), None, "{id}");
    }
}

#[test]
fn research_runs_on_the_harnesses_the_registry_names_for_it() {
    for harness in registry().all() {
        let id = harness.id();
        assert_eq!(
            harness.research(),
            crate::research::runner::ResearchHarness::of(id).is_some(),
            "{id}"
        );
    }
}

#[test]
fn codex_claude_code_and_rcli_name_the_engines_own_drivers_readers_and_sub_agents() {
    let kinds = |name: &str| {
        let harness = registry().get(name).unwrap();
        (harness.piped(), harness.record(), harness.sub_agents())
    };
    assert_eq!(
        kinds("codex"),
        (
            Some(DriverKind::CodexAppServer),
            RecordKind::CodexRollouts,
            SubAgents::Threads
        )
    );
    assert_eq!(
        kinds("claude-code"),
        (
            Some(DriverKind::ClaudeStreamJson),
            RecordKind::ClaudeSession,
            SubAgents::Teams
        )
    );
    assert_eq!(
        kinds("rcli"),
        (
            Some(DriverKind::Rcli),
            RecordKind::Unavailable,
            SubAgents::App
        )
    );
    assert_eq!(kinds("pi"), (None, RecordKind::Unavailable, SubAgents::App));
}

#[test]
fn a_harness_is_found_by_its_id_or_an_alias_and_by_nothing_else() {
    let claude = registry().get("claude").unwrap();
    assert_eq!(claude.id(), "claude-code");
    assert_eq!(claude.aliases(), ["claude"]);
    assert_eq!(registry().position("claude"), Some(1));
    assert_eq!(registry().position("claude-code"), Some(1));
    assert_eq!(registry().position("codex"), Some(0));
    for unknown in ["", "Claude", "claude-codex", "cursor-agent", "antigravity"] {
        assert!(registry().get(unknown).is_none(), "{unknown}");
        assert!(spec(unknown).is_none(), "{unknown}");
    }
    let aliases: usize = registry()
        .all()
        .iter()
        .map(|harness| harness.aliases().len())
        .sum();
    assert_eq!(aliases, 1);
    assert_eq!(spec("claude").unwrap().id, "claude-code");
}

#[test]
fn a_registry_keeps_the_order_it_is_given_and_reads_any_manifest_that_is_valid() {
    let one = sample("one", Some("uno"), "one-cli");
    let two = sample("two", None, "two-cli");
    let built = Registry::build(&[("one", one.as_str()), ("two", two.as_str())]).unwrap();
    assert_eq!(built.all().len(), 2);
    assert_eq!(built.position("uno"), Some(0));
    assert_eq!(built.position("two"), Some(1));
    assert_eq!(built.get("uno").unwrap().manifest().id, "one");
    assert!(built.get("three").is_none());
    assert!(!built.get("one").unwrap().runnable());
    assert!(Registry::build(&[]).unwrap().all().is_empty());
}

#[test]
fn a_duplicate_id_alias_or_program_name_is_refused_with_both_manifests_named() {
    let one = sample("one", Some("uno"), "one-cli");
    let same_id = sample("one", None, "other-cli");
    assert_eq!(
        refusal(&[("one", one.as_str()), ("one", same_id.as_str())]),
        (
            manifest::DUPLICATE,
            "one: the name one is already used by one.".to_owned()
        )
    );
    let alias_is_id = sample("two", Some("one"), "two-cli");
    assert_eq!(
        refusal(&[("one", one.as_str()), ("two", alias_is_id.as_str())]),
        (
            manifest::DUPLICATE,
            "two: the name one is already used by one.".to_owned()
        )
    );
    let same_alias = sample("two", Some("uno"), "two-cli");
    assert_eq!(
        refusal(&[("one", one.as_str()), ("two", same_alias.as_str())]),
        (
            manifest::DUPLICATE,
            "two: the name uno is already used by one.".to_owned()
        )
    );
    let id_is_alias = sample("uno", None, "two-cli");
    assert_eq!(
        refusal(&[("one", one.as_str()), ("uno", id_is_alias.as_str())]),
        (
            manifest::DUPLICATE,
            "uno: the name uno is already used by one.".to_owned()
        )
    );
    let same_program = sample("two", None, "one-cli");
    assert_eq!(
        refusal(&[("one", one.as_str()), ("two", same_program.as_str())]),
        (
            manifest::DUPLICATE,
            "two: the program one-cli is already listed by one.".to_owned()
        )
    );
}

#[test]
fn a_file_not_named_after_its_id_or_holding_a_refused_manifest_names_the_file() {
    let one = sample("one", None, "one-cli");
    assert_eq!(
        refusal(&[("wrong", one.as_str())]),
        (
            manifest::VALUE,
            "wrong: the id inside is one; a manifest file is named after its id.".to_owned()
        )
    );
    assert_eq!(
        refusal(&[("one", one.as_str()), ("two", "schema: 2\n")]),
        (
            manifest::SCHEMA,
            "two: schema: this app reads schema: 1 harness manifests only.".to_owned()
        )
    );
    assert_eq!(
        refusal(&[("one", "")]),
        (manifest::YAML, "one: The file has no settings.".to_owned())
    );
}

#[test]
fn a_harness_speaks_the_wire_formats_its_manifest_lists_and_a_star_ends_a_prefix() {
    let speaks = |id: &str, protocol: &str| registry().get(id).unwrap().speaks(protocol);
    for protocol in [
        "openai",
        "openai-chat",
        "openai-responses",
        "openai-compatible",
    ] {
        assert!(speaks("codex", protocol), "{protocol}");
        assert!(!speaks("claude-code", protocol), "{protocol}");
        assert!(speaks("rcli", protocol), "{protocol}");
    }
    assert!(speaks("claude-code", "anthropic-messages"));
    assert!(speaks("claude", "anthropic-messages"));
    assert!(!speaks("claude-code", "anthropic-messages-2"));
    assert!(!speaks("claude-code", "anthropic"));
    assert!(!speaks("codex", "anthropic-messages"));
    assert!(!speaks("codex", "native"));
    assert!(!speaks("codex", "open"));
    assert!(speaks("rcli", "anthropic-messages"));
    assert!(speaks("rcli", ""));
    let mut silent = 0;
    for harness in registry().all() {
        if harness.account_protocols().is_empty() {
            silent += 1;
            assert!(!harness.speaks("openai-chat"), "{}", harness.id());
            assert!(!harness.speaks(""), "{}", harness.id());
        }
    }
    assert_eq!(silent, 15);
    assert_eq!(
        registry().get("codex").unwrap().account_protocols(),
        ["openai*"]
    );
}

#[test]
fn a_harness_is_named_by_its_id_and_its_aliases_only() {
    let claude = registry().get("claude-code").unwrap();
    assert!(claude.named("claude-code") && claude.named("claude"));
    assert!(!claude.named("Claude") && !claude.named("codex") && !claude.named(""));
}

#[test]
fn the_spec_of_a_harness_is_built_once_however_often_it_is_asked_for() {
    for harness in registry().all() {
        let (one, two) = (harness.spec(), harness.spec());
        assert!(
            std::ptr::eq(one.binaries, two.binaries),
            "{}: a second call copied the program names again",
            harness.id()
        );
        assert!(std::ptr::eq(one.name, two.name), "{}", harness.id());
        assert_eq!(one.binaries, two.binaries, "{}", harness.id());
        assert_eq!(one.locations, two.locations, "{}", harness.id());
    }
    for (harness, listed) in registry().all().iter().zip(specs()) {
        assert!(
            std::ptr::eq(harness.spec().binaries, listed.binaries),
            "{}: the catalog row and the harness share one copy",
            harness.id()
        );
    }
}
