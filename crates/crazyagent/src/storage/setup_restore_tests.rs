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

/// The restore writes `restoring` before it starts and does not
/// start when that write fails, so no restore runs that a crash could leave unmarked: here
/// the record's temp file is a folder, and no backups folder is ever made.
#[cfg(unix)]
#[test]
fn a_restore_whose_restoring_line_cannot_be_written_never_starts() -> Result<(), String> {
    let tmp = std::fs::canonicalize("/tmp").map_err(|error| format!("resolve /tmp: {error}"))?;
    let root = tempfile::Builder::new()
        .tempdir_in(&tmp)
        .map_err(|error| format!("make a temporary folder in {tmp:?}: {error}"))?;
    let temp = root.path().join("restore-record.new");
    for folder in [temp.clone(), temp.join("held")] {
        std::fs::create_dir(&folder).map_err(|error| format!("create {folder:?}: {error}"))?;
    }

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
    Ok(())
}
