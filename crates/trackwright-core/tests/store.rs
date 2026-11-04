use rusqlite::{Connection, Transaction, TransactionBehavior};
use std::fs;
use std::path::Path;
use trackwright_core::events;
use trackwright_core::model::{
    Actor, CRITERIA_PER_TICKET_MAX, CriterionState, Priority, ProjectKey, ShortId, TicketKind,
    TicketStatus, codes,
};
use trackwright_core::schema;
use trackwright_core::store::{self, NewTicket, Store, StoreError, TicketChange};
use trackwright_core::text::{self, TextField};

fn opened(dir: &Path) -> Store {
    Store::open(&dir.join("tracker").join("tracker.db")).unwrap()
}

fn actor(name: &str) -> Actor {
    Actor::new(name).unwrap()
}

fn key() -> ProjectKey {
    ProjectKey::new("CA").unwrap()
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

fn refused<T: std::fmt::Debug>(result: Result<T, StoreError>) -> &'static str {
    match result {
        Err(StoreError::Refused(refusal)) => refusal.code,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

fn count(tx: &Transaction<'_>, table: &str) -> i64 {
    tx.query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
        row.get(0)
    })
    .unwrap()
}

fn snapshot(tx: &Transaction<'_>) -> Vec<(String, String, String)> {
    let mut statement = tx
        .prepare("SELECT type, name, coalesce(sql, '') FROM sqlite_master ORDER BY type, name")
        .unwrap();
    let mut rows: Vec<(String, String, String)> = statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    let mut meta = tx
        .prepare("SELECT key, value FROM tk_meta ORDER BY key")
        .unwrap();
    rows.extend(
        meta.query_map([], |row| {
            Ok((
                "meta".to_owned(),
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?.to_string(),
            ))
        })
        .unwrap()
        .map(Result::unwrap),
    );
    rows
}

#[test]
fn events_replay_to_the_current_state() {
    let dir = tempfile::tempdir().unwrap();
    let owner = actor("owner");
    let agent = actor("agent:w1");
    {
        let mut store = opened(dir.path());
        let tx = store.transaction().unwrap();
        store::create_project(&tx, &owner, &key(), "CrazyAgent", 1_000).unwrap();
        let first =
            store::create_ticket(&tx, &owner, &key(), &new_ticket("Store tickets"), 1_001).unwrap();
        let second =
            store::create_ticket(&tx, &agent, &key(), &new_ticket("Replay events"), 1_002).unwrap();
        assert_eq!(first.short_id.to_string(), "CA-1");
        assert_eq!(second.short_id.to_string(), "CA-2");
        let moved = store::update_ticket(
            &tx,
            &agent,
            &first.short_id,
            first.version,
            &TicketChange {
                status: Some(TicketStatus::InProgress),
                assignee: Some(Some(agent.clone())),
                ..TicketChange::default()
            },
            1_003,
        )
        .unwrap();
        assert_eq!(moved.version, first.version + 1);
        let criterion =
            store::add_criterion(&tx, &owner, &first.short_id, "events replay", 1_004).unwrap();
        store::set_criterion(
            &tx,
            &agent,
            &first.short_id,
            criterion.position,
            criterion.version,
            CriterionState::Met,
            1_005,
        )
        .unwrap();
        store::add_link(
            &tx,
            &agent,
            &first.short_id,
            "branch",
            "feat/v3a-at1-core",
            1_006,
        )
        .unwrap();
        store::add_link(&tx, &agent, &first.short_id, "worktree", "/work/at1", 1_007).unwrap();
        store::remove_link(&tx, &agent, &first.short_id, "worktree", "/work/at1", 1_008).unwrap();
        store::update_ticket(
            &tx,
            &owner,
            &first.short_id,
            moved.version,
            &TicketChange {
                status: Some(TicketStatus::Done),
                close_reason: Some(Some("shipped".to_owned())),
                ..TicketChange::default()
            },
            1_009,
        )
        .unwrap();
        tx.commit().unwrap();
    }

    let mut store = opened(dir.path());
    let tx = store.transaction().unwrap();
    let current = store::state(&tx, &key()).unwrap();
    let replayed = events::replay(&tx, &key()).unwrap();
    assert_eq!(replayed, current, "the events replay to the stored rows");
    assert_eq!(current.tickets.len(), 2);
    let done = &current.tickets[0];
    assert_eq!(done.status, TicketStatus::Done);
    assert_eq!(done.closed_by.as_ref(), Some(&owner));
    assert_eq!(done.closed_ms, Some(1_009));
    assert_eq!(done.close_reason.as_deref(), Some("shipped"));
    assert_eq!(done.assignee.as_ref(), Some(&agent));
    assert_eq!(current.criteria.len(), 1);
    assert_eq!(current.criteria[0].state, CriterionState::Met);
    assert_eq!(current.links.len(), 1);
    assert_eq!(current.links[0].target, "feat/v3a-at1-core");
    let first = ShortId::parse("CA-1").unwrap();
    let ops: Vec<String> = events::history(&tx, &first)
        .unwrap()
        .into_iter()
        .map(|event| event.op)
        .collect();
    assert_eq!(
        ops,
        [
            events::TICKET_CREATE,
            events::TICKET_UPDATE,
            events::CRITERION_ADD,
            events::CRITERION_SET,
            events::LINK_ADD,
            events::LINK_ADD,
            events::LINK_REMOVE,
            events::TICKET_UPDATE,
        ]
    );
    assert!(
        tx.execute("DELETE FROM tk_events", []).is_err(),
        "the event log refuses a delete"
    );
    assert!(
        tx.execute("UPDATE tk_events SET actor = 'owner'", [])
            .is_err(),
        "the event log refuses an update"
    );
}

#[test]
fn an_interrupted_write_leaves_neither_row_nor_event() {
    let dir = tempfile::tempdir().unwrap();
    let owner = actor("owner");
    {
        let mut store = opened(dir.path());
        let tx = store.transaction().unwrap();
        store::create_project(&tx, &owner, &key(), "CrazyAgent", 2_000).unwrap();
        tx.commit().unwrap();

        let tx = store.transaction().unwrap();
        tx.execute_batch(
            "CREATE TEMP TRIGGER interrupt_event BEFORE INSERT ON tk_events \
             BEGIN SELECT RAISE(ABORT, 'interrupted'); END;",
        )
        .unwrap();
        let interrupted =
            store::create_ticket(&tx, &owner, &key(), &new_ticket("Half written"), 2_001);
        assert!(
            matches!(interrupted, Err(StoreError::Sqlite(_))),
            "{interrupted:?}"
        );
        assert_eq!(
            count(&tx, "tk_tickets"),
            0,
            "the row written before the failed event is gone"
        );
        tx.execute_batch("DROP TRIGGER interrupt_event;").unwrap();
        let whole =
            store::create_ticket(&tx, &owner, &key(), &new_ticket("Never committed"), 2_002)
                .unwrap();
        assert_eq!(
            whole.short_id.number(),
            1,
            "the failed write used no number"
        );
        drop(tx);
    }

    let mut store = opened(dir.path());
    let tx = store.transaction().unwrap();
    assert_eq!(
        count(&tx, "tk_tickets"),
        0,
        "a dropped transaction kept no row"
    );
    assert_eq!(count(&tx, "tk_events"), 1, "only the project's event stays");
    assert!(store::state(&tx, &key()).unwrap().tickets.is_empty());
}

#[test]
fn every_core_statement_prepares_against_a_database_holding_only_the_core_schema() {
    let mut connection = Connection::open_in_memory().unwrap();
    let tx = connection.transaction().unwrap();
    schema::migrate(&tx).unwrap();
    let mut tables = tx
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
        .unwrap();
    let names: Vec<String> = tables
        .query_map([], |row| row.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    drop(tables);
    assert_eq!(
        names,
        [
            "tk_case_criteria",
            "tk_cases",
            "tk_checks",
            "tk_criteria",
            "tk_cycles",
            "tk_deps",
            "tk_events",
            "tk_evidence",
            "tk_links",
            "tk_meta",
            "tk_plan_items",
            "tk_plans",
            "tk_policy",
            "tk_projects",
            "tk_requests",
            "tk_results",
            "tk_runs",
            "tk_set_cases",
            "tk_sets",
            "tk_tickets",
            "tk_versions",
        ]
    );
    let statements = store::statements();
    assert!(
        statements.len() >= 20,
        "the list holds the core's statements"
    );
    for statement in statements {
        if let Err(problem) = tx.prepare(statement) {
            panic!("{statement}: {problem}");
        }
    }
}

#[test]
fn migrate_twice_inside_a_host_transaction_changes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let mut connection = Connection::open(dir.path().join("host.db")).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE host_agents(id TEXT PRIMARY KEY, name TEXT NOT NULL);
             INSERT INTO host_agents VALUES ('a1', 'Ada');",
        )
        .unwrap();
    let tx = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    schema::migrate(&tx).unwrap();
    let once = snapshot(&tx);
    assert!(once.iter().any(|(_, name, _)| name == "tk_tickets"));
    assert!(once.contains(&(
        "meta".to_owned(),
        "schema_version".to_owned(),
        schema::SCHEMA_VERSION.to_string()
    )));
    schema::migrate(&tx).unwrap();
    assert_eq!(
        snapshot(&tx),
        once,
        "a second migrate in the same transaction"
    );
    tx.commit().unwrap();

    let tx = connection.transaction().unwrap();
    schema::migrate(&tx).unwrap();
    assert_eq!(snapshot(&tx), once, "a migrate in a later transaction");
    let agents: Vec<(String, String)> = tx
        .prepare("SELECT id, name FROM host_agents")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(agents, [("a1".to_owned(), "Ada".to_owned())]);
    tx.execute(
        "UPDATE tk_meta SET value = value + 1 WHERE key = 'schema_version'",
        [],
    )
    .unwrap();
    match schema::migrate(&tx) {
        Err(StoreError::Refused(refusal)) => assert_eq!(refusal.code, codes::STORE_SCHEMA_NEWER),
        other => panic!("a newer schema must be refused: {other:?}"),
    }
}

#[test]
fn bounds_and_hidden_characters_are_refused_plainly() {
    let cases = [
        (TextField::Title, "a".repeat(201), codes::TEXT_TOO_LONG),
        (TextField::Title, "   ".to_owned(), codes::TEXT_EMPTY),
        (
            TextField::Title,
            "two\nlines".to_owned(),
            codes::TEXT_LINE_BREAK,
        ),
        (
            TextField::Title,
            "safe\u{202e}txt".to_owned(),
            codes::TEXT_HIDDEN_CHARACTERS,
        ),
        (
            TextField::Title,
            "zero\u{200b}width".to_owned(),
            codes::TEXT_HIDDEN_CHARACTERS,
        ),
        (
            TextField::Title,
            "tag\u{e0041}".to_owned(),
            codes::TEXT_HIDDEN_CHARACTERS,
        ),
        (
            TextField::Body,
            "x".repeat(16 * 1024 + 1),
            codes::TEXT_TOO_LONG,
        ),
        (
            TextField::Body,
            "bell\u{7}".to_owned(),
            codes::TEXT_HIDDEN_CHARACTERS,
        ),
        (
            TextField::Body,
            "over\rwrite".to_owned(),
            codes::TEXT_HIDDEN_CHARACTERS,
        ),
        (TextField::Criterion, "c".repeat(501), codes::TEXT_TOO_LONG),
        (TextField::CheckRef, "r".repeat(513), codes::TEXT_TOO_LONG),
        (
            TextField::CloseReason,
            "r".repeat(1025),
            codes::TEXT_TOO_LONG,
        ),
        (TextField::LinkTarget, "t".repeat(513), codes::TEXT_TOO_LONG),
    ];
    for (field, value, code) in &cases {
        assert_eq!(
            text::check(*field, value).unwrap_err().code,
            *code,
            "{field:?}"
        );
    }
    assert!(text::check(TextField::Title, &"a".repeat(200)).is_ok());
    assert!(text::check(TextField::Body, "").is_ok());
    assert!(text::check(TextField::Body, "line one\nline two\tend").is_ok());
    assert!(text::check(TextField::Title, "Ship it \u{2764}\u{fe0f}").is_ok());
    assert!(text::check(TextField::Title, "Team \u{1f468}\u{200d}\u{1f469}").is_ok());

    for bad in ["C", "ca", "ABCDEFGHI", "C1", ""] {
        assert_eq!(
            ProjectKey::new(bad).unwrap_err().code,
            codes::PROJECT_KEY_INVALID,
            "{bad:?}"
        );
    }
    for bad in [
        "CA-0",
        "CA-01",
        "CA-",
        "ca-1",
        "CA-4294967296",
        "CA1",
        "CA--1",
    ] {
        assert_eq!(
            ShortId::parse(bad).unwrap_err().code,
            codes::SHORT_ID_INVALID,
            "{bad:?}"
        );
    }
    assert_eq!(ShortId::parse("CA-42").unwrap().to_string(), "CA-42");
    let too_long = "a".repeat(129);
    for bad in ["host:evidence", "", "two words", too_long.as_str()] {
        assert_eq!(
            Actor::new(bad).unwrap_err().code,
            codes::ACTOR_INVALID,
            "{bad:?}"
        );
    }
    assert!(Actor::new("agent:w1").is_ok());
    assert_eq!(Priority::new(5).unwrap_err().code, codes::PRIORITY_INVALID);

    let dir = tempfile::tempdir().unwrap();
    let mut store = opened(dir.path());
    let owner = actor("owner");
    let tx = store.transaction().unwrap();
    store::create_project(&tx, &owner, &key(), "CrazyAgent", 3_000).unwrap();
    assert_eq!(
        refused(store::create_ticket(
            &tx,
            &owner,
            &key(),
            &new_ticket("bad\u{202e}title"),
            3_001
        )),
        codes::TEXT_HIDDEN_CHARACTERS
    );
    assert_eq!(
        refused(store::create_ticket(
            &tx,
            &owner,
            &key(),
            &new_ticket(&"t".repeat(201)),
            3_002
        )),
        codes::TEXT_TOO_LONG
    );
    assert_eq!(count(&tx, "tk_tickets"), 0);
    assert_eq!(count(&tx, "tk_events"), 1);
    assert_eq!(
        refused(store::create_project(&tx, &owner, &key(), "Again", 3_003)),
        codes::PROJECT_EXISTS
    );
    let ticket = store::create_ticket(&tx, &owner, &key(), &new_ticket("Bounded"), 3_004).unwrap();
    for n in 0..CRITERIA_PER_TICKET_MAX {
        store::add_criterion(
            &tx,
            &owner,
            &ticket.short_id,
            &format!("criterion {n}"),
            3_005,
        )
        .unwrap();
    }
    assert_eq!(
        refused(store::add_criterion(
            &tx,
            &owner,
            &ticket.short_id,
            "one too many",
            3_006
        )),
        codes::CRITERIA_FULL
    );
    assert_eq!(
        refused(store::update_ticket(
            &tx,
            &owner,
            &ticket.short_id,
            ticket.version + 1,
            &TicketChange {
                title: Some("Renamed".to_owned()),
                ..TicketChange::default()
            },
            3_007,
        )),
        codes::STALE_VERSION
    );
    assert_eq!(
        refused(store::add_link(
            &tx,
            &owner,
            &ticket.short_id,
            "branch",
            "a\u{200b}b",
            3_008
        )),
        codes::TEXT_HIDDEN_CHARACTERS
    );
    store::add_link(&tx, &owner, &ticket.short_id, "branch", "feat/x", 3_009).unwrap();
    assert_eq!(
        refused(store::add_link(
            &tx,
            &owner,
            &ticket.short_id,
            "branch",
            "feat/x",
            3_010
        )),
        codes::LINK_EXISTS
    );
    assert_eq!(
        refused(store::ticket(&tx, &ShortId::parse("CA-99").unwrap())),
        codes::TICKET_UNKNOWN
    );
}

#[test]
#[should_panic(expected = "source.bytes().all(actor_byte)")]
fn the_core_s_own_actor_takes_only_a_plain_source() {
    Actor::host("two words");
}

#[cfg(unix)]
#[test]
fn the_standalone_file_is_0600_in_a_0700_folder() {
    use std::os::unix::fs::PermissionsExt;
    let mode = |path: &Path| fs::symlink_metadata(path).unwrap().permissions().mode() & 0o777;
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().join("tracker");
    let path = folder.join("tracker.db");
    drop(Store::open(&path).unwrap());
    assert_eq!(mode(&folder), 0o700);
    assert_eq!(mode(&path), 0o600);
    drop(Store::open(&path).unwrap());

    fs::set_permissions(&folder, fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(refused(Store::open(&path)), codes::STORE_NOT_PRIVATE);
    fs::set_permissions(&folder, fs::Permissions::from_mode(0o700)).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(refused(Store::open(&path)), codes::STORE_NOT_PRIVATE);
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    let link = folder.join("link.db");
    std::os::unix::fs::symlink(&path, &link).unwrap();
    assert_eq!(refused(Store::open(&link)), codes::STORE_PATH_REFUSED);
    let linked_folder = dir.path().join("linked");
    std::os::unix::fs::symlink(&folder, &linked_folder).unwrap();
    assert_eq!(
        refused(Store::open(&linked_folder.join("tracker.db"))),
        codes::STORE_PATH_REFUSED
    );
}

#[test]
fn no_trackwright_crate_depends_on_crazyagent() {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut seen = 0;
    for name in ["trackwright-core", "trackwright-mcp", "trackwright-cli"] {
        let manifest: toml::Table = fs::read_to_string(crates.join(name).join("Cargo.toml"))
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(manifest["package"]["name"].as_str(), Some(name));
        let mut tables: Vec<toml::Table> = Vec::new();
        let sections = ["dependencies", "dev-dependencies", "build-dependencies"];
        for section in sections {
            if let Some(table) = manifest.get(section).and_then(toml::Value::as_table) {
                tables.push(table.clone());
            }
        }
        if let Some(targets) = manifest.get("target").and_then(toml::Value::as_table) {
            for target in targets.values() {
                for section in sections {
                    if let Some(table) = target.get(section).and_then(toml::Value::as_table) {
                        tables.push(table.clone());
                    }
                }
            }
        }
        for table in &tables {
            for (dependency, spec) in table {
                let package = spec
                    .get("package")
                    .and_then(toml::Value::as_str)
                    .unwrap_or(dependency);
                assert!(
                    !package.starts_with("crazyagent"),
                    "{name} depends on {package}"
                );
                if let Some(path) = spec.get("path").and_then(toml::Value::as_str) {
                    assert!(
                        path.starts_with("../trackwright-"),
                        "{name} reaches outside the trackwright crates through {path}"
                    );
                }
                seen += 1;
            }
        }
    }
    assert!(
        seen >= 7,
        "the check read every dependency table ({seen} entries)"
    );
}

#[test]
fn a_store_hands_over_its_migrated_connection_with_foreign_keys_on() {
    let dir = tempfile::tempdir().unwrap();
    let connection = opened(dir.path()).into_connection();
    let foreign_keys: i64 = connection
        .query_row("PRAGMA foreign_keys", [], |row| row.get(0))
        .unwrap();
    assert_eq!(foreign_keys, 1);
    let version: i64 = connection
        .query_row(
            "SELECT value FROM tk_meta WHERE key = 'schema_version'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(version, schema::SCHEMA_VERSION);
}

#[test]
fn a_caller_cannot_pose_as_the_core_s_own_actor() {
    #[derive(Debug, serde::Deserialize)]
    struct Request {
        assignee: Option<Actor>,
    }
    let posed = serde_json::from_str::<Request>(r#"{"assignee":"host:evidence"}"#).unwrap_err();
    assert!(posed.to_string().contains(codes::ACTOR_INVALID), "{posed}");
    let named = serde_json::from_str::<Request>(r#"{"assignee":"agent:w1"}"#).unwrap();
    assert_eq!(named.assignee, Some(actor("agent:w1")));
    assert_eq!(
        Actor::try_from("host:evidence".to_owned())
            .unwrap_err()
            .code,
        codes::ACTOR_INVALID
    );

    let dir = tempfile::tempdir().unwrap();
    let mut store = opened(dir.path());
    let core = Actor::host("import");
    let tx = store.transaction().unwrap();
    store::create_project(&tx, &core, &key(), "CrazyAgent", 4_000).unwrap();
    let imported =
        store::create_ticket(&tx, &core, &key(), &new_ticket("Imported"), 4_001).unwrap();
    store::update_ticket(
        &tx,
        &core,
        &imported.short_id,
        imported.version,
        &TicketChange {
            assignee: Some(Some(core.clone())),
            ..TicketChange::default()
        },
        4_002,
    )
    .unwrap();
    store::add_link(&tx, &core, &imported.short_id, "roadmap", "V3a", 4_003).unwrap();
    let current = store::state(&tx, &key()).unwrap();
    assert!(current.tickets[0].created_by.is_host());
    assert_eq!(current.tickets[0].assignee.as_ref(), Some(&core));
    assert_eq!(
        events::replay(&tx, &key()).unwrap(),
        current,
        "the core's own actors still read back from rows and events"
    );
}

#[test]
fn the_core_runs_sql_only_through_its_listed_statements() {
    let allowed = [
        ("store.rs", "tx.prepare(sql)"),
        ("store.rs", "tx.execute_batch(\"SAVEPOINTtk_write\")"),
        ("store.rs", "tx.execute_batch(\"RELEASEtk_write\")"),
        (
            "store.rs",
            "tx.execute_batch(\"ROLLBACKTOtk_write;RELEASEtk_write\")",
        ),
        ("schema_sql.rs", "tx.execute_batch(META)"),
        ("schema_sql.rs", "tx.execute_batch(migration)"),
    ];
    let receivers = ["tx", "connection", "transaction", "conn", "db"];
    let methods = [
        "execute",
        "execute_batch",
        "query_row",
        "query_row_and_then",
        "prepare",
        "prepare_cached",
        "prepare_with_flags",
        "pragma_query",
        "pragma_query_value",
    ];
    let mut folders = vec![Path::new(env!("CARGO_MANIFEST_DIR")).join("src")];
    let mut found: Vec<(String, String)> = Vec::new();
    let mut files = 0;
    while let Some(folder) = folders.pop() {
        for entry in fs::read_dir(&folder).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                folders.push(path);
                continue;
            }
            if path.extension().and_then(|extension| extension.to_str()) != Some("rs") {
                continue;
            }
            files += 1;
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            let compact: String = fs::read_to_string(&path)
                .unwrap()
                .chars()
                .filter(|c| !c.is_whitespace())
                .collect();
            for receiver in receivers {
                for method in methods {
                    let call = format!("{receiver}.{method}(");
                    for (at, _) in compact.match_indices(&call) {
                        let before = compact[..at].chars().next_back();
                        if before.is_some_and(|c| c.is_alphanumeric() || c == '_') {
                            continue;
                        }
                        let end = compact[at..]
                            .find(')')
                            .map_or(compact.len(), |close| at + close + 1);
                        found.push((name.clone(), compact[at..end].to_owned()));
                    }
                }
            }
        }
    }
    assert!(
        files >= 6,
        "the scan read the core's sources ({files} files)"
    );
    let direct: Vec<&(String, String)> = found
        .iter()
        .filter(|(file, call)| !allowed.contains(&(file.as_str(), call.as_str())))
        .collect();
    assert!(
        direct.is_empty(),
        "SQL outside store::prepare and its STATEMENTS list: {direct:?}"
    );
    for (file, call) in allowed {
        let seen = found
            .iter()
            .filter(|(at, text)| at == file && text == call)
            .count();
        assert_eq!(seen, 1, "{file}: {call}");
    }
}
