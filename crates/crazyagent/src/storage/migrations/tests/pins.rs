use super::*;
use kernel_storage::migrations::{backup_for_test, prune_for_test};
use std::process::Command;

/// The variable that turns a run of the crash row into the child process that crashes.
const CRASH_ROOT: &str = "CRAZYAGENT_L4_CRASH_ROOT";

/// The exit code the crash step ends its process with, so the parent tells it from a failure.
const CRASH_EXIT: i32 = 86;

/// The file the crash step writes beside engine.db before it ends the process.
const REACHED: &str = "reached";

/// Header bytes SQLite rewrites on every backup-API copy: the change counter, which every
/// page 1 write sets to the last one read plus one, with version-valid-for and the library
/// version beside it (sqlite3.c:62689-62701), and the schema cookie, which a backup sets to
/// the destination's plus one (sqlite3.c:85131-85142). Every other byte matches its source.
const REWRITTEN: [std::ops::Range<usize>; 3] = [24..28, 40..44, 92..100];

/// One day.
const DAY: Duration = Duration::from_secs(86_400);

/// One hour.
const HOUR: Duration = Duration::from_secs(3_600);

/// Copies the committed P5 fixture, the engine.db a 2.4.2 engine leaves with settings, into a
/// fresh engine root. Its rows name /private/tmp folders that no longer exist, as P5 says.
/// Output: the root and the fixture's bytes.
fn fixture() -> (Root, Vec<u8>) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/v242-engine.db");
    let bytes = std::fs::read(&path).unwrap_or_else(|cause| {
        panic!(
            "the P5 fixture {} is missing ({cause}): capture it first with \
             SNAPSHOT_V242_WRITE=1 on the 2.4.2 release commit",
            path.display()
        )
    });
    let root = Root::new();
    std::fs::write(root.database(), &bytes).unwrap();
    std::fs::set_permissions(root.database(), std::fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(version(&root.connection()).unwrap(), V24_VERSION_MAX);
    assert_eq!(std::fs::read(root.database()).unwrap(), bytes);
    (root, bytes)
}

/// The settings sources of a root. Input: the root. Output: its sources.
fn sources(root: &Root) -> SettingsSources {
    SettingsSources::open(&root.global(), &root.project()).unwrap()
}

/// A later 2.5 step that changes nothing. Input: the transaction. Output: Ok.
fn kept(_tx: &Transaction<'_>) -> Result<(), StorageError> {
    Ok(())
}

/// The backup folder names of a root, oldest first. Input: the root. Output: the names.
fn names(root: &Root) -> Vec<String> {
    root.backups()
        .iter()
        .map(|path| path.file_name().unwrap().to_str().unwrap().to_owned())
        .collect()
}

/// The name of the backup the rotation leaves alone at a time. Inputs: the root and the
/// time. Output: the pinned folder's name, or None.
fn pinned_name(root: &Root, now: SystemTime) -> Option<String> {
    let folder = root.path().join(BACKUPS);
    let found = backups_in(&folder).unwrap();
    pinned(&folder, &found, now).map(|index| found[index].1.clone())
}

/// One big-endian 4-byte field of a database header, as the SQLite file format stores it.
/// Inputs: the file's bytes and the field's offset. Output: the value.
fn header(bytes: &[u8], at: usize) -> u32 {
    u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap())
}

/// The offsets where two database files differ outside the header bytes SQLite rewrites.
/// Inputs: both files' bytes, of one length. Output: the offsets, empty when they match.
fn differences(left: &[u8], right: &[u8]) -> Vec<usize> {
    assert_eq!(left.len(), right.len());
    left.iter()
        .zip(right)
        .enumerate()
        .filter(|(offset, (one, other))| {
            one != other && !REWRITTEN.iter().any(|field| field.contains(offset))
        })
        .map(|(offset, _)| offset)
        .collect()
}

/// Whether engine.db holds the probe step's table. Input: the connection. Output: the answer.
fn probed(db: &Connection) -> bool {
    db.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name = 'migration_probe')",
        [],
        |row| row.get(0),
    )
    .unwrap()
}

/// A copy check that drops one project row from the copy first, as a write that slipped in
/// between the copy and its check would look. Inputs: the live connection and the copy.
/// Output: the real check's answer.
fn tampered(db: &Connection, copy: &Path) -> Result<(), StorageError> {
    let opened = Connection::open(copy).unwrap();
    let dropped = opened
        .execute(
            "DELETE FROM projects WHERE rowid = (SELECT MIN(rowid) FROM projects)",
            [],
        )
        .unwrap();
    assert_eq!(dropped, 1, "the P5 fixture holds its project");
    drop(opened);
    check_copy(db, copy)
}

/// The kernel backup with the [`tampered`] check in place of the real one. Inputs and output
/// as `kernel_storage::migrations::backup`.
fn tampered_backup(
    db: &Connection,
    path: &Path,
    texts: Texts<'_>,
    versions: (i64, i64),
    now: SystemTime,
) -> Result<PathBuf, StorageError> {
    backup_for_test(db, path, texts, versions, now, tampered)
}

/// The kernel backup with the [`corrupted`] check in place of the real one. Inputs and output
/// as `kernel_storage::migrations::backup`.
fn corrupted_backup(
    db: &Connection,
    path: &Path,
    texts: Texts<'_>,
    versions: (i64, i64),
    now: SystemTime,
) -> Result<PathBuf, StorageError> {
    backup_for_test(db, path, texts, versions, now, corrupted)
}

/// A copy check that overwrites the projects table's root page in the copy first, as a disk
/// that lost the write would look. Inputs: the live connection and the copy. Output: the
/// real check's answer.
fn corrupted(db: &Connection, copy: &Path) -> Result<(), StorageError> {
    let opened = Connection::open(copy).unwrap();
    let root_page: i64 = opened
        .query_row(
            "SELECT rootpage FROM sqlite_schema WHERE type = 'table' AND name = 'projects'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let page_size: i64 = opened
        .query_row("PRAGMA page_size", [], |row| row.get(0))
        .unwrap();
    drop(opened);
    assert!(root_page > 1);
    let start = usize::try_from((root_page - 1) * page_size).unwrap();
    let end = start + usize::try_from(page_size).unwrap();
    let mut bytes = std::fs::read(copy).unwrap();
    assert!(end <= bytes.len());
    bytes[start..end].fill(0xff);
    std::fs::write(copy, &bytes).unwrap();
    check_copy(db, copy)
}

/// The child side of the crash row: upgrades the root's engine.db with the probe step and then
/// a step that ends the process while the upgrade transaction is open. Input: the root.
/// Output: never returns.
fn crash_between_steps(root: &Path) -> ! {
    let database = root.join("engine.db");
    let sources =
        SettingsSources::open(&root.join("global.toml"), &root.join("project.toml")).unwrap();
    let mut db = Connection::open(&database).unwrap();
    let steps = steps(&[probe, crash]);
    let ended = upgrade_at(
        &mut db,
        &database,
        Some(&sources),
        &steps,
        SystemTime::now(),
        backup,
    );
    panic!("the crash step must end the process; the upgrade returned {ended:?}");
}

/// The rows the P5 fixture holds after one upgrade through the probe step with no crash, the
/// end a crashed upgrade must reach once it runs again. Output: those rows.
fn uncrashed() -> BTreeMap<String, Vec<String>> {
    let (root, _) = fixture();
    let sources = sources(&root);
    let mut db = root.connection();
    upgrade_at(
        &mut db,
        &root.database(),
        Some(&sources),
        &steps(&[probe]),
        SystemTime::now(),
        backup,
    )
    .unwrap();
    assert_eq!(version(&db).unwrap(), latest(STEPS) + 1);
    stable_rows(&db)
}

/// The rows of an upgraded database, shaped as `rows` shapes them, with the bytes two runs of
/// one upgrade write differently named in place and nothing else changed: the kept text key,
/// each keyed kept text digest (named by the rank of the text it hashes), the state log
/// `at_ms` times, and each SnapshotDone sha256, which hashes chunks holding those digests and
/// is checked against its chunks first. Input: the connection. Output: the rows.
fn stable_rows(db: &Connection) -> BTreeMap<String, Vec<String>> {
    let done: Vec<i64> = db
        .prepare("SELECT seq FROM state_log WHERE kind = 'SnapshotDone'")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert!(!done.is_empty(), "the upgrade puts snapshots on the log");
    for seq in done {
        kernel_storage::state_log::verify_snapshot(db, u64::try_from(seq).unwrap()).unwrap();
    }
    let mut kept: Vec<(String, Vec<u8>)> = db
        .prepare("SELECT digest, text FROM kept_texts")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert!(!kept.is_empty(), "the upgrade keeps texts");
    kept.sort_by(|left, right| left.1.cmp(&right.1));
    let named = |text: &str| {
        let mut text = text.to_owned();
        for (rank, (digest, _)) in kept.iter().enumerate() {
            assert_eq!(digest.len(), 64);
            text = text.replace(digest.as_str(), &format!("<digest of kept text {rank}>"));
        }
        text
    };
    let tables: Vec<String> = rows(db).into_keys().collect();
    tables
        .into_iter()
        .map(|table| {
            let mut statement = db.prepare(&format!("SELECT * FROM \"{table}\"")).unwrap();
            let columns: Vec<String> = statement
                .column_names()
                .into_iter()
                .map(str::to_owned)
                .collect();
            let mut values: Vec<String> = statement
                .query_map([], |row| {
                    let kind = columns
                        .iter()
                        .position(|column| table == "state_log" && column == "kind")
                        .map(|index| row.get::<_, String>(index))
                        .transpose()?;
                    let mut cells = Vec::with_capacity(columns.len());
                    for (index, column) in columns.iter().enumerate() {
                        let value = row.get::<_, rusqlite::types::Value>(index)?;
                        cells.push(stable_cell(&table, column, kind.as_deref(), value, &named));
                    }
                    Ok(cells.join("|"))
                })
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap();
            values.sort();
            (table, values)
        })
        .collect()
}

/// One cell of [`stable_rows`]. Inputs: the table, the column, the state log row's kind,
/// the value and the digest namer. Output: the value as `rows` prints it, or the run-random
/// part named: the kept text key, a state log time, a digest, or the sha256 of a SnapshotDone
/// body, which [`stable_rows`] checked against its chunks; no other body's bytes are named.
fn stable_cell(
    table: &str,
    column: &str,
    kind: Option<&str>,
    value: rusqlite::types::Value,
    named: &dyn Fn(&str) -> String,
) -> String {
    use rusqlite::types::Value;
    match (table, column, value) {
        ("kept_text_key", _, Value::Blob(_)) => "Blob(<kept text key>)".to_owned(),
        ("state_log", "at_ms", Value::Integer(_)) => "Integer(<at_ms>)".to_owned(),
        ("state_log", "body", Value::Blob(body)) => {
            let body = named(std::str::from_utf8(&body).unwrap());
            if kind == Some("SnapshotDone") {
                format!("Body({})", checked_sha256(&body))
            } else {
                format!("Body({body})")
            }
        }
        (_, _, Value::Text(text)) => format!("{:?}", Value::Text(named(&text))),
        (_, _, value) => format!("{value:?}"),
    }
}

/// Names the sha256 of a SnapshotDone body, which [`stable_rows`] checked against its chunks.
/// Input: a state log body. Output: the body with each `"sha256":"<64 hex>"` value named.
fn checked_sha256(body: &str) -> String {
    const KEY: &str = "\"sha256\":\"";
    let mut out = String::with_capacity(body.len());
    let mut rest = body;
    while let Some(at) = rest.find(KEY) {
        let (head, tail) = rest.split_at(at + KEY.len());
        let hex = tail.get(..64).unwrap();
        assert!(hex.bytes().all(|byte| byte.is_ascii_hexdigit()), "{body}");
        out.push_str(head);
        out.push_str("<checked sha256>");
        rest = &tail[64..];
    }
    out.push_str(rest);
    out
}

/// A step that ends the process mid-upgrade as a crash would: it marks that it ran, then exits
/// with no rollback and no destructor. Input: the open upgrade transaction. Output: never
/// returns.
fn crash(tx: &Transaction<'_>) -> Result<(), StorageError> {
    let database = Path::new(tx.path().expect("the upgrade runs on a file"));
    std::fs::write(database.with_file_name(REACHED), b"").unwrap();
    std::process::exit(CRASH_EXIT);
}

#[test]
fn the_first_upgrade_of_a_2_4_database_checks_and_pins_its_backup() {
    let (root, _) = fixture();
    let sources = sources(&root);
    let mut db = root.connection();
    let now = SystemTime::now();

    upgrade_at(
        &mut db,
        &root.database(),
        Some(&sources),
        &steps(&[probe]),
        now,
        backup,
    )
    .unwrap();

    let first = latest(STEPS) + 1;
    assert_eq!(version(&db).unwrap(), first);
    let names = names(&root);
    assert_eq!(names.len(), 1);
    let prefix = format!("{V24_VERSION_MAX}-to-{first}-");
    assert!(names[0].starts_with(&prefix), "{names:?}");
    let backup = &root.backups()[0];
    assert_eq!(mode(&backup.join(CHECKED)), 0o600);
    assert_eq!(pinned_name(&root, now), Some(names[0].clone()));
    assert_eq!(
        pinned_name(&root, now + DAY * (PINNED_DAYS - 1)),
        Some(names[0].clone())
    );
    assert_eq!(pinned_name(&root, now + DAY * PINNED_DAYS), None);
    let copy = Connection::open(backup.join(DATABASE)).unwrap();
    assert_eq!(version(&copy).unwrap(), V24_VERSION_MAX);
    assert!(!probed(&copy));
    drop(copy);
    assert_eq!(backup_failure(root.path()), None);
}

#[test]
fn a_failing_step_leaves_the_p5_fixture_byte_identical() {
    let (root, bytes) = fixture();
    let sources = sources(&root);
    let mut db = root.connection();

    let failed = upgrade_at(
        &mut db,
        &root.database(),
        Some(&sources),
        &steps(&[probe, broken]),
        SystemTime::now(),
        backup,
    )
    .unwrap_err()
    .0;

    assert!(failed.contains("the database is unchanged"), "{failed}");
    assert_eq!(version(&db).unwrap(), V24_VERSION_MAX);
    assert!(!probed(&db));
    drop(db);
    assert_eq!(std::fs::read(root.database()).unwrap(), bytes);
    assert_eq!(names(&root).len(), 1);
}

#[test]
fn a_step_that_ends_the_transaction_restores_the_p5_fixture_byte_identical() {
    let (root, bytes) = fixture();
    let before = rows(&root.connection());
    let sources = sources(&root);
    let mut db = root.connection();

    let failed = upgrade_at(
        &mut db,
        &root.database(),
        Some(&sources),
        &steps(&[probe, escaping]),
        SystemTime::now(),
        backup,
    )
    .unwrap_err()
    .0;

    assert!(
        failed.contains("the database was restored from the backup"),
        "{failed}"
    );
    assert_eq!(version(&db).unwrap(), V24_VERSION_MAX);
    assert_eq!(rows(&db), before);
    drop(db);
    let restored = std::fs::read(root.database()).unwrap();
    assert_eq!(differences(&restored, &bytes), Vec::<usize>::new());
    let counter = header(&restored, 24);
    assert!(counter > header(&bytes, 24), "the change counter moves on");
    assert_eq!(header(&restored, 92), counter, "version-valid-for names it");
    let library = u32::try_from(rusqlite::version_number()).unwrap();
    assert_eq!(header(&restored, 96), library);
    let cookie = header(&restored, 40);
    assert!(cookie >= header(&bytes, 40) + 2, "the cookie moves on");
    let read: i64 = root
        .connection()
        .query_row("PRAGMA schema_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(read, i64::from(cookie));
}

#[test]
fn the_pinned_backup_survives_six_later_upgrades() {
    let (root, _) = fixture();
    let sources = sources(&root);
    let mut db = root.connection();
    let start = SystemTime::now();
    let extra: [StepRun; 7] = [probe, kept, kept, kept, kept, kept, kept];
    let first = latest(STEPS) + 1;

    for index in 0..extra.len() {
        let now = start + HOUR * u32::try_from(index).unwrap();
        upgrade_at(
            &mut db,
            &root.database(),
            Some(&sources),
            &steps(&extra[..=index]),
            now,
            backup,
        )
        .unwrap();
        let reached = first + i64::try_from(index).unwrap();
        assert_eq!(version(&db).unwrap(), reached);
    }

    let names = names(&root);
    assert_eq!(names.len(), BACKUPS_KEPT + 1, "{names:?}");
    let pin = format!("{V24_VERSION_MAX}-to-{first}-");
    assert!(names[0].starts_with(&pin), "{names:?}");
    let oldest_later = format!("{}-to-{}-", first + 1, first + 2);
    assert!(names[1].starts_with(&oldest_later), "{names:?}");
    assert_eq!(pinned_name(&root, start + HOUR * 6), Some(names[0].clone()));
    for name in &names {
        let checked = root.path().join(BACKUPS).join(name).join(CHECKED);
        assert!(checked.is_file(), "{name}");
    }
}

#[test]
fn a_pinned_backup_joins_the_rotation_after_thirty_days() {
    let root = Root::new();
    let folder = root.path().join(BACKUPS);
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&folder)
        .unwrap();
    let start = UNIX_EPOCH + Duration::from_secs(1_800_000_000);
    let made = |from: i64, at: SystemTime| {
        let name = format!("{from}-to-{}-{}", from + 1, stamp(at).unwrap());
        std::fs::create_dir(folder.join(&name)).unwrap();
        std::fs::write(folder.join(&name).join(CHECKED), b"").unwrap();
        name
    };
    let pinned = made(V24_VERSION_MAX, start);
    let later: Vec<String> = (1..=u32::try_from(BACKUPS_KEPT).unwrap())
        .map(|hour| made(V24_VERSION_MAX + 1, start + HOUR * hour))
        .collect();

    let newest = folder.join(later.last().unwrap());
    prune_for_test(&folder, &newest, start + DAY * (PINNED_DAYS - 1)).unwrap();
    assert!(folder.join(&pinned).is_dir());
    assert_eq!(names(&root).len(), BACKUPS_KEPT + 1);

    prune_for_test(&folder, &newest, start + DAY * PINNED_DAYS).unwrap();
    assert!(!folder.join(&pinned).exists());
    assert_eq!(names(&root), later);
}

#[test]
fn only_the_newest_checked_backup_a_2_5_upgrade_took_from_a_2_4_database_is_pinned() {
    let root = Root::new();
    let folder = root.path().join(BACKUPS);
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&folder)
        .unwrap();
    let start = UNIX_EPOCH + Duration::from_secs(1_800_000_000);
    let made = |from: i64, to: i64, hour: u32, checked: bool| {
        let name = format!("{from}-to-{to}-{}", stamp(start + HOUR * hour).unwrap());
        std::fs::create_dir(folder.join(&name)).unwrap();
        if checked {
            std::fs::write(folder.join(&name).join(CHECKED), b"").unwrap();
        }
        name
    };
    let v24 = V24_VERSION_MAX;
    made(v24, v24 + 1, 0, false);
    made(v24 - 1, v24, 1, true);
    made(v24, v24 + 1, 2, true);
    let newest = made(v24, v24 + 1, 3, true);
    made(v24 + 1, v24 + 2, 4, true);
    let not_a_file = made(v24, v24 + 1, 5, false);
    std::fs::create_dir(folder.join(&not_a_file).join(CHECKED)).unwrap();

    assert_eq!(pinned_name(&root, start + HOUR * 6), Some(newest.clone()));
    assert_eq!(
        pinned_name(&root, start + DAY * PINNED_DAYS + HOUR * 2),
        Some(newest)
    );
    assert_eq!(
        pinned_name(&root, start + DAY * PINNED_DAYS + HOUR * 3),
        None
    );
}

#[test]
fn a_count_mismatch_stops_the_upgrade_before_any_step() {
    let (root, bytes) = fixture();
    let sources = sources(&root);
    let mut db = root.connection();

    let failed = upgrade_at(
        &mut db,
        &root.database(),
        Some(&sources),
        &steps(&[probe]),
        SystemTime::now(),
        tampered_backup,
    )
    .unwrap_err()
    .0;

    assert!(
        failed.starts_with("storage_backup_failed: the backup copy holds "),
        "{failed}"
    );
    assert!(
        failed.contains(" rows in projects where engine.db holds "),
        "{failed}"
    );
    assert_eq!(version(&db).unwrap(), V24_VERSION_MAX);
    assert!(!probed(&db));
    assert!(names(&root).is_empty());
    assert_eq!(backup_failure(root.path()), Some(BACKUP_FAILED));
    drop(db);
    assert_eq!(std::fs::read(root.database()).unwrap(), bytes);
}

#[test]
fn a_corrupt_copy_stops_the_upgrade_before_any_step() {
    let (root, bytes) = fixture();
    let sources = sources(&root);
    let mut db = root.connection();

    let failed = upgrade_at(
        &mut db,
        &root.database(),
        Some(&sources),
        &steps(&[probe]),
        SystemTime::now(),
        corrupted_backup,
    )
    .unwrap_err()
    .0;

    assert!(
        failed.starts_with("storage_backup_failed: the backup copy failed its integrity check"),
        "{failed}"
    );
    assert_eq!(version(&db).unwrap(), V24_VERSION_MAX);
    assert!(!probed(&db));
    assert!(names(&root).is_empty());
    drop(db);
    assert_eq!(std::fs::read(root.database()).unwrap(), bytes);
}

#[test]
fn a_crash_between_steps_keeps_the_2_4_database_and_pins_the_newest_2_4_copy() {
    if let Some(crashing) = std::env::var_os(CRASH_ROOT) {
        crash_between_steps(Path::new(&crashing));
    }
    let (root, bytes) = fixture();
    let before = rows(&root.connection());
    let module = module_path!().split_once("::").unwrap().1;
    let test = format!(
        "{module}::a_crash_between_steps_keeps_the_2_4_database_and_pins_the_newest_2_4_copy"
    );

    let ran = kernel_processes::spawn_gate::collected(
        Command::new(std::env::current_exe().unwrap())
            .args([test.as_str(), "--exact", "--nocapture", "--test-threads=1"])
            .env(CRASH_ROOT, root.path()),
    )
    .unwrap();

    assert_eq!(
        ran.status.code(),
        Some(CRASH_EXIT),
        "{}",
        String::from_utf8_lossy(&ran.stderr)
    );
    assert!(root.path().join(REACHED).is_file());
    assert_eq!(std::fs::read(root.database()).unwrap(), bytes);
    let mut db = root.connection();
    assert_eq!(version(&db).unwrap(), V24_VERSION_MAX);
    assert!(!probed(&db));
    assert_eq!(rows(&db), before);
    let crashed = names(&root);
    assert_eq!(crashed.len(), 1);
    assert!(root.backups()[0].join(CHECKED).is_file());
    let crashed_copy = std::fs::read(root.backups()[0].join(DATABASE)).unwrap();

    let sources = sources(&root);
    upgrade_at(
        &mut db,
        &root.database(),
        Some(&sources),
        &steps(&[probe]),
        SystemTime::now(),
        backup,
    )
    .unwrap();

    assert_eq!(version(&db).unwrap(), latest(STEPS) + 1);
    assert_eq!(stable_rows(&db), uncrashed());
    let kept = names(&root);
    assert_eq!(kept.len(), 2);
    let rerun = kept.iter().find(|name| **name != crashed[0]).unwrap();
    assert_eq!(pinned_name(&root, SystemTime::now()).as_ref(), Some(rerun));
    let rerun_copy = std::fs::read(root.path().join(BACKUPS).join(rerun).join(DATABASE)).unwrap();
    assert_eq!(
        differences(&rerun_copy, &crashed_copy),
        Vec::<usize>::new(),
        "the pinned re-run copy holds the 2.4 bytes of the crashed copy"
    );
}

#[test]
fn a_backup_stamp_reads_back_as_its_time() {
    let seconds = [0, 951_782_400, 1_800_000_000, 253_402_300_799];
    for second in seconds {
        let time = UNIX_EPOCH + Duration::from_millis(second * 1_000 + 123);
        assert_eq!(stamp_time(&stamp(time).unwrap()), Some(time), "{second}");
    }
    let impossible = [
        "20260231T000000000Z",
        "20261301T000000000Z",
        "20260100T000000000Z",
        "20260101T240000000Z",
        "20260101T006000000Z",
        "20260101T000060000Z",
        "2026010T0000000000Z",
    ];
    for time in impossible {
        assert_eq!(stamp_time(time), None, "{time}");
    }
}

/// Writes one worktree row of each table into a 2.4 database, as 2.4.2 wrote them, before
/// any log existed. Input: the connection. Output: none.
fn worktree_rows(db: &Connection) {
    db.execute_batch(
        "INSERT INTO repository_admissions VALUES ('p5','/p5/repo','{}');
        INSERT INTO worktree_leases VALUES ('t5','/p5/repo','/p5/repo/.wt/t5','{}');",
    )
    .unwrap();
}

/// The `state` revision, the `events` rows and both worktree tables of a database.
type Standing = (
    i64,
    i64,
    Vec<Vec<rusqlite::types::Value>>,
    Vec<Vec<rusqlite::types::Value>>,
);

/// Reads a database's [`Standing`]. Input: the connection. Output: the standing.
fn standing_of(db: &Connection) -> Standing {
    let count = |sql: &str| db.query_row(sql, [], |row| row.get(0)).unwrap();
    (
        count("SELECT revision FROM state WHERE id=1"),
        count("SELECT COUNT(*) FROM events"),
        crate::state_log::replay::rows(db, "repository_admissions").unwrap(),
        crate::state_log::replay::rows(db, "worktree_leases").unwrap(),
    )
}

/// v2.5 L6 P5-upgrade row: an upgrade of the 2.4.2 fixture with a worktree row of each table
/// runs step 13 once: one verified snapshot run at one revision with its `events` row, no
/// row changed, and replay of the log equals the live tables. Its settings, memory library
/// and publish prompt rows add the runs of steps 14 (L10), 15 (L8) and 17 (L9), each at the
/// next revision.
#[test]
fn an_upgrade_of_the_p5_fixture_puts_its_worktree_rows_on_the_log() {
    let (root, _) = fixture();
    let sources = sources(&root);
    let mut db = root.connection();
    worktree_rows(&db);
    let before = standing_of(&db);

    upgrade_at(
        &mut db,
        &root.database(),
        Some(&sources),
        STEPS,
        SystemTime::now(),
        backup,
    )
    .unwrap();

    assert_eq!(version(&db).unwrap(), latest(STEPS));
    let after = standing_of(&db);
    assert_eq!((after.0, after.1), (before.0 + 4, before.1 + 4));
    assert_eq!((&after.2, &after.3), (&before.2, &before.3));
    let done: Vec<(i64, String)> = db
        .prepare("SELECT seq, aggregate FROM state_log WHERE kind=?1 ORDER BY seq")
        .unwrap()
        .query_map([kernel_storage::state_log::SNAPSHOT_DONE], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    let aggregates: Vec<&str> = done.iter().map(|(_, name)| name.as_str()).collect();
    assert_eq!(
        aggregates,
        ["worktrees", "settings", "memory", "landing"],
        "one run per step"
    );
    for (seq, _) in &done {
        kernel_storage::state_log::verify_snapshot(&db, *seq as u64).unwrap();
    }
    let tx = db.transaction().unwrap();
    crate::worktrees::log::rebuild(&tx).unwrap();
    tx.commit().unwrap();
    assert_eq!(standing_of(&db), after, "replay equals the live tables");
}

/// v2.5 L6 injected-failure row: a step that fails after step 13 ran rolls the whole upgrade
/// back, so step 13 leaves no log, no revision move, no `events` row and no changed row.
#[test]
fn a_failure_after_step_13_leaves_no_event_and_no_change() {
    let (root, _) = fixture();
    let sources = sources(&root);
    let mut db = root.connection();
    worktree_rows(&db);
    let before = standing_of(&db);

    let failed = upgrade_at(
        &mut db,
        &root.database(),
        Some(&sources),
        &steps(&[broken]),
        SystemTime::now(),
        backup,
    )
    .unwrap_err()
    .0;

    assert!(failed.contains("the database is unchanged"), "{failed}");
    assert_eq!(version(&db).unwrap(), V24_VERSION_MAX);
    let logged: bool = db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name='state_log')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(!logged, "step 13's run rolled back with the upgrade");
    assert_eq!(standing_of(&db), before);
}

mod budget;
mod restore;
mod v244_fixture;
