#![cfg(target_os = "macos")]

use crazyagent::sandbox::hidden_paths;
use crazyagent::sandbox::{ProjectSandbox, SandboxNetwork};
use rusqlite::Connection;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};

struct Case {
    _temp: tempfile::TempDir,
    project: PathBuf,
    home: PathBuf,
    tmp: PathBuf,
    secret: PathBuf,
    profile: PathBuf,
}

fn case() -> Case {
    let temp = tempfile::Builder::new()
        .prefix("ca-hidden-e2e-")
        .tempdir_in(fs::canonicalize("/tmp").unwrap())
        .unwrap();
    let root = temp.path().canonicalize().unwrap();
    let project = root.join("project");
    let home = root.join("agent").join("home");
    let tmp = root.join("agent").join("tmp");
    let secret = project.join(".local-state").join("flow-sprint");
    for directory in [&project, &home, &tmp, &secret] {
        fs::create_dir_all(directory).unwrap();
    }
    fs::write(secret.join("notes.txt"), "leader notes\n").unwrap();
    let tool = secret.join("slot.sh");
    fs::write(&tool, "#!/bin/sh\necho ran\n").unwrap();
    fs::set_permissions(&tool, fs::Permissions::from_mode(0o755)).unwrap();
    Case {
        _temp: temp,
        project,
        home,
        tmp,
        secret,
        profile: root.join("agent.sb"),
    }
}

fn sandbox_with_hidden(case: &Case, hidden: &[PathBuf]) -> ProjectSandbox {
    let connection = Connection::open_in_memory().unwrap();
    hidden_paths::save(&connection, "app", hidden).unwrap();
    let loaded = hidden_paths::load(&connection, "app").unwrap();
    assert_eq!(loaded, hidden);
    let mut sandbox = ProjectSandbox::new(
        &case.project,
        &case.home,
        &case.tmp,
        SandboxNetwork::Only(Vec::new()),
    )
    .unwrap();
    for folder in &loaded {
        sandbox = sandbox.hide(folder).unwrap();
    }
    sandbox
}

fn run(sandbox: &ProjectSandbox, profile: &Path, script: &str) -> Output {
    sandbox.install(profile).unwrap();
    let mut command = sandbox.command(profile, Path::new("/bin/sh"));
    command
        .arg("-c")
        .arg(script)
        .current_dir(sandbox.project_root())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    crazyagent::spawn_gate::output(&mut command).unwrap()
}

#[test]
fn with_no_hidden_folders_the_agent_reads_a_file_in_the_folder() {
    let case = case();
    let sandbox = sandbox_with_hidden(&case, &[]);
    let notes = case.secret.join("notes.txt");
    let output = run(
        &sandbox,
        &case.profile,
        &format!("cat '{}'", notes.display()),
    );
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, b"leader notes\n");
}

#[test]
fn with_the_folder_hidden_the_agent_cannot_read_a_file_in_it() {
    let case = case();
    let sandbox = sandbox_with_hidden(&case, std::slice::from_ref(&case.secret));
    let notes = case.secret.join("notes.txt");
    let output = run(
        &sandbox,
        &case.profile,
        &format!("cat '{}'", notes.display()),
    );
    assert!(!output.status.success(), "{output:?}");
    let other = case.project.join("open.txt");
    fs::write(&other, "open\n").unwrap();
    let readable = run(
        &sandbox,
        &case.profile,
        &format!("cat '{}'", other.display()),
    );
    assert!(readable.status.success(), "{readable:?}");
    assert_eq!(readable.stdout, b"open\n");
}

#[test]
fn with_the_folder_hidden_the_agent_cannot_run_a_macho_in_it() {
    let case = case();
    let binary = case.secret.join("echo");
    fs::copy("/bin/echo", &binary).unwrap();
    fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)).unwrap();
    let sandbox = sandbox_with_hidden(&case, std::slice::from_ref(&case.secret));
    let output = run(
        &sandbox,
        &case.profile,
        &format!("'{}' hi", binary.display()),
    );
    assert!(!output.status.success(), "{output:?}");
}

#[test]
fn with_the_folder_hidden_under_a_confined_worktree_the_agent_cannot_read_it() {
    let case = case();
    let git = |args: &[&str]| {
        let mut command = std::process::Command::new("/usr/bin/git");
        command
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .current_dir(&case.project)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let status = crazyagent::spawn_gate::status(&mut command).unwrap();
        assert!(status.success(), "git {args:?}");
    };
    git(&["init", "-q"]);
    git(&["config", "user.email", "a@b.c"]);
    git(&["config", "user.name", "t"]);
    fs::write(case.project.join("open.txt"), "open\n").unwrap();
    git(&["add", "open.txt"]);
    git(&["commit", "-q", "-m", "base"]);
    let worktrees = case.project.join("worktrees");
    fs::create_dir_all(&worktrees).unwrap();
    let worktree = worktrees.join("w").canonicalize().ok();
    git(&["worktree", "add", "-q", "worktrees/w"]);
    let worktree = worktree.unwrap_or_else(|| worktrees.join("w").canonicalize().unwrap());
    let connection = Connection::open_in_memory().unwrap();
    hidden_paths::save(&connection, "app", std::slice::from_ref(&case.secret)).unwrap();
    let mut sandbox = ProjectSandbox::new(
        &case.project,
        &case.home,
        &case.tmp,
        SandboxNetwork::Only(Vec::new()),
    )
    .unwrap()
    .confine_to_worktree(&worktree)
    .unwrap();
    for folder in hidden_paths::load(&connection, "app").unwrap() {
        sandbox = sandbox.hide(&folder).unwrap();
    }
    let notes = case.secret.join("notes.txt");
    let output = run(
        &sandbox,
        &case.profile,
        &format!("cat '{}'", notes.display()),
    );
    assert!(!output.status.success(), "{output:?}");
}

#[test]
fn a_hidden_folder_resists_hard_link_clone_and_firmlink() {
    let case = case();
    let sandbox = sandbox_with_hidden(&case, std::slice::from_ref(&case.secret));
    let notes = case.secret.join("notes.txt");
    let linked = case.project.join("linked.txt");
    let link = run(
        &sandbox,
        &case.profile,
        &format!(
            "ln '{}' '{}' && cat '{}'",
            notes.display(),
            linked.display(),
            linked.display()
        ),
    );
    assert!(!link.status.success(), "hard link leaked: {link:?}");
    let cloned = case.project.join("cloned.txt");
    let clone = run(
        &sandbox,
        &case.profile,
        &format!(
            "cp -c '{}' '{}' && cat '{}'",
            notes.display(),
            cloned.display(),
            cloned.display()
        ),
    );
    assert!(!clone.status.success(), "clonefile leaked: {clone:?}");
    let firmlink = format!("/System/Volumes/Data{}", notes.display());
    assert!(
        Path::new(&firmlink).exists(),
        "firmlink alias {firmlink} does not resolve on this host, so the deny is untested"
    );
    let aliased = run(&sandbox, &case.profile, &format!("cat '{firmlink}'"));
    assert!(
        !aliased.status.success(),
        "FIRMLINK READ LEAKED through {firmlink}: {aliased:?}"
    );
}
