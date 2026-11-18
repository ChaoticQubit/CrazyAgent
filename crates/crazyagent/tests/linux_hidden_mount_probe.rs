#![cfg(target_os = "linux")]
use crazyagent::sandbox::linux::{BWRAP_PATHS, LinuxBackend, LinuxSandbox};

use crazyagent::sandbox::{ProjectSandbox, Sandbox, SandboxNetwork};

use std::fs;

use std::io::{BufRead, BufReader, Read, Write};

use std::path::{Path, PathBuf};

use std::process::{Command, Stdio};

const HELPER: &str = env!("CARGO_BIN_EXE_crazyagent-landlock");

const SECRET: &str = "hidden secret words\n";

/// One probe case as the linux_sandbox rows make it: a temporary root holding the project,
/// the agent's home and temp folders, and the hidden folder two levels below the project
/// with one secret file. No input. Output: the case.
struct Case {
    _temp: tempfile::TempDir,
    root: PathBuf,
    project: PathBuf,
    private: PathBuf,
}

fn case() -> Case {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let project = root.join("project");
    let private = project.join("notes").join("inner").join("private");
    let git = project.join(".git");
    for directory in [
        &private,
        &root.join("agent").join("home"),
        &root.join("agent").join("tmp"),
        &project.join("context"),
        &project.join("worktrees"),
        &git.join("hooks"),
        &git.join("objects"),
    ] {
        fs::create_dir_all(directory).unwrap();
    }
    fs::write(git.join("config"), "[core]\n").unwrap();
    fs::write(project.join("context").join("brief.md"), "brief\n").unwrap();
    fs::write(private.join("secret.txt"), SECRET).unwrap();
    Case {
        _temp: temp,
        root,
        project,
        private,
    }
}

/// The two backends that hold a hidden folder. Nested is asserted to be what detection
/// picks, so a runner that cannot nest fails the probe instead of skipping it.
#[derive(Clone, Copy, Debug)]
enum Leg {
    Nested,
    Bubblewrap,
}

const LEGS: [Leg; 2] = [Leg::Nested, Leg::Bubblewrap];

/// The sandbox for one case on one backend, hiding the case's private folder. Inputs: the
/// backend and the case. Output: the sandbox.
fn sandbox(leg: Leg, case: &Case) -> LinuxSandbox {
    let policy = ProjectSandbox::new(
        &case.project,
        &case.root.join("agent").join("home"),
        &case.root.join("agent").join("tmp"),
        SandboxNetwork::Only(Vec::new()),
    )
    .unwrap()
    .hide(&case.private)
    .unwrap();
    match leg {
        Leg::Nested => {
            let sandbox = LinuxSandbox::detect(policy, Path::new(HELPER)).unwrap();
            assert!(
                matches!(sandbox.backend(), LinuxBackend::Nested { .. }),
                "{:?}",
                sandbox.backend()
            );
            sandbox
        }
        Leg::Bubblewrap => {
            let bwrap = BWRAP_PATHS
                .iter()
                .map(Path::new)
                .find(|path| path.is_file())
                .expect("Linux test machines need bubblewrap installed");
            LinuxSandbox::new(
                policy,
                LinuxBackend::Bubblewrap {
                    bwrap: bwrap.to_path_buf(),
                    helper: PathBuf::from(HELPER),
                },
            )
            .unwrap()
        }
    }
}

/// A bind mount the probe makes with `sudo -n mount --bind`, taken down with a lazy unmount
/// when it is dropped. Inputs: the folder shown and the existing folder it is shown at.
/// Output: the guard.
struct Bound(PathBuf);

impl Bound {
    fn new(source: &Path, at: &Path) -> Self {
        let status = crazyagent::spawn_gate::status(
            Command::new("sudo")
                .args(["-n", "mount", "--bind"])
                .arg(source)
                .arg(at)
                .stdin(Stdio::null()),
        )
        .unwrap();
        assert!(status.success(), "sudo -n mount --bind {source:?} {at:?}");
        Self(at.to_path_buf())
    }
}

impl Drop for Bound {
    fn drop(&mut self) {
        let _ = crazyagent::spawn_gate::status(
            Command::new("sudo")
                .args(["-n", "umount", "-l"])
                .arg(&self.0)
                .stdin(Stdio::null()),
        );
    }
}

/// The agent's tries on the hidden folder through the second path `alias`, which shows the
/// case root: read, list, write, remove and move, then a read through its own path, then
/// the mount lines that name `alias`. Inputs: the case and the alias. Output: the script.
fn tries(case: &Case, alias: &Path) -> String {
    let through = alias.join(case.private.strip_prefix(&case.root).unwrap());
    format!(
        "cat '{through}/secret.txt' 2>&1; echo \"alias cat $?\"; ls -A '{through}' 2>&1; \
         echo \"alias ls $?\"; (echo agent > '{through}/new.txt') 2>&1; \
         echo \"alias write $?\"; rm -f '{through}/secret.txt' 2>&1; echo \"alias rm $?\"; \
         mv '{through}' '{through}-moved' 2>&1; echo \"alias mv $?\"; \
         cat '{own}/secret.txt' 2>&1; echo \"own cat $?\"; \
         grep -F '{alias}' /proc/self/mountinfo; echo done",
        through = through.display(),
        own = case.private.display(),
        alias = alias.display()
    )
}

/// What the host shows of the hidden folder after a run: `host intact` when the secret
/// file holds its words and no try left a file or a moved folder, else what changed.
/// Input: the case. Output: the line.
fn host(case: &Case) -> String {
    let secret = fs::read_to_string(case.private.join("secret.txt")).ok();
    let made = case.private.join("new.txt").exists();
    let moved = case.private.with_file_name("private-moved").exists();
    if secret.as_deref() == Some(SECRET) && !made && !moved {
        return "host intact".to_owned();
    }
    format!(
        "host CHANGED: secret {:?}, new.txt {made}, moved {moved}",
        secret.is_some()
    )
}

/// Runs a script inside the sandbox from the project's worktrees folder. Inputs: the
/// sandbox, the case and the script. Output: its status, output and error output as text.
fn run(sandbox: &LinuxSandbox, case: &Case, script: &str) -> String {
    let mut harness = Command::new("/bin/sh");
    harness
        .arg("-c")
        .arg(script)
        .current_dir(case.project.join("worktrees"));
    let output = crazyagent::spawn_gate::output(
        sandbox
            .wrap(&harness)
            .unwrap()
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
    )
    .unwrap();
    format!(
        "status {:?}\n{}{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

/// Writes what one leg saw to the file `PROBE_OUT` names, when it is set. Inputs: the
/// probe, the leg and the text. Output: none.
fn record(probe: &str, leg: Leg, seen: &str) {
    let Ok(path) = std::env::var("PROBE_OUT") else {
        return;
    };
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap();
    writeln!(file, "### {probe} {leg:?}\n{seen}").unwrap();
}

/// Fails when any leg read the secret or changed the hidden folder on the host, naming
/// each that did. Inputs: the probe and what each leg saw. Output: none.
fn judge(probe: &str, seen: &[(Leg, String)]) {
    for (leg, text) in seen {
        record(probe, *leg, text);
    }
    let read: Vec<String> = seen
        .iter()
        .filter(|(_, text)| text.contains("secret words"))
        .map(|(leg, _)| format!("{leg:?}"))
        .collect();
    let changed: Vec<String> = seen
        .iter()
        .filter(|(_, text)| !text.contains("host intact"))
        .map(|(leg, _)| format!("{leg:?}"))
        .collect();
    assert!(
        read.is_empty() && changed.is_empty(),
        "{probe}: the secret was read on {read:?}, the hidden folder changed on {changed:?}"
    );
}

/// rv-code4 F1 probe: the case root bind-mounted at a second path outside every grant
/// before the agent starts. Fails when a leg reads the hidden file through that path.
#[test]
fn a_second_mount_made_before_launch_keeps_the_hidden_folder_out() {
    let seen: Vec<(Leg, String)> = LEGS
        .iter()
        .map(|&leg| {
            let case = case();
            let alias = tempfile::tempdir().unwrap();
            let alias_root = alias.path().canonicalize().unwrap();
            let _bound = Bound::new(&case.root, &alias_root);
            let sandbox = sandbox(leg, &case);
            let seen = run(&sandbox, &case, &tries(&case, &alias_root));
            (leg, format!("{seen}{}\n", host(&case)))
        })
        .collect();
    judge("second mount before launch", &seen);
}

/// rv-code4 F1 probe: the same bind mount made after the agent started, while it waits on
/// its input, to see whether the mount reaches the agent's namespace. Fails when a leg
/// reads the hidden file through that path.
#[test]
fn a_second_mount_made_after_launch_keeps_the_hidden_folder_out() {
    let seen: Vec<(Leg, String)> = LEGS
        .iter()
        .map(|&leg| {
            let case = case();
            let alias = tempfile::tempdir().unwrap();
            let alias_root = alias.path().canonicalize().unwrap();
            let sandbox = sandbox(leg, &case);
            let mut harness = Command::new("/bin/sh");
            harness
                .arg("-c")
                .arg(format!(
                    "echo ready; read go; {}",
                    tries(&case, &alias_root)
                ))
                .current_dir(case.project.join("worktrees"));
            let mut child = crazyagent::spawn_gate::spawn(
                sandbox
                    .wrap(&harness)
                    .unwrap()
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::null()),
            )
            .unwrap();
            let mut output = BufReader::new(child.stdout.take().unwrap());
            let mut line = String::new();
            output.read_line(&mut line).unwrap();
            assert_eq!(line, "ready\n", "{leg:?}");
            let _bound = Bound::new(&case.root, &alias_root);
            let mut input = child.stdin.take().unwrap();
            input.write_all(b"go\n").unwrap();
            drop(input);
            let mut rest = String::new();
            output.read_to_string(&mut rest).unwrap();
            let status = child.wait().unwrap();
            (leg, format!("status {status:?}\n{rest}{}\n", host(&case)))
        })
        .collect();
    judge("second mount after launch", &seen);
}

/// Probe beside rv-code4 F1: the case root bind-mounted inside the project, a granted root,
/// before the agent starts, to see whether either leg keeps the hidden folder out there.
/// Fails when a leg reads the hidden file through that path.
#[test]
fn a_mount_inside_the_project_made_before_launch_keeps_the_hidden_folder_out() {
    let seen: Vec<(Leg, String)> = LEGS
        .iter()
        .map(|&leg| {
            let case = case();
            let alias_root = case.project.join("worktrees").join("alias");
            fs::create_dir(&alias_root).unwrap();
            let _bound = Bound::new(&case.root, &alias_root);
            let sandbox = sandbox(leg, &case);
            let seen = run(&sandbox, &case, &tries(&case, &alias_root));
            (leg, format!("{seen}{}\n", host(&case)))
        })
        .collect();
    judge("mount inside the project before launch", &seen);
}
