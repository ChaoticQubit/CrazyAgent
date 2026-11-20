#![cfg(target_os = "linux")]
use crazyagent::research::runner::add_worktree_at;
use crazyagent::sandbox::{
    InstalledSandbox, LinuxStrict, LinuxSupervisor, ProjectSandbox, SandboxNetwork,
};

use std::fs;

use std::path::{Path, PathBuf};

use std::process::Command;

use std::sync::OnceLock;

/// The engine root every row of this binary records, made once, since a process records
/// one engine root ([`crazyagent::tools::record_engine_root`]), as the engine binary does
/// at its start. No input. Output: the recorded root.
fn engine() -> &'static Path {
    static ROOT: OnceLock<(tempfile::TempDir, PathBuf)> = OnceLock::new();
    let (_, root) = ROOT.get_or_init(|| {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap().join("engine");
        fs::create_dir(&root).unwrap();
        crazyagent::tools::record_engine_root(&root);
        (temp, root)
    });
    root
}

/// Runs the owner's own git in `folder` with no system or global config and checks it
/// succeeded. Inputs: the folder and git's arguments. Output: none, or a panic with git's
/// error output.
fn owner_git(folder: &Path, arguments: &[&str]) {
    let output = crazyagent::spawn_gate::output(
        Command::new("git")
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .arg("-C")
            .arg(folder)
            .args(arguments),
    )
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
    engine();
    let project = root.join("project");
    let home = root.join("agent").join("home");
    let tmp = root.join("agent").join("tmp");
    let profiles = root.join("profiles");
    for folder in [&project, &home, &tmp, &profiles] {
        fs::create_dir_all(folder).unwrap();
    }
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

/// The worktree-add step's own boundary holds every admin folder already in
/// `.git/worktrees` (rv-sec3 #760 L1): run for real under the call supervisor, an append to
/// another worktree's `HEAD` and a ref write through that worktree's admin folder are
/// refused, and its `HEAD`, `gitdir` and `commondir` stay byte for byte, while the step may
/// still make a new admin folder, as its own add does.
#[test]
fn the_worktree_add_step_cannot_write_an_admin_folder_it_holds() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    engine();
    let project = root.join("project");
    let home = root.join("agent").join("home");
    let tmp = root.join("agent").join("tmp");
    let profiles = root.join("profiles");
    for folder in [&project, &home, &tmp, &profiles] {
        fs::create_dir_all(folder).unwrap();
    }
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
    let first = project.join("worktrees").join("agent-one");
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
    let admin = admins.join("agent-one");
    let held = || ["HEAD", "gitdir", "commondir"].map(|name| fs::read(admin.join(name)).unwrap());
    let before = held();
    let sandbox =
        ProjectSandbox::new(&project, &home, &tmp, SandboxNetwork::Only(Vec::new())).unwrap();
    let step = InstalledSandbox::install(
        sandbox.adding_worktrees().unwrap(),
        &profiles.join("adding.sb"),
        LinuxSupervisor::On,
        LinuxStrict::Off,
    )
    .unwrap();
    let script = "\
        e=$( { printf x >> .git/worktrees/agent-one/HEAD; } 2>&1 ) \
            && echo append=wrote || echo \"append=refused:$e\"
        e=$(/usr/bin/git --git-dir=.git/worktrees/agent-one symbolic-ref HEAD refs/heads/planted 2>&1) \
            && echo ref=wrote || echo \"ref=refused:$e\"
        /bin/mkdir .git/worktrees/agent-two && echo made=yes || echo made=no
    ";
    let mut harness = Command::new("/bin/sh");
    harness
        .arg("-c")
        .arg(script)
        .current_dir(&project)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null");
    let mut wrapped = step
        .wrap(&harness)
        .expect("the step's boundary holds agent-one under the call supervisor");
    let output = crazyagent::spawn_gate::output(&mut wrapped).unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let refused = |probe: &str| {
        text.lines().any(|line| {
            line.starts_with(&format!("{probe}=refused:"))
                && (line.contains("Permission denied") || line.contains("Operation not permitted"))
        })
    };
    assert!(refused("append"), "{text}");
    assert!(refused("ref"), "{text}");
    assert!(text.lines().any(|line| line == "made=yes"), "{text}");
    assert_eq!(held(), before, "{text}");
    assert!(
        admin.join("HEAD.lock").symlink_metadata().is_err(),
        "{text}"
    );
}
