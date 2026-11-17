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
        CREATE TABLE events (cursor INTEGER PRIMARY KEY, body TEXT NOT NULL);
        INSERT INTO state VALUES (1,0);",
    )
    .unwrap();
    fence::seed(&db);
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
    let mut log = Appender::own(&tx, fence::engine()).unwrap();
    assert_eq!(log.append(&draft(b"{}")).unwrap(), 1);
    assert_eq!(log.append(&draft(b"{}")).unwrap(), 2);
    tx.commit().unwrap();
    let tx = db.transaction().unwrap();
    let mut log = Appender::own(&tx, fence::engine()).unwrap();
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
        code(Appender::owner(&tx, 0, fence::engine())),
        Some("state_log_revision_not_newer".into())
    );
    drop(tx);
    db.execute("UPDATE state SET revision=5 WHERE id=1", [])
        .unwrap();
    let tx = db.transaction().unwrap();
    assert_eq!(
        code(Appender::owner(&tx, 4, fence::engine())),
        Some("state_log_revision_stale".into())
    );
    let mut log = Appender::owner(&tx, 5, fence::engine()).unwrap();
    assert_eq!(log.append(&draft(b"{}")).unwrap(), 1);
    tx.commit().unwrap();
    let tx = db.transaction().unwrap();
    assert_eq!(
        code(Appender::owner(&tx, 5, fence::engine())),
        Some("state_log_revision_not_newer".into()),
        "a second appending transaction must move the revision again"
    );
    tx.execute("UPDATE state SET revision=3 WHERE id=1", [])
        .unwrap();
    assert_eq!(
        code(Appender::owner(&tx, 3, fence::engine())),
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
        code(Appender::own(&tx, fence::engine())),
        Some("state_log_revision_exhausted".into())
    );
}

#[test]
fn a_body_at_the_cap_is_kept_and_one_byte_over_is_refused() {
    let mut db = db();
    let tx = db.transaction().unwrap();
    let mut log = Appender::own(&tx, fence::engine()).unwrap();
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
    let mut log = Appender::own(&tx, fence::engine()).unwrap();
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
    let mut log = Appender::own(&tx, fence::engine()).unwrap();
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
    let mut log = Appender::own(&tx, fence::engine()).unwrap();
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
    let mut log = Appender::own(&tx, fence::engine()).unwrap();
    assert_eq!(log.append(&draft(b"{}")).unwrap(), 1);
    tx.commit().unwrap();
    let tx = db.transaction().unwrap();
    let mut log = Appender::own(&tx, fence::engine()).unwrap();
    assert_eq!(log.append(&draft(b"{}")).unwrap(), 2);
    drop(tx);
    assert_eq!(revision(&db), 1);
    assert_eq!(logged(&db).len(), 1);
}

#[test]
fn a_snapshot_run_verifies_and_a_missing_or_changed_chunk_fails_its_hash() {
    let mut db = db();
    let tx = db.transaction().unwrap();
    let mut log = Appender::own(&tx, fence::engine()).unwrap();
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
    let mut log = Appender::own(&tx, fence::engine()).unwrap();
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
    let mut log = Appender::own(&tx, fence::engine()).unwrap();
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
    Appender::own(&tx, fence::engine()).unwrap();
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
    Appender::own(&tx, fence::engine()).unwrap();
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

/// Every kind a writer appends to the log. [`no_event_body_can_carry_a_secret`] holds a body
/// rule for each and fails for a kind listed here without one.
const KINDS: [&str; 25] = [
    SNAPSHOT_CHUNK,
    SNAPSHOT_DONE,
    replay::PROCESS_EXITED,
    crate::project_setup::removed::event::KIND,
    crate::master_session::SESSION_DELETED,
    crate::worktrees::log::KIND_ADMITTED,
    crate::worktrees::log::KIND_RESERVED,
    crate::worktrees::log::KIND_SAVED,
    crate::ide::log::IDE_ASKED,
    crate::ide::log::RUN_MODE_SET,
    crate::ide::log::FILE_UNLOCKED,
    crate::ide::log::WORKSPACE_WRITTEN,
    crate::ide::log::WORKSPACE_FORGOTTEN,
    crate::settings_log::KIND_SANDBOX,
    crate::settings_log::KIND_CLEANUP,
    crate::settings_log::KIND_QUIT,
    crate::settings_log::KIND_RETRY,
    crate::settings_log::KIND_HARNESS_UPDATE,
    crate::settings_log::KIND_INSTRUCTION_APP,
    crate::settings_log::KIND_INSTRUCTION,
    crate::settings_log::KIND_ROADMAP,
    crate::settings_log::KIND_STEPS,
    crate::settings_log::KIND_TIME,
    crate::settings_log::KIND_FRAMES,
    crate::settings_log::KIND_PROFILE,
];

/// The one body key per kind that may hold free text, each by name (leader 07:16, A): a file
/// path inside a worktree, which decodes only through `landing::owner_edit::check_path`, so
/// it is never empty, absolute, NUL-bearing, `.` or `..` in a part, or over its byte bound.
const FREE_FORM: [(&str, &str); 1] = [(crate::ide::log::FILE_UNLOCKED, "path")];

/// The source files outside tests that name the log's appender, as paths under `src` in
/// sorted order, each with what it may put after `kind: `: the constants of its own kinds.
/// ide/log.rs writes `change.kind()`, which maps each `Change` variant to one of its kinds.
const WRITERS: [(&str, &[&str]); 6] = [
    ("ide/log.rs", &["change.kind(),"]),
    ("project_setup/removed/event.rs", &["KIND,"]),
    (
        "settings_log.rs",
        &[
            "KIND_SANDBOX,",
            "KIND_CLEANUP,",
            "KIND_QUIT,",
            "KIND_RETRY,",
            "KIND_HARNESS_UPDATE,",
            "KIND_INSTRUCTION_APP,",
            "KIND_INSTRUCTION,",
            "KIND_ROADMAP,",
            "KIND_STEPS,",
            "KIND_TIME,",
            "KIND_FRAMES,",
            "KIND_PROFILE,",
        ],
    ),
    ("state_log.rs", &["SNAPSHOT_CHUNK,", "SNAPSHOT_DONE,"]),
    (
        "storage/master_session/delete.rs",
        &["SESSION_DELETED,", "SESSION_DELETED.into(),"],
    ),
    (
        "worktrees/log.rs",
        &["KIND_ADMITTED,", "KIND_RESERVED,", "KIND_SAVED,"],
    ),
];

/// The writers outside this module that take a snapshot, each with the only tables its runs
/// may carry. A run copies those tables' stored columns as they stand, rows the typed events
/// wrote; the run does not check them again.
const SNAPSHOTS: [(&str, &[&str]); 2] = [
    (
        "worktrees/log.rs",
        &["repository_admissions", "worktree_leases"],
    ),
    (
        "settings_log.rs",
        &[
            "sandbox_settings",
            "agent_cleanup_settings",
            "agent_quit_settings",
            "provider_retry_settings",
            "harness_update_settings",
            "agent_instruction_app_settings",
            "agent_instruction_settings",
            "roadmap_status_settings",
            "configured_master_steps",
            "configured_master_time",
            "configured_master_frames",
            "user_profile",
        ],
    ),
];

/// A lease record as the worktrees events carry it, of task t1 in /repo under the engine's
/// branch. Output: the record as JSON.
fn lease_sample() -> serde_json::Value {
    let identity = json!({"device": 1, "inode": 2});
    json!({
        "lease": {
            "project_id": "app",
            "task_id": "t1",
            "repository": "/repo",
            "path": format!("/repo/.wt/{}", "a".repeat(64)),
            "branch": format!("crazyagent/{}", "a".repeat(64)),
            "owner_session": "s1",
            "native_descendant": null,
            "state": "Reserved"
        },
        "admission": {"repository": "/repo", "root": identity, "metadata": identity},
        "head": "0".repeat(40),
        "directory": null,
        "metadata": null
    })
}

/// Checks that a worktrees snapshot run carries only the tables [`SNAPSHOTS`] names for
/// worktrees/log.rs, on a store holding both worktree tables. Output: none.
fn worktrees_runs_carry_only_their_named_tables() {
    let (writer, named) = SNAPSHOTS[0];
    assert_eq!(writer, "worktrees/log.rs");
    let mut db = db();
    crate::worktrees::initialize(&db).unwrap();
    let tx = db.transaction().unwrap();
    let mut log = Appender::own(&tx, fence::engine()).unwrap();
    crate::worktrees::log::snapshot(&mut log, 9).unwrap();
    drop(log);
    let mut statement = tx
        .prepare("SELECT body FROM state_log WHERE kind=?1")
        .unwrap();
    let tables: Vec<String> = statement
        .query_map([SNAPSHOT_CHUNK], |row| row.get::<_, Vec<u8>>(0))
        .unwrap()
        .map(|body| {
            let chunk: serde_json::Value = serde_json::from_slice(&body.unwrap()).unwrap();
            chunk["table"].as_str().unwrap().to_owned()
        })
        .collect();
    assert_eq!(tables.len(), named.len(), "one chunk per empty table");
    assert!(
        tables.iter().all(|table| named.contains(&table.as_str())),
        "{tables:?}"
    );
}

/// Text no event body may hold: prose with a key in it, longer than any identity.
/// Output: the text.
fn free_text() -> String {
    format!(
        "the owner's secret prompt with sk-ant-api03-{}",
        "x".repeat(128)
    )
}

/// Checks a typed body: the sample decodes, its keys are exactly `keys`, and the decoder
/// refuses it with free text in any one key but the kind's [`FREE_FORM`] key, which must
/// refuse every path `check_path` refuses, or with one key more. Inputs: the kind, whether
/// its decoder takes a body, the sample and its keys.
fn closed_body(
    kind: &str,
    decodes: impl Fn(&[u8]) -> bool,
    sample: serde_json::Value,
    keys: &[&str],
) {
    let free: Vec<&str> = FREE_FORM
        .iter()
        .filter(|(named, _)| *named == kind)
        .map(|(_, key)| *key)
        .collect();
    assert!(
        free.iter().all(|key| keys.contains(key)),
        "{kind}: {free:?}"
    );
    let mut found: Vec<&str> = sample
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    found.sort_unstable();
    let mut wanted = keys.to_vec();
    wanted.sort_unstable();
    assert_eq!(found, wanted);
    assert!(decodes(&serde_json::to_vec(&sample).unwrap()), "{sample}");
    for key in &free {
        let too_long = "a".repeat(crate::landing::owner_edit::PATH_BYTES_MAX + 1);
        for path in [
            "",
            "/etc/hosts",
            "a//b",
            "./a",
            "a/../b",
            "a\0b",
            too_long.as_str(),
        ] {
            let mut changed = sample.clone();
            changed[*key] = json!(path);
            assert!(
                !decodes(&serde_json::to_vec(&changed).unwrap()),
                "{kind}: {key} took {path:?}"
            );
        }
        let mut number = sample.clone();
        number[*key] = json!(7);
        assert!(!decodes(&serde_json::to_vec(&number).unwrap()), "{kind}");
    }
    for key in keys.iter().filter(|key| !free.contains(key)) {
        let mut changed = sample.clone();
        changed[*key] = json!(free_text());
        assert!(
            !decodes(&serde_json::to_vec(&changed).unwrap()),
            "{key} took free text"
        );
    }
    let mut widened = sample.clone();
    widened["key"] = json!("sk-ant-api03-key");
    assert!(
        !decodes(&serde_json::to_vec(&widened).unwrap()),
        "a body took one key more"
    );
}

/// The source files outside tests whose text names the log's appender, each as its path
/// under `src` with `/` between parts, and its text. A file or folder whose name ends in
/// `tests` or `tests.rs` is left out. Output: the files.
fn writer_sources() -> Vec<(String, String)> {
    const ENTRIES_MAX: usize = 100_000;
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut folders = vec![src.clone()];
    let mut found = Vec::new();
    let mut seen = 0;
    while let Some(folder) = folders.pop() {
        for entry in std::fs::read_dir(&folder).unwrap() {
            seen += 1;
            assert!(seen <= ENTRIES_MAX);
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            if name.ends_with("tests") || name.ends_with("tests.rs") {
                continue;
            }
            if path.is_dir() {
                folders.push(path);
                continue;
            }
            if !name.ends_with(".rs") {
                continue;
            }
            let text = std::fs::read_to_string(&path).unwrap();
            if text.contains("Appender") {
                let parts: Vec<String> = path
                    .strip_prefix(&src)
                    .unwrap()
                    .components()
                    .map(|part| part.as_os_str().to_string_lossy().into_owned())
                    .collect();
                found.push((parts.join("/"), text));
            }
        }
    }
    found
}

/// Checks the snapshot rule of [`no_event_body_can_carry_a_secret`]: only state_log.rs and
/// the writers [`SNAPSHOTS`] names take a snapshot, and a worktrees run carries only its
/// named tables. Input: the writers' paths and sources. Output: none.
fn only_named_writers_take_a_snapshot(writers: &[(String, String)]) {
    assert!(!writers.is_empty(), "the log has writers");
    for (name, text) in writers {
        let named = SNAPSHOTS.iter().any(|(path, _)| *path == name.as_str());
        assert!(
            name.as_str() == "state_log.rs" || named || !text.contains(".snapshot("),
            "{name} takes a snapshot"
        );
    }
    worktrees_runs_carry_only_their_named_tables();
    settings_runs_carry_only_their_named_tables();
}

/// Checks that a settings snapshot run carries only the tables [`SNAPSHOTS`] names for
/// settings_log.rs, one chunk each on a store holding every settings table. Output: none.
fn settings_runs_carry_only_their_named_tables() {
    let (writer, named) = SNAPSHOTS[1];
    assert_eq!(writer, "settings_log.rs");
    let mut db = db();
    crate::settings_log::initialize(&db).unwrap();
    let tx = db.transaction().unwrap();
    let mut log = Appender::own(&tx, fence::engine()).unwrap();
    crate::settings_log::snapshot(&mut log, 9).unwrap();
    drop(log);
    let mut statement = tx
        .prepare("SELECT body FROM state_log WHERE kind=?1 ORDER BY seq")
        .unwrap();
    let tables: Vec<String> = statement
        .query_map([SNAPSHOT_CHUNK], |row| row.get::<_, Vec<u8>>(0))
        .unwrap()
        .map(|body| {
            let chunk: serde_json::Value = serde_json::from_slice(&body.unwrap()).unwrap();
            chunk["table"].as_str().unwrap().to_owned()
        })
        .collect();
    assert_eq!(tables, named, "one chunk per empty table, in order");
}

/// Checks the closed body of one settings kind (v2.5 L10): the setting and, for a project
/// setting, the project id, or the profile's kept-text digest, refusing free text in any and
/// any key more. The settings are enums, bounded numbers and switches, and the profile text
/// stays in owner_texts (Q5), never in a body. Input: the kind. Output: none.
fn settings_body_is_closed(kind: &str) {
    use crate::settings_log as settings;
    assert!(KINDS.contains(&kind), "{kind} is a listed kind");
    let decodes = |body: &[u8]| {
        settings::decode(&replay::Logged {
            seq: 1,
            revision: 1,
            aggregate: Aggregate::Settings,
            kind: kind.into(),
            schema: 1,
            at_ms: 1,
            body: body.to_vec(),
        })
        .is_ok()
    };
    let value = |settings: serde_json::Value| json!({"settings": settings});
    let (sample, keys): (serde_json::Value, &[&str]) = match kind {
        settings::KIND_SANDBOX => (
            value(json!(crate::sandbox_settings::SandboxSettings::default())),
            &["settings"],
        ),
        settings::KIND_CLEANUP => (
            value(json!(crate::cleanup_settings::CleanupSettings::default())),
            &["settings"],
        ),
        settings::KIND_QUIT => (
            value(json!(crate::quit_settings::QuitSettings::default())),
            &["settings"],
        ),
        settings::KIND_RETRY => (
            value(json!(crate::provider_retry::ProviderRetry::default())),
            &["settings"],
        ),
        settings::KIND_HARNESS_UPDATE => (
            value(json!(crate::harnesses::update::UpdateSettings::default())),
            &["settings"],
        ),
        settings::KIND_INSTRUCTION_APP => (
            value(json!(crate::instruction_files::AppSettings::default())),
            &["settings"],
        ),
        settings::KIND_INSTRUCTION => (
            json!({
                "project": "app",
                "settings": crate::instruction_files::Settings::default()
            }),
            &["project", "settings"],
        ),
        settings::KIND_ROADMAP => (
            json!({
                "project": "app",
                "settings": crate::roadmap_status::Refresh::default()
            }),
            &["project", "settings"],
        ),
        settings::KIND_STEPS => (
            value(json!(
                crate::master_step_limit::MasterStepSettings::default()
            )),
            &["settings"],
        ),
        settings::KIND_TIME => (
            value(json!(
                crate::master_time_limit::MasterTimeSettings::default()
            )),
            &["settings"],
        ),
        settings::KIND_FRAMES => (
            value(json!(
                crate::master_frame_limit::MasterFrameSettings::default()
            )),
            &["settings"],
        ),
        settings::KIND_PROFILE => (json!({"body": "0".repeat(64)}), &["body"]),
        other => panic!("{other} is not a settings kind"),
    };
    let mut nested = sample.clone();
    if let Some(fields) = nested["settings"].as_object_mut() {
        fields.insert("key".into(), json!("sk-ant-api03-key"));
        assert!(
            !decodes(&serde_json::to_vec(&nested).unwrap()),
            "{kind}: a setting took one key more"
        );
    }
    closed_body(kind, decodes, sample, keys);
}

/// Checks that a snapshot's closing body holds only its aggregate, its chunk count and its
/// hash, 64 lowercase hex digits, so no free text rides in it. Output: none.
fn a_closing_body_is_its_hash() {
    let mut db = db();
    let tx = db.transaction().unwrap();
    let mut log = Appender::own(&tx, fence::engine()).unwrap();
    let done = log.snapshot(Aggregate::Worktrees, 9, chunks()).unwrap();
    let seq = i64::try_from(done).expect("a log seq fits in i64");
    let body: Vec<u8> = tx
        .query_row("SELECT body FROM state_log WHERE seq=?1", [seq], |row| {
            row.get(0)
        })
        .unwrap();
    let closing: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let mut keys: Vec<&str> = closing
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(keys, ["aggregate", "chunks", "sha256"]);
    let sha256 = closing["sha256"].as_str().unwrap();
    assert_eq!(sha256.len(), 64);
    assert!(
        sha256
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    );
}

/// Checks the closed body of one worktrees kind: project or task and the typed record,
/// refusing free text in either and any key more. Input: the kind. Output: none.
fn worktrees_body_is_closed(kind: &str) {
    assert!(KINDS.contains(&kind), "{kind} is a listed kind");
    match kind {
        crate::worktrees::log::KIND_ADMITTED => closed_body(
            kind,
            |body| serde_json::from_slice::<crate::worktrees::log::Admitted>(body).is_ok(),
            json!({"project": "app", "body": lease_sample()["admission"].clone()}),
            &["project", "body"],
        ),
        crate::worktrees::log::KIND_RESERVED => closed_body(
            kind,
            |body| serde_json::from_slice::<crate::worktrees::log::Reserved>(body).is_ok(),
            json!({"task": "t1", "body": lease_sample()}),
            &["task", "body"],
        ),
        crate::worktrees::log::KIND_SAVED => closed_body(
            kind,
            |body| serde_json::from_slice::<crate::worktrees::log::Saved>(body).is_ok(),
            json!({"task": "t1", "body": lease_sample()}),
            &["task", "body"],
        ),
        other => panic!("{other} is not a worktrees kind"),
    }
}

/// Checks the closed body of one ide kind: the project, ids, typed values and the one
/// [`FREE_FORM`] path, refusing free text elsewhere and any key more. Input: the kind.
/// Output: none.
fn ide_body_is_closed(kind: &str) {
    use crate::ide::log::WorkspaceWritten;
    use crate::ide::log::{FileUnlocked, IdeAsked, RunModeSet, WorkspaceForgotten};
    assert!(KINDS.contains(&kind), "{kind} is a listed kind");
    match kind {
        crate::ide::log::IDE_ASKED => closed_body(
            kind,
            |body| serde_json::from_slice::<IdeAsked>(body).is_ok(),
            json!({"project": "app", "entry": "m1", "version": 3, "decision": "approve",
                   "asked_ms": 5}),
            &["project", "entry", "version", "decision", "asked_ms"],
        ),
        crate::ide::log::RUN_MODE_SET => closed_body(
            kind,
            |body| serde_json::from_slice::<RunModeSet>(body).is_ok(),
            json!({"project": "app", "mode": "type_only"}),
            &["project", "mode"],
        ),
        crate::ide::log::FILE_UNLOCKED => closed_body(
            kind,
            |body| serde_json::from_slice::<FileUnlocked>(body).is_ok(),
            json!({"project": "app", "agent": 7, "path": "src/main file.rs", "turns": 2}),
            &["project", "agent", "path", "turns"],
        ),
        crate::ide::log::WORKSPACE_WRITTEN => closed_body(
            kind,
            |body| serde_json::from_slice::<WorkspaceWritten>(body).is_ok(),
            json!({"project": "app", "show_all": true}),
            &["project", "show_all"],
        ),
        crate::ide::log::WORKSPACE_FORGOTTEN => closed_body(
            kind,
            |body| serde_json::from_slice::<WorkspaceForgotten>(body).is_ok(),
            json!({"project": "app"}),
            &["project"],
        ),
        other => panic!("{other} is not an ide kind"),
    }
}

/// L1-N1: no event body can carry a secret, a key, a token or prompt text. Each kind's body
/// has closed keys whose decoder refuses free text and any key more (ids: 1 to 128 safe ASCII
/// bytes), but for one [`FREE_FORM`] path that refuses what `check_path` refuses. A closing
/// hash is 64 hex digits; chunk rows are any JSON, so a writer outside this module snapshots
/// only its [`SNAPSHOTS`] tables. [`WRITERS`] lists the writers outside tests and their kinds.
#[test]
fn no_event_body_can_carry_a_secret() {
    for kind in crate::ide::log::KINDS {
        assert!(
            KINDS.contains(&kind),
            "the ide kind {kind} needs a body rule"
        );
    }
    let writers = writer_sources();
    let mut names: Vec<&str> = writers.iter().map(|(name, _)| name.as_str()).collect();
    names.sort_unstable();
    let listed: Vec<&str> = WRITERS.iter().map(|(name, _)| *name).collect();
    assert_eq!(names, listed, "a new log writer needs its kinds in KINDS");
    for (name, text) in &writers {
        let (_, own) = WRITERS
            .iter()
            .find(|(path, _)| *path == name.as_str())
            .unwrap();
        for line in text.lines().map(str::trim) {
            if let Some(written) = line.strip_prefix("kind: ") {
                assert!(own.contains(&written), "{name}: {line}");
            }
        }
    }
    for kind in KINDS {
        match kind {
            SNAPSHOT_CHUNK => only_named_writers_take_a_snapshot(&writers),
            SNAPSHOT_DONE => a_closing_body_is_its_hash(),
            replay::PROCESS_EXITED => closed_body(
                kind,
                |body| serde_json::from_slice::<replay::ProcessExited>(body).is_ok(),
                json!({"pid": 7, "code": 0, "signal": null}),
                &["pid", "code", "signal"],
            ),
            crate::project_setup::removed::event::KIND => closed_body(
                kind,
                |body| {
                    serde_json::from_slice::<crate::project_setup::removed::event::ProjectRemoved>(
                        body,
                    )
                    .is_ok()
                },
                json!({"project": "app", "kept": true}),
                &["project", "kept"],
            ),
            crate::master_session::SESSION_DELETED => closed_body(
                kind,
                |body| {
                    serde_json::from_slice::<crate::master_session::SessionDeleted>(body).is_ok()
                },
                json!({"project": "app", "session": "s1"}),
                &["project", "session"],
            ),
            crate::worktrees::log::KIND_ADMITTED
            | crate::worktrees::log::KIND_RESERVED
            | crate::worktrees::log::KIND_SAVED => worktrees_body_is_closed(kind),
            crate::ide::log::IDE_ASKED
            | crate::ide::log::RUN_MODE_SET
            | crate::ide::log::FILE_UNLOCKED
            | crate::ide::log::WORKSPACE_WRITTEN
            | crate::ide::log::WORKSPACE_FORGOTTEN => ide_body_is_closed(kind),
            settings if crate::settings_log::KINDS.contains(&settings) => {
                settings_body_is_closed(settings)
            }
            other => panic!("the kind {other} has no body rule"),
        }
    }
}
