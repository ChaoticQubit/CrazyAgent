use std::hint::black_box;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::{at, row};
use crate::migrations::{
    BACKUP_FAILED, BACKUP_FAILURE_FILE, BACKUPS, CHECKED, backup_failure, backups_in, pinned, stamp,
};
use crate::perf_kit::{SAMPLES, time};
use crate::restore::{read_restore_record, restorable_until, write_restore_record};

const READ_BATCH: usize = 10;
const WRITE_BATCH: usize = 1;
const RECORD: &str = "restored replaced-20261124T082500123Z";
const OLDER: [&str; 4] = ["7-to-8", "8-to-9", "9-to-10", "10-to-11"];
const PINNED_VERSIONS: &str = "11-to-12";

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

/// An engine root as a 2.5 engine leaves it after an upgrade from 2.4: a private
/// `backups` folder with four older dated backups and one checked 11-to-12 backup a day
/// old, a backup failure record and a restore record. Input: the temp folder. Output: the
/// root.
pub(crate) fn engine_root(temp: &Path) -> PathBuf {
    let root = temp.join("engine");
    private_folder(&root);
    let backups = root.join(BACKUPS);
    private_folder(&backups);
    for (days, versions) in OLDER.iter().enumerate() {
        let taken = at() - Duration::from_secs(86_400 * (40 - days as u64));
        private_folder(&backups.join(format!("{versions}-{}", stamp(taken).expect("stamp"))));
    }
    let taken = at() - Duration::from_secs(86_400);
    let pinned = backups.join(format!(
        "{PINNED_VERSIONS}-{}",
        stamp(taken).expect("stamp")
    ));
    private_folder(&pinned);
    private_text(&pinned.join(CHECKED), b"");
    private_text(&root.join(BACKUP_FAILURE_FILE), BACKUP_FAILED.as_bytes());
    write_restore_record(&root, RECORD).expect("record");
    root
}

/// Times the storage calls that read or write the engine root's own files without an
/// engine: the backup failure record, the backup listing and pin, how long a restore stays
/// possible, and the restore record. Checks each answer once first. Prints one PERF line
/// per call.
#[test]
#[ignore = "timing row: run through scripts/perf/run.sh"]
fn storage_root_file_calls() {
    let temp = tempfile::tempdir().expect("temp");
    let root = engine_root(temp.path());
    let backups = root.join(BACKUPS);
    let found = backups_in(&backups).expect("backups");
    assert_eq!(found.len(), OLDER.len() + 1);
    assert_eq!(backup_failure(&root), Some(BACKUP_FAILED));
    assert_eq!(pinned(&backups, &found, at()), Some(OLDER.len()));
    assert!(restorable_until(&root, at()).is_some());
    assert_eq!(read_restore_record(&root).as_deref(), Some(RECORD));
    write_restore_record(&root, RECORD).expect("record");

    time(
        &row("migrations.backup_failure", READ_BATCH),
        SAMPLES,
        || {
            black_box(backup_failure(black_box(&root)));
        },
    );
    time(&row("migrations.backups_in", READ_BATCH), SAMPLES, || {
        let _ = black_box(backups_in(black_box(&backups)));
    });
    time(&row("migrations.pinned", READ_BATCH), SAMPLES, || {
        black_box(pinned(
            black_box(&backups),
            black_box(&found),
            black_box(at()),
        ));
    });
    time(
        &row("restore.restorable_until", READ_BATCH),
        SAMPLES,
        || {
            black_box(restorable_until(black_box(&root), black_box(at())));
        },
    );
    time(
        &row("restore.write_restore_record", WRITE_BATCH),
        SAMPLES,
        || {
            let _ = black_box(write_restore_record(black_box(&root), black_box(RECORD)));
        },
    );
    time(
        &row("restore.read_restore_record", READ_BATCH),
        SAMPLES,
        || {
            black_box(read_restore_record(black_box(&root)));
        },
    );
}
