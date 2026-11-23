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
    let copy = "replaced-20261117T001500000Z";
    let cases = [
        ("pending", Some(RestoreState::Pending)),
        ("restoring", Some(RestoreState::Restoring { copy: None })),
        (
            "restoring replaced-20261117T001500000Z",
            Some(RestoreState::Restoring {
                copy: Some(copy.into()),
            }),
        ),
        (
            "failed storage_restore_rollback_failed",
            Some(RestoreState::Broken { copy: None }),
        ),
        (
            "failed storage_restore_rollback_failed replaced-20261117T001500000Z",
            Some(RestoreState::Broken {
                copy: Some(copy.into()),
            }),
        ),
        (
            "restoring replaced-2026.11.17",
            Some(RestoreState::Restoring { copy: None }),
        ),
        (
            "failed storage_restore_rollback_failed ../elsewhere",
            Some(RestoreState::Broken { copy: None }),
        ),
        (
            "failed storage_restore_busy replaced-20261117T001500000Z",
            None,
        ),
        (
            "restored replaced-20261117T001500000Z unverified_pin",
            Some(RestoreState::Restored { copy: copy.into() }),
        ),
        (
            "restoring now",
            Some(RestoreState::Restoring { copy: None }),
        ),
        ("restored replaced-20261117T001500000Z unchecked", None),
        (
            "restored replaced-20261117T001500000Z",
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
        ("restored replaced-2026.11.17", None),
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
    assert_eq!(blocks_start(dir.path(), true), None);
    assert_eq!(blocks_start(dir.path(), false), None);
    record(dir.path(), "pending", 0o600);
    assert_eq!(blocks_start(dir.path(), true), Some(ENGINE_RESTORING));
    record(dir.path(), "restored replaced-20261117T001500000Z", 0o600);
    assert_eq!(blocks_start(dir.path(), true), Some(ENGINE_RESTORED));
    assert_eq!(blocks_start(dir.path(), false), Some(ENGINE_RESTORED));
    record(dir.path(), "failed storage_restore_busy", 0o600);
    assert_eq!(blocks_start(dir.path(), true), None);
    assert_eq!(blocks_start(dir.path(), false), None);
}

/// A pending record whose engine process is gone was left by a restore that died, not one
/// that runs: holding every start on it would strand the owner, and the start that follows
/// clears it. One whose process still runs holds even the owner's explicit restart, which
/// would stop the process in the middle of the copy.
#[test]
fn a_pending_record_holds_starts_only_while_its_engine_process_still_runs() {
    let dir = root();
    record(dir.path(), "pending", 0o600);
    assert_eq!(blocks_start(dir.path(), true), Some(ENGINE_RESTORING));
    assert_eq!(blocks_start(dir.path(), false), None);
    assert!(holds_restart(dir.path(), true));
    assert!(!holds_restart(dir.path(), false));
    record(dir.path(), "restored replaced-20261117T001500000Z", 0o600);
    assert!(
        !holds_restart(dir.path(), true),
        "restart is the owner's way back to 2.5"
    );
}

#[test]
fn a_stale_pending_record_with_no_request_in_this_process_restores_nothing() {
    let dir = root();
    let before = database(dir.path());
    record(dir.path(), "pending", 0o600);
    let global = dir.path().join("global.toml");
    let project = dir.path().join("project.toml");

    assert_eq!(
        after_stop_when(false, dir.path(), &global, &project),
        Ok(())
    );

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
    assert_eq!(blocks_start(dir.path(), true), None);
}

/// A restore whose rollback failed, or one that stopped part way and whose engine process is
/// gone, holds every start for the recovery screen; a restoring record whose process still
/// runs is a restore in progress.
#[test]
fn a_broken_or_abandoned_restore_holds_every_start_for_the_recovery_screen() {
    let dir = root();
    for text in [
        "failed storage_restore_rollback_failed",
        "failed storage_restore_rollback_failed replaced-20261117T001500000Z",
    ] {
        record(dir.path(), text, 0o600);
        assert_eq!(blocks_start(dir.path(), true), Some(ENGINE_RESTORE_BROKEN));
        assert_eq!(blocks_start(dir.path(), false), Some(ENGINE_RESTORE_BROKEN));
    }
    for text in ["restoring", "restoring replaced-20261117T001500000Z"] {
        record(dir.path(), text, 0o600);
        assert_eq!(blocks_start(dir.path(), true), Some(ENGINE_RESTORING));
        assert_eq!(blocks_start(dir.path(), false), Some(ENGINE_RESTORE_BROKEN));
        assert!(holds_restart(dir.path(), true));
        assert!(!holds_restart(dir.path(), false));
    }
}

/// While the engine's login service still runs a process, no recovery action reads or writes:
/// each refuses as busy and the record and engine.db stay as they were.
#[test]
fn recovery_refuses_every_action_while_the_engine_service_runs() {
    let dir = root();
    let before = database(dir.path());
    record(
        dir.path(),
        "failed storage_restore_rollback_failed replaced-20261117T001500000Z",
        0o600,
    );
    let global = dir.path().join("global.toml");
    let project = dir.path().join("project.toml");

    let refused = [
        recovery(dir.path(), true).unwrap_err(),
        put_back(dir.path(), true).unwrap_err(),
        continue_2_5(dir.path(), true).unwrap_err(),
        finish(dir.path(), true, &global, &project).unwrap_err(),
    ];

    for code in refused {
        assert!(code.starts_with("storage_putback_busy"), "{code}");
    }
    assert_eq!(std::fs::read(dir.path().join("engine.db")).unwrap(), before);
    assert_eq!(
        state(dir.path()),
        Some(RestoreState::Broken {
            copy: Some("replaced-20261117T001500000Z".into())
        })
    );
}
