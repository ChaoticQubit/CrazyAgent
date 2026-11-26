use std::hint::black_box;
use std::path::Path;
use std::time::Instant;

use rusqlite::{Connection, Transaction};

use super::{AT_MS, chunks, nanos, row, sampled};
use crate::events::{logged, persist};
use crate::namespace::{self, Namespace, PLUGINS};
use crate::perf_kit::{Row, SAMPLES, time};
use crate::state_log::compact::{Compaction, compact};
use crate::state_log::replay::{Logged, checkpoint, redact, redact_matching, replay, rows};
use crate::state_log::{
    AggregateName, Appender, Draft, FenceCheck, TOMBSTONE, ensure, verify_snapshot,
};

const READ_BATCH: usize = 10;
const WRITE_BATCH: usize = 1;
const PLUGIN: &str = "acme-embeddings";
const RECORDS: usize = 256;
const VALUE_BYTES: usize = 256;
const LOGGED: usize = 1000;
const EVENTS_KEPT: u64 = 128;
const EVENT_BODY: &str =
    r#"{"cursor":1,"request_id":"perf-request-0001","client_id":"perf-client"}"#;
const LIST_LIMIT: usize = 64;
const CATCH_UP: u64 = 100;

/// The engine's aggregate names these rows write. Input: the name. Output: it, or none.
fn known(name: &str) -> Option<AggregateName> {
    (name == PLUGINS.name()).then_some(PLUGINS)
}

/// A fence that passes every check, as a running host's proof does. Output: the hook.
fn passing() -> FenceCheck {
    Box::new(|_| Ok(()))
}

/// The key of one record. Input: its index. Output: the key.
fn key(index: usize) -> String {
    format!("record-{index:04}")
}

/// Opens the store a 2.5 engine leaves after a plugin has written its records: WAL,
/// `state` and `events` as the engine makes them with [`EVENTS_KEPT`] events, the log with
/// [`LOGGED`] record puts over [`RECORDS`] keys of [`VALUE_BYTES`] each, one per revision,
/// then one snapshot run. Input: the folder. Output: the connection and the seq of the
/// run's closing event.
pub(crate) fn store(folder: &Path) -> (Connection, u64) {
    let mut db = Connection::open(folder.join("engine.db")).expect("store");
    db.pragma_update(None, "journal_mode", "WAL").expect("wal");
    db.execute_batch(
        "CREATE TABLE state (id INTEGER PRIMARY KEY CHECK(id=1), revision INTEGER NOT NULL);
         INSERT INTO state VALUES (1, 0);
         CREATE TABLE events (cursor INTEGER PRIMARY KEY, body TEXT NOT NULL);",
    )
    .expect("schema");
    ensure(&db).expect("log");
    namespace::initialize(&db).expect("records");
    let ns = Namespace::for_plugin(PLUGIN).expect("namespace");
    let value = vec![b'v'; VALUE_BYTES];
    for index in 0..LOGGED {
        let tx = db.transaction().expect("tx");
        let mut appender = Appender::own(&tx, passing(), known, |revision| {
            Ok(EVENT_BODY.replace("\"cursor\":1", &format!("\"cursor\":{revision}")))
        })
        .expect("appender");
        namespace::put(&mut appender, &ns, &key(index % RECORDS), &value, AT_MS).expect("put");
        drop(appender);
        tx.commit().expect("commit");
    }
    let tx = db.transaction().expect("tx");
    let mut appender =
        Appender::own(&tx, passing(), known, |_| Ok(EVENT_BODY.to_owned())).expect("appender");
    let done = appender
        .snapshot(PLUGINS, AT_MS, chunks())
        .expect("snapshot");
    drop(appender);
    tx.commit().expect("commit");
    db.execute(
        "DELETE FROM events WHERE cursor <= (SELECT revision FROM state) - ?1",
        [EVENTS_KEPT as i64],
    )
    .expect("trim");
    (db, done)
}

/// The store's `state` revision. Input: the connection. Output: the revision.
pub(crate) fn revision(db: &Connection) -> u64 {
    let revision: i64 = db
        .query_row("SELECT revision FROM state WHERE id=1", [], |row| {
            row.get(0)
        })
        .expect("revision");
    u64::try_from(revision).expect("revision")
}

/// Opens an appender the way a writer on its own connection does, moving the revision and
/// writing its `events` row. Input: the transaction. Output: the appender.
fn own<'a, 'c>(tx: &'a Transaction<'c>) -> Appender<'a, 'c> {
    Appender::own(tx, passing(), known, |_| Ok(EVENT_BODY.to_owned())).expect("own")
}

/// Opens an appender the way an owner request does: [`persist`] moves the revision and
/// writes its `events` row, then [`Appender::owner`] checks that revision. Inputs: the
/// transaction and the revision before. Output: the appender.
fn owner<'a, 'c>(tx: &'a Transaction<'c>, before: u64) -> Appender<'a, 'c> {
    persist(tx, before + 1, EVENT_BODY, EVENTS_KEPT).expect("persist");
    Appender::owner(tx, before + 1, passing(), known).expect("owner")
}

/// The write the checkpoint row commits before each call, outside the timed span, so the
/// WAL holds one changed page as after an owner delete. The 2.4.4 twin runs the same text.
const WAL_WRITE: &str =
    "UPDATE events SET body = body WHERE cursor = (SELECT MAX(cursor) FROM events)";

/// Times one call per sample in a transaction rolled back after it, so every sample starts
/// from the same store: the begin, `prepare` and the rollback run outside the timed span.
/// Inputs: the row (batch 1), the connection, what to do before the call and the call.
/// Output: none; prints the PERF line.
fn time_tx<T>(
    row: &Row,
    db: &mut Connection,
    mut prepare: impl FnMut(&Transaction<'_>) -> T,
    mut call: impl FnMut(&Transaction<'_>, T),
) {
    sampled(row, SAMPLES, || {
        let tx = db.transaction().expect("tx");
        let input = prepare(&tx);
        let started = Instant::now();
        call(&tx, input);
        let spent = nanos(started);
        tx.rollback().expect("rollback");
        spent
    });
}

/// Times one appender call per sample: the begin, the appender's open through
/// [`Appender::own`], `prepare` and the rollback run outside the timed span. Inputs: the
/// row (batch 1), the connection, what to build before the call and the call. Output: none;
/// prints the PERF line.
fn time_appender<T>(
    row: &Row,
    db: &mut Connection,
    mut prepare: impl FnMut() -> T,
    mut call: impl FnMut(&mut Appender<'_, '_>, T),
) {
    sampled(row, SAMPLES, || {
        let tx = db.transaction().expect("tx");
        let mut appender = own(&tx);
        let input = prepare();
        let started = Instant::now();
        call(&mut appender, input);
        let spent = nanos(started);
        drop(appender);
        tx.rollback().expect("rollback");
        spent
    });
}

/// Times every storage call on one engine store: the watch's events, plugin records, the
/// log's appender, snapshot check, compaction, replay, redaction and the WAL checkpoint.
/// Each write runs in a transaction rolled back after it, with the begin, any appender open
/// and the rollback outside the timed span. `Appender.owner` has `events::persist` run
/// before it outside the span, as an owner request moves the revision first. Compaction is
/// timed on its two paths that write nothing: Off, the default, and On before the next run
/// is due. `replay.rows` is a test-only call, timed because it is public. Prints one PERF
/// line per call.
#[test]
#[ignore = "timing row: run through scripts/perf/run.sh"]
fn storage_store_calls() {
    let temp = tempfile::tempdir().expect("temp");
    let (mut db, done) = store(temp.path());
    let now = revision(&db);
    let ns = Namespace::for_plugin(PLUGIN).expect("namespace");
    let value = vec![b'w'; VALUE_BYTES];
    let draft_body = vec![b'd'; VALUE_BYTES];
    let draft = Draft {
        aggregate: PLUGINS,
        kind: "RecordPut",
        schema: 1,
        at_ms: AT_MS,
        body: &draft_body,
    };
    let newest_put: i64 = db
        .query_row(
            "SELECT MAX(seq) FROM state_log WHERE kind='RecordPut' AND body<>?1",
            [TOMBSTONE],
            |row| row.get(0),
        )
        .expect("newest put");
    let newest_put = u64::try_from(newest_put).expect("seq");
    let event: Logged = {
        let mut found = None;
        replay(&db, PLUGINS, newest_put - 1, known, |event| {
            if found.is_none() {
                found = Some(event.clone());
            }
            Ok::<(), crate::StorageError>(())
        })
        .expect("replay");
        found.expect("one put")
    };
    {
        let tx = db.transaction().expect("tx");
        assert_eq!(
            logged(&tx, now - CATCH_UP, now)
                .expect("logged")
                .map(|bodies| bodies.len()),
            Some(CATCH_UP as usize)
        );
        assert_eq!(
            namespace::get(&tx, &ns, &key(0))
                .expect("get")
                .map(|v| v.len()),
            Some(VALUE_BYTES)
        );
        assert_eq!(
            namespace::list(&tx, &ns, None, LIST_LIMIT)
                .expect("list")
                .len(),
            LIST_LIMIT
        );
        let mut appender = owner(&tx, now);
        assert_eq!(appender.revision(), now + 1);
        namespace::put(&mut appender, &ns, &key(1), &value, AT_MS).expect("put");
        assert!(namespace::delete(&mut appender, &ns, &key(2), AT_MS).expect("delete"));
        assert_eq!(
            compact(&mut appender, Compaction::On, PLUGINS, AT_MS, |_| Ok::<
                _,
                crate::StorageError,
            >(
                chunks()
            ))
            .expect("compact"),
            0
        );
        drop(appender);
        assert_eq!(
            redact_matching(
                &tx,
                &[PLUGINS],
                Some(&["RecordPut"]),
                "key",
                &key(3),
                0,
                &passing()
            )
            .expect("redact matching"),
            1
        );
        tx.rollback().expect("rollback");
    }
    verify_snapshot(&db, done).expect("snapshot run");
    assert_eq!(rows(&db, "plugin_records").expect("rows").len(), RECORDS);
    assert!(checkpoint(&db));

    {
        let tx = db.transaction().expect("tx");
        time(&row("events.logged", READ_BATCH), SAMPLES, || {
            let _ = black_box(logged(black_box(&tx), now - CATCH_UP, now));
        });
        time(&row("namespace.get", READ_BATCH), SAMPLES, || {
            let _ = black_box(namespace::get(black_box(&tx), &ns, black_box(&key(0))));
        });
        time(&row("namespace.list", READ_BATCH), SAMPLES, || {
            let _ = black_box(namespace::list(black_box(&tx), &ns, None, LIST_LIMIT));
        });
        let appender = own(&tx);
        time(&row("state_log.Appender.revision", 1000), SAMPLES, || {
            black_box(black_box(&appender).revision());
        });
        drop(appender);
        tx.rollback().expect("rollback");
    }
    time(&row("namespace.initialize", READ_BATCH), SAMPLES, || {
        let _ = black_box(namespace::initialize(black_box(&db)));
    });
    time(&row("state_log.ensure", READ_BATCH), SAMPLES, || {
        let _ = black_box(ensure(black_box(&db)));
    });
    time(
        &row("state_log.verify_snapshot", WRITE_BATCH),
        SAMPLES,
        || {
            let _ = black_box(verify_snapshot(black_box(&db), done));
        },
    );
    time(
        &row("state_log.replay.replay", WRITE_BATCH),
        SAMPLES,
        || {
            let _ = black_box(replay(&db, PLUGINS, 0, known, |event| {
                black_box(event);
                Ok::<(), crate::StorageError>(())
            }));
        },
    );
    time(&row("state_log.replay.rows", WRITE_BATCH), SAMPLES, || {
        let _ = black_box(rows(black_box(&db), "plugin_records"));
    });
    time_tx(
        &row("events.persist", WRITE_BATCH),
        &mut db,
        |_| (),
        |tx, ()| persist(tx, now + 1, EVENT_BODY, EVENTS_KEPT).expect("persist"),
    );
    time_tx(
        &row("state_log.Appender.owner", WRITE_BATCH),
        &mut db,
        |tx| {
            persist(tx, now + 1, EVENT_BODY, EVENTS_KEPT).expect("persist");
            passing()
        },
        |tx, fence| {
            black_box(Appender::owner(tx, now + 1, fence, known).expect("owner"));
        },
    );
    time_tx(
        &row("state_log.Appender.own", WRITE_BATCH),
        &mut db,
        |_| (),
        |tx, ()| {
            black_box(own(tx));
        },
    );
    time_appender(
        &row("state_log.Appender.project", WRITE_BATCH),
        &mut db,
        || (),
        |appender, ()| {
            let _ = black_box(appender.project(|_| Ok::<_, crate::StorageError>(())));
        },
    );
    time_appender(
        &row("state_log.Appender.append", WRITE_BATCH),
        &mut db,
        || (),
        |appender, ()| {
            black_box(appender.append(black_box(&draft)).expect("append"));
        },
    );
    time_appender(
        &row("state_log.Appender.snapshot", WRITE_BATCH),
        &mut db,
        chunks,
        |appender, run| {
            black_box(appender.snapshot(PLUGINS, AT_MS, run).expect("snapshot"));
        },
    );
    time_appender(
        &row("namespace.put", WRITE_BATCH),
        &mut db,
        || (),
        |appender, ()| namespace::put(appender, &ns, &key(1), &value, AT_MS).expect("put"),
    );
    time_appender(
        &row("namespace.delete", WRITE_BATCH),
        &mut db,
        || (),
        |appender, ()| {
            black_box(namespace::delete(appender, &ns, &key(2), AT_MS).expect("delete"));
        },
    );
    time_appender(
        &row("state_log.compact.compact.off", WRITE_BATCH),
        &mut db,
        || (),
        |appender, ()| {
            let _ = black_box(compact(appender, Compaction::Off, PLUGINS, AT_MS, |_| {
                Ok::<_, crate::StorageError>(chunks())
            }));
        },
    );
    time_appender(
        &row("state_log.compact.compact.on_not_due", WRITE_BATCH),
        &mut db,
        || (),
        |appender, ()| {
            let _ = black_box(compact(appender, Compaction::On, PLUGINS, AT_MS, |_| {
                Ok::<_, crate::StorageError>(chunks())
            }));
        },
    );
    time_tx(
        &row("namespace.apply", WRITE_BATCH),
        &mut db,
        |_| (),
        |tx, ()| namespace::apply(tx, black_box(&event)).expect("apply"),
    );
    time_tx(
        &row("state_log.replay.redact", WRITE_BATCH),
        &mut db,
        |_| passing(),
        |tx, fence| redact(tx, newest_put, &fence).expect("redact"),
    );
    time_tx(
        &row("state_log.replay.redact_matching", WRITE_BATCH),
        &mut db,
        |_| passing(),
        |tx, fence| {
            black_box(
                redact_matching(
                    tx,
                    &[PLUGINS],
                    Some(&["RecordPut"]),
                    "key",
                    &key(3),
                    0,
                    &fence,
                )
                .expect("redact matching"),
            );
        },
    );
    sampled(
        &row("state_log.replay.checkpoint", WRITE_BATCH),
        SAMPLES,
        || {
            db.execute(WAL_WRITE, []).expect("wal write");
            let started = Instant::now();
            black_box(checkpoint(black_box(&db)));
            nanos(started)
        },
    );
}
