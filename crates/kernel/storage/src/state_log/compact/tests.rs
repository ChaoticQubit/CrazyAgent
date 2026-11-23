use super::*;
use crate::state_log::replay::{Logged, replay};
use crate::state_log::{Appender, Draft, FenceCheck};
use rusqlite::Connection;
use serde_json::json;
use std::cell::Cell;

/// The aggregate most rows compact.
const LEASES: AggregateName = AggregateName::new("worktrees");
/// A second aggregate whose events interleave with the first.
const IDE: AggregateName = AggregateName::new("ide");

/// The names these rows log. Input: the stored name. Output: its name, none for another.
fn known(name: &str) -> Option<AggregateName> {
    [LEASES, IDE].into_iter().find(|known| known.name() == name)
}

/// A fence that passes every check. Output: the hook.
fn passing() -> FenceCheck {
    Box::new(|_| Ok(()))
}

/// An in-memory store with the `state` row at revision 0 and the `events` table. Output:
/// the connection.
fn store() -> Connection {
    let db = Connection::open_in_memory().unwrap();
    db.execute_batch(
        "CREATE TABLE state (id INTEGER PRIMARY KEY CHECK(id=1), revision INTEGER NOT NULL);
        CREATE TABLE events (cursor INTEGER PRIMARY KEY, body TEXT NOT NULL);
        INSERT INTO state VALUES (1,0);",
    )
    .unwrap();
    db
}

/// Starts an own write behind a passing fence. Input: the transaction. Output: the appender.
fn own<'a, 'c>(tx: &'a rusqlite::Transaction<'c>) -> Appender<'a, 'c> {
    Appender::own(tx, passing(), known, |revision| Ok(format!("r{revision}"))).unwrap()
}

/// The live state these rows project: how many `Added` events an aggregate holds and the
/// sum of their values.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Live {
    count: u64,
    sum: u64,
}

/// The snapshot run of a live state: one chunk of one row. Input: the state. Output: the
/// chunks.
fn chunks(live: Live) -> Vec<Chunk> {
    vec![Chunk {
        table: "totals".into(),
        rows: vec![json!({ "count": live.count, "sum": live.sum })],
    }]
}

/// Appends `count` `Added` events to one aggregate, at most 1,000 per transaction, and
/// adds them to its live state. Inputs: the store, the aggregate, the count and the state.
/// Output: none.
fn add(db: &mut Connection, aggregate: AggregateName, count: u64, live: &mut Live) {
    let mut left = count;
    while left > 0 {
        let now = left.min(1_000);
        let tx = db.transaction().unwrap();
        let mut log = own(&tx);
        for _ in 0..now {
            live.count += 1;
            live.sum += live.count;
            let body = serde_json::to_vec(&json!({ "value": live.count })).unwrap();
            log.append(&Draft {
                aggregate,
                kind: "Added",
                schema: 1,
                at_ms: 1,
                body: &body,
            })
            .unwrap();
        }
        drop(log);
        tx.commit().unwrap();
        left -= now;
    }
}

/// Runs compaction for one aggregate in its own committed transaction. Inputs: the store,
/// the switch, the aggregate and its live state. Output: the tombstoned row count.
fn compacted(
    db: &mut Connection,
    compaction: Compaction,
    aggregate: AggregateName,
    live: Live,
) -> u64 {
    let tx = db.transaction().unwrap();
    let mut log = own(&tx);
    let pruned = compact(&mut log, compaction, aggregate, 2, || Ok(chunks(live))).unwrap();
    drop(log);
    tx.commit().unwrap();
    pruned
}

/// The seqs of one aggregate's `SnapshotDone` events, oldest first. Inputs: the store and
/// the aggregate. Output: the seqs.
fn dones(db: &Connection, aggregate: AggregateName) -> Vec<i64> {
    db.prepare("SELECT seq FROM state_log WHERE aggregate=?1 AND kind=?2 ORDER BY seq")
        .unwrap()
        .query_map(params![aggregate.name(), SNAPSHOT_DONE], |row| row.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

/// Every log row as seq, aggregate, kind and body. Input: the store. Output: the rows.
fn every_row(db: &Connection) -> Vec<(i64, String, String, Vec<u8>)> {
    db.prepare("SELECT seq, aggregate, kind, body FROM state_log ORDER BY seq")
        .unwrap()
        .query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

/// Replays one aggregate into a live state: a snapshot chunk sets it, an `Added` event adds
/// to it. Inputs: the store and the aggregate. Output: the state, or replay's refusal.
fn replayed(db: &Connection, aggregate: AggregateName) -> Result<Live, StorageError> {
    let mut live = Live::default();
    replay(db, aggregate, 0, known, |event: &Logged| {
        if event.kind == SNAPSHOT_CHUNK {
            let chunk = crate::state_log::replay::chunk(event)?;
            let row = &chunk.rows[0];
            live = Live {
                count: row["count"].as_u64().unwrap(),
                sum: row["sum"].as_u64().unwrap(),
            };
        } else {
            let body: serde_json::Value = serde_json::from_slice(&event.body).unwrap();
            assert_eq!(body["value"].as_u64(), Some(live.count + 1));
            live.count += 1;
            live.sum += live.count;
        }
        Ok::<(), StorageError>(())
    })?;
    Ok(live)
}

/// Builds three compaction runs of [`COMPACT_EVERY`] events on [`LEASES`], with [`IDE`]
/// events between them, and compacts after each. Input: the store. Output: the live state
/// of each aggregate.
fn three_runs(db: &mut Connection) -> (Live, Live) {
    let (mut leases, mut ide) = (Live::default(), Live::default());
    for _ in 0..3 {
        add(db, LEASES, COMPACT_EVERY, &mut leases);
        add(db, IDE, 3, &mut ide);
        compacted(db, Compaction::On, LEASES, leases);
    }
    (leases, ide)
}

#[test]
fn prune_never_passes_the_second_newest_snapshot_done() {
    let mut db = store();
    three_runs(&mut db);
    let done = dones(&db, LEASES);
    assert_eq!(done.len(), 3);
    let second = done[1];
    let second_first_chunk = second - 1;

    for (seq, aggregate, kind, body) in every_row(&db) {
        let pruned = body.as_slice() == TOMBSTONE;
        let behind = aggregate == LEASES.name() && seq < second_first_chunk;
        assert_eq!(pruned, behind, "{seq} {aggregate} {kind}");
    }
    assert_ne!(
        db.query_row("SELECT body FROM state_log WHERE seq=?1", [second], |row| {
            row.get::<_, Vec<u8>>(0)
        })
        .unwrap(),
        TOMBSTONE
    );
}

#[test]
fn replay_from_the_newest_snapshot_equals_the_live_rows() {
    let mut db = store();
    let (leases, ide) = three_runs(&mut db);

    assert_eq!(replayed(&db, LEASES), Ok(leases));
    assert_eq!(replayed(&db, IDE), Ok(ide));
}

#[test]
fn after_a_prune_replay_of_every_aggregate_passes() {
    let mut db = store();
    let (mut leases, mut ide) = three_runs(&mut db);
    add(&mut db, IDE, 2, &mut ide);
    add(&mut db, LEASES, 2, &mut leases);

    for (aggregate, live) in [(LEASES, leases), (IDE, ide)] {
        assert_eq!(replayed(&db, aggregate), Ok(live), "{aggregate:?}");
    }
}

#[test]
fn a_really_removed_row_after_a_prune_still_gives_the_replay_gap() {
    let mut db = store();
    three_runs(&mut db);
    let last: i64 = db
        .query_row("SELECT MAX(seq) FROM state_log", [], |row| row.get(0))
        .unwrap();
    db.execute("DELETE FROM state_log WHERE seq=?1", [last - 1])
        .unwrap();

    for aggregate in [LEASES, IDE] {
        assert_eq!(
            replayed(&db, aggregate).err(),
            Some(StorageError("state_log_replay_gap".into())),
            "{aggregate:?}"
        );
    }
}

#[test]
fn a_pruned_body_reads_as_a_tombstone_never_as_the_old_bytes() {
    let folder = tempfile::tempdir().unwrap();
    let path = folder.path().join("engine.db");
    let mut db = Connection::open(&path).unwrap();
    db.execute_batch(
        "PRAGMA journal_mode=WAL;
        CREATE TABLE state (id INTEGER PRIMARY KEY CHECK(id=1), revision INTEGER NOT NULL);
        CREATE TABLE events (cursor INTEGER PRIMARY KEY, body TEXT NOT NULL);
        INSERT INTO state VALUES (1,0);",
    )
    .unwrap();
    let mut leases = Live::default();
    add(&mut db, LEASES, 1, &mut leases);
    let marker = b"{\"value\":1}";
    for _ in 0..2 {
        add(&mut db, LEASES, COMPACT_EVERY, &mut leases);
        compacted(&mut db, Compaction::On, LEASES, leases);
    }
    let _: (i64, i64, i64) = db
        .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })
        .unwrap();

    let first: Vec<u8> = db
        .query_row("SELECT body FROM state_log WHERE seq=1", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(first, TOMBSTONE);
    let file = std::fs::read(&path).unwrap();
    assert!(
        !file.windows(marker.len()).any(|window| window == marker),
        "the old body is zeroed in the file"
    );
}

#[test]
fn off_writes_no_snapshot_and_deletes_no_row() {
    let mut db = store();
    let mut leases = Live::default();
    add(&mut db, LEASES, COMPACT_EVERY * 2, &mut leases);
    let before = every_row(&db);
    let asked = Cell::new(false);

    let tx = db.transaction().unwrap();
    let mut log = own(&tx);
    let pruned = compact(&mut log, Compaction::Off, LEASES, 2, || {
        asked.set(true);
        Ok(chunks(leases))
    })
    .unwrap();
    drop(log);
    tx.commit().unwrap();

    assert_eq!(pruned, 0);
    assert!(!asked.get(), "Off never reads the rows");
    assert_eq!(every_row(&db), before);
    assert!(dones(&db, LEASES).is_empty());
}

#[test]
fn a_failed_verify_prunes_nothing() {
    let mut db = store();
    let mut leases = Live::default();
    for _ in 0..2 {
        add(&mut db, LEASES, COMPACT_EVERY, &mut leases);
        compacted(&mut db, Compaction::On, LEASES, leases);
    }
    add(&mut db, LEASES, COMPACT_EVERY, &mut leases);
    let second_chunk = dones(&db, LEASES)[1] - 1;
    let body: Vec<u8> = db
        .query_row(
            "SELECT body FROM state_log WHERE seq=?1",
            [second_chunk],
            |row| row.get(0),
        )
        .unwrap();
    let mut changed: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let count = changed["rows"][0]["count"].as_u64().unwrap();
    changed["rows"][0]["count"] = json!(count + 1);
    db.execute(
        "UPDATE state_log SET body=?1 WHERE seq=?2",
        params![serde_json::to_vec(&changed).unwrap(), second_chunk],
    )
    .unwrap();
    let before = every_row(&db);

    let tx = db.transaction().unwrap();
    let mut log = own(&tx);
    let refused = compact(&mut log, Compaction::On, LEASES, 2, || Ok(chunks(leases)));
    drop(log);
    drop(tx);

    assert_eq!(
        refused.err(),
        Some(StorageError("state_log_snapshot_hash".into()))
    );
    assert_eq!(every_row(&db), before);
}

#[test]
fn a_rollback_mid_prune_keeps_every_row() {
    let mut db = store();
    let mut leases = Live::default();
    add(&mut db, LEASES, COMPACT_EVERY, &mut leases);
    compacted(&mut db, Compaction::On, LEASES, leases);
    add(&mut db, LEASES, COMPACT_EVERY, &mut leases);
    let before = every_row(&db);

    let tx = db.transaction().unwrap();
    let mut log = own(&tx);
    let pruned = compact(&mut log, Compaction::On, LEASES, 2, || Ok(chunks(leases))).unwrap();
    assert!(pruned > 0, "the prune ran inside the transaction");
    drop(log);
    drop(tx);

    assert_eq!(every_row(&db), before);
}
