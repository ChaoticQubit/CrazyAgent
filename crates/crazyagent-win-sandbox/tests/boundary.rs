#![cfg(windows)]

use crazyagent_win_sandbox::profile::{BoundaryProfile, Guard, Guarded, check_path};
use crazyagent_win_sandbox::request::REFUSED_EXIT;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::mpsc;
use std::time::Duration;

const HELPER: &str = env!("CARGO_BIN_EXE_crazyagent-win-boundary");

struct Case {
    _temp: tempfile::TempDir,
    project: PathBuf,
    home: PathBuf,
    outside: PathBuf,
    secret: PathBuf,
    profile: PathBuf,
}

fn plain(path: &Path) -> String {
    let text = path.to_str().unwrap().to_owned();
    assert_eq!(check_path(&text), Ok(()), "{text}");
    text
}

fn cmd() -> String {
    let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
    format!(r"{root}\System32\cmd.exe")
}

fn case(container: &str) -> Case {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().to_path_buf();
    let project = root.join("project");
    let home = root.join("agent").join("home");
    let tmp = root.join("agent").join("tmp");
    let owner = root.join("owner");
    let outside = root.join("outside");
    let profiles = root.join("profiles");
    for folder in [
        &project,
        &project.join("context"),
        &home,
        &tmp,
        &owner.join(".ssh"),
        &outside,
        &profiles,
    ] {
        fs::create_dir_all(folder).unwrap();
    }
    fs::write(project.join("readme.txt"), "project readme\r\n").unwrap();
    fs::write(project.join("context").join("brief.md"), "brief\r\n").unwrap();
    let secret = owner.join(".ssh").join("id_ed25519");
    fs::write(&secret, "PRIVATE KEY MATERIAL\r\n").unwrap();
    let profile = BoundaryProfile {
        container: container.into(),
        capabilities: Vec::new(),
        read_roots: Vec::new(),
        write_roots: vec![plain(&project), plain(&home), plain(&tmp)],
        guarded: vec![
            Guarded {
                path: plain(&project.join("context")),
                guard: Guard::ReadOnly,
            },
            Guarded {
                path: plain(&owner.join(".ssh")),
                guard: Guard::Hidden,
            },
        ],
        active_processes: 32,
    };
    let profile_path = profiles.join("agent.profile");
    fs::write(&profile_path, profile.encode().unwrap()).unwrap();
    Case {
        _temp: temp,
        project,
        home,
        outside,
        secret,
        profile: profile_path,
    }
}

fn boundary(case: &Case) -> Command {
    let mut command = Command::new(HELPER);
    command
        .arg("--profile")
        .arg(&case.profile)
        .arg("--")
        .arg(cmd())
        .current_dir(&case.project);
    command
}

fn run(case: &Case, args: &[&str]) -> Output {
    boundary(case)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .unwrap()
}

#[test]
fn an_agent_reads_its_project_but_not_a_secret_under_the_owners_home() {
    let case = case("crazyagent.test.read");
    let readme = run(
        &case,
        &["/C", "type", &plain(&case.project.join("readme.txt"))],
    );
    assert!(
        readme.status.success(),
        "{}",
        String::from_utf8_lossy(&readme.stderr)
    );
    assert!(String::from_utf8_lossy(&readme.stdout).contains("project readme"));
    let secret = run(&case, &["/C", "type", &plain(&case.secret)]);
    assert!(!secret.status.success());
    assert!(!String::from_utf8_lossy(&secret.stdout).contains("PRIVATE KEY"));
    assert!(!String::from_utf8_lossy(&secret.stderr).contains("PRIVATE KEY"));
}

#[test]
fn an_agent_writes_in_its_roots_but_not_outside_them_or_into_protected_paths() {
    let case = case("crazyagent.test.write");
    let inside = case.project.join("inside.txt");
    let home = case.home.join("home.txt");
    let escape = case.outside.join("escape.txt");
    let brief = case.project.join("context").join("brief.md");
    run(&case, &["/C", "echo", "inside>", &plain(&inside)]);
    run(&case, &["/C", "echo", "home>", &plain(&home)]);
    run(&case, &["/C", "echo", "escape>", &plain(&escape)]);
    run(&case, &["/C", "echo", "changed>", &plain(&brief)]);
    run(&case, &["/C", "del", "/F", "/Q", &plain(&brief)]);
    assert!(fs::read_to_string(&inside).unwrap().starts_with("inside"));
    assert!(fs::read_to_string(&home).unwrap().starts_with("home"));
    assert!(!escape.exists());
    assert_eq!(fs::read_to_string(&brief).unwrap(), "brief\r\n");
}

#[test]
fn killing_the_helper_kills_the_agent_it_started() {
    let case = case("crazyagent.test.kill");
    let mut helper = boundary(&case)
        .arg("/K")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdin = helper.stdin.take().unwrap();
    let stdout = helper.stdout.take().unwrap();
    let (events, received) = mpsc::channel();
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        let mut line = String::new();
        while reader.read_line(&mut line).is_ok_and(|read| read > 0) {
            if line.trim() == "ready" {
                let _ = events.send("ready");
            }
            line.clear();
        }
        let _ = events.send("closed");
    });
    stdin.write_all(b"echo ready\r\n").unwrap();
    stdin.flush().unwrap();
    assert_eq!(received.recv_timeout(Duration::from_secs(30)), Ok("ready"));
    helper.kill().unwrap();
    helper.wait().unwrap();
    assert_eq!(
        received.recv_timeout(Duration::from_secs(30)),
        Ok("closed"),
        "the agent kept its output open after the helper died"
    );
    drop(stdin);
}

#[test]
fn a_bad_profile_or_batch_program_starts_nothing() {
    let case = case("crazyagent.test.refuse");
    let marker = case.project.join("started.txt");
    fs::write(
        &case.profile,
        "crazyagent-win-boundary 1\ncontainer crazyagent.test.refuse\n",
    )
    .unwrap();
    let output = run(&case, &["/C", "echo", "started>", &plain(&marker)]);
    assert_eq!(output.status.code(), Some(i32::from(REFUSED_EXIT)));
    assert!(String::from_utf8_lossy(&output.stderr).contains("boundary profile refused"));
    assert!(!marker.exists());
    let batch = Command::new(HELPER)
        .arg("--profile")
        .arg(&case.profile)
        .arg("--")
        .arg(plain(&case.project.join("run.bat")))
        .output()
        .unwrap();
    assert_eq!(batch.status.code(), Some(i32::from(REFUSED_EXIT)));
    assert!(String::from_utf8_lossy(&batch.stderr).contains("batch files are refused"));
}
