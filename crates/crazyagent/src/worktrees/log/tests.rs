use super::*;
use crate::state_log::fence::{engine, seed};
use crate::state_log::replay::{Logged, redact, rows};
use crate::state_log::{Aggregate, Appender, SNAPSHOT_CHUNK};
use rusqlite::types::Value;
use rusqlite::{Connection, params};

type Tables = (Vec<Vec<Value>>, Vec<Vec<Value>>);

/// A store with the `state` row at revision 0, the owner tables holding the test host
/// started, and the two worktree tables. Output: the connection.
fn store() -> Connection {
    let db = Connection::open_in_memory().unwrap();
    db.execute_batch(
        "CREATE TABLE state (id INTEGER PRIMARY KEY CHECK(id=1), revision INTEGER NOT NULL);
        INSERT INTO state VALUES (1,0);",
    )
    .unwrap();
    seed(&db);
    crate::worktrees::initialize(&db).unwrap();
    db
}

/// Both worktree tables, each ordered by all its columns. Input: the store. Output: the
/// admissions and the leases.
fn tables(db: &Connection) -> Tables {
    (
        rows(db, "repository_admissions").unwrap(),
        rows(db, "worktree_leases").unwrap(),
    )
}

/// The refusal code of a result, none when it succeeded. Input: the result. Output: the code.
fn code<T>(result: Result<T, EngineError>) -> Option<String> {
    result.err().map(|error| error.0)
}

fn admitted(project: &str, repository: &str) -> Event {
    Event::Admitted(Admitted {
        project: project.into(),
        repository: repository.into(),
        body: format!("{{\"repository\":\"{repository}\"}}"),
    })
}

fn reserved(task: &str, repository: &str, body: &str) -> Event {
    Event::Reserved(Reserved {
        task: task.into(),
        repository: repository.into(),
        path: format!("{repository}/.wt/{task}"),
        body: body.into(),
    })
}

fn saved(task: &str, repository: &str, body: &str) -> Event {
    Event::Saved(Saved {
        task: task.into(),
        repository: repository.into(),
        body: body.into(),
    })
}

/// Records events in one appending transaction of the engine host. Inputs: the store and
/// the events. Output: each event's seq.
fn write(db: &mut Connection, events: &[Event]) -> Vec<u64> {
    let tx = db.transaction().unwrap();
    let mut appender = Appender::own(&tx, engine()).unwrap();
    let seqs = events
        .iter()
        .map(|event| record(&mut appender, 5, event).unwrap())
        .collect();
    drop(appender);
    tx.commit().unwrap();
    seqs
}

/// v2.5 L6 replay row: the three writes leave both tables as replay of their events
/// rebuilds them, and a skipped (redacted) save shows in the rebuilt lease.
#[test]
fn replay_rebuilds_both_worktree_tables_as_the_writes_left_them() {
    let mut db = store();
    write(
        &mut db,
        &[
            admitted("p1", "/r1"),
            admitted("p2", "/r2"),
            reserved("t1", "/r1", "{\"v\":1}"),
        ],
    );
    let seqs = write(
        &mut db,
        &[
            saved("t1", "/r1", "{\"v\":2}"),
            reserved("t2", "/r2", "{\"v\":1}"),
        ],
    );
    let live = tables(&db);
    assert_eq!((live.0.len(), live.1.len()), (2, 2));
    assert!(
        live.1
            .iter()
            .any(|row| row[3] == Value::Text("{\"v\":2}".into()))
    );

    let tx = db.transaction().unwrap();
    assert_eq!(rebuild(&tx).unwrap(), 5, "the last seq read");
    tx.commit().unwrap();
    assert_eq!(tables(&db), live, "replay equals the live tables");

    let tx = db.transaction().unwrap();
    redact(&tx, seqs[0], &engine()).unwrap();
    rebuild(&tx).unwrap();
    tx.commit().unwrap();
    let skipped = tables(&db);
    assert_eq!(skipped.0, live.0);
    assert_ne!(skipped.1, live.1, "a skipped save shows");
    assert!(
        skipped
            .1
            .iter()
            .any(|row| row[3] == Value::Text("{\"v\":1}".into()))
    );
}

/// v2.5 L6: a snapshot run of the tables as they stand, written in more than one chunk,
/// and the events after it replay to the same rows; rebuilding twice gives the same rows.
#[test]
fn a_snapshot_run_and_the_events_after_it_replay_to_the_same_rows() {
    let mut db = store();
    let body = format!("{{\"pad\":\"{}\"}}", "x".repeat(3_000));
    for index in 0..40 {
        db.execute(
            "INSERT INTO worktree_leases VALUES (?1,?2,?3,?4)",
            params![
                format!("t{index}"),
                "/r1",
                format!("/r1/.wt/t{index}"),
                body
            ],
        )
        .unwrap();
    }
    db.execute(
        "INSERT INTO repository_admissions VALUES ('p1','/r1','{}')",
        [],
    )
    .unwrap();
    let tx = db.transaction().unwrap();
    let mut appender = Appender::own(&tx, engine()).unwrap();
    let done = snapshot(&mut appender, 9).unwrap();
    record(&mut appender, 9, &saved("t3", "/r1", "{\"v\":2}")).unwrap();
    record(&mut appender, 9, &admitted("p2", "/r2")).unwrap();
    drop(appender);
    tx.commit().unwrap();
    crate::state_log::verify_snapshot(&db, done).unwrap();
    let chunks: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM state_log WHERE kind=?1",
            [SNAPSHOT_CHUNK],
            |row| row.get(0),
        )
        .unwrap();
    assert!(chunks >= 2, "{chunks} chunks: 40 leases outgrow one body");
    let live = tables(&db);
    assert_eq!((live.0.len(), live.1.len()), (2, 40));
    for _ in 0..2 {
        let tx = db.transaction().unwrap();
        rebuild(&tx).unwrap();
        tx.commit().unwrap();
        assert_eq!(tables(&db), live);
    }
}

/// v2.5 L6: an event that does not fit the tables is refused, not applied: a save of a
/// lease that is not there, an admission or a lease that already is.
#[test]
fn an_event_that_does_not_fit_the_tables_is_refused() {
    let mut db = store();
    let tx = db.transaction().unwrap();
    assert_eq!(
        code(apply(&tx, &saved("t9", "/r9", "{}"))),
        Some("worktrees_log_lease_missing".into())
    );
    apply(&tx, &admitted("p1", "/r1")).unwrap();
    assert_eq!(
        code(apply(&tx, &admitted("p1", "/r1"))),
        Some("worktrees_log_admission_exists".into())
    );
    apply(&tx, &reserved("t1", "/r1", "{}")).unwrap();
    assert_eq!(
        code(apply(&tx, &reserved("t1", "/r1", "{}"))),
        Some("worktrees_log_lease_exists".into())
    );
    let before = tables(&tx);
    assert_eq!((before.0.len(), before.1.len()), (1, 1));
}

/// v2.5 L6: replay reads only the three worktrees kinds at schema 1 with exactly their
/// fields; anything else is refused.
#[test]
fn replay_refuses_a_worktrees_event_it_does_not_know() {
    let logged = |kind: &str, schema: u16, body: &str| Logged {
        seq: 1,
        revision: 1,
        aggregate: Aggregate::Worktrees,
        kind: kind.into(),
        schema,
        at_ms: 1,
        body: body.as_bytes().to_vec(),
    };
    let good = "{\"task\":\"t1\",\"repository\":\"/r1\",\"body\":\"{}\"}";
    assert!(matches!(
        decode(&logged(KIND_SAVED, 1, good)),
        Ok(Event::Saved(_))
    ));
    for (kind, schema, body) in [
        ("LeaseDropped", 1, good),
        (KIND_SAVED, 2, good),
        (
            KIND_SAVED,
            1,
            "{\"task\":\"t1\",\"repository\":\"/r1\",\"body\":\"{}\",\"x\":1}",
        ),
        (KIND_ADMITTED, 1, good),
    ] {
        assert_eq!(
            code(decode(&logged(kind, schema, body))),
            Some("worktrees_log_event_invalid".into()),
            "{kind} {schema} {body}"
        );
    }
}
