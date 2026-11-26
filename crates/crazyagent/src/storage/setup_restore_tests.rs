use super::*;

#[test]
fn a_restore_names_its_copy_or_its_code_and_a_failed_rollback_is_recorded_as_failed() {
    use kernel_storage::restore::{PinCheck, Restored};
    let kept = Path::new("/engine/backups/replaced-20261122T110000Z").to_path_buf();
    let failed = |text: String| restore_line(&Err(EngineError(text)), None);
    let named = Some("replaced-20261122T110000Z");

    assert_eq!(
        restore_line(
            &Ok(Restored {
                kept: kept.clone(),
                pin: PinCheck::Digest,
            }),
            named
        ),
        "restored replaced-20261122T110000Z"
    );
    assert_eq!(
        restore_line(
            &Ok(Restored {
                kept,
                pin: PinCheck::Unverified,
            }),
            None
        ),
        "restored replaced-20261122T110000Z unverified_pin"
    );
    assert_eq!(
        failed("storage_restore_busy".into()),
        "failed storage_restore_busy"
    );
    assert_eq!(
        failed("storage_restore_failed: check; the replaced data stays in /x".into()),
        "failed storage_restore_failed"
    );
    assert_eq!(
        failed(format!(
            "{}: check; the rollback failed: check; the replaced data stays in /x",
            kernel_storage::restore::RESTORE_ROLLBACK_FAILED
        )),
        "failed storage_restore_rollback_failed"
    );
    let rollback = format!(
        "{}: check; the rollback failed: check; the replaced data stays in /x",
        kernel_storage::restore::RESTORE_ROLLBACK_FAILED
    );
    assert_eq!(
        restore_line(&Err(EngineError(rollback)), named),
        "failed storage_restore_rollback_failed replaced-20261122T110000Z"
    );
    assert_eq!(
        restore_line(&Err(EngineError("storage_restore_busy".into())), named),
        "failed storage_restore_busy"
    );
}

#[test]
fn a_failed_rollback_line_names_only_a_kept_copy_and_stays_one_short_line() {
    let rollback = || {
        Err(EngineError(format!(
            "{}: check; the rollback failed: check",
            kernel_storage::restore::RESTORE_ROLLBACK_FAILED
        )))
    };
    let long = format!("replaced-{}", "x".repeat(600));
    for named in [long.as_str(), "replaced-x\nrestored y", "not-a-copy"] {
        assert_eq!(
            restore_line(&rollback(), Some(named)),
            "failed storage_restore_rollback_failed"
        );
    }
}

/// The restore writes `restoring` before it starts and does not
/// start when that write fails, so no restore runs that a crash could leave unmarked: here
/// the record's temp file is a folder, and no backups folder is ever made.
#[cfg(unix)]
#[test]
fn a_restore_whose_restoring_line_cannot_be_written_never_starts() {
    let root = tempfile::Builder::new()
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    std::fs::create_dir(root.path().join("restore-record.new")).unwrap();
    std::fs::create_dir(root.path().join("restore-record.new").join("held")).unwrap();

    let refused = restore_after_stop(
        root.path(),
        &root.path().join("global.toml"),
        &root.path().join("project.toml"),
    )
    .unwrap_err()
    .0;

    assert!(refused.starts_with("storage_restore_failed"), "{refused}");
    assert!(
        !root
            .path()
            .join(kernel_storage::migrations::BACKUPS)
            .exists()
    );
    assert_eq!(
        kernel_storage::restore::read_restore_record(root.path()),
        None
    );
}

/// A `restoring` line that was renamed into place but whose folder flush failed stops the
/// restore before it starts and leaves a failed record, which the next start accepts.
#[cfg(unix)]
#[test]
fn a_restoring_line_that_is_visible_but_not_flushed_leaves_a_failed_record() {
    let root = tempfile::Builder::new()
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    kernel_storage::restore::fail_next_record_flush();

    let refused = restore_after_stop(
        root.path(),
        &root.path().join("global.toml"),
        &root.path().join("project.toml"),
    )
    .unwrap_err()
    .0;

    assert!(refused.starts_with("storage_restore_failed"), "{refused}");
    assert!(
        !root
            .path()
            .join(kernel_storage::migrations::BACKUPS)
            .exists()
    );
    assert_eq!(
        kernel_storage::restore::read_restore_record(root.path()).as_deref(),
        Some("failed storage_restore_failed")
    );
}
