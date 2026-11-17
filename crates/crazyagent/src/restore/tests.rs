use super::*;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

/// A private engine root of its own. Output: the folder, removed on drop.
fn root() -> tempfile::TempDir {
    let dir = tempfile::Builder::new()
        .prefix("ca-restore-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    dir
}

/// Writes the restore record as a file of a mode. Inputs: the root, the text and the mode.
fn record(root: &Path, text: &str, mode: u32) {
    let path = root.join("restore-record");
    let _ = std::fs::remove_file(&path);
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(&path)
        .unwrap();
    std::io::Write::write_all(&mut file, text.as_bytes()).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
}

/// A small engine.db in a root. Input: the root. Output: its bytes.
fn database(root: &Path) -> Vec<u8> {
    let db = rusqlite::Connection::open(root.join("engine.db")).unwrap();
    db.execute_batch("CREATE TABLE kept (a); INSERT INTO kept VALUES (1);")
        .unwrap();
    drop(db);
    std::fs::read(root.join("engine.db")).unwrap()
}

#[test]
fn the_record_reads_back_as_each_state_and_nothing_else() {
    let dir = root();
    assert_eq!(state(dir.path()), None);
    let copy = "replaced-20261117T001500.000Z";
    let cases = [
        ("pending", Some(RestoreState::Pending)),
        (
            "restored replaced-20261117T001500.000Z",
            Some(RestoreState::Restored { copy: copy.into() }),
        ),
        (
            "failed storage_restore_busy",
            Some(RestoreState::Failed {
                code: "storage_restore_busy".into(),
            }),
        ),
        ("pending now", None),
        ("restored", None),
        ("failed", None),
        ("failed storage_restore_busy again", None),
        ("restored ../elsewhere", None),
        ("done storage_restore_busy", None),
        ("", None),
    ];
    for (text, expected) in cases {
        record(dir.path(), text, 0o600);
        assert_eq!(state(dir.path()), expected, "{text:?}");
    }
    record(dir.path(), "pending", 0o640);
    assert_eq!(state(dir.path()), None);
    std::fs::remove_file(dir.path().join("restore-record")).unwrap();
    let elsewhere = root();
    record(elsewhere.path(), "pending", 0o600);
    std::os::unix::fs::symlink(
        elsewhere.path().join("restore-record"),
        dir.path().join("restore-record"),
    )
    .unwrap();
    assert_eq!(state(dir.path()), None);
}

#[test]
fn a_client_starts_no_engine_while_a_restore_is_pending_or_done_and_does_after_a_failure() {
    let dir = root();
    assert_eq!(blocks_start(dir.path()), None);
    record(dir.path(), "pending", 0o600);
    assert_eq!(blocks_start(dir.path()), Some(ENGINE_RESTORING));
    record(dir.path(), "restored replaced-20261117T001500.000Z", 0o600);
    assert_eq!(blocks_start(dir.path()), Some(ENGINE_RESTORED));
    record(dir.path(), "failed storage_restore_busy", 0o600);
    assert_eq!(blocks_start(dir.path()), None);
}

#[test]
fn a_stale_pending_record_with_no_request_in_this_process_restores_nothing() {
    let dir = root();
    let before = database(dir.path());
    record(dir.path(), "pending", 0o600);
    let global = dir.path().join("global.toml");
    let project = dir.path().join("project.toml");

    assert_eq!(after_stop_when(false, dir.path(), &global, &project), Ok(()));

    assert_eq!(std::fs::read(dir.path().join("engine.db")).unwrap(), before);
    assert_eq!(state(dir.path()), Some(RestoreState::Pending));
    assert!(!dir.path().join("backups").exists());
}

#[test]
fn a_restore_that_cannot_run_records_its_code_alone_and_changes_nothing() {
    let dir = root();
    let before = database(dir.path());
    record(dir.path(), "pending", 0o600);
    let global = dir.path().join("global.toml");
    let project = dir.path().join("project.toml");

    let failed = after_stop_when(true, dir.path(), &global, &project).unwrap_err();

    assert!(failed.starts_with("storage_"), "{failed}");
    assert_eq!(std::fs::read(dir.path().join("engine.db")).unwrap(), before);
    let Some(RestoreState::Failed { code }) = state(dir.path()) else {
        panic!("the record says the restore failed");
    };
    assert!(code.starts_with("storage_restore_"), "{code}");
    assert!(!code.contains('/'), "{code}");
    assert_eq!(blocks_start(dir.path()), None);
}
