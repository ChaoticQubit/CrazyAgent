use super::*;
use crate::protocol::{ProjectId, SessionId, TaskId};
use crate::state_log::fence::{engine, seed};
use crate::state_log::replay::{Logged, redact, rows};
use crate::state_log::{Aggregate, Appender, SNAPSHOT_CHUNK};
use crate::storage::opening::Opening;
use crate::worktrees::{Admission, Identity, LeaseRecord, LeaseState, WorktreeLease};
use rusqlite::types::Value;
use rusqlite::{Connection, params};

type Tables = (Vec<Vec<Value>>, Vec<Vec<Value>>);

/// A store with the `state` row at revision 0, the owner tables holding the test host
/// started, and the two worktree tables. Output: the connection.
fn store() -> Connection {
    let db = Connection::open_in_memory().unwrap();
    db.execute_batch(
        "CREATE TABLE state (id INTEGER PRIMARY KEY CHECK(id=1), revision INTEGER NOT NULL);
        CREATE TABLE events (cursor INTEGER PRIMARY KEY, body TEXT NOT NULL);
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

/// A store no engine host runs: the `state` and `events` tables, the projects table with
/// project p1, and the two worktree tables. Output: the connection.
fn unhosted() -> Connection {
    let db = Connection::open_in_memory().unwrap();
    db.execute_batch(
        "CREATE TABLE state (id INTEGER PRIMARY KEY CHECK(id=1), revision INTEGER NOT NULL);
        CREATE TABLE events (cursor INTEGER PRIMARY KEY, body TEXT NOT NULL);
        INSERT INTO state VALUES (1,0);
        CREATE TABLE projects (id TEXT PRIMARY KEY, body TEXT NOT NULL);
        INSERT INTO projects VALUES ('p1','{}');",
    )
    .unwrap();
    crate::worktrees::initialize(&db).unwrap();
    db
}

/// A stored lease body of project p1 with no checked-out directory, as `persist_reservation`
/// writes it. Inputs: the task, the repository and the state. Output: the body text.
fn lease_body(task: &str, repository: &str, state: LeaseState) -> String {
    let identity = Identity {
        device: 1,
        inode: 2,
    };
    let record = LeaseRecord {
        lease: WorktreeLease {
            project_id: ProjectId::new("p1").unwrap(),
            task_id: TaskId::new(task).unwrap(),
            repository: repository.into(),
            path: format!("{repository}/.wt/{task}"),
            branch: format!("crazyagent/{task}"),
            owner_session: SessionId::new("s1").unwrap(),
            native_descendant: None,
            state,
        },
        admission: Admission {
            repository: repository.into(),
            root: identity.clone(),
            metadata: identity,
        },
        head: "0".repeat(40),
        directory: None,
        metadata: None,
    };
    serde_json::to_string(&record).unwrap()
}

/// Writes rows as a writer before the log wrote them: admissions of p1 at /r1 and of a
/// removed project at /r2, and t1's lease at /r1 in a state. Inputs: the store and the
/// lease state. Output: none.
fn unlogged(db: &Connection, state: LeaseState) {
    db.execute(
        "INSERT INTO repository_admissions VALUES ('p1','/r1','{}'),('gone','/r2','{}')",
        [],
    )
    .unwrap();
    db.execute(
        "INSERT INTO worktree_leases VALUES ('t1','/r1','/r1/.wt/t1',?1)",
        [lease_body("t1", "/r1", state)],
    )
    .unwrap();
}

/// The `state` revision, the logged events (0 with no log) and the `events` rows. Input: the
/// store. Output: the three counts.
fn moved(db: &Connection) -> (i64, i64, i64) {
    let revision = db
        .query_row("SELECT revision FROM state WHERE id=1", [], |row| {
            row.get(0)
        })
        .unwrap();
    let logged: bool = db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name='state_log')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let count = |sql: &str| db.query_row(sql, [], |row| row.get(0)).unwrap();
    let events = if logged {
        count("SELECT COUNT(*) FROM state_log")
    } else {
        0
    };
    (revision, events, count("SELECT COUNT(*) FROM events"))
}

/// Asserts that replay of the log rebuilds both tables as they stand. Input: the store.
/// Output: none.
fn replays_to_live(db: &mut Connection) {
    let live = tables(db);
    let tx = db.transaction().unwrap();
    rebuild(&tx).unwrap();
    tx.commit().unwrap();
    assert_eq!(tables(db), live, "replay equals the live tables");
}

/// v2.5 L6 Q8: at open, a lease a stop left Reserved with no event is settled Uncertain and
/// the removed project's admission is forgotten; rows changed, so one snapshot run follows
/// at one revision through the open's proof, and replay equals the live tables.
#[cfg(unix)]
#[test]
fn an_open_sweep_that_changes_rows_logs_one_snapshot_run() {
    let mut db = unhosted();
    unlogged(&db, LeaseState::Reserved);
    assert!(sweep(&db, Opening::for_test(5)).unwrap());
    let live = tables(&db);
    assert_eq!(
        live.0.len(),
        1,
        "the removed project's admission is forgotten"
    );
    assert!(matches!(&live.1[0][3], Value::Text(body) if body.contains("Uncertain")));
    assert_eq!(
        moved(&db),
        (1, 3, 1),
        "one revision: two chunks, the closing event"
    );
    replays_to_live(&mut db);
}

/// v2.5 L6 Q8: an open whose sweep changes no row builds no appender, so a plain start
/// moves no revision and writes no event.
#[cfg(unix)]
#[test]
fn an_open_sweep_that_changes_nothing_moves_no_revision() {
    let db = unhosted();
    unlogged(&db, LeaseState::Uncertain);
    db.execute("DELETE FROM repository_admissions WHERE project='gone'", [])
        .unwrap();
    let before = tables(&db);
    assert!(!sweep(&db, Opening::for_test(5)).unwrap());
    assert_eq!(tables(&db), before);
    assert_eq!(moved(&db), (0, 0, 0));
}

/// v2.5 L6 Q1 refusal row at its call site: once a host of the store runs, the open's sweep
/// is refused and rolls back whole, its lease left Reserved.
#[cfg(unix)]
#[test]
fn an_open_sweep_is_refused_once_a_host_of_the_store_runs() {
    let db = store();
    db.execute(
        "INSERT INTO worktree_leases VALUES ('t1','/r1','/r1/.wt/t1',?1)",
        [lease_body("t1", "/r1", LeaseState::Reserved)],
    )
    .unwrap();
    let before = tables(&db);
    assert_eq!(
        code(sweep(&db, Opening::for_test(5))),
        Some("state_log_opening_host_running".into())
    );
    assert_eq!(tables(&db), before, "the sweep rolled back");
    assert_eq!(moved(&db), (0, 0, 0));
}

/// v2.5 L6 step 12: rows written before the log reach replay through one snapshot run at
/// one revision, through the open's proof.
#[test]
fn the_first_run_puts_rows_written_before_the_log_on_it() {
    let mut db = unhosted();
    unlogged(&db, LeaseState::Reserved);
    let tx = db.transaction().unwrap();
    assert!(first_run(&tx, Opening::for_test(3)).unwrap());
    tx.commit().unwrap();
    assert_eq!(moved(&db), (1, 3, 1));
    assert_eq!(
        (tables(&db).0.len(), tables(&db).1.len()),
        (2, 1),
        "the run changes no row"
    );
    replays_to_live(&mut db);
}

/// v2.5 L6 step 12: a store with no worktree row, no worktree tables or no `state` table
/// gets no run, so the step moves no revision.
#[test]
fn the_first_run_writes_nothing_without_worktree_rows() {
    let mut db = unhosted();
    let tx = db.transaction().unwrap();
    assert!(!first_run(&tx, Opening::for_test(3)).unwrap());
    tx.commit().unwrap();
    assert_eq!(moved(&db), (0, 0, 0));

    let mut bare = Connection::open_in_memory().unwrap();
    bare.execute_batch(
        "CREATE TABLE state (id INTEGER PRIMARY KEY CHECK(id=1), revision INTEGER NOT NULL);
        CREATE TABLE events (cursor INTEGER PRIMARY KEY, body TEXT NOT NULL);
        INSERT INTO state VALUES (1,0);",
    )
    .unwrap();
    let tx = bare.transaction().unwrap();
    assert!(
        !first_run(&tx, Opening::for_test(3)).unwrap(),
        "no worktree tables"
    );
    drop(tx);

    let mut stateless = Connection::open_in_memory().unwrap();
    crate::worktrees::initialize(&stateless).unwrap();
    stateless
        .execute(
            "INSERT INTO repository_admissions VALUES ('p1','/r1','{}')",
            [],
        )
        .unwrap();
    let tx = stateless.transaction().unwrap();
    assert!(
        !first_run(&tx, Opening::for_test(3)).unwrap(),
        "no state table"
    );
}
