use super::ProjectSandbox;
use crate::error::SandboxError;
use crate::net::SandboxNetwork;
use crate::paths::{SECRET_ROOT_REFUSED, canonical, owner_home, owner_secrets};
use crate::test_hooks::STAND_IN;
use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

/// A guarded sandbox over fresh folders: the project, home and temp folders and an engine
/// runtime holding `engine.db` and a real `stand-in-secrets` folder, with `stand-in-keys`
/// left missing. Output: the scratch guard, its canonical root, the project, the runtime
/// and the sandbox.
fn guarded() -> (tempfile::TempDir, PathBuf, PathBuf, PathBuf, ProjectSandbox) {
    let scratch = tempfile::tempdir().unwrap();
    let root = canonical(scratch.path()).unwrap();
    let [project, home, tmp, runtime] =
        ["project", "home", "tmp", "runtime"].map(|name| root.join(name));
    for folder in [&project, &home, &tmp, &runtime] {
        fs::create_dir(folder).unwrap();
    }
    fs::write(runtime.join("engine.db"), "engine secret\n").unwrap();
    fs::create_dir(runtime.join("stand-in-secrets")).unwrap();
    let sandbox = ProjectSandbox::new(
        &STAND_IN,
        &project,
        &home,
        &tmp,
        SandboxNetwork::Only(Vec::new()),
    )
    .unwrap()
    .guard_engine(&runtime)
    .unwrap();
    assert!(!runtime.join("stand-in-keys").exists());
    (scratch, root, project, runtime, sandbox)
}

/// Asserts that installing the sandbox's profile beside the scratch folders is refused as
/// a root that holds or sits in a secret. Inputs: the sandbox and the scratch root.
fn install_refused(sandbox: &ProjectSandbox, root: &Path) {
    let installed = sandbox.install(&root.join("agent.sb"));
    assert!(
        matches!(&installed, Err(SandboxError::Invalid(reason)) if reason == SECRET_ROOT_REFUSED),
        "{installed:?}"
    );
}

/// Runs one phase of a sandbox build and keeps its reading. Inputs: the phase name, the
/// readings so far and the phase. Output: the phase's result, with the phase's name, walk
/// count and time in ns pushed onto the readings.
fn phase<T>(
    name: &'static str,
    readings: &mut Vec<(&'static str, usize, u128)>,
    run: impl FnOnce() -> T,
) -> T {
    let walks = crate::paths::walks();
    let started = std::time::Instant::now();
    let out = run();
    let spent = started.elapsed().as_nanos();
    readings.push((name, crate::paths::walks() - walks, spent));
    out
}

/// A build shaped like a team launch walks the owner home, the harness homes and the owner
/// secrets once, the engine secrets once when guarded, each granted root or file once, and
/// every secret and every granted root or file once more at install, so a launch costs those
/// walks and no more. It prints the walks and the time in ns of each phase, and their sums.
#[test]
fn a_team_shaped_build_walks_each_list_once_and_each_candidate_once() {
    let scratch = tempfile::tempdir().unwrap();
    let root = canonical(scratch.path()).unwrap();
    let names = [
        "project", "home", "tmp", "runtime", "short", "install", "one", "two",
    ];
    let [project, home, tmp, runtime, short, install, one, two] = names.map(|name| root.join(name));
    for folder in [
        &project, &home, &tmp, &runtime, &short, &install, &one, &two,
    ] {
        fs::create_dir(folder).unwrap();
    }
    fs::write(runtime.join("engine.db"), "engine secret\n").unwrap();
    let program = install.join("harness");
    fs::write(&program, "#!/bin/sh\n").unwrap();
    let harnesses = (STAND_IN.access.harness_folders)();
    let owner = owner_home().unwrap();
    let secrets = owner_secrets(&owner, &harnesses).count();
    let before = crate::paths::walks();
    let mut readings = Vec::new();
    let sandbox = phase("new", &mut readings, || {
        ProjectSandbox::new(
            &STAND_IN,
            &project,
            &home,
            &tmp,
            SandboxNetwork::Only(Vec::new()),
        )
    })
    .unwrap();
    let sandbox = phase("guard_engine", &mut readings, || {
        sandbox.guard_engine(&runtime)
    })
    .unwrap();
    let sandbox = phase("allow_scratch", &mut readings, || {
        sandbox.allow_scratch(&short)
    })
    .unwrap();
    let sandbox = phase("allow_read", &mut readings, || sandbox.allow_read(&install)).unwrap();
    let sandbox = phase("allow_read", &mut readings, || sandbox.allow_read(&one)).unwrap();
    let sandbox = phase("allow_read", &mut readings, || sandbox.allow_read(&two)).unwrap();
    let sandbox = phase("allow_exec", &mut readings, || sandbox.allow_exec(&program)).unwrap();
    phase("install", &mut readings, || {
        sandbox.install(&root.join("agent.sb"))
    })
    .unwrap();
    assert_eq!(readings.len(), 8);
    let walked: usize = readings.iter().map(|reading| reading.1).sum();
    let spent: u128 = readings.iter().map(|reading| reading.2).sum();
    assert_eq!(walked, crate::paths::walks() - before);
    let engine = sandbox.secret_paths().len() - secrets;
    assert!(secrets > 0 && engine > 0);
    for (name, walks, ns) in &readings {
        eprintln!("team_shaped_build phase={name} walks={walks} ns={ns}");
    }
    eprintln!("team_shaped_build walks={walked} build_ns={spent}");
    let lists = 1 + harnesses.len() + secrets + engine;
    let candidates = 3 + 1 + 3 + 1;
    assert_eq!(
        walked,
        lists + candidates + secrets + engine + candidates,
        "{walked} walks"
    );
}

/// An engine secret that did not exist when the sandbox was guarded, made afterwards as a
/// link into the project folder, refuses the install, since the install walks every secret
/// again against the roots it granted.
#[test]
fn a_secret_made_after_the_walk_as_a_link_into_the_project_refuses_the_install() {
    let (_scratch, root, project, runtime, sandbox) = guarded();
    fs::create_dir(project.join("keys")).unwrap();
    symlink(project.join("keys"), runtime.join("stand-in-keys")).unwrap();
    install_refused(&sandbox, &root);
}

/// An engine secret folder swapped for a link into the project folder after the build
/// refuses the install.
#[test]
fn a_secret_folder_swapped_for_a_link_into_the_project_after_the_build_refuses_the_install() {
    let (_scratch, root, project, runtime, sandbox) = guarded();
    fs::rename(runtime.join("stand-in-secrets"), root.join("secrets.old")).unwrap();
    fs::create_dir(project.join("inner")).unwrap();
    symlink(project.join("inner"), runtime.join("stand-in-secrets")).unwrap();
    install_refused(&sandbox, &root);
}

/// An engine secret folder renamed into the project folder after the build, with a link
/// left at its old path, keeps its identity but now sits in a granted root, and refuses
/// the install.
#[test]
fn a_secret_folder_renamed_into_the_project_with_a_link_left_behind_refuses_the_install() {
    let (_scratch, root, project, runtime, sandbox) = guarded();
    let moved = project.join("moved");
    fs::rename(runtime.join("stand-in-secrets"), &moved).unwrap();
    symlink(&moved, runtime.join("stand-in-secrets")).unwrap();
    install_refused(&sandbox, &root);
}

/// The folder above the engine secrets swapped for a link to the project folder after the
/// build refuses the install, since every secret below it now leads into a granted root.
#[test]
fn the_runtime_folder_swapped_for_a_link_into_the_project_after_the_build_refuses_the_install() {
    let (_scratch, root, project, runtime, sandbox) = guarded();
    fs::rename(&runtime, root.join("runtime.old")).unwrap();
    symlink(&project, &runtime).unwrap();
    install_refused(&sandbox, &root);
}

/// A secret made after the build outside every granted root leaves the install as it was.
#[test]
fn a_secret_made_after_the_build_outside_every_root_keeps_the_install() {
    let (_scratch, root, _project, runtime, sandbox) = guarded();
    fs::create_dir(runtime.join("stand-in-keys")).unwrap();
    sandbox.install(&root.join("agent.sb")).unwrap();
}

/// A root that holds an engine secret the guard added is refused, as the secrets of the
/// guarded list are walked for every later grant.
#[test]
fn a_read_root_holding_a_guarded_engine_secret_is_refused() {
    let (_scratch, _root, _project, runtime, sandbox) = guarded();
    let refused = sandbox.allow_read(&runtime);
    assert!(
        matches!(&refused, Err(SandboxError::Invalid(reason)) if reason == SECRET_ROOT_REFUSED),
        "{refused:?}"
    );
}

/// Asserts that installing the sandbox's profile is refused as a granted root that changed
/// since the build. Inputs: the sandbox and the scratch root.
fn install_refused_as_changed(sandbox: &ProjectSandbox, root: &Path) {
    let installed = sandbox.install(&root.join("agent.sb"));
    let changed = "a granted path changed between the sandbox build and its install";
    assert!(
        matches!(&installed, Err(SandboxError::Invalid(reason)) if reason == changed),
        "{installed:?}"
    );
}

/// A granted read root renamed away after the build, with a link to the engine runtime left
/// at its path, refuses the install: the profile grants by path, so the root is walked
/// again and its new identity is not the one the build checked.
#[test]
fn a_granted_root_replaced_by_a_link_to_the_runtime_after_the_build_refuses_the_install() {
    let (_scratch, root, _project, runtime, sandbox) = guarded();
    let granted = root.join("granted");
    fs::create_dir(&granted).unwrap();
    let sandbox = sandbox.allow_read(&granted).unwrap();
    fs::rename(&granted, root.join("granted.old")).unwrap();
    symlink(&runtime, &granted).unwrap();
    install_refused_as_changed(&sandbox, &root);
}

/// A granted read root renamed away after the build, with a new plain folder made at its
/// path, refuses the install, since the folder the profile grants is not the one checked.
#[test]
fn a_granted_root_replaced_by_a_new_folder_after_the_build_refuses_the_install() {
    let (_scratch, root, _project, _runtime, sandbox) = guarded();
    let granted = root.join("granted");
    fs::create_dir(&granted).unwrap();
    let sandbox = sandbox.allow_read(&granted).unwrap();
    fs::rename(&granted, root.join("granted.old")).unwrap();
    fs::create_dir(&granted).unwrap();
    install_refused_as_changed(&sandbox, &root);
}
