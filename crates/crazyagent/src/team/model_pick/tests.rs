use super::*;
use crate::model_choice::store::{Begun, custom_begin, custom_checked, set};
use crate::model_choice::{Slot, Written};
use crate::protocol::CredentialRef;
use crate::team::TeamReach;
use serde_json::json;

const PROJECT: &str = "models";

/// An engine database with the authority, team and event tables, in a private temporary
/// folder. Output: the folder, kept alive by the caller, and the connection.
fn engine() -> (tempfile::TempDir, Connection) {
    let tmp = tempfile::Builder::new()
        .prefix("ca-model-pick-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    let db = Connection::open(tmp.path().join("engine.db")).unwrap();
    crate::storage::initialize_authority_schemas(&db).unwrap();
    super::super::initialize(&db).unwrap();
    super::super::events::initialize(&db).unwrap();
    (tmp, db)
}

fn project() -> ProjectId {
    ProjectId::new(PROJECT).unwrap()
}

/// Adds the owner's verified ids for Codex, ticks the allowed list and sets the agent
/// default at app scope. Takes the database, the ids, the ticked ids and the default.
fn owner_models(db: &Connection, ids: &[&str], allowed: &[&str], agent: Option<&str>) {
    let catalog = crate::model_choice::catalog("codex");
    for id in ids {
        assert_eq!(
            custom_begin(db, "codex", &catalog, id).unwrap(),
            Begun::Pending
        );
        custom_checked(db, "codex", id, true).unwrap();
    }
    let scope = crate::harness_settings::Scope::App;
    let mut sets = vec![(Slot::Allowed, json!(allowed))];
    if let Some(agent) = agent {
        sets.push((Slot::Agent, json!(agent)));
    }
    for (slot, value) in sets {
        let written = set(db, "codex", &catalog, &scope, slot, &value, 0).unwrap();
        assert!(matches!(written, Written::Saved { .. }), "{written:?}");
    }
}

fn task(db: &Connection, prompt: &str) -> i64 {
    db.execute(
        "INSERT INTO team_tasks(project,source,harness,tools,prompt,status,created_ms) VALUES (?1,'master','codex',1,?2,'queued',1)",
        params![PROJECT, prompt],
    )
    .unwrap();
    db.last_insert_rowid()
}

fn pick(model: &str) -> ModelPick {
    ModelPick {
        model: model.into(),
        reason: format!("{model} fits"),
    }
}

#[test]
fn a_task_wants_its_pinned_pick_else_the_owner_s_default() {
    let (_tmp, mut db) = engine();
    let plain = task(&db, "Plain");
    assert_eq!(wanted(&db, plain, &project(), "codex").unwrap(), None);
    owner_models(&db, &["gpt-a", "gpt-b"], &["gpt-a"], Some("gpt-b"));
    assert_eq!(
        wanted(&db, plain, &project(), "codex").unwrap(),
        Some("gpt-b".to_owned())
    );
    let picked = task(&db, "Picked");
    let tx = db.transaction().unwrap();
    assert_eq!(
        pin(&tx, &project(), picked, "codex", Some(&pick("gpt-a"))).unwrap(),
        Ok(" on gpt-a".to_owned())
    );
    assert_eq!(
        pin(&tx, &project(), plain, "codex", None).unwrap(),
        Ok(String::new())
    );
    tx.commit().unwrap();
    assert_eq!(pinned(&db, picked).unwrap(), Some(pick("gpt-a")));
    assert_eq!(pinned(&db, plain).unwrap(), None);
    assert_eq!(
        wanted(&db, picked, &project(), "codex").unwrap(),
        Some("gpt-a".to_owned())
    );
    assert_eq!(wanted(&db, plain, &project(), "claude-code").unwrap(), None);
}

#[test]
fn a_pin_of_a_task_the_project_no_longer_keeps_is_dropped_at_the_next_pin() {
    let (_tmp, mut db) = engine();
    owner_models(&db, &["gpt-a"], &["gpt-a"], None);
    let first = task(&db, "First");
    let tx = db.transaction().unwrap();
    pin(&tx, &project(), first, "codex", Some(&pick("gpt-a")))
        .unwrap()
        .unwrap();
    tx.commit().unwrap();
    db.execute("DELETE FROM team_tasks WHERE id=?1", [first])
        .unwrap();
    let second = task(&db, "Second");
    let tx = db.transaction().unwrap();
    pin(&tx, &project(), second, "codex", Some(&pick("gpt-a")))
        .unwrap()
        .unwrap();
    tx.commit().unwrap();
    assert_eq!(pinned(&db, first).unwrap(), None);
    assert_eq!(pinned(&db, second).unwrap(), Some(pick("gpt-a")));
}

#[test]
fn an_endpoint_runs_only_its_own_model_and_a_managed_profile_any() {
    let endpoint = TeamReach::Endpoint {
        base_url: "http://127.0.0.1:11434/v1".into(),
        model: "qwen3:32b".into(),
        key: None,
    };
    let managed = TeamReach::Managed {
        profile_ref: CredentialRef::new("codex-profile").unwrap(),
        model: None,
    };
    assert!(can_run(&endpoint, None));
    assert!(can_run(&endpoint, Some("qwen3:32b")));
    assert!(!can_run(&endpoint, Some("gpt-a")));
    assert!(can_run(&managed, None));
    assert!(can_run(&managed, Some("gpt-a")));
}

#[test]
fn an_agent_keeps_the_model_it_started_on_until_it_runs_the_default() {
    let (_tmp, db) = engine();
    assert_eq!(agent_model(&db, 7).unwrap(), None);
    record_agent(&db, &project(), 7, Some("gpt-a")).unwrap();
    record_agent(&db, &project(), 8, Some("gpt-b")).unwrap();
    assert_eq!(agent_model(&db, 7).unwrap(), Some("gpt-a".to_owned()));
    record_agent(&db, &project(), 7, Some("gpt-c")).unwrap();
    assert_eq!(agent_model(&db, 7).unwrap(), Some("gpt-c".to_owned()));
    record_agent(&db, &project(), 7, None).unwrap();
    assert_eq!(agent_model(&db, 7).unwrap(), None);
    assert_eq!(agent_model(&db, 8).unwrap(), Some("gpt-b".to_owned()));
}

#[test]
#[should_panic]
fn an_agent_record_needs_a_real_agent_id() {
    let (_tmp, db) = engine();
    let _ = record_agent(&db, &project(), 0, Some("gpt-a"));
}

#[test]
fn a_fallback_is_told_once_to_the_owner_and_the_master_until_the_model_runs_again() {
    let (_tmp, mut db) = engine();
    let told = |db: &mut Connection, task: i64| {
        let tx = db.transaction().unwrap();
        let told = fell_back(
            &tx,
            &project(),
            "codex",
            "gpt-a",
            Fallen::Task(task),
            1_000 + task,
        )
        .unwrap();
        tx.commit().unwrap();
        told
    };
    assert!(told(&mut db, 1));
    assert!(!told(&mut db, 2));
    let other = {
        let tx = db.transaction().unwrap();
        let told = fell_back(&tx, &project(), "codex", "gpt-b", Fallen::Task(3), 1_003).unwrap();
        tx.commit().unwrap();
        told
    };
    assert!(other);
    let master = {
        let tx = db.transaction().unwrap();
        let told = fell_back(
            &tx,
            &project(),
            "claude-code",
            "opus",
            Fallen::Master,
            1_004,
        )
        .unwrap();
        tx.commit().unwrap();
        told
    };
    assert!(master);
    let notices: Vec<(String, String)> = db
        .prepare("SELECT kind,text FROM owner_notices WHERE project=?1 AND subject LIKE 'model_fallback:%' ORDER BY id")
        .unwrap()
        .query_map([PROJECT], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(notices.len(), 3);
    assert!(notices.iter().all(|(kind, _)| kind == "pool_failover"));
    assert_eq!(
        notices[0].1,
        "No account in this project's pool can run gpt-a for codex agents now, so task 1 runs on the default model. You are told again only after gpt-a runs once more."
    );
    let messages: Vec<(String, String, String)> = db
        .prepare("SELECT author,state,text FROM team_messages WHERE project=?1 ORDER BY id")
        .unwrap()
        .query_map([PROJECT], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(messages.len(), 3);
    assert!(
        messages
            .iter()
            .all(|(author, state, text)| author == "engine"
                && state == "pending"
                && text.ends_with("(model_fallback)"))
    );
    ran(&db, &project(), "codex", "gpt-a").unwrap();
    assert!(told(&mut db, 4));
    assert!(!told(&mut db, 5));
}

#[test]
fn a_master_fallback_names_the_master_instead_of_a_task() {
    let (_tmp, mut db) = engine();
    let tx = db.transaction().unwrap();
    assert!(fell_back(&tx, &project(), "codex", "gpt-a", Fallen::Master, 1_000).unwrap());
    tx.commit().unwrap();
    let text: String = db
        .query_row(
            "SELECT text FROM team_notifications WHERE project=?1",
            [PROJECT],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        text,
        "No account in this project's pool can run gpt-a for the codex master now, so the master runs on the default model. You are told again only after gpt-a runs once more."
    );
}

#[test]
fn an_agent_starting_again_with_no_task_is_named_neither_a_task_nor_the_master() {
    let (_tmp, mut db) = engine();
    let tx = db.transaction().unwrap();
    assert!(fell_back(&tx, &project(), "codex", "gpt-a", Fallen::Agent, 1_000).unwrap());
    tx.commit().unwrap();
    let text: String = db
        .query_row(
            "SELECT text FROM team_notifications WHERE project=?1",
            [PROJECT],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        text,
        "No account in this project's pool can run gpt-a for codex agents now, so an agent starting again runs on the default model. You are told again only after gpt-a runs once more."
    );
}

#[test]
fn a_master_starts_on_the_owner_s_master_model_and_an_agent_on_its_record() {
    let (_tmp, db) = engine();
    assert_eq!(
        start_model(&db, &project(), 1, "codex", true).unwrap(),
        None
    );
    owner_models(&db, &["gpt-a", "gpt-b"], &["gpt-b"], None);
    let catalog = crate::model_choice::catalog("codex");
    let scope = crate::harness_settings::Scope::App;
    let written = set(
        &db,
        "codex",
        &catalog,
        &scope,
        Slot::Master,
        &json!("gpt-a"),
        0,
    )
    .unwrap();
    assert!(matches!(written, Written::Saved { .. }), "{written:?}");
    record_agent(&db, &project(), 1, Some("gpt-b")).unwrap();
    record_agent(&db, &project(), 2, Some("gpt-b")).unwrap();
    assert_eq!(
        start_model(&db, &project(), 1, "codex", true).unwrap(),
        Some("gpt-a".to_owned())
    );
    assert_eq!(
        start_model(&db, &project(), 2, "codex", false).unwrap(),
        Some("gpt-b".to_owned())
    );
    assert_eq!(
        start_model(&db, &project(), 3, "codex", false).unwrap(),
        None
    );
    assert_eq!(start_model(&db, &project(), 1, "rcli", true).unwrap(), None);
}

#[test]
fn a_hint_rule_pins_an_unticked_pick_and_a_default_rule_runs_the_default_and_says_so() {
    use crate::model_choice::rules::{self, PickRule, Rule, RuleWritten};
    let (_tmp, mut db) = engine();
    owner_models(&db, &["gpt-a", "gpt-b", "gpt-c"], &["gpt-a"], Some("gpt-b"));
    let rule = |db: &Connection, rule: PickRule| {
        let version = rules::rules(db).unwrap().pick_version;
        let written = rules::set_rule(db, Rule::Pick(rule), version).unwrap();
        assert!(matches!(written, RuleWritten::Saved { .. }), "{written:?}");
    };
    let pin_one = |db: &mut Connection, prompt: &str, model: &str| {
        let id = task(db, prompt);
        let tx = db.transaction().unwrap();
        let said = pin(&tx, &project(), id, "codex", Some(&pick(model))).unwrap();
        tx.commit().unwrap();
        (id, said)
    };
    rule(&db, PickRule::Hint);
    let (hinted, said) = pin_one(&mut db, "Hinted", "gpt-c");
    assert_eq!(said, Ok(" on gpt-c".to_owned()));
    assert_eq!(pinned(&db, hinted).unwrap(), Some(pick("gpt-c")));
    rule(&db, PickRule::Default);
    let (overruled, said) = pin_one(&mut db, "Overruled", "gpt-a");
    assert_eq!(
        said,
        Ok(" on the default model gpt-b: the owner runs every agent on the default".to_owned())
    );
    assert_eq!(pinned(&db, overruled).unwrap(), None);
    assert_eq!(
        wanted(&db, overruled, &project(), "codex").unwrap(),
        Some("gpt-b".to_owned())
    );
}

#[test]
fn a_pin_or_record_the_owner_since_took_off_the_list_starts_on_the_default() {
    let (_tmp, mut db) = engine();
    owner_models(&db, &["gpt-a", "gpt-b"], &["gpt-a"], Some("gpt-b"));
    let id = task(&db, "Picked");
    let tx = db.transaction().unwrap();
    assert_eq!(
        pin(&tx, &project(), id, "codex", Some(&pick("gpt-a"))).unwrap(),
        Ok(" on gpt-a".to_owned())
    );
    tx.commit().unwrap();
    record_agent(&db, &project(), 4, Some("gpt-a")).unwrap();
    let starts = |db: &Connection| {
        (
            wanted(db, id, &project(), "codex").unwrap(),
            start_model(db, &project(), 4, "codex", false).unwrap(),
        )
    };
    let on = |model: &str| Some(model.to_owned());
    assert_eq!(starts(&db), (on("gpt-a"), on("gpt-a")));
    let catalog = crate::model_choice::catalog("codex");
    let scope = crate::harness_settings::Scope::App;
    let version = crate::model_choice::store::values(&db, "codex", &catalog, &scope, None)
        .unwrap()
        .rows
        .iter()
        .find(|row| row.slot == Slot::Allowed)
        .unwrap()
        .version;
    let written = set(
        &db,
        "codex",
        &catalog,
        &scope,
        Slot::Allowed,
        &json!([]),
        version,
    )
    .unwrap();
    assert!(matches!(written, Written::Saved { .. }), "{written:?}");
    assert_eq!(starts(&db), (on("gpt-b"), on("gpt-b")));
    assert!(crate::model_choice::store::custom_remove(&db, "codex", "gpt-b").unwrap());
    assert_eq!(starts(&db), (None, None));
    assert_eq!(pinned(&db, id).unwrap(), Some(pick("gpt-a")));
    assert_eq!(agent_model(&db, 4).unwrap(), on("gpt-a"));
}

#[test]
fn a_model_refusal_holds_for_its_project_connection_and_model_until_it_expires() {
    let (_tmp, db) = engine();
    let a = CredentialRef::new("account-a").unwrap();
    let b = CredentialRef::new("account-b").unwrap();
    let other = ProjectId::new("other").unwrap();
    assert!(!refused(&db, &project(), &a, "gpt-a", 1_000).unwrap());
    refuse(&db, &project(), &a, "gpt-a", 1_000).unwrap();
    assert!(refused(&db, &project(), &a, "gpt-a", 1_001).unwrap());
    assert!(!refused(&db, &project(), &a, "gpt-b", 1_001).unwrap());
    assert!(!refused(&db, &project(), &b, "gpt-a", 1_001).unwrap());
    assert!(!refused(&db, &other, &a, "gpt-a", 1_001).unwrap());
    assert!(!refused(&db, &project(), &a, "gpt-a", 1_000 + MODEL_REFUSAL_MS).unwrap());
    let extra = 4;
    for n in 0..(REFUSALS_MAX + extra) {
        refuse(&db, &project(), &a, &format!("m-{n}"), 2_000 + n as i64).unwrap();
    }
    let kept: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM team_model_refusals WHERE project=?1",
            [PROJECT],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(kept, REFUSALS_MAX as i64);
    let last = format!("m-{}", REFUSALS_MAX + extra - 1);
    assert!(refused(&db, &project(), &a, &last, 3_000).unwrap());
    assert!(!refused(&db, &project(), &a, "m-0", 3_000).unwrap());
}

/// The engine's check of a model refusal a pane said runs at most once per account and
/// model in `MODEL_REFUSAL_MS` (DEC-150 addendum 9g, 9h): the first claim runs, a later one
/// is held, one whose check refused answers refused with the check's own time, and a
/// claim runs again once the last one ended.
#[test]
fn a_model_check_runs_once_per_account_and_model_in_its_window() {
    let (_tmp, db) = engine();
    let a = CredentialRef::new("account-a").unwrap();
    let b = CredentialRef::new("account-b").unwrap();
    let other = ProjectId::new("other").unwrap();
    assert_eq!(
        claim_check(&db, &a, "gpt-a", 1_000, false).unwrap(),
        Claim::Run
    );
    assert_eq!(
        claim_check(&db, &a, "gpt-a", 1_001, false).unwrap(),
        Claim::Held
    );
    assert_eq!(
        claim_check(&db, &a, "gpt-b", 1_001, false).unwrap(),
        Claim::Run
    );
    assert_eq!(
        claim_check(&db, &b, "gpt-a", 1_001, false).unwrap(),
        Claim::Run
    );
    check_refused(&db, &other, &a, "gpt-a", 2_000).unwrap();
    assert!(refused(&db, &other, &a, "gpt-a", 2_001).unwrap());
    assert!(!refused(&db, &project(), &a, "gpt-a", 2_001).unwrap());
    assert_eq!(
        claim_check(&db, &a, "gpt-a", 3_000, false).unwrap(),
        Claim::Refused { checked_ms: 1_000 }
    );
    assert_eq!(
        claim_check(&db, &a, "gpt-b", 3_000, false).unwrap(),
        Claim::Held,
        "only the refused model answers refused"
    );
    assert_eq!(
        claim_check(&db, &a, "gpt-a", 1_000 + MODEL_REFUSAL_MS, false).unwrap(),
        Claim::Run
    );
    assert_eq!(
        claim_check(&db, &a, "gpt-a", 1_001 + MODEL_REFUSAL_MS, false).unwrap(),
        Claim::Held,
        "a new claim starts unrefused"
    );
}

/// Live model checks are never dropped (DEC-150 addendum 9h): with `CHECKS_MAX` live
/// claims a new account and model is held with no row, the oldest live claim still
/// holds, and a claim past its end makes room.
#[test]
fn a_full_table_of_live_model_checks_holds_a_new_one_and_drops_none() {
    let (_tmp, db) = engine();
    let a = CredentialRef::new("account-a").unwrap();
    for n in 0..CHECKS_MAX {
        let model = format!("m-{n}");
        assert_eq!(
            claim_check(&db, &a, &model, 1_000 + n as i64, false).unwrap(),
            Claim::Run
        );
    }
    let rows = || -> i64 {
        db.query_row("SELECT COUNT(*) FROM team_model_checks", [], |row| {
            row.get(0)
        })
        .unwrap()
    };
    assert_eq!(
        claim_check(&db, &a, "m-new", 2_000, false).unwrap(),
        Claim::Held
    );
    assert_eq!(rows(), CHECKS_MAX as i64);
    assert_eq!(
        claim_check(&db, &a, "m-0", 2_000, false).unwrap(),
        Claim::Held
    );
    assert_eq!(
        claim_check(&db, &a, "m-new", 1_000 + MODEL_REFUSAL_MS, false).unwrap(),
        Claim::Run,
        "the claim on m-0 ended and made room"
    );
    assert_eq!(rows(), CHECKS_MAX as i64);
}

#[test]
fn the_master_is_told_each_harness_s_models_as_the_pick_rule_offers_them() {
    use crate::model_choice::rules::{self, PickRule, Rule, RuleWritten};
    let (_tmp, db) = engine();
    owner_models(&db, &["gpt-a", "gpt-b"], &["gpt-a"], Some("gpt-b"));
    let lines = || master_lines(&db, &project(), &["codex", "rcli", "codex"]).unwrap();
    assert_eq!(
        lines(),
        format!("{MODELS_HEAD}\n- codex: gpt-a; leave model out for the default, gpt-b\n")
    );
    let rule = |pick: PickRule| {
        let version = rules::rules(&db).unwrap().pick_version;
        let written = rules::set_rule(&db, Rule::Pick(pick), version).unwrap();
        assert!(matches!(written, RuleWritten::Saved { .. }), "{written:?}");
    };
    rule(PickRule::Hint);
    let catalog = crate::model_choice::catalog("codex").join(", ");
    assert_eq!(
        lines(),
        format!(
            "{MODELS_HEAD}\n- codex: {catalog}, gpt-a, gpt-b; leave model out for the default, gpt-b\n"
        )
    );
    rule(PickRule::Default);
    assert_eq!(
        lines(),
        format!("{MODELS_HEAD}\n- codex: leave model out; tasks run the default, gpt-b\n")
    );
    assert_eq!(master_lines(&db, &project(), &["rcli"]).unwrap(), "");
}
