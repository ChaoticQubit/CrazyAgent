use super::*;
use rusqlite::Connection;
use std::collections::BTreeMap;
use std::path::Path;

static SCHEMA: Schema = Schema {
    tables: &["meta", "facts"],
    version: "SELECT version FROM meta",
    versions: &[3],
    facts: "SELECT seq, kind, body FROM facts WHERE seq > ?1 ORDER BY seq LIMIT ?2",
};

/// A temp folder reached by its real path. SQLite refuses a link in any part of a record
/// path, and the system temp folder on macOS sits below the `/var` link. Output: the folder.
fn root() -> tempfile::TempDir {
    tempfile::tempdir_in(std::fs::canonicalize(std::env::temp_dir()).unwrap()).unwrap()
}

fn database(path: &Path, version: i64) -> Connection {
    let connection = Connection::open(path).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE meta(version INTEGER);
             CREATE TABLE facts(seq INTEGER PRIMARY KEY, kind TEXT, body TEXT);",
        )
        .unwrap();
    connection
        .execute("INSERT INTO meta(version) VALUES (?1)", [version])
        .unwrap();
    connection
}

fn add(connection: &Connection, rows: &[(&str, Option<&str>)]) {
    for (kind, body) in rows {
        connection
            .execute(
                "INSERT INTO facts(kind, body) VALUES (?1, ?2)",
                rusqlite::params![kind, body],
            )
            .unwrap();
    }
}

fn turn() -> [(&'static str, Option<&'static str>); 4] {
    [
        ("turn_started", None),
        ("text", Some("hi")),
        ("tool_use", Some("grep")),
        ("turn_finished", None),
    ]
}

fn turn_events() -> Vec<TranscriptEvent> {
    vec![
        TranscriptEvent::TurnStarted,
        TranscriptEvent::Text { text: "hi".into() },
        TranscriptEvent::ToolUse {
            name: "grep".into(),
        },
        TranscriptEvent::TurnFinished { success: true },
    ]
}

fn folder(path: &Path) -> BTreeMap<String, (Vec<u8>, std::time::SystemTime)> {
    std::fs::read_dir(path)
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            let bytes = std::fs::read(entry.path()).unwrap();
            let modified = entry.metadata().unwrap().modified().unwrap();
            (
                entry.file_name().to_string_lossy().into_owned(),
                (bytes, modified),
            )
        })
        .collect()
}

/// Plan Task 15: a rollback journal database is read without one byte of it, or of its
/// folder, changing, and its rows become the turn's facts in order.
#[test]
fn a_database_is_read_without_being_written() {
    let tmp = root();
    let path = tmp.path().join("record.db");
    let writer = database(&path, 3);
    add(&writer, &turn());
    drop(writer);
    let before = folder(tmp.path());

    let mut reader = SqlitePoll::new(path.clone(), &SCHEMA, 0);
    let events = reader.read().unwrap();

    assert_eq!(events, turn_events());
    assert_eq!(reader.after(), 4);
    assert_eq!(folder(tmp.path()), before);
    assert_eq!(reader.read().unwrap(), Vec::new());
    assert_eq!(folder(tmp.path()), before);
}

/// Plan Task 15: a WAL database the harness holds open is read with its rows still in the
/// WAL file, and neither the database nor the WAL file changes; once the harness closes it,
/// a read makes no WAL or index file beside it.
#[test]
fn a_wal_database_is_read_live_and_closed_without_side_files_made() {
    let tmp = root();
    let path = tmp.path().join("record.db");
    let writer = database(&path, 3);
    writer.pragma_update(None, "journal_mode", "wal").unwrap();
    writer.pragma_update(None, "wal_autocheckpoint", 0).unwrap();
    add(&writer, &turn());
    let wal = tmp.path().join("record.db-wal");
    let database_bytes = std::fs::read(&path).unwrap();
    let wal_bytes = std::fs::read(&wal).unwrap();

    let mut reader = SqlitePoll::new(path.clone(), &SCHEMA, 0);
    let live = reader.read().unwrap();

    assert_eq!(live, turn_events());
    assert_eq!(std::fs::read(&path).unwrap(), database_bytes);
    assert_eq!(std::fs::read(&wal).unwrap(), wal_bytes);
    drop(writer);
    match std::fs::metadata(&wal) {
        Ok(kept) => assert_eq!(kept.len(), 0, "the harness's close checkpoints its WAL"),
        Err(error) => assert_eq!(error.kind(), std::io::ErrorKind::NotFound),
    }
    for kept in ["record.db-wal", "record.db-shm"] {
        if let Err(error) = std::fs::remove_file(tmp.path().join(kept)) {
            assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
        }
    }
    let before = folder(tmp.path());
    let mut closed = SqlitePoll::new(path.clone(), &SCHEMA, 0);
    assert_eq!(closed.read().unwrap(), turn_events());
    assert_eq!(folder(tmp.path()), before);
}

/// Plan Task 15: a database another process holds locked gives no facts, read after read,
/// until it frees; one that stays locked past the bound is refused, and a read that gets
/// through starts the count again.
#[test]
fn a_locked_database_is_retried_then_refused_past_the_bound() {
    let tmp = root();
    let path = tmp.path().join("record.db");
    let writer = database(&path, 3);
    add(&writer, &turn());
    let mut reader = SqlitePoll::new(path.clone(), &SCHEMA, 0);
    writer.execute_batch("BEGIN EXCLUSIVE").unwrap();

    for _ in 0..BUSY_READS_MAX {
        assert_eq!(reader.read().unwrap(), Vec::new());
    }
    assert_eq!(reader.read().unwrap_err().0, BUSY);
    let mut again = SqlitePoll::new(path.clone(), &SCHEMA, 0);
    for _ in 0..BUSY_READS_MAX - 1 {
        assert_eq!(again.read().unwrap(), Vec::new());
    }
    writer.execute_batch("COMMIT").unwrap();
    assert_eq!(again.read().unwrap(), turn_events());
    writer.execute_batch("BEGIN EXCLUSIVE").unwrap();
    for _ in 0..BUSY_READS_MAX {
        assert_eq!(again.read().unwrap(), Vec::new());
    }
    writer.execute_batch("COMMIT").unwrap();
}

/// Plan Task 15: a database whose schema version or layout no query set knows is refused,
/// and the apps get a plain sentence for it that names no code.
#[test]
fn an_unknown_schema_version_is_refused_in_plain_words() {
    let tmp = root();
    let newer = tmp.path().join("newer.db");
    add(&database(&newer, 99), &turn());
    let viewed = tmp.path().join("viewed.db");
    let connection = Connection::open(&viewed).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE meta(version INTEGER);
             INSERT INTO meta(version) VALUES (3);
             CREATE VIEW facts AS SELECT 1 AS seq, 'text' AS kind, 'x' AS body;",
        )
        .unwrap();
    drop(connection);

    let refused = SqlitePoll::new(newer, &SCHEMA, 0).read().unwrap_err();
    let viewed = SqlitePoll::new(viewed, &SCHEMA, 0).read().unwrap_err();

    assert_eq!(refused.0, VERSION_UNKNOWN);
    assert_eq!(viewed.0, VERSION_UNKNOWN);
    for code in [UNSAFE, BUSY, VERSION_UNKNOWN, UNREADABLE, TOO_LARGE] {
        let said = sentence(code).unwrap_or_else(|| panic!("{code} has no sentence"));
        assert!(said.ends_with('.') && !said.contains("record_"), "{said}");
    }
    assert_eq!(sentence("launch_harness_unsupported"), None);
}

/// Plan Task 15: a link, a folder or a file that is not a database at the record path is
/// refused, and a path with nothing there yet reads as no facts.
#[cfg(unix)]
#[test]
fn only_a_plain_database_file_at_the_record_path_is_read() {
    let tmp = root();
    let real = tmp.path().join("real.db");
    add(&database(&real, 3), &turn());
    let link = tmp.path().join("link.db");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let folder = tmp.path().join("folder.db");
    std::fs::create_dir(&folder).unwrap();
    let text = tmp.path().join("text.db");
    std::fs::write(&text, b"not a database\n").unwrap();
    let linked = tmp.path().join("linked");
    std::os::unix::fs::symlink(tmp.path(), &linked).unwrap();

    let read = |path: PathBuf| SqlitePoll::new(path, &SCHEMA, 0).read();

    assert_eq!(
        read(linked.join("real.db")).map_err(|error| error.0),
        Err(UNSAFE)
    );
    assert_eq!(read(link).unwrap_err().0, UNSAFE);
    assert_eq!(read(folder).unwrap_err().0, UNSAFE);
    assert_eq!(read(text).unwrap_err().0, UNREADABLE);
    assert_eq!(read(tmp.path().join("missing.db")).unwrap(), Vec::new());
    assert_eq!(read(real).unwrap(), turn_events());
}

/// Plan Task 15: one read takes at most its row bound and each row at most its text bound,
/// cut at a character boundary; the next read goes on from the last row taken.
#[test]
fn a_read_keeps_to_its_row_and_text_bounds() {
    let tmp = root();
    let path = tmp.path().join("record.db");
    let writer = database(&path, 3);
    let long = "é".repeat(ROW_TEXT_BYTES_MAX);
    add(&writer, &[("text", Some(long.as_str()))]);
    let rows = vec![("waiting", None); ROWS_PER_READ_MAX + 4];
    add(&writer, &rows);
    let mut reader = SqlitePoll::new(path, &SCHEMA, 0);

    let first = reader.read().unwrap();
    let second = reader.read().unwrap();

    assert_eq!(first.len(), ROWS_PER_READ_MAX);
    let TranscriptEvent::Text { text } = &first[0] else {
        panic!("{:?}", first[0]);
    };
    assert!(text.len() <= ROW_TEXT_BYTES_MAX && text.len() > ROW_TEXT_BYTES_MAX - 2);
    assert!(text.chars().all(|letter| letter == 'é'));
    assert_eq!(second, vec![TranscriptEvent::Waiting; 5]);
    assert_eq!(
        reader.after(),
        i64::try_from(ROWS_PER_READ_MAX + 5).unwrap()
    );
}

/// Plan Task 15: rows up to the position a reader starts past are never read again, and a
/// row whose fact no reader knows is passed over.
#[test]
fn a_resumed_reader_reads_only_rows_past_its_position() {
    let tmp = root();
    let path = tmp.path().join("record.db");
    let writer = database(&path, 3);
    add(&writer, &turn());
    add(
        &writer,
        &[
            ("turn_started", None),
            ("unknown_fact", Some("x")),
            ("turn_error", None),
        ],
    );

    let events = SqlitePoll::new(path, &SCHEMA, 4).read().unwrap();

    assert_eq!(
        events,
        vec![
            TranscriptEvent::TurnStarted,
            TranscriptEvent::TurnError {
                rate_limited: false
            },
        ]
    );
}

/// Review t15-sec F1: a rollback journal beside the database that is not a plain file, such
/// as a FIFO the harness made, is refused before SQLite opens anything, so a read never waits
/// on it.
#[cfg(unix)]
#[test]
fn a_journal_that_is_not_a_plain_file_is_refused_without_waiting() {
    let tmp = root();
    let path = tmp.path().join("record.db");
    add(&database(&path, 3), &turn());
    let journal = tmp.path().join("record.db-journal");
    let made = std::process::Command::new("/usr/bin/mkfifo")
        .arg(&journal)
        .status()
        .unwrap();
    assert!(made.success());
    let (sender, answer) = std::sync::mpsc::channel();
    let reading = path.clone();
    std::thread::spawn(move || {
        drop(sender.send(SqlitePoll::new(reading, &SCHEMA, 0).read()));
    });

    let read = answer.recv_timeout(Duration::from_secs(5));
    if read.is_err() {
        drop(rustix::fs::open(
            &journal,
            rustix::fs::OFlags::WRONLY | rustix::fs::OFlags::NONBLOCK,
            rustix::fs::Mode::empty(),
        ));
    }

    assert_eq!(
        read.map(|read| read.map_err(|error| error.0)),
        Ok(Err(UNSAFE))
    );
}

/// Review t15-sec F2: a table with a generated column, virtual or stored, is refused before
/// any row is read, so no expression the database names runs at a read.
#[test]
fn a_table_with_a_generated_column_is_refused_before_any_row_is_read() {
    let tmp = root();
    let read = |name: &str, column: &str| {
        let path = tmp.path().join(name);
        let connection = Connection::open(&path).unwrap();
        connection
            .execute_batch(&format!(
                "CREATE TABLE meta(version INTEGER);
                 INSERT INTO meta(version) VALUES (3);
                 CREATE TABLE facts(seq INTEGER PRIMARY KEY, kind TEXT, raw TEXT, {column});
                 INSERT INTO facts(kind, raw) VALUES ('text', 'hi');"
            ))
            .unwrap();
        drop(connection);
        SqlitePoll::new(path, &SCHEMA, 0)
            .read()
            .map_err(|error| error.0)
    };

    let virtual_body = read(
        "virtual.db",
        "body TEXT GENERATED ALWAYS AS (raw || raw) VIRTUAL",
    );
    let stored_body = read(
        "stored.db",
        "body TEXT GENERATED ALWAYS AS (upper(raw)) STORED",
    );

    assert_eq!(virtual_body, Err(VERSION_UNKNOWN));
    assert_eq!(stored_body, Err(VERSION_UNKNOWN));
}

/// Review t15-sec F2: a value longer than the value bound is refused by SQLite as it is
/// read, so one read never builds a value past the bound.
#[test]
fn a_value_past_the_value_bound_is_refused_not_built() {
    let tmp = root();
    let path = tmp.path().join("record.db");
    let writer = database(&path, 3);
    let long = "x".repeat(VALUE_BYTES_MAX + 1);
    add(&writer, &[("text", Some(long.as_str()))]);
    drop(writer);

    let read = SqlitePoll::new(path, &SCHEMA, 0).read();

    assert_eq!(read.map_err(|error| error.0), Err(UNREADABLE));
}

/// Review t15-sec F5: a virtual table under a schema table name is refused, so no table
/// module runs over tables the harness wrote.
#[test]
fn a_virtual_table_under_a_schema_name_is_refused() {
    let tmp = root();
    let path = tmp.path().join("virtual.db");
    let connection = Connection::open(&path).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE meta(version INTEGER);
             INSERT INTO meta(version) VALUES (3);
             CREATE VIRTUAL TABLE facts USING fts5(seq, kind, body);
             INSERT INTO facts(seq, kind, body) VALUES (1, 'text', 'hi');",
        )
        .unwrap();
    drop(connection);

    let read = SqlitePoll::new(path, &SCHEMA, 0).read();

    assert_eq!(read.map_err(|error| error.0), Err(VERSION_UNKNOWN));
}

/// Review t15-code F7: one read gives at most its text bound, and the row that would pass it
/// is not passed over: the next read starts with it.
#[test]
fn the_row_past_the_read_text_bound_waits_for_the_next_read() {
    let tmp = root();
    let path = tmp.path().join("record.db");
    let writer = database(&path, 3);
    let long = "x".repeat(ROW_TEXT_BYTES_MAX);
    let rows = vec![("text", Some(long.as_str())); READ_TEXT_BYTES_MAX / ROW_TEXT_BYTES_MAX + 1];
    add(&writer, &rows);
    drop(writer);
    let mut reader = SqlitePoll::new(path, &SCHEMA, 0);

    let first = reader.read().unwrap();
    let after_first = reader.after();
    let second = reader.read().unwrap();

    assert_eq!(first.len(), READ_TEXT_BYTES_MAX / ROW_TEXT_BYTES_MAX);
    assert_eq!(after_first, i64::try_from(first.len()).unwrap());
    assert_eq!(second, vec![TranscriptEvent::Text { text: long }]);
    assert_eq!(reader.after(), after_first + 1);
}
