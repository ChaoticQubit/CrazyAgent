use super::*;
use rusqlite::{Connection, params};
use serde_json::json;
use sha2::{Digest, Sha256};

/// An in-memory engine store holding only the `state` row, as storage.rs creates it.
/// Output: the connection, at revision 0.
fn db() -> Connection {
    let db = Connection::open_in_memory().unwrap();
    db.execute_batch(
        "CREATE TABLE state (id INTEGER PRIMARY KEY CHECK(id=1), revision INTEGER NOT NULL);
        INSERT INTO state VALUES (1,0);",
    )
    .unwrap();
    db
}

/// A worktrees event with the given body. Input: the body. Output: the draft.
fn draft(body: &[u8]) -> Draft<'_> {
    Draft {
        aggregate: Aggregate::Worktrees,
        kind: "WorktreeAllocated",
        schema: 1,
        at_ms: 7,
        body,
    }
}

/// The refusal code of a result, none when it succeeded. Input: the result. Output: the code.
fn code<T>(result: Result<T, EngineError>) -> Option<String> {
    result.err().map(|error| error.0)
}

/// Every logged event's seq, revision, aggregate and kind, in seq order. Input: the
/// connection. Output: the rows.
fn logged(db: &Connection) -> Vec<(i64, i64, String, String)> {
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

/// The store's `state` revision. Input: the connection. Output: the revision.
fn revision(db: &Connection) -> i64 {
    db.query_row("SELECT revision FROM state WHERE id=1", [], |row| {
        row.get(0)
    })
    .unwrap()
}

/// Three one-row chunks of the worktrees table. Output: the chunks.
fn chunks() -> Vec<Chunk> {
    (0..3)
        .map(|index| Chunk {
            table: "worktrees".into(),
            rows: vec![json!({ "id": index })],
        })
        .collect()
}

#[test]
fn each_own_transaction_moves_the_revision_once_and_numbers_its_events_after_the_last() {
    let mut db = db();
    let tx = db.transaction().unwrap();
    let mut log = Appender::own(&tx).unwrap();
    assert_eq!(log.append(&draft(b"{}")).unwrap(), 1);
    assert_eq!(log.append(&draft(b"{}")).unwrap(), 2);
    tx.commit().unwrap();
    let tx = db.transaction().unwrap();
    let mut log = Appender::own(&tx).unwrap();
    assert_eq!(log.revision(), 2);
    assert_eq!(log.append(&draft(b"{}")).unwrap(), 3);
    tx.commit().unwrap();
    assert_eq!(revision(&db), 2);
    let kind = || "WorktreeAllocated".to_string();
    let worktrees = || "worktrees".to_string();
    assert_eq!(
        logged(&db),
        vec![
            (1, 1, worktrees(), kind()),
            (2, 1, worktrees(), kind()),
            (3, 2, worktrees(), kind()),
        ]
    );
}

#[test]
fn an_owner_append_takes_the_revision_its_request_moved_and_refuses_a_stale_or_lower_one() {
    let mut db = db();
    let tx = db.transaction().unwrap();
    assert_eq!(
        code(Appender::owner(&tx, 0)),
        Some("state_log_revision_not_newer".into())
    );
    drop(tx);
    db.execute("UPDATE state SET revision=5 WHERE id=1", [])
        .unwrap();
    let tx = db.transaction().unwrap();
    assert_eq!(
        code(Appender::owner(&tx, 4)),
        Some("state_log_revision_stale".into())
    );
    let mut log = Appender::owner(&tx, 5).unwrap();
    assert_eq!(log.append(&draft(b"{}")).unwrap(), 1);
    tx.commit().unwrap();
    let tx = db.transaction().unwrap();
    assert_eq!(
        code(Appender::owner(&tx, 5)),
        Some("state_log_revision_not_newer".into()),
        "a second appending transaction must move the revision again"
    );
    tx.execute("UPDATE state SET revision=3 WHERE id=1", [])
        .unwrap();
    assert_eq!(
        code(Appender::owner(&tx, 3)),
        Some("state_log_revision_not_newer".into()),
        "a revision lower than the last logged one is refused"
    );
    drop(tx);
    assert_eq!(revision(&db), 5);
    assert_eq!(logged(&db).len(), 1);
}

#[test]
fn an_own_writer_is_refused_once_the_revision_is_exhausted() {
    let mut db = db();
    db.execute("UPDATE state SET revision=?1 WHERE id=1", [i64::MAX])
        .unwrap();
    let tx = db.transaction().unwrap();
    assert_eq!(
        code(Appender::own(&tx)),
        Some("state_log_revision_exhausted".into())
    );
}

#[test]
fn a_body_at_the_cap_is_kept_and_one_byte_over_is_refused() {
    let mut db = db();
    let tx = db.transaction().unwrap();
    let mut log = Appender::own(&tx).unwrap();
    let full = vec![b'x'; BODY_BYTES_MAX];
    assert_eq!(log.append(&draft(&full)).unwrap(), 1);
    let over = vec![b'x'; BODY_BYTES_MAX + 1];
    assert_eq!(
        code(log.append(&draft(&over))),
        Some("state_log_body_bounds".into())
    );
    assert_eq!(logged(&tx).len(), 1);
}

#[test]
fn a_kind_schema_or_time_out_of_bounds_is_refused_and_snapshot_kinds_are_reserved() {
    let mut db = db();
    let tx = db.transaction().unwrap();
    let mut log = Appender::own(&tx).unwrap();
    let longest = "k".repeat(KIND_BYTES_MAX);
    let too_long = "k".repeat(KIND_BYTES_MAX + 1);
    let base = draft(b"{}");
    assert_eq!(
        log.append(&Draft {
            kind: &longest,
            ..base
        })
        .unwrap(),
        1
    );
    for kind in ["", too_long.as_str(), "Two words", "Kind\n"] {
        assert_eq!(
            code(log.append(&Draft { kind, ..base })),
            Some("state_log_kind".into()),
            "{kind:?}"
        );
    }
    for kind in [SNAPSHOT_CHUNK, SNAPSHOT_DONE] {
        assert_eq!(
            code(log.append(&Draft { kind, ..base })),
            Some("state_log_kind_reserved".into())
        );
    }
    assert_eq!(
        code(log.append(&Draft { schema: 0, ..base })),
        Some("state_log_schema".into())
    );
    assert_eq!(
        code(log.append(&Draft {
            at_ms: i64::MAX as u64 + 1,
            ..base
        })),
        Some("state_log_at_ms".into())
    );
    assert_eq!(logged(&tx).len(), 1);
}

#[test]
fn an_append_past_the_last_possible_seq_is_refused() {
    let mut db = db();
    let tx = db.transaction().unwrap();
    let mut log = Appender::own(&tx).unwrap();
    tx.execute(
        "INSERT INTO state_log VALUES (?1,1,'worktrees','Planted',1,0,x'')",
        [i64::MAX],
    )
    .unwrap();
    assert_eq!(
        code(log.append(&draft(b"{}"))),
        Some("state_log_seq_exhausted".into())
    );
}

#[test]
fn one_transaction_holds_at_most_its_bound_of_events() {
    let mut db = db();
    let tx = db.transaction().unwrap();
    let mut log = Appender::own(&tx).unwrap();
    let last = TRANSACTION_EVENTS_MAX as u64;
    for seq in 1..last {
        assert_eq!(log.append(&draft(b"")).unwrap(), seq);
    }
    let one: Vec<Chunk> = chunks().into_iter().take(1).collect();
    assert_eq!(
        code(log.snapshot(Aggregate::Worktrees, 9, one)),
        Some("state_log_transaction_full".into()),
        "a one-chunk run needs two places and one is left"
    );
    assert_eq!(logged(&tx).len(), TRANSACTION_EVENTS_MAX - 1);
    assert_eq!(log.snapshot(Aggregate::Ide, 9, Vec::new()).unwrap(), last);
    assert_eq!(
        code(log.append(&draft(b""))),
        Some("state_log_transaction_full".into())
    );
    assert_eq!(logged(&tx).len(), TRANSACTION_EVENTS_MAX);
}

#[test]
fn the_log_and_the_revision_roll_back_with_their_transaction() {
    let mut db = db();
    let tx = db.transaction().unwrap();
    let mut log = Appender::own(&tx).unwrap();
    assert_eq!(log.append(&draft(b"{}")).unwrap(), 1);
    tx.commit().unwrap();
    let tx = db.transaction().unwrap();
    let mut log = Appender::own(&tx).unwrap();
    assert_eq!(log.append(&draft(b"{}")).unwrap(), 2);
    drop(tx);
    assert_eq!(revision(&db), 1);
    assert_eq!(logged(&db).len(), 1);
}

#[test]
fn a_snapshot_run_verifies_and_a_missing_or_changed_chunk_fails_its_hash() {
    let mut db = db();
    let tx = db.transaction().unwrap();
    let mut log = Appender::own(&tx).unwrap();
    let done = log.snapshot(Aggregate::Worktrees, 9, chunks()).unwrap();
    let empty = log.snapshot(Aggregate::Ide, 9, Vec::new()).unwrap();
    tx.commit().unwrap();
    assert_eq!((done, empty), (4, 5));
    let kinds: Vec<String> = logged(&db).into_iter().map(|row| row.3).collect();
    assert_eq!(
        kinds,
        [
            SNAPSHOT_CHUNK,
            SNAPSHOT_CHUNK,
            SNAPSHOT_CHUNK,
            SNAPSHOT_DONE,
            SNAPSHOT_DONE
        ]
    );
    assert_eq!(verify_snapshot(&db, done), Ok(()));
    assert_eq!(verify_snapshot(&db, empty), Ok(()));
    assert_eq!(
        code(verify_snapshot(&db, 2)),
        Some("state_log_snapshot_missing".into())
    );
    assert_eq!(
        code(verify_snapshot(&db, 99)),
        Some("state_log_snapshot_missing".into())
    );
    let tx = db.transaction().unwrap();
    tx.execute("DELETE FROM state_log WHERE seq=2", []).unwrap();
    assert_eq!(
        code(verify_snapshot(&tx, done)),
        Some("state_log_snapshot_hash".into())
    );
    drop(tx);
    let tx = db.transaction().unwrap();
    tx.execute(
        "UPDATE state_log SET body=CAST(replace(CAST(body AS TEXT), '\"id\":1', '\"id\":7') AS BLOB) WHERE seq=2",
        [],
    )
    .unwrap();
    assert_eq!(
        code(verify_snapshot(&tx, done)),
        Some("state_log_snapshot_hash".into())
    );
    drop(tx);
    assert_eq!(verify_snapshot(&db, done), Ok(()));
}

#[test]
fn a_closing_event_whose_hash_covers_rows_that_are_not_its_chunks_is_refused() {
    let mut db = db();
    let tx = db.transaction().unwrap();
    let mut log = Appender::own(&tx).unwrap();
    log.append(&draft(b"a")).unwrap();
    log.append(&draft(b"b")).unwrap();
    let sha256 = format!("{:x}", Sha256::digest(b"ab"));
    let body =
        serde_json::to_vec(&json!({ "aggregate": "worktrees", "chunks": 2, "sha256": sha256 }))
            .unwrap();
    tx.execute(
        "INSERT INTO state_log VALUES (3,1,'worktrees',?1,1,0,?2)",
        params![SNAPSHOT_DONE, body],
    )
    .unwrap();
    assert_eq!(
        code(verify_snapshot(&tx, 3)),
        Some("state_log_snapshot_chunks".into())
    );
}

#[test]
fn a_snapshot_chunk_over_the_cap_is_refused_before_any_chunk_is_appended() {
    let mut db = db();
    let tx = db.transaction().unwrap();
    let mut log = Appender::own(&tx).unwrap();
    let mut run = chunks();
    run.push(Chunk {
        table: "worktrees".into(),
        rows: vec![json!("x".repeat(BODY_BYTES_MAX))],
    });
    assert_eq!(
        code(log.snapshot(Aggregate::Worktrees, 9, run)),
        Some("state_log_chunk_bounds".into())
    );
    assert_eq!(logged(&tx).len(), 0);
}

/// Plants one row in the log outside the appender, as a damaged or forged store holds it.
/// Inputs: the connection, the seq, the kind and the body. Output: none.
fn plant(db: &Connection, seq: i64, kind: &str, body: &[u8]) {
    db.execute(
        "INSERT INTO state_log VALUES (?1,1,'worktrees',?2,1,0,?3)",
        params![seq, kind, body],
    )
    .unwrap();
}

/// The body of a worktrees closing event counting `chunks` and hashing the given chunk
/// bodies in order. Inputs: the count and the bodies. Output: the body.
fn closing(chunks: u32, bodies: &[&[u8]]) -> Vec<u8> {
    let mut digest = Sha256::new();
    for body in bodies {
        digest.update(body);
    }
    let sha256 = format!("{:x}", digest.finalize());
    serde_json::to_vec(&json!({ "aggregate": "worktrees", "chunks": chunks, "sha256": sha256 }))
        .unwrap()
}

#[test]
fn a_closing_event_with_a_bad_body_or_a_count_no_run_can_hold_is_invalid() {
    let mut db = db();
    let tx = db.transaction().unwrap();
    Appender::own(&tx).unwrap();
    let impossible = u32::try_from(TRANSACTION_EVENTS_MAX).unwrap();
    plant(&tx, 1, SNAPSHOT_DONE, b"not json");
    plant(&tx, 2, SNAPSHOT_DONE, &closing(impossible, &[]));
    plant(&tx, 3, SNAPSHOT_DONE, &closing(impossible - 1, &[]));
    for (done, expected) in [
        (1, "state_log_snapshot_invalid"),
        (2, "state_log_snapshot_invalid"),
        (3, "state_log_snapshot_hash"),
    ] {
        assert_eq!(
            code(verify_snapshot(&tx, done)),
            Some(expected.into()),
            "closing event {done}"
        );
    }
}

#[test]
fn a_run_short_of_its_count_or_with_chunks_out_of_order_or_of_another_aggregate_fails() {
    let mut db = db();
    let tx = db.transaction().unwrap();
    Appender::own(&tx).unwrap();
    plant(&tx, 1, SNAPSHOT_DONE, &closing(1, &[]));
    assert_eq!(
        code(verify_snapshot(&tx, 1)),
        Some("state_log_snapshot_chunks".into()),
        "a run counting one chunk with none before it"
    );
    let chunk = |aggregate: Aggregate, seq: u32| {
        serde_json::to_vec(&SnapshotChunk {
            aggregate,
            table: "worktrees".into(),
            seq,
            rows: Vec::new(),
        })
        .unwrap()
    };
    let swapped = [
        chunk(Aggregate::Worktrees, 1),
        chunk(Aggregate::Worktrees, 0),
    ];
    let foreign = [chunk(Aggregate::Worktrees, 0), chunk(Aggregate::Ide, 1)];
    let mut seq: i64 = 1;
    for run in [&swapped, &foreign] {
        for body in run {
            seq += 1;
            plant(&tx, seq, SNAPSHOT_CHUNK, body);
        }
        seq += 1;
        plant(
            &tx,
            seq,
            SNAPSHOT_DONE,
            &closing(2, &[run[0].as_slice(), run[1].as_slice()]),
        );
        assert_eq!(
            code(verify_snapshot(&tx, seq as u64)),
            Some("state_log_snapshot_chunks".into()),
            "run closed at {seq}"
        );
    }
}

#[test]
fn a_body_over_the_cap_in_a_log_made_without_its_checks_is_invalid() {
    let unchecked = || {
        let db = db();
        db.execute_batch(
            "CREATE TABLE state_log (seq INTEGER PRIMARY KEY, revision INTEGER, aggregate TEXT,
            kind TEXT, schema INTEGER, at_ms INTEGER, body BLOB)",
        )
        .unwrap();
        db
    };
    let padded = |mut body: Vec<u8>| {
        body.resize(BODY_BYTES_MAX + 1, b' ');
        body
    };
    let db = unchecked();
    plant(&db, 1, SNAPSHOT_DONE, &padded(closing(0, &[])));
    assert_eq!(
        code(verify_snapshot(&db, 1)),
        Some("state_log_snapshot_invalid".into()),
        "a closing event over the cap"
    );
    let db = unchecked();
    let chunk = padded(
        serde_json::to_vec(&SnapshotChunk {
            aggregate: Aggregate::Worktrees,
            table: "worktrees".into(),
            seq: 0,
            rows: Vec::new(),
        })
        .unwrap(),
    );
    plant(&db, 1, SNAPSHOT_CHUNK, &chunk);
    plant(&db, 2, SNAPSHOT_DONE, &closing(1, &[chunk.as_slice()]));
    assert_eq!(
        code(verify_snapshot(&db, 2)),
        Some("state_log_snapshot_invalid".into()),
        "a chunk over the cap"
    );
}
