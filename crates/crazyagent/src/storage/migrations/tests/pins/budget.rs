use super::*;
use crate::state_log::replay::replay;
use crate::state_log::{Aggregate, Appender, Chunk, Draft, fence};
use std::time::Instant;

/// Samples of the one-event owner append, the call the engine makes most.
const APPEND_RUNS: usize = 2_000;

/// Samples of each slower call: snapshot, replay, upgrade and backup.
const SLOW_RUNS: usize = 200;

/// Chunks in the timed snapshot run.
const SNAPSHOT_CHUNKS: usize = 64;

/// Events of one aggregate the timed replay reads.
const REPLAY_EVENTS: usize = 10_000;

/// One timed storage call: its name, its sample count and the work, which sets up outside
/// its timed span and returns the span. Its samples are taken on a scratch engine.db.
struct Call {
    name: &'static str,
    runs: usize,
    run: Box<dyn FnMut() -> Duration>,
}

/// A file engine.db with the log's tables, WAL and full sync as the storage open sets them,
/// and the host fence seeded. Input: the folder. Output: the connection.
fn log_store(folder: &Path) -> Connection {
    let db = Connection::open(folder.join("engine.db")).unwrap();
    let mode: String = db
        .query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))
        .unwrap();
    assert_eq!(mode, "wal");
    db.execute_batch(
        "PRAGMA synchronous=FULL;
        CREATE TABLE state (id INTEGER PRIMARY KEY CHECK(id=1), revision INTEGER NOT NULL);
        CREATE TABLE events (cursor INTEGER PRIMARY KEY, body TEXT NOT NULL);
        INSERT INTO state VALUES (1,0);",
    )
    .unwrap();
    fence::seed(&db);
    db
}

/// A worktrees event of a short id body, the shape of the line's typed events.
/// Output: the draft.
fn draft() -> Draft<'static> {
    Draft {
        aggregate: Aggregate::Worktrees,
        kind: "WorktreeAllocated",
        schema: 1,
        at_ms: 7,
        body: br#"{"id":"w-0001"}"#,
    }
}

/// The append call: one own transaction, one event, commit. Input: the scratch folder.
/// Output: the call.
fn append_call(folder: &Path) -> Call {
    let mut db = log_store(folder);
    Call {
        name: "append",
        runs: APPEND_RUNS,
        run: Box::new(move || {
            let tx = db.transaction().unwrap();
            let start = Instant::now();
            let mut log = Appender::own(&tx, fence::engine()).unwrap();
            log.append(&draft()).unwrap();
            drop(log);
            tx.commit().unwrap();
            start.elapsed()
        }),
    }
}

/// The snapshot call: one own transaction writing a run of [`SNAPSHOT_CHUNKS`] chunks, commit.
/// Input: the scratch folder. Output: the call.
fn snapshot_call(folder: &Path) -> Call {
    let mut db = log_store(folder);
    Call {
        name: "snapshot",
        runs: SLOW_RUNS,
        run: Box::new(move || {
            let chunks: Vec<Chunk> = (0..SNAPSHOT_CHUNKS)
                .map(|index| Chunk {
                    table: "worktrees".into(),
                    rows: vec![serde_json::json!({ "id": index, "path": "w/0001" })],
                })
                .collect();
            let tx = db.transaction().unwrap();
            let start = Instant::now();
            let mut log = Appender::own(&tx, fence::engine()).unwrap();
            log.snapshot(Aggregate::Worktrees, 9, chunks).unwrap();
            drop(log);
            tx.commit().unwrap();
            start.elapsed()
        }),
    }
}

/// The replay call: every event of one aggregate holding [`REPLAY_EVENTS`] events, read in
/// order into a projection that keeps nothing. Input: the scratch folder. Output: the call.
fn replay_call(folder: &Path) -> Call {
    let mut db = log_store(folder);
    let tx = db.transaction().unwrap();
    let mut log = Appender::own(&tx, fence::engine()).unwrap();
    for _ in 0..REPLAY_EVENTS {
        log.append(&draft()).unwrap();
    }
    drop(log);
    tx.commit().unwrap();
    Call {
        name: "replay",
        runs: SLOW_RUNS,
        run: Box::new(move || {
            let mut applied = 0_usize;
            let start = Instant::now();
            let last = replay(&db, Aggregate::Worktrees, 0, |_| {
                applied += 1;
                Ok(())
            })
            .unwrap();
            let spent = start.elapsed();
            assert_eq!(applied, REPLAY_EVENTS);
            assert_eq!(last, REPLAY_EVENTS as u64);
            spent
        }),
    }
}

/// The upgrade call: the P5 2.4.2 fixture copied into a fresh root, upgraded through every
/// step with its backup. Output: the call.
fn upgrade_call() -> Call {
    Call {
        name: "upgrade",
        runs: SLOW_RUNS,
        run: Box::new(|| {
            let (root, _) = fixture();
            let sources = sources(&root);
            let mut db = root.connection();
            let start = Instant::now();
            upgrade(&mut db, &root.database(), Some(&sources), STEPS).unwrap();
            let spent = start.elapsed();
            assert_eq!(version(&db).unwrap(), latest(STEPS));
            spent
        }),
    }
}

/// The backup call: the checked backup of the P5 fixture an upgrade to the newest step takes.
/// Output: the call.
fn backup_call() -> Call {
    Call {
        name: "backup",
        runs: SLOW_RUNS,
        run: Box::new(|| {
            let (root, _) = fixture();
            let sources = sources(&root);
            let db = root.connection();
            let versions = (V24_VERSION_MAX, latest(STEPS));
            let start = Instant::now();
            let folder = backup(
                &db,
                &root.database(),
                Some(&sources),
                versions,
                SystemTime::now(),
                check_copy,
            )
            .unwrap();
            let spent = start.elapsed();
            assert!(folder.join(CHECKED).exists());
            spent
        }),
    }
}

/// Times each storage call on a scratch engine.db and prints its p50 and p95 in
/// microseconds (pod B Task 2, the before of owner Q3). Inputs: none (the P5 fixture under
/// tests/fixtures). Output: one `budget` line per call on stdout; every sample is taken.
#[test]
#[ignore = "budget row: run with --ignored --nocapture on a quiet and a loaded Mac"]
fn budget_rows() {
    let scratch = Root::new();
    let folders: Vec<PathBuf> = ["append", "snapshot", "replay"]
        .iter()
        .map(|name| scratch.path().join(name))
        .collect();
    for folder in &folders {
        std::fs::create_dir(folder).unwrap();
    }
    let calls = [
        append_call(&folders[0]),
        snapshot_call(&folders[1]),
        replay_call(&folders[2]),
        upgrade_call(),
        backup_call(),
    ];
    for Call {
        name,
        runs,
        mut run,
    } in calls
    {
        let mut micros: Vec<u128> = (0..runs).map(|_| run().as_micros()).collect();
        assert_eq!(micros.len(), runs);
        micros.sort_unstable();
        println!(
            "budget {name} n={runs} p50_us={} p95_us={}",
            micros[runs / 2],
            micros[runs * 95 / 100]
        );
    }
}
