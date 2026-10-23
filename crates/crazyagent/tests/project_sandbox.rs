use crazyagent::sandbox::{ProjectSandbox, SandboxNetwork, SandboxRemote};
use std::fs;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};

struct Case {
    _temp: tempfile::TempDir,
    root: PathBuf,
    project: PathBuf,
    home: PathBuf,
    tmp: PathBuf,
    outside: PathBuf,
    profiles: PathBuf,
}

fn case() -> Case {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let project = root.join("project");
    let home = root.join("agent").join("home");
    let tmp = root.join("agent").join("tmp");
    let outside = root.join("outside");
    let profiles = root.join("profiles");
    for directory in [&project, &home, &tmp, &outside, &profiles] {
        fs::create_dir_all(directory).unwrap();
    }
    fs::create_dir_all(project.join("context")).unwrap();
    fs::create_dir_all(project.join("worktrees")).unwrap();
    fs::write(project.join("context").join("brief.md"), "brief\n").unwrap();
    fs::write(outside.join("fixture.txt"), "outside fixture\n").unwrap();
    Case {
        _temp: temp,
        root,
        project,
        home,
        tmp,
        outside,
        profiles,
    }
}

fn sandbox(case: &Case, network: SandboxNetwork) -> ProjectSandbox {
    ProjectSandbox::new(&case.project, &case.home, &case.tmp, network).unwrap()
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
    command.output().unwrap()
}

fn owner_home() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").unwrap())
}

fn quoted(path: &Path) -> String {
    let text = path.to_str().unwrap();
    assert!(!text.contains('\''));
    format!("'{text}'")
}

#[test]
fn sandboxed_child_writes_only_inside_the_project_and_its_own_dirs() {
    let case = case();
    let sandbox = sandbox(&case, SandboxNetwork::Only(Vec::new()));
    let profile = case.profiles.join("writes.sb");
    let inside = case.project.join("worktrees").join("inside.txt");
    let home_file = case.home.join("home.txt");
    let tmp_file = case.tmp.join("tmp.txt");
    let script = format!(
        "echo inside > {} && echo home > {} && echo tmp > {} && echo ok",
        quoted(&inside),
        quoted(&home_file),
        quoted(&tmp_file)
    );
    let output = run(&sandbox, &profile, &script);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), "ok\n");
    assert_eq!(fs::read_to_string(&inside).unwrap(), "inside\n");
    assert_eq!(fs::read_to_string(&home_file).unwrap(), "home\n");
    assert_eq!(fs::read_to_string(&tmp_file).unwrap(), "tmp\n");
}

#[test]
fn sandboxed_child_cannot_write_outside_the_project_or_into_the_owner_home() {
    let case = case();
    let sandbox = sandbox(&case, SandboxNetwork::Only(Vec::new()));
    let profile = case.profiles.join("denied.sb");
    let outside = case.outside.join("escape.txt");
    let marker = format!("crazyagent-sandbox-probe-{}", std::process::id());
    let in_home = owner_home().join(&marker);
    let in_context = case.project.join("context").join("tampered.md");
    let sibling = case.root.join("sibling.txt");
    for target in [&outside, &in_home, &in_context, &sibling] {
        let script = format!("echo escaped > {}", quoted(target));
        let output = run(&sandbox, &profile, &script);
        assert!(!output.status.success(), "{}", target.display());
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("Operation not permitted"),
            "{}: {}",
            target.display(),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!target.exists(), "{}", target.display());
    }
    let brief = case.project.join("context").join("brief.md");
    let output = run(
        &sandbox,
        &profile,
        &format!("echo tampered >> {}", quoted(&brief)),
    );
    assert!(!output.status.success());
    assert_eq!(fs::read_to_string(&brief).unwrap(), "brief\n");
    let link = case.project.join("worktrees").join("context-link");
    let output = run(
        &sandbox,
        &profile,
        &format!(
            "ln -s {} {} && echo tampered > {}/via-link.md",
            quoted(&case.project.join("context")),
            quoted(&link),
            quoted(&link)
        ),
    );
    assert!(!output.status.success());
    assert!(!case.project.join("context").join("via-link.md").exists());
}

#[test]
fn sandboxed_child_cannot_read_outside_files_or_the_owner_codex_and_claude_dirs() {
    let case = case();
    let sandbox = sandbox(&case, SandboxNetwork::Open);
    let profile = case.profiles.join("reads.sb");
    let output = run(
        &sandbox,
        &profile,
        &format!(
            "cat {} > /dev/null",
            quoted(&case.outside.join("fixture.txt"))
        ),
    );
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("Operation not permitted"));
    assert!(!String::from_utf8_lossy(&output.stderr).contains("outside fixture"));
    let home = owner_home();
    for private in [home.join(".codex"), home.join(".claude")] {
        let script = format!(
            "ls -d {dir} > /dev/null 2>&1 && exit 10; cat {dir}/config.toml > /dev/null 2>&1 && exit 11; ls {dir} > /dev/null 2>&1 && exit 12; ls -d {dir} 2>&1 >/dev/null; exit 0",
            dir = quoted(&private)
        );
        let output = run(&sandbox, &profile, &script);
        assert_eq!(output.status.code(), Some(0), "{}", private.display());
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("Operation not permitted"),
            "{}",
            private.display()
        );
    }
}

#[test]
fn sandboxed_child_can_run_git_worktrees_inside_the_project() {
    let case = case();
    let sandbox = sandbox(&case, SandboxNetwork::Only(Vec::new()));
    let profile = case.profiles.join("git.sb");
    let worktree = case.project.join("worktrees").join("agent-one");
    for arguments in [
        vec!["init", "-q"],
        vec![
            "-c",
            "user.email=engine@local",
            "-c",
            "user.name=engine",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "start",
        ],
    ] {
        let status = std::process::Command::new("/usr/bin/git")
            .args(arguments)
            .current_dir(&case.project)
            .env("HOME", &case.home)
            .status()
            .unwrap();
        assert!(status.success());
    }
    let script = format!(
        "git worktree add -q -b agent-one {wt} && cd {wt} && echo work > work.txt && git add work.txt && git -c user.email=agent@local -c user.name=agent commit -q -m work && echo ok",
        wt = quoted(&worktree)
    );
    let output = run(&sandbox, &profile, &script);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), "ok\n");
    assert!(worktree.join(".git").is_file());
    let hook = case
        .project
        .join(".git")
        .join("hooks")
        .join("post-checkout");
    let output = run(
        &sandbox,
        &profile,
        &format!("echo '#!/bin/sh' > {}", quoted(&hook)),
    );
    assert!(!output.status.success());
    assert!(!hook.exists());
    let before = fs::read_to_string(case.project.join(".git").join("config")).unwrap();
    let output = run(&sandbox, &profile, "git config core.hooksPath /private/tmp");
    assert!(!output.status.success());
    assert_eq!(
        fs::read_to_string(case.project.join(".git").join("config")).unwrap(),
        before
    );
}

#[test]
fn network_is_open_only_when_tools_are_on() {
    let case = case();
    let allowed = TcpListener::bind("127.0.0.1:0").unwrap();
    let other = TcpListener::bind("127.0.0.1:0").unwrap();
    let allowed_port = allowed.local_addr().unwrap().port();
    let other_port = other.local_addr().unwrap().port();
    let connect = |sandbox: &ProjectSandbox, name: &str, port: u16| {
        let profile = case.profiles.join(name);
        run(
            sandbox,
            &profile,
            &format!("/usr/bin/nc -z -w 2 127.0.0.1 {port}"),
        )
        .status
        .success()
    };
    let open = sandbox(&case, SandboxNetwork::Open);
    assert!(connect(&open, "open-a.sb", allowed_port));
    assert!(connect(&open, "open-b.sb", other_port));
    let closed = sandbox(&case, SandboxNetwork::Only(Vec::new()));
    assert!(!connect(&closed, "closed-a.sb", allowed_port));
    assert!(!connect(&closed, "closed-b.sb", other_port));
    let model = sandbox(
        &case,
        SandboxNetwork::Only(vec![SandboxRemote::Loopback(allowed_port)]),
    );
    assert!(connect(&model, "model-a.sb", allowed_port));
    assert!(!connect(&model, "model-b.sb", other_port));
}

#[test]
fn sandbox_refuses_unsafe_roots_and_profile_locations() {
    let case = case();
    let network = || SandboxNetwork::Only(Vec::new());
    assert!(ProjectSandbox::new(Path::new("relative"), &case.home, &case.tmp, network()).is_err());
    assert!(ProjectSandbox::new(&owner_home(), &case.home, &case.tmp, network()).is_err());
    assert!(ProjectSandbox::new(Path::new("/"), &case.home, &case.tmp, network()).is_err());
    assert!(
        ProjectSandbox::new(
            &case.project,
            &owner_home().join(".codex"),
            &case.tmp,
            network()
        )
        .is_err()
    );
    let quote = case.root.join("bad\"name");
    fs::create_dir_all(&quote).unwrap();
    assert!(ProjectSandbox::new(&quote, &case.home, &case.tmp, network()).is_err());
    assert!(
        ProjectSandbox::new(
            &case.project.join("missing"),
            &case.home,
            &case.tmp,
            network()
        )
        .is_err()
    );
    let sandbox = sandbox(&case, network());
    assert!(sandbox.install(&case.project.join("inside.sb")).is_err());
    assert!(sandbox.install(&case.home.join("home.sb")).is_err());
    assert!(sandbox.install(&case.tmp.join("tmp.sb")).is_err());
    let profile = case.profiles.join("once.sb");
    sandbox.install(&profile).unwrap();
    let text = fs::read_to_string(&profile).unwrap();
    assert!(text.starts_with("(version 1)\n(deny default)\n"));
    assert!(!text.contains("(allow network*)"));
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(
        fs::metadata(&profile).unwrap().permissions().mode() & 0o777,
        0o600
    );
}
