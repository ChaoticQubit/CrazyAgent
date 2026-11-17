use super::*;
use crate::state_log::{Appender, Draft, ensure};
use rusqlite::{Connection, params};
use serde_json::json;
use std::path::Path;

const LEASES: &str =
    "CREATE TABLE leases (id INTEGER PRIMARY KEY, path TEXT NOT NULL, at_ms INTEGER NOT NULL)";
const AGENTS: &str = "CREATE TABLE agents (id INTEGER PRIMARY KEY, pid INTEGER NOT NULL, code INTEGER, signal INTEGER)";

type Projection = fn(&Connection, &Logged) -> Result<(), EngineError>;

/// An in-memory engine store with the `state` row at revision 0 and the leases table, as
/// storage.rs and a projection create them. Output: the connection.
fn store() -> Connection {
    let db = Connection::open_in_memory().unwrap();
    db.execute_batch(&format!(
        "CREATE TABLE state (id INTEGER PRIMARY KEY CHECK(id=1), revision INTEGER NOT NULL);
        INSERT INTO state VALUES (1,0); {LEASES};"
    ))
    .unwrap();
    fence::seed(&db);
    db
}

/// The refusal code of a result, none when it succeeded. Input: the result. Output: the code.
fn code<T>(result: Result<T, EngineError>) -> Option<String> {
    result.err().map(|error| error.0)
}

/// The leases projection: a snapshot chunk loads its rows, a taken lease is a row stamped
/// with the event's time, a released one is gone. Inputs: the store and the event. Output:
/// any refusal.
fn leases(db: &Connection, event: &Logged) -> Result<(), EngineError> {
    let insert = "INSERT INTO leases VALUES (?1,?2,?3)";
    if event.kind == SNAPSHOT_CHUNK {
        let chunk = chunk(event)?;
        assert_eq!(chunk.table, "leases");
        for row in chunk.rows {
            db.execute(
                insert,
                params![
                    row["id"].as_i64(),
                    row["path"].as_str(),
                    row["at_ms"].as_i64()
                ],
            )
            .map_err(error)?;
        }
        return Ok(());
    }
    let body: serde_json::Value = serde_json::from_slice(&event.body).map_err(error)?;
    let changed = match event.kind.as_str() {
        "LeaseTaken" => db.execute(
            insert,
            params![
                body["id"].as_i64(),
                body["path"].as_str(),
                event.at_ms as i64
            ],
        ),
        "LeaseReleased" => db.execute("DELETE FROM leases WHERE id=?1", [body["id"].as_i64()]),
        _ => return Err(error("projection_kind")),
    };
    changed.map(|_| ()).map_err(error)
}

/// The agents projection: a started agent is a row, and its process exit records the code
/// or signal it ended with. Inputs: the store and the event. Output: any refusal.
fn agents(db: &Connection, event: &Logged) -> Result<(), EngineError> {
    let changed = match event.kind.as_str() {
        "AgentStarted" => {
            let body: serde_json::Value = serde_json::from_slice(&event.body).map_err(error)?;
            db.execute(
                "INSERT INTO agents VALUES (?1,?2,NULL,NULL)",
                params![body["id"].as_i64(), body["pid"].as_i64()],
            )
        }
        PROCESS_EXITED => {
            let exit: ProcessExited = serde_json::from_slice(&event.body).map_err(error)?;
            db.execute(
                "UPDATE agents SET code=?1, signal=?2 WHERE pid=?3",
                params![exit.code, exit.signal, exit.pid],
            )
        }
        _ => return Err(error("projection_kind")),
    };
    changed.map(|_| ()).map_err(error)
}

/// Appends one event in its own transaction and applies it to the live tables in that
/// transaction, as a synchronous projection does. Inputs: the store, the aggregate, the
/// kind, the body, the time and the projection. Output: the event's seq.
fn record(
    db: &mut Connection,
    aggregate: Aggregate,
    kind: &str,
    body: serde_json::Value,
    at_ms: u64,
    project: Projection,
) -> u64 {
    let body = serde_json::to_vec(&body).unwrap();
    let tx = db.transaction().unwrap();
    let mut log = Appender::own(&tx, fence::engine()).unwrap();
    let draft = Draft {
        aggregate,
        kind,
        schema: 1,
        at_ms,
        body: &body,
    };
    let seq = log.append(&draft).unwrap();
    let event = Logged {
        seq,
        revision: log.revision(),
        aggregate,
        kind: kind.into(),
        schema: 1,
        at_ms,
        body: body.clone(),
    };
    project(&tx, &event).unwrap();
    tx.commit().unwrap();
    seq
}

/// Rebuilds one aggregate's tables from the live store's log into a fresh store, skipping
/// one seq when asked, as a projection that misses an event would. Inputs: the live store,
/// the aggregate, the tables' schema, the projection and the seq to skip. Output: the
/// rebuilt store and the last seq replay read.
fn rebuild(
    live: &Connection,
    aggregate: Aggregate,
    schema: &str,
    project: Projection,
    skip: Option<u64>,
) -> Result<(Connection, u64), EngineError> {
    let rebuilt = Connection::open_in_memory().unwrap();
    rebuilt.execute_batch(schema).unwrap();
    let last = replay(live, aggregate, 0, |event| {
        if Some(event.seq) == skip {
            return Ok(());
        }
        project(&rebuilt, event)
    })?;
    Ok((rebuilt, last))
}

/// Plants one row in the log outside the appender, as a damaged or pruned store holds it.
/// Inputs: the store, the seq, the revision, the aggregate name and the schema. Output: none.
fn plant(db: &Connection, seq: i64, revision: i64, aggregate: &str, schema: i64) {
    ensure(db).unwrap();
    db.execute(
        "INSERT INTO state_log VALUES (?1,?2,?3,'Planted',?4,0,x'7b7d')",
        params![seq, revision, aggregate, schema],
    )
    .unwrap();
}

/// Every logged event's seq, revision, aggregate and kind, in seq order. Input: the store.
/// Output: the rows.
fn logged_rows(db: &Connection) -> Vec<(i64, i64, String, String)> {
    let mut statement = db
        .prepare("SELECT seq, revision, aggregate, kind FROM state_log ORDER BY seq")
        .unwrap();
    statement
        .query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

/// The milliseconds since the Unix epoch now, read from the system. Output: the time.
fn system_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

/// Moves every WAL frame into the database file and empties the WAL, failing when a reader
/// kept it busy. Input: the store. Output: none.
fn checkpoint(db: &Connection) {
    let (busy, _, _): (i64, i64, i64) = db
        .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })
        .unwrap();
    assert_eq!(busy, 0, "the checkpoint ran");
}

/// Whether a file holds the marker bytes anywhere. Inputs: the path and the marker. Output:
/// true when found.
fn holds(path: &Path, marker: &[u8]) -> bool {
    std::fs::read(path)
        .unwrap()
        .windows(marker.len())
        .any(|window| window == marker)
}

#[test]
fn a_replay_rebuilds_the_live_table_and_a_projection_that_skips_an_event_differs() {
    let mut live = store();
    let worktrees = Aggregate::Worktrees;
    record(
        &mut live,
        worktrees,
        "LeaseTaken",
        json!({ "id": 1, "path": "/a" }),
        10,
        leases,
    );
    let skipped = record(
        &mut live,
        worktrees,
        "LeaseTaken",
        json!({ "id": 2, "path": "/b" }),
        11,
        leases,
    );
    record(
        &mut live,
        worktrees,
        "LeaseTaken",
        json!({ "id": 3, "path": "/c" }),
        12,
        leases,
    );
    record(
        &mut live,
        worktrees,
        "LeaseReleased",
        json!({ "id": 1 }),
        13,
        leases,
    );
    let expected = rows(&live, "leases").unwrap();
    assert_eq!(expected.len(), 2);
    let (rebuilt, last) = rebuild(&live, worktrees, LEASES, leases, None).unwrap();
    assert_eq!(last, 4);
    assert_eq!(rows(&rebuilt, "leases").unwrap(), expected);
    let (missed, _) = rebuild(&live, worktrees, LEASES, leases, Some(skipped)).unwrap();
    assert_ne!(
        rows(&missed, "leases").unwrap(),
        expected,
        "a projection that skips an event must not rebuild equal"
    );
}

#[test]
fn replay_reads_at_most_its_batch_and_a_store_without_a_log_replays_nothing() {
    let mut db = store();
    assert_eq!(batch(&db, 0, 0).unwrap(), Vec::new());
    assert_eq!(
        replay(&db, Aggregate::Ide, 7, |_| Err(error("applied"))).unwrap(),
        7
    );
    let tx = db.transaction().unwrap();
    let mut log = Appender::own(&tx, fence::engine()).unwrap();
    let tick = Draft {
        aggregate: Aggregate::Ide,
        kind: "Tick",
        schema: 1,
        at_ms: 1,
        body: b"{}",
    };
    for _ in 0..=REPLAY_BATCH_MAX {
        log.append(&tick).unwrap();
    }
    tx.commit().unwrap();
    let last = REPLAY_BATCH_MAX as u64 + 1;
    assert_eq!(batch(&db, 0, 0).unwrap().len(), REPLAY_BATCH_MAX);
    assert_eq!(batch(&db, last - 1, 1).unwrap().len(), 1);
    let mut seen = Vec::new();
    let read = replay(&db, Aggregate::Ide, 0, |event| {
        seen.push(event.seq);
        Ok(())
    });
    assert_eq!(read, Ok(last));
    assert_eq!(seen, (1..=last).collect::<Vec<_>>());
}

#[test]
fn replay_refuses_a_gap_in_seq_or_a_revision_that_goes_down_and_resumes_after_a_seq() {
    let replayed = |planted: &[(i64, i64)], after: u64| {
        let db = store();
        for (seq, revision) in planted {
            plant(&db, *seq, *revision, "worktrees", 1);
        }
        code(replay(&db, Aggregate::Worktrees, after, |_| Ok(())))
    };
    let gap = Some("state_log_replay_gap".to_string());
    assert_eq!(replayed(&[(2, 1)], 0), gap, "a log whose start is missing");
    assert_eq!(replayed(&[(1, 1), (3, 1)], 0), gap);
    assert_eq!(replayed(&[(1, 1)], u64::MAX), gap);
    assert_eq!(
        replayed(&[(1, 2), (2, 1)], 0),
        Some("state_log_replay_revision".into())
    );
    assert_eq!(
        replayed(&[(1, 1), (2, 1), (3, 2)], 1),
        None,
        "replay resumes after the last seq it applied"
    );
}

#[test]
fn replay_hands_apply_only_its_aggregate_but_checks_every_event() {
    let mut db = store();
    let tx = db.transaction().unwrap();
    let mut log = Appender::own(&tx, fence::engine()).unwrap();
    for aggregate in [
        Aggregate::Worktrees,
        Aggregate::Ide,
        Aggregate::Worktrees,
        Aggregate::Ide,
    ] {
        log.append(&Draft {
            aggregate,
            kind: "Tick",
            schema: 1,
            at_ms: 1,
            body: b"{}",
        })
        .unwrap();
    }
    tx.commit().unwrap();
    let mut seen = Vec::new();
    replay(&db, Aggregate::Ide, 0, |event| {
        seen.push((event.seq, event.aggregate));
        Ok(())
    })
    .unwrap();
    assert_eq!(seen, [(2, Aggregate::Ide), (4, Aggregate::Ide)]);
    plant(&db, 6, 1, "worktrees", 1);
    assert_eq!(
        code(replay(&db, Aggregate::Ide, 0, |_| Ok(()))),
        Some("state_log_replay_gap".into()),
        "a gap among another aggregate's events"
    );
}

#[test]
fn an_unknown_aggregate_an_out_of_range_schema_or_an_oversize_body_is_refused() {
    let db = store();
    plant(&db, 1, 1, "nowhere", 1);
    assert_eq!(
        code(replay(&db, Aggregate::Worktrees, 0, |_| Ok(()))),
        Some("state_log_replay_aggregate".into())
    );
    let db = store();
    plant(&db, 1, 1, "worktrees", 70_000);
    assert_eq!(
        code(replay(&db, Aggregate::Worktrees, 0, |_| Ok(()))),
        Some("state_log_replay_row".into())
    );
    let db = store();
    db.execute_batch(
        "CREATE TABLE state_log (seq INTEGER PRIMARY KEY, revision INTEGER, aggregate TEXT,
        kind TEXT, schema INTEGER, at_ms INTEGER, body BLOB)",
    )
    .unwrap();
    db.execute(
        "INSERT INTO state_log VALUES (1,1,'worktrees','Planted',1,0,?1)",
        [vec![b' '; BODY_BYTES_MAX + 1]],
    )
    .unwrap();
    assert_eq!(
        code(replay(&db, Aggregate::Worktrees, 0, |_| Ok(()))),
        Some("state_log_replay_row".into()),
        "a body over the cap in a log made without the table's checks"
    );
}

#[test]
fn every_aggregate_is_read_back_under_its_stored_name() {
    for aggregate in [
        Aggregate::Worktrees,
        Aggregate::Ide,
        Aggregate::Memory,
        Aggregate::Projects,
        Aggregate::Landing,
        Aggregate::Settings,
        Aggregate::Plugins,
        Aggregate::Sessions,
        Aggregate::Intake,
        Aggregate::Engine,
        Aggregate::Accounts,
        Aggregate::Team,
    ] {
        assert_eq!(parse(aggregate.name()), Some(aggregate), "{aggregate:?}");
    }
    assert_eq!(parse("Worktrees"), None);
    assert_eq!(parse(""), None);
}

#[test]
fn an_apply_error_stops_the_replay_and_is_returned() {
    let mut live = store();
    for id in 1..=3 {
        record(
            &mut live,
            Aggregate::Worktrees,
            "LeaseTaken",
            json!({ "id": id, "path": "/p" }),
            1,
            leases,
        );
    }
    let mut calls = 0;
    let result = replay(&live, Aggregate::Worktrees, 0, |event| {
        calls += 1;
        if event.seq == 2 {
            return Err(error("projection_failed"));
        }
        Ok(())
    });
    assert_eq!(code(result), Some("projection_failed".into()));
    assert_eq!(calls, 2);
}

#[test]
fn a_manual_clock_reads_what_it_was_set_to_and_the_system_clock_cannot_be_set() {
    let clock = Clock::manual(5);
    assert_eq!(clock.now_ms(), 5);
    clock.set_ms(9).unwrap();
    assert_eq!(clock.now_ms(), 9);
    let before = system_ms();
    let now = Clock::System.now_ms();
    let after = system_ms();
    assert!(
        before <= now && now <= after,
        "{before} <= {now} <= {after}"
    );
    assert_eq!(
        code(Clock::System.set_ms(1)),
        Some("state_log_clock_system".into())
    );
}

#[test]
fn a_process_exit_enters_the_log_and_replays_to_the_same_row() {
    let mut live = store();
    live.execute_batch(AGENTS).unwrap();
    let team = Aggregate::Team;
    record(
        &mut live,
        team,
        "AgentStarted",
        json!({ "id": 1, "pid": 4242 }),
        1,
        agents,
    );
    record(
        &mut live,
        team,
        "AgentStarted",
        json!({ "id": 2, "pid": 4343 }),
        2,
        agents,
    );
    let exit = ProcessExited {
        pid: 4242,
        code: Some(3),
        signal: None,
    };
    let body = serde_json::to_value(exit).unwrap();
    record(&mut live, team, PROCESS_EXITED, body, 3, agents);
    let expected = rows(&live, "agents").unwrap();
    assert_eq!(
        expected,
        vec![
            vec![
                Value::Integer(1),
                Value::Integer(4242),
                Value::Integer(3),
                Value::Null
            ],
            vec![
                Value::Integer(2),
                Value::Integer(4343),
                Value::Null,
                Value::Null
            ],
        ]
    );
    let (rebuilt, _) = rebuild(&live, team, AGENTS, agents, None).unwrap();
    assert_eq!(rows(&rebuilt, "agents").unwrap(), expected);
    assert!(
        serde_json::from_str::<ProcessExited>(r#"{"pid":1,"code":null,"signal":9,"cpu":1}"#)
            .is_err()
    );
}

#[test]
fn rows_are_ordered_by_every_column_and_a_bad_table_name_is_refused() {
    let first = Connection::open_in_memory().unwrap();
    first
        .execute_batch("CREATE TABLE pairs (a INTEGER, b TEXT); INSERT INTO pairs VALUES (2,'x'),(1,'z'),(1,'y');")
        .unwrap();
    let second = Connection::open_in_memory().unwrap();
    second
        .execute_batch("CREATE TABLE pairs (a INTEGER, b TEXT); INSERT INTO pairs VALUES (1,'y'),(2,'x'),(1,'z');")
        .unwrap();
    let pair = |a: i64, b: &str| vec![Value::Integer(a), Value::Text(b.into())];
    let expected = vec![pair(1, "y"), pair(1, "z"), pair(2, "x")];
    assert_eq!(rows(&first, "pairs").unwrap(), expected);
    assert_eq!(rows(&second, "pairs").unwrap(), expected);
    let longest = "t".repeat(64);
    let too_long = "t".repeat(65);
    for table in ["", "pairs;", "pairs b", "\"pairs\"", too_long.as_str()] {
        assert_eq!(
            code(rows(&first, table)),
            Some("state_log_replay_table".into()),
            "{table:?}"
        );
    }
    assert_ne!(
        code(rows(&first, &longest)),
        Some("state_log_replay_table".into()),
        "a 64-byte name passes the check and reaches SQLite"
    );
}

#[test]
fn a_log_that_opens_with_a_snapshot_run_rebuilds_equal_and_a_changed_chunk_is_refused() {
    let mut live = store();
    live.execute_batch("INSERT INTO leases VALUES (1,'/a',5),(2,'/b',5);")
        .unwrap();
    let tx = live.transaction().unwrap();
    let mut log = Appender::own(&tx, fence::engine()).unwrap();
    let run = vec![Chunk {
        table: "leases".into(),
        rows: vec![
            json!({ "id": 1, "path": "/a", "at_ms": 5 }),
            json!({ "id": 2, "path": "/b", "at_ms": 5 }),
        ],
    }];
    assert_eq!(log.snapshot(Aggregate::Worktrees, 5, run).unwrap(), 2);
    tx.commit().unwrap();
    let worktrees = Aggregate::Worktrees;
    record(
        &mut live,
        worktrees,
        "LeaseTaken",
        json!({ "id": 3, "path": "/c" }),
        6,
        leases,
    );
    record(
        &mut live,
        worktrees,
        "LeaseReleased",
        json!({ "id": 1 }),
        7,
        leases,
    );
    let expected = rows(&live, "leases").unwrap();
    assert_eq!(expected.len(), 2);
    let (rebuilt, last) = rebuild(&live, worktrees, LEASES, leases, None).unwrap();
    assert_eq!(last, 4);
    assert_eq!(rows(&rebuilt, "leases").unwrap(), expected);
    let first = batch(&live, 0, 0).unwrap().remove(0);
    assert_eq!(chunk(&first).unwrap().rows.len(), 2);
    let foreign = Logged {
        aggregate: Aggregate::Ide,
        ..first.clone()
    };
    let taken = batch(&live, 2, 1).unwrap().remove(0);
    for event in [&foreign, &taken] {
        assert_eq!(
            code(chunk(event)),
            Some("state_log_replay_chunk".into()),
            "{}",
            event.kind
        );
    }
    live.execute(
        "UPDATE state_log SET body=CAST(replace(CAST(body AS TEXT), '/b', '/x') AS BLOB) WHERE seq=1",
        [],
    )
    .unwrap();
    assert_eq!(
        code(rebuild(&live, worktrees, LEASES, leases, None)),
        Some("state_log_snapshot_hash".into())
    );
}

#[test]
fn a_redacted_event_keeps_its_place_replay_skips_it_and_the_rebuild_stays_equal() {
    let mut live = store();
    let worktrees = Aggregate::Worktrees;
    let taken = record(
        &mut live,
        worktrees,
        "LeaseTaken",
        json!({ "id": 1, "path": "/secret" }),
        1,
        leases,
    );
    let kept = record(
        &mut live,
        worktrees,
        "LeaseTaken",
        json!({ "id": 2, "path": "/b" }),
        2,
        leases,
    );
    record(
        &mut live,
        worktrees,
        "LeaseReleased",
        json!({ "id": 1 }),
        3,
        leases,
    );
    let before = logged_rows(&live);
    let secure: i64 = live
        .query_row("PRAGMA secure_delete", [], |row| row.get(0))
        .unwrap();
    let tx = live.transaction().unwrap();
    redact(&tx, taken, &fence::engine()).unwrap();
    redact(&tx, taken, &fence::engine()).unwrap();
    let after: i64 = tx
        .query_row("PRAGMA secure_delete", [], |row| row.get(0))
        .unwrap();
    assert_eq!(after, secure, "redact puts secure_delete back");
    tx.commit().unwrap();
    assert_eq!(
        logged_rows(&live),
        before,
        "seq, revision, aggregate and kind stay"
    );
    let body: Vec<u8> = live
        .query_row(
            "SELECT body FROM state_log WHERE seq=?1",
            [taken as i64],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(body, TOMBSTONE);
    let expected = rows(&live, "leases").unwrap();
    assert_eq!(expected.len(), 1);
    let (rebuilt, last) = rebuild(&live, worktrees, LEASES, leases, None).unwrap();
    assert_eq!(last, 3);
    assert_eq!(rows(&rebuilt, "leases").unwrap(), expected);
    let tx = live.transaction().unwrap();
    redact(&tx, kept, &fence::engine()).unwrap();
    let (rebuilt, _) = rebuild(&tx, worktrees, LEASES, leases, None).unwrap();
    assert_ne!(
        rows(&rebuilt, "leases").unwrap(),
        expected,
        "redacting the event of a row still live loses it: redact only after its delete"
    );
}

#[test]
fn redact_refuses_a_missing_event_or_a_snapshot_and_append_refuses_the_tombstone() {
    let mut empty = store();
    let tx = empty.transaction().unwrap();
    assert_eq!(
        code(redact(&tx, 1, &fence::engine())),
        Some("state_log_redact_missing".into()),
        "a store with no log"
    );
    drop(tx);
    let mut db = store();
    let tx = db.transaction().unwrap();
    let mut log = Appender::own(&tx, fence::engine()).unwrap();
    let run = vec![Chunk {
        table: "ide".into(),
        rows: Vec::new(),
    }];
    let done = log.snapshot(Aggregate::Ide, 1, run).unwrap();
    assert_eq!(
        code(log.append(&Draft {
            aggregate: Aggregate::Ide,
            kind: "Tick",
            schema: 1,
            at_ms: 1,
            body: TOMBSTONE,
        })),
        Some("state_log_body_reserved".into())
    );
    for seq in [99, u64::MAX] {
        assert_eq!(
            code(redact(&tx, seq, &fence::engine())),
            Some("state_log_redact_missing".into()),
            "{seq}"
        );
    }
    for seq in [done - 1, done] {
        assert_eq!(
            code(redact(&tx, seq, &fence::engine())),
            Some("state_log_redact_snapshot".into()),
            "{seq}"
        );
    }
}

#[test]
fn a_redacted_body_is_gone_from_the_database_file_and_its_wal() {
    let folder = tempfile::tempdir().unwrap();
    let path = folder.path().join("engine.db");
    let mut db = Connection::open(&path).unwrap();
    let mode: String = db
        .query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))
        .unwrap();
    assert_eq!(mode, "wal");
    db.execute_batch(
        "CREATE TABLE state (id INTEGER PRIMARY KEY CHECK(id=1), revision INTEGER NOT NULL);
        INSERT INTO state VALUES (1,0);",
    )
    .unwrap();
    fence::seed(&db);
    db.execute_batch("PRAGMA secure_delete=OFF;").unwrap();
    let pragma = |name: &str| -> i64 {
        db.query_row(&format!("PRAGMA {name}"), [], |row| row.get(0))
            .unwrap()
    };
    assert_eq!(pragma("secure_delete"), 0, "only redact turns it on");
    assert_eq!(pragma("auto_vacuum"), 0, "freed pages stay in the file");
    let marker = b"redact-marker-31d7";
    let body = marker.repeat(1_200);
    let tx = db.transaction().unwrap();
    let mut log = Appender::own(&tx, fence::engine()).unwrap();
    let seq = log
        .append(&Draft {
            aggregate: Aggregate::Worktrees,
            kind: "Secret",
            schema: 1,
            at_ms: 1,
            body: &body,
        })
        .unwrap();
    tx.commit().unwrap();
    checkpoint(&db);
    assert!(holds(&path, marker), "the body spills into the file first");
    let tx = db.transaction().unwrap();
    redact(&tx, seq, &fence::engine()).unwrap();
    tx.commit().unwrap();
    let secure: i64 = db
        .query_row("PRAGMA secure_delete", [], |row| row.get(0))
        .unwrap();
    assert_eq!(secure, 0, "redact puts secure_delete back");
    checkpoint(&db);
    assert!(!holds(&path, marker), "the redacted body is zeroed");
    let wal = path.with_extension("db-wal");
    assert!(
        !wal.exists() || !holds(&wal, marker),
        "the WAL keeps no copy"
    );
}
