use super::seatbelt::engine_secrets_in;
use crate::error::SandboxError;
use crate::test_hooks::STAND_IN;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

const RECORDS: &str = "stand-in-records";

/// A fresh engine root in a scratch folder. Input: none. Output: the scratch folder, kept
/// alive by the caller, and the canonical engine root inside it.
fn engine_root() -> (tempfile::TempDir, PathBuf) {
    let scratch = tempfile::tempdir().unwrap();
    let root = scratch.path().canonicalize().unwrap().join("engine");
    fs::create_dir(&root).unwrap();
    (scratch, root)
}

/// Makes a folder with a mode and that many empty files. Inputs: the folder, its mode and the
/// count of files. Output: none.
fn filled(folder: &Path, mode: u32, entries: usize) {
    fs::create_dir(folder).unwrap();
    fs::set_permissions(folder, fs::Permissions::from_mode(mode)).unwrap();
    for at in 0..entries {
        fs::write(folder.join(format!("record-{at}")), "").unwrap();
    }
}

/// An engine record folder, a plain 0700 folder of the engine's own named by the hooks, is
/// never a socket place, so one past the entry bound no longer refuses every guarded launch.
#[test]
fn a_record_folder_past_the_entry_bound_keeps_the_engine_guard() {
    let (_scratch, root) = engine_root();
    let records = root.join(RECORDS);
    filled(&records, 0o700, 1025);
    let secrets = engine_secrets_in(&STAND_IN, &root);
    assert!(
        matches!(&secrets, Ok(secrets) if !secrets.contains(&records)),
        "{secrets:?}"
    );
}

/// A listed engine folder past the entry bound is refused with a line that names it, so the
/// owner can tell which folder to clear.
#[test]
fn an_engine_folder_past_the_entry_bound_is_refused_by_name() {
    let (_scratch, root) = engine_root();
    filled(&root.join("full"), 0o700, 1025);
    let refused = engine_secrets_in(&STAND_IN, &root);
    let named = "engine folder full holds more than 1024 entries";
    assert!(
        matches!(&refused, Err(SandboxError::Invalid(reason)) if reason == named),
        "{refused:?}"
    );
}

/// A folder under a record folder's name that others may write is listed as any engine
/// folder is: a socket in it makes it an engine secret.
#[test]
fn a_record_named_folder_others_may_write_is_still_searched_for_sockets() {
    let (_scratch, root) = engine_root();
    let records = root.join(RECORDS);
    filled(&records, 0o770, 1);
    let _socket = std::os::unix::net::UnixListener::bind(records.join("s")).unwrap();
    let secrets = engine_secrets_in(&STAND_IN, &root);
    assert!(
        matches!(&secrets, Ok(secrets) if secrets.contains(&records)),
        "{secrets:?}"
    );
}

/// A link under a record folder's name is checked as any entry is: it is never followed,
/// so the folder it names is no socket place, as before.
#[test]
fn a_record_named_link_is_checked_as_any_entry() {
    let (_scratch, root) = engine_root();
    let target = root.parent().unwrap().join("elsewhere");
    filled(&target, 0o700, 1);
    let _socket = std::os::unix::net::UnixListener::bind(target.join("s")).unwrap();
    let records = root.join(RECORDS);
    std::os::unix::fs::symlink(&target, &records).unwrap();
    let secrets = engine_secrets_in(&STAND_IN, &root);
    assert!(
        matches!(&secrets, Ok(secrets) if !secrets.contains(&records)),
        "{secrets:?}"
    );
}
