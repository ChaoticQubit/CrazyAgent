use crazyagent::sandbox::{InstalledSandbox, ProjectSandbox, SandboxNetwork, SandboxRemote};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

struct Case {
    _temp: tempfile::TempDir,
    root: PathBuf,
    project: PathBuf,
    worktree: PathBuf,
    home: PathBuf,
    tmp: PathBuf,
    outside: PathBuf,
    profiles: PathBuf,
}

fn case() -> Case {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let project = root.join("project");
    let worktree = root.join("engine").join("worktrees").join("agent");
    let home = root.join("engine").join("agent").join("home");
    let tmp = root.join("engine").join("agent").join("tmp");
    let outside = root.join("outside");
    let profiles = root.join("engine").join("profiles");
    for directory in [&project, &worktree, &home, &tmp, &outside, &profiles] {
        fs::create_dir_all(directory).unwrap();
    }
    Case {
        _temp: temp,
        root,
        project,
        worktree,
        home,
        tmp,
        outside,
        profiles,
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
    .allow_worktree(&case.worktree)
    .unwrap()
}

fn shell(installed: &InstalledSandbox, cwd: &Path, script: &str) -> Output {
    let mut harness = Command::new("/bin/sh");
    harness.arg("-c").arg(script).current_dir(cwd);
    let mut command = installed.wrap(&harness).unwrap();
    assert_eq!(command.get_program(), "/usr/bin/sandbox-exec");
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command.output().unwrap()
}

fn quoted(path: &Path) -> String {
    let text = path.to_str().unwrap();
    assert!(!text.contains('\''));
    format!("'{text}'")
}

#[test]
fn an_agent_in_an_engine_worktree_writes_there_and_in_the_project_and_nowhere_else() {
    let case = case();
    let profile = case.profiles.join("agent.sb");
    let installed = InstalledSandbox::install(sandbox(&case), &profile).unwrap();
    assert_eq!(installed.profile(), profile.as_path());
    assert_eq!(
        fs::read_to_string(&profile).unwrap(),
        installed.sandbox().profile()
    );

    let work = case.worktree.join("work.txt");
    let shared = case.project.join("shared.txt");
    let scratch = case.tmp.join("scratch.txt");
    let allowed = shell(
        &installed,
        &case.worktree,
        &format!(
            "echo work > {} && echo shared > {} && echo scratch > {} && pwd",
            quoted(&work),
            quoted(&shared),
            quoted(&scratch)
        ),
    );
    assert!(
        allowed.status.success(),
        "{}",
        String::from_utf8_lossy(&allowed.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&allowed.stdout).trim(),
        case.worktree.to_str().unwrap()
    );
    assert_eq!(fs::read_to_string(&work).unwrap(), "work\n");
    assert_eq!(fs::read_to_string(&shared).unwrap(), "shared\n");
    assert_eq!(fs::read_to_string(&scratch).unwrap(), "scratch\n");

    let before = fs::read(&profile).unwrap();
    for target in [
        case.outside.join("escape.txt"),
        case.root.join("engine").join("escape.txt"),
        case.profiles.join("agent.sb"),
        case.profiles.join("other.sb"),
    ] {
        let refused = shell(
            &installed,
            &case.worktree,
            &format!("echo escape > {}", quoted(&target)),
        );
        assert!(!refused.status.success(), "{}", target.display());
        assert!(
            !target.exists() || target == profile,
            "{} was written",
            target.display()
        );
    }
    assert_eq!(fs::read(&profile).unwrap(), before);
}

#[test]
fn worktrees_are_checked_bounded_and_kept_out_of_the_profile_location() {
    let case = case();
    let text = sandbox(&case).profile();
    let granted = format!("(subpath \"{}\")", case.worktree.display());
    assert_eq!(text.matches(&granted).count(), 2, "{text}");
    let ancestors = format!("(path-ancestors \"{}\")", case.worktree.display());
    assert_eq!(text.matches(&ancestors).count(), 1, "{text}");
    assert_eq!(
        sandbox(&case)
            .writable_roots()
            .map(PathBuf::as_path)
            .collect::<Vec<_>>(),
        [
            case.project.as_path(),
            case.home.as_path(),
            case.tmp.as_path(),
            case.worktree.as_path()
        ]
    );

    let base = || {
        ProjectSandbox::new(
            &case.project,
            &case.home,
            &case.tmp,
            SandboxNetwork::Only(Vec::new()),
        )
        .unwrap()
    };
    let link = case.root.join("link");
    std::os::unix::fs::symlink(&case.worktree, &link).unwrap();
    let owner = PathBuf::from(std::env::var_os("HOME").unwrap());
    for unsafe_root in [
        link.clone(),
        case.root.join("missing"),
        PathBuf::from("relative"),
        PathBuf::from("/private"),
        owner,
    ] {
        assert!(
            base().allow_worktree(&unsafe_root).is_err(),
            "{}",
            unsafe_root.display()
        );
    }
    let mut widened = base();
    for index in 0..8 {
        let extra = case.root.join(format!("extra-{index}"));
        fs::create_dir(&extra).unwrap();
        widened = widened.allow_worktree(&extra).unwrap();
    }
    let ninth = case.root.join("extra-9");
    fs::create_dir(&ninth).unwrap();
    assert!(widened.allow_worktree(&ninth).is_err());

    let inside = case.worktree.join("agent.sb");
    assert!(InstalledSandbox::install(sandbox(&case), &inside).is_err());
    assert!(!inside.exists());

    let installed =
        InstalledSandbox::install(sandbox(&case), &case.profiles.join("agent.sb")).unwrap();
    for cwd in [&case.outside, &case.tmp, &case.home] {
        let mut harness = Command::new("/bin/sh");
        harness.current_dir(cwd);
        assert!(installed.wrap(&harness).is_err(), "{}", cwd.display());
    }
    let mut relative = Command::new("sh");
    relative.current_dir(&case.worktree);
    assert!(installed.wrap(&relative).is_err());
}

#[test]
fn a_model_endpoint_becomes_one_loopback_or_remote_port() {
    for (url, remote) in [
        ("http://127.0.0.1:11434/v1", SandboxRemote::Loopback(11434)),
        ("http://localhost:8080", SandboxRemote::Loopback(8080)),
        ("http://LOCALHOST", SandboxRemote::Loopback(80)),
        ("http://[::1]:9000/v1", SandboxRemote::Loopback(9000)),
        ("https://api.openai.com/v1", SandboxRemote::AnyHost(443)),
        ("http://10.0.0.5:8000", SandboxRemote::AnyHost(8000)),
    ] {
        assert_eq!(SandboxRemote::for_endpoint(url).unwrap(), remote, "{url}");
    }
    for url in [
        "ftp://127.0.0.1/v1",
        "file:///tmp/model",
        "not a url",
        "http://127.0.0.1:0/v1",
        "",
    ] {
        assert!(SandboxRemote::for_endpoint(url).is_err(), "{url}");
    }
}
