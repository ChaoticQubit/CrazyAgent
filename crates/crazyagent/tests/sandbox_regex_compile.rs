#![cfg(target_os = "macos")]

use crazyagent::sandbox::{ProjectSandbox, SandboxNetwork};
use std::fs;
use std::os::macos::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const UF_IMMUTABLE: u32 = 0x2;

struct Case {
    temp: tempfile::TempDir,
    project: PathBuf,
    own: PathBuf,
    home: PathBuf,
    tmp: PathBuf,
    profiles: PathBuf,
}

impl Drop for Case {
    /// Clears every immutable flag a failed row may have left, so the temporary folder can
    /// be removed. Input: the case. Output: none.
    fn drop(&mut self) {
        let mut command = Command::new("/usr/bin/chflags");
        command
            .args(["-R", "nouchg"])
            .arg(self.temp.path())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let _ = crazyagent::spawn_gate::status(&mut command);
    }
}

fn case() -> Case {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let project = root.join("project");
    let own = project
        .join(".crazyagent")
        .join("worktrees")
        .join("agent-1");
    let home = root.join("home");
    let tmp = root.join("tmp");
    let profiles = root.join("profiles");
    for folder in [&own, &home, &tmp, &profiles] {
        fs::create_dir_all(folder).unwrap();
    }
    Case {
        temp,
        project,
        own,
        home,
        tmp,
        profiles,
    }
}

fn sandbox(case: &Case) -> ProjectSandbox {
    ProjectSandbox::new(
        &case.own,
        &case.home,
        &case.tmp,
        SandboxNetwork::Only(Vec::new()),
    )
    .unwrap()
    .allow_read(&case.project)
    .unwrap()
}

/// Runs one program under the installed profile and waits for it. Inputs: the sandbox, its
/// installed profile, the program and its arguments. Output: whether it exited 0, and the
/// process id `sandbox-exec` ran as, under which libsandbox logs a compile error.
fn run(sandbox: &ProjectSandbox, profile: &Path, program: &str, args: &[&str]) -> (bool, u32) {
    let mut command = sandbox.command(profile, Path::new(program));
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut child = crazyagent::spawn_gate::spawn(&mut command).unwrap();
    let pid = child.id();
    let status = child.wait().unwrap();
    (status.success(), pid)
}

/// The `sandbox compile:` errors libsandbox logged in the last three minutes in any of the
/// given processes. Input: the process ids. Output: the log lines.
fn compile_errors(pids: &[u32]) -> Vec<String> {
    assert!(!pids.is_empty());
    let processes = pids
        .iter()
        .map(|pid| format!("processID == {pid}"))
        .collect::<Vec<_>>()
        .join(" OR ");
    let predicate = format!(
        "senderImagePath CONTAINS \"libsandbox\" AND eventMessage CONTAINS \"sandbox compile\" AND ({processes})"
    );
    let mut command = Command::new("/usr/bin/log");
    command
        .args(["show", "--last", "3m", "--style", "compact", "--predicate"])
        .arg(&predicate)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let output = crazyagent::spawn_gate::output(&mut command).unwrap();
    assert!(output.status.success(), "{output:?}");
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| line.contains("sandbox compile"))
        .map(str::to_owned)
        .collect()
}

/// Starts `/usr/bin/true` under a profile holding regex shapes libsandbox logs a compile
/// error for, then waits at most 60 s until the unified log shows that error, so the errors
/// of every process started before it are in the log too. Input: a folder for the profile.
/// Output: none; it fails when the control error never shows, which means libsandbox no
/// longer logs those shapes and this check has nothing left to prove.
fn log_caught_up(folder: &Path) {
    let profile = folder.join("control.sb");
    fs::write(
        &profile,
        "(version 1)\n(allow default)\n(deny file-write-flags\n  (regex #\"^/private/tmp/ca-regex-control\\.d/x/(a|b)(/.*)?$\")\n  (regex #\"^/private/tmp/ca-regex-control\\.d/x/\\.(s|S|\u{17f})[yY](s|S|\u{17f})[tT][eE][mM](/.*)?$\"))\n",
    )
    .unwrap();
    let mut command = Command::new("/usr/bin/sandbox-exec");
    command
        .arg("-f")
        .arg(&profile)
        .arg("/usr/bin/true")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut child = crazyagent::spawn_gate::spawn(&mut command).unwrap();
    let pid = child.id();
    assert!(child.wait().unwrap().success());
    let deadline = Instant::now() + Duration::from_secs(60);
    while compile_errors(&[pid]).is_empty() {
        assert!(
            Instant::now() < deadline,
            "libsandbox never logged the control shapes' compile error"
        );
        std::thread::sleep(Duration::from_millis(500));
    }
}

/// SANDBOX-REGEX: the LL-2 deny on flags in Codex's `skills/.system` compiles with no
/// libsandbox error, and an agent still cannot set the immutable flag on `.system` or a file
/// in it under any spelling that reaches it: plain, upper, mixed, long s, Kelvin sign or the
/// Data volume path, while it can flag a plain file in its home.
#[test]
fn the_codex_system_skills_flag_deny_compiles_clean_and_holds_for_every_spelling() {
    let case = case();
    let system = case.home.join("skills").join(".system");
    fs::create_dir_all(&system).unwrap();
    let skill = system.join("SKILL.md");
    fs::write(&skill, "skill").unwrap();
    let own = case.home.join("own.txt");
    fs::write(&own, "own").unwrap();
    let sandbox = sandbox(&case);
    let profile = case.profiles.join("flags.sb");
    sandbox.install(&profile).unwrap();
    let home = case.home.display().to_string();
    let spellings = [
        format!("{home}/skills/.system"),
        format!("{home}/skills/.system/SKILL.md"),
        format!("{home}/SKILLS/.SYSTEM/SKILL.md"),
        format!("{home}/Skills/.System/SKILL.md"),
        format!("{home}/\u{17f}kills/.\u{17f}y\u{17f}tem/SKILL.md"),
        format!("{home}/s\u{212a}ills/.system/SKILL.md"),
        format!("/System/Volumes/Data{home}/skills/.system/SKILL.md"),
    ];
    let mut pids = Vec::new();
    for path in &spellings {
        assert!(Path::new(path).exists(), "{path}");
        let (flagged, pid) = run(&sandbox, &profile, "/usr/bin/chflags", &["uchg", path]);
        pids.push(pid);
        assert!(!flagged, "{path}");
        assert_eq!(fs::metadata(&skill).unwrap().st_flags() & UF_IMMUTABLE, 0, "{path}");
        assert_eq!(fs::metadata(&system).unwrap().st_flags() & UF_IMMUTABLE, 0, "{path}");
    }
    let (flagged, pid) = run(
        &sandbox,
        &profile,
        "/usr/bin/chflags",
        &["uchg", own.to_str().unwrap()],
    );
    pids.push(pid);
    assert!(flagged);
    assert_eq!(
        fs::metadata(&own).unwrap().st_flags() & UF_IMMUTABLE,
        UF_IMMUTABLE
    );
    log_caught_up(&case.profiles);
    let errors = compile_errors(&pids);
    assert!(errors.is_empty(), "{errors:#?}");
}

/// SANDBOX-REGEX: the rule that keeps every top-level entry of the project closed but the
/// open names compiles with no libsandbox error. An open name reads in any letter case;
/// a closed one, in any case, one whose name only starts with an open name and one made
/// after the profile was installed stay closed though each path resolves outside the sandbox.
#[test]
fn the_closed_tops_rule_compiles_clean_and_keeps_only_the_open_names_readable() {
    let case = case();
    fs::create_dir_all(case.project.join("notes")).unwrap();
    fs::write(case.project.join("notes").join("plan.txt"), "plan").unwrap();
    fs::write(case.project.join("README.md"), "readme").unwrap();
    fs::create_dir_all(case.project.join("drafts")).unwrap();
    fs::write(case.project.join("drafts").join("draft.txt"), "draft").unwrap();
    fs::create_dir_all(case.project.join("notes2")).unwrap();
    fs::write(case.project.join("notes2").join("x.txt"), "near").unwrap();
    fs::write(case.project.join("README.md.bak"), "near").unwrap();
    let sandbox = sandbox(&case)
        .keep_project_secrets(&case.project, &[])
        .unwrap()
        .open_only_tops(&["notes".into(), "README.md".into()])
        .unwrap();
    let profile = case.profiles.join("tops.sb");
    sandbox.install(&profile).unwrap();
    fs::write(case.project.join("later.txt"), "later").unwrap();
    let mut pids = Vec::new();
    let mut read = |relative: &str| {
        let path = case.project.join(relative);
        assert!(path.exists(), "{relative}");
        let (read, pid) = run(&sandbox, &profile, "/bin/cat", &[path.to_str().unwrap()]);
        pids.push(pid);
        read
    };
    for open in ["notes/plan.txt", "NOTES/plan.txt", "README.md", "readme.md"] {
        assert!(read(open), "{open}");
    }
    for closed in [
        "drafts/draft.txt",
        "DRAFTS/draft.txt",
        "notes2/x.txt",
        "README.md.bak",
        "later.txt",
    ] {
        assert!(!read(closed), "{closed}");
    }
    log_caught_up(&case.profiles);
    let errors = compile_errors(&pids);
    assert!(errors.is_empty(), "{errors:#?}");
}
