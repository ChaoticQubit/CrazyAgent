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

/// land-19 r2 M1 and L1: in a project with no `.git/info` or `.git/objects/info`, as a clone
/// or `init --template=` leaves it, no confined process makes `.git/info`, `.git/shallow` or
/// an alternates file in any letter case, the long s included, from which the owner's git
/// would read grafts, shallow cuts or other object stores through the case-insensitive
/// volume.
#[test]
fn sandboxed_child_cannot_make_git_info_or_shallow_in_any_letter_case() {
    let case = case();
    let git = case.project.join(".git");
    fs::create_dir_all(git.join("objects")).unwrap();
    let sandbox = sandbox(&case, SandboxNetwork::Only(Vec::new()));
    let profile = case.profiles.join("info.sb");
    for script in [
        format!("mkdir {}", quoted(&git.join("INFO"))),
        format!("mkdir {}", quoted(&git.join("info"))),
        format!("echo x > {}", quoted(&git.join("SHALLOW"))),
        format!("echo x > {}", quoted(&git.join("shallow"))),
        format!(
            "echo x > {}",
            quoted(&case.project.join(".GIT").join("Shallow"))
        ),
        format!("echo x > {}", quoted(&git.join("\u{17f}hallow"))),
        format!(
            "mkdir -p {0} && echo x > {0}/ALTERNATES",
            quoted(&git.join("objects").join("INFO"))
        ),
        format!(
            "mkdir -p {0} && echo x > {0}/Http-Alternates",
            quoted(&git.join("objects").join("Info"))
        ),
    ] {
        let output = run(&sandbox, &profile, &script);
        assert!(!output.status.success(), "{script}");
    }
    assert_eq!(fs::read_dir(&git).unwrap().count(), 1);
    let info = git.join("objects").join("info");
    assert!(!info.join("alternates").exists() && !info.join("http-alternates").exists());
}

/// land-19 r1 M1: no confined process writes a commit-graph file into the project's object
/// store, in any letter case, since git answers ancestry from it without reading commits.
#[test]
fn sandboxed_child_cannot_write_a_commit_graph_in_any_letter_case() {
    let case = case();
    let info = case.project.join(".git").join("objects").join("info");
    fs::create_dir_all(info.join("commit-graphs")).unwrap();
    let sandbox = sandbox(&case, SandboxNetwork::Only(Vec::new()));
    let profile = case.profiles.join("graph.sb");
    for target in [
        info.join("commit-graph"),
        case.project.join(".git/OBJECTS/Info/Commit-Graph"),
        info.join("commit-graphs").join("graph-0.graph"),
        case.project
            .join(".git/objects/INFO/COMMIT-GRAPHS/commit-graph-chain"),
    ] {
        let output = run(
            &sandbox,
            &profile,
            &format!("echo forged > {}", quoted(&target)),
        );
        assert!(!output.status.success(), "{}", target.display());
    }
    assert!(!info.join("commit-graph").exists());
    assert_eq!(fs::read_dir(info.join("commit-graphs")).unwrap().count(), 0);
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

/// SECAUDIT-GIT-STATE-FILES and SECAUDIT-IDE-SERVER-INDEX: a sandboxed process cannot take
/// the project's index lock or plant git state that names a program or another repository,
/// by the plain name or a case variant. An agent's own git folder keeps its rebase state.
#[test]
fn a_sandbox_refuses_git_state_that_names_a_program_or_another_repository() {
    let (case, sandbox, profile, _worktree) = git_case();
    let git = case.project.join(".git");
    for below in [
        "index.lock",
        "rebase-merge/git-rebase-todo",
        "rebase-apply/next",
        "remotes/origin",
        "branches/origin",
        "REBASE-MERGE/git-rebase-todo",
    ] {
        let target = git.join(below);
        let script = format!(
            "mkdir -p {} && : > {}",
            quoted(target.parent().unwrap()),
            quoted(&target)
        );
        write_refused(&sandbox, &profile, &script, &target);
    }
    let own = git
        .join("worktrees")
        .join("engine-one")
        .join("rebase-merge");
    let output = run(
        &sandbox,
        &profile,
        &format!(
            "mkdir -p {own} && : > {own}/git-rebase-todo",
            own = quoted(&own)
        ),
    );
    assert!(output.status.success(), "{output:?}");
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
        (
            format!("mkdir -p {h}/proj/.git", h = quoted(&case.home)),
            case.home.join("proj").join(".git"),
        ),
        (
            format!("cd {t} && git init -q box", t = quoted(&case.tmp)),
            case.tmp.join("box").join(".git"),
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
        (
            format!("cd {t} && git init -q box", t = quoted(&case.tmp)),
            case.tmp.join("box").join(".git"),
        ),
        (
            format!("mkdir -p {h}/proj/.git", h = quoted(&case.home)),
            case.home.join("proj").join(".git"),
        ),
        (
            format!(
                "mkdir -p {a}/evil/.git",
                a = quoted(&case.project.join(".git").join("worktrees").join("one"))
            ),
            case.project
                .join(".git")
                .join("worktrees")
                .join("one")
                .join("evil")
                .join(".git"),
        ),
    ] {
        write_refused(&confined, &profile, &script, &target);
    }
    let worked = run(
        &confined,
        &profile,
        &format!(
            "cd {} && echo work > work.txt && git reset -q && git status --porcelain && echo ok",
            quoted(&worktree)
        ),
    );
    assert!(
        worked.status.success(),
        "{}",
        String::from_utf8_lossy(&worked.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&worked.stdout), "?? work.txt\nok\n");
    let objects = case.project.join(".git").join("objects");
    let before = object_entries(&objects);
    let added = run(
        &confined,
        &profile,
        &format!("cd {} && git add work.txt", quoted(&worktree)),
    );
    assert!(!added.status.success(), "the agent added an object");
    assert!(
        String::from_utf8_lossy(&added.stderr).contains("failed to insert into database"),
        "{}",
        String::from_utf8_lossy(&added.stderr)
    );
    assert_eq!(object_entries(&objects), before);
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

/// Starts `nc -l` on one address and port inside the sandbox, sends it a line from inside
/// the sandbox when `connect` is set, and stops it. Returns what the listener received and
/// what it reported.
fn listen(
    sandbox: &ProjectSandbox,
    case: &Case,
    name: &str,
    host: &str,
    port: u16,
    connect: bool,
) -> (String, String) {
    let got = case.tmp.join(format!("{name}.got"));
    let said = case.tmp.join(format!("{name}.err"));
    let client = if connect {
        format!("printf hello | /usr/bin/nc -w 2 {host} {port}; ")
    } else {
        String::new()
    };
    let script = format!(
        "/usr/bin/nc -l {host} {port} > {} 2> {} & pid=$!; /bin/sleep 1; {client}/bin/sleep 0.3; kill $pid 2>/dev/null; wait $pid; exit 0",
        quoted(&got),
        quoted(&said)
    );
    let output = run(sandbox, &case.profiles.join(name), &script);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    (
        fs::read_to_string(&got).unwrap_or_default(),
        fs::read_to_string(&said).unwrap_or_default(),
    )
}

#[test]
fn a_tools_on_agent_serves_on_loopback_but_never_listens_on_an_owner_tool_port() {
    let case = case();
    let open = sandbox(&case, SandboxNetwork::Open);
    let free = crazyagent::spawn_gate::bind_tcp_std("127.0.0.1:0").unwrap();
    let port = free.local_addr().unwrap().port();
    drop(free);
    assert_eq!(
        listen(&open, &case, "dev-server.sb", "127.0.0.1", port, true),
        ("hello".to_owned(), String::new()),
        "an agent's dev server on loopback takes connections"
    );
    assert_eq!(crazyagent::sandbox::OWNER_PORTS, [1455, 11434]);
    for port in crazyagent::sandbox::OWNER_PORTS {
        for host in ["127.0.0.1", "0.0.0.0", "::1"] {
            let name = format!("owner-{port}-{}.sb", host.replace([':', '.'], "-"));
            let (got, said) = listen(&open, &case, &name, host, port, false);
            assert_eq!(got, "", "{host} {port}");
            assert!(
                said.contains("Operation not permitted"),
                "an agent cannot take {host}:{port} from the owner's tool: {said}"
            );
        }
    }
}

/// One owner model connection in settings, at `url`. Input: its id and URL. Output: the
/// connection as the settings file writes it.
fn connection(id: &str, url: &str) -> String {
    format!(
        r#"{{ id = "{id}", provider = "openai-compatible", harness = "rcli", protocol = "openai-chat", account_ref = "local-account", endpoint = {{ kind = "http", url = "{url}" }}, model = {{ id = "qwen2.5:0.5b", author = "openai-compatible" }}, auth = {{ method = "none" }} }}"#
    )
}

/// Writes owner settings whose one local model connection listens on `port`, as the owner
/// configures LM Studio or vLLM, and reads them back through the engine's settings
/// resolution. Input: the case and the port. Output: the model access settings.
fn local_model_access(case: &Case, port: u16) -> crazyagent::config::ModelAccess {
    use std::os::unix::fs::PermissionsExt;
    let global = case.root.join("global.toml");
    fs::write(
        &global,
        format!(
            "version = 1\ntimeout_ms = 2000\nallowed_destinations = []\n\n[model_access]\naccounts = [\"local-account\"]\nconnections = [{}]\npools = [{{ id = \"local-pool\", members = [\"local-connection\"], projects = [], roles = [], fallback_allowed = false }}]\n",
            connection("local-connection", &format!("http://127.0.0.1:{port}/v1"))
        ),
    )
    .unwrap();
    fs::set_permissions(&global, fs::Permissions::from_mode(0o600)).unwrap();
    crazyagent::config::resolve_config(&global, &case.root.join("project.toml"))
        .unwrap()
        .settings
        .model_access
        .unwrap()
}

#[test]
fn a_configured_local_model_port_stays_reachable_but_no_tools_on_agent_listens_on_it() {
    let case = case();
    let server = crazyagent::spawn_gate::bind_tcp_std("127.0.0.1:0").unwrap();
    let port = server.local_addr().unwrap().port();
    let ports = crazyagent::sandbox::owner_ports(&local_model_access(&case, port));
    assert_eq!(ports, [port]);
    let open = sandbox(&case, SandboxNetwork::Open)
        .keep_ports(&ports)
        .unwrap();
    assert!(
        open.profile().contains(&format!(
            "(deny network-bind (local ip \"*:1455\") (local ip \"*:11434\") (local ip \"*:{port}\"))"
        )),
        "{}",
        open.profile()
    );
    let served = std::thread::spawn(move || {
        let (mut stream, _) = crazyagent::spawn_gate::accept_tcp_std(&server).unwrap();
        let mut got = String::new();
        std::io::Read::read_to_string(&mut stream, &mut got).unwrap();
        got
    });
    let sent = run(
        &open,
        &case.profiles.join("reach-model.sb"),
        &format!("printf hello | /usr/bin/nc -w 2 127.0.0.1 {port}"),
    );
    assert!(
        sent.status.success(),
        "{}",
        String::from_utf8_lossy(&sent.stderr)
    );
    assert_eq!(
        served.join().unwrap(),
        "hello",
        "a tools-on agent still reaches the owner's model server on a kept port"
    );
    for host in ["127.0.0.1", "0.0.0.0", "::1"] {
        let name = format!("kept-{port}-{}.sb", host.replace([':', '.'], "-"));
        let (got, said) = listen(&open, &case, &name, host, port, false);
        assert_eq!(got, "", "{host} {port}");
        assert!(
            said.contains("Operation not permitted"),
            "an agent cannot take {host}:{port} from the owner's model server: {said}"
        );
    }
}

#[test]
fn only_loopback_model_endpoints_name_kept_ports_and_the_count_is_bounded() {
    let access: crazyagent::config::ModelAccess = toml::from_str(&format!(
        "accounts = [\"local-account\"]\nconnections = [{}]\n",
        [
            connection("a", "http://localhost:1234/v1"),
            connection("b", "http://[::1]:8000/v1"),
            connection("c", "https://LOCALHOST/v1"),
            connection("d", "http://127.0.0.1:8080/v1"),
            connection("e", "http://127.0.0.1:8080/v2"),
            connection("f", "http://10.0.0.5:9000/v1"),
            connection("g", "https://api.example.com/v1"),
            connection("h", "not a url"),
        ]
        .join(", ")
    ))
    .unwrap();
    assert_eq!(
        crazyagent::sandbox::owner_ports(&access),
        [443, 1234, 8000, 8080],
        "loopback endpoints only, each port once, the scheme's port when none is named"
    );
    let case = case();
    let open = sandbox(&case, SandboxNetwork::Open);
    assert!(matches!(
        open.clone().keep_ports(&[0]),
        Err(SandboxError::Invalid(_))
    ));
    let many: Vec<u16> =
        (20_000..20_000 + crazyagent::sandbox::KEPT_PORTS_MAX as u16 + 1).collect();
    assert!(matches!(
        open.clone().keep_ports(&many),
        Err(SandboxError::Invalid(_))
    ));
    let kept = open
        .keep_ports(&many[..crazyagent::sandbox::KEPT_PORTS_MAX])
        .unwrap()
        .keep_ports(&[1455, 11434, 20_000])
        .unwrap();
    assert_eq!(
        kept.profile().matches("(local ip \"*:20000\")").count(),
        2,
        "a kept port is denied once for bind and once for inbound"
    );
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
fn a_confined_agent_cannot_move_its_worktree_head_but_git_still_works_there() {
    let (case, sandbox, profile, linked) = git_case();
    let worktrees = case.project.join(".crazyagent").join("worktrees");
    fs::create_dir_all(&worktrees).unwrap();
    let worktree = worktrees.join("engine-one");
    let placed = owner_git(
        &case.project,
        &case.home,
        &[
            "worktree",
            "move",
            linked.to_str().unwrap(),
            worktree.to_str().unwrap(),
        ],
    );
    assert!(placed.status.success(), "{placed:?}");
    let moved = owner_git(
        &case.project,
        &case.home,
        &["commit", "-q", "--allow-empty", "-m", "second"],
    );
    assert!(moved.status.success(), "{moved:?}");
    let second = owner_git(&case.project, &case.home, &["rev-parse", "HEAD"]);
    let second = String::from_utf8(second.stdout).unwrap().trim().to_owned();
    let admin = case
        .project
        .join(".git")
        .join("worktrees")
        .join("engine-one");
    let head = admin.join("HEAD");
    let before = fs::read_to_string(&head).unwrap();
    assert_ne!(before.trim(), second);
    let confined = sandbox.confine_to_worktree(&worktree).unwrap();
    let place = quoted(&worktree);
    let quiet = run(
        &confined,
        &profile,
        &format!("cd {place} && git status --short && git reset -q --hard HEAD && echo ok"),
    );
    assert!(
        quiet.status.success(),
        "{}",
        String::from_utf8_lossy(&quiet.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&quiet.stdout), "ok\n");
    for script in [
        format!("cd {place} && git update-ref --no-deref HEAD {second}"),
        format!("cd {place} && git checkout -q --detach {second}"),
        format!("printf 'ref: refs/heads/main\\n' > {}", quoted(&head)),
        format!("mv {} {}", quoted(&head), quoted(&admin.join("HEAD.old"))),
        format!("rm {}", quoted(&head)),
    ] {
        let output = run(&confined, &profile, &script);
        assert!(!output.status.success(), "{script}");
        assert_eq!(fs::read_to_string(&head).unwrap(), before, "{script}");
    }
    assert!(!admin.join("HEAD.lock").exists());
    assert!(!admin.join("HEAD.old").exists());
}

#[test]
fn a_confined_sandbox_guards_a_real_admin_submodule_store_and_refuses_a_planted_one() {
    let case = case();
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
    let store = case.project.join(".git/worktrees/one/modules");
    assert!(fs::symlink_metadata(&store).is_err());
    let confine =
        || sandbox(&case, SandboxNetwork::Only(Vec::new())).confine_to_worktree(&worktree);
    let confined = confine().unwrap();
    use std::os::unix::fs::PermissionsExt;
    let metadata = fs::symlink_metadata(&store).unwrap();
    assert!(metadata.is_dir());
    assert_eq!(metadata.permissions().mode() & 0o777, 0o700);
    assert!(confined.protected_paths().contains(&store));
    assert!(
        !sandbox(&case, SandboxNetwork::Only(Vec::new()))
            .protected_paths()
            .contains(&store)
    );
    assert!(confine().unwrap().protected_paths().contains(&store));
    fs::remove_dir(&store).unwrap();
    fs::write(&store, b"planted").unwrap();
    let result = confine();
    assert!(
        matches!(result, Err(SandboxError::Invalid(_))),
        "{:?}",
        result.map(|sandbox| sandbox.profile())
    );
    assert_eq!(fs::read(&store).unwrap(), b"planted");
    fs::remove_file(&store).unwrap();
    std::os::unix::fs::symlink(&case.outside, &store).unwrap();
    assert!(matches!(confine(), Err(SandboxError::Invalid(_))));
    assert!(
        fs::symlink_metadata(&store)
            .unwrap()
            .file_type()
            .is_symlink()
    );
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

/// Lists what a confined agent could have added to a git object store: every entry below
/// it except `info`, `pack` and the `tmp_obj_` scratch files git writes before naming an
/// object, with its kind. Input: the object store. Output: the sorted entries.
fn object_entries(objects: &Path) -> Vec<(String, &'static str)> {
    let mut found = Vec::new();
    let mut pending = vec![objects.to_path_buf()];
    while let Some(folder) = pending.pop() {
        for entry in fs::read_dir(&folder).unwrap() {
            let path = entry.unwrap().path();
            let relative = path.strip_prefix(objects).unwrap().display().to_string();
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            if ["info", "pack"].contains(&relative.as_str()) || name.starts_with("tmp_obj_") {
                continue;
            }
            let kind = fs::symlink_metadata(&path).unwrap().file_type();
            let shown = if kind.is_symlink() {
                "link"
            } else if kind.is_dir() {
                pending.push(path);
                "folder"
            } else {
                "file"
            };
            found.push((relative, shown));
        }
    }
    found.sort();
    found
}

#[test]
fn a_confined_agent_cannot_add_shadow_or_link_a_git_object_in_the_shared_store() {
    let case = case();
    fs::write(case.project.join("owner.txt"), "owner\n").unwrap();
    fs::write(case.root.join("loose.txt"), "loose\n").unwrap();
    let loose_source = case.root.join("loose.txt");
    let git = |arguments: &[&str]| {
        let made = owner_git(&case.project, &case.home, arguments);
        assert!(made.status.success(), "{made:?}");
        String::from_utf8(made.stdout).unwrap().trim().to_owned()
    };
    git(&["init", "-q"]);
    git(&["add", "owner.txt"]);
    git(&["commit", "-q", "-m", "start"]);
    git(&["repack", "-a", "-d", "-q"]);
    git(&["prune-packed"]);
    let packed = git(&["rev-parse", "HEAD:owner.txt"]);
    let loose = git(&["hash-object", "-w", loose_source.to_str().unwrap()]);
    assert_ne!(packed[..2], loose[..2]);
    let one = case.project.join("worktrees").join("one");
    git(&[
        "worktree",
        "add",
        "-q",
        "--detach",
        one.to_str().unwrap(),
        "HEAD",
    ]);
    let objects = case.project.join(".git").join("objects");
    let absent = objects.join(&packed[..2]);
    let present = objects.join(&loose[..2]);
    assert!(fs::symlink_metadata(&absent).is_err() && present.is_dir());
    let fresh = "0".repeat(38);
    let staged = case.tmp.join("staged");
    let script = [
        format!("mkdir {}", quoted(&absent)),
        format!("ln -s {} {}", quoted(&case.tmp), quoted(&absent)),
        format!(
            "ln -s {} {}",
            quoted(&case.tmp),
            quoted(&objects.join("notes"))
        ),
        format!(
            "mkdir {staged} && printf x > {staged}/{rest} && mv {staged} {absent}",
            staged = quoted(&staged),
            rest = &packed[2..],
            absent = quoted(&absent)
        ),
        format!("printf x > {}", quoted(&present.join(&fresh))),
        format!(
            "printf x > {tmp} && mv {tmp} {object}",
            tmp = quoted(&present.join("tmp_obj_agent")),
            object = quoted(&present.join(&fresh))
        ),
        format!(
            "printf x > {file} && ln {file} {object}",
            file = quoted(&case.tmp.join("f")),
            object = quoted(&present.join(&fresh))
        ),
        format!(
            "ln -s {} {}",
            quoted(&case.tmp.join("f")),
            quoted(&present.join(&fresh))
        ),
    ]
    .map(|step| format!("({step}) 2>/dev/null; echo tried"))
    .join("\n");
    let sandbox = sandbox(&case, SandboxNetwork::Only(Vec::new()))
        .confine_to_worktree(&one)
        .unwrap();
    let before = object_entries(&objects);
    let tried = run(&sandbox, &case.profiles.join("objects.sb"), &script);
    assert_eq!(
        String::from_utf8_lossy(&tried.stdout),
        "tried\n".repeat(8),
        "{}",
        String::from_utf8_lossy(&tried.stderr)
    );
    assert_eq!(
        object_entries(&objects),
        before,
        "the agent added to the store"
    );
    assert!(fs::symlink_metadata(&absent).is_err());
    assert!(fs::symlink_metadata(objects.join("notes")).is_err());
    assert_eq!(git(&["cat-file", "-p", &packed]), "owner");
    let wrote = run(
        &sandbox,
        &case.profiles.join("objects.sb"),
        &format!(
            "printf mine > {} && echo wrote",
            quoted(&one.join("mine.txt"))
        ),
    );
    assert_eq!(String::from_utf8_lossy(&wrote.stdout), "wrote\n");
    assert_eq!(fs::read_to_string(one.join("mine.txt")).unwrap(), "mine");
}

const AGENT_GIT: &str = "/Library/Developer/CommandLineTools/usr/bin/git";

/// A project whose agent works in its own clone (DEC-102 Own branch): a committed
/// repository with a real `.crazyagent` folder and `<project>/.crazyagent/worktrees/agent-1`
/// cloned from it with `--shared`, so the clone borrows the project's objects, then readied
/// as the engine readies a clone before each launch. Output: the case, the clone and the
/// project's HEAD before the agent ran.
fn clone_case() -> (Case, PathBuf, String) {
    let case = case();
    for arguments in [
        &["init", "-q"][..],
        &["commit", "-q", "--allow-empty", "-m", "start"],
    ] {
        let made = owner_git(&case.project, &case.home, arguments);
        assert!(made.status.success(), "{made:?}");
    }
    let worktrees = case.project.join(".crazyagent").join("worktrees");
    fs::create_dir_all(&worktrees).unwrap();
    let clone = worktrees.join("agent-1");
    let cloned = owner_git(
        &case.project,
        &case.home,
        &[
            "clone",
            "-q",
            "--shared",
            case.project.to_str().unwrap(),
            clone.to_str().unwrap(),
        ],
    );
    assert!(cloned.status.success(), "{cloned:?}");
    crazyagent::landing::ready_agent_clone(&clone).unwrap();
    let head = owner_git(&case.project, &case.home, &["rev-parse", "HEAD"]);
    let head = String::from_utf8(head.stdout).unwrap().trim().to_owned();
    (case, clone.canonicalize().unwrap(), head)
}

#[test]
fn an_own_branch_agent_commits_in_its_own_clone_but_never_writes_the_project_git() {
    assert!(Path::new(AGENT_GIT).is_file());
    let (case, clone, head) = clone_case();
    let profile = case.profiles.join("agent.sb");
    let confined = sandbox(&case, SandboxNetwork::Only(Vec::new()))
        .confine_to_clone(&clone)
        .unwrap();
    let git = format!(
        "{AGENT_GIT} -c user.email=agent@local -c user.name=agent -c core.hooksPath=/dev/null"
    );
    let committed = run(
        &confined,
        &profile,
        &format!(
            "cd {clone} && printf 'agent work\\n' > work.txt && {git} add work.txt && {git} commit -q -m work && {git} checkout -q -b agent/agent-1 && {git} rev-parse HEAD",
            clone = quoted(&clone),
        ),
    );
    assert!(committed.status.success(), "{committed:?}");
    let agent_head = String::from_utf8(committed.stdout)
        .unwrap()
        .trim()
        .to_owned();
    assert_ne!(agent_head, head);
    let project_git = case.project.join(".git");
    for target in [
        project_git.join("planted"),
        project_git.join("objects").join("planted"),
        project_git.join("refs").join("heads").join("planted"),
        case.project.join("planted.txt"),
    ] {
        write_refused(
            &confined,
            &profile,
            &format!("printf x > {}", quoted(&target)),
            &target,
        );
    }
    let pushed = run(
        &confined,
        &profile,
        &format!(
            "cd {} && {git} push -q origin HEAD:refs/heads/planted",
            quoted(&clone)
        ),
    );
    assert!(!pushed.status.success(), "{pushed:?}");
    let after = owner_git(&case.project, &case.home, &["rev-parse", "HEAD"]);
    assert_eq!(String::from_utf8(after.stdout).unwrap().trim(), head);
    let planted = owner_git(
        &case.project,
        &case.home,
        &["rev-parse", "--verify", "-q", "refs/heads/planted"],
    );
    assert!(!planted.status.success());
}

#[test]
fn an_own_branch_agent_never_repoints_the_store_its_clone_borrows() {
    assert!(Path::new(AGENT_GIT).is_file());
    let (case, clone, _head) = clone_case();
    let profile = case.profiles.join("agent.sb");
    let confined = sandbox(&case, SandboxNetwork::Only(Vec::new()))
        .confine_to_clone(&clone)
        .unwrap();
    let git_dir = clone.join(".git");
    let objects = git_dir.join("objects");
    let info = objects.join("info");
    let alternates = info.join("alternates");
    let line = fs::read(&alternates).unwrap();
    assert_eq!(
        line,
        format!("{}\n", case.project.join(".git").join("objects").display()).into_bytes()
    );
    let elsewhere = case.home.join("elsewhere");
    fs::create_dir(&elsewhere).unwrap();
    for (script, target) in [
        (
            format!("printf x > {}", quoted(&info.join("http-alternates"))),
            info.join("http-alternates"),
        ),
        (
            format!(
                "printf x > {}",
                quoted(&git_dir.join("OBJECTS").join("INFO").join("HTTP-ALTERNATES"))
            ),
            info.join("http-alternates"),
        ),
        (
            format!("printf x > {}", quoted(&git_dir.join("commondir"))),
            git_dir.join("commondir"),
        ),
        (
            format!(
                "printf x > {}",
                quoted(&clone.join(".GIT").join("COMMONDIR"))
            ),
            git_dir.join("commondir"),
        ),
        (
            format!(
                "ln -s {} {}",
                quoted(&elsewhere),
                quoted(&git_dir.join("OBJECTS").join("cd"))
            ),
            objects.join("cd").join("elsewhere"),
        ),
        (
            format!(
                "ln -s {} {}",
                quoted(&elsewhere),
                quoted(&objects.join("ab").join("0123"))
            ),
            objects.join("ab").join("0123"),
        ),
        (
            format!(
                "rmdir {ab} && ln -s {} {ab}",
                quoted(&elsewhere),
                ab = quoted(&objects.join("ab"))
            ),
            objects.join("ab").join("elsewhere"),
        ),
        (
            format!("mv {} {}", quoted(&info), quoted(&objects.join("moved"))),
            objects.join("moved"),
        ),
        (
            format!("mv {} {}", quoted(&objects), quoted(&git_dir.join("moved"))),
            git_dir.join("moved"),
        ),
    ] {
        write_refused(&confined, &profile, &script, &target);
    }
    for fanout in ["ab", "cd"] {
        assert!(fs::symlink_metadata(objects.join(fanout)).unwrap().is_dir());
    }
    for script in [
        format!(
            "printf '%s\\n' {} >> {}",
            quoted(&elsewhere),
            quoted(&alternates)
        ),
        format!("printf x > {}", quoted(&alternates)),
        format!(
            "printf x > {}",
            quoted(&git_dir.join("OBJECTS").join("INFO").join("ALTERNATES"))
        ),
        format!("rm -f {}", quoted(&alternates)),
        format!(
            "printf x > {} && mv {} {}",
            quoted(&case.tmp.join("swap")),
            quoted(&case.tmp.join("swap")),
            quoted(&alternates)
        ),
    ] {
        let output = run(&confined, &profile, &script);
        assert!(!output.status.success(), "{script}");
        assert_eq!(fs::read(&alternates).unwrap(), line, "{script}");
    }
    let git = format!(
        "{AGENT_GIT} -c user.email=agent@local -c user.name=agent -c core.hooksPath=/dev/null"
    );
    let committed = run(
        &confined,
        &profile,
        &format!(
            "cd {} && printf 'agent work\\n' > work.txt && {git} add work.txt && {git} commit -q -m work && {git} repack -q -a -d",
            quoted(&clone),
        ),
    );
    assert!(
        committed.status.success(),
        "the clone's own objects and packs stay writable: {committed:?}"
    );
    assert_eq!(fs::read(&alternates).unwrap(), line);
}

#[test]
fn an_own_branch_agent_never_redirects_or_replaces_its_clones_git_folder() {
    assert!(Path::new(AGENT_GIT).is_file());
    let (case, clone, _head) = clone_case();
    let profile = case.profiles.join("agent.sb");
    let confined = sandbox(&case, SandboxNetwork::Only(Vec::new()))
        .confine_to_clone(&clone)
        .unwrap();
    let git_dir = clone.join(".git");
    let elsewhere = clone.join("elsewhere");
    fs::create_dir(&elsewhere).unwrap();
    for (script, target) in [
        (
            format!("printf x > {}", quoted(&git_dir.join("commondir"))),
            git_dir.join("commondir"),
        ),
        (
            format!("printf 'gitdir: x\\n' > {}", quoted(&git_dir.join(".git"))),
            git_dir.join(".git"),
        ),
        (
            format!("mkdir {}", quoted(&git_dir.join(".git"))),
            git_dir.join(".git"),
        ),
        (
            format!(
                "ln -s {} {}",
                quoted(&elsewhere),
                quoted(&git_dir.join(".git"))
            ),
            git_dir.join(".git"),
        ),
        (
            format!("mv {} {}", quoted(&git_dir), quoted(&clone.join("moved"))),
            clone.join("moved"),
        ),
        (
            format!(
                "mv {} {}",
                quoted(&clone.join(".GIT")),
                quoted(&clone.join("moved-upper"))
            ),
            clone.join("moved-upper"),
        ),
    ] {
        write_refused(&confined, &profile, &script, &target);
        assert!(
            fs::symlink_metadata(&git_dir).is_ok_and(|found| found.is_dir()),
            "{script}"
        );
    }
    let git = format!(
        "{AGENT_GIT} -c user.email=agent@local -c user.name=agent -c core.hooksPath=/dev/null"
    );
    let committed = run(
        &confined,
        &profile,
        &format!(
            "cd {} && printf 'agent work\\n' > work.txt && {git} add work.txt && {git} commit -q -m work",
            quoted(&clone),
        ),
    );
    assert!(committed.status.success(), "{committed:?}");
}

#[test]
fn an_own_branch_agent_never_plants_the_fallback_repository_upload_pack_tries() {
    let (case, clone, _head) = clone_case();
    let profile = case.profiles.join("agent.sb");
    let confined = sandbox(&case, SandboxNetwork::Only(Vec::new()))
        .confine_to_clone(&clone)
        .unwrap();
    let fallback = clone.join(".git.git");
    let elsewhere = clone.join("elsewhere");
    fs::create_dir(&elsewhere).unwrap();
    for script in [
        format!("mkdir {}", quoted(&fallback)),
        format!("mkdir {}", quoted(&clone.join(".GIT.GIT"))),
        format!("printf 'gitdir: x\\n' > {}", quoted(&fallback)),
        format!("ln -s {} {}", quoted(&elsewhere), quoted(&fallback)),
        format!(
            "mkdir {} && mv {} {}",
            quoted(&elsewhere.join("repo")),
            quoted(&elsewhere.join("repo")),
            quoted(&fallback)
        ),
    ] {
        write_refused(&confined, &profile, &script, &fallback);
    }
}

#[test]
fn an_own_branch_agent_never_writes_a_multi_pack_index_into_its_clone() {
    assert!(Path::new(AGENT_GIT).is_file());
    let (case, clone, _head) = clone_case();
    let profile = case.profiles.join("agent.sb");
    let confined = sandbox(&case, SandboxNetwork::Only(Vec::new()))
        .confine_to_clone(&clone)
        .unwrap();
    let pack = clone.join(".git").join("objects").join("pack");
    fs::create_dir_all(&pack).unwrap();
    let git = format!(
        "{AGENT_GIT} -c user.email=agent@local -c user.name=agent -c core.hooksPath=/dev/null"
    );
    let committed = run(
        &confined,
        &profile,
        &format!(
            "cd {} && printf 'agent work\\n' > work.txt && {git} add work.txt && {git} commit -q -m work && {git} repack -q -a -d",
            quoted(&clone),
        ),
    );
    assert!(committed.status.success(), "{committed:?}");
    for (script, target) in [
        (
            format!("printf x > {}", quoted(&pack.join("multi-pack-index"))),
            pack.join("multi-pack-index"),
        ),
        (
            format!("printf x > {}", quoted(&pack.join("MULTI-PACK-INDEX"))),
            pack.join("multi-pack-index"),
        ),
        (
            format!(
                "printf x > {}",
                quoted(&pack.join("multi-pack-index-0123.bitmap"))
            ),
            pack.join("multi-pack-index-0123.bitmap"),
        ),
        (
            format!("mkdir {}", quoted(&pack.join("multi-pack-index.d"))),
            pack.join("multi-pack-index.d"),
        ),
        (
            format!("cd {} && {git} multi-pack-index write", quoted(&clone)),
            pack.join("multi-pack-index"),
        ),
    ] {
        write_refused(&confined, &profile, &script, &target);
    }
}

/// An Own branch agent's clone the engine made and checked once (DEC-102), for the tests
/// of what an agent plants after that check: a committed project, `add_agent_clone`,
/// `record_worktree_start`, the agent's sandbox, its first commit and the turn end where
/// the engine checks the clone's store and fetches it.
struct CheckedClone {
    case: Case,
    clone: PathBuf,
    git_dir: PathBuf,
    db: rusqlite::Connection,
    workspace: crazyagent::landing::Workspace,
    confined: ProjectSandbox,
    profile: PathBuf,
    git: String,
}

impl CheckedClone {
    /// Makes and checks the clone. Output: the clone and the change set its first turn
    /// end made.
    fn new() -> (Self, crazyagent::landing::ChangeSetId) {
        use crazyagent::landing::{AgentKind, ChangeAgent, Workspace};
        assert!(Path::new(AGENT_GIT).is_file());
        let case = case();
        for arguments in [
            &["init", "-q"][..],
            &["config", "user.email", "owner@local"],
            &["config", "user.name", "owner"],
            &["commit", "-q", "--allow-empty", "-m", "start"],
        ] {
            let made = owner_git(&case.project, &case.home, arguments);
            assert!(made.status.success(), "{made:?}");
        }
        let worktrees = case.project.join(".crazyagent").join("worktrees");
        fs::create_dir_all(&worktrees).unwrap();
        let clone = worktrees.join("agent-1");
        crazyagent::landing::add_agent_clone(&case.project, &clone).unwrap();
        let clone = clone.canonicalize().unwrap();
        let git_dir = crazyagent::agent_git::git_dir(&clone);
        let engine = case.root.join("engine");
        fs::create_dir(&engine).unwrap();
        let db = crazyagent::landing::open(&engine).unwrap();
        crazyagent::landing::record_worktree_start(&db, &case.project, &clone, &git_dir).unwrap();
        let workspace = Workspace {
            project: crazyagent::protocol::ProjectId::new("project-1").unwrap(),
            agent: ChangeAgent {
                kind: AgentKind::Sub,
                id: "1".into(),
                label: "Agent 1".into(),
            },
            repository: case.project.clone(),
            worktree: clone.clone(),
            git_dir: git_dir.clone(),
        };
        let profile = case.profiles.join("agent.sb");
        let confined = sandbox(&case, SandboxNetwork::Only(Vec::new()))
            .confine_to_clone(&clone)
            .unwrap();
        let checked = Self {
            case,
            clone,
            git_dir,
            db,
            workspace,
            confined,
            profile,
            git: format!(
                "{AGENT_GIT} -c user.email=agent@local -c user.name=agent -c core.hooksPath=/dev/null"
            ),
        };
        checked.commit("one.txt");
        let set = checked
            .turn_end("turn-1")
            .expect("the engine checked and fetched the clone");
        (checked, set)
    }

    /// Runs a script as the agent, under its profile. Input: the script. Output: its result.
    fn agent(&self, script: &str) -> Output {
        run(&self.confined, &self.profile, script)
    }

    /// Commits one new file as the agent. Input: the file name.
    fn commit(&self, file: &str) {
        let committed = self.agent(&format!(
            "cd {} && printf 'agent work\\n' > {file} && {git} add {file} && {git} commit -q -m {file}",
            quoted(&self.clone),
            git = self.git,
        ));
        assert!(committed.status.success(), "{committed:?}");
    }

    /// Ends one successful turn, where the engine checks the clone's store and fetches its
    /// commits. Input: the turn name. Output: the change set the work belongs to.
    fn turn_end(&self, name: &str) -> Option<crazyagent::landing::ChangeSetId> {
        use crazyagent::landing::{TurnEnd, TurnOutcome};
        crazyagent::landing::record_turn_end(
            &self.db,
            &self.workspace,
            &TurnEnd {
                outcome: TurnOutcome::Succeeded,
                turn: name.into(),
                prompt_line: "Write work".into(),
            },
        )
        .unwrap()
    }

    /// Makes a second commit and turn end, then checks that the engine's check passed and
    /// the project's one change set ends at the clone's HEAD, so only the agent's own
    /// commits landed. Input: the set the first turn end made.
    fn lands_only_its_own_commits(&self, set: &crazyagent::landing::ChangeSetId) {
        self.commit("two.txt");
        let again = self
            .turn_end("turn-2")
            .expect("the next check passes: nothing foreign was planted");
        assert_eq!(&again, set);
        let head = self.agent(&format!(
            "cd {} && {} rev-parse HEAD",
            quoted(&self.clone),
            self.git
        ));
        let head = String::from_utf8(head.stdout).unwrap().trim().to_owned();
        let sets = crazyagent::landing::list(&self.db, &self.workspace.project).unwrap();
        assert_eq!(sets.len(), 1, "{sets:#?}");
        assert_eq!(
            sets[0].head, head,
            "the set holds only the agent's own commits"
        );
    }
}

#[test]
fn a_multi_pack_index_an_agent_plants_after_the_engine_checked_its_clone_never_lands() {
    let (checked, set) = CheckedClone::new();
    let git = &checked.git;
    let pack = checked.git_dir.join("objects").join("pack");
    let index = pack.join("multi-pack-index");
    for script in [
        format!("printf x > {}", quoted(&index)),
        format!("printf x > {}", quoted(&pack.join("MULTI-PACK-INDEX"))),
        format!(
            "cd {} && {git} repack -q -a -d && {git} multi-pack-index write",
            quoted(&checked.clone)
        ),
    ] {
        write_refused(&checked.confined, &checked.profile, &script, &index);
    }
    checked.lands_only_its_own_commits(&set);
    assert!(!index.exists());
}

/// A pack and a loose object in a repository outside the project, the objects an agent
/// would slip into its clone's store. Input: the case. Output: the pack file, its index,
/// the loose object's file and its id, and the id of a commit only the pack holds.
fn foreign_objects(case: &Case) -> (PathBuf, PathBuf, PathBuf, String, String) {
    let foreign = case.outside.join("foreign");
    fs::create_dir(&foreign).unwrap();
    let git = |arguments: &[&str]| {
        let done = owner_git(&foreign, &case.home, arguments);
        assert!(done.status.success(), "{done:?}");
        String::from_utf8(done.stdout).unwrap().trim().to_owned()
    };
    git(&["init", "-q"]);
    fs::write(foreign.join("foreign.txt"), "foreign work\n").unwrap();
    git(&["add", "foreign.txt"]);
    git(&["commit", "-q", "-m", "foreign"]);
    let commit = git(&["rev-parse", "HEAD"]);
    git(&["repack", "-q", "-a", "-d"]);
    fs::write(foreign.join("loose.txt"), "foreign loose\n").unwrap();
    let loose = git(&["hash-object", "-w", "loose.txt"]);
    let objects = foreign.join(".git").join("objects");
    let named = |suffix: &str| {
        fs::read_dir(objects.join("pack"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| path.to_str().unwrap().ends_with(suffix))
            .unwrap()
    };
    let file = objects.join(&loose[..2]).join(&loose[2..]);
    assert!(file.is_file());
    (named(".pack"), named(".idx"), file, loose, commit)
}

#[test]
fn a_folder_an_agent_moves_into_its_clones_store_after_the_engine_checked_it_never_lands() {
    let (checked, set) = CheckedClone::new();
    let (pack_file, index_file, loose_file, loose, commit) = foreign_objects(&checked.case);
    let objects = checked.git_dir.join("objects");
    let fanout = objects.join(&loose[..2]);
    assert!(objects.join("pack").is_dir() && fanout.is_dir());
    let stage = checked.clone.join("stage");
    let staged_pack = stage.join("pack");
    let staged_fanout = stage.join(&loose[..2]);
    let pack_name = pack_file.file_name().unwrap().to_str().unwrap();
    let index_name = index_file.file_name().unwrap().to_str().unwrap();
    let staged = checked.agent(&format!(
        "mkdir -p {sp} {sf} && ln -s {pf} {sp}/{pack_name} && ln -s {xf} {sp}/{index_name} && printf x > {sp}/multi-pack-index && ln -s {lf} {sf}/{rest} && mkdir {stage}/control && /usr/bin/perl -e 'rename($ARGV[0], $ARGV[1]) or exit 1' {stage}/control {stage}/renamed",
        sp = quoted(&staged_pack),
        sf = quoted(&staged_fanout),
        pf = quoted(&pack_file),
        xf = quoted(&index_file),
        lf = quoted(&loose_file),
        rest = &loose[2..],
        stage = quoted(&stage),
    ));
    assert!(staged.status.success(), "{staged:?}");
    assert!(
        stage.join("renamed").is_dir(),
        "perl renames a folder in the sandbox"
    );
    let rename = |from: &Path, to: &Path| {
        format!(
            "/usr/bin/perl -e 'rename($ARGV[0], $ARGV[1]) or exit 1' {} {}",
            quoted(from),
            quoted(to)
        )
    };
    let pack = objects.join("pack");
    let planted_pack = pack.join(pack_name);
    let planted_loose = fanout.join(&loose[2..]);
    let upper = objects.join(loose[..2].to_uppercase());
    for (script, target) in [
        (
            format!(
                "mv {} {}",
                quoted(&pack),
                quoted(&objects.join("pack-away"))
            ),
            objects.join("pack-away"),
        ),
        (
            format!(
                "mv {} {} && mv {} {}",
                quoted(&pack),
                quoted(&objects.join("pack-away")),
                quoted(&staged_pack),
                quoted(&pack)
            ),
            planted_pack.clone(),
        ),
        (rename(&staged_pack, &pack), planted_pack.clone()),
        (format!("rmdir {}", quoted(&fanout)), stage.join("never")),
        (rename(&staged_fanout, &fanout), planted_loose.clone()),
        (
            format!("mv {} {}", quoted(&upper), quoted(&stage.join("moved"))),
            stage.join("moved"),
        ),
        (
            format!(
                "rmdir {} && mv {} {}",
                quoted(&fanout),
                quoted(&staged_fanout),
                quoted(&fanout)
            ),
            planted_loose.clone(),
        ),
    ] {
        write_refused(&checked.confined, &checked.profile, &script, &target);
    }
    assert!(pack.is_dir() && fanout.is_dir());
    assert!(!pack.join("multi-pack-index").exists());
    fs::remove_dir_all(&stage).unwrap();
    checked.lands_only_its_own_commits(&set);
    for id in [loose.as_str(), commit.as_str()] {
        let found = owner_git(
            &checked.case.project,
            &checked.case.home,
            &["cat-file", "-e", id],
        );
        assert!(
            !found.status.success(),
            "the project store never holds {id}"
        );
    }
}

#[test]
fn an_own_branch_agent_keeps_commit_repack_gc_prune_and_fsck_in_its_clone() {
    assert!(Path::new(AGENT_GIT).is_file());
    let (case, clone, _head) = clone_case();
    let objects = clone.join(".git").join("objects");
    let profile = case.profiles.join("agent.sb");
    let confined = sandbox(&case, SandboxNetwork::Only(Vec::new()))
        .confine_to_clone(&clone)
        .unwrap();
    let made = fs::read_dir(&objects)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .filter(|name| name.len() == 2 && name.chars().all(|c| c.is_ascii_hexdigit()))
        .count();
    assert_eq!(made, 256, "the engine made every fan-out folder");
    assert!(objects.join("pack").is_dir());
    let git = format!(
        "{AGENT_GIT} -c user.email=agent@local -c user.name=agent -c core.hooksPath=/dev/null"
    );
    let kept = run(
        &confined,
        &profile,
        &format!(
            "cd {} && printf 'w\\n' > w && {git} add w && {git} commit -q -m w && printf 'v\\n' > v && {git} add v && {git} commit -q -m v && {git} repack -q -a -d && {git} gc -q && {git} prune --expire=now && {git} fsck --no-progress && printf 'z\\n' > z && {git} add z && {git} commit -q -m z && {git} cat-file -e HEAD",
            quoted(&clone),
        ),
    );
    assert!(kept.status.success(), "{kept:?}");
    assert!(objects.join("pack").is_dir());
}

#[test]
fn a_sandboxed_agent_in_a_linked_worktree_cannot_commit() {
    assert!(Path::new(AGENT_GIT).is_file());
    let case = case();
    for arguments in [
        &["init", "-q"][..],
        &["commit", "-q", "--allow-empty", "-m", "start"],
    ] {
        let made = owner_git(&case.project, &case.home, arguments);
        assert!(made.status.success(), "{made:?}");
    }
    let worktrees = case.project.join(".crazyagent").join("worktrees");
    fs::create_dir_all(&worktrees).unwrap();
    let worktree = worktrees.join("agent-1");
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
    let worktree = worktree.canonicalize().unwrap();
    let head = owner_git(&case.project, &case.home, &["rev-parse", "HEAD"]);
    let profile = case.profiles.join("agent.sb");
    let confined = sandbox(&case, SandboxNetwork::Only(Vec::new()))
        .confine_to_worktree(&worktree)
        .unwrap();
    assert!(matches!(
        sandbox(&case, SandboxNetwork::Only(Vec::new()))
            .confine_to_worktree(&worktree)
            .unwrap()
            .confine_to_clone(&worktree),
        Err(SandboxError::Invalid(_))
    ));
    let git = format!("{AGENT_GIT} -c user.email=agent@local -c user.name=agent");
    let edited = run(
        &confined,
        &profile,
        &format!(
            "printf 'agent work\\n' > {}",
            quoted(&worktree.join("work.txt"))
        ),
    );
    assert!(edited.status.success(), "{edited:?}");
    let committed = run(
        &confined,
        &profile,
        &format!(
            "cd {} && {git} add work.txt && {git} commit -q -m work",
            quoted(&worktree)
        ),
    );
    assert!(!committed.status.success(), "{committed:?}");
    let after = owner_git(&case.project, &case.home, &["rev-parse", "HEAD"]);
    assert_eq!(after.stdout, head.stdout);
}

#[test]
fn an_agent_cannot_exec_an_app_bundle_binary_in_any_case_but_bundle_tools_still_run() {
    let case = case();
    let sandbox = sandbox(&case, SandboxNetwork::Only(Vec::new()));
    let text = sandbox.profile();
    let allow = text.find("(allow process-exec)\n").unwrap();
    let deny = text.find("(deny process-exec\n").unwrap();
    assert!(allow < deny, "{text}");
    let blender = "/Applications/Blender.app/Contents/MacOS/Blender";
    assert!(Path::new(blender).is_file(), "the probe needs {blender}");
    let profile = case.profiles.join("apps.sb");
    for app in [
        blender,
        "/applications/blender.APP/contents/MACOS/Blender",
        "/System/Volumes/Data/Applications/Blender.app/Contents/MacOS/Blender",
        "/system/volumes/data/Applications/Blender.app/Contents/MacOS/Blender",
    ] {
        let output = run(
            &sandbox,
            &profile,
            &format!("{} --version", quoted(Path::new(app))),
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!output.status.success(), "{app} ran");
        assert!(
            stderr.contains("Operation not permitted"),
            "{app}: {stderr}"
        );
        assert!(output.stdout.is_empty(), "{app}");
    }
    for (tool, expected) in [
        (
            "/Applications/Docker.app/Contents/Resources/bin/docker",
            "Docker version",
        ),
        (
            "/Applications/Xcode.app/Contents/Developer/usr/bin/git",
            "git version",
        ),
    ] {
        let output = run(
            &sandbox,
            &profile,
            &format!("{} --version", quoted(Path::new(tool))),
        );
        assert!(
            output.status.success(),
            "{tool}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            String::from_utf8_lossy(&output.stdout).starts_with(expected),
            "{tool}"
        );
    }
}

/// OP-9 r1 (leader 14:29): a native lead's teammates write only the teammates' folder of the
/// lead's clone and the git parts their own commits need. A teammate commits in the worktree
/// the engine made for it on its own `<task>--<name>` branch, but never writes the lead's
/// files, the clone's hooks, config or HEAD, the lead's task branch, the links that tie a
/// teammate worktree to the clone, or a new worktree record.
#[test]
fn a_native_lead_s_teammates_write_only_their_worktrees_in_the_lead_s_clone() {
    use std::os::unix::fs::PermissionsExt;
    assert!(Path::new(AGENT_GIT).is_file());
    let (case, clone, _head) = clone_case();
    let task = "crazyagent/write-the-page";
    let switched = owner_git(&clone, &case.home, &["switch", "-q", "-c", task]);
    assert!(switched.status.success(), "{switched:?}");
    let tmux = crazyagent::terminal::tmux::installed(&case.project).expect("tmux is installed");
    let tmp = crazyagent::sandbox::toolchains::make_tmp(&case.home).unwrap();
    for folder in crazyagent::sandbox::CLAUDE_TEAM_SHARED {
        fs::create_dir_all(case.home.join(folder)).unwrap();
    }
    let mates_home = case.home.with_file_name("mates-home");
    fs::create_dir_all(&mates_home).unwrap();
    fs::set_permissions(&mates_home, fs::Permissions::from_mode(0o700)).unwrap();
    let mailbox = case.tmp.join(crazyagent::team::tool::MAILBOX_FOLDER);
    fs::create_dir(&mailbox).unwrap();
    fs::set_permissions(&mailbox, fs::Permissions::from_mode(0o700)).unwrap();
    let (lead, teammates) = sandbox(&case, SandboxNetwork::Only(Vec::new()))
        .allow_scratch(&tmp)
        .unwrap()
        .confine_to_clone(&clone)
        .unwrap()
        .lead_branch(task)
        .unwrap()
        .native_team(
            &tmp.join("tmux.sock"),
            &tmp.join("teammates-0123456789abcdef.sock"),
            &tmp.join(crazyagent::team::caller::CALLER_SOCKET),
            &tmux,
            &mates_home,
        )
        .unwrap();
    let (lead_profile, teammates_profile) = (
        case.profiles.join("lead.sb"),
        case.profiles.join("teammates.sb"),
    );
    let greet = clone
        .join(crazyagent::agent_git::TEAMMATES_FOLDER)
        .join("greet");
    let added = run(
        &lead,
        &lead_profile,
        &format!(
            "cd {} && {AGENT_GIT} -c core.hooksPath=/dev/null worktree add -q -b {task}--greet {} HEAD",
            quoted(&clone),
            quoted(&greet)
        ),
    );
    assert!(added.status.success(), "{added:?}");
    let committed = run(
        &teammates,
        &teammates_profile,
        &format!(
            "cd {} && printf 'hello\\n' > greet.txt && {AGENT_GIT} add greet.txt && {AGENT_GIT} -c user.name=greet -c user.email=greet@local -c core.hooksPath=/dev/null commit -q -m greet",
            quoted(&greet)
        ),
    );
    assert!(
        committed.status.success(),
        "a teammate commits in its own worktree: {}",
        String::from_utf8_lossy(&committed.stderr)
    );
    let teammate_tip = owner_git(
        &clone,
        &case.home,
        &["log", "-1", "--format=%s", &format!("{task}--greet")],
    );
    assert_eq!(
        String::from_utf8_lossy(&teammate_tip.stdout).trim(),
        "greet"
    );
    let git = clone.join(".git");
    let kept: Vec<(PathBuf, Vec<u8>)> = [
        git.join("config"),
        git.join("HEAD"),
        git.join("refs").join("heads").join(task),
        greet.join(".git"),
        git.join("worktrees").join("greet").join("gitdir"),
    ]
    .into_iter()
    .map(|file| {
        let bytes = fs::read(&file).unwrap();
        (file, bytes)
    })
    .collect();
    for (file, before) in &kept {
        let output = run(
            &teammates,
            &teammates_profile,
            &format!("printf 'x\\n' >> {}", quoted(file)),
        );
        assert!(!output.status.success(), "{}", file.display());
        assert_eq!(&fs::read(file).unwrap(), before, "{}", file.display());
    }
    for target in [
        clone.join("lead.txt"),
        git.join("hooks").join("post-commit"),
    ] {
        write_refused(
            &teammates,
            &teammates_profile,
            &format!("printf x > {}", quoted(&target)),
            &target,
        );
    }
    let admin = git.join("worktrees").join("other");
    write_refused(
        &teammates,
        &teammates_profile,
        &format!("mkdir {}", quoted(&admin)),
        &admin,
    );
    let record = git.join("worktrees").join("greet");
    let links: Vec<(PathBuf, Vec<u8>)> = [
        record.join("gitdir"),
        record.join("commondir"),
        greet.join(".git"),
    ]
    .into_iter()
    .map(|file| {
        let bytes = fs::read(&file).unwrap();
        (file, bytes)
    })
    .collect();
    let lead_only = clone.join("lead-only");
    fs::create_dir(&lead_only).unwrap();
    let folder = clone.join(crazyagent::agent_git::TEAMMATES_FOLDER);
    let [bob, carl, dave, erin] = ["bob", "carl", "dave", "erin"].map(|name| folder.join(name));
    let planted = case.home.join("teams").join("planted.lock");
    let mut escaped: Vec<String> = Vec::new();
    for (label, script) in [
        (
            "GITDIR",
            format!("printf 'x\\n' >> {}", quoted(&record.join("GITDIR"))),
        ),
        (
            "COMMONDIR",
            format!("printf 'x\\n' >> {}", quoted(&record.join("COMMONDIR"))),
        ),
        (
            ".GIT",
            format!("printf 'x\\n' >> {}", quoted(&greet.join(".GIT"))),
        ),
        (
            ".GIT made in a new teammate folder",
            format!(
                "mkdir {} && printf 'gitdir: {}\\n' > {}",
                quoted(&erin),
                git.display(),
                quoted(&erin.join(".GIT"))
            ),
        ),
        (
            "symlink at a teammate folder",
            format!("ln -s {} {}", quoted(&lead_only), quoted(&bob)),
        ),
        (
            "symlink moved in as a teammate folder",
            format!(
                "ln -s {} {} && mv {} {}",
                quoted(&lead_only),
                quoted(&greet.join("link")),
                quoted(&greet.join("link")),
                quoted(&carl)
            ),
        ),
        (
            "team folder holding a .git moved in as a teammate folder",
            format!(
                "mkdir {} && printf 'gitdir: {}\\n' > {} && mv {} {}",
                quoted(&planted),
                git.display(),
                quoted(&planted.join(".git")),
                quoted(&planted),
                quoted(&dave)
            ),
        ),
    ] {
        if run(&teammates, &teammates_profile, &script)
            .status
            .success()
        {
            escaped.push(label.to_owned());
        }
    }
    for (file, before) in &links {
        if &fs::read(file).unwrap() != before {
            escaped.push(format!("changed {}", file.display()));
        }
    }
    for made in [&bob, &carl, &dave, &erin.join(".git")] {
        if fs::symlink_metadata(made).is_ok() {
            escaped.push(format!("made {}", made.display()));
        }
    }
    let replaced = run(
        &teammates,
        &teammates_profile,
        &format!(
            "mv {} {} && ln -s {} {}",
            quoted(&folder),
            quoted(&record.join("moved")),
            quoted(&lead_only),
            quoted(&folder)
        ),
    );
    if replaced.status.success() {
        escaped.push("teammates folder entry replaced".to_owned());
    }
    if !fs::symlink_metadata(&folder).unwrap().is_dir() || !greet.join(".git").is_file() {
        escaped.push(format!("moved {}", folder.display()));
    }
    escaped.extend(sibling_folder_escapes(
        &teammates,
        &teammates_profile,
        &folder,
        &tmp,
    ));
    assert!(
        escaped.is_empty(),
        "a teammate changes what ties its worktree to the clone: {escaped:?}"
    );
    assert!(
        fs::read_dir(&lead_only).unwrap().next().is_none(),
        "nothing lands in the lead's folder"
    );
}

/// AGENT-REF-ALLOW-NARROW, a tightening: a clone lead's teammates write only the refs of
/// `<lead's branch>--<name>` teammate branches. The lead's branch has a teammate branch's
/// shape here only to pin the rule, since a native lead is master-started on
/// `crazyagent/<task>`. A teammate commits on its own branch, but never writes the lead's
/// branch, its reflog or lock, nor another task's teammate branch.
#[test]
fn a_native_lead_s_teammates_never_move_a_lead_branch_of_teammate_shape() {
    use std::os::unix::fs::PermissionsExt;
    assert!(Path::new(AGENT_GIT).is_file());
    let (case, clone, _head) = clone_case();
    let task = "crazyagent/write-the-page--lead";
    let switched = owner_git(&clone, &case.home, &["switch", "-q", "-c", task]);
    assert!(switched.status.success(), "{switched:?}");
    let tmux = crazyagent::terminal::tmux::installed(&case.project).expect("tmux is installed");
    let tmp = crazyagent::sandbox::toolchains::make_tmp(&case.home).unwrap();
    for folder in crazyagent::sandbox::CLAUDE_TEAM_SHARED {
        fs::create_dir_all(case.home.join(folder)).unwrap();
    }
    let mates_home = case.home.with_file_name("mates-home");
    fs::create_dir_all(&mates_home).unwrap();
    fs::set_permissions(&mates_home, fs::Permissions::from_mode(0o700)).unwrap();
    let mailbox = case.tmp.join(crazyagent::team::tool::MAILBOX_FOLDER);
    fs::create_dir(&mailbox).unwrap();
    fs::set_permissions(&mailbox, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(
        sandbox(&case, SandboxNetwork::Only(Vec::new()))
            .lead_branch(task)
            .is_err(),
        "only a clone lead's sandbox names its branch"
    );
    for branch in ["main", "agent/agent-1", "crazyagent/write-the-page/lead"] {
        assert!(
            sandbox(&case, SandboxNetwork::Only(Vec::new()))
                .confine_to_clone(&clone)
                .unwrap()
                .lead_branch(branch)
                .is_err(),
            "{branch}"
        );
    }
    let (lead, teammates) = sandbox(&case, SandboxNetwork::Only(Vec::new()))
        .allow_scratch(&tmp)
        .unwrap()
        .confine_to_clone(&clone)
        .unwrap()
        .lead_branch(task)
        .unwrap()
        .native_team(
            &tmp.join("tmux.sock"),
            &tmp.join("teammates-0123456789abcdef.sock"),
            &tmp.join(crazyagent::team::caller::CALLER_SOCKET),
            &tmux,
            &mates_home,
        )
        .unwrap();
    let (lead_profile, teammates_profile) = (
        case.profiles.join("lead.sb"),
        case.profiles.join("teammates.sb"),
    );
    let greet = clone
        .join(crazyagent::agent_git::TEAMMATES_FOLDER)
        .join("greet");
    let added = run(
        &lead,
        &lead_profile,
        &format!(
            "cd {} && {AGENT_GIT} -c core.hooksPath=/dev/null worktree add -q -b {task}--greet {} HEAD",
            quoted(&clone),
            quoted(&greet)
        ),
    );
    assert!(added.status.success(), "{added:?}");
    let committed = run(
        &teammates,
        &teammates_profile,
        &format!(
            "cd {} && printf 'hello\\n' > greet.txt && {AGENT_GIT} add greet.txt && {AGENT_GIT} -c user.name=greet -c user.email=greet@local -c core.hooksPath=/dev/null commit -q -m greet",
            quoted(&greet)
        ),
    );
    assert!(
        committed.status.success(),
        "a teammate commits on its own branch: {}",
        String::from_utf8_lossy(&committed.stderr)
    );
    let git = clone.join(".git");
    let (heads, logs) = (
        git.join("refs").join("heads"),
        git.join("logs").join("refs").join("heads"),
    );
    for file in [heads.join(task), logs.join(task)] {
        let before = fs::read(&file).unwrap();
        let output = run(
            &teammates,
            &teammates_profile,
            &format!("printf 'x\\n' >> {}", quoted(&file)),
        );
        assert!(!output.status.success(), "{}", file.display());
        assert_eq!(fs::read(&file).unwrap(), before, "{}", file.display());
    }
    for target in [
        heads.join(format!("{task}.lock")),
        heads.join("crazyagent").join("other-task--x"),
        logs.join("crazyagent").join("other-task--x"),
    ] {
        write_refused(
            &teammates,
            &teammates_profile,
            &format!("printf x > {}", quoted(&target)),
            &target,
        );
    }
}

/// AGENT-REF-ALLOW-NARROW: the engine's setup git for a linked lead's native teammates
/// writes the refs of that lead's own teammate branches, `<lead's branch>--<name>`, and
/// nothing else under `crazyagent/`: not the lead's branch when it has a teammate branch's
/// shape, its reflog or lock, nor another task's teammate branch.
#[test]
fn the_teammates_setup_git_writes_only_its_lead_s_teammate_branches() {
    assert!(Path::new(AGENT_GIT).is_file());
    let case = case();
    for arguments in [
        &["init", "-q"][..],
        &["commit", "-q", "--allow-empty", "-m", "start"],
    ] {
        let made = owner_git(&case.project, &case.home, arguments);
        assert!(made.status.success(), "{made:?}");
    }
    let worktrees = case.project.join(".crazyagent").join("worktrees");
    fs::create_dir_all(&worktrees).unwrap();
    let lead_worktree = worktrees.join("agent-1");
    let task = "crazyagent/write-the-page--lead";
    let added = owner_git(
        &case.project,
        &case.home,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            task,
            lead_worktree.to_str().unwrap(),
            "HEAD",
        ],
    );
    assert!(added.status.success(), "{added:?}");
    for branch in ["main", "agent/agent-1", "crazyagent/write-the-page/lead"] {
        assert!(
            sandbox(&case, SandboxNetwork::Only(Vec::new()))
                .confine_to_teammates_setup(&lead_worktree, branch)
                .is_err(),
            "{branch}"
        );
    }
    let setup = sandbox(&case, SandboxNetwork::Only(Vec::new()))
        .confine_to_teammates_setup(&lead_worktree, task)
        .unwrap();
    let profile = case.profiles.join("setup.sb");
    let greet = lead_worktree
        .join(crazyagent::agent_git::TEAMMATES_FOLDER)
        .join("greet");
    let made = run(
        &setup,
        &profile,
        &format!(
            "cd {} && {AGENT_GIT} -c core.hooksPath=/dev/null worktree add -q -b {task}--greet {} HEAD",
            quoted(&lead_worktree),
            quoted(&greet)
        ),
    );
    assert!(
        made.status.success(),
        "the setup git adds a teammate's worktree: {}",
        String::from_utf8_lossy(&made.stderr)
    );
    let git = case.project.join(".git");
    let (heads, logs) = (
        git.join("refs").join("heads"),
        git.join("logs").join("refs").join("heads"),
    );
    assert!(heads.join(format!("{task}--greet")).is_file());
    for file in [heads.join(task), logs.join(task)] {
        let before = fs::read(&file).unwrap();
        let output = run(
            &setup,
            &profile,
            &format!("printf 'x\\n' >> {}", quoted(&file)),
        );
        assert!(!output.status.success(), "{}", file.display());
        assert_eq!(fs::read(&file).unwrap(), before, "{}", file.display());
    }
    for target in [
        heads.join(format!("{task}.lock")),
        heads.join("crazyagent").join("other-task--x"),
        logs.join("crazyagent").join("other-task--x"),
    ] {
        write_refused(
            &setup,
            &profile,
            &format!("printf x > {}", quoted(&target)),
            &target,
        );
    }
}

/// OP-9b (plan step 2): a native lead in a linked worktree (the Sandboxed default) has
/// teammates who write only the teammates' folder of the lead's worktree, where the engine
/// made each one's linked worktree. The engine commits their turns, so they never write the
/// lead's files, the lead's or their own worktree records, the object store or the links
/// that tie a teammate worktree to the repository.
#[test]
fn a_linked_lead_s_teammates_write_only_their_worktrees_in_the_lead_s_worktree() {
    use std::os::unix::fs::PermissionsExt;
    assert!(Path::new(AGENT_GIT).is_file());
    let case = case();
    for arguments in [
        &["init", "-q"][..],
        &["commit", "-q", "--allow-empty", "-m", "start"],
    ] {
        let made = owner_git(&case.project, &case.home, arguments);
        assert!(made.status.success(), "{made:?}");
    }
    let worktrees = case.project.join(".crazyagent").join("worktrees");
    fs::create_dir_all(&worktrees).unwrap();
    let lead_worktree = worktrees.join("agent-1");
    let greet = lead_worktree
        .join(crazyagent::agent_git::TEAMMATES_FOLDER)
        .join("greet");
    for arguments in [
        vec![
            "worktree",
            "add",
            "-q",
            "--detach",
            lead_worktree.to_str().unwrap(),
            "HEAD",
        ],
        vec![
            "worktree",
            "add",
            "-q",
            "-b",
            "crazyagent/write-the-page--greet",
            greet.to_str().unwrap(),
            "HEAD",
        ],
    ] {
        let added = owner_git(&case.project, &case.home, &arguments);
        assert!(added.status.success(), "{added:?}");
    }
    let tmux = crazyagent::terminal::tmux::installed(&case.project).expect("tmux is installed");
    let tmp = crazyagent::sandbox::toolchains::make_tmp(&case.home).unwrap();
    for folder in crazyagent::sandbox::CLAUDE_TEAM_SHARED {
        fs::create_dir_all(case.home.join(folder)).unwrap();
    }
    let mates_home = case.home.with_file_name("mates-home");
    fs::create_dir_all(&mates_home).unwrap();
    fs::set_permissions(&mates_home, fs::Permissions::from_mode(0o700)).unwrap();
    let mailbox = case.tmp.join(crazyagent::team::tool::MAILBOX_FOLDER);
    fs::create_dir(&mailbox).unwrap();
    fs::set_permissions(&mailbox, fs::Permissions::from_mode(0o700)).unwrap();
    let (_lead, teammates) = sandbox(&case, SandboxNetwork::Only(Vec::new()))
        .allow_scratch(&tmp)
        .unwrap()
        .confine_to_worktree(&lead_worktree)
        .unwrap()
        .native_team(
            &tmp.join("tmux.sock"),
            &tmp.join("teammates-0123456789abcdef.sock"),
            &tmp.join(crazyagent::team::caller::CALLER_SOCKET),
            &tmux,
            &mates_home,
        )
        .unwrap();
    let profile = case.profiles.join("teammates.sb");
    let wrote = run(
        &teammates,
        &profile,
        &format!(
            "printf 'hello\\n' > {} && mkdir -p {} && printf 'x\\n' > {}",
            quoted(&greet.join("greet.txt")),
            quoted(&greet.join("src")),
            quoted(&greet.join("src").join("a.txt"))
        ),
    );
    assert!(
        wrote.status.success(),
        "a teammate writes in its own worktree: {}",
        String::from_utf8_lossy(&wrote.stderr)
    );
    let git = case.project.join(".git");
    let kept: Vec<(PathBuf, Vec<u8>)> = [
        greet.join(".git"),
        git.join("worktrees").join("greet").join("gitdir"),
        git.join("worktrees").join("greet").join("HEAD"),
        git.join("worktrees").join("agent-1").join("commondir"),
        git.join("worktrees").join("agent-1").join("HEAD"),
    ]
    .into_iter()
    .map(|file| {
        let bytes = fs::read(&file).unwrap();
        (file, bytes)
    })
    .collect();
    for (file, before) in &kept {
        let output = run(
            &teammates,
            &profile,
            &format!("printf 'x\\n' >> {}", quoted(file)),
        );
        assert!(!output.status.success(), "{}", file.display());
        assert_eq!(&fs::read(file).unwrap(), before, "{}", file.display());
    }
    for target in [
        lead_worktree.join("lead.txt"),
        git.join("worktrees").join("agent-1").join("teammate-probe"),
        git.join("worktrees").join("greet").join("index.lock"),
        git.join("objects").join("teammate-probe"),
    ] {
        write_refused(
            &teammates,
            &profile,
            &format!("printf x > {}", quoted(&target)),
            &target,
        );
    }
    let lead_only = lead_worktree.join("lead-only");
    fs::create_dir(&lead_only).unwrap();
    let folder = lead_worktree.join(crazyagent::agent_git::TEAMMATES_FOLDER);
    let [bob, carl, dave] = ["bob", "carl", "dave"].map(|name| folder.join(name));
    let planted = case.home.join("teams").join("planted.lock");
    let mut escaped: Vec<String> = Vec::new();
    for (label, script) in [
        (
            "symlink at a teammate folder",
            format!("ln -s {} {}", quoted(&lead_only), quoted(&bob)),
        ),
        (
            "symlink moved in as a teammate folder",
            format!(
                "ln -s {} {} && mv {} {}",
                quoted(&lead_only),
                quoted(&greet.join("link")),
                quoted(&greet.join("link")),
                quoted(&carl)
            ),
        ),
        (
            "team folder holding a .git moved in as a teammate folder",
            format!(
                "mkdir {} && printf 'gitdir: {}\\n' > {} && mv {} {}",
                quoted(&planted),
                git.display(),
                quoted(&planted.join(".git")),
                quoted(&planted),
                quoted(&dave)
            ),
        ),
    ] {
        if run(&teammates, &profile, &script).status.success() {
            escaped.push(label.to_owned());
        }
    }
    for made in [&bob, &carl, &dave] {
        if fs::symlink_metadata(made).is_ok() {
            escaped.push(format!("made {}", made.display()));
        }
    }
    let replaced = run(
        &teammates,
        &profile,
        &format!(
            "mv {} {} && ln -s {} {}",
            quoted(&folder),
            quoted(&tmp.join("teammates-away")),
            quoted(&lead_only),
            quoted(&folder)
        ),
    );
    if replaced.status.success() {
        escaped.push("teammates folder entry replaced".to_owned());
    }
    if !fs::symlink_metadata(&folder).unwrap().is_dir() || !greet.join(".git").is_file() {
        escaped.push(format!("moved {}", folder.display()));
    }
    escaped.extend(sibling_folder_escapes(&teammates, &profile, &folder, &tmp));
    assert!(
        escaped.is_empty(),
        "a teammate changes where the engine adds a teammate worktree: {escaped:?}"
    );
    assert!(
        fs::read_dir(&lead_only).unwrap().next().is_none(),
        "nothing lands in the lead's folder"
    );
}

/// Tries from a native lead's teammates' profile to rename, move out or remove a teammate's
/// worktree folder and to leave a stray entry beside them in the teammates' folder
/// (OP9B-C1), also spelled in another letter case and as a hard link (OP9B-R3-L1), which
/// one profile shared by all teammates would let one do to another. Takes
/// the teammates' sandbox and profile, the teammates' folder and a temp folder outside it.
/// Returns what got through, each named.
fn sibling_folder_escapes(
    teammates: &ProjectSandbox,
    profile: &Path,
    folder: &Path,
    tmp: &Path,
) -> Vec<String> {
    let (frank, gina) = (folder.join("frank"), folder.join("gina"));
    fs::create_dir(&frank).unwrap();
    fs::write(frank.join("work.txt"), "Frank's work.\n").unwrap();
    fs::create_dir(&gina).unwrap();
    let upper = folder.with_file_name(crazyagent::agent_git::TEAMMATES_FOLDER.to_uppercase());
    assert!(
        upper.join("frank").is_dir(),
        "the volume finds the teammates' folder in any letter case"
    );
    let mut escaped: Vec<String> = Vec::new();
    for (label, script) in [
        (
            "sibling folder renamed in another letter case",
            format!(
                "mv {} {}",
                quoted(&upper.join("frank")),
                quoted(&upper.join("frank-case"))
            ),
        ),
        (
            "stray folder in another letter case",
            format!("mkdir {}", quoted(&upper.join("case-stray"))),
        ),
        (
            "hard link beside the teammates' worktrees",
            format!(
                "ln {} {}",
                quoted(&frank.join("work.txt")),
                quoted(&folder.join("linked.txt"))
            ),
        ),
        (
            "sibling folder renamed",
            format!(
                "mv {} {}",
                quoted(&frank),
                quoted(&folder.join("frank-renamed"))
            ),
        ),
        (
            "sibling folder moved out",
            format!("mv {} {}", quoted(&frank), quoted(&tmp.join("frank-away"))),
        ),
        (
            "empty sibling folder removed",
            format!("rmdir {}", quoted(&gina)),
        ),
        (
            "stray file",
            format!("printf x > {}", quoted(&folder.join("stray.txt"))),
        ),
        (
            "stray folder",
            format!("mkdir {}", quoted(&folder.join("stray"))),
        ),
    ] {
        if run(teammates, profile, &script).status.success() {
            escaped.push(label.to_owned());
        }
    }
    if fs::read(frank.join("work.txt")).ok().as_deref() != Some(b"Frank's work.\n".as_slice()) {
        escaped.push("sibling's work moved".to_owned());
    }
    if !gina.is_dir() {
        escaped.push("empty sibling folder gone".to_owned());
    }
    for stray in [
        "frank-renamed",
        "stray.txt",
        "stray",
        "frank-case",
        "case-stray",
        "linked.txt",
    ] {
        if fs::symlink_metadata(folder.join(stray)).is_ok() {
            escaped.push(format!("made {stray}"));
        }
    }
    escaped
}

#[test]
fn an_agent_cannot_exec_an_app_bundle_it_put_in_its_own_folders_but_bundle_tools_still_run() {
    use std::os::unix::fs::PermissionsExt;
    let case = case();
    let sandbox = sandbox(&case, SandboxNetwork::Only(Vec::new()));
    let profile = case.profiles.join("own-apps.sb");
    let script = |root: &Path, path: &str| {
        let file = root.join(path);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, "#!/bin/sh\necho ran\n").unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o755)).unwrap();
        file
    };
    let chrome = script(
        &case.home,
        ".cache/puppeteer/chrome/mac_arm-131.0.6778.85/chrome-mac-arm64/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing",
    );
    let apps = [
        chrome.clone(),
        script(
            &case.home,
            "Library/Caches/ms-playwright/chromium-1148/chrome-mac/Chromium.app/Contents/MacOS/Chromium",
        ),
        script(
            &case.project,
            "node_modules/electron/dist/Electron.app/Contents/MacOS/Electron",
        ),
        script(&case.tmp, "unpacked/Some Tool.APP/contents/macos/Some Tool"),
        Path::new("/System/Volumes/Data").join(chrome.strip_prefix("/").unwrap()),
        PathBuf::from(chrome.to_str().unwrap().to_uppercase()),
    ];
    for app in &apps {
        assert!(app.is_file(), "{}", app.display());
        let output = run(&sandbox, &profile, &quoted(app));
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!output.status.success(), "{} ran", app.display());
        assert!(
            stderr.contains("Operation not permitted"),
            "{}: {stderr}",
            app.display()
        );
        assert!(output.stdout.is_empty(), "{}", app.display());
    }
    let tool = script(
        &case.project,
        "node_modules/electron/dist/Electron.app/Contents/Resources/bin/tool",
    );
    let output = run(&sandbox, &profile, &quoted(&tool));
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), "ran\n");
}

#[test]
fn a_teammate_worktree_is_added_in_the_clone_but_no_other_git_lands_below_it() {
    assert!(Path::new(AGENT_GIT).is_file());
    let (case, clone, _head) = clone_case();
    let profile = case.profiles.join("agent.sb");
    let confined = sandbox(&case, SandboxNetwork::Only(Vec::new()))
        .confine_to_clone(&clone)
        .unwrap();
    let teammates = clone.join(crazyagent::agent_git::TEAMMATES_FOLDER);
    let bob = teammates.join("bob");
    let added = run(
        &confined,
        &profile,
        &format!(
            "cd {} && {AGENT_GIT} -c core.hooksPath=/dev/null worktree add -q -b agent/bob {} HEAD",
            quoted(&clone),
            quoted(&bob)
        ),
    );
    assert!(added.status.success(), "{added:?}");
    assert!(bob.join(".git").is_file());
    for target in [
        teammates.join("dave").join(".git").join("config"),
        bob.join("nested").join(".git"),
        clone.join("sub").join(".git"),
    ] {
        write_refused(
            &confined,
            &profile,
            &format!(
                "mkdir -p {} && printf x > {}",
                quoted(target.parent().unwrap()),
                quoted(&target)
            ),
            &target,
        );
    }
}

#[test]
fn a_confined_agent_cannot_exec_an_app_bundle_from_a_folder_it_only_reads() {
    use std::os::unix::fs::PermissionsExt;
    let case = case();
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
    let sandbox = sandbox(&case, SandboxNetwork::Only(Vec::new()));
    let profile = case.profiles.join("read-only-apps.sb");
    let script = |root: &Path, path: &str| {
        let file = root.join(path);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, "#!/bin/sh\necho ran\n").unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o755)).unwrap();
        file
    };
    let electron = script(
        &case.project,
        "node_modules/electron/dist/Electron.app/Contents/MacOS/Electron",
    );
    let apps = [
        electron.clone(),
        script(
            &case.outside,
            "tools/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing",
        ),
        script(
            &case.outside,
            "unpacked/Some Tool.APP/contents/macos/Some Tool",
        ),
        Path::new("/System/Volumes/Data").join(electron.strip_prefix("/").unwrap()),
        PathBuf::from(electron.to_str().unwrap().to_uppercase()),
    ];
    let tool = script(
        &case.project,
        "node_modules/electron/dist/Electron.app/Contents/Resources/bin/tool",
    );
    let confined = sandbox
        .allow_read(&case.outside)
        .unwrap()
        .confine_to_worktree(&worktree)
        .unwrap();
    assert!(confined.read_roots().contains(&case.project));
    assert!(!confined.writes(&electron));
    for app in &apps {
        assert!(app.is_file(), "{}", app.display());
        let output = run(&confined, &profile, &quoted(app));
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!output.status.success(), "{} ran", app.display());
        assert!(
            stderr.contains("Operation not permitted"),
            "{}: {stderr}",
            app.display()
        );
        assert!(output.stdout.is_empty(), "{}", app.display());
    }
    let output = run(&confined, &profile, &quoted(&tool));
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), "ran\n");
}

#[test]
fn the_harness_the_engine_launches_from_an_app_bundle_still_starts_but_its_bundle_does_not() {
    use std::os::unix::fs::PermissionsExt;
    let case = case();
    let bundle = case
        .outside
        .join("Harness.app")
        .join("Contents")
        .join("MacOS");
    fs::create_dir_all(&bundle).unwrap();
    let program = |name: &str| {
        let file = bundle.join(name);
        fs::write(&file, "#!/bin/sh\necho ran\n").unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o755)).unwrap();
        file
    };
    let harness = program("harness");
    let sibling = program("Helper");
    let fresh = || sandbox(&case, SandboxNetwork::Only(Vec::new()));
    let allowed = fresh()
        .allow_read(&case.outside)
        .unwrap()
        .allow_exec(&harness)
        .unwrap()
        .allow_exec(&harness)
        .unwrap();
    let profile = case.profiles.join("harness.sb");
    allowed.install(&profile).unwrap();
    let mut launched = allowed.command(&profile, &harness);
    launched
        .current_dir(allowed.project_root())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let output = crazyagent::spawn_gate::output(&mut launched).unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), "ran\n");
    let own = case
        .home
        .join("Own.app")
        .join("Contents")
        .join("MacOS")
        .join("own");
    fs::create_dir_all(own.parent().unwrap()).unwrap();
    fs::write(&own, "#!/bin/sh\necho ran\n").unwrap();
    fs::set_permissions(&own, fs::Permissions::from_mode(0o755)).unwrap();
    let writable = fresh().allow_exec(&own).unwrap();
    assert!(writable.writes(&own));
    for (sandbox, app) in [(&allowed, &sibling), (&writable, &own)] {
        let output = run(sandbox, &profile, &quoted(app));
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!output.status.success(), "{} ran", app.display());
        assert!(stderr.contains("Operation not permitted"), "{stderr}");
        assert!(output.stdout.is_empty());
    }
    let refused = |result: Result<ProjectSandbox, SandboxError>| {
        assert!(
            matches!(result, Err(SandboxError::Invalid(_))),
            "{:?}",
            result.map(|sandbox| sandbox.profile())
        );
    };
    refused(fresh().allow_exec(Path::new("Harness.app/Contents/MacOS/harness")));
    refused(fresh().allow_exec(&bundle));
    refused(fresh().allow_exec(&bundle.join("missing")));
    refused(
        fresh().allow_exec(
            &case
                .outside
                .join("Harness.app/Contents/MacOS/../MacOS/harness"),
        ),
    );
    let mut many = fresh();
    for index in 0..4 {
        many = many.allow_exec(&program(&format!("tool-{index}"))).unwrap();
    }
    many = many.allow_exec(&bundle.join("tool-0")).unwrap();
    refused(many.allow_exec(&program("tool-4")));
}

#[test]
fn an_agent_still_runs_framework_python_whose_launcher_starts_its_python_app() {
    let case = case();
    let sandbox = sandbox(&case, SandboxNetwork::Only(Vec::new()));
    let profile = case.profiles.join("python.sb");
    let python = Path::new("/opt/homebrew/bin/python3");
    let real = fs::canonicalize(python).unwrap();
    assert!(
        real.to_str().unwrap().contains(".framework/Versions/"),
        "the probe needs a framework build at {}",
        python.display()
    );
    let output = run(
        &sandbox,
        &profile,
        &format!("{} -c 'print(\"ok\")'", quoted(python)),
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), "ok\n");
}

/// SECAUDIT sandbox-seatbelt-unfiltered-sysctl-read-procargs: a confined child reads neither
/// the arguments nor the environment of a process outside its sandbox through either
/// process-arguments sysctl, and still reads a process of its own sandbox and an ordinary
/// sysctl value.
#[test]
fn a_confined_child_reads_no_process_arguments_by_sysctl() {
    const NAME: &str = "a_confined_child_reads_no_process_arguments_by_sysctl";
    const CANARY: &str = "DUMMY_CANARY=dummy-value";
    const PLATFORM_ARGUMENT: &str = "30.0417";
    const PROBE: &str = r#"$m=pack("i3",1,shift,shift);$b="\0"x262144;$l=pack("Q",262144);syscall(202,$m,3,$b,$l,0,0)==0 or exit 3;print $b"#;
    const NCPU: &str = r#"$m=pack("i2",6,3);$b="\0"x4;$l=pack("Q",4);syscall(202,$m,2,$b,$l,0,0)==0 or exit 3;print "cpus=",unpack("i",$b)"#;
    if std::env::var_os("DUMMY_PROCARGS_CHILD").is_some() {
        std::thread::sleep(std::time::Duration::from_secs(30));
        return;
    }
    let quiet = |command: &mut std::process::Command| {
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        crazyagent::spawn_gate::spawn(command).unwrap()
    };
    let mut dummy = quiet(
        std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", NAME, "--test-threads=1"])
            .env_clear()
            .env("DUMMY_PROCARGS_CHILD", "1")
            .env("DUMMY_CANARY", "dummy-value"),
    );
    let mut apple = quiet(
        std::process::Command::new("/usr/bin/env")
            .env_clear()
            .args(["DUMMY_CANARY=dummy-value", "/bin/sleep", PLATFORM_ARGUMENT]),
    );
    std::thread::sleep(std::time::Duration::from_millis(500));
    let case = case();
    let confined = sandbox(&case, SandboxNetwork::Only(Vec::new()));
    let holds =
        |output: &Output, text: &str| String::from_utf8_lossy(&output.stdout).contains(text);
    let mut problems = Vec::new();
    for mib in [49, 38] {
        let probe = |name: &str, target: u32| {
            run(
                &confined,
                &case.profiles.join(format!("{name}-{mib}.sb")),
                &format!("/usr/bin/perl -e '{PROBE}' {mib} {target}"),
            )
        };
        let plain = |target: u32| {
            crazyagent::spawn_gate::output(
                std::process::Command::new("/usr/bin/perl")
                    .args(["-e", PROBE, &mib.to_string(), &target.to_string()])
                    .stdin(Stdio::null())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped()),
            )
            .unwrap()
        };
        let plain_platform = plain(apple.id());
        if !plain_platform.status.success() || !holds(&plain_platform, PLATFORM_ARGUMENT) {
            problems.push(format!(
                "{mib}: inconclusive, the unconfined probe read no platform argument"
            ));
        }
        let unconfined = plain(dummy.id());
        if !unconfined.status.success() || !holds(&unconfined, NAME) {
            problems.push(format!(
                "{mib}: inconclusive, the unconfined probe read nothing"
            ));
        }
        let across = probe("across", dummy.id());
        if holds(&across, CANARY) || holds(&across, NAME) {
            problems.push(format!("{mib}: read a process outside the sandbox"));
        }
        let platform = probe("apple", apple.id());
        if holds(&platform, CANARY) || holds(&platform, PLATFORM_ARGUMENT) {
            problems.push(format!(
                "{mib}: read a platform binary's arguments or environment"
            ));
        }
    }
    let ordinary = run(
        &confined,
        &case.profiles.join("ncpu.sb"),
        &format!("/usr/bin/perl -e '{NCPU}'"),
    );
    let inside = run(
        &confined,
        &case.profiles.join("inside.sb"),
        &format!("/bin/sleep 30 & p=$!; /usr/bin/perl -e '{PROBE}' 49 $p; c=$?; kill $p; exit $c"),
    );
    for child in [&mut dummy, &mut apple] {
        let _ = child.kill();
        let _ = child.wait();
    }
    if !inside.status.success() || !holds(&inside, "sleep") {
        problems.push(format!(
            "a process of the same sandbox was not readable: {inside:?}"
        ));
    }
    if !ordinary.status.success() || !holds(&ordinary, "cpus=") {
        problems.push(format!("an ordinary sysctl read failed: {ordinary:?}"));
    }
    assert!(problems.is_empty(), "{problems:#?}");
}

/// SECAUDIT sandbox-seatbelt-unfiltered-sysctl-read-procargs r2: `lsof` lists process ids
/// before it reads one, so listing them stays open and a confined agent can still ask what
/// its own process holds.
#[test]
fn a_confined_lsof_on_its_own_process_succeeds() {
    let case = case();
    let confined = sandbox(&case, SandboxNetwork::Only(Vec::new()));
    let output = run(
        &confined,
        &case.profiles.join("lsof.sb"),
        "/usr/sbin/lsof -p $$",
    );
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "{output:?}");
    assert!(text.contains("COMMAND") && text.contains("cwd"), "{text}");
}

/// SECAUDIT sandbox-seatbelt-unfiltered-sysctl-read-procargs r2: a confined shell lists its
/// own running child, the list a harness walks to stop a tool's processes. The probe makes
/// the `proc_info` list call with the parent-only filter, which `proc_listchildpids` wraps.
#[test]
fn a_confined_shell_lists_its_own_running_child() {
    const LIST: &str = r#"$b="\0"x65536;$n=syscall(336,1,6,getppid(),0,$b,65536);$n>=0 or exit 3;print join(" ",grep{$_}unpack("i*",substr($b,0,$n))),"\n""#;
    let case = case();
    let confined = sandbox(&case, SandboxNetwork::Only(Vec::new()));
    let output = run(
        &confined,
        &case.profiles.join("children.sb"),
        &format!(
            "/bin/sleep 30 & p=$!; echo child=$p; /usr/bin/perl -e '{LIST}'; c=$?; kill $p; exit $c"
        ),
    );
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "{output:?}");
    let child = text
        .lines()
        .find_map(|line| line.strip_prefix("child="))
        .unwrap();
    let listed = text.lines().last().unwrap();
    assert!(listed.split(' ').any(|pid| pid == child), "{text}");
}
