use crate::agents::now_ms;
use crate::config::resolve_config_bytes;
use crate::engine::EngineError;
use crate::perf_kit::{Row, SAMPLES, SAMPLES_MIN, line_ns, time};
use crate::protocol::{ClientId, Cursor, Event, Outcome, RequestId};
use crate::storage::migrations::{
    BACKUP_FAILURE_FILE, Step, backup, backup_failed, backup_failure, backup_time, check, prune,
    recorded, restore, schema, stamp, upgrade,
};
use crate::storage::{events, persist_event_ids};
use rusqlite::{Connection, OpenFlags, Transaction};
use std::hint::black_box;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const BATCH: usize = 1000;
const READ_BATCH: usize = 10;
const WRITE_BATCH: usize = 1;
const HEAVY_SAMPLES: usize = 40;
const BACKUP_NAME: &str = "9-to-10-20261124T082500123Z";
const STAMP: &str = "20261124T082500123Z";
const AT_MS: u64 = 1_795_508_700_123;
const BACKUP_FAILED_CODE: &str = "storage_backup_failed";
const OLDER: [&str; 4] = ["7-to-8", "8-to-9", "9-to-10", "10-to-11"];
const PINNED_VERSIONS: &str = "11-to-12";
const LOGGED: u64 = 1001;
const EVENTS_KEPT: u64 = 128;
const CATCH_UP: u64 = 100;
const REQUEST: &str = "perf-request-0001";
const CLIENT: &str = "perf-client";

/// The write the checkpoint row commits before each call, outside the timed span, so the
/// WAL holds one changed page as after an owner delete. The 2.5 row runs the same text.
const WAL_WRITE: &str =
    "UPDATE events SET body = body WHERE cursor = (SELECT MAX(cursor) FROM events)";

/// The sized engine.db both builds time: 4,096 rows of 256 bytes, about 1.1 MiB. The same
/// bytes are in the 2.5 perf file.
const FIXTURE_SQL: &str = "CREATE TABLE fixture (id INTEGER PRIMARY KEY, body TEXT NOT NULL);
WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 4096)
INSERT INTO fixture SELECT i, printf('%0256d', i) FROM n;";
const FIXTURE_ROWS: i64 = 4096;
const NEWEST: i64 = 12;
const PINNED_FROM: i64 = 11;

/// One storage row at layer 1, under the 2.5 call's name. Inputs: the dotted call name and
/// the calls each sample batches. Output: the row.
fn row(name: &'static str, batch: usize) -> Row {
    Row {
        layer: 1,
        module: "storage",
        name,
        batch,
    }
}

/// The time the rows format: 2026-11-24 08:25:00.123 UTC. Output: the time.
fn at() -> SystemTime {
    UNIX_EPOCH + Duration::from_millis(AT_MS)
}

/// The nanoseconds since a start. Input: the start. Output: the span.
fn nanos(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_nanos()).expect("span fits u64")
}

/// Times a call whose fixture each sample sets up again: `one` sets it up, times one call
/// and returns that span, a tenth of the samples running first as warm-up. Prints the PERF
/// line in nanoseconds, so a pair near 10 us prints in one unit on both builds. Inputs: the row (batch 1), the samples and the sample. Output: none.
fn sampled(row: &Row, samples: usize, mut one: impl FnMut() -> u64) {
    assert!(samples >= SAMPLES_MIN);
    assert_eq!(row.batch, 1);
    for _ in 0..samples / 10 {
        one();
    }
    let mut spans: Vec<u64> = (0..samples).map(|_| one()).collect();
    assert_eq!(spans.len(), samples);
    spans.sort_unstable();
    println!("{}", line_ns(row, &spans));
}

/// Times the 2.4.4 code that did the job of each storage call with no I/O, under the 2.5
/// call's row name, with the same inputs and batch, after checking each answer once. The
/// error row builds 2.4.4's backup error, the twin of the 2.5 restore error. Prints one
/// PERF line per call.
#[test]
#[ignore = "timing row: run through scripts/perf/run.sh"]
fn storage_pure_calls() {
    assert_eq!(backup_time(BACKUP_NAME), Some(STAMP));
    assert_eq!(stamp(at()).expect("stamp"), STAMP);
    assert!(now_ms() > AT_MS - 86_400_000 * 365);
    time(&row("migrations.backup_time", BATCH), SAMPLES, || {
        black_box(backup_time(black_box(BACKUP_NAME)));
    });
    time(&row("migrations.stamp", BATCH), SAMPLES, || {
        let _ = black_box(stamp(black_box(at())));
    });
    time(&row("restore.restore_failed", BATCH), SAMPLES, || {
        black_box(backup_failed(black_box("copy changed")));
    });
    time(&row("state_log.Clock.now_ms", BATCH), SAMPLES, || {
        black_box(now_ms());
    });
}

/// Makes a private folder. Input: the path. Output: none; a failure panics.
fn private_folder(path: &Path) {
    std::fs::create_dir(path).expect("folder");
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).expect("mode");
}

/// Writes a private file. Inputs: the path and its bytes. Output: none; a failure panics.
fn private_text(path: &Path, bytes: &[u8]) {
    std::fs::write(path, bytes).expect("file");
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).expect("mode");
}

/// The engine root the 2.5 root rows read, as 2.4.4 can hold it: a private `backups` folder
/// with four older dated backups and the newest a day old, and a backup failure record.
/// Input: the temp folder. Output: the root and the newest backup's folder.
fn engine_root(temp: &Path) -> (PathBuf, PathBuf) {
    let root = temp.join("engine");
    private_folder(&root);
    let backups = root.join("backups");
    private_folder(&backups);
    for (days, versions) in OLDER.iter().enumerate() {
        let taken = at() - Duration::from_secs(86_400 * (40 - days as u64));
        private_folder(&backups.join(format!("{versions}-{}", stamp(taken).expect("stamp"))));
    }
    let taken = at() - Duration::from_secs(86_400);
    let newest = backups.join(format!(
        "{PINNED_VERSIONS}-{}",
        stamp(taken).expect("stamp")
    ));
    private_folder(&newest);
    private_text(
        &root.join(BACKUP_FAILURE_FILE),
        BACKUP_FAILED_CODE.as_bytes(),
    );
    (root, newest)
}

/// Times the 2.4.4 code that did the job of each engine-root file call. `backups_in` is
/// stood in by `prune` with the newest backup made, so it lists, reads each time, checks
/// each folder and sorts, and deletes nothing; `write_restore_record` by `recorded` on its
/// failure arm, which replaces the failure record; `read_restore_record` by
/// `backup_failure`, the private one-line record read. Prints one PERF line per call.
#[test]
#[ignore = "timing row: run through scripts/perf/run.sh"]
fn storage_root_file_calls() {
    let temp = tempfile::tempdir().expect("temp");
    let (root, newest) = engine_root(temp.path());
    let backups = root.join("backups");
    let path = root.join("engine.db");
    assert_eq!(backup_failure(&root), Some(BACKUP_FAILED_CODE));
    prune(&backups, &newest).expect("prune");
    assert_eq!(
        std::fs::read_dir(&backups).expect("list").count(),
        OLDER.len() + 1
    );
    let _ = recorded(&path, Err(backup_failed("copy changed")));
    assert_eq!(backup_failure(&root), Some(BACKUP_FAILED_CODE));

    time(
        &row("migrations.backup_failure", READ_BATCH),
        SAMPLES,
        || {
            black_box(backup_failure(black_box(&root)));
        },
    );
    time(&row("migrations.backups_in", READ_BATCH), SAMPLES, || {
        let _ = black_box(prune(black_box(&backups), black_box(&newest)));
    });
    time(
        &row("restore.write_restore_record", WRITE_BATCH),
        SAMPLES,
        || {
            let _ = black_box(recorded(
                black_box(&path),
                Err(backup_failed("copy changed")),
            ));
        },
    );
    time(
        &row("restore.read_restore_record", READ_BATCH),
        SAMPLES,
        || {
            black_box(backup_failure(black_box(&root)));
        },
    );
}

/// One 2.4.4 event body. Input: its cursor. Output: the JSON body.
fn event_body(cursor: u64) -> String {
    serde_json::to_string(&Event {
        cursor: Cursor::new(cursor),
        request_id: RequestId::new(REQUEST).expect("request"),
        client_id: ClientId::new(CLIENT).expect("client"),
    })
    .expect("event")
}

/// Opens the store 2.4.4 kept the watch's events in, at the revision the 2.5 store reaches:
/// WAL, `state` and `events` as the engine makes them with [`EVENTS_KEPT`] events, and an
/// empty settings publication. Input: the folder. Output: the connection.
fn store(folder: &Path) -> Connection {
    let db = Connection::open(folder.join("engine.db")).expect("store");
    db.pragma_update(None, "journal_mode", "WAL").expect("wal");
    db.execute_batch(
        "CREATE TABLE state (id INTEGER PRIMARY KEY CHECK(id=1), revision INTEGER NOT NULL);
         CREATE TABLE events (cursor INTEGER PRIMARY KEY, body TEXT NOT NULL);
         CREATE TABLE settings_publication (singleton INTEGER PRIMARY KEY CHECK(singleton=1), body BLOB NOT NULL);",
    )
    .expect("schema");
    db.execute("INSERT INTO state VALUES (1, ?1)", [LOGGED as i64])
        .expect("state");
    for cursor in LOGGED - EVENTS_KEPT + 1..=LOGGED {
        db.execute(
            "INSERT INTO events VALUES (?1, ?2)",
            rusqlite::params![cursor as i64, event_body(cursor)],
        )
        .expect("event");
    }
    db
}

/// Times one call per sample in a transaction rolled back after it, so every sample starts
/// from the same store: the begin and the rollback run outside the timed span. Inputs: the
/// row (batch 1), the connection and the call. Output: none; prints the PERF line.
fn time_tx(row: &Row, db: &mut Connection, mut call: impl FnMut(&Transaction<'_>)) {
    sampled(row, SAMPLES, || {
        let tx = db.transaction().expect("tx");
        let started = Instant::now();
        call(&tx);
        let spent = nanos(started);
        tx.rollback().expect("rollback");
        spent
    });
}

/// Times the 2.4.4 code that did the job of each store call: the catch-up events read, the
/// event write with its revision and prune, and the WAL checkpoint after an owner delete,
/// which commits one small write first outside the timed span. Prints one PERF line per
/// call.
#[test]
#[ignore = "timing row: run through scripts/perf/run.sh"]
fn storage_store_calls() {
    let temp = tempfile::tempdir().expect("temp");
    let mut db = store(temp.path());
    let config = resolve_config_bytes("", "").expect("config");
    assert_eq!(config.settings.events_retained as u64, EVENTS_KEPT);
    let request = RequestId::new(REQUEST).expect("request");
    let client = ClientId::new(CLIENT).expect("client");
    let now = LOGGED;
    {
        let tx = db.transaction().expect("tx");
        match events(&tx, now - CATCH_UP, now).expect("events") {
            Outcome::Events { events } => assert_eq!(events.len() as u64, CATCH_UP),
            _ => panic!("events"),
        }
        persist_event_ids(&tx, &config, &request, &client, now + 1).expect("persist");
        tx.rollback().expect("rollback");
    }
    {
        let tx = db.transaction().expect("tx");
        time(&row("events.logged", READ_BATCH), SAMPLES, || {
            let _ = black_box(events(black_box(&tx), now - CATCH_UP, now));
        });
        tx.rollback().expect("rollback");
    }
    time_tx(&row("events.persist", WRITE_BATCH), &mut db, |tx| {
        persist_event_ids(tx, &config, &request, &client, now + 1).expect("persist");
    });
    sampled(
        &row("state_log.replay.checkpoint", WRITE_BATCH),
        SAMPLES,
        || {
            db.execute(WAL_WRITE, []).expect("wal write");
            let started = Instant::now();
            db.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))
                .expect("checkpoint");
            nanos(started)
        },
    );
}

/// A step that changes nothing, so the upgrade rows time the upgrade itself. Input: the
/// transaction. Output: Ok.
fn no_change(_: &Transaction<'_>) -> Result<(), EngineError> {
    Ok(())
}

/// Steps 2 to [`NEWEST`], each changing nothing. Output: the steps.
fn steps() -> Vec<Step> {
    (2..=NEWEST).map(|to| Step { to, run: no_change }).collect()
}

/// An engine root in a canonical private temp folder holding the sized engine.db at a
/// version. Input: the version. Output: the folder guard, the root and the database path.
fn sized(at: i64) -> (tempfile::TempDir, PathBuf, PathBuf) {
    let temp = tempfile::Builder::new()
        .tempdir_in(std::fs::canonicalize("/tmp").expect("tmp"))
        .expect("temp");
    let root = temp.path().join("engine");
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&root)
        .expect("root");
    let path = root.join("engine.db");
    let db = Connection::open(&path).expect("db");
    db.execute_batch(FIXTURE_SQL).expect("fixture");
    db.pragma_update(None, "user_version", at).expect("version");
    drop(db);
    (temp, root, path)
}

/// Removes every backup in a backups folder and keeps the folder, as an engine root holds it
/// between upgrades, outside the timed span. Input: the folder. Output: none.
fn clear_backups(folder: &Path) {
    let Ok(entries) = std::fs::read_dir(folder) else {
        return;
    };
    for entry in entries {
        std::fs::remove_dir_all(entry.expect("entry").path()).expect("remove");
    }
}

/// Copies a folder tree, keeping each file's mode and making each folder 0700. Inputs: the
/// source and the target, which must not exist. Output: none; a failure panics.
fn copy_tree(from: &Path, to: &Path) {
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(to)
        .expect("folder");
    for entry in std::fs::read_dir(from).expect("list") {
        let entry = entry.expect("entry");
        let target = to.join(entry.file_name());
        if entry.file_type().expect("type").is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).expect("copy");
        }
    }
}

/// Puts an engine root back to a template's state outside the timed span: removes it and
/// copies the template over. Inputs: the template and the root. Output: none.
fn reset(template: &Path, root: &Path) {
    std::fs::remove_dir_all(root).expect("remove");
    copy_tree(template, root);
}

/// Times the 2.4.4 code that did the job of each sized engine.db call, over the same
/// 4,096 rows of 256 bytes, about 1.1 MiB, with no settings sources, as the 2.5 rows keep
/// no settings texts. `restore_pinned` and `put_back` are stood in by 2.4.4's online-backup
/// `restore` of a backup over engine.db, opening engine.db inside the timed span as the 2.5
/// calls do; 2.4.4 had no pin, record, lock or copy check. Rows that change the root reset
/// it from a template before each call outside the span. Prints one PERF line per call.
#[test]
#[ignore = "timing row: run through scripts/perf/run.sh"]
fn storage_sized_database_calls() {
    let steps = steps();

    let (_backup_temp, backup_root, backup_path) = sized(NEWEST);
    let backup_db = Connection::open(&backup_path).expect("db");
    let rows: i64 = backup_db
        .query_row("SELECT COUNT(*) FROM fixture", [], |row| row.get(0))
        .expect("rows");
    assert_eq!(rows, FIXTURE_ROWS);
    backup(&backup_db, &backup_path, None, NEWEST, NEWEST + 1).expect("backup");
    sampled(&row("migrations.backup", 1), HEAVY_SAMPLES, || {
        clear_backups(&backup_root.join("backups"));
        let started = Instant::now();
        black_box(backup(&backup_db, &backup_path, None, NEWEST, NEWEST + 1).expect("backup"));
        nanos(started)
    });
    drop(backup_db);

    let read_only = Connection::open_with_flags(&backup_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .expect("read only");
    check(&read_only, &steps).expect("check");
    time(&row("upgrade.check", READ_BATCH), SAMPLES, || {
        check(black_box(&read_only), black_box(&steps)).expect("check");
    });
    drop(read_only);

    let (_upgrade_temp, upgrade_root, upgrade_path) = sized(NEWEST);
    let mut upgrade_db = Connection::open(&upgrade_path).expect("db");
    time(&row("upgrade.upgrade.newest", READ_BATCH), SAMPLES, || {
        upgrade(&mut upgrade_db, &upgrade_path, None, &steps).expect("newest");
    });
    sampled(&row("upgrade.upgrade.one_step", 1), HEAVY_SAMPLES, || {
        upgrade_db
            .pragma_update(None, "user_version", NEWEST - 1)
            .expect("version");
        clear_backups(&upgrade_root.join("backups"));
        let started = Instant::now();
        upgrade(&mut upgrade_db, &upgrade_path, None, &steps).expect("one step");
        nanos(started)
    });
    let found: i64 = upgrade_db
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .expect("version");
    assert_eq!(found, NEWEST);
    drop(upgrade_db);

    let (_restore_temp, template, template_path) = sized(PINNED_FROM);
    let template_db = Connection::open(&template_path).expect("db");
    let pinned = backup(&template_db, &template_path, None, PINNED_FROM, NEWEST).expect("backup");
    let pinned_schema = schema(&template_db).expect("schema");
    template_db
        .pragma_update(None, "user_version", NEWEST)
        .expect("version");
    drop(template_db);
    let root = template.with_file_name("sample");
    copy_tree(&template, &root);
    let path = root.join("engine.db");
    let source = root.join("backups").join(pinned.file_name().expect("name"));
    for name in ["restore.restore_pinned", "restore.recovery.put_back"] {
        sampled(&row(name, 1), HEAVY_SAMPLES, || {
            reset(&template, &root);
            let started = Instant::now();
            let mut db = Connection::open(&path).expect("db");
            restore(&mut db, &source, PINNED_FROM, &pinned_schema).expect("restore");
            drop(db);
            nanos(started)
        });
    }
    let restored = Connection::open(&path).expect("db");
    let found: i64 = restored
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .expect("version");
    assert_eq!(found, PINNED_FROM);
}
