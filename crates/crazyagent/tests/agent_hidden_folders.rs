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
fn with_the_folder_hidden_the_agent_cannot_run_a_program_in_it() {
    let case = case();
    let sandbox = sandbox_with_hidden(&case, std::slice::from_ref(&case.secret));
    let tool = case.secret.join("slot.sh");
    let output = run(&sandbox, &case.profile, &format!("'{}'", tool.display()));
    assert!(!output.status.success(), "{output:?}");
}
