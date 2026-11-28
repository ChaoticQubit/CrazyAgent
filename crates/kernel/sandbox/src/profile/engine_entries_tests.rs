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
    let _socket = kernel_processes::spawn_gate::bind_unix_std(records.join("s")).unwrap();
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
    let _socket = kernel_processes::spawn_gate::bind_unix_std(target.join("s")).unwrap();
    let records = root.join(RECORDS);
    std::os::unix::fs::symlink(&target, &records).unwrap();
    let secrets = engine_secrets_in(&STAND_IN, &root);
    assert!(
        matches!(&secrets, Ok(secrets) if !secrets.contains(&records)),
        "{secrets:?}"
    );
}

/// The refusal of a full engine folder gives a launch its own code and tells the owner the
/// folder and the step to take; any other invalid sandbox keeps the plain code and line.
#[test]
fn a_full_engine_folder_names_its_own_code_and_the_owner_s_step() {
    let (_scratch, root) = engine_root();
    filled(&root.join("full"), 0o700, 1025);
    let refused = engine_secrets_in(&STAND_IN, &root).unwrap_err();
    assert_eq!(crate::lines::invalid_code(&refused), "engine_folder_full");
    assert_eq!(
        crate::lines::launch_refused_line(&refused),
        "The engine folder full holds more than 1024 entries, so agents cannot start. Remove \
         entries you no longer need from it, then start again."
    );
    let other = crate::error::invalid("engine folder full of agents");
    assert_eq!(crate::lines::invalid_code(&other), "sandbox_refused");
    assert_eq!(
        crate::lines::launch_refused_line(&other),
        "the agent sandbox could not be set up"
    );
}

/// A project folder, agent home and temp folder for a guarded build, beside an engine root
/// holding its database. Input: the engine root. Output: the home and the temp folder.
fn agent_folders(root: &Path) -> (PathBuf, PathBuf) {
    fs::write(root.join("engine.db"), "engine secret\n").unwrap();
    let agent = root.parent().unwrap().join("agent");
    let (home, tmp) = (agent.join("home"), agent.join("tmp"));
    for folder in [&home, &tmp] {
        fs::create_dir_all(folder).unwrap();
        fs::set_permissions(folder, fs::Permissions::from_mode(0o700)).unwrap();
    }
    (home, tmp)
}

/// The engine refusal code a guarded build of a project folder gets, or none when the
/// build is accepted. Inputs: the project folder, the agent home and temp folder and the
/// engine root. Output: the code.
fn guarded_code(project: &Path, home: &Path, tmp: &Path, root: &Path) -> Option<&'static str> {
    let network = super::SandboxNetwork::Only(Vec::new());
    super::ProjectSandbox::new(&STAND_IN, project, home, tmp, network)
        .and_then(|sandbox| sandbox.guard_engine(root))
        .err()
        .map(|error| crate::lines::invalid_code(&error))
}

/// A project folder that is one of the engine's record folders, or the engine root itself,
/// is refused by its own code: either would let the agent write the engine's own state.
#[test]
fn a_project_folder_in_or_at_the_engine_root_is_refused_by_its_own_code() {
    let (_scratch, root) = engine_root();
    let (home, tmp) = agent_folders(&root);
    let records = root.join(RECORDS);
    filled(&records, 0o700, 1);
    let deeper = records.join("project");
    fs::create_dir(&deeper).unwrap();
    for project in [&records, &deeper, &root] {
        assert_eq!(
            guarded_code(project, &home, &tmp, &root),
            Some("project_in_engine_folder"),
            "{}",
            project.display()
        );
    }
    let outside = root.parent().unwrap().join("project");
    fs::create_dir(&outside).unwrap();
    assert_eq!(guarded_code(&outside, &home, &tmp, &root), None);
}

/// A project folder named through a link into the engine root is refused: the link itself
/// is no canonical root, and the folder it resolves to, which callers pass, gets the engine
/// folder's code.
#[test]
fn a_project_folder_linked_into_the_engine_root_is_refused() {
    let (_scratch, root) = engine_root();
    let (home, tmp) = agent_folders(&root);
    let records = root.join(RECORDS);
    filled(&records, 0o700, 1);
    let link = root.parent().unwrap().join("linked-project");
    std::os::unix::fs::symlink(&records, &link).unwrap();
    assert!(guarded_code(&link, &home, &tmp, &root).is_some());
    let resolved = link.canonicalize().unwrap();
    assert_eq!(resolved, records);
    assert_eq!(
        guarded_code(&resolved, &home, &tmp, &root),
        Some("project_in_engine_folder")
    );
}

/// A scratch the engine made in its own engine folder is the one project root its guard
/// lets sit there, while the same folder built as a plain project root is refused, so only
/// a folder the engine made for the run becomes a project root inside the engine folder.
#[test]
fn an_engine_scratch_passes_its_own_guard_and_the_same_folder_as_a_plain_root_does_not() {
    let (_scratch, root) = engine_root();
    let (home, tmp) = agent_folders(&root);
    let made = crate::engine_scratch::EngineScratch::make(
        &STAND_IN,
        &root,
        &["research", "agents", "run"],
        "work",
    )
    .unwrap();
    let network = super::SandboxNetwork::Only(Vec::new());
    let guarded = super::ProjectSandbox::in_engine_scratch(&STAND_IN, &made, &home, &tmp, network)
        .and_then(|sandbox| sandbox.guard_engine(&root));
    assert!(guarded.is_ok(), "{:?}", guarded.err());
    assert_eq!(
        guarded_code(made.path(), &home, &tmp, &root),
        Some("project_in_engine_folder")
    );
}

/// A scratch made in a runtime folder is refused when guarded against the wider engine
/// root around it: the exemption holds only for the engine folder it was made in.
#[test]
fn an_engine_scratch_guarded_against_another_engine_folder_is_refused() {
    let (_scratch, outer) = engine_root();
    let runtime = outer.join("runtime");
    fs::create_dir(&runtime).unwrap();
    fs::set_permissions(&runtime, fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(runtime.join("engine.db"), "engine secret\n").unwrap();
    let (home, tmp) = agent_folders(&outer);
    let made = crate::engine_scratch::EngineScratch::make(&STAND_IN, &runtime, &["setup"], "check")
        .unwrap();
    let network = super::SandboxNetwork::Only(Vec::new());
    let refused = super::ProjectSandbox::in_engine_scratch(&STAND_IN, &made, &home, &tmp, network)
        .and_then(|sandbox| sandbox.guard_engine(&runtime))
        .and_then(|sandbox| sandbox.guard_engine_around(&outer, &runtime))
        .err()
        .map(|error| crate::lines::invalid_code(&error));
    assert_eq!(refused, Some("project_in_engine_folder"));
}
