use super::NativeProfileCustody;
use crate::engine::EngineError;
use crate::engine::bootstrap::NATIVE_PROFILES_FOLDER;
use crate::engine::owner::HostStamp;
use crazyagent_win_sandbox::private;
use std::fs::File;
use std::path::{Path, PathBuf};

/// One Windows custody case: a W1 private engine root in its plain long-name spelling, its
/// private `engine.db`, and the handle the caller opened the database with, which shares
/// delete so a row can swap the name under it.
struct Case {
    _temp: tempfile::TempDir,
    state: PathBuf,
    database: PathBuf,
    opened: File,
    host: HostStamp,
}

/// Makes a fresh case. Output: the case.
fn case() -> Case {
    let temp = tempfile::tempdir().unwrap();
    let full = std::fs::canonicalize(temp.path()).unwrap();
    let base = PathBuf::from(full.to_str().unwrap().strip_prefix(r"\\?\").unwrap());
    let state = base.join("state");
    drop(private::new_dir(&state).unwrap());
    let database = state.join("engine.db");
    drop(private::file(&database, true).unwrap());
    let opened = private::read_only_replaceable(&database).unwrap();
    assert!(state.is_dir());
    assert!(database.is_file());
    Case {
        _temp: temp,
        state,
        database,
        opened,
        host: HostStamp {
            db_instance: "a".repeat(64),
            engine_epoch: "b".repeat(64),
        },
    }
}

/// The refusal code a call returned. Input: the result. Output: its code text.
fn refusal<T>(result: Result<T, EngineError>) -> String {
    match result {
        Ok(_) => "accepted".to_owned(),
        Err(error) => error.0,
    }
}

/// Moves `from` to `to` by name, as a swap outside the engine would. Inputs: the two paths.
/// Output: none.
fn swap_away(from: &Path, to: &Path) {
    std::fs::rename(from, to).unwrap();
    assert!(!from.exists());
    assert!(to.exists());
}

/// A database held by its caller is captured on Windows and verifies (DESIGN-0515 row 5).
#[test]
fn a_held_database_is_captured_and_verified() {
    let case = case();
    let custody = NativeProfileCustody::capture_held(&case.database, &case.opened).unwrap();
    custody.verify().unwrap();
    custody.verify().unwrap();
}

/// A database name swapped between the caller's open and the capture is refused: the file
/// custody opens is not the caller's (DESIGN-0515 row 5, QUESTION-1700 Q1).
#[test]
fn a_database_swapped_before_the_capture_is_refused() {
    let case = case();
    swap_away(&case.database, &case.state.join("engine.db.old"));
    drop(private::file(&case.database, true).unwrap());
    let captured = NativeProfileCustody::capture_held(&case.database, &case.opened);
    assert_eq!(refusal(captured), "native_profile_custody_unsafe");
}

/// A database with a second name, a hard link the private judge counts, is refused: custody
/// opens only a file with one link (rv-sec3 RULING-16:50 C5 row 2, the hard-link variant).
#[test]
fn a_database_with_a_second_link_is_refused() {
    let case = case();
    std::fs::hard_link(&case.database, case.state.join("engine-link.db")).unwrap();
    let captured = NativeProfileCustody::capture_held(&case.database, &case.opened);
    assert_eq!(refusal(captured), "native_profile_custody_unsafe");
}

/// A custody whose stored database identity is zero fails `verify`, the twin of the Unix
/// `inode == 0` refusal (rv-sec3 RULING-16:50 C2, C5 row 5).
#[test]
fn a_zero_database_identity_fails_verify() {
    let case = case();
    let mut custody = NativeProfileCustody::capture_held(&case.database, &case.opened).unwrap();
    custody.verify().unwrap();
    custody.forget_database_identity();
    assert_eq!(refusal(custody.verify()), "native_profile_custody_unsafe");
}

/// While custody holds the database, its name cannot be swapped: the rename is refused and
/// the custody still verifies.
#[test]
fn a_held_database_cannot_be_swapped_under_custody() {
    let case = case();
    let custody = NativeProfileCustody::capture_held(&case.database, &case.opened).unwrap();
    drop(case.opened);
    let moved = std::fs::rename(&case.database, case.state.join("engine.db.old"));
    assert!(moved.is_err(), "the held database was renamed");
    custody.verify().unwrap();
}

/// The profile root is made once, pinned, and opened again by a second custody of the
/// same database with the same identity; a root swapped for another folder holding the
/// same manifest is refused.
#[test]
fn the_profile_root_is_pinned_and_a_swapped_root_is_refused() {
    let case = case();
    let mut first = NativeProfileCustody::capture_held(&case.database, &case.opened).unwrap();
    let (_, _, made) = first.root(&case.host).unwrap();
    let mut second = NativeProfileCustody::capture_held(&case.database, &case.opened).unwrap();
    let (_, _, opened) = second.root(&case.host).unwrap();
    assert!(made == opened);
    let root = case.state.join(NATIVE_PROFILES_FOLDER);
    let moved = case.state.join("moved");
    swap_away(&root, &moved);
    drop(private::new_dir(&root).unwrap());
    swap_away(&moved.join("identity.json"), &root.join("identity.json"));
    assert_eq!(
        refusal(first.root(&case.host)),
        "native_profile_binding_conflict"
    );
    assert_eq!(
        refusal(second.root(&case.host)),
        "native_profile_binding_conflict"
    );
}

/// A capture from metadata alone is refused on Windows, where metadata cannot name the
/// file (QUESTION-1700 Q1); a pin that holds before and after N6a.
#[test]
fn a_capture_from_metadata_is_refused_on_windows() {
    let case = case();
    let metadata = std::fs::metadata(&case.database).unwrap();
    let captured = NativeProfileCustody::capture(&case.database, &metadata);
    assert_eq!(refusal(captured), "native_profile_custody_unavailable");
}

/// A staging folder whose creator still holds its lock stream claim is left by a sweep, and
/// the creator still commits it, as the sweep never opens a claimed folder (DESIGN-0601 R1,
/// R4); an abandoned staging folder, whose creator died after its claim and left the stream
/// unlocked, is removed (rv-sec3 RULING-17:20 K3 row 3).
#[test]
fn a_sweep_leaves_a_claimed_staging_folder_and_removes_an_abandoned_one() {
    use super::staging::{ROOT_STAGING, Staging, sweep};
    let case = case();
    let custody = NativeProfileCustody::capture_held(&case.database, &case.opened).unwrap();
    let abandoned = case.state.join(format!("{ROOT_STAGING}{}", "0".repeat(32)));
    drop(private::new_dir(&abandoned).unwrap());
    let mut stream = abandoned.as_os_str().to_owned();
    stream.push(":crazyagent.lock");
    drop(std::fs::File::create(PathBuf::from(stream)).unwrap());
    assert!(has_lock_stream(&abandoned));
    let live = Staging::create(&custody.anchor_folder, ROOT_STAGING).unwrap();
    let staged: Vec<PathBuf> = std::fs::read_dir(&case.state)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path != &abandoned && path.is_dir())
        .collect();
    assert_eq!(staged.len(), 1, "{staged:?}");
    let live_path = staged[0].clone();
    sweep(&custody.anchor_folder, ROOT_STAGING).unwrap();
    assert!(!abandoned.exists(), "the abandoned staging folder stayed");
    assert!(live_path.is_dir(), "the claimed staging folder was swept");
    let committed = live.commit(&custody.anchor_folder, "committed").unwrap();
    assert!(case.state.join("committed").is_dir());
    assert!(!live_path.exists());
    drop(committed);
}

/// The parent's entries, sorted, so a row can show a sweep made or removed nothing.
/// Input: the folder. Output: the entry names.
fn entries(folder: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(folder)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    assert!(names.len() < 64);
    names
}

/// Whether the entry's `crazyagent.lock` stream exists. Input: the entry. Output: the answer.
fn has_lock_stream(entry: &Path) -> bool {
    let mut stream = entry.as_os_str().to_owned();
    stream.push(":crazyagent.lock");
    std::fs::symlink_metadata(PathBuf::from(stream)).is_ok()
}

/// A sweeper's claim on a staging name a live creator just renamed away makes nothing and
/// answers `Missing`, so the parent's entries stay as they were (rv-sec3 RULING-17:20 K1,
/// K3 row 1): an open that created the stream would leave a plain staging-named file that
/// every later sweep refuses.
#[test]
fn a_sweep_claim_on_a_vanished_staging_name_makes_nothing() {
    use super::HeldError;
    use super::staging::ROOT_STAGING;
    let case = case();
    let custody = NativeProfileCustody::capture_held(&case.database, &case.opened).unwrap();
    let before = entries(&case.state);
    let gone = format!("{ROOT_STAGING}{}", "1".repeat(32));
    let name = super::held_name(std::ffi::OsStr::new(&gone)).unwrap();
    let claimed = custody.anchor_folder.claim_entry(&name);
    assert!(matches!(claimed, Err(HeldError::Missing)));
    assert_eq!(entries(&case.state), before);
}

/// A staging-named plain file and a staging-named junction are left by a sweep, which makes
/// no stream on either and no new entry, and the junction's target is untouched (rv-sec3
/// RULING-17:20 K1, K3 row 2).
#[test]
fn a_sweep_makes_no_stream_on_a_staging_named_file_or_junction() {
    use super::staging::{ROOT_STAGING, sweep};
    let case = case();
    let custody = NativeProfileCustody::capture_held(&case.database, &case.opened).unwrap();
    let file = case.state.join(format!("{ROOT_STAGING}{}", "2".repeat(32)));
    drop(private::file(&file, true).unwrap());
    let target = case.state.parent().unwrap().join("target");
    std::fs::create_dir(&target).unwrap();
    let junction = case.state.join(format!("{ROOT_STAGING}{}", "3".repeat(32)));
    let made = crate::spawn_gate::status(
        std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&junction)
            .arg(&target)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null()),
    )
    .unwrap();
    assert!(made.success());
    assert!(junction.symlink_metadata().is_ok());
    let before = entries(&case.state);
    sweep(&custody.anchor_folder, ROOT_STAGING).unwrap();
    assert_eq!(entries(&case.state), before);
    assert!(
        !has_lock_stream(&file),
        "the sweep made a stream on the file"
    );
    assert!(
        !has_lock_stream(&junction),
        "the sweep made a stream on the junction"
    );
    assert!(!has_lock_stream(&target), "the sweep followed the junction");
    assert!(entries(&target).is_empty());
}
