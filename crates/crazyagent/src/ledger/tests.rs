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
fn resolution_finishes_done_returns_not_started_and_never_returns_unknown_for_replay() {
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

    let resolved = resolve(&db, ActionKind::WorktreeCreate, &FileCheck).unwrap();
    assert_eq!(ids(&resolved.not_started), vec![not_started]);
    assert_eq!(ids(&resolved.unknown), vec![unknown]);
    assert_eq!(stored_outcome(&db, done).as_deref(), Some("done"));
    assert_eq!(
        stored_outcome(&db, unknown),
        None,
        "an unknown action stays open until the owner closes it"
    );
    assert_eq!(
        stored_outcome(&db, other_kind),
        None,
        "another kind is not checked"
    );
    drop(db);

    let db = open(root.path()).unwrap();
    let again = resolve(&db, ActionKind::WorktreeCreate, &FileCheck).unwrap();
    assert_eq!(ids(&again.not_started), vec![not_started]);
    assert_eq!(
        ids(&again.unknown),
        vec![unknown],
        "an unknown action is surfaced again, never offered for replay"
    );

    finish(&db, unknown, Outcome::Unknown).unwrap();
    let closed = resolve(&db, ActionKind::WorktreeCreate, &FileCheck).unwrap();
    assert!(closed.unknown.is_empty());
    assert_eq!(stored_outcome(&db, unknown).as_deref(), Some("unknown"));
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
fn the_owner_dismisses_only_a_pending_action_of_its_kind_with_the_shown_outcome() {
    let (root, db) = store();
    let start = begin(&db, ActionKind::AgentSpawn, "team-4", "{\"team_agent\":4}").unwrap();
    let accept = begin(&db, ActionKind::LandingAccept, "set-9", "{\"landed\":true}").unwrap();
    drop(db);

    let db = open(root.path()).unwrap();
    assert_eq!(
        code(dismiss(
            &db,
            ActionKind::LandingAccept,
            start.get(),
            Outcome::Unknown
        )),
        "ledger_missing"
    );
    assert_eq!(
        code(dismiss(
            &db,
            ActionKind::AgentSpawn,
            accept.get() + 100,
            Outcome::Unknown
        )),
        "ledger_missing"
    );
    assert_eq!(
        stored_outcome(&db, start),
        None,
        "a refused dismiss changes nothing"
    );
    assert_eq!(
        ids(&pending(&db, ActionKind::AgentSpawn).unwrap()),
        vec![start]
    );

    dismiss(&db, ActionKind::AgentSpawn, start.get(), Outcome::Unknown).unwrap();
    dismiss(&db, ActionKind::LandingAccept, accept.get(), Outcome::Done).unwrap();
    assert_eq!(stored_outcome(&db, start).as_deref(), Some("unknown"));
    assert_eq!(stored_outcome(&db, accept).as_deref(), Some("done"));
    assert!(pending(&db, ActionKind::AgentSpawn).unwrap().is_empty());
    assert!(pending(&db, ActionKind::LandingAccept).unwrap().is_empty());
    assert_eq!(
        code(dismiss(
            &db,
            ActionKind::AgentSpawn,
            start.get(),
            Outcome::Done
        )),
        "ledger_finished"
    );
    assert_eq!(stored_outcome(&db, start).as_deref(), Some("unknown"));
    begin(&db, ActionKind::AgentSpawn, "team-4", "{\"team_agent\":4}").unwrap();
}
