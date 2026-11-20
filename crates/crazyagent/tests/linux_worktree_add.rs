#![cfg(target_os = "linux")]
use crazyagent::research::runner::add_worktree_at;
use crazyagent::sandbox::{LinuxSupervisor, ProjectSandbox, SandboxNetwork};

use std::fs;

use std::path::Path;

use std::process::Command;

/// Runs the owner's own git in `folder` with no system or global config and checks it
/// succeeded. Inputs: the folder and git's arguments. Output: none, or a panic with git's
/// error output.
fn owner_git(folder: &Path, arguments: &[&str]) {
    let output = Command::new("git")
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .arg("-C")
        .arg(folder)
        .args(arguments)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {arguments:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// With an engine root recorded, as the engine binary records it at its start, a Linux
/// worktree add runs under the call supervisor, which holds every admin folder already in
/// `.git/worktrees`: a second agent worktree is added beside one the owner's git made, and
/// that first admin folder's `gitdir` and `commondir` are byte for byte what they were
/// (V244-LINUX-TEAM S4, leader 12:3x).
#[test]
fn a_supervised_linux_worktree_add_keeps_every_other_admin_folder_byte_for_byte() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let engine = root.join("engine");
    let project = root.join("project");
    let home = root.join("agent").join("home");
    let tmp = root.join("agent").join("tmp");
    let profiles = root.join("profiles");
    for folder in [&engine, &project, &home, &tmp, &profiles] {
        fs::create_dir_all(folder).unwrap();
    }
    crazyagent::tools::record_engine_root(&engine);
    owner_git(&project, &["init", "-q", "--template="]);
    owner_git(
        &project,
        &[
            "-c",
            "user.name=Owner",
            "-c",
            "user.email=owner@example.invalid",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "Start project",
        ],
    );
    let worktrees = project.join("worktrees");
    let first = worktrees.join("agent-one");
    owner_git(
        &project,
        &[
            "worktree",
            "add",
            "-q",
            "--detach",
            first.to_str().unwrap(),
            "HEAD",
        ],
    );
    let admins = project.join(".git").join("worktrees");
    let links = |admin: &Path| -> [Vec<u8>; 2] {
        ["gitdir", "commondir"].map(|name| fs::read(admin.join(name)).unwrap())
    };
    let before = links(&admins.join("agent-one"));
    assert!(before.iter().all(|bytes| !bytes.is_empty()));
    let sandbox =
        ProjectSandbox::new(&project, &home, &tmp, SandboxNetwork::Only(Vec::new())).unwrap();
    let second = worktrees.join("agent-two");

    add_worktree_at(
        &sandbox,
        &profiles.join("agent.sb"),
        &second,
        "HEAD",
        LinuxSupervisor::On,
    )
    .unwrap();

    assert!(second.join(".git").is_file());
    assert!(admins.join("agent-two").join("gitdir").is_file());
    assert_eq!(links(&admins.join("agent-one")), before);
    let mut names: Vec<String> = fs::read_dir(&admins)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(names, ["agent-one", "agent-two"]);
}
