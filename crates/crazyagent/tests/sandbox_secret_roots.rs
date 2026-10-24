#![cfg(unix)]

use crazyagent::sandbox::{ProjectSandbox, SECRET_ROOT_REFUSED, SandboxError, SandboxNetwork};
use std::fs;
use std::path::PathBuf;

struct Case {
    _temp: tempfile::TempDir,
    root: PathBuf,
    runtime: PathBuf,
    project: PathBuf,
    home: PathBuf,
    tmp: PathBuf,
}

fn case() -> Case {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let runtime = root.join("runtime");
    let project = root.join("project");
    let home = runtime.join("team").join("agents").join("a").join("home");
    let tmp = runtime.join("team").join("agents").join("a").join("tmp");
    for directory in [&project, &home, &tmp] {
        fs::create_dir_all(directory).unwrap();
    }
    fs::create_dir_all(runtime.join("account-keys").join("inner")).unwrap();
    fs::write(runtime.join("engine.db"), "").unwrap();
    Case {
        _temp: temp,
        root,
        runtime,
        project,
        home,
        tmp,
    }
}

fn sandbox(case: &Case) -> ProjectSandbox {
    ProjectSandbox::new(
        &case.project,
        &case.home,
        &case.tmp,
        SandboxNetwork::Only(Vec::new()),
    )
    .unwrap()
}

fn guarded(case: &Case) -> ProjectSandbox {
    sandbox(case).guard_engine(&case.runtime).unwrap()
}

fn owner_home() -> PathBuf {
    let user = nix::unistd::User::from_uid(nix::unistd::getuid())
        .unwrap()
        .unwrap();
    user.dir.canonicalize().unwrap_or(user.dir)
}

fn walled(result: Result<ProjectSandbox, SandboxError>) {
    match result {
        Err(SandboxError::Invalid(reason)) => assert_eq!(reason, SECRET_ROOT_REFUSED),
        other => panic!("expected the secret wall refusal, got {other:?}"),
    }
}

#[test]
fn a_root_that_holds_an_owner_secret_is_refused_as_read_root_worktree_and_project() {
    let case = case();
    let home = owner_home();
    for root in [
        home.join(".config"),
        home.join(".config").join("git"),
        home.join("Library"),
    ] {
        walled(sandbox(&case).allow_read(&root));
        walled(sandbox(&case).allow_worktree(&root));
        walled(ProjectSandbox::new(
            &root,
            &case.home,
            &case.tmp,
            SandboxNetwork::Only(Vec::new()),
        ));
    }
}

#[test]
fn a_root_inside_an_owner_secret_is_refused() {
    let case = case();
    let home = owner_home();
    for root in [
        home.join(".ssh"),
        home.join(".ssh").join("keys"),
        home.join(".config").join("gh").join("hosts"),
        home.join(".aws").join("cli"),
        home.join("Library").join("Keychains").join("login"),
    ] {
        walled(sandbox(&case).allow_read(&root));
        walled(sandbox(&case).allow_worktree(&root));
        walled(ProjectSandbox::new(
            &case.project,
            &root,
            &case.tmp,
            SandboxNetwork::Only(Vec::new()),
        ));
    }
}

#[test]
fn a_root_that_holds_engine_secrets_is_refused_in_either_order() {
    let case = case();
    walled(guarded(&case).allow_read(&case.runtime));
    walled(guarded(&case).allow_worktree(&case.runtime));
    walled(
        sandbox(&case)
            .allow_read(&case.runtime)
            .unwrap()
            .guard_engine(&case.runtime),
    );
    walled(
        sandbox(&case)
            .allow_worktree(&case.runtime)
            .unwrap()
            .guard_engine(&case.runtime),
    );
    walled(
        ProjectSandbox::new(
            &case.root,
            &case.home,
            &case.tmp,
            SandboxNetwork::Only(Vec::new()),
        )
        .unwrap()
        .guard_engine(&case.runtime),
    );
}

#[test]
fn a_root_inside_an_engine_secret_is_refused_in_either_order() {
    let case = case();
    let inner = case.runtime.join("account-keys").join("inner");
    let keys = case.runtime.join("account-keys");
    for root in [&keys, &inner] {
        walled(guarded(&case).allow_read(root));
        walled(guarded(&case).allow_worktree(root));
        walled(
            sandbox(&case)
                .allow_read(root)
                .unwrap()
                .guard_engine(&case.runtime),
        );
    }
    walled(
        ProjectSandbox::new(
            &case.project,
            &inner,
            &case.tmp,
            SandboxNetwork::Only(Vec::new()),
        )
        .unwrap()
        .guard_engine(&case.runtime),
    );
}

#[test]
fn unrelated_roots_and_look_alike_names_still_pass() {
    let case = case();
    let harness = case.root.join("harness");
    let look_alike = case.runtime.join("account-keys-old");
    let sibling = case.runtime.join("research");
    let worktree = case.root.join("worktree");
    for directory in [&harness, &look_alike, &sibling, &worktree] {
        fs::create_dir_all(directory).unwrap();
    }
    let widened = guarded(&case)
        .allow_read(&harness)
        .unwrap()
        .allow_read(&look_alike)
        .unwrap()
        .allow_read(&sibling)
        .unwrap()
        .allow_worktree(&worktree)
        .unwrap();
    assert_eq!(
        widened.read_roots(),
        [harness.clone(), look_alike.clone(), sibling.clone()].as_slice()
    );
    let regrouped = sandbox(&case)
        .allow_read(&sibling)
        .unwrap()
        .allow_worktree(&worktree)
        .unwrap()
        .guard_engine(&case.runtime)
        .unwrap();
    assert_eq!(regrouped.read_roots(), [sibling].as_slice());
    let secrets = regrouped.secret_paths();
    assert!(secrets.contains(&case.runtime.join("engine.db")));
    assert!(secrets.contains(&case.runtime.join("account-keys")));
    assert!(secrets.contains(&owner_home().join(".config").join("gh")));
    for root in regrouped.writable_roots().chain(regrouped.read_roots()) {
        assert!(
            secrets
                .iter()
                .all(|secret| !root.starts_with(secret) && !secret.starts_with(root)),
            "{}",
            root.display()
        );
    }
}
