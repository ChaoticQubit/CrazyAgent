use super::*;
use rusqlite::OptionalExtension;

fn store() -> (tempfile::TempDir, Connection) {
    let root = tempfile::tempdir().unwrap();
    let db = open(root.path()).unwrap();
    (root, db)
}

fn code<T: std::fmt::Debug>(result: Result<T, EngineError>) -> String {
    result.unwrap_err().0
}

fn stored_outcome(db: &Connection, id: ActionId) -> Option<String> {
    db.query_row(
        "SELECT outcome FROM ledger_actions WHERE id=?1",
        [id.0],
        |row| row.get::<_, Option<String>>(0),
    )
    .optional()
    .unwrap()
    .flatten()
}

fn finished_rows(db: &Connection) -> i64 {
    db.query_row(
        "SELECT COUNT(*) FROM ledger_actions WHERE outcome IS NOT NULL",
        [],
        |row| row.get(0),
    )
    .unwrap()
}

fn ids(actions: &[Action]) -> Vec<ActionId> {
    actions.iter().map(|action| action.id).collect()
}

struct FileCheck;

impl Check for FileCheck {
    fn check(&self, action: &Action) -> Resolution {
        let path = Path::new(&action.payload);
        if path.exists() {
            Resolution::Done
        } else if path.parent().is_some_and(Path::is_dir) {
            Resolution::NotStarted
        } else {
            Resolution::Unknown
        }
    }
}

#[test]
fn intent_without_outcome_is_pending_after_reopen() {
    let (root, db) = store();
    let sent = begin(
        &db,
        ActionKind::Message,
        "alpha/agent-1/m-7",
        "{\"text\":\"hi\"}",
    )
    .unwrap();
    let spawned = begin(&db, ActionKind::AgentSpawn, "alpha/agent-2", "{}").unwrap();
    finish(&db, spawned, Outcome::Done).unwrap();
    drop(db);

    let db = open(root.path()).unwrap();
    let messages = pending(&db, ActionKind::Message).unwrap();
    assert_eq!(messages.len(), 1, "the intent survives the reopen");
    let action = &messages[0];
    assert_eq!(action.id, sent);
    assert_eq!(action.kind, ActionKind::Message);
    assert_eq!(action.key, "alpha/agent-1/m-7");
    assert_eq!(action.payload, "{\"text\":\"hi\"}");
    assert!(action.started_ms > 0);
    assert!(
        pending(&db, ActionKind::AgentSpawn).unwrap().is_empty(),
        "a finished action is not pending"
    );
    assert!(pending(&db, ActionKind::Dispatch).unwrap().is_empty());
    assert_eq!(stored_outcome(&db, spawned).as_deref(), Some("done"));
}

#[test]
fn finish_twice_is_refused() {
    let (root, db) = store();
    let id = begin(&db, ActionKind::Dispatch, "alpha/d-1", "{}").unwrap();
    finish(&db, id, Outcome::Failed).unwrap();
    assert_eq!(code(finish(&db, id, Outcome::Done)), "ledger_finished");
    assert_eq!(code(finish(&db, id, Outcome::Failed)), "ledger_finished");
    drop(db);

    let db = open(root.path()).unwrap();
    assert_eq!(code(finish(&db, id, Outcome::Unknown)), "ledger_finished");
    assert_eq!(
        stored_outcome(&db, id).as_deref(),
        Some("failed"),
        "the first outcome stands"
    );
    assert_eq!(
        code(finish(&db, ActionId(id.0 + 1000), Outcome::Done)),
        "ledger_missing"
    );
}

#[test]
fn second_pending_action_for_one_key_is_refused() {
    let (_root, db) = store();
    let first = begin(&db, ActionKind::Message, "alpha/agent-1/m-1", "first").unwrap();
    assert_eq!(
        code(begin(
            &db,
            ActionKind::Message,
            "alpha/agent-1/m-1",
            "second"
        )),
        "ledger_pending"
    );
    let messages = pending(&db, ActionKind::Message).unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(
        messages[0].payload, "first",
        "the refused intent wrote nothing"
    );

    begin(&db, ActionKind::Dispatch, "alpha/agent-1/m-1", "{}").unwrap();
    begin(&db, ActionKind::Message, "alpha/agent-1/m-2", "{}").unwrap();

    finish(&db, first, Outcome::Done).unwrap();
    let again = begin(&db, ActionKind::Message, "alpha/agent-1/m-1", "again").unwrap();
    assert_ne!(
        again, first,
        "a finished key takes a new action with a new id"
    );
}

#[test]
fn resolution_finishes_done_and_not_started_and_keeps_an_unknown_one_pending_until_the_owner_closes_it()
 {
    let (root, db) = store();
    let folder = root.path().join("work");
    std::fs::create_dir(&folder).unwrap();
    let applied = folder.join("applied");
    let absent = folder.join("absent");
    let lost = root.path().join("gone").join("lost");
    let done = begin(
        &db,
        ActionKind::WorktreeCreate,
        "alpha/w-1",
        applied.to_str().unwrap(),
    )
    .unwrap();
    let not_started = begin(
        &db,
        ActionKind::WorktreeCreate,
        "alpha/w-2",
        absent.to_str().unwrap(),
    )
    .unwrap();
    let unknown = begin(
        &db,
        ActionKind::WorktreeCreate,
        "alpha/w-3",
        lost.to_str().unwrap(),
    )
    .unwrap();
    let other_kind = begin(
        &db,
        ActionKind::AgentSpawn,
        "alpha/w-1",
        lost.to_str().unwrap(),
    )
    .unwrap();
    std::fs::write(&applied, "").unwrap();
    drop(db);

    let db = open(root.path()).unwrap();
    let resolved = resolve(&db, ActionKind::WorktreeCreate, &FileCheck).unwrap();
    assert_eq!(ids(&resolved.done), vec![done]);
    assert_eq!(ids(&resolved.not_started), vec![not_started]);
    assert_eq!(ids(&resolved.unknown), vec![unknown]);
    assert_eq!(stored_outcome(&db, done).as_deref(), Some("done"));
    assert_eq!(
        stored_outcome(&db, not_started).as_deref(),
        Some("failed"),
        "an action that never started is closed; the caller completes it as a new one"
    );
    assert_eq!(
        stored_outcome(&db, unknown),
        None,
        "an unknown action stays open until the owner closes it"
    );
    assert_eq!(
        ids(&pending(&db, ActionKind::WorktreeCreate).unwrap()),
        vec![unknown]
    );
    assert_eq!(
        stored_outcome(&db, other_kind),
        None,
        "another kind is not checked"
    );

    drop(db);
    let db = open(root.path()).unwrap();
    let again = resolve(&db, ActionKind::WorktreeCreate, &FileCheck).unwrap();
    assert!(
        again.done.is_empty() && again.not_started.is_empty(),
        "a second start finds nothing to replay"
    );
    assert_eq!(
        ids(&again.unknown),
        vec![unknown],
        "an unknown action is shown again, never offered for replay"
    );
    assert_eq!(stored_outcome(&db, unknown), None);
    let redo = begin(
        &db,
        ActionKind::WorktreeCreate,
        "alpha/w-2",
        absent.to_str().unwrap(),
    )
    .unwrap();
    assert_ne!(redo, not_started);
    assert_eq!(
        pending_key(&db, ActionKind::WorktreeCreate, "alpha/w-2")
            .unwrap()
            .map(|action| action.id),
        Some(redo)
    );
    assert_eq!(
        pending_key(&db, ActionKind::WorktreeCreate, "alpha/w-3")
            .unwrap()
            .map(|action| action.id),
        Some(unknown)
    );

    finish(&db, unknown, Outcome::Unknown).unwrap();
    let closed = resolve(&db, ActionKind::WorktreeCreate, &FileCheck).unwrap();
    assert!(closed.unknown.is_empty(), "the owner closed it");
    assert_eq!(stored_outcome(&db, unknown).as_deref(), Some("unknown"));
    assert_eq!(
        pending_key(&db, ActionKind::WorktreeCreate, "alpha/w-3").unwrap(),
        None
    );
}

#[test]
fn only_a_second_pending_action_reads_as_pending_and_any_kind_is_stored() {
    let root = tempfile::tempdir().unwrap();
    let db = Connection::open(root.path().join("engine.db")).unwrap();
    db.execute_batch(
        "CREATE TABLE ledger_actions (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            kind TEXT NOT NULL CHECK(kind IN ('message')),
            action_key TEXT NOT NULL,
            payload TEXT NOT NULL,
            started_ms INTEGER NOT NULL,
            outcome TEXT CHECK(outcome IN ('done','failed','unknown')),
            finished_ms INTEGER,
            CHECK((outcome IS NULL) = (finished_ms IS NULL))
        );",
    )
    .unwrap();
    initialize(&db).unwrap();
    assert_eq!(
        code(begin(&db, ActionKind::LandingAccept, "alpha/c-1", "{}")),
        "ledger_unavailable",
        "a table that refuses the kind is a storage failure, not a pending action"
    );
    begin(&db, ActionKind::Message, "alpha/m-1", "{}").unwrap();
    assert_eq!(
        code(begin(&db, ActionKind::Message, "alpha/m-1", "{}")),
        "ledger_pending"
    );
    drop(db);

    let (_fresh_root, fresh) = store();
    for kind in [
        ActionKind::Message,
        ActionKind::Dispatch,
        ActionKind::WorktreeCreate,
        ActionKind::AgentSpawn,
        ActionKind::LandingAccept,
    ] {
        begin(&fresh, kind, "alpha/one", "{}").unwrap();
        assert_eq!(pending(&fresh, kind).unwrap().len(), 1);
    }
}

#[test]
fn invalid_keys_and_payloads_are_refused_and_record_nothing() {
    let (_root, db) = store();
    let long_key = "k".repeat(KEY_BYTES_MAX + 1);
    let long_payload = "p".repeat(PAYLOAD_BYTES_MAX + 1);
    for (key, payload) in [
        ("", "{}"),
        ("  ", "{}"),
        ("alpha\nbeta", "{}"),
        ("alpha\u{0}", "{}"),
        (long_key.as_str(), "{}"),
        ("alpha", long_payload.as_str()),
    ] {
        assert_eq!(
            code(begin(&db, ActionKind::Message, key, payload)),
            "ledger_invalid",
            "{key:?}"
        );
    }
    assert!(pending(&db, ActionKind::Message).unwrap().is_empty());
    begin(
        &db,
        ActionKind::Message,
        &"k".repeat(KEY_BYTES_MAX),
        &"p".repeat(PAYLOAD_BYTES_MAX),
    )
    .unwrap();
    assert_eq!(code(open(Path::new("relative"))), "ledger_unavailable");
}

#[test]
fn pending_actions_are_bounded_and_finished_ones_are_pruned_to_the_kept_count() {
    let (_root, db) = store();
    let oldest = begin(&db, ActionKind::Dispatch, "keep-open", "{}").unwrap();
    let mut open_ids = vec![oldest];
    for index in 1..PENDING_MAX {
        open_ids.push(begin(&db, ActionKind::Message, &format!("m-{index}"), "{}").unwrap());
    }
    assert_eq!(
        code(begin(&db, ActionKind::Message, "one-more", "{}")),
        "ledger_full"
    );
    for id in open_ids.drain(1..) {
        finish(&db, id, Outcome::Done).unwrap();
    }
    for index in 0..FINISHED_KEPT {
        let id = begin(&db, ActionKind::AgentSpawn, &format!("s-{index}"), "{}").unwrap();
        finish(&db, id, Outcome::Done).unwrap();
    }
    assert_eq!(usize::try_from(finished_rows(&db)).unwrap(), FINISHED_KEPT);
    assert_eq!(
        pending(&db, ActionKind::Dispatch).unwrap().len(),
        1,
        "a pending action is never pruned"
    );
    assert_eq!(stored_outcome(&db, oldest), None);
}

#[test]
fn the_owner_dismisses_only_a_notice_of_its_kind_with_the_outcome_it_shows() {
    let (root, db) = store();
    let starting = begin(&db, ActionKind::AgentSpawn, "team-4", "{\"team_agent\":4}").unwrap();
    let notice_text = "{\"team_agent\":5,\"pane\":false,\"unknown\":true}";
    let start = begin(&db, ActionKind::AgentSpawn, "team-5", notice_text).unwrap();
    let landed = begin(&db, ActionKind::LandingAccept, "set-9", "{\"landed\":true}").unwrap();
    let dropped = begin(
        &db,
        ActionKind::LandingAccept,
        "set-8",
        "{\"landed\":false}",
    )
    .unwrap();
    drop(db);

    let db = open(root.path()).unwrap();
    assert_eq!(
        code(dismiss(&db, ActionKind::AgentSpawn, starting.get())),
        "ledger_not_notice"
    );
    assert_eq!(
        code(dismiss(&db, ActionKind::LandingAccept, start.get())),
        "ledger_missing"
    );
    assert_eq!(
        code(dismiss(&db, ActionKind::AgentSpawn, dropped.get() + 100)),
        "ledger_missing"
    );
    assert_eq!(
        stored_outcome(&db, starting),
        None,
        "a refused dismiss changes nothing"
    );
    assert_eq!(
        ids(&pending(&db, ActionKind::AgentSpawn).unwrap()),
        vec![starting, start]
    );

    assert_eq!(
        dismiss(&db, ActionKind::AgentSpawn, start.get()).unwrap(),
        Outcome::Unknown
    );
    assert_eq!(
        dismiss(&db, ActionKind::LandingAccept, landed.get()).unwrap(),
        Outcome::Done
    );
    assert_eq!(
        dismiss(&db, ActionKind::LandingAccept, dropped.get()).unwrap(),
        Outcome::Failed
    );
    assert_eq!(stored_outcome(&db, start).as_deref(), Some("unknown"));
    assert_eq!(stored_outcome(&db, landed).as_deref(), Some("done"));
    assert_eq!(stored_outcome(&db, dropped).as_deref(), Some("failed"));
    assert_eq!(
        ids(&pending(&db, ActionKind::AgentSpawn).unwrap()),
        vec![starting]
    );
    assert!(pending(&db, ActionKind::LandingAccept).unwrap().is_empty());
    assert_eq!(
        code(dismiss(&db, ActionKind::AgentSpawn, start.get())),
        "ledger_finished"
    );
    begin(&db, ActionKind::AgentSpawn, "team-5", "{\"team_agent\":5}").unwrap();
}

#[test]
fn only_marked_actions_and_listed_accepts_are_notices() {
    let action = |kind, payload: &str| Action {
        id: ActionId(1),
        kind,
        key: "k".to_owned(),
        payload: payload.to_owned(),
        started_ms: 1,
    };
    let unknown = "{\"unknown\":true}";
    assert_eq!(
        notice(&action(ActionKind::WorktreeCreate, unknown)),
        Some(Outcome::Unknown)
    );
    assert_eq!(
        notice(&action(ActionKind::Message, unknown)),
        Some(Outcome::Unknown)
    );
    assert_eq!(
        notice(&action(ActionKind::AgentSpawn, "{\"unknown\":false}")),
        None
    );
    assert_eq!(
        notice(&action(ActionKind::AgentSpawn, "{\"landed\":true}")),
        None
    );
    assert_eq!(
        notice(&action(ActionKind::LandingAccept, "{\"landed\":1}")),
        None
    );
    assert_eq!(notice(&action(ActionKind::LandingAccept, "not json")), None);
    assert_eq!(notice(&action(ActionKind::Dispatch, "{}")), None);
}

#[test]
fn a_pending_payload_is_replaced_until_the_action_ends() {
    let (_root, db) = store();
    let id = begin(&db, ActionKind::Message, "alpha/m-1", "{\"seen\":false}").unwrap();
    note(&db, id, "{\"seen\":true}").unwrap();
    let kept = pending_key(&db, ActionKind::Message, "alpha/m-1")
        .unwrap()
        .expect("still pending");
    assert_eq!((kept.id, kept.payload.as_str()), (id, "{\"seen\":true}"));
    assert_eq!(
        code(note(&db, id, &"x".repeat(PAYLOAD_BYTES_MAX + 1))),
        "ledger_invalid"
    );
    assert_eq!(
        pending(&db, ActionKind::Message).unwrap()[0].payload,
        "{\"seen\":true}",
        "a refused note changes nothing"
    );
    finish(&db, id, Outcome::Unknown).unwrap();
    assert_eq!(code(note(&db, id, "{}")), "ledger_finished");
    assert_eq!(stored_outcome(&db, id).as_deref(), Some("unknown"));
    assert_eq!(
        code(note(&db, ActionId(id.0 + 1000), "{}")),
        "ledger_missing"
    );
}
