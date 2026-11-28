use super::EngineScratch;
use crate::error::SandboxError;
use crate::paths::canonical;
use crate::test_hooks::STAND_IN;
use std::fs;
use std::path::PathBuf;

/// An engine folder for one row, canonical, with the engine's database in it.
/// Output: the guard that removes it and its canonical path.
fn engine() -> (tempfile::TempDir, PathBuf) {
    let scratch = tempfile::tempdir().unwrap();
    let engine = canonical(scratch.path()).unwrap();
    fs::write(engine.join("engine.db"), "engine secret\n").unwrap();
    (scratch, engine)
}

/// The refusal reason of a make that failed. Input: the result. Output: the reason.
fn reason(made: Result<EngineScratch, SandboxError>) -> String {
    match made {
        Err(SandboxError::Invalid(reason)) => reason,
        other => panic!("expected an Invalid refusal, got {other:?}"),
    }
}

/// A made scratch is a new folder at the named place in the engine folder, private to this
/// user, with each missing parent part made private too.
#[test]
fn a_scratch_is_a_new_private_folder_at_its_place_in_the_engine_folder() {
    let (_guard, engine) = engine();
    let made =
        EngineScratch::make(&STAND_IN, &engine, &["research", "agents", "run"], "work").unwrap();
    assert_eq!(made.path(), engine.join("research/agents/run/work"));
    assert_eq!(made.engine(), engine);
    #[cfg(unix)]
    for folder in [
        "research",
        "research/agents",
        "research/agents/run",
        "research/agents/run/work",
    ] {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::symlink_metadata(engine.join(folder))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o700, "{folder}");
    }
}

/// A name already taken is refused, never reused, so a scratch is always a folder the
/// engine made for this run.
#[test]
fn a_scratch_name_already_there_is_refused() {
    let (_guard, engine) = engine();
    EngineScratch::make(&STAND_IN, &engine, &["mcp-checks", "server"], "project").unwrap();
    let again = EngineScratch::make(&STAND_IN, &engine, &["mcp-checks", "server"], "project");
    assert_eq!(reason(again), "engine scratch folder already exists");
}

/// A part that is not one plain name is refused before anything is made.
#[test]
fn a_scratch_part_that_is_not_one_plain_name_is_refused() {
    let (_guard, engine) = engine();
    for (parent, name) in [
        (&["setup"][..], ".."),
        (&["setup"][..], "a/b"),
        (&["."][..], "work"),
        (&[""][..], "work"),
        (&["setup"][..], ""),
    ] {
        let made = EngineScratch::make(&STAND_IN, &engine, parent, name);
        assert_eq!(
            reason(made),
            "engine scratch names must each be one plain name",
            "{parent:?} {name}"
        );
    }
    assert_eq!(fs::read_dir(&engine).unwrap().count(), 1);
}

/// A parent part that is a link is refused, never followed, so no scratch lands outside
/// the engine folder or in a folder someone else made.
#[cfg(unix)]
#[test]
fn a_scratch_parent_that_is_a_link_is_refused() {
    let (_guard, engine) = engine();
    let elsewhere = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(elsewhere.path(), engine.join("setup")).unwrap();
    let made = EngineScratch::make(&STAND_IN, &engine, &["setup", "npm"], "check");
    assert_eq!(
        reason(made),
        "engine scratch folder is not private to this user"
    );
    assert_eq!(fs::read_dir(elsewhere.path()).unwrap().count(), 0);
}

/// A parent part that others may write is refused, so no other user can swap a scratch
/// folder under a run.
#[cfg(unix)]
#[test]
fn a_scratch_parent_others_may_write_is_refused() {
    use std::os::unix::fs::PermissionsExt;
    let (_guard, engine) = engine();
    fs::create_dir(engine.join("setup")).unwrap();
    fs::set_permissions(engine.join("setup"), fs::Permissions::from_mode(0o770)).unwrap();
    let made = EngineScratch::make(&STAND_IN, &engine, &["setup", "npm"], "check");
    assert_eq!(
        reason(made),
        "engine scratch folder is not private to this user"
    );
    assert!(!engine.join("setup/npm").exists());
}

/// A scratch whose first part is a record folder the engine never searches for sockets is
/// a programming error: the make stops before anything is made.
#[test]
#[should_panic(expected = "an engine scratch folder never sits in a record folder")]
fn a_scratch_in_a_record_folder_trips_the_assert() {
    let (_guard, engine) = engine();
    let _ = EngineScratch::make(&STAND_IN, &engine, &["stand-in-records"], "work");
}

/// A scratch named as a record folder itself, with no parent part, trips the same assert.
#[test]
#[should_panic(expected = "an engine scratch folder never sits in a record folder")]
fn a_scratch_named_as_a_record_folder_trips_the_assert() {
    let (_guard, engine) = engine();
    let _ = EngineScratch::make(&STAND_IN, &engine, &[], "stand-in-records");
}

/// On Windows a made scratch and each part made for it are judged private by W1, in an
/// engine folder made as the engine makes its data root: no account but this user, SYSTEM
/// and the administrators holds any right on them, so the inherited list is owner-only.
#[cfg(windows)]
#[test]
fn a_windows_scratch_is_judged_private_by_w1_in_a_private_engine_folder() {
    use crazyagent_win_sandbox::private;
    let scratch = tempfile::tempdir().unwrap();
    let engine = scratch.path().join("engine");
    drop(private::dir(&engine).unwrap());
    let engine = canonical(&engine).unwrap();
    fs::write(engine.join("engine.db"), "engine secret\n").unwrap();
    let made =
        EngineScratch::make(&STAND_IN, &engine, &["mcp-checks", "server"], "project").unwrap();
    for folder in [
        "mcp-checks",
        "mcp-checks/server",
        "mcp-checks/server/project",
    ] {
        drop(private::open_dir(&engine.join(folder)).unwrap());
    }
    assert_eq!(
        made.path(),
        engine.join("mcp-checks").join("server").join("project")
    );
}
