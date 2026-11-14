use super::{ProjectSandbox, SandboxNetwork, any_case, sbpl_overlong, seatbelt_pattern};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};

/// One private temp root holding a project, the agent's home and temp folders, a folder
/// standing in for the owner's temp folder and the profiles.
struct Case {
    _temp: tempfile::TempDir,
    project: PathBuf,
    home: PathBuf,
    tmp: PathBuf,
    owner_tmp: PathBuf,
    profiles: PathBuf,
}

/// Makes the case folders. Output: the case.
fn case() -> Case {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let case = Case {
        project: root.join("project"),
        home: root.join("agent").join("home"),
        tmp: root.join("agent").join("tmp"),
        owner_tmp: root.join("otmp"),
        profiles: root.join("profiles"),
        _temp: temp,
    };
    for folder in [
        &case.project,
        &case.home,
        &case.tmp,
        &case.owner_tmp,
        &case.profiles,
    ] {
        fs::create_dir_all(folder).unwrap();
    }
    case
}

/// The project-root sandbox of the case, as the IDE server and IDE shell build it.
fn sandbox(case: &Case) -> ProjectSandbox {
    ProjectSandbox::new(
        &case.project,
        &case.home,
        &case.tmp,
        SandboxNetwork::Only(Vec::new()),
    )
    .unwrap()
}

/// Runs the owner's own git in `folder` with no system or global config. Inputs: the
/// folder, the home and the arguments. Output: the finished process.
fn owner_git(folder: &Path, home: &Path, arguments: &[&str]) -> Output {
    crate::spawn_gate::output(
        std::process::Command::new("/usr/bin/git")
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .args(["-c", "user.email=owner@local", "-c", "user.name=owner"])
            .args(arguments)
            .current_dir(folder)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
    )
    .unwrap()
}

/// Makes the project a committed repository with an engine-made worktree in
/// `.crazyagent/worktrees/one`. Input: the case. Output: the worktree.
fn worktree(case: &Case) -> PathBuf {
    for arguments in [
        &["init", "-q"][..],
        &["commit", "-q", "--allow-empty", "-m", "start"],
    ] {
        let made = owner_git(&case.project, &case.home, arguments);
        assert!(made.status.success(), "{made:?}");
    }
    let worktree = case
        .project
        .join(".crazyagent")
        .join("worktrees")
        .join("one");
    fs::create_dir_all(worktree.parent().unwrap()).unwrap();
    let added = owner_git(
        &case.project,
        &case.home,
        &[
            "worktree",
            "add",
            "-q",
            "--detach",
            worktree.to_str().unwrap(),
            "HEAD",
        ],
    );
    assert!(added.status.success(), "{added:?}");
    worktree
}

/// Single-quotes a path for `/bin/sh`. Input: a path with no quote. Output: the word.
fn quoted(path: &Path) -> String {
    let text = path.to_str().unwrap();
    assert!(!text.contains('\''));
    format!("'{text}'")
}

/// Runs `script` with `/bin/sh -c` under the installed profile of `sandbox`, in its project
/// folder. Inputs: the sandbox, the profile path and the script. Output: the process.
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
    crate::spawn_gate::output(&mut command).unwrap()
}

/// Fails unless `script` fails under the profile and leaves nothing at `target`.
fn refused(sandbox: &ProjectSandbox, profile: &Path, script: &str, target: &Path) {
    let output = run(sandbox, profile, script);
    assert!(!output.status.success(), "{script} ran");
    assert!(
        fs::symlink_metadata(target).is_err(),
        "{script} made {}",
        target.display()
    );
}

/// Fails unless `script` succeeds under the profile.
fn allowed(sandbox: &ProjectSandbox, profile: &Path, script: &str) {
    let output = run(sandbox, profile, script);
    assert!(
        output.status.success(),
        "{script}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// RV-DS241-15 hardening: no git or engine path below the project folder, the worktree's
/// admin folder or the shared object store is denied by a case-exact `literal` or `subpath`
/// rule; each protected name is an any-case regex, an entry alone where the rule was a
/// literal and the entry with everything below it where it was a subpath, so the profile no
/// longer depends on how Seatbelt folds letter case. Every string still fits Seatbelt's reader.
#[test]
fn every_protected_git_path_is_denied_by_an_any_case_regex() {
    let case = case();
    let worktree = worktree(&case);
    let extra = case.owner_tmp.join("extra");
    fs::create_dir_all(&extra).unwrap();
    let open = sandbox(&case).allow_worktree(&extra).unwrap();
    let text = open.profile();
    let project = case.project.display();
    for exact in [
        format!("(literal \"{project}/"),
        format!("(subpath \"{project}/"),
    ] {
        assert!(!text.contains(&exact), "{exact}\n{text}");
    }
    let pattern = seatbelt_pattern(&case.project);
    let git = any_case(".git");
    let state = any_case(".crazyagent");
    let context = any_case("context");
    let objects = format!("{git}/{}", any_case("objects"));
    let mut rules: Vec<(String, bool)> = vec![
        (context.clone(), true),
        (format!("{state}/{context}"), true),
        (git.clone(), false),
        (state.clone(), false),
        (objects.clone(), false),
        (format!("{objects}/{}", any_case("info")), false),
    ];
    for name in [
        "hooks",
        "rebase-merge",
        "rebase-apply",
        "remotes",
        "branches",
        "sequencer",
    ] {
        rules.push((format!("{git}/{}", any_case(name)), true));
    }
    for name in [
        "config",
        "config.lock",
        "index",
        "index.lock",
        "commondir",
        "config.worktree",
    ] {
        rules.push((format!("{git}/{}", any_case(name)), false));
    }
    for (path, below) in &rules {
        let tail = if *below { "(/.*)?" } else { "" };
        let rule = format!("(regex #\"^{pattern}/{path}{tail}$\")");
        assert!(text.contains(&rule), "{rule}\n{text}");
    }
    assert!(text.contains(&format!("/{}(/.*)?$\")", any_case("modules"))));
    assert!(text.contains(&format!(
        "(regex #\"^{}/{git}$\")",
        seatbelt_pattern(&extra)
    )));
    assert!(sbpl_overlong(&text).is_empty());

    let confined = sandbox(&case).confine_to_worktree(&worktree).unwrap();
    let text = confined.profile();
    let admin = case.project.join(".git").join("worktrees").join("one");
    let store = case.project.join(".git").join("objects");
    for exact in [
        format!("(literal \"{}/", admin.display()),
        format!("(subpath \"{}/", store.display()),
    ] {
        assert!(!text.contains(&exact), "{exact}\n{text}");
    }
    for rule in [
        format!(
            "(regex #\"^{}/{}$\")",
            seatbelt_pattern(&admin),
            any_case("HEAD")
        ),
        format!(
            "(regex #\"^{}/({}|{})(/.*)?$\")",
            seatbelt_pattern(&store),
            any_case("info"),
            any_case("pack")
        ),
    ] {
        assert!(text.contains(&rule), "{rule}\n{text}");
    }
    assert!(sbpl_overlong(&text).is_empty());
}

/// RV-DS241-15 regression row, passing before the hardening on macOS 26.6.2, whose Seatbelt
/// folds letter case itself (w-v241-fix7 probe): under the real profile a project-root agent,
/// as the IDE server and IDE shell run, writes no protected git or engine path in its own
/// case or another one, the long s and the Kelvin sign included, where the name does not
/// exist yet, nor makes `.git` or `.crazyagent` in a project without them; the writes git
/// and the agent need beside those names, an object, a ref, a file in `.crazyagent` and a
/// name that only starts like `context`, still land.
#[test]
fn a_project_agent_writes_no_protected_path_in_any_letter_case() {
    let case = case();
    let git = case.project.join(".git");
    fs::create_dir_all(git.join("objects")).unwrap();
    fs::create_dir_all(case.project.join(".crazyagent")).unwrap();
    let sandbox = sandbox(&case);
    let profile = case.profiles.join("cased.sb");
    for name in [
        "COMMONDIR",
        "commondir",
        "Config.Worktree",
        "config.worktree",
        "CONFIG",
        "config",
        "CONFIG.LOCK",
        "config.lock",
        "Index",
        "index",
        "INDEX.LOCK",
        "index.lock",
    ] {
        refused(
            &sandbox,
            &profile,
            &format!("printf x > {}", quoted(&git.join(name))),
            &git.join(name),
        );
    }
    for name in [
        "HOOKS",
        "hooks",
        "hoo\u{212a}s",
        "Rebase-Merge",
        "rebase-merge",
        "REBASE-APPLY",
        "rebase-apply",
        "\u{17f}equencer",
        "sequencer",
        "REMOTES",
        "remotes",
        "Branches",
        "branches",
    ] {
        refused(
            &sandbox,
            &profile,
            &format!("mkdir {}", quoted(&git.join(name))),
            &git.join(name),
        );
    }
    for folder in [
        case.project.join("CONTEXT"),
        case.project.join("context"),
        case.project.join(".crazyagent").join("Context"),
        case.project.join(".crazyagent").join("context"),
    ] {
        refused(
            &sandbox,
            &profile,
            &format!("mkdir {}", quoted(&folder)),
            &folder,
        );
    }
    assert_eq!(fs::read_dir(&git).unwrap().count(), 1);
    for script in [
        "mkdir -p .git/objects/ab && printf x > .git/objects/ab/cdef",
        "mkdir -p .git/refs/heads && printf x > .git/refs/heads/topic",
        "printf x > .git/description",
        "printf x > .crazyagent/note.txt",
        "printf x > contexts.txt",
    ] {
        allowed(&sandbox, &profile, script);
    }

    let bare = self::case();
    let sandbox = self::sandbox(&bare);
    let profile = bare.profiles.join("bare.sb");
    for name in [".GIT", ".git", ".CrazyAgent", ".crazyagent"] {
        let folder = bare.project.join(name);
        refused(
            &sandbox,
            &profile,
            &format!("mkdir {}", quoted(&folder)),
            &folder,
        );
    }
    assert_eq!(fs::read_dir(&bare.project).unwrap().count(), 0);
}

/// RV-DS241-15 regression row, passing before the hardening on macOS 26.6.2 (see above): a
/// confined agent rewrites no `HEAD` of its admin folder and moves no `info` or `pack`
/// folder out of the shared object store, in its own case or another one, while a file in
/// its worktree still lands.
#[test]
fn a_confined_agent_moves_no_admin_head_or_store_folder_in_any_letter_case() {
    let case = case();
    let worktree = worktree(&case);
    let confined = sandbox(&case).confine_to_worktree(&worktree).unwrap();
    let profile = case.profiles.join("confined.sb");
    let admin = case.project.join(".git").join("worktrees").join("one");
    let store = case.project.join(".git").join("objects");
    let head = fs::read(admin.join("HEAD")).unwrap();
    for name in ["head", "HEAD"] {
        let output = run(
            &confined,
            &profile,
            &format!("printf planted > {}", quoted(&admin.join(name))),
        );
        assert!(!output.status.success(), "{name}");
    }
    assert_eq!(fs::read(admin.join("HEAD")).unwrap(), head);
    for (from, to) in [
        ("INFO", "i0"),
        ("info", "i1"),
        ("Pack", "p0"),
        ("pack", "p1"),
    ] {
        refused(
            &confined,
            &profile,
            &format!(
                "mv {} {}",
                quoted(&store.join(from)),
                quoted(&store.join(to))
            ),
            &store.join(to),
        );
    }
    allowed(
        &confined,
        &profile,
        &format!("printf w > {}", quoted(&worktree.join("work.txt"))),
    );
}
