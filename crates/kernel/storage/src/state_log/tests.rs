use super::*;
use rusqlite::{Connection, params};
use serde_json::json;
use sha2::{Digest, Sha256};

const WORKTREES: AggregateName = AggregateName::new("worktrees");

/// An in-memory store holding an empty log. Input: none. Output: the connection.
fn db() -> Connection {
    let db = Connection::open_in_memory().unwrap();
    ensure(&db).unwrap();
    db
}

/// A refusal's code. Input: a result. Output: the error text, or None for Ok.
fn code<T>(result: Result<T, StorageError>) -> Option<String> {
    result.err().map(|cause| cause.0)
}

/// Writes one row into the log as a hand-made log would, without the appender. Inputs: the
/// connection, the row's seq, its aggregate column, its kind and its body. Output: none.
fn plant(db: &Connection, seq: i64, aggregate: &str, kind: &str, body: &[u8]) {
    db.execute(
        "INSERT INTO state_log VALUES (?1,1,?2,?3,1,0,?4)",
        params![seq, aggregate, kind, body],
    )
    .unwrap();
}

/// The body of a closing event naming `aggregate`, counting `chunks` and hashing the given
/// chunk bodies in order. Inputs: the name, the count and the bodies. Output: the body.
fn closing(aggregate: &str, chunks: u32, bodies: &[&[u8]]) -> Vec<u8> {
    let mut digest = Sha256::new();
    for body in bodies {
        digest.update(body);
    }
    let sha256 = format!("{:x}", digest.finalize());
    serde_json::to_vec(&json!({ "aggregate": aggregate, "chunks": chunks, "sha256": sha256 }))
        .unwrap()
}

/// The body of one chunk of `aggregate` at `seq`, with no rows, as the engine writes it.
/// Inputs: the name and the seq. Output: the body.
fn chunk(aggregate: &str, seq: u32) -> Vec<u8> {
    serde_json::to_vec(
        &json!({ "aggregate": aggregate, "table": "worktrees", "seq": seq, "rows": [] }),
    )
    .unwrap()
}

/// Plants a run of one chunk and its closing event, each as a (column, body name) pair,
/// then verifies it. Inputs: the chunk's and the closing event's column and body name.
/// Output: the verdict.
fn one_chunk_run(chunk_row: (&str, &str), closing_row: (&str, &str)) -> Option<String> {
    let db = db();
    let body = chunk(chunk_row.1, 0);
    plant(&db, 1, chunk_row.0, SNAPSHOT_CHUNK, &body);
    plant(
        &db,
        2,
        closing_row.0,
        SNAPSHOT_DONE,
        &closing(closing_row.1, 1, &[&body]),
    );
    code(verify_snapshot(&db, 2))
}

#[test]
fn the_snapshot_bodies_are_the_bytes_the_engine_wrote_and_verify() {
    let chunks = vec![
        Chunk {
            table: "worktrees".into(),
            rows: vec![json!({"id": 1})],
        },
        Chunk {
            table: "worktree_leases".into(),
            rows: Vec::new(),
        },
    ];
    let (bodies, done) = snapshot_bodies(WORKTREES, chunks).unwrap();
    assert_eq!(
        bodies,
        [
            br#"{"aggregate":"worktrees","table":"worktrees","seq":0,"rows":[{"id":1}]}"#.to_vec(),
            br#"{"aggregate":"worktrees","table":"worktree_leases","seq":1,"rows":[]}"#.to_vec(),
        ]
    );
    assert_eq!(done, closing("worktrees", 2, &[&bodies[0], &bodies[1]]));
    let db = db();
    plant(&db, 1, "worktrees", SNAPSHOT_CHUNK, &bodies[0]);
    plant(&db, 2, "worktrees", SNAPSHOT_CHUNK, &bodies[1]);
    plant(&db, 3, "worktrees", SNAPSHOT_DONE, &done);
    assert_eq!(code(verify_snapshot(&db, 3)), None);
}

#[test]
fn a_well_formed_aggregate_no_engine_knows_verifies_on_its_hash() {
    let notes = ("plugin_notes", "plugin_notes");
    assert_eq!(one_chunk_run(notes, notes), None);
}

#[test]
fn a_malformed_aggregate_name_in_a_snapshot_body_is_invalid() {
    let long = "a".repeat(AGGREGATE_BYTES_MAX + 1);
    let worktrees = ("worktrees", "worktrees");
    for name in ["Bad-Name", long.as_str(), ""] {
        assert_eq!(
            one_chunk_run(worktrees, (name, name)),
            Some("state_log_snapshot_invalid".into()),
            "closing {name:?}"
        );
        assert_eq!(
            one_chunk_run((name, name), worktrees),
            Some("state_log_snapshot_invalid".into()),
            "chunk {name:?}"
        );
    }
}

#[test]
fn a_closing_body_naming_another_aggregate_than_its_row_is_invalid() {
    assert_eq!(
        one_chunk_run(("ide", "ide"), ("worktrees", "ide")),
        Some("state_log_snapshot_invalid".into())
    );
}

#[test]
fn a_chunk_body_naming_another_aggregate_than_its_row_is_invalid() {
    assert_eq!(
        one_chunk_run(("worktrees", "ide"), ("ide", "ide")),
        Some("state_log_snapshot_invalid".into())
    );
}

#[test]
fn a_closing_event_with_a_bad_body_or_a_count_no_run_can_hold_is_invalid() {
    let db = db();
    let impossible = u32::try_from(TRANSACTION_EVENTS_MAX).unwrap();
    plant(&db, 1, "worktrees", SNAPSHOT_DONE, b"not json");
    plant(
        &db,
        2,
        "worktrees",
        SNAPSHOT_DONE,
        &closing("worktrees", impossible, &[]),
    );
    plant(
        &db,
        3,
        "worktrees",
        SNAPSHOT_DONE,
        &closing("worktrees", impossible - 1, &[]),
    );
    for (done, expected) in [
        (1, "state_log_snapshot_invalid"),
        (2, "state_log_snapshot_invalid"),
        (3, "state_log_snapshot_hash"),
    ] {
        assert_eq!(
            code(verify_snapshot(&db, done)),
            Some(expected.into()),
            "closing event {done}"
        );
    }
    assert_eq!(
        code(verify_snapshot(&db, 9)),
        Some("state_log_snapshot_missing".into())
    );
}

#[test]
fn a_run_short_of_its_count_or_with_chunks_out_of_order_or_of_another_aggregate_fails() {
    let db = db();
    plant(
        &db,
        1,
        "worktrees",
        SNAPSHOT_DONE,
        &closing("worktrees", 1, &[]),
    );
    assert_eq!(
        code(verify_snapshot(&db, 1)),
        Some("state_log_snapshot_chunks".into()),
        "a run counting one chunk with none before it"
    );
    let swapped = [
        ("worktrees", chunk("worktrees", 1)),
        ("worktrees", chunk("worktrees", 0)),
    ];
    let foreign = [
        ("worktrees", chunk("worktrees", 0)),
        ("ide", chunk("ide", 1)),
    ];
    let mut seq: i64 = 1;
    for run in [&swapped, &foreign] {
        for (column, body) in run {
            seq += 1;
            plant(&db, seq, column, SNAPSHOT_CHUNK, body);
        }
        seq += 1;
        plant(
            &db,
            seq,
            "worktrees",
            SNAPSHOT_DONE,
            &closing("worktrees", 2, &[run[0].1.as_slice(), run[1].1.as_slice()]),
        );
        assert_eq!(
            code(verify_snapshot(&db, seq as u64)),
            Some("state_log_snapshot_chunks".into()),
            "run closed at {seq}"
        );
    }
}

#[test]
fn a_body_over_the_cap_in_a_log_made_without_its_checks_is_invalid() {
    let unchecked = || {
        let db = Connection::open_in_memory().unwrap();
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
    plant(
        &db,
        1,
        "worktrees",
        SNAPSHOT_DONE,
        &padded(closing("worktrees", 0, &[])),
    );
    assert_eq!(
        code(verify_snapshot(&db, 1)),
        Some("state_log_snapshot_invalid".into()),
        "a closing event over the cap"
    );
    let db = unchecked();
    let body = padded(chunk("worktrees", 0));
    plant(&db, 1, "worktrees", SNAPSHOT_CHUNK, &body);
    plant(
        &db,
        2,
        "worktrees",
        SNAPSHOT_DONE,
        &closing("worktrees", 1, &[&body]),
    );
    assert_eq!(
        code(verify_snapshot(&db, 2)),
        Some("state_log_snapshot_invalid".into()),
        "a chunk over the cap"
    );
}

#[test]
fn a_snapshot_chunk_over_the_cap_is_refused_before_any_body_is_made() {
    let big = Chunk {
        table: "worktrees".into(),
        rows: vec![json!("x".repeat(BODY_BYTES_MAX))],
    };
    assert_eq!(
        code(snapshot_bodies(WORKTREES, vec![big])),
        Some("state_log_chunk_bounds".into())
    );
}

#[test]
fn a_draft_out_of_the_log_bounds_is_refused() {
    let at = |body: &'static [u8], kind: &'static str, schema: u16, at_ms: u64| Draft {
        aggregate: WORKTREES,
        kind,
        schema,
        at_ms,
        body,
    };
    let over: &'static [u8] = Box::leak(vec![b'x'; BODY_BYTES_MAX + 1].into_boxed_slice());
    let cap: &'static [u8] = Box::leak(vec![b'x'; BODY_BYTES_MAX].into_boxed_slice());
    let long: &'static str = Box::leak("k".repeat(KIND_BYTES_MAX + 1).into_boxed_str());
    assert_eq!(code(check(&at(cap, "Kind1", 1, 0))), None);
    for (draft, expected) in [
        (at(over, "Kind1", 1, 0), "state_log_body_bounds"),
        (at(TOMBSTONE, "Kind1", 1, 0), "state_log_body_reserved"),
        (at(b"{}", "", 1, 0), "state_log_kind"),
        (at(b"{}", long, 1, 0), "state_log_kind"),
        (at(b"{}", "Kind_1", 1, 0), "state_log_kind"),
        (at(b"{}", "Kind1", 0, 0), "state_log_schema"),
        (
            at(b"{}", "Kind1", 1, i64::MAX as u64 + 1),
            "state_log_at_ms",
        ),
    ] {
        assert_eq!(code(check(&draft)), Some(expected.into()), "{draft:?}");
    }
}

#[test]
fn a_manual_clock_reads_what_it_was_set_to_and_the_system_clock_cannot_be_set() {
    let system_ms = || {
        u64::try_from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis(),
        )
        .unwrap()
    };
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
fn a_chunk_body_reads_back_only_for_the_aggregate_it_names() {
    let body = chunk("worktrees", 0);
    assert_eq!(
        read_chunk("worktrees", &body),
        Some(Chunk {
            table: "worktrees".into(),
            rows: Vec::new()
        })
    );
    assert_eq!(read_chunk("ide", &body), None);
    assert_eq!(read_chunk("worktrees", b"not json"), None);
    let closing = closing("worktrees", 0, &[]);
    assert_eq!(read_chunk("worktrees", &closing), None);
}
