#![cfg(target_os = "macos")]

use crazyagent::sandbox::{
    InstalledSandbox, ProjectSandbox, Sandbox, SandboxError, SandboxNetwork, SandboxRemote,
};
use std::fs;
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
    crazyagent::spawn_gate::output(&mut command).unwrap()
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
        let status = crazyagent::spawn_gate::status(
            std::process::Command::new("/usr/bin/git")
                .args(arguments)
                .current_dir(&case.project)
                .env("HOME", &case.home),
        )
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

/// Runs git the way the owner does, outside every sandbox, with no global or system
/// configuration. Input: the folder, the agent home as HOME and the arguments.
/// Output: the command's output.
fn owner_git(folder: &Path, home: &Path, arguments: &[&str]) -> Output {
    crazyagent::spawn_gate::output(
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

/// Builds one attack's shell script from its case and the config line it appends.
type Attack = fn(&Case, &str) -> String;

/// A fresh project for one attack: a committed repository, an engine-made worktree in
/// `<project>/worktrees/engine-one` and a protected `.crazyagent/context/brief.md`.
/// Output: the case, its sandbox, its installed profile path and the worktree.
fn git_case() -> (Case, ProjectSandbox, PathBuf, PathBuf) {
    let case = case();
    for arguments in [
        &["init", "-q"][..],
        &["commit", "-q", "--allow-empty", "-m", "start"],
    ] {
        let made = owner_git(&case.project, &case.home, arguments);
        assert!(made.status.success(), "{made:?}");
    }
    let worktree = case.project.join("worktrees").join("engine-one");
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
    let brief = case.project.join(".crazyagent").join("context");
    fs::create_dir_all(&brief).unwrap();
    fs::write(brief.join("brief.md"), "brief\n").unwrap();
    let sandbox = sandbox(&case, SandboxNetwork::Only(Vec::new()));
    let profile = case.profiles.join("agent.sb");
    (case, sandbox, profile, worktree)
}

#[test]
fn an_agent_cannot_move_or_redirect_the_git_metadata_the_owner_git_reads() {
    const MARK: &str = "/private/tmp/crazyagent-agent-chosen-program";
    let marked = format!("printf '[core]\\n\\tfsmonitor = {MARK}\\n' >>");
    let attacks: [(&str, Attack); 14] = [
        (
            "rename .git, edit its config, rename it back",
            |_, marked| format!("mv .git .g; {marked} .g/config; mv .g .git"),
        ),
        ("edit .git/config through a hard link", |_, marked| {
            format!("ln .git/config cfg; {marked} cfg")
        }),
        ("replace .git with a gitdir: file", |case, marked| {
            let evil = quoted(&case.tmp.join("evil"));
            format!(
                "cp -R .git {evil}; {marked} {evil}/config; rm -rf .git; printf 'gitdir: %s\\n' {evil} > .git"
            )
        }),
        ("replace .git with a symbolic link", |case, marked| {
            let evil = quoted(&case.tmp.join("evil"));
            format!("cp -R .git {evil}; {marked} {evil}/config; rm -rf .git; ln -s {evil} .git")
        }),
        (
            "move the project out, edit, move it back",
            |case, marked| {
                let project = quoted(&case.project);
                let moved = quoted(&case.tmp.join("moved"));
                format!("mv {project} {moved}; {marked} {moved}/.git/config; mv {moved} {project}")
            },
        ),
        ("point .git/commondir at an agent copy", |case, marked| {
            let evil = quoted(&case.tmp.join("evil"));
            format!(
                "cp -R .git {evil}; {marked} {evil}/config; printf '%s\\n' {evil} > .git/commondir"
            )
        }),
        (
            "point the worktree's .git file at an agent copy",
            |case, marked| {
                let evil = quoted(&case.tmp.join("evil"));
                format!(
                    "cp -R .git {evil}; {marked} {evil}/config; printf 'gitdir: %s\\n' {evil} > worktrees/engine-one/.git"
                )
            },
        ),
        (
            "move the worktree out, rewrite its .git file, move it back",
            |case, marked| {
                let evil = quoted(&case.tmp.join("evil"));
                let moved = quoted(&case.tmp.join("wt"));
                format!(
                    "cp -R .git {evil}; {marked} {evil}/config; mv worktrees/engine-one {moved}; printf 'gitdir: %s\\n' {evil} > {moved}/.git; mv {moved} worktrees/engine-one"
                )
            },
        ),
        (
            "move the worktrees folder, rewrite a .git file, move it back",
            |case, marked| {
                let evil = quoted(&case.tmp.join("evil"));
                format!(
                    "cp -R .git {evil}; {marked} {evil}/config; mv worktrees w; printf 'gitdir: %s\\n' {evil} > w/engine-one/.git; mv w worktrees"
                )
            },
        ),
        (
            "point the worktree's commondir at an agent copy",
            |case, marked| {
                let evil = quoted(&case.tmp.join("evil"));
                format!(
                    "cp -R .git {evil}; {marked} {evil}/config; printf '%s\\n' {evil} > .git/worktrees/engine-one/commondir"
                )
            },
        ),
        (
            "move .git/worktrees, rewrite a commondir, move it back",
            |case, marked| {
                let evil = quoted(&case.tmp.join("evil"));
                format!(
                    "cp -R .git {evil}; {marked} {evil}/config; mv .git/worktrees .git/w; printf '%s\\n' {evil} > .git/w/engine-one/commondir; mv .git/w .git/worktrees"
                )
            },
        ),
        (
            "rewrite the worktree's .git file through a hard link",
            |case, marked| {
                let evil = quoted(&case.tmp.join("evil"));
                format!(
                    "cp -R .git {evil}; {marked} {evil}/config; ln worktrees/engine-one/.git link; printf 'gitdir: %s\\n' {evil} > link"
                )
            },
        ),
        (
            "rewrite the worktree's commondir through a hard link",
            |case, marked| {
                let evil = quoted(&case.tmp.join("evil"));
                format!(
                    "cp -R .git {evil}; {marked} {evil}/config; ln .git/worktrees/engine-one/commondir link; printf '%s\\n' {evil} > link"
                )
            },
        ),
        (
            "move .crazyagent, edit the protected brief, move it back",
            |_, _| {
                "mv .crazyagent .c; echo agent > .c/context/brief.md; mv .c .crazyagent".to_owned()
            },
        ),
    ];
    let mut landed = Vec::new();
    for (name, script) in attacks {
        let (case, sandbox, profile, worktree) = git_case();
        let brief = case.project.join(".crazyagent/context/brief.md");
        let _ = run(&sandbox, &profile, &script(&case, &marked));
        for folder in [&case.project, &worktree] {
            let seen = owner_git(folder, &case.home, &["config", "--get", "core.fsmonitor"]);
            if String::from_utf8_lossy(&seen.stdout).contains(MARK) {
                landed.push(format!("{name}: owner git in {}", folder.display()));
            }
        }
        if fs::read_to_string(&brief).unwrap_or_default() != "brief\n" {
            landed.push(format!("{name}: {}", brief.display()));
        }
    }
    assert_eq!(landed, Vec::<String>::new());
    let (case, sandbox, profile, worktree) = git_case();
    let committed = run(
        &sandbox,
        &profile,
        "cd worktrees/engine-one && echo work > work.txt && git add work.txt && git -c user.email=agent@local -c user.name=agent commit -q -m work && echo ok",
    );
    assert!(
        committed.status.success(),
        "{}",
        String::from_utf8_lossy(&committed.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&committed.stdout), "ok\n");
    assert!(case.project.join(".git").is_dir());
    assert!(worktree.join(".git").is_file());
    let root_commit = run(
        &sandbox,
        &profile,
        "echo root > root.txt && git add root.txt && git -c user.email=agent@local -c user.name=agent commit -q -m root",
    );
    assert!(!root_commit.status.success());
}

/// Checks that `script`, run under `sandbox` and `profile`, cannot create `target`: the
/// command fails and the file is absent. Input: the sandbox, the profile, the script and
/// the target path.
fn write_refused(sandbox: &ProjectSandbox, profile: &Path, script: &str, target: &Path) {
    let output = run(sandbox, profile, script);
    assert!(!output.status.success(), "{script}");
    assert!(!target.exists(), "{}", target.display());
}

#[test]
fn a_sandbox_refuses_writes_to_the_submodule_store() {
    let (case, sandbox, profile, _worktree) = git_case();
    let git = case.project.join(".git");
    let admin = git.join("worktrees").join("engine-one").join("modules");
    for (script, target) in [
        (
            "mkdir -p .git/modules/sub && : > .git/modules/sub/config",
            git.join("modules").join("sub").join("config"),
        ),
        (
            "mkdir -p .git/MODULES/sub && : > .git/MODULES/sub/config",
            git.join("MODULES").join("sub").join("config"),
        ),
        (
            "mkdir -p .git/worktrees/engine-one/modules/sub && : > .git/worktrees/engine-one/modules/sub/config",
            admin.join("sub").join("config"),
        ),
    ] {
        write_refused(&sandbox, &profile, script, &target);
    }
    for (script, target) in [
        (
            "mkdir -p sub/.git".to_owned(),
            case.project.join("sub").join(".git"),
        ),
        (
            "mkdir -p filed && : > filed/.git".to_owned(),
            case.project.join("filed").join(".git"),
        ),
        (
            "mkdir -p cased && : > cased/.GIT".to_owned(),
            case.project.join("cased").join(".GIT"),
        ),
        (
            format!(
                "mkdir -p linked && ln -s {} linked/.git",
                quoted(&case.project)
            ),
            case.project.join("linked").join(".git"),
        ),
        (
            ": > staged && mkdir -p renamed && mv staged renamed/.git".to_owned(),
            case.project.join("renamed").join(".git"),
        ),
        (
            "mkdir -p worktrees/engine-one/nested/.git".to_owned(),
            case.project
                .join("worktrees")
                .join("engine-one")
                .join("nested")
                .join(".git"),
        ),
    ] {
        write_refused(&sandbox, &profile, &script, &target);
    }
    let index = git.join("index");
    let before = fs::read(&index).ok();
    let output = run(&sandbox, &profile, "printf planted > .git/index");
    assert!(!output.status.success());
    assert_eq!(fs::read(&index).ok(), before);

    let case = self::case();
    for arguments in [
        &["init", "-q"][..],
        &["commit", "-q", "--allow-empty", "-m", "start"],
    ] {
        let made = owner_git(&case.project, &case.home, arguments);
        assert!(made.status.success(), "{made:?}");
    }
    let worktree = case.project.join("worktrees").join("one");
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
    let confined = self::sandbox(&case, SandboxNetwork::Only(Vec::new()))
        .confine_to_worktree(&worktree)
        .unwrap();
    let profile = case.profiles.join("confined.sb");
    let admin = case.project.join(".git/worktrees/one/modules");
    for (script, target) in [
        (
            format!(
                "mkdir -p {dir}/sub && : > {dir}/sub/config",
                dir = quoted(&admin)
            ),
            admin.join("sub").join("config"),
        ),
        (
            format!("mkdir -p {}/sub/.git", quoted(&worktree)),
            worktree.join("sub").join(".git"),
        ),
        (
            format!(
                "mkdir -p {wt}/filed && : > {wt}/filed/.git",
                wt = quoted(&worktree)
            ),
            worktree.join("filed").join(".git"),
        ),
    ] {
        write_refused(&confined, &profile, &script, &target);
    }
    let committed = run(
        &confined,
        &profile,
        &format!(
            "cd {} && echo work > work.txt && git add work.txt && git -c user.email=agent@local -c user.name=agent commit -q -m work && echo ok",
            quoted(&worktree)
        ),
    );
    assert!(
        committed.status.success(),
        "{}",
        String::from_utf8_lossy(&committed.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&committed.stdout), "ok\n");
}

#[test]
fn network_is_open_only_when_tools_are_on() {
    let case = case();
    let allowed = crazyagent::spawn_gate::bind_tcp_std("127.0.0.1:0").unwrap();
    let other = crazyagent::spawn_gate::bind_tcp_std("127.0.0.1:0").unwrap();
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

#[test]
fn wrap_puts_an_existing_harness_command_under_the_sandbox_and_keeps_only_safe_env() {
    let case = case();
    let sandbox = sandbox(&case, SandboxNetwork::Only(Vec::new()));
    let profile = case.profiles.join("wrap.sb");
    sandbox.install(&profile).unwrap();
    let escape = case.outside.join("wrapped-escape.txt");
    let inside = case.project.join("worktrees").join("wrapped.txt");
    let mut harness = std::process::Command::new("/bin/sh");
    harness
        .arg("-c")
        .arg(format!(
            "echo \"$HARNESS_MODE\" > {} && echo \"$HOME\" >> {} && (echo x > {} || true)",
            quoted(&inside),
            quoted(&inside),
            quoted(&escape)
        ))
        .env("HARNESS_MODE", "headless")
        .env("HOME", owner_home())
        .current_dir(case.project.join("worktrees"));
    let mut wrapped = sandbox.wrap(&profile, &harness).unwrap();
    let output = crazyagent::spawn_gate::output(
        wrapped
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
    )
    .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read_to_string(&inside).unwrap(),
        format!("headless\n{}\n", case.home.display())
    );
    assert!(!escape.exists());
    let mut outside = std::process::Command::new("/bin/sh");
    outside.current_dir(&case.outside);
    assert!(sandbox.wrap(&profile, &outside).is_err());
    let mut relative = std::process::Command::new("sh");
    relative.current_dir(&case.project);
    assert!(sandbox.wrap(&profile, &relative).is_err());
}

#[test]
fn installed_sandbox_wraps_through_the_platform_seam_and_reports_its_policy() {
    let case = case();
    let network = SandboxNetwork::Only(vec![SandboxRemote::Loopback(8080)]);
    let read_root = case.root.join("harness");
    fs::create_dir_all(&read_root).unwrap();
    let sandbox = sandbox(&case, network.clone())
        .allow_read(&read_root)
        .unwrap();
    assert_eq!(sandbox.read_roots(), [read_root.clone()].as_slice());
    assert_eq!(sandbox.network(), &network);
    let git = case.project.join(".git");
    assert_eq!(
        sandbox.protected_paths(),
        [
            case.project.join("context"),
            case.project.join(".crazyagent").join("context"),
            git.join("hooks"),
            git.join("config"),
            git.join("config.lock"),
            git.join("modules"),
        ]
    );
    let text = sandbox.profile();
    for path in sandbox.protected_paths() {
        assert!(
            text.contains(&format!("\"{}\")", path.display())),
            "{}",
            path.display()
        );
    }
    let installed = InstalledSandbox::install(sandbox, &case.profiles.join("seam.sb")).unwrap();
    let seam: &dyn Sandbox = &installed;
    let mut harness = std::process::Command::new("/bin/sh");
    harness
        .arg("-c")
        .arg("echo seam")
        .current_dir(&case.project);
    let mut wrapped = seam.wrap(&harness).unwrap();
    let direct = installed.wrap(&harness).unwrap();
    assert_eq!(wrapped.get_program(), direct.get_program());
    assert!(wrapped.get_args().eq(direct.get_args()));
    let output = crazyagent::spawn_gate::output(
        wrapped
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
    )
    .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"seam\n");
    let mut outside = std::process::Command::new("/bin/sh");
    outside.current_dir(&case.outside);
    assert!(seam.wrap(&outside).is_err());
    assert_eq!(
        SandboxError::Unavailable("bubblewrap is missing".into()).to_string(),
        "sandbox unavailable: bubblewrap is missing"
    );
}

#[test]
fn a_sandbox_confines_only_to_an_engine_worktree_whose_git_links_agree() {
    let case = case();
    for arguments in [
        &["init", "-q"][..],
        &["commit", "-q", "--allow-empty", "-m", "start"],
    ] {
        let made = owner_git(&case.project, &case.home, arguments);
        assert!(made.status.success(), "{made:?}");
    }
    let one = case.project.join("worktrees").join("one");
    let two = case.project.join("worktrees").join("two");
    for worktree in [&one, &two] {
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
    }
    let confine = |worktree: &Path| {
        sandbox(&case, SandboxNetwork::Only(Vec::new())).confine_to_worktree(worktree)
    };
    let refused = |result: Result<ProjectSandbox, SandboxError>| {
        assert!(
            matches!(result, Err(SandboxError::Invalid(_))),
            "{:?}",
            result.map(|sandbox| sandbox.profile())
        );
    };
    let link = one.join(".git");
    let original = fs::read_to_string(&link).unwrap();
    let confined = confine(&one).unwrap();
    assert!(confined.read_roots().contains(&case.project));
    assert!(confined.writable_roots().all(|root| root != &case.project));
    refused(confined.confine_to_worktree(&one));
    refused(confine(&case.outside));
    refused(confine(&case.project.join("worktrees")));
    let git = case.project.join(".git");
    for forged in [
        format!("gitdir: {}\n", git.display()),
        format!("gitdir: {}\n", git.join("worktrees").join("two").display()),
        format!("gitdir: {}", git.join("worktrees").join("one").display()),
        String::from("gitdir: \n"),
    ] {
        fs::write(&link, forged).unwrap();
        refused(confine(&one));
    }
    fs::remove_file(&link).unwrap();
    std::os::unix::fs::symlink(two.join(".git"), &link).unwrap();
    refused(confine(&one));
    fs::remove_file(&link).unwrap();
    fs::write(&link, &original).unwrap();
    assert!(confine(&one).is_ok());
}

#[test]
fn an_agent_cannot_plant_the_git_redirect_files_the_engine_rejects() {
    let plants: [(&str, &str, &str); 12] = [
        (
            "commondir",
            "printf '/x\\n' > .git/commondir",
            ".git/commondir",
        ),
        (
            "config.worktree",
            "printf '[core]\\n' > .git/config.worktree",
            ".git/config.worktree",
        ),
        (
            "alternates",
            "printf '/x\\n' > .git/objects/info/alternates",
            ".git/objects/info/alternates",
        ),
        (
            "http-alternates",
            "printf 'http://x\\n' > .git/objects/info/http-alternates",
            ".git/objects/info/http-alternates",
        ),
        (
            "grafts",
            "mkdir -p .git/info; printf 'x\\n' > .git/info/grafts",
            ".git/info/grafts",
        ),
        ("shallow", "printf 'x\\n' > .git/shallow", ".git/shallow"),
        (
            "alternates through a staged objects/info",
            "mkdir s; printf '/x\\n' > s/alternates; mv .git/objects/info .git/objects/i0; mv s .git/objects/info",
            ".git/objects/info/alternates",
        ),
        (
            "alternates through a staged objects store",
            "cp -R .git/objects o; mkdir -p o/info; printf '/x\\n' > o/info/alternates; mv .git/objects .git/o0; mv o .git/objects",
            ".git/objects/info/alternates",
        ),
        (
            "grafts through a staged info folder",
            "mkdir s; printf 'x\\n' > s/grafts; mv .git/info .git/i0; mv s .git/info",
            ".git/info/grafts",
        ),
        (
            "grafts through a staged info folder with none before",
            "rm -rf .git/info; mkdir s; printf 'x\\n' > s/grafts; mv s .git/info",
            ".git/info/grafts",
        ),
        (
            "commondir through a staged .git",
            "cp -R .git g; printf '/x\\n' > g/commondir; mv .git .g0; mv g .git",
            ".git/commondir",
        ),
        (
            "brief through a staged .crazyagent",
            "cp -R .crazyagent c; echo agent > c/context/brief.md; mv .crazyagent .c0; mv c .crazyagent",
            ".crazyagent/context/brief.md",
        ),
    ];
    let mut landed = Vec::new();
    for (name, script, target) in plants {
        let (case, sandbox, profile, _) = git_case();
        let target = case.project.join(target);
        let before = fs::read(&target).ok();
        let _ = run(&sandbox, &profile, script);
        if fs::read(&target).ok() != before {
            landed.push(name);
        }
    }
    assert_eq!(landed, Vec::<&str>::new());
}
