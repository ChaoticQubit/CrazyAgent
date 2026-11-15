use super::*;
use crate::coord::settings::{HarnessKind, NativeSubAgents, native_sub_agents};
use crate::harnesses::interactive::{Support, support};
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
        assert!(!text.contains('#'), "{file} holds no comment");
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

#[test]
fn the_registry_answers_what_the_engine_answers_today_for_every_harness() {
    for harness in registry().all() {
        let id = harness.id();
        let listed = spec(id).unwrap();
        assert_eq!(
            harness.runnable(),
            crate::team::launch_harness(id).is_some(),
            "{id}"
        );
        assert_eq!(harness.runnable(), support(id) == Support::Terminal, "{id}");
        assert_eq!(
            harness.master(),
            crate::team::master_backend(id).is_some(),
            "{id}"
        );
        assert_eq!(
            harness.research(),
            crate::research::runner::ResearchHarness::of(id).is_some(),
            "{id}"
        );
        assert_eq!(harness.tools_off(), listed.tools_limited.is_none(), "{id}");
        assert_eq!(harness.piped().is_some(), harness.master(), "{id}");
        assert_eq!(
            harness.record() != RecordKind::Unavailable,
            harness.runnable(),
            "{id}"
        );
        assert_eq!(listed.launch.is_none(), harness.runnable(), "{id}");
        assert!(harness.owner_folders().is_empty(), "{id}");
        assert_eq!(harness.minimum_version(), None, "{id}");
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
    for (kind, id) in [
        (HarnessKind::ClaudeCode, "claude-code"),
        (HarnessKind::Codex, "codex"),
        (HarnessKind::Rcli, "rcli"),
    ] {
        let native = native_sub_agents(kind);
        match registry().get(id).unwrap().sub_agents() {
            SubAgents::Teams => assert_eq!(native, NativeSubAgents::Teams, "{id}"),
            SubAgents::Threads => assert_eq!(native, NativeSubAgents::Threads, "{id}"),
            SubAgents::App => {
                assert!(matches!(native, NativeSubAgents::Unavailable(_)), "{id}");
            }
        }
    }
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
