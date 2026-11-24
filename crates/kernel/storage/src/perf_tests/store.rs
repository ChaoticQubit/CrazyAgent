use std::hint::black_box;
use std::path::Path;

use rusqlite::{Connection, Transaction};

use super::{AT_MS, chunks, row};
use crate::events::{logged, persist};
use crate::namespace::{self, Namespace, PLUGINS};
use crate::perf_kit::{SAMPLES, time};
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

/// Runs one write in a transaction and rolls it back, so every sample starts from the same
/// store; the begin and the rollback are inside the timed span. Inputs: the connection and
/// the write. Output: none.
fn rolled_back(db: &mut Connection, write: impl FnOnce(&Transaction<'_>)) {
    let tx = db.transaction().expect("tx");
    write(&tx);
    tx.rollback().expect("rollback");
}

/// Times every storage call on one engine store: the watch's events, plugin records, the
/// log's appender, snapshot check, compaction, replay, redaction and the WAL checkpoint.
/// Each write row runs in a transaction rolled back per call, begin and rollback inside the
/// timed span, and its name ends `.rolled_back`; `tx.begin_rollback` times that pair alone.
/// The appender rows open their appender with `own` inside the span, as `Appender.own`
/// times; `Appender.owner` runs `events::persist` first, as an owner request does.
/// `replay.rows` is a test-only call, timed because it is public. Prints one PERF line per
/// call.
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

    time(&row("tx.begin_rollback", READ_BATCH), SAMPLES, || {
        rolled_back(&mut db, |tx| {
            black_box(tx);
        });
    });
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
    time(
        &row("events.persist.rolled_back", WRITE_BATCH),
        SAMPLES,
        || {
            rolled_back(&mut db, |tx| {
                persist(tx, now + 1, EVENT_BODY, EVENTS_KEPT).expect("persist");
            });
        },
    );
    time(&row("namespace.initialize", READ_BATCH), SAMPLES, || {
        let _ = black_box(namespace::initialize(black_box(&db)));
    });
    time(&row("state_log.ensure", READ_BATCH), SAMPLES, || {
        let _ = black_box(ensure(black_box(&db)));
    });
    time(
        &row("state_log.Appender.owner.rolled_back", WRITE_BATCH),
        SAMPLES,
        || {
            rolled_back(&mut db, |tx| {
                black_box(owner(tx, now));
            });
        },
    );
    time(
        &row("state_log.Appender.own.rolled_back", WRITE_BATCH),
        SAMPLES,
        || {
            rolled_back(&mut db, |tx| {
                black_box(own(tx));
            });
        },
    );
    time(
        &row("state_log.Appender.project.rolled_back", WRITE_BATCH),
        SAMPLES,
        || {
            rolled_back(&mut db, |tx| {
                let appender = own(tx);
                let _ = black_box(appender.project(|_| Ok::<_, crate::StorageError>(())));
            });
        },
    );
    time(
        &row("state_log.Appender.append.rolled_back", WRITE_BATCH),
        SAMPLES,
        || {
            rolled_back(&mut db, |tx| {
                let mut appender = own(tx);
                black_box(appender.append(black_box(&draft)).expect("append"));
            });
        },
    );
    time(
        &row("state_log.Appender.snapshot.rolled_back", WRITE_BATCH),
        SAMPLES,
        || {
            rolled_back(&mut db, |tx| {
                let mut appender = own(tx);
                black_box(
                    appender
                        .snapshot(PLUGINS, AT_MS, chunks())
                        .expect("snapshot"),
                );
            });
        },
    );
    time(
        &row("namespace.put.rolled_back", WRITE_BATCH),
        SAMPLES,
        || {
            rolled_back(&mut db, |tx| {
                let mut appender = own(tx);
                namespace::put(&mut appender, &ns, &key(1), &value, AT_MS).expect("put");
            });
        },
    );
    time(
        &row("namespace.delete.rolled_back", WRITE_BATCH),
        SAMPLES,
        || {
            rolled_back(&mut db, |tx| {
                let mut appender = own(tx);
                black_box(namespace::delete(&mut appender, &ns, &key(2), AT_MS).expect("delete"));
            });
        },
    );
    time(
        &row("namespace.apply.rolled_back", WRITE_BATCH),
        SAMPLES,
        || {
            rolled_back(&mut db, |tx| {
                namespace::apply(tx, black_box(&event)).expect("apply");
            });
        },
    );
    time(
        &row("state_log.compact.compact.off.rolled_back", WRITE_BATCH),
        SAMPLES,
        || {
            rolled_back(&mut db, |tx| {
                let mut appender = own(tx);
                let _ = black_box(compact(
                    &mut appender,
                    Compaction::Off,
                    PLUGINS,
                    AT_MS,
                    |_| Ok::<_, crate::StorageError>(chunks()),
                ));
            });
        },
    );
    time(
        &row(
            "state_log.compact.compact.on_not_due.rolled_back",
            WRITE_BATCH,
        ),
        SAMPLES,
        || {
            rolled_back(&mut db, |tx| {
                let mut appender = own(tx);
                let _ = black_box(compact(
                    &mut appender,
                    Compaction::On,
                    PLUGINS,
                    AT_MS,
                    |_| Ok::<_, crate::StorageError>(chunks()),
                ));
            });
        },
    );
    time(
        &row("state_log.replay.redact.rolled_back", WRITE_BATCH),
        SAMPLES,
        || {
            rolled_back(&mut db, |tx| {
                redact(tx, newest_put, &passing()).expect("redact");
            });
        },
    );
    time(
        &row("state_log.replay.redact_matching.rolled_back", WRITE_BATCH),
        SAMPLES,
        || {
            rolled_back(&mut db, |tx| {
                black_box(
                    redact_matching(
                        tx,
                        &[PLUGINS],
                        Some(&["RecordPut"]),
                        "key",
                        &key(3),
                        0,
                        &passing(),
                    )
                    .expect("redact matching"),
                );
            });
        },
    );
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
    time(
        &row("state_log.replay.checkpoint", WRITE_BATCH),
        SAMPLES,
        || {
            black_box(checkpoint(black_box(&db)));
        },
    );
}
