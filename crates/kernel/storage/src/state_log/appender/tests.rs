use super::*;
use crate::state_log::BODY_BYTES_MAX;
use rusqlite::Connection;
use std::cell::Cell;

/// A store at `state` revision 4 holding one `events` row and one log event at revision 3,
/// as the engine leaves them. Input: none. Output: the connection.
fn store() -> Connection {
    let db = Connection::open_in_memory().unwrap();
    db.execute_batch(
        "CREATE TABLE state (id INTEGER PRIMARY KEY CHECK(id=1), revision INTEGER NOT NULL);
        INSERT INTO state VALUES (1, 4);
        CREATE TABLE events (cursor INTEGER PRIMARY KEY, body TEXT NOT NULL);
        INSERT INTO events VALUES (3, 'three');",
    )
    .unwrap();
    crate::state_log::ensure(&db).unwrap();
    db.execute(
        "INSERT INTO state_log VALUES (1,3,'worktrees','Kept',1,0,x'00')",
        [],
    )
    .unwrap();
    db
}

/// What the rows compare: the `state` revision, every `events` row as cursor and body, and
/// every log row as seq, revision and body.
type Written = (i64, Vec<(i64, String)>, Vec<(i64, i64, Vec<u8>)>);

/// What the rows compare, read from the store. Input: the open transaction. Output: the
/// [`Written`] three, in a form that compares.
fn written(tx: &Transaction<'_>) -> Written {
    let revision = tx
        .query_row("SELECT revision FROM state WHERE id=1", [], |row| {
            row.get(0)
        })
        .unwrap();
    let events = tx
        .prepare("SELECT cursor, body FROM events ORDER BY cursor")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    let log = tx
        .prepare("SELECT seq, revision, body FROM state_log ORDER BY seq")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    (revision, events, log)
}

/// A fence that refuses every check. Output: the hook.
fn refusing() -> FenceCheck {
    Box::new(|_| Err(StorageError("fence_refused".into())))
}

/// A name set holding the names these rows write. Input: the name. Output: it, or none.
fn any(name: &str) -> Option<AggregateName> {
    ["worktrees", "ide"]
        .into_iter()
        .find(|known| *known == name)
        .map(AggregateName::new)
}

/// A name set that holds only `ide`. Input: the name. Output: `ide`, or none.
fn ide_only(name: &str) -> Option<AggregateName> {
    (name == "ide").then(|| AggregateName::new("ide"))
}

/// A fence that passes every check. Output: the hook.
fn passing() -> FenceCheck {
    Box::new(|_| Ok(()))
}

/// A refusing fence stops `owner` and `own` before any write, inside the open transaction:
/// the log, the `state` revision and `events` stay as they were, and `own` never builds the
/// `events` body. The same calls with a passing fence open, so the refusal is the hook's.
#[test]
fn a_refusing_fence_leaves_the_log_the_revision_and_events_unchanged() {
    let mut db = store();
    let tx = db.transaction().unwrap();
    let before = written(&tx);
    let built = Cell::new(false);
    let owner = Appender::owner(&tx, 4, refusing(), any);
    assert_eq!(owner.err(), Some(StorageError("fence_refused".into())));
    let own = Appender::own(&tx, refusing(), any, |_| {
        built.set(true);
        Ok("body".into())
    });
    assert_eq!(own.err(), Some(StorageError("fence_refused".into())));
    assert!(!built.get(), "the body is built after the fence");
    assert_eq!(written(&tx), before);
    assert!(Appender::owner(&tx, 4, passing(), any).is_ok());
    let opened = Appender::own(&tx, passing(), any, |revision| Ok(format!("r{revision}"))).unwrap();
    assert_eq!(opened.revision(), 5);
    assert_eq!(written(&tx).0, 5);
    assert!(written(&tx).1.contains(&(5, "r5".to_owned())));
}

/// `project` checks the fence again before its apply: a fence that passed at the start and
/// refuses now stops the apply from running.
#[test]
fn a_fence_that_refuses_after_the_start_stops_project_before_its_apply() {
    let mut db = store();
    let tx = db.transaction().unwrap();
    let checks = std::rc::Rc::new(Cell::new(0));
    let counted = std::rc::Rc::clone(&checks);
    let fence: FenceCheck = Box::new(move |_| {
        let checks = &counted;
        checks.set(checks.get() + 1);
        if checks.get() == 1 {
            Ok(())
        } else {
            Err(StorageError("fence_refused".into()))
        }
    });
    let appender = Appender::owner(&tx, 4, fence, any).unwrap();
    let applied = Cell::new(false);
    let projected = appender.project(|_| {
        applied.set(true);
        Ok::<_, StorageError>(())
    });
    assert_eq!(projected.err(), Some(StorageError("fence_refused".into())));
    assert!(!applied.get(), "the apply runs only after the fence passed");
    assert_eq!(checks.get(), 2);
}

/// An `events` body one byte over [`BODY_BYTES_MAX`] is refused before any write, inside
/// the open transaction; a body of exactly the bound is written as text.
#[test]
fn an_events_body_over_the_bound_is_refused_and_nothing_is_written() {
    let mut db = store();
    let tx = db.transaction().unwrap();
    let before = written(&tx);
    let over = Appender::own(&tx, passing(), any, |_| Ok("a".repeat(BODY_BYTES_MAX + 1)));
    assert_eq!(
        over.err(),
        Some(StorageError("state_log_body_bounds".into()))
    );
    assert_eq!(written(&tx), before);
    Appender::own(&tx, passing(), any, |_| Ok("a".repeat(BODY_BYTES_MAX))).unwrap();
    let kind: String = tx
        .query_row(
            "SELECT typeof(body) FROM events WHERE cursor=5",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(kind, "text");
}

/// A name the engine's check refuses is refused by an append and by a
/// snapshot with `state_log_aggregate_unknown` before any row is written, while a name it
/// reads still appends.
#[test]
fn a_name_the_engine_check_refuses_is_refused_by_an_append_and_a_snapshot() {
    let mut db = store();
    let tx = db.transaction().unwrap();
    let mut log = Appender::owner(&tx, 4, passing(), ide_only).unwrap();
    let before = written(&tx);
    let custom = AggregateName::new("custom");
    let draft = |aggregate| Draft {
        aggregate,
        kind: "Tick",
        schema: 1,
        at_ms: 1,
        body: b"{}",
    };
    let chunk = vec![Chunk {
        table: "t".into(),
        rows: Vec::new(),
    }];

    let appended = log.append(&draft(custom));
    let snapshot = log.snapshot(custom, 1, chunk);

    let unknown = Some(StorageError("state_log_aggregate_unknown".into()));
    assert_eq!(appended.err(), unknown);
    assert_eq!(snapshot.err(), unknown);
    assert_eq!(written(&tx), before);
    assert_eq!(log.append(&draft(AggregateName::new("ide"))).unwrap(), 2);
}

/// A statement on the shared transaction that moves the `state` revision between two
/// appends makes the next append refuse as stale and write nothing, so no event is logged
/// at a revision the store has left.
#[test]
fn an_append_after_the_state_revision_moved_is_refused_as_stale() {
    let mut db = store();
    let tx = db.transaction().unwrap();
    let mut log = Appender::owner(&tx, 4, passing(), any).unwrap();
    let draft = Draft {
        aggregate: AggregateName::new("ide"),
        kind: "Tick",
        schema: 1,
        at_ms: 1,
        body: b"{}",
    };
    assert_eq!(log.append(&draft).unwrap(), 2);
    tx.execute("UPDATE state SET revision=revision+1 WHERE id=1", [])
        .unwrap();
    let before = written(&tx);

    let appended = log.append(&draft);

    assert_eq!(
        appended.err(),
        Some(StorageError("state_log_revision_stale".into()))
    );
    assert_eq!(written(&tx), before);
}
