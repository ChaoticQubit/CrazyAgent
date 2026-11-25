use super::*;
use rusqlite::Connection;

/// An in-memory store holding the `state` row at `revision` and one event body per cursor
/// in `logged`, in the two tables the engine store keeps. Inputs: the revision and the
/// cursors. Output: the connection.
fn store(revision: u64, logged: impl IntoIterator<Item = u64>) -> Connection {
    let db = Connection::open_in_memory().unwrap();
    db.execute_batch(
        "CREATE TABLE state (id INTEGER PRIMARY KEY CHECK(id=1), revision INTEGER NOT NULL);
        CREATE TABLE events (cursor INTEGER PRIMARY KEY, body TEXT NOT NULL);",
    )
    .unwrap();
    db.execute("INSERT INTO state VALUES (1,?1)", [revision as i64])
        .unwrap();
    for cursor in logged {
        db.execute(
            "INSERT INTO events VALUES (?1,?2)",
            rusqlite::params![cursor as i64, body(cursor)],
        )
        .unwrap();
    }
    db
}

/// The body the store keeps for one cursor here. Input: the cursor. Output: the text.
fn body(cursor: u64) -> String {
    format!("{{\"cursor\":{cursor}}}")
}

#[test]
fn the_bodies_after_a_cursor_come_in_cursor_order_up_to_the_revision() {
    let mut db = store(3, [3, 1, 2]);
    let tx = db.transaction().unwrap();
    assert_eq!(
        logged(&tx, 0, 3).unwrap(),
        Some(vec![body(1), body(2), body(3)])
    );
    assert_eq!(logged(&tx, 2, 3).unwrap(), Some(vec![body(3)]));
    assert_eq!(logged(&tx, 3, 3).unwrap(), Some(Vec::new()));
}

#[test]
fn a_cursor_below_the_retained_events_or_ahead_of_the_store_reads_none() {
    let mut db = store(5, [4, 5]);
    let tx = db.transaction().unwrap();
    for after in [0, 1, 2, 6] {
        assert_eq!(logged(&tx, after, 5).unwrap(), None, "after {after}");
    }
    assert_eq!(logged(&tx, 3, 5).unwrap(), Some(vec![body(4), body(5)]));
    let mut empty = store(0, []);
    let tx = empty.transaction().unwrap();
    assert_eq!(logged(&tx, 0, 0).unwrap(), Some(Vec::new()));
}

#[test]
fn one_read_returns_at_most_the_catch_up_bound() {
    let mut db = store(130, 1..=130);
    let tx = db.transaction().unwrap();
    let first = logged(&tx, 0, 130).unwrap().unwrap();
    assert_eq!(first.len(), CATCH_UP_MAX);
    assert_eq!(first.last(), Some(&body(128)));
    assert_eq!(
        logged(&tx, 128, 130).unwrap(),
        Some(vec![body(129), body(130)])
    );
}

#[test]
fn persisting_moves_the_revision_logs_the_body_and_prunes_past_the_retained_window() {
    let mut db = store(3, 1..=3);
    let tx = db.transaction().unwrap();
    persist(&tx, 4, &body(4), 2).unwrap();
    let revision: i64 = tx
        .query_row("SELECT revision FROM state WHERE id=1", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(revision, 4);
    let kept: Vec<i64> = tx
        .prepare("SELECT cursor FROM events ORDER BY cursor")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(kept, vec![3, 4]);
    assert_eq!(logged(&tx, 2, 4).unwrap(), Some(vec![body(3), body(4)]));
    persist(&tx, 5, &body(5), 128).unwrap();
    assert_eq!(logged(&tx, 4, 5).unwrap(), Some(vec![body(5)]));
}

#[test]
fn a_store_error_keeps_the_sqlite_text() {
    let mut db = Connection::open_in_memory().unwrap();
    let tx = db.transaction().unwrap();
    let refused = logged(&tx, 0, 0).unwrap_err();
    assert!(refused.0.contains("no such table: events"), "{refused}");
    let refused = persist(&tx, 1, &body(1), 128).unwrap_err();
    assert!(refused.0.contains("no such table: state"), "{refused}");
}
