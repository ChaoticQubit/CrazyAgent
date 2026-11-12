use super::store::{self, Begun, Pick};
use super::*;
use crate::harness_settings::{Access, Scope, Source};
use crate::protocol::ProjectId;
use rusqlite::Connection;
use serde_json::{Value, json};

const PROJECT: &str = "alpha";

/// An engine database holding the authority tables and one set-up project, in a private
/// temporary folder. Output: the folder, kept alive by the caller, and the connection.
fn engine() -> (tempfile::TempDir, Connection) {
    let tmp = tempfile::Builder::new()
        .prefix("ca-model-choice-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    let base = tmp.path().canonicalize().unwrap();
    let folder = base.join("project");
    std::fs::create_dir(&folder).unwrap();
    let db = Connection::open(base.join("engine.db")).unwrap();
    crate::storage::initialize_authority_schemas(&db).unwrap();
    crate::ide::workspace::tests::add_project(&db, PROJECT, &folder);
    (tmp, db)
}

fn alpha() -> Scope {
    Scope::Project(ProjectId::new(PROJECT).unwrap())
}

fn catalog(ids: &[&str]) -> Vec<String> {
    ids.iter().map(|id| (*id).to_owned()).collect()
}

fn row(models: &Models, slot: Slot) -> &Row {
    models
        .rows
        .iter()
        .find(|row| row.slot == slot)
        .unwrap_or_else(|| panic!("no {slot:?} row"))
}

fn saved(written: Written) -> Row {
    match written {
        Written::Saved { row } => row,
        other => panic!("not saved: {other:?}"),
    }
}

fn set(
    db: &Connection,
    harness: &str,
    ids: &[&str],
    scope: &Scope,
    slot: Slot,
    value: Value,
) -> Result<Written, crate::engine::EngineError> {
    let models = store::values(db, harness, &catalog(ids), scope, None).unwrap();
    let version = row(&models, slot).version;
    store::set(db, harness, &catalog(ids), scope, slot, &value, version)
}

#[test]
fn a_model_id_is_checked_before_it_reaches_a_command_line() {
    for good in [
        "gpt-5.6-sol",
        "claude-opus-5-5",
        "openai/gpt-4o",
        "qwen3:32b",
        "a",
    ] {
        assert!(model_valid("codex", good), "{good}");
        assert!(model_valid("claude-code", good), "{good}");
    }
    assert!(model_valid("claude-code", "opus[1m]"));
    assert!(!model_valid("codex", "opus[1m]"));
    let long = "m".repeat(MODEL_BYTES_MAX + 1);
    for bad in [
        "",
        "-c",
        "--model",
        "has space",
        "model\"\nfeatures.shell_tool=true",
        "quote\"",
        "back\\slash",
        "tab\tid",
        "[1m]",
        "ünïcode",
        long.as_str(),
    ] {
        assert!(!model_valid("codex", bad), "{bad:?}");
        assert!(!model_valid("claude-code", bad), "{bad:?}");
    }
    assert!(model_valid("codex", &"m".repeat(MODEL_BYTES_MAX)));
}

#[test]
fn with_nothing_set_every_slot_leaves_the_model_to_the_account() {
    let (_tmp, db) = engine();
    for scope in [Scope::App, alpha()] {
        let models = store::values(&db, "codex", &catalog(&["a", "b"]), &scope, None).unwrap();
        assert_eq!(models.harness, "codex");
        assert_eq!(models.rows.len(), 3);
        for slot in [Slot::Master, Slot::Agent] {
            let row = row(&models, slot);
            assert_eq!(row.here, None);
            assert_eq!(row.effective, Value::Null);
            assert_eq!(row.source, Source::Default);
            assert_eq!(row.version, 0);
            assert_eq!(row.access, Access::Editable);
        }
        let allowed = row(&models, Slot::Allowed);
        assert_eq!(allowed.effective, json!([]));
        assert_eq!(allowed.source, Source::Default);
    }
    assert_eq!(
        store::launch_model(&db, "codex", &catalog(&["a"]), None, Slot::Agent).unwrap(),
        None
    );
}

#[test]
fn a_project_override_beats_the_app_value_and_a_reset_falls_back() {
    let (_tmp, db) = engine();
    let ids = ["gpt-a", "gpt-b", "gpt-c"];
    let project = ProjectId::new(PROJECT).unwrap();
    saved(set(&db, "codex", &ids, &Scope::App, Slot::Agent, json!("gpt-a")).unwrap());
    saved(
        set(
            &db,
            "codex",
            &ids,
            &Scope::App,
            Slot::Master,
            json!("gpt-c"),
        )
        .unwrap(),
    );
    let here = saved(set(&db, "codex", &ids, &alpha(), Slot::Agent, json!("gpt-b")).unwrap());
    assert_eq!(here.here, Some(json!("gpt-b")));
    assert_eq!(here.effective, json!("gpt-b"));
    assert_eq!(here.source, Source::Project);
    assert_eq!(here.version, 1);

    let app = store::values(&db, "codex", &catalog(&ids), &Scope::App, None).unwrap();
    assert_eq!(row(&app, Slot::Agent).effective, json!("gpt-a"));
    assert_eq!(row(&app, Slot::Agent).source, Source::App);
    let mine = store::values(&db, "codex", &catalog(&ids), &alpha(), None).unwrap();
    assert_eq!(row(&mine, Slot::Master).here, None);
    assert_eq!(row(&mine, Slot::Master).effective, json!("gpt-c"));
    assert_eq!(row(&mine, Slot::Master).source, Source::App);
    assert_eq!(
        store::launch_model(&db, "codex", &catalog(&ids), Some(&project), Slot::Agent).unwrap(),
        Some("gpt-b".to_owned())
    );
    assert_eq!(
        store::launch_model(&db, "codex", &catalog(&ids), None, Slot::Agent).unwrap(),
        Some("gpt-a".to_owned())
    );

    let reset = store::reset(&db, "codex", &catalog(&ids), &alpha(), Slot::Agent, 1).unwrap();
    let reset = saved(reset);
    assert_eq!(reset.here, None);
    assert_eq!(reset.effective, json!("gpt-a"));
    assert_eq!(reset.source, Source::App);
    assert_eq!(reset.version, 2);
    assert_eq!(
        store::launch_model(&db, "codex", &catalog(&ids), Some(&project), Slot::Agent).unwrap(),
        Some("gpt-a".to_owned())
    );
}

#[test]
fn a_value_outside_the_harness_models_or_its_shape_is_refused() {
    let (_tmp, db) = engine();
    let ids = ["gpt-a", "gpt-b"];
    let cases = [
        (Slot::Agent, json!("gpt-z")),
        (Slot::Agent, json!(["gpt-a"])),
        (Slot::Agent, json!(7)),
        (Slot::Agent, json!("bad id")),
        (Slot::Master, json!(null)),
        (Slot::Allowed, json!("gpt-a")),
        (Slot::Allowed, json!(["gpt-a", "gpt-a"])),
        (Slot::Allowed, json!(["gpt-a", "gpt-z"])),
        (Slot::Allowed, json!(["gpt-a", 3])),
    ];
    for (slot, value) in cases {
        let refused = set(&db, "codex", &ids, &Scope::App, slot, value.clone()).unwrap_err();
        assert_eq!(refused.0, "model_choice_value_invalid", "{slot:?} {value}");
    }
    let many: Vec<String> = (0..=ALLOWED_MAX).map(|n| format!("m{n}")).collect();
    let names: Vec<&str> = many.iter().map(String::as_str).collect();
    let refused = set(
        &db,
        "codex",
        &names,
        &Scope::App,
        Slot::Allowed,
        json!(many),
    )
    .unwrap_err();
    assert_eq!(refused.0, "model_choice_value_invalid");
    let models = store::values(&db, "codex", &catalog(&ids), &Scope::App, None).unwrap();
    assert!(
        models
            .rows
            .iter()
            .all(|row| row.version == 0 && row.here.is_none())
    );

    let allowed = saved(
        set(
            &db,
            "codex",
            &ids,
            &Scope::App,
            Slot::Allowed,
            json!(["gpt-b", "gpt-a"]),
        )
        .unwrap(),
    );
    assert_eq!(allowed.effective, json!(["gpt-b", "gpt-a"]));
    let none = saved(set(&db, "codex", &ids, &alpha(), Slot::Allowed, json!([])).unwrap());
    assert_eq!(none.effective, json!([]));
    assert_eq!(none.source, Source::Project);
}

#[test]
fn a_stale_set_is_refused_with_the_current_row() {
    let (_tmp, db) = engine();
    let ids = catalog(&["gpt-a", "gpt-b"]);
    saved(
        store::set(
            &db,
            "codex",
            &ids,
            &Scope::App,
            Slot::Agent,
            &json!("gpt-a"),
            0,
        )
        .unwrap(),
    );
    match store::set(
        &db,
        "codex",
        &ids,
        &Scope::App,
        Slot::Agent,
        &json!("gpt-b"),
        0,
    )
    .unwrap()
    {
        Written::Stale { row } => {
            assert_eq!(row.here, Some(json!("gpt-a")));
            assert_eq!(row.version, 1);
        }
        other => panic!("not stale: {other:?}"),
    }
}

#[test]
fn a_slot_the_harness_cannot_apply_shows_not_supported_and_refuses_a_set() {
    let (_tmp, db) = engine();
    let models = store::values(&db, "rcli", &catalog(&["local"]), &Scope::App, None).unwrap();
    for slot in [Slot::Master, Slot::Agent, Slot::Allowed] {
        assert_eq!(
            row(&models, slot).access,
            Access::ReadOnly {
                reason: NOT_SUPPORTED.into()
            },
            "{slot:?}"
        );
    }
    for slot in [Slot::Master, Slot::Agent] {
        match set(&db, "rcli", &["local"], &Scope::App, slot, json!("local")).unwrap() {
            Written::Refused { code, row } => {
                assert_eq!(code, "model_choice_not_supported");
                assert_eq!(row.here, None);
            }
            other => panic!("not refused: {other:?}"),
        }
    }
    let unknown = store::values(&db, "nope", &catalog(&["a"]), &Scope::App, None).unwrap_err();
    assert_eq!(unknown.0, "model_choice_harness_unknown");
    let alias = store::values(&db, "claude", &catalog(&["sonnet"]), &Scope::App, None).unwrap();
    assert_eq!(alias.harness, "claude-code");
}

#[test]
fn an_unknown_project_is_refused() {
    let (_tmp, db) = engine();
    let other = Scope::Project(ProjectId::new("beta").unwrap());
    let refused = store::values(&db, "codex", &catalog(&["a"]), &other, None).unwrap_err();
    assert_eq!(refused.0, "model_choice_project_unknown");
    let refused = store::set(
        &db,
        "codex",
        &catalog(&["a"]),
        &other,
        Slot::Agent,
        &json!("a"),
        0,
    )
    .unwrap_err();
    assert_eq!(refused.0, "model_choice_project_unknown");
}

#[test]
fn an_added_model_is_usable_only_after_its_test_launch_passes() {
    let (_tmp, db) = engine();
    let ids = catalog(&["gpt-a"]);
    assert_eq!(
        store::custom_begin(&db, "codex", &ids, "gpt-a").unwrap(),
        Begun::Listed
    );
    assert_eq!(
        store::custom_begin(&db, "codex", &ids, "bad id")
            .unwrap_err()
            .0,
        "model_choice_value_invalid"
    );
    assert_eq!(
        store::custom_begin(&db, "codex", &ids, "my-model").unwrap(),
        Begun::Pending
    );
    let models = store::values(&db, "codex", &ids, &Scope::App, None).unwrap();
    let mine = models
        .candidates
        .iter()
        .find(|c| c.id == "my-model")
        .unwrap();
    assert_eq!(mine.origin, Origin::Pending);
    let refused = store::set(
        &db,
        "codex",
        &ids,
        &Scope::App,
        Slot::Agent,
        &json!("my-model"),
        0,
    )
    .unwrap_err();
    assert_eq!(refused.0, "model_choice_value_invalid");

    store::custom_checked(&db, "codex", "my-model", false).unwrap();
    let models = store::values(&db, "codex", &ids, &Scope::App, None).unwrap();
    let mine = models
        .candidates
        .iter()
        .find(|c| c.id == "my-model")
        .unwrap();
    assert_eq!(mine.origin, Origin::Failed);
    assert_eq!(
        store::custom_checked(&db, "codex", "my-model", true)
            .unwrap_err()
            .0,
        "model_choice_custom_not_pending"
    );

    assert_eq!(
        store::custom_begin(&db, "codex", &ids, "my-model").unwrap(),
        Begun::Pending
    );
    store::custom_checked(&db, "codex", "my-model", true).unwrap();
    let row = saved(
        store::set(
            &db,
            "codex",
            &ids,
            &Scope::App,
            Slot::Agent,
            &json!("my-model"),
            0,
        )
        .unwrap(),
    );
    assert_eq!(row.effective, json!("my-model"));
    assert_eq!(
        store::launch_model(&db, "codex", &ids, None, Slot::Agent).unwrap(),
        Some("my-model".to_owned())
    );
    let other =
        store::values(&db, "claude-code", &catalog(&["sonnet"]), &Scope::App, None).unwrap();
    assert!(other.candidates.iter().all(|c| c.id != "my-model"));

    assert!(store::custom_remove(&db, "codex", "my-model").unwrap());
    assert!(!store::custom_remove(&db, "codex", "my-model").unwrap());
    let models = store::values(&db, "codex", &ids, &Scope::App, None).unwrap();
    assert_eq!(row_here(&models, Slot::Agent), Some(json!("my-model")));
    assert_eq!(
        super::store::launch_model(&db, "codex", &ids, None, Slot::Agent).unwrap(),
        None
    );
}

fn row_here(models: &Models, slot: Slot) -> Option<Value> {
    row(models, slot).here.clone()
}

#[test]
fn added_models_stop_at_their_bound() {
    let (_tmp, db) = engine();
    let ids = catalog(&["gpt-a"]);
    for n in 0..CUSTOM_MAX {
        assert_eq!(
            store::custom_begin(&db, "codex", &ids, &format!("own-{n}")).unwrap(),
            Begun::Pending
        );
    }
    assert_eq!(
        store::custom_begin(&db, "codex", &ids, "one-more")
            .unwrap_err()
            .0,
        "model_choice_custom_full"
    );
    assert_eq!(
        store::custom_begin(&db, "codex", &ids, "own-0").unwrap(),
        Begun::Pending
    );
}

#[test]
fn a_stored_model_the_harness_no_longer_lists_is_passed_over() {
    let (_tmp, db) = engine();
    saved(
        set(
            &db,
            "codex",
            &["old", "new"],
            &Scope::App,
            Slot::Agent,
            json!("old"),
        )
        .unwrap(),
    );
    saved(
        set(
            &db,
            "codex",
            &["old", "new"],
            &Scope::App,
            Slot::Allowed,
            json!(["old", "new"]),
        )
        .unwrap(),
    );
    let models = store::values(&db, "codex", &catalog(&["new"]), &Scope::App, None).unwrap();
    let agent = row(&models, Slot::Agent);
    assert_eq!(agent.here, Some(json!("old")));
    assert_eq!(agent.effective, Value::Null);
    assert_eq!(agent.source, Source::Default);
    let allowed = row(&models, Slot::Allowed);
    assert_eq!(allowed.effective, json!(["new"]));
    assert_eq!(allowed.source, Source::App);
    assert_eq!(
        store::launch_model(&db, "codex", &catalog(&["new"]), None, Slot::Agent).unwrap(),
        None
    );
    let reset = store::reset(
        &db,
        "codex",
        &catalog(&["new"]),
        &Scope::App,
        Slot::Agent,
        1,
    );
    assert_eq!(saved(reset.unwrap()).here, None);
}

#[test]
fn a_spawn_pick_runs_only_from_the_allowed_list_or_the_default() {
    let (_tmp, db) = engine();
    let ids = ["gpt-a", "gpt-b", "gpt-c"];
    let project = ProjectId::new(PROJECT).unwrap();
    let pick =
        |picked: Option<&str>| store::pick(&db, "codex", &catalog(&ids), &project, picked).unwrap();
    assert_eq!(pick(None), Pick::Run(None));
    assert_eq!(
        pick(Some("gpt-a")),
        Pick::Refused {
            allowed: Vec::new(),
            default: None
        }
    );

    saved(set(&db, "codex", &ids, &Scope::App, Slot::Agent, json!("gpt-c")).unwrap());
    saved(
        set(
            &db,
            "codex",
            &ids,
            &Scope::App,
            Slot::Allowed,
            json!(["gpt-a", "gpt-b"]),
        )
        .unwrap(),
    );
    assert_eq!(pick(None), Pick::Run(Some("gpt-c".into())));
    assert_eq!(pick(Some("gpt-a")), Pick::Run(Some("gpt-a".into())));
    assert_eq!(pick(Some("gpt-c")), Pick::Run(Some("gpt-c".into())));
    assert_eq!(
        pick(Some("gpt-z")),
        Pick::Refused {
            allowed: vec!["gpt-a".into(), "gpt-b".into()],
            default: Some("gpt-c".into())
        }
    );

    saved(
        set(
            &db,
            "codex",
            &ids,
            &alpha(),
            Slot::Allowed,
            json!(["gpt-b"]),
        )
        .unwrap(),
    );
    assert_eq!(
        pick(Some("gpt-a")),
        Pick::Refused {
            allowed: vec!["gpt-b".into()],
            default: Some("gpt-c".into())
        }
    );
    assert_eq!(pick(Some("gpt-b")), Pick::Run(Some("gpt-b".into())));

    let rcli = store::pick(&db, "rcli", &catalog(&["local"]), &project, Some("local")).unwrap();
    assert_eq!(rcli, Pick::Unsupported);
    let none = store::pick(&db, "rcli", &catalog(&["local"]), &project, None).unwrap();
    assert_eq!(none, Pick::Run(None));
}

#[test]
#[should_panic(expected = "codex: bad id")]
fn a_catalog_with_an_id_no_launch_accepts_is_a_broken_caller() {
    let (_tmp, db) = engine();
    let _ = store::values(
        &db,
        "codex",
        &catalog(&["gpt-a", "bad id"]),
        &Scope::App,
        None,
    );
}

#[test]
#[should_panic(expected = "codex: gpt-a")]
fn a_catalog_with_a_repeated_id_is_a_broken_caller() {
    let (_tmp, db) = engine();
    let _ = store::values(
        &db,
        "codex",
        &catalog(&["gpt-a", "gpt-a"]),
        &Scope::App,
        None,
    );
}

#[test]
fn every_refusal_code_is_listed() {
    let (_tmp, db) = engine();
    let codes = [
        store::values(&db, "nope", &catalog(&["a"]), &Scope::App, None)
            .unwrap_err()
            .0,
        store::values(
            &db,
            "codex",
            &catalog(&["a"]),
            &Scope::Project(ProjectId::new("beta").unwrap()),
            None,
        )
        .unwrap_err()
        .0,
        store::custom_begin(&db, "codex", &catalog(&["a"]), "bad id")
            .unwrap_err()
            .0,
        store::custom_checked(&db, "codex", "never", true)
            .unwrap_err()
            .0,
    ];
    for code in codes {
        assert!(MODEL_CHOICE_CODES.contains(&code.as_str()), "{code}");
    }
    for code in [
        "model_choice_not_supported",
        "model_choice_custom_full",
        "model_choice_unavailable",
    ] {
        assert!(MODEL_CHOICE_CODES.contains(&code), "{code}");
    }
}

#[test]
fn each_launch_harness_ships_a_checked_model_catalog() {
    for harness in ["claude-code", "codex"] {
        let shipped = shipped(harness).unwrap();
        assert_eq!(shipped.harness, harness);
        assert!(!shipped.qualified_versions.is_empty());
        assert!(!shipped.sources.is_empty());
        assert!(!shipped.models.is_empty());
        assert_eq!(catalog(harness), shipped.models);
    }
    assert!(catalog("codex").contains(&"gpt-6.1-sol".to_owned()));
    assert!(catalog("claude-code").contains(&"opus[1m]".to_owned()));
    assert_eq!(catalog("claude"), catalog("claude-code"));
    assert!(shipped("rcli").is_none());
    assert!(catalog("rcli").is_empty());
    assert!(catalog("no-such-harness").is_empty());
}

#[test]
fn a_model_catalog_out_of_contract_is_refused() {
    let good = json!({
        "harness": "codex",
        "qualified_versions": ["0.164.0"],
        "sources": ["https://developers.openai.com/codex/models"],
        "models": ["gpt-6.1-sol", "gpt-6-luna"],
    });
    assert_eq!(
        parse_catalog(good.to_string().as_bytes()).unwrap().models,
        vec!["gpt-6.1-sol".to_owned(), "gpt-6-luna".to_owned()]
    );
    let broken = |field: &str, value: Value| {
        let mut catalog = good.clone();
        catalog[field] = value;
        catalog.to_string()
    };
    let too_many: Vec<String> = (0..=CATALOG_MODELS_MAX)
        .map(|n| format!("gpt-{n}"))
        .collect();
    for bad in [
        broken("harness", json!("rcli")),
        broken("harness", json!("no-such-harness")),
        broken("qualified_versions", json!([])),
        broken("qualified_versions", json!(["latest"])),
        broken("sources", json!([])),
        broken(
            "sources",
            json!(["http://developers.openai.com/codex/models"]),
        ),
        broken("models", json!([])),
        broken("models", json!(["-gpt"])),
        broken("models", json!(["opus[1m]"])),
        broken("models", json!(["gpt-6-luna", "gpt-6-luna"])),
        broken("models", json!(too_many)),
        broken("extra", json!(1)),
        "not json".to_owned(),
    ] {
        assert!(parse_catalog(bad.as_bytes()).is_none(), "{bad}");
    }
}

fn set_rule(db: &Connection, rule: rules::Rule) -> rules::Rules {
    let current = rules::rules(db).unwrap();
    let version = match rule {
        rules::Rule::Pick(_) => current.pick_version,
        rules::Rule::Fallback(_) => current.fallback_version,
    };
    match rules::set_rule(db, rule, version).unwrap() {
        rules::RuleWritten::Saved { rules } => rules,
        other => panic!("not saved: {other:?}"),
    }
}

#[test]
fn the_pick_and_fallback_rules_start_on_the_owner_s_picks_and_save_with_a_version() {
    use rules::{FallbackRule, PickRule, Rule, RuleWritten};
    let (_tmp, db) = engine();
    let first = rules::rules(&db).unwrap();
    assert_eq!(
        (first.pick, first.fallback),
        (PickRule::Strict, FallbackRule::Notice)
    );
    let saved = set_rule(&db, Rule::Pick(PickRule::Hint));
    assert_eq!(
        (saved.pick, saved.fallback),
        (PickRule::Hint, FallbackRule::Notice)
    );
    assert!(saved.pick_version > first.pick_version);
    assert_eq!(saved.fallback_version, first.fallback_version);
    let stale = rules::set_rule(&db, Rule::Pick(PickRule::Default), first.pick_version).unwrap();
    assert_eq!(
        stale,
        RuleWritten::Stale {
            rules: saved.clone()
        }
    );
    let saved = set_rule(&db, Rule::Fallback(FallbackRule::Stop));
    assert_eq!(
        (saved.pick, saved.fallback),
        (PickRule::Hint, FallbackRule::Stop)
    );
    assert_eq!(rules::rules(&db).unwrap(), saved);
    let wire = serde_json::to_value(Rule::Fallback(FallbackRule::Silent)).unwrap();
    assert_eq!(wire, json!({"rule": "fallback", "value": "silent"}));
}

#[test]
fn a_hint_rule_runs_any_usable_pick_and_a_default_rule_overrules_every_pick() {
    use rules::{PickRule, Rule};
    let (_tmp, db) = engine();
    let ids = ["gpt-a", "gpt-b", "gpt-c"];
    let project = ProjectId::new(PROJECT).unwrap();
    saved(
        set(
            &db,
            "codex",
            &ids,
            &Scope::App,
            Slot::Allowed,
            json!(["gpt-a"]),
        )
        .unwrap(),
    );
    saved(set(&db, "codex", &ids, &Scope::App, Slot::Agent, json!("gpt-c")).unwrap());
    let pick =
        |picked: Option<&str>| store::pick(&db, "codex", &catalog(&ids), &project, picked).unwrap();
    assert!(matches!(pick(Some("gpt-b")), Pick::Refused { .. }));
    assert_eq!(pick(Some("gpt-a")), Pick::Run(Some("gpt-a".into())));
    set_rule(&db, Rule::Pick(PickRule::Hint));
    assert_eq!(pick(Some("gpt-b")), Pick::Run(Some("gpt-b".into())));
    assert!(matches!(pick(Some("gpt-z")), Pick::Refused { .. }));
    assert_eq!(pick(None), Pick::Run(Some("gpt-c".into())));
    set_rule(&db, Rule::Pick(PickRule::Default));
    assert_eq!(pick(Some("gpt-a")), Pick::Overruled(Some("gpt-c".into())));
    assert_eq!(pick(Some("gpt-c")), Pick::Run(Some("gpt-c".into())));
    assert_eq!(pick(None), Pick::Run(Some("gpt-c".into())));
    assert_eq!(
        store::pick(&db, "rcli", &[], &project, Some("gpt-a")).unwrap(),
        Pick::Unsupported
    );
}

#[test]
fn a_claude_code_id_holds_brackets_only_as_the_catalog_s_1m_suffix() {
    for model in ["opus[1m]", "sonnet[1m]", "claude-opus-4-6[1m]"] {
        assert!(model_valid("claude-code", model), "{model}");
        assert!(!model_valid("codex", model), "{model}");
    }
    for model in [
        "opus[2m]",
        "op[1m]us",
        "opus[1m][1m]",
        "opus]",
        "opus[",
        "[1m]",
        "opus[]",
    ] {
        assert!(!model_valid("claude-code", model), "{model}");
    }
    for model in catalog("claude-code") {
        assert!(model_valid("claude-code", &model), "{model}");
    }
}

/// Removing a project clears its model choices and the team's model records for it, and
/// moves its choices' versions on (DEC-150 F6), so a project added again under the same
/// id inherits nothing and a screen read before the removal cannot save onto it.
#[test]
fn removing_a_project_clears_its_model_choices_so_a_reused_id_inherits_nothing() {
    let (_tmp, db) = engine();
    let project = ProjectId::new(PROJECT).unwrap();
    let ids = catalog(&["gpt-a", "gpt-b"]);
    let version = row(
        &store::values(&db, "codex", &ids, &alpha(), None).unwrap(),
        Slot::Master,
    )
    .version;
    let written = store::set(
        &db,
        "codex",
        &ids,
        &alpha(),
        Slot::Master,
        &json!("gpt-b"),
        version,
    )
    .unwrap();
    assert!(matches!(written, Written::Saved { .. }), "{written:?}");
    let saw = row(
        &store::values(&db, "codex", &ids, &alpha(), None).unwrap(),
        Slot::Master,
    )
    .version;
    crate::team::model_pick::record_agent(&db, &project, 7, Some("gpt-a")).unwrap();
    db.execute(
        "INSERT INTO team_model_fallbacks(project,harness,model,noticed_ms) VALUES (?1,'codex','gpt-a',1)",
        [PROJECT],
    )
    .unwrap();
    let account = crate::protocol::CredentialRef::new("account-a").unwrap();
    crate::team::model_pick::refuse(&db, &project, &account, "gpt-a", 1).unwrap();

    let config = crate::config::resolve_config_bytes("", "").unwrap();
    let tx = db.unchecked_transaction().unwrap();
    let outcome = crate::project_setup::execute(
        &tx,
        &config,
        &crate::project_setup::SetupAction::Remove {
            project_id: project.clone(),
        },
        2,
    )
    .unwrap();
    assert!(
        matches!(outcome, crate::protocol::Outcome::Applied),
        "{outcome:?}"
    );
    tx.commit().unwrap();
    db.execute("INSERT INTO projects(id, body) VALUES(?1,'{}')", [PROJECT])
        .unwrap();

    let after = store::values(&db, "codex", &ids, &alpha(), None).unwrap();
    assert_eq!(row(&after, Slot::Master).here, None, "nothing inherited");
    assert!(row(&after, Slot::Master).version > saw);
    let stale = store::set(
        &db,
        "codex",
        &ids,
        &alpha(),
        Slot::Master,
        &json!("gpt-a"),
        saw,
    )
    .unwrap();
    assert!(matches!(stale, Written::Stale { .. }), "{stale:?}");
    for table in [
        "team_agent_models",
        "team_model_fallbacks",
        "team_model_refusals",
    ] {
        let left: i64 = db
            .query_row(
                &format!("SELECT COUNT(*) FROM {table} WHERE project=?1"),
                [PROJECT],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(left, 0, "{table}");
    }
}

/// A shipped catalog names the harness versions it was checked on, and an installed
/// version it does not name, or one the engine could not read, shows why on the Models
/// section without taking the catalog ids away (DEC-150 G1).
#[test]
fn a_shipped_catalog_names_the_versions_it_was_checked_on_and_an_unchecked_install_shows_why() {
    for harness in ["claude-code", "codex"] {
        let shipped = shipped(harness).unwrap();
        let name = crate::harnesses::spec(harness).unwrap().name;
        let listed = shipped.qualified_versions.join(", ");
        for version in &shipped.qualified_versions {
            assert_eq!(unqualified(harness, Some(version)), None, "{harness}");
        }
        assert_eq!(
            unqualified(harness, Some("999.0.0")),
            Some(format!(
                "listed for {name} {listed}; installed 999.0.0 not yet checked"
            ))
        );
        assert_eq!(
            unqualified(harness, None),
            Some(format!(
                "listed for {name} {listed}; installed version unknown"
            ))
        );
    }
}

/// The Models section says whether the installed version was checked, and a catalog id
/// still saves and stays usable on a version the catalog does not name: qualification is
/// recorded, not enforced (DEC-150 G1).
#[test]
fn the_models_section_says_whether_the_installed_version_was_checked_and_still_saves_a_listed_id() {
    let (_tmp, db) = engine();
    let shipped = shipped("codex").unwrap();
    let ids = shipped.models.clone();
    let checked = shipped.qualified_versions[0].as_str();
    let models = store::values(&db, "codex", &ids, &Scope::App, Some(checked)).unwrap();
    assert!(models.qualified && models.reason.is_none(), "{models:?}");

    let models = store::values(&db, "codex", &ids, &Scope::App, Some("999.0.0")).unwrap();
    assert!(!models.qualified, "{models:?}");
    assert_eq!(models.reason, unqualified("codex", Some("999.0.0")));
    assert!(
        models
            .candidates
            .iter()
            .all(|candidate| candidate.origin == Origin::Catalog),
        "{models:?}"
    );
    let version = row(&models, Slot::Agent).version;
    let written = store::set(
        &db,
        "codex",
        &ids,
        &Scope::App,
        Slot::Agent,
        &json!(ids[0]),
        version,
    )
    .unwrap();
    assert_eq!(saved(written).effective, json!(ids[0]));
    assert_eq!(
        store::launch_model(&db, "codex", &ids, None, Slot::Agent).unwrap(),
        Some(ids[0].clone())
    );
}
