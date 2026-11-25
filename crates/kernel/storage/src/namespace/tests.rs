use super::*;
use crate::state_log::replay::replay;
use crate::state_log::{AggregateName, FenceCheck};
use rusqlite::Connection;
use std::cell::Cell;
use std::path::{Path, PathBuf};

/// The names these rows log. Input: the stored name. Output: its name, none for another.
fn known(name: &str) -> Option<AggregateName> {
    (name == PLUGINS.name()).then_some(PLUGINS)
}

/// A fence that passes every check. Output: the hook.
fn passing() -> FenceCheck {
    Box::new(|_| Ok(()))
}

/// An in-memory store with the `state` row, the `events` table and `plugin_records`.
/// Output: the connection.
fn store() -> Connection {
    let db = Connection::open_in_memory().unwrap();
    db.execute_batch(
        "CREATE TABLE state (id INTEGER PRIMARY KEY CHECK(id=1), revision INTEGER NOT NULL);
        CREATE TABLE events (cursor INTEGER PRIMARY KEY, body TEXT NOT NULL);
        INSERT INTO state VALUES (1,0);",
    )
    .unwrap();
    crate::state_log::ensure(&db).unwrap();
    initialize(&db).unwrap();
    db
}

/// Runs one namespace write in its own own-write transaction, committed when it succeeds.
/// Inputs: the store and the write. Output: the write's result.
fn write<T>(
    db: &mut Connection,
    act: impl FnOnce(&mut Appender<'_, '_>) -> Result<T, StorageError>,
) -> Result<T, StorageError> {
    let tx = db.transaction().unwrap();
    let mut log =
        Appender::own(&tx, passing(), known, |revision| Ok(format!("r{revision}"))).unwrap();
    let done = act(&mut log);
    drop(log);
    if done.is_ok() {
        tx.commit().unwrap();
    }
    done
}

/// The namespace of one plugin id. Input: the id. Output: the namespace.
fn ns(plugin: &str) -> Namespace {
    Namespace::for_plugin(plugin).unwrap()
}

/// Puts one value in its own transaction. Inputs: the store, the namespace, the key and the
/// value. Output: the put's result.
fn put_one(
    db: &mut Connection,
    ns: &Namespace,
    key: &str,
    value: &[u8],
) -> Result<(), StorageError> {
    write(db, |log| put(log, ns, key, value, 1))
}

/// Reads one value outside any write. Inputs: the store, the namespace and the key. Output:
/// the value, or none.
fn read(db: &mut Connection, ns: &Namespace, key: &str) -> Option<Vec<u8>> {
    let tx = db.transaction().unwrap();
    get(&tx, ns, key).unwrap()
}

/// Every `plugin_records` row as plugin, key, value and put_seq, sorted. Input: the store.
/// Output: the rows.
fn records(db: &Connection) -> Vec<(String, String, Vec<u8>, i64)> {
    db.prepare("SELECT plugin, key, value, put_seq FROM plugin_records ORDER BY plugin, key")
        .unwrap()
        .query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

/// Every log row as seq and body. Input: the store. Output: the rows.
fn bodies(db: &Connection) -> Vec<(i64, Vec<u8>)> {
    db.prepare("SELECT seq, body FROM state_log ORDER BY seq")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

/// Whether any log body holds a value's bytes as a put writes them. Inputs: the store and
/// the value. Output: true when one does.
fn logged(db: &Connection, value: &[u8]) -> bool {
    let encoded = BASE64.encode(value);
    bodies(db).iter().any(|(_, body)| {
        body.windows(encoded.len())
            .any(|window| window == encoded.as_bytes())
    })
}

/// Rebuilds `plugin_records` from the log into the live store: the table is emptied, then
/// every Plugins event is applied in order. Input: the store. Output: replay's result.
fn rebuilt(db: &mut Connection) -> Result<(), StorageError> {
    let tx = db.transaction().unwrap();
    tx.execute("DELETE FROM plugin_records", []).unwrap();
    replay(&tx, PLUGINS, 0, known, |event| apply(&tx, event))?;
    tx.commit().unwrap();
    Ok(())
}

#[test]
fn a_plugin_reads_only_its_own_records() {
    let mut db = store();
    let (alpha, beta) = (ns("alpha"), ns("beta-2"));
    put_one(&mut db, &alpha, "token", b"a").unwrap();
    put_one(&mut db, &beta, "token", b"b").unwrap();
    put_one(&mut db, &beta, "other", b"c").unwrap();

    assert_eq!(read(&mut db, &alpha, "token"), Some(b"a".to_vec()));
    assert_eq!(read(&mut db, &beta, "token"), Some(b"b".to_vec()));
    assert_eq!(read(&mut db, &alpha, "other"), None);
    let tx = db.transaction().unwrap();
    assert_eq!(list(&tx, &alpha, None, 10).unwrap(), ["token"]);
    assert_eq!(list(&tx, &beta, None, 10).unwrap(), ["other", "token"]);
    assert_eq!(list(&tx, &beta, Some("other"), 10).unwrap(), ["token"]);
    assert_eq!(list(&tx, &beta, None, 1).unwrap(), ["other"]);
    drop(tx);
    assert!(write(&mut db, |log| delete(log, &alpha, "other", 1)) == Ok(false));
    assert_eq!(read(&mut db, &beta, "other"), Some(b"c".to_vec()));
}

#[test]
fn a_plugin_id_out_of_shape_is_refused() {
    let refused = Some("namespace_plugin_name".to_string());
    for plugin in ["", "Alpha", "a.b", "a/b", &"a".repeat(PLUGIN_BYTES_MAX + 1)] {
        assert_eq!(
            Namespace::for_plugin(plugin).err().map(|error| error.0),
            refused,
            "{plugin:?}"
        );
    }
    assert!(Namespace::for_plugin(&"a".repeat(PLUGIN_BYTES_MAX)).is_ok());
    assert!(Namespace::for_plugin("a_b-9").is_ok());
}

#[test]
fn put_and_delete_replay_to_the_same_rows() {
    let mut db = store();
    let (alpha, beta) = (ns("alpha"), ns("beta"));
    put_one(&mut db, &alpha, "one", b"1").unwrap();
    put_one(&mut db, &alpha, "two", b"2").unwrap();
    put_one(&mut db, &beta, "one", b"b1").unwrap();
    put_one(&mut db, &alpha, "one", b"1b").unwrap();
    assert_eq!(
        write(&mut db, |log| delete(log, &alpha, "two", 1)),
        Ok(true)
    );
    let live = records(&db);
    assert_eq!(live.len(), 2);

    rebuilt(&mut db).unwrap();

    assert_eq!(records(&db), live);
}

#[test]
fn a_value_of_exactly_value_bytes_max_puts_and_replays() {
    let mut db = store();
    let alpha = ns("alpha");
    let value = vec![0xa5; VALUE_BYTES_MAX];

    put_one(&mut db, &alpha, "big", &value).unwrap();
    let live = records(&db);
    rebuilt(&mut db).unwrap();

    assert_eq!(read(&mut db, &alpha, "big"), Some(value));
    assert_eq!(records(&db), live);
}

#[test]
fn key_and_value_bounds_refuse_before_any_write() {
    let mut db = store();
    let alpha = ns("alpha");
    let before = bodies(&db);
    let key = Some("namespace_key_bounds".to_string());
    let value = Some("namespace_value_bounds".to_string());
    let long_key = "k".repeat(KEY_BYTES_MAX + 1);
    for (name, bytes, refused) in [
        ("", &b"v"[..], &key),
        (long_key.as_str(), &b"v"[..], &key),
        ("a\nb", &b"v"[..], &key),
        ("ok", &vec![0; VALUE_BYTES_MAX + 1][..], &value),
    ] {
        assert_eq!(
            put_one(&mut db, &alpha, name, bytes)
                .err()
                .map(|error| error.0),
            *refused,
            "{name:?}"
        );
    }
    assert_eq!(
        write(&mut db, |log| delete(log, &alpha, "", 1))
            .err()
            .map(|error| error.0),
        key
    );
    assert_eq!(bodies(&db), before);
    assert!(records(&db).is_empty());
    assert!(put_one(&mut db, &alpha, &"k".repeat(KEY_BYTES_MAX), b"v").is_ok());
}

#[test]
fn the_row_cap_and_the_byte_cap_refuse_the_next_put() {
    let mut db = store();
    let rows = ns("rows");
    write(&mut db, |log| {
        for index in 0..RECORDS_PER_PLUGIN_MAX {
            put(log, &rows, &format!("k{index}"), b"v", 1)?;
        }
        Ok(())
    })
    .unwrap();
    let full = Some("namespace_full".to_string());
    assert_eq!(
        put_one(&mut db, &rows, "one_more", b"v").err().map(|e| e.0),
        full
    );
    assert!(
        put_one(&mut db, &rows, "k0", b"w").is_ok(),
        "a replace stays under the row cap"
    );

    let bytes = ns("bytes");
    let value = vec![1; VALUE_BYTES_MAX];
    let fits = BYTES_PER_PLUGIN_MAX / (VALUE_BYTES_MAX as u64 + 4);
    write(&mut db, |log| {
        for index in 0..fits {
            put(log, &bytes, &format!("{index:04}"), &value, 1)?;
        }
        Ok(())
    })
    .unwrap();
    assert_eq!(
        put_one(&mut db, &bytes, "next", &value).err().map(|e| e.0),
        full
    );
    assert!(
        put_one(&mut db, &bytes, "tiny", b"t").is_ok(),
        "the byte cap is a total"
    );
}

#[test]
fn a_put_without_a_fence_is_refused() {
    let mut db = store();
    let alpha = ns("alpha");
    let checks = std::rc::Rc::new(Cell::new(0));
    let counted = std::rc::Rc::clone(&checks);
    let fence: FenceCheck = Box::new(move |_| {
        counted.set(counted.get() + 1);
        if counted.get() == 1 {
            Ok(())
        } else {
            Err(StorageError("fence_refused".into()))
        }
    });
    let tx = db.transaction().unwrap();
    let mut log = Appender::own(&tx, fence, known, |revision| Ok(format!("r{revision}"))).unwrap();

    let refused = put(&mut log, &alpha, "key", b"value", 1);

    assert_eq!(refused, Err(StorageError("fence_refused".into())));
    drop(log);
    drop(tx);
    assert!(records(&db).is_empty());
    assert!(bodies(&db).is_empty());
    assert!(checks.get() >= 2);
}

#[test]
fn delete_leaves_no_earlier_value_in_the_log() {
    let mut db = store();
    let alpha = ns("alpha");
    put_one(&mut db, &alpha, "key", b"the-old-value").unwrap();
    assert!(logged(&db, b"the-old-value"));

    assert_eq!(
        write(&mut db, |log| delete(log, &alpha, "key", 1)),
        Ok(true)
    );

    assert!(!logged(&db, b"the-old-value"));
    assert_eq!(read(&mut db, &alpha, "key"), None);
}

#[test]
fn a_replacing_put_leaves_no_earlier_value_in_the_log() {
    let mut db = store();
    let alpha = ns("alpha");
    put_one(&mut db, &alpha, "key", b"the-old-value").unwrap();

    put_one(&mut db, &alpha, "key", b"the-new-value").unwrap();

    assert!(!logged(&db, b"the-old-value"));
    assert!(logged(&db, b"the-new-value"));
    assert_eq!(
        read(&mut db, &alpha, "key"),
        Some(b"the-new-value".to_vec())
    );
}

#[test]
fn a_put_redacts_only_the_seq_it_replaces() {
    let mut db = store();
    let alpha = ns("alpha");
    put_one(&mut db, &alpha, "kept", b"kept-value").unwrap();
    put_one(&mut db, &alpha, "key", b"first").unwrap();
    let before = bodies(&db);

    put_one(&mut db, &alpha, "key", b"second").unwrap();

    let after = bodies(&db);
    let replaced = records(&db)
        .iter()
        .find(|(_, key, _, _)| key == "key")
        .map(|(_, _, _, seq)| *seq)
        .unwrap();
    let changed: Vec<i64> = before
        .iter()
        .zip(&after)
        .filter(|(old, new)| old.1 != new.1)
        .map(|(old, _)| old.0)
        .collect();
    assert_eq!(changed.len(), 1, "one earlier body tombstoned");
    assert_eq!(
        after[(changed[0] - 1) as usize].1,
        crate::state_log::TOMBSTONE
    );
    assert!(replaced > changed[0]);
    assert!(logged(&db, b"kept-value"));
}

#[test]
fn replay_sets_put_seq_from_the_event() {
    let mut db = store();
    let alpha = ns("alpha");
    put_one(&mut db, &alpha, "one", b"1").unwrap();
    put_one(&mut db, &alpha, "two", b"2").unwrap();
    put_one(&mut db, &alpha, "one", b"3").unwrap();
    let puts: Vec<i64> = db
        .prepare("SELECT seq FROM state_log WHERE kind=?1 AND body<>?2 ORDER BY seq")
        .unwrap()
        .query_map(params![RECORD_PUT, crate::state_log::TOMBSTONE], |row| {
            row.get(0)
        })
        .unwrap()
        .map(Result::unwrap)
        .collect();
    db.execute("UPDATE plugin_records SET put_seq=999", [])
        .unwrap();

    rebuilt(&mut db).unwrap();

    let seqs: Vec<i64> = records(&db).iter().map(|(_, _, _, seq)| *seq).collect();
    assert_eq!(
        seqs,
        [puts[1], puts[0]],
        "one at its replacing put, two at its put"
    );
}

/// Every Rust source under a folder, recursively. Input: the folder. Output: the paths.
fn sources(folder: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let Ok(entries) = std::fs::read_dir(folder) else {
        return found;
    };
    for entry in entries.map(Result::unwrap) {
        let path = entry.path();
        if path.is_dir() {
            if path
                .file_name()
                .is_some_and(|name| name == "target" || name == "node_modules")
            {
                continue;
            }
            found.extend(sources(&path));
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            found.push(path);
        }
    }
    found
}

/// Whether a source file may name a plugin's namespace without the gateway's grant check.
/// Input: the path from the repo root. Output: true for the bus gateway's source and for
/// the storage crate's own test-only files that make a namespace (its namespace tests and
/// its perf rows), false for every other file.
fn exempt(path: &Path) -> bool {
    path.starts_with("crates/kernel/bus_gateway/src")
        || path == Path::new("crates/kernel/storage/src/namespace/tests.rs")
        || path == Path::new("crates/kernel/storage/src/perf_tests.rs")
        || path.starts_with("crates/kernel/storage/src/perf_tests")
}

#[test]
fn a_storage_perf_row_is_exempt_and_product_code_is_not() {
    for path in [
        "crates/kernel/storage/src/perf_tests.rs",
        "crates/kernel/storage/src/perf_tests/store.rs",
    ] {
        assert!(exempt(Path::new(path)), "{path}");
    }
    for path in [
        "crates/kernel/storage/src/namespace.rs",
        "crates/kernel/storage/src/lib.rs",
        "crates/crazyagent/src/lib.rs",
        "crates/kernel/processes/src/perf_tests/update.rs",
    ] {
        assert!(!exempt(Path::new(path)), "{path}");
    }
}

#[test]
fn namespace_is_made_only_by_the_bus_gateway() {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let lib = std::fs::read_to_string(repo.join("crates/kernel/storage/src/lib.rs")).unwrap();
    let lines: Vec<&str> = lib.lines().map(str::trim).collect();
    let at = lines
        .iter()
        .position(|line| *line == "mod perf_tests;")
        .expect("lib.rs declares perf_tests");
    assert!(
        at > 0 && lines[at - 1] == "#[cfg(test)]",
        "perf_tests stays test code"
    );
    let walked: Vec<PathBuf> = ["crates", "apps"]
        .iter()
        .flat_map(|top| sources(&repo.join(top)))
        .collect();
    assert!(walked.len() > 100, "the walk reads the workspace");
    assert!(
        walked
            .iter()
            .any(|path| path.ends_with("namespace/tests.rs"))
    );
    assert!(
        walked
            .iter()
            .any(|path| path.ends_with("storage/src/perf_tests.rs"))
    );

    let callers: Vec<&PathBuf> = walked
        .iter()
        .filter(|path| !exempt(path.strip_prefix(&repo).unwrap()))
        .filter(|path| {
            std::fs::read_to_string(path).is_ok_and(|code| code.contains("Namespace::for_plugin"))
        })
        .collect();

    assert!(callers.is_empty(), "{callers:?}");
}
