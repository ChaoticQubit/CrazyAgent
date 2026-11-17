use super::*;

#[test]
fn release_does_not_depend_on_a_stale_file_description_alias() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("db");
    let lock = lock_database(&path).unwrap();
    let stale_alias = lock.file.try_clone().unwrap();
    assert!(lock_database(&path).is_err());

    release_database_lock(lock).unwrap();
    let new_owner = lock_database(&path).unwrap();
    drop(stale_alias);
    assert!(lock_database(&path).is_err());

    release_database_lock(new_owner).unwrap();
    let final_owner = lock_database(&path).unwrap();
    release_database_lock(final_owner).unwrap();
}

#[test]
fn dropping_the_lock_frees_it_while_a_forked_alias_still_shares_its_file() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("db");
    let lock = lock_database(&path).unwrap();
    let forked_alias = lock.file.try_clone().unwrap();

    drop(lock);
    let next_owner = lock_database(&path).unwrap();
    drop(forked_alias);
    assert!(lock_database(&path).is_err());

    drop(next_owner);
    release_database_lock(lock_database(&path).unwrap()).unwrap();
}

fn journal_mode(connection: &Connection) -> String {
    connection
        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .unwrap()
}

/// SQLITE-WAL-UNCHECKED: a store that answers the WAL request by keeping a rollback journal,
/// here an anonymous temporary database, fails the engine schema with one plain code instead
/// of opening in rollback mode, where a read-only reader beside the writer gets IOERR_LOCK.
#[test]
fn the_engine_schema_refuses_a_store_that_keeps_a_rollback_journal() {
    let temporary = Connection::open("").unwrap();
    let refused = initialize_schema(&temporary);
    assert_eq!(
        refused.map_err(|error| error.0),
        Err("database_wal_refused: delete".to_owned())
    );
    assert_eq!(journal_mode(&temporary), "delete");
}

/// A store with a file name runs in write-ahead logging, and an anonymous in-memory store,
/// which no other connection can open, keeps its memory journal (DEC-147 addendum).
#[test]
fn a_file_store_runs_in_wal_and_an_anonymous_memory_store_keeps_its_memory_journal() {
    let root = tempfile::tempdir().unwrap();
    let file = Connection::open(root.path().join("engine.db")).unwrap();
    initialize_schema(&file).unwrap();
    assert_eq!(journal_mode(&file), "wal");
    let memory = Connection::open_in_memory().unwrap();
    initialize_schema(&memory).unwrap();
    assert_eq!(journal_mode(&memory), "memory");
}
