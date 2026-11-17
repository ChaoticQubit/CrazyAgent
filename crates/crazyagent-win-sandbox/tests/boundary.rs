#![cfg(windows)]

use crazyagent_win_sandbox::profile::{BoundaryProfile, Guard, Guarded, HEADER, check_path};
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
    grants: PathBuf,
    profile: PathBuf,
    base: BoundaryProfile,
}

fn plain(path: &Path) -> String {
    let text = path.to_str().unwrap().to_owned();
    assert_eq!(check_path(&text), Ok(()), "{text}");
    text
}

fn system(program: &str) -> String {
    let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
    format!(r"{root}\System32\{program}")
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
    let grants = root.join("grants");
    for folder in [
        &project,
        &project.join("context"),
        &home,
        &tmp,
        &owner.join(".ssh"),
        &outside,
        &profiles,
        &grants,
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
        grants: plain(&grants),
    };
    let profile_path = profiles.join("agent.profile");
    fs::write(&profile_path, profile.encode().unwrap()).unwrap();
    Case {
        _temp: temp,
        project,
        home,
        outside,
        secret,
        grants,
        profile: profile_path,
        base: profile,
    }
}

fn install(case: &Case, profile: &BoundaryProfile) {
    fs::write(&case.profile, profile.encode().unwrap()).unwrap();
}

fn read_sddl(path: &Path) -> String {
    let read = Command::new(system(r"WindowsPowerShell\v1.0\powershell.exe"))
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "(Get-Acl -LiteralPath $env:CRAZYAGENT_TEST_ACL_PATH).Sddl",
        ])
        .env("CRAZYAGENT_TEST_ACL_PATH", path)
        .output();
    match read {
        Ok(output) => format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout).trim(),
            String::from_utf8_lossy(&output.stderr).trim()
        ),
        Err(error) => format!("powershell failed: {error}"),
    }
}

fn sddl(paths: &[&Path]) -> String {
    paths
        .iter()
        .map(|path| format!("{} => {}", path.display(), read_sddl(path)))
        .collect::<Vec<_>>()
        .join("\n")
}

/// A path's security descriptor in SDDL with only the access list's auto-inherited flag (AI)
/// left out, so two lists compare equal when their owner, group, protection and every entry
/// with its flags and order match (DEC-130 option b). Input: the path. Output: the text.
fn entries(path: &Path) -> String {
    let text = read_sddl(path);
    let Some(start) = text.find("D:") else {
        return text;
    };
    let flags_end = text[start..].find('(').map_or(text.len(), |at| start + at);
    format!(
        "{}{}{}",
        &text[..start + 2],
        text[start + 2..flags_end].replace("AI", ""),
        &text[flags_end..]
    )
}

/// The SDDL text with every entry naming `sid` removed. Inputs: the text and the SID. Output:
/// the text without those entries.
fn without(sddl: &str, sid: &str) -> String {
    let mut kept = String::new();
    let mut rest = sddl;
    while let Some(open) = rest.find('(') {
        let close = open + rest[open..].find(')').expect(sddl);
        kept.push_str(&rest[..open]);
        if !rest[open..=close].contains(sid) {
            kept.push_str(&rest[open..=close]);
        }
        rest = &rest[close + 1..];
    }
    kept.push_str(rest);
    kept
}

/// The container SID on the entry the boundary put on the project folder. Input: the case,
/// launched at least once. Output: the SID text.
fn container_sid(case: &Case) -> String {
    let text = read_sddl(&case.project);
    let start = text.find("S-1-15-2-").expect(&text);
    let sid: String = text[start..]
        .chars()
        .take_while(|unit| unit.is_ascii_alphanumeric() || *unit == '-')
        .collect();
    assert!(sid.len() > "S-1-15-2-".len(), "{text}");
    sid
}

fn ran(output: &Output) -> String {
    format!(
        "exit {:?} stdout {:?} stderr {:?}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn agent(profile: &Path, cwd: &Path, program: &str) -> Command {
    let mut command = Command::new(HELPER);
    command
        .arg("--profile")
        .arg(profile)
        .arg("--")
        .arg(program)
        .current_dir(cwd);
    command
}

fn boundary(case: &Case) -> Command {
    agent(&case.profile, &case.project, &system("cmd.exe"))
}

fn output(mut command: Command, args: &[&str]) -> Output {
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .unwrap()
}

fn run(case: &Case, args: &[&str]) -> Output {
    output(boundary(case), args)
}

fn exe(case: &Case, program: &str, args: &[&str]) -> Output {
    output(agent(&case.profile, &case.project, &system(program)), args)
}

const CALL: &str = "CRAZYAGENT_FILE_CALL";
const CHILD: [&str; 5] = [
    "file_call_child",
    "--exact",
    "--nocapture",
    "--test-threads",
    "1",
];

/// Runs one file call through the Windows API inside the case's container: this test binary,
/// copied into the agent's home, started through the helper as [`file_call_child`]. `cmd`'s
/// `del`, `ren` and `move` fail in any AppContainer (DEC-193 step 0c, B1), so a delete or
/// rename the boundary allows or refuses is only seen through the API. Inputs: the case and
/// the call, such as `["rename", from, to]`. Output: what the child printed and its exit.
fn call(case: &Case, step: &[&str]) -> Output {
    assert!(!step.is_empty() && step.len() <= 3);
    let probe = case.home.join("file-call.exe");
    if !probe.is_file() {
        fs::copy(std::env::current_exe().unwrap(), &probe).unwrap();
    }
    let mut command = agent(&case.profile, &case.project, &plain(&probe));
    command.env(CALL, step.join("|"));
    output(command, &CHILD)
}

/// The child [`call`] starts: makes the one call named in its environment and exits 1 when
/// Windows refuses it; without that variable, as in a normal test run, it does nothing.
/// Calls: `delete`, `rename` (replacing the target), `rmdir`, `rmtree` and `mkdir`.
#[test]
fn file_call_child() {
    let Ok(line) = std::env::var(CALL) else {
        return;
    };
    let parts: Vec<&str> = line.split('|').collect();
    let done = match parts.as_slice() {
        ["delete", path] => fs::remove_file(path),
        ["rename", from, to] => fs::rename(from, to),
        ["rmdir", path] => fs::remove_dir(path),
        ["rmtree", path] => fs::remove_dir_all(path),
        ["mkdir", path] => fs::create_dir(path),
        _ => panic!("unknown file call {line}"),
    };
    if let Err(error) = done {
        println!("CALL {line} => {error}");
        std::process::exit(1);
    }
    println!("CALL {line} => done");
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
    let context = case.project.join("context");
    let temp = case.project.parent().unwrap();
    let watched = [
        temp.parent().unwrap(),
        temp,
        case.project.as_path(),
        context.as_path(),
        brief.as_path(),
    ];
    let before = sddl(&watched);
    run(&case, &["/C", "echo", "inside>", &plain(&inside)]);
    let granted = sddl(&watched);
    run(&case, &["/C", "echo", "home>", &plain(&home)]);
    run(&case, &["/C", "echo", "escape>", &plain(&escape)]);
    let changed = run(&case, &["/C", "echo", "changed>", &plain(&brief)]);
    let deleted = call(&case, &["delete", &plain(&brief)]);
    assert!(fs::read_to_string(&inside).unwrap().starts_with("inside"));
    assert!(fs::read_to_string(&home).unwrap().starts_with("home"));
    assert!(!escape.exists());
    assert_eq!(
        fs::read_to_string(&brief).unwrap(),
        "brief\r\n",
        "write: {}\ndelete: {}\nbefore:\n{before}\nafter the first launch:\n{granted}\nafter:\n{}",
        ran(&changed),
        ran(&deleted),
        sddl(&watched)
    );
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
        format!("{HEADER}\ncontainer crazyagent.test.refuse\n"),
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

#[test]
fn a_narrower_profile_takes_back_what_the_one_before_granted() {
    let case = case("crazyagent.test.contract");
    let before = entries(&case.outside);
    let temp = case.outside.parent().unwrap();
    let watched = [temp.parent().unwrap(), temp, case.outside.as_path()];
    let before_sddl = sddl(&watched);
    let first = case.outside.join("first.txt");
    let mut wide = case.base.clone();
    wide.write_roots.push(plain(&case.outside));
    install(&case, &wide);
    run(&case, &["/C", "echo", "first>", &plain(&first)]);
    assert!(
        fs::read_to_string(&first).unwrap().starts_with("first"),
        "the wide profile lets the agent write there"
    );
    assert_ne!(entries(&case.outside), before);

    let mut narrowed = case.base.clone();
    narrowed.read_roots.push(plain(&case.outside));
    install(&case, &narrowed);
    let read = run(&case, &["/C", "type", &plain(&first)]);
    assert!(
        read.status.success(),
        "{}",
        String::from_utf8_lossy(&read.stderr)
    );
    assert!(String::from_utf8_lossy(&read.stdout).contains("first"));
    let second = case.outside.join("second.txt");
    run(&case, &["/C", "echo", "second>", &plain(&second)]);
    call(&case, &["delete", &plain(&first)]);
    assert!(
        !second.exists(),
        "modify narrowed to read and run takes back writes"
    );
    assert!(first.is_file(), "and deletes");

    install(&case, &case.base);
    let hidden = run(&case, &["/C", "type", &plain(&first)]);
    assert!(!hidden.status.success());
    assert!(!String::from_utf8_lossy(&hidden.stdout).contains("first"));
    assert_eq!(
        entries(&case.outside),
        before,
        "a root the profile no longer names keeps no entry of the container\nbefore:\n{before_sddl}\nafter:\n{}",
        sddl(&watched)
    );
}

#[test]
fn releasing_a_profile_takes_back_every_grant_and_its_record() {
    let case = case("crazyagent.test.release");
    let context = case.project.join("context");
    let before = [&case.project, &context, &case.home].map(|path| entries(path));
    let temp = case.project.parent().unwrap();
    let watched = [
        temp.parent().unwrap(),
        temp,
        case.project.as_path(),
        context.as_path(),
        case.home.as_path(),
    ];
    let before_sddl = sddl(&watched);
    let readme = run(
        &case,
        &["/C", "type", &plain(&case.project.join("readme.txt"))],
    );
    assert!(
        readme.status.success(),
        "{}",
        String::from_utf8_lossy(&readme.stderr)
    );
    let granted = [&case.project, &context, &case.home].map(|path| entries(path));
    for (granted, before) in granted.iter().zip(&before) {
        assert_ne!(granted, before);
    }
    let record = case.grants.join("container-crazyagent.test.release.grants");
    assert!(record.is_file());
    let granted_sddl = sddl(&watched);
    crazyagent_win_sandbox::release(&case.profile).unwrap();
    assert_eq!(
        [&case.project, &context, &case.home].map(|path| entries(path)),
        before,
        "every root and protected path is back to the access list it had\nbefore:\n{before_sddl}\ngranted:\n{granted_sddl}\nafter:\n{}",
        sddl(&watched)
    );
    assert!(!record.exists());
    crazyagent_win_sandbox::release(&case.profile).unwrap();
}

const REPO_FILES: [(&str, &str); 9] = [
    (r".git\HEAD", "ref: refs/heads/main\n"),
    (r".git\config", "[core]\n\tbare = false\n"),
    (r".git\index", "index\n"),
    (r".git\hooks\pre-commit.sample", "exit 0\n"),
    (r".git\refs\heads\main", "main\n"),
    (r".git\logs\HEAD", "log\n"),
    (r".crazyagent\context\memo.md", "memo\n"),
    (r".crazyagent\instructions.md", "instructions\n"),
    (r".vault\key.txt", "VAULT KEY MATERIAL\n"),
];

const REPO_GUARDS: [(&str, Guard); 6] = [
    (r".crazyagent\context", Guard::ReadOnly),
    (r".crazyagent\instructions.md", Guard::ReadOnly),
    (r".git\hooks", Guard::ReadOnly),
    (r".git\config", Guard::ReadOnly),
    (r".git\modules", Guard::ReadOnly),
    (".vault", Guard::Hidden),
];

fn at(case: &Case, relative: &str) -> PathBuf {
    relative
        .split('\\')
        .fold(case.project.clone(), |path, part| path.join(part))
}

/// A case whose project is a git work tree guarded the way the engine guards one on Windows:
/// `context`, `.crazyagent\context`, `.crazyagent\instructions.md`, `.git\hooks`,
/// `.git\config` and `.git\modules` read-only, `.git\modules` present as the engine makes it
/// before each launch, plus a hidden `.vault`. Inputs: the container name and the project
/// paths to remove before the first launch. Output: the case with that profile installed.
fn repo(container: &str, missing: &[&str]) -> Case {
    let mut case = case(container);
    for folder in [
        r".git\hooks",
        r".git\modules",
        r".git\objects\pack",
        r".git\refs\heads",
        r".git\logs",
        r".crazyagent\context",
        ".vault",
    ] {
        fs::create_dir_all(at(&case, folder)).unwrap();
    }
    for (file, text) in REPO_FILES {
        fs::write(at(&case, file), text).unwrap();
    }
    for relative in missing {
        let path = at(&case, relative);
        if path.is_dir() {
            fs::remove_dir_all(&path).unwrap();
        } else {
            fs::remove_file(&path).unwrap();
        }
    }
    for (relative, guard) in REPO_GUARDS {
        let path = plain(&at(&case, relative));
        case.base.guarded.push(Guarded { path, guard });
    }
    install(&case, &case.base);
    case
}

/// Every boundary case one row expects, kept so a row reports all of them at once with what
/// the agent's command printed.
struct Checks(Vec<String>);

impl Checks {
    fn expect(&mut self, holds: bool, what: &str, output: &Output) {
        if !holds {
            self.0.push(format!("{what}: {}", ran(output)));
        }
    }

    fn done(self, watched: &[&Path]) {
        assert!(
            self.0.is_empty(),
            "{}\naccess lists now:\n{}",
            self.0.join("\n"),
            sddl(watched)
        );
    }
}

fn reads(path: &Path, text: &str) -> bool {
    fs::read_to_string(path).is_ok_and(|now| now == text)
}

fn starts(path: &Path, text: &str) -> bool {
    fs::read_to_string(path).is_ok_and(|now| now.starts_with(text))
}

#[test]
fn read_only_paths_refuse_every_change_while_the_project_stays_writable() {
    let case = repo("crazyagent.test.readonly", &[]);
    let path = |relative: &str| at(&case, relative);
    let mut checks = Checks(Vec::new());
    let note = path("note.txt");
    let made = run(&case, &["/C", "echo", "note>", &plain(&note)]);
    checks.expect(
        starts(&note, "note"),
        "a new project file is written",
        &made,
    );
    for (relative, words) in [
        (r"context\brief.md", ["echo", "changed>"]),
        (r".git\config", ["echo", "changed>>"]),
        (r".crazyagent\instructions.md", ["echo", "changed>"]),
        (r".crazyagent\context\memo.md", ["echo", "changed>"]),
    ] {
        let target = path(relative);
        let before = fs::read_to_string(&target).unwrap();
        let output = run(&case, &["/C", words[0], words[1], &plain(&target)]);
        checks.expect(
            reads(&target, &before),
            &format!("{relative} stays unchanged"),
            &output,
        );
    }
    for relative in [r".git\hooks\post-checkout", r".crazyagent\context\new.md"] {
        let target = path(relative);
        let output = run(&case, &["/C", "echo", "new>", &plain(&target)]);
        checks.expect(
            !target.exists(),
            &format!("{relative} is not created"),
            &output,
        );
    }
    let sample = path(r".git\hooks\pre-commit.sample");
    let deleted = call(&case, &["delete", &plain(&sample)]);
    checks.expect(sample.is_file(), "a hook is not deleted", &deleted);
    let config = path(r".git\config");
    let lock = path(r".git\config.lock");
    let made = run(&case, &["/C", "echo", "planted>", &plain(&lock)]);
    checks.expect(starts(&lock, "planted"), "config.lock is made", &made);
    let replaced = call(&case, &["rename", &plain(&lock), &plain(&config)]);
    checks.expect(
        reads(&config, "[core]\n\tbare = false\n"),
        "config.lock is not renamed over config",
        &replaced,
    );
    let removed = call(&case, &["delete", &plain(&config)]);
    checks.expect(config.is_file(), "config is not deleted", &removed);
    for relative in [r".git\hooks", "context", r".crazyagent\context"] {
        let folder = path(relative);
        let moved = plain(&folder.with_file_name("moved"));
        let output = call(&case, &["rename", &plain(&folder), &moved]);
        checks.expect(
            folder.is_dir() && !folder.with_file_name("moved").exists(),
            &format!("{relative} is not renamed"),
            &output,
        );
    }
    let watched = [
        path(".git"),
        path(r".git\hooks"),
        path(r".git\config"),
        path("context"),
        path(r".crazyagent\context"),
        path(r".crazyagent\instructions.md"),
    ];
    checks.done(&watched.each_ref().map(PathBuf::as_path));
}

#[test]
fn anchored_folders_stay_in_place_while_git_writes_its_own_files() {
    let case = repo("crazyagent.test.anchor", &[]);
    let path = |relative: &str| at(&case, relative);
    let mut checks = Checks(Vec::new());
    for (name, mark) in [
        (r".git\index", "staged"),
        (r".git\HEAD", "moved"),
        (r".git\refs\heads\main", "next"),
    ] {
        let target = path(name);
        let lock = target.with_file_name(format!(
            "{}.lock",
            target.file_name().unwrap().to_str().unwrap()
        ));
        let made = run(&case, &["/C", "echo", &format!("{mark}>"), &plain(&lock)]);
        checks.expect(starts(&lock, mark), &format!("{name}.lock is made"), &made);
        let moved = call(&case, &["rename", &plain(&lock), &plain(&target)]);
        checks.expect(
            starts(&target, mark) && !lock.exists(),
            &format!("{name}.lock is renamed over {name}"),
            &moved,
        );
    }
    let objects = path(r".git\objects\ab");
    let folder = run(&case, &["/C", "mkdir", &plain(&objects)]);
    checks.expect(objects.is_dir(), "an object folder is made", &folder);
    let object = objects.join("cd");
    let written = run(&case, &["/C", "echo", "object>", &plain(&object)]);
    checks.expect(starts(&object, "object"), "an object is written", &written);
    let log = path(r".git\logs\HEAD");
    let appended = run(&case, &["/C", "echo", "entry>>", &plain(&log)]);
    checks.expect(
        fs::read_to_string(&log).is_ok_and(|text| text.contains("entry")),
        "the reflog is appended",
        &appended,
    );
    let rebase = path(r".git\rebase-merge");
    let step = rebase.join("done");
    let made = run(&case, &["/C", "mkdir", &plain(&rebase)]);
    checks.expect(rebase.is_dir(), "a rebase folder is made", &made);
    let stepped = run(&case, &["/C", "echo", "step>", &plain(&step)]);
    checks.expect(starts(&step, "step"), "a rebase step is written", &stepped);
    let done = call(&case, &["delete", &plain(&step)]);
    checks.expect(!step.exists(), "a rebase step is deleted", &done);
    let removed = call(&case, &["rmdir", &plain(&rebase)]);
    checks.expect(!rebase.exists(), "the rebase folder is removed", &removed);
    let hooks = path(r".git\hooks");
    let moved = call(&case, &["rename", &plain(&hooks), &plain(&path("hooks"))]);
    checks.expect(hooks.is_dir(), "the hooks folder is not moved out", &moved);
    for (relative, name) in [(".git", "git-moved"), (".crazyagent", "crazyagent-moved")] {
        let folder = path(relative);
        let output = call(&case, &["rename", &plain(&folder), &plain(&path(name))]);
        checks.expect(
            folder.is_dir() && !path(name).exists(),
            &format!("{relative} is not renamed"),
            &output,
        );
    }
    let watched = [case.project.clone(), path(".git"), path(".crazyagent")];
    checks.done(&watched.each_ref().map(PathBuf::as_path));
}

#[test]
fn a_missing_protected_path_cannot_be_made() {
    let case = repo(
        "crazyagent.test.missing",
        &[
            r".git\hooks",
            r".git\modules",
            r".crazyagent\instructions.md",
        ],
    );
    let path = |relative: &str| at(&case, relative);
    let mut checks = Checks(Vec::new());
    for relative in [r".git\hooks", r".git\modules"] {
        let folder = path(relative);
        let output = run(&case, &["/C", "mkdir", &plain(&folder)]);
        checks.expect(
            !folder.exists(),
            &format!("{relative} is not made"),
            &output,
        );
    }
    let instructions = path(r".crazyagent\instructions.md");
    let written = run(&case, &["/C", "echo", "planted>", &plain(&instructions)]);
    checks.expect(
        !starts(&instructions, "planted"),
        "instructions.md is not planted",
        &written,
    );
    let lock = path(r".git\index.lock");
    let made = run(&case, &["/C", "echo", "staged>", &plain(&lock)]);
    checks.expect(
        !lock.exists(),
        "no entry is added to .git, the cost the engine's pre-create avoids",
        &made,
    );
    let log = path(r".git\logs\HEAD");
    let appended = run(&case, &["/C", "echo", "entry>>", &plain(&log)]);
    checks.expect(
        fs::read_to_string(&log).is_ok_and(|text| text.contains("entry")),
        "files already in .git are still written",
        &appended,
    );
    let objects = path(r".git\objects\ab");
    let folder = call(&case, &["mkdir", &plain(&objects)]);
    checks.expect(objects.is_dir(), "work below .git goes on", &folder);
    let watched = [path(".git"), path(".crazyagent")];
    checks.done(&watched.each_ref().map(PathBuf::as_path));
}

#[test]
fn a_hidden_folder_inside_the_project_is_neither_read_nor_listed() {
    let case = repo("crazyagent.test.hidden", &[]);
    let path = |relative: &str| at(&case, relative);
    let mut checks = Checks(Vec::new());
    let readme = run(&case, &["/C", "type", &plain(&path("readme.txt"))]);
    checks.expect(
        String::from_utf8_lossy(&readme.stdout).contains("project readme"),
        "the project readme is read",
        &readme,
    );
    let key = path(r".vault\key.txt");
    let leaked = |output: &Output| {
        String::from_utf8_lossy(&output.stdout).contains("VAULT KEY")
            || String::from_utf8_lossy(&output.stderr).contains("VAULT KEY")
    };
    let read = run(&case, &["/C", "type", &plain(&key)]);
    checks.expect(
        !read.status.success() && !leaked(&read),
        "the hidden key is not read",
        &read,
    );
    let listed = run(&case, &["/C", "dir", "/B", &plain(&path(".vault"))]);
    checks.expect(
        !String::from_utf8_lossy(&listed.stdout).contains("key.txt"),
        "the hidden folder is not listed",
        &listed,
    );
    let copy = path("copy.txt");
    let copied = run(&case, &["/C", "copy", &plain(&key), &plain(&copy)]);
    checks.expect(
        !fs::read_to_string(&copy).is_ok_and(|text| text.contains("VAULT KEY")),
        "the hidden key is not copied out",
        &copied,
    );
    checks.done(&[&path(".vault"), &key]);
}

#[test]
fn an_agent_cannot_change_an_access_list_or_take_ownership() {
    let case = repo("crazyagent.test.acl", &[]);
    let path = |relative: &str| at(&case, relative);
    run(&case, &["/C", "echo", "first>", &plain(&path("first.txt"))]);
    let sid = container_sid(&case);
    let hooks = path(r".git\hooks");
    let config = path(r".git\config");
    let context = path("context");
    let guarded = [&hooks, &config, &context];
    let before = guarded.map(|path| entries(path));
    let mut checks = Checks(Vec::new());
    let shown = exe(&case, "icacls.exe", &[&plain(&hooks)]);
    checks.expect(
        shown.status.success(),
        "icacls runs in the container and reads an access list",
        &shown,
    );
    let all = format!("*{sid}:(OI)(CI)(F)");
    let one = format!("*{sid}:(F)");
    let (hooks_text, config_text, context_text) = (plain(&hooks), plain(&config), plain(&context));
    let attempts: [(&str, &[&str]); 6] = [
        ("icacls.exe", &[&hooks_text, "/grant", &all]),
        ("icacls.exe", &[&hooks_text, "/inheritance:e"]),
        ("icacls.exe", &[&config_text, "/grant", &one]),
        ("icacls.exe", &[&config_text, "/reset"]),
        ("takeown.exe", &["/F", &config_text]),
        ("icacls.exe", &[&context_text, "/grant", &all]),
    ];
    for (program, args) in attempts {
        let output = exe(&case, program, args);
        checks.expect(
            guarded.map(|path| entries(path)) == before,
            &format!("{program} {args:?} changes no access list"),
            &output,
        );
    }
    let hook = hooks.join("post-checkout");
    let planted = run(&case, &["/C", "echo", "hook>", &plain(&hook)]);
    checks.expect(!hook.exists(), "no hook is created afterwards", &planted);
    let appended = run(&case, &["/C", "echo", "changed>>", &plain(&config)]);
    checks.expect(
        reads(&config, "[core]\n\tbare = false\n"),
        "config stays unchanged afterwards",
        &appended,
    );
    let brief = context.join("brief.md");
    let changed = run(&case, &["/C", "echo", "changed>", &plain(&brief)]);
    checks.expect(
        reads(&brief, "brief\r\n"),
        "the brief stays unchanged afterwards",
        &changed,
    );
    checks.done(&guarded.map(PathBuf::as_path));
}

#[test]
fn two_agents_on_one_project_keep_each_others_guards() {
    let case = repo("crazyagent.test.first", &[]);
    let path = |relative: &str| at(&case, relative);
    let watched = [
        case.project.clone(),
        path(".git"),
        path(r".git\hooks"),
        path(r".git\config"),
        path("context"),
        path(".crazyagent"),
        path(r".crazyagent\context"),
        path(r".crazyagent\instructions.md"),
        path(".vault"),
    ];
    let before = watched.each_ref().map(|path| entries(path));
    let mut second = case.base.clone();
    second.container = "crazyagent.test.second".into();
    let second_profile = case.profile.with_file_name("second.profile");
    fs::write(&second_profile, second.encode().unwrap()).unwrap();
    let as_second = |args: &[&str]| {
        output(
            agent(&second_profile, &case.project, &system("cmd.exe")),
            args,
        )
    };
    let mut checks = Checks(Vec::new());
    let first = run(&case, &["/C", "echo", "a>", &plain(&path("a.txt"))]);
    checks.expect(
        starts(&path("a.txt"), "a"),
        "the first agent writes",
        &first,
    );
    let second_run = as_second(&["/C", "echo", "b>", &plain(&path("b.txt"))]);
    checks.expect(
        starts(&path("b.txt"), "b"),
        "the second agent writes",
        &second_run,
    );
    crazyagent_win_sandbox::release(&case.profile).unwrap();
    let hook = path(r".git\hooks\post-checkout");
    let planted = as_second(&["/C", "echo", "hook>", &plain(&hook)]);
    checks.expect(
        !hook.exists(),
        "the second agent still cannot create a hook after the first is released",
        &planted,
    );
    let config = path(r".git\config");
    let appended = as_second(&["/C", "echo", "changed>>", &plain(&config)]);
    checks.expect(
        reads(&config, "[core]\n\tbare = false\n"),
        "the second agent still cannot change config",
        &appended,
    );
    let still = as_second(&["/C", "echo", "c>", &plain(&path("c.txt"))]);
    checks.expect(
        starts(&path("c.txt"), "c"),
        "the second agent still writes its project",
        &still,
    );
    crazyagent_win_sandbox::release(&second_profile).unwrap();
    let after = watched.each_ref().map(|path| entries(path));
    if after != before {
        checks.0.push(format!(
            "every access list is back to before once both are released\nbefore:\n{}\nafter:\n{}",
            before.join("\n"),
            after.join("\n")
        ));
    }
    checks.done(&watched.each_ref().map(PathBuf::as_path));
}

#[test]
fn an_agent_works_normally_in_its_project_and_home() {
    let case = repo("crazyagent.test.work", &[]);
    let path = |relative: &str| at(&case, relative);
    let mut checks = Checks(Vec::new());
    let deep = path(r"build\out\deep");
    let made = run(&case, &["/C", "mkdir", &plain(&deep)]);
    checks.expect(deep.is_dir(), "a nested build folder is made", &made);
    let artifact = deep.join("o.txt");
    let written = run(&case, &["/C", "echo", "built>", &plain(&artifact)]);
    checks.expect(
        starts(&artifact, "built"),
        "a build file is written",
        &written,
    );
    let renamed = call(
        &case,
        &[
            "rename",
            &plain(&path(r"build\out")),
            &plain(&path(r"build\out-moved")),
        ],
    );
    checks.expect(
        path(r"build\out-moved\deep\o.txt").is_file(),
        "a build folder is renamed",
        &renamed,
    );
    let (old, new) = (path("old.txt"), path("new.txt"));
    run(&case, &["/C", "echo", "old>", &plain(&old)]);
    run(&case, &["/C", "echo", "new>", &plain(&new)]);
    let replaced = call(&case, &["rename", &plain(&new), &plain(&old)]);
    checks.expect(
        starts(&old, "new") && !new.exists(),
        "a file is renamed over another",
        &replaced,
    );
    let deleted = call(&case, &["delete", &plain(&old)]);
    checks.expect(!old.exists(), "a file is deleted", &deleted);
    let removed = call(&case, &["rmtree", &plain(&path("build"))]);
    checks.expect(!path("build").exists(), "a build tree is removed", &removed);
    let home = case.home.join("notes.txt");
    let noted = run(&case, &["/C", "echo", "home>", &plain(&home)]);
    checks.expect(starts(&home, "home"), "the agent home is written", &noted);
    checks.done(&[&case.project, &case.home]);
}

#[test]
fn releasing_restores_every_guarded_path_the_owner_left_alone() {
    let case = repo("crazyagent.test.restore", &[]);
    let path = |relative: &str| at(&case, relative);
    let watched = [
        case.project.clone(),
        path(".git"),
        path(r".git\hooks"),
        path(r".git\hooks\pre-commit.sample"),
        path(r".git\config"),
        path("context"),
        path(r"context\brief.md"),
        path(".crazyagent"),
        path(r".crazyagent\context"),
        path(r".crazyagent\instructions.md"),
        path(".vault"),
        case.home.clone(),
    ];
    let before = watched.each_ref().map(|path| entries(path));
    let readme = run(&case, &["/C", "type", &plain(&path("readme.txt"))]);
    assert!(readme.status.success(), "{}", ran(&readme));
    let granted = watched.each_ref().map(|path| entries(path));
    assert_ne!(granted, before);
    crazyagent_win_sandbox::release(&case.profile).unwrap();
    let after = watched.each_ref().map(|path| entries(path));
    assert_eq!(
        after,
        before,
        "every guarded path is back to the access list it had\ngranted:\n{}",
        granted.join("\n")
    );
    let record = case.grants.join("container-crazyagent.test.restore.grants");
    assert!(!record.exists());
}

#[test]
fn releasing_keeps_an_access_list_the_owner_changed_and_drops_only_the_agents_entries() {
    let case = repo("crazyagent.test.owner", &[]);
    let path = |relative: &str| at(&case, relative);
    let config = path(r".git\config");
    let config_before = entries(&config);
    run(&case, &["/C", "echo", "first>", &plain(&path("first.txt"))]);
    let sid = container_sid(&case);
    let hooks = path(r".git\hooks");
    let granted = Command::new(system("icacls.exe"))
        .args([plain(&hooks).as_str(), "/grant", "*S-1-5-19:(OI)(CI)(RX)"])
        .output()
        .unwrap();
    assert!(granted.status.success(), "{}", ran(&granted));
    let changed = entries(&hooks);
    assert!(changed.contains(";;;LS)"), "{changed}");
    crazyagent_win_sandbox::release(&case.profile).unwrap();
    let after = entries(&hooks);
    assert_eq!(
        after,
        without(&changed, &sid),
        "the owner's entry stays and only the agent's entries go\nchanged:\n{changed}"
    );
    fs::write(hooks.join("owner.sample"), "owner\n").unwrap();
    assert_eq!(
        entries(&config),
        config_before,
        "a guarded path the owner left alone is restored"
    );
}
