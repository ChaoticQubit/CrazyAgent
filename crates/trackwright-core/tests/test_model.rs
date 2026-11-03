use rusqlite::{Connection, Transaction, TransactionBehavior};
use std::path::Path;
use trackwright_core::events;
use trackwright_core::model::{Actor, Priority, ProjectKey, ShortId, TicketKind, TicketStatus};
use trackwright_core::schema;
use trackwright_core::store::{self, NewTicket, Store, StoreError};
use trackwright_core::tests_model::{
    CaseDetail, CaseKind, CaseProtectedVisibility, CaseStatus, CriterionRef, PlanItemRef,
    PlanItemView, PlanStatus, ResultOutcome, StepsView, TestId, TestObject, case_view, codes,
};
use trackwright_core::tests_store::{self, CaseChange, NewCase, PlanFields};

const V3A_TABLES: &[&str] = &[
    "tk_checks",
    "tk_criteria",
    "tk_cycles",
    "tk_deps",
    "tk_events",
    "tk_evidence",
    "tk_links",
    "tk_policy",
    "tk_projects",
    "tk_requests",
    "tk_tickets",
    "tk_versions",
];

const TEST_TABLES: &[&str] = &[
    "tk_case_criteria",
    "tk_cases",
    "tk_plan_items",
    "tk_plans",
    "tk_results",
    "tk_runs",
    "tk_set_cases",
    "tk_sets",
];

fn actor(name: &str) -> Actor {
    Actor::new(name).unwrap()
}

fn key(text: &str) -> ProjectKey {
    ProjectKey::new(text).unwrap()
}

fn opened(dir: &Path) -> Store {
    Store::open(&dir.join("tracker").join("tracker.db")).unwrap()
}

fn new_ticket(title: &str) -> NewTicket {
    NewTicket {
        title: title.to_owned(),
        body: String::new(),
        kind: TicketKind::Feature,
        status: TicketStatus::Backlog,
        priority: Priority::new(2).unwrap(),
    }
}

fn new_case(title: &str, stable_name: Option<&str>) -> NewCase {
    NewCase {
        title: title.to_owned(),
        kind: CaseKind::Unit,
        stable_name: stable_name.map(str::to_owned),
        steps: vec![
            "Arrange a store\nwith two tickets".to_owned(),
            "Assert".to_owned(),
        ],
        protected: false,
        status: CaseStatus::Ready,
    }
}

fn refused<T: std::fmt::Debug>(result: Result<T, StoreError>) -> &'static str {
    match result {
        Err(StoreError::Refused(refusal)) => refusal.code,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

fn rows(tx: &Transaction<'_>, table: &str) -> Vec<Vec<String>> {
    let mut statement = tx.prepare(&format!("SELECT * FROM {table}")).unwrap();
    let columns = statement.column_count();
    let mut rows: Vec<Vec<String>> = statement
        .query_map([], |row| {
            (0..columns)
                .map(|at| row.get_ref(at).map(|value| format!("{value:?}")))
                .collect()
        })
        .unwrap()
        .map(Result::unwrap)
        .collect();
    rows.sort();
    rows
}

fn count(tx: &Transaction<'_>, table: &str) -> i64 {
    tx.query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
        row.get(0)
    })
    .unwrap()
}

fn schema_step(tx: &Transaction<'_>) -> i64 {
    tx.query_row(
        "SELECT value FROM tk_meta WHERE key = 'schema_version'",
        [],
        |row| row.get(0),
    )
    .unwrap()
}

fn master(tx: &Transaction<'_>) -> Vec<(String, String, String)> {
    let mut statement = tx
        .prepare("SELECT type, name, coalesce(sql, '') FROM sqlite_master ORDER BY type, name")
        .unwrap();
    statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

fn private_file(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    #[cfg(not(unix))]
    assert!(path.is_file());
}

fn write_v3a_file(path: &Path, owner: &Actor) {
    let mut connection = Connection::open(path).unwrap();
    private_file(path);
    connection
        .pragma_update(None, "foreign_keys", true)
        .unwrap();
    let tx = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    schema::migrate_to(&tx, 1).unwrap();
    store::create_project(&tx, owner, &key("CA"), "CrazyAgent", 1_000).unwrap();
    let ticket =
        store::create_ticket(&tx, owner, &key("CA"), &new_ticket("Link tests"), 1_001).unwrap();
    store::create_ticket(&tx, owner, &key("CA"), &new_ticket("Second"), 1_002).unwrap();
    store::add_criterion(&tx, owner, &ticket.short_id, "linked tests pass", 1_003).unwrap();
    store::add_link(
        &tx,
        owner,
        &ticket.short_id,
        "branch",
        "feat/v3b-vt1-model",
        1_004,
    )
    .unwrap();
    assert_eq!(schema_step(&tx), 1, "the file is a V3a file");
    tx.commit().unwrap();
}

fn project_id(tx: &Transaction<'_>, key: &str) -> i64 {
    tx.query_row("SELECT id FROM tk_projects WHERE key = ?1", [key], |row| {
        row.get(0)
    })
    .unwrap()
}

fn row_id(tx: &Transaction<'_>, table: &str, key: &str, number: u32) -> i64 {
    let sql = format!(
        "SELECT t.id FROM {table} t JOIN tk_projects p ON p.id = t.project \
         WHERE p.key = ?1 AND t.number = ?2"
    );
    tx.query_row(&sql, rusqlite::params![key, number], |row| row.get(0))
        .unwrap()
}

fn insert_finished_run(tx: &Transaction<'_>, project: i64, case_id: i64, outcome: &str) {
    tx.execute(
        "INSERT INTO tk_runs(project, number, title, state, verdict, source_json, policy_json,
             started_ms, finished_ms, recorded_by, total, passed, failed, skipped, errored,
             version)
         VALUES (?1, 1, 'unit', 'finished', 'failed', '{\"kind\":\"declared\",\"ref\":\"ci\"}',
             '{}', 5000, 5001, 'ci', 1, 0, 1, 0, 0, 1)",
        [project],
    )
    .unwrap();
    let run = tx.last_insert_rowid();
    tx.execute(
        "INSERT INTO tk_results(run_id, position, case_id, name, outcome)
         VALUES (?1, 1, ?2, 'store::tests::protected', ?3)",
        rusqlite::params![run, case_id, outcome],
    )
    .unwrap();
}

#[test]
fn migrate_step_two_on_a_v3a_database_keeps_every_ticket_and_adds_the_test_tables() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tracker.db");
    let owner = actor("owner");
    write_v3a_file(&path, &owner);

    let mut connection = Connection::open(&path).unwrap();
    let tx = connection.transaction().unwrap();
    let before: Vec<Vec<Vec<String>>> = V3A_TABLES.iter().map(|t| rows(&tx, t)).collect();
    assert!(before.iter().all(|table| table.len() <= 64));
    assert_eq!(rows(&tx, "tk_tickets").len(), 2);
    assert_eq!(schema_step(&tx), 1);
    drop(tx);
    drop(connection);

    let mut store = Store::open(&path).unwrap();
    let tx = store.transaction().unwrap();
    assert_eq!(schema_step(&tx), 2);
    assert_eq!(schema::SCHEMA_VERSION, 2);
    let after: Vec<Vec<Vec<String>>> = V3A_TABLES.iter().map(|t| rows(&tx, t)).collect();
    assert_eq!(after, before, "step 2 leaves every V3a row as it was");
    for table in TEST_TABLES {
        assert_eq!(count(&tx, table), 0, "{table} starts empty");
    }
    assert_eq!(
        events::replay(&tx, &key("CA")).unwrap(),
        store::state(&tx, &key("CA")).unwrap()
    );

    let case =
        tests_store::create_case(&tx, &owner, &key("CA"), &new_case("Links", None), 2_000).unwrap();
    let criterion = CriterionRef {
        ticket: ShortId::parse("CA-1").unwrap(),
        position: 1,
    };
    let linked =
        tests_store::link_case(&tx, &owner, &case.short_id, case.version, &criterion, 2_001)
            .unwrap();
    assert_eq!(linked.version, case.version + 1);
    let shown = tests_store::case_show(&tx, &owner, &case.short_id).unwrap();
    assert_eq!(shown.detail.unwrap().criteria, [criterion]);

    let once = master(&tx);
    schema::migrate(&tx).unwrap();
    assert_eq!(master(&tx), once, "a second migrate changes nothing");
    assert_eq!(
        events::replay(&tx, &key("CA")).unwrap(),
        store::state(&tx, &key("CA")).unwrap(),
        "the ticket replay skips test events"
    );
    tx.commit().unwrap();
}

#[test]
fn case_events_replay_to_the_current_cases() {
    let dir = tempfile::tempdir().unwrap();
    let owner = actor("owner");
    let agent = actor("agent:w1");
    {
        let mut store = opened(dir.path());
        let tx = store.transaction().unwrap();
        store::create_project(&tx, &owner, &key("CA"), "CrazyAgent", 1_000).unwrap();
        let ticket =
            store::create_ticket(&tx, &owner, &key("CA"), &new_ticket("Tests"), 1_001).unwrap();
        store::add_criterion(&tx, &owner, &ticket.short_id, "replays", 1_002).unwrap();
        let criterion = CriterionRef {
            ticket: ticket.short_id.clone(),
            position: 1,
        };
        let first = tests_store::create_case(
            &tx,
            &agent,
            &key("CA"),
            &new_case("First", Some("store::first")),
            1_003,
        )
        .unwrap();
        let second =
            tests_store::create_case(&tx, &owner, &key("CA"), &new_case("Second", None), 1_004)
                .unwrap();
        assert_eq!(first.short_id.to_string(), "CA-T1");
        assert_eq!(second.short_id.to_string(), "CA-T2");
        let first = tests_store::edit_case(
            &tx,
            &agent,
            &first.short_id,
            first.version,
            &CaseChange {
                title: Some("First, renamed".to_owned()),
                kind: Some(CaseKind::Integration),
                ..CaseChange::default()
            },
            1_005,
        )
        .unwrap();
        let first = tests_store::link_case(
            &tx,
            &agent,
            &first.short_id,
            first.version,
            &criterion,
            1_006,
        )
        .unwrap();
        let second = tests_store::link_case(
            &tx,
            &owner,
            &second.short_id,
            second.version,
            &criterion,
            1_007,
        )
        .unwrap();
        tests_store::unlink_case(
            &tx,
            &owner,
            &second.short_id,
            second.version,
            &criterion,
            1_008,
        )
        .unwrap();
        let set = tests_store::upsert_set(&tx, &owner, &key("CA"), None, "Smoke", 1_009).unwrap();
        assert_eq!(set.short_id.to_string(), "CA-S1");
        let set = tests_store::set_add(
            &tx,
            &owner,
            &set.short_id,
            set.version,
            &first.short_id,
            1_010,
        )
        .unwrap();
        let set = tests_store::set_add(
            &tx,
            &owner,
            &set.short_id,
            set.version,
            &second.short_id,
            1_011,
        )
        .unwrap();
        let set = tests_store::set_remove(
            &tx,
            &owner,
            &set.short_id,
            set.version,
            &first.short_id,
            1_012,
        )
        .unwrap();
        let set = tests_store::upsert_set(
            &tx,
            &owner,
            &key("CA"),
            Some((&set.short_id, set.version)),
            "Smoke checks",
            1_013,
        )
        .unwrap();
        let fields = PlanFields {
            name: "Release 1".to_owned(),
            release: None,
            cycle: None,
            status: PlanStatus::Draft,
        };
        let plan = tests_store::upsert_plan(&tx, &owner, &key("CA"), None, &fields, 1_014).unwrap();
        assert_eq!(plan.short_id.to_string(), "CA-P1");
        let by_case = PlanItemRef::Case(first.short_id.clone());
        let by_set = PlanItemRef::Set(set.short_id.clone());
        let plan =
            tests_store::plan_add(&tx, &owner, &plan.short_id, plan.version, &by_case, 1_015)
                .unwrap();
        let plan = tests_store::plan_add(&tx, &owner, &plan.short_id, plan.version, &by_set, 1_016)
            .unwrap();
        let plan =
            tests_store::plan_remove(&tx, &owner, &plan.short_id, plan.version, &by_case, 1_017)
                .unwrap();
        let active = PlanFields {
            status: PlanStatus::Active,
            ..fields
        };
        tests_store::upsert_plan(
            &tx,
            &owner,
            &key("CA"),
            Some((&plan.short_id, plan.version)),
            &active,
            1_018,
        )
        .unwrap();
        tx.commit().unwrap();
    }

    let mut store = opened(dir.path());
    let tx = store.transaction().unwrap();
    let current = tests_store::state(&tx, &key("CA")).unwrap();
    let replayed = tests_store::replay(&tx, &key("CA")).unwrap();
    assert_eq!(
        replayed, current,
        "the test events replay to the stored rows"
    );
    assert_eq!(current.cases.len(), 2);
    assert_eq!(current.cases[0].title, "First, renamed");
    assert_eq!(current.cases[0].kind, CaseKind::Integration);
    assert_eq!(current.cases[0].version, 3);
    assert_eq!(current.cases[0].created_by, agent);
    assert_eq!(current.case_criteria.len(), 1);
    let first = tests_store::case_show(&tx, &owner, &current.cases[0].short_id).unwrap();
    let detail = first.detail.unwrap();
    assert_eq!(detail.criteria.len(), 1);
    assert_eq!(detail.criteria[0].position, 1);
    assert!(detail.sets.is_empty(), "the first case left the set");
    assert_eq!(current.sets.len(), 1);
    assert_eq!(current.sets[0].name, "Smoke checks");
    assert_eq!(current.set_cases.len(), 1);
    let set = tests_store::set_show(&tx, &current.sets[0].short_id).unwrap();
    assert_eq!(set.cases, [current.cases[1].short_id.clone()]);
    assert_eq!(current.plans.len(), 1);
    assert_eq!(current.plans[0].status, PlanStatus::Active);
    assert_eq!(current.plan_items.len(), 1);
    let plan = tests_store::plan_show(&tx, &current.plans[0].short_id).unwrap();
    let item = PlanItemView {
        position: 2,
        item: PlanItemRef::Set(current.sets[0].short_id.clone()),
    };
    assert_eq!(plan.items, [item]);
    let ops: Vec<String> = tx
        .prepare("SELECT op FROM tk_events ORDER BY id")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .map(Result::unwrap)
        .filter(|op: &String| tests_store::OPS.contains(&op.as_str()))
        .collect();
    assert_eq!(
        ops,
        [
            tests_store::CASE_CREATE,
            tests_store::CASE_CREATE,
            tests_store::CASE_EDIT,
            tests_store::CASE_LINK,
            tests_store::CASE_LINK,
            tests_store::CASE_UNLINK,
            tests_store::SET_UPSERT,
            tests_store::SET_ADD,
            tests_store::SET_ADD,
            tests_store::SET_REMOVE,
            tests_store::SET_UPSERT,
            tests_store::PLAN_UPSERT,
            tests_store::PLAN_ADD,
            tests_store::PLAN_ADD,
            tests_store::PLAN_REMOVE,
            tests_store::PLAN_UPSERT,
        ]
    );
    assert_eq!(
        events::replay(&tx, &key("CA")).unwrap(),
        store::state(&tx, &key("CA")).unwrap(),
        "the ticket replay skips test events"
    );
    assert!(tx.execute("DELETE FROM tk_events", []).is_err());
}

#[test]
fn a_stable_name_is_unique_per_project() {
    let dir = tempfile::tempdir().unwrap();
    let owner = actor("owner");
    let mut store = opened(dir.path());
    let tx = store.transaction().unwrap();
    store::create_project(&tx, &owner, &key("CA"), "CrazyAgent", 1_000).unwrap();
    store::create_project(&tx, &owner, &key("TW"), "Trackwright", 1_001).unwrap();
    let name = "store::tests::events_replay_to_the_current_state";
    let first =
        tests_store::create_case(&tx, &owner, &key("CA"), &new_case("A", Some(name)), 1_002)
            .unwrap();
    assert_eq!(
        refused(tests_store::create_case(
            &tx,
            &owner,
            &key("CA"),
            &new_case("B", Some(name)),
            1_003
        )),
        codes::STABLE_NAME_TAKEN
    );
    let other =
        tests_store::create_case(&tx, &owner, &key("TW"), &new_case("A", Some(name)), 1_004)
            .unwrap();
    let unnamed =
        tests_store::create_case(&tx, &owner, &key("CA"), &new_case("C", None), 1_005).unwrap();
    tests_store::create_case(&tx, &owner, &key("CA"), &new_case("D", None), 1_006).unwrap();
    assert_eq!(
        refused(tests_store::edit_case(
            &tx,
            &owner,
            &unnamed.short_id,
            unnamed.version,
            &CaseChange {
                stable_name: Some(Some(name.to_owned())),
                ..CaseChange::default()
            },
            1_007
        )),
        codes::STABLE_NAME_TAKEN
    );
    assert_eq!(
        tests_store::case_for(&tx, &key("CA"), name).unwrap(),
        Some(first.short_id.clone())
    );
    assert_eq!(
        tests_store::case_for(&tx, &key("TW"), name).unwrap(),
        Some(other.short_id.clone())
    );
    assert_eq!(
        tests_store::case_for(&tx, &key("CA"), "nope").unwrap(),
        None
    );
    let long = "n".repeat(513);
    for bad in ["a\u{202e}b", "two\nlines", "  ", long.as_str()] {
        assert!(matches!(
            tests_store::create_case(&tx, &owner, &key("CA"), &new_case("E", Some(bad)), 1_008),
            Err(StoreError::Refused(_))
        ));
    }
    let freed = tests_store::edit_case(
        &tx,
        &owner,
        &first.short_id,
        first.version,
        &CaseChange {
            stable_name: Some(None),
            ..CaseChange::default()
        },
        1_009,
    )
    .unwrap();
    assert_eq!(freed.stable_name, None);
    assert_eq!(tests_store::case_for(&tx, &key("CA"), name).unwrap(), None);
    tests_store::edit_case(
        &tx,
        &owner,
        &unnamed.short_id,
        unnamed.version,
        &CaseChange {
            stable_name: Some(Some(name.to_owned())),
            ..CaseChange::default()
        },
        1_010,
    )
    .unwrap();
    assert_eq!(
        tests_store::case_for(&tx, &key("CA"), name).unwrap(),
        Some(unnamed.short_id.clone())
    );
}

#[test]
fn a_plan_item_holds_exactly_one_case_or_set() {
    let dir = tempfile::tempdir().unwrap();
    let owner = actor("owner");
    let mut store = opened(dir.path());
    let tx = store.transaction().unwrap();
    store::create_project(&tx, &owner, &key("CA"), "CrazyAgent", 1_000).unwrap();
    store::create_project(&tx, &owner, &key("TW"), "Trackwright", 1_001).unwrap();
    let case =
        tests_store::create_case(&tx, &owner, &key("CA"), &new_case("A", None), 1_002).unwrap();
    let foreign =
        tests_store::create_case(&tx, &owner, &key("TW"), &new_case("B", None), 1_003).unwrap();
    let set = tests_store::upsert_set(&tx, &owner, &key("CA"), None, "Smoke", 1_004).unwrap();
    let fields = PlanFields {
        name: "Release 1".to_owned(),
        release: None,
        cycle: None,
        status: PlanStatus::Active,
    };
    let plan = tests_store::upsert_plan(&tx, &owner, &key("CA"), None, &fields, 1_005).unwrap();
    let by_case = PlanItemRef::Case(case.short_id.clone());
    let by_set = PlanItemRef::Set(set.short_id.clone());
    let plan =
        tests_store::plan_add(&tx, &owner, &plan.short_id, plan.version, &by_case, 1_006).unwrap();
    let plan =
        tests_store::plan_add(&tx, &owner, &plan.short_id, plan.version, &by_set, 1_007).unwrap();
    let shown = tests_store::plan_show(&tx, &plan.short_id).unwrap();
    let items: Vec<PlanItemRef> = shown.items.into_iter().map(|item| item.item).collect();
    assert_eq!(items, [by_case.clone(), by_set.clone()]);
    assert_eq!(
        refused(tests_store::plan_add(
            &tx,
            &owner,
            &plan.short_id,
            plan.version,
            &by_case,
            1_008
        )),
        codes::PLAN_ITEM_EXISTS
    );
    assert_eq!(
        refused(tests_store::plan_add(
            &tx,
            &owner,
            &plan.short_id,
            plan.version,
            &PlanItemRef::Case(foreign.short_id.clone()),
            1_009
        )),
        codes::CROSS_PROJECT
    );
    assert_eq!(
        refused(tests_store::plan_add(
            &tx,
            &owner,
            &plan.short_id,
            plan.version,
            &PlanItemRef::Set(TestId::parse("CA-T1").unwrap()),
            1_010
        )),
        codes::TEST_ID_INVALID
    );

    let case_id = row_id(&tx, "tk_cases", "CA", case.short_id.number());
    let set_id = row_id(&tx, "tk_sets", "CA", set.short_id.number());
    let plan_id = row_id(&tx, "tk_plans", "CA", plan.short_id.number());
    for (case_id, set_id) in [(None, None), (Some(case_id), Some(set_id))] {
        let inserted = tx.execute(
            "INSERT INTO tk_plan_items(plan_id, position, case_id, set_id) VALUES (?1, 9, ?2, ?3)",
            rusqlite::params![plan_id, case_id, set_id],
        );
        assert!(
            inserted.is_err(),
            "the table refuses {case_id:?} with {set_id:?}"
        );
    }
    for wire in [
        r#"{"case":"CA-T1","set":"CA-S1"}"#,
        r#"{}"#,
        r#"{"case":"CA-T1","extra":1}"#,
        r#"{"ticket":"CA-1"}"#,
    ] {
        assert!(
            serde_json::from_str::<PlanItemRef>(wire).is_err(),
            "{wire} is not one plan item"
        );
    }
    assert_eq!(
        serde_json::from_str::<PlanItemRef>(r#"{"set":"CA-S1"}"#).unwrap(),
        by_set
    );
    assert_eq!(count(&tx, "tk_plan_items"), 2);
}

#[test]
fn a_protected_case_shows_only_id_title_and_outcome_to_an_agent_actor() {
    let dir = tempfile::tempdir().unwrap();
    let owner = actor("owner");
    let agent = actor("agent:w1");
    let mut store = opened(dir.path());
    let tx = store.transaction().unwrap();
    store::create_project(&tx, &owner, &key("CA"), "CrazyAgent", 1_000).unwrap();
    let mut secret = new_case("Holdout", Some("holdout::secret_name"));
    secret.protected = true;
    secret.kind = CaseKind::Scenario;
    secret.steps = vec!["Open the secret door\nthen look".to_owned()];
    let held = tests_store::create_case(&tx, &owner, &key("CA"), &secret, 1_001).unwrap();
    let open =
        tests_store::create_case(&tx, &owner, &key("CA"), &new_case("Open", None), 1_002).unwrap();
    let held_id = row_id(&tx, "tk_cases", "CA", held.short_id.number());
    insert_finished_run(&tx, project_id(&tx, "CA"), held_id, "failed");

    let seen = tests_store::case_show(&tx, &agent, &held.short_id).unwrap();
    assert_eq!(seen.id, held.short_id);
    assert_eq!(seen.title, "Holdout");
    assert!(seen.protected);
    assert_eq!(seen.latest, Some(ResultOutcome::Failed));
    assert_eq!(seen.steps, StepsView::Hidden);
    assert_eq!(seen.detail, None);
    let wire = serde_json::to_string(&seen).unwrap();
    for leaked in ["secret door", "holdout::secret_name", "scenario"] {
        assert!(!wire.contains(leaked), "the agent view leaks {leaked}");
    }

    let full = tests_store::case_show(&tx, &owner, &held.short_id).unwrap();
    assert_eq!(full.steps, StepsView::Full(held.steps.clone()));
    assert_eq!(full.detail.as_ref().unwrap().kind, CaseKind::Scenario);
    let unprotected = tests_store::case_show(&tx, &agent, &open.short_id).unwrap();
    assert!(matches!(unprotected.steps, StepsView::Full(_)));
    assert!(unprotected.detail.is_some());

    let detail: CaseDetail = full.detail.clone().unwrap();
    let summary = case_view(
        &held,
        detail.clone(),
        seen.latest,
        &agent,
        CaseProtectedVisibility::Summary,
    );
    assert_eq!(
        summary.steps,
        StepsView::Headings(vec!["Open the secret door".to_owned()])
    );
    assert_eq!(summary.detail, None);
    let shown = case_view(&held, detail, None, &agent, CaseProtectedVisibility::Full);
    assert_eq!(shown.steps, StepsView::Full(held.steps.clone()));

    let rows = tests_store::case_list(&tx, &agent, &key("CA"), &Default::default()).unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].id, held.short_id);
    assert_eq!(rows[0].kind, None);
    assert_eq!(rows[0].status, None);
    assert_eq!(rows[0].latest, Some(ResultOutcome::Failed));
    assert_eq!(rows[1].kind, Some(CaseKind::Unit));
    let by_kind = tests_store::case_list(
        &tx,
        &agent,
        &key("CA"),
        &tests_store::CaseFilter {
            kind: Some(CaseKind::Scenario),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(by_kind.is_empty(), "a hidden kind is never matched");
    assert_eq!(
        refused(tests_store::edit_case(
            &tx,
            &agent,
            &held.short_id,
            held.version,
            &CaseChange {
                protected: Some(false),
                ..CaseChange::default()
            },
            1_003
        )),
        codes::CASE_PROTECTED
    );
}

#[test]
fn every_new_statement_prepares_against_a_database_holding_only_the_core_schema() {
    let mut connection = Connection::open_in_memory().unwrap();
    let tx = connection.transaction().unwrap();
    schema::migrate(&tx).unwrap();
    let names: Vec<String> = tx
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    let mut expected: Vec<&str> = V3A_TABLES.iter().chain(TEST_TABLES).copied().collect();
    expected.push("tk_meta");
    expected.sort_unstable();
    assert_eq!(names, expected);
    let statements = tests_store::statements();
    assert!(
        statements.len() >= 20,
        "the list holds the test model's statements"
    );
    for statement in statements {
        if let Err(problem) = tx.prepare(statement) {
            panic!("{statement}: {problem}");
        }
    }
}

#[test]
fn test_ids_read_only_their_own_shape() {
    let id = TestId::parse("CA-T12").unwrap();
    assert_eq!(id.object(), TestObject::Case);
    assert_eq!(id.number(), 12);
    assert_eq!(id.to_string(), "CA-T12");
    for object in TestObject::ALL {
        let text = format!("TW-{}7", object.letter());
        assert_eq!(TestId::parse(&text).unwrap().object(), *object);
    }
    for bad in [
        "CA-12",
        "CA-T0",
        "CA-T012",
        "CA-X1",
        "ca-T1",
        "CA-T",
        "CA-T1x",
        "CA-T99999999999",
    ] {
        assert_eq!(
            TestId::parse(bad).unwrap_err().code,
            codes::TEST_ID_INVALID,
            "{bad}"
        );
    }
    assert!(
        ShortId::parse("CA-T1").is_err(),
        "a test id is never a ticket id"
    );
    assert_eq!(
        TestId::parse_of("CA-S1", TestObject::Case)
            .unwrap_err()
            .code,
        codes::TEST_ID_INVALID
    );
    let wire: TestId = serde_json::from_str("\"CA-P3\"").unwrap();
    assert_eq!(serde_json::to_string(&wire).unwrap(), "\"CA-P3\"");
}
