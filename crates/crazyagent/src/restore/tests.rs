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

/// Writes the restore record through the kernel's own writer, then gives it a mode; an
/// empty text, which that writer never takes, is an empty file. Inputs: the root, the text
/// and the mode.
fn record(root: &Path, text: &str, mode: u32) {
    let path = root.join("restore-record");
    let _ = std::fs::remove_file(&path);
    if text.is_empty() {
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(mode)
            .open(&path)
            .unwrap();
        drop(file);
    } else {
        kernel_storage::restore::write_restore_record(root, text).unwrap();
    }
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
    assert_eq!(restart_refusal(dir.path(), true), Some(ENGINE_RESTORING));
    assert_eq!(restart_refusal(dir.path(), false), None);
    record(dir.path(), "restored replaced-20261117T001500000Z", 0o600);
    assert_eq!(
        restart_refusal(dir.path(), true),
        None,
        "restart is the owner's way back to 2.5"
    );
}

/// The owner's explicit restart is refused in every restore state a start would break: a
/// restore still copying, and one that did not finish, whose start the engine refuses and
/// whose login service must never be started into a loop; it runs with no record, after a
/// restore that failed cleanly and after one that brought the 2.4 data back.
#[test]
fn a_restart_is_refused_in_every_restore_state_a_start_would_break() {
    let dir = root();
    assert_eq!(restart_refusal(dir.path(), true), None);
    assert_eq!(restart_refusal(dir.path(), false), None);
    let cases: [(&str, Option<&str>, Option<&str>); 7] = [
        ("pending", Some(ENGINE_RESTORING), None),
        (
            "restoring",
            Some(ENGINE_RESTORING),
            Some(ENGINE_RESTORE_BROKEN),
        ),
        (
            "restoring replaced-20261117T001500000Z",
            Some(ENGINE_RESTORING),
            Some(ENGINE_RESTORE_BROKEN),
        ),
        (
            "failed storage_restore_rollback_failed",
            Some(ENGINE_RESTORE_BROKEN),
            Some(ENGINE_RESTORE_BROKEN),
        ),
        (
            "failed storage_restore_rollback_failed replaced-20261117T001500000Z",
            Some(ENGINE_RESTORE_BROKEN),
            Some(ENGINE_RESTORE_BROKEN),
        ),
        ("failed storage_restore_busy", None, None),
        ("restored replaced-20261117T001500000Z", None, None),
    ];
    for (text, running, stopped) in cases {
        record(dir.path(), text, 0o600);
        assert_eq!(restart_refusal(dir.path(), true), running, "{text}");
        assert_eq!(restart_refusal(dir.path(), false), stopped, "{text}");
    }
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
        assert_eq!(restart_refusal(dir.path(), true), Some(ENGINE_RESTORING));
        assert_eq!(
            restart_refusal(dir.path(), false),
            Some(ENGINE_RESTORE_BROKEN)
        );
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

/// A recovery refusal reaches a client as its known code, whatever text follows it, and any
/// other text as the restore's plain failure; the backups folder sits beside engine.db.
#[test]
fn a_recovery_refusal_reaches_a_client_as_its_known_code() {
    for code in [
        "storage_putback_busy",
        "storage_putback_copy_missing",
        "storage_putback_copy_failed",
        "storage_putback_check_failed",
        "storage_restore_busy",
        "storage_restore_unavailable",
    ] {
        assert_eq!(recovery_code(code), code);
        assert_eq!(recovery_code(&format!("{code}: more")), code);
    }
    assert_eq!(recovery_code("disk I/O error"), "storage_restore_failed");
    assert_eq!(
        recovery_code("storage_putback_bogus"),
        "storage_restore_failed"
    );
    let dir = root();
    assert_eq!(backups(dir.path()), dir.path().join("backups"));
}

/// An engine root holding an empty engine.db, and a folder that links to it, as a moved
/// home or CRAZYAGENT_HOME through a link reaches it. Output: the root and the folder
/// holding the link, both removed on drop, and the root's path through the link, or the
/// error of the write or link that failed.
fn linked_root() -> std::io::Result<(tempfile::TempDir, tempfile::TempDir, std::path::PathBuf)> {
    let dir = root();
    std::fs::write(dir.path().join("engine.db"), b"")?;
    let beside = root();
    let link = beside.path().join("engine");
    std::os::unix::fs::symlink(dir.path(), &link)?;
    Ok((dir, beside, link))
}

#[test]
fn an_engine_folder_reached_through_a_link_reads_no_offer() -> std::io::Result<()> {
    let (_dir, _beside, link) = linked_root()?;

    assert_eq!(recovery(&link, false), Ok(None));
    Ok(())
}

#[test]
fn an_engine_db_that_is_itself_a_link_is_still_refused() -> std::io::Result<()> {
    let (dir, _beside, _link) = linked_root()?;
    let real = root();
    std::fs::write(real.path().join("engine.db"), b"").unwrap();
    std::fs::remove_file(dir.path().join("engine.db")).unwrap();
    std::os::unix::fs::symlink(real.path().join("engine.db"), dir.path().join("engine.db"))
        .unwrap();

    assert!(recovery(dir.path(), false).is_err());
    assert_eq!(std::fs::read(real.path().join("engine.db")).unwrap(), b"");
    Ok(())
}

/// Puts the committed 2.4.2 engine.db in a root and upgrades it to 2.5, which takes the pinned
/// 2.4 backup a restore brings back; no connection stays open. Input: the root. Output: none.
fn pinned(root: &Path) {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/v242-engine.db");
    let path = root.join(kernel_storage::migrations::DATABASE);
    std::fs::copy(&fixture, &path).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let mut db = rusqlite::Connection::open(&path).unwrap();
    crate::storage::migrations::upgrade(&mut db, &path, None, crate::storage::migrations::STEPS)
        .unwrap();
}

#[test]
fn an_admitted_restore_through_a_linked_root_brings_back_the_pinned_backup() -> std::io::Result<()>
{
    let (dir, _beside, link) = linked_root()?;
    pinned(dir.path());
    record(dir.path(), "pending", 0o600);

    let restored = after_stop_when(
        true,
        &link,
        &link.join("global.toml"),
        &link.join("project.toml"),
    );

    assert_eq!(restored, Ok(()));
    let Some(RestoreState::Restored { copy }) = state(dir.path()) else {
        panic!("the record says the restore ended and names its copy");
    };
    assert!(dir.path().join("backups").join(copy).is_dir());
    Ok(())
}

#[test]
fn a_put_back_through_a_linked_root_brings_the_named_copy_back() -> std::io::Result<()> {
    let (dir, _beside, link) = linked_root()?;
    pinned(dir.path());
    let global = dir.path().join("global.toml");
    let project = dir.path().join("project.toml");
    after_stop_when(true, dir.path(), &global, &project).unwrap();
    let Some(RestoreState::Restored { copy }) = state(dir.path()) else {
        panic!("the restore names the copy it kept");
    };
    record(
        dir.path(),
        &format!("failed storage_restore_rollback_failed {copy}"),
        0o600,
    );

    assert_eq!(put_back(&link, false), Ok(()));

    assert_eq!(state(dir.path()), None);
    Ok(())
}
