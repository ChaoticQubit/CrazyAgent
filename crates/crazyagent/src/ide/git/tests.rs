use super::*;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Output, Stdio};

/// Runs the system git in a folder with a clean configuration, plus the given variables, as
/// a process the IDE server starts would. Inputs: the folder, the extra variables and the
/// arguments. Output: what git printed and its status.
fn git(dir: &Path, env: &[(&str, &str)], args: &[&str]) -> Output {
    crate::spawn_gate::output(
        Command::new("/usr/bin/git")
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", dir)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("LC_ALL", "C")
            .envs(env.iter().copied())
            .arg("-C")
            .arg(dir)
            .args([
                "-c",
                "user.name=Owner",
                "-c",
                "user.email=owner@example.invalid",
            ])
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
    )
    .unwrap()
}

fn ok(dir: &Path, args: &[&str]) -> String {
    let output = git(dir, &[], args);
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn ide_git_never_enters_a_repository_planted_below_a_worktree() {
    let directory = tempfile::tempdir().unwrap();
    let base = directory.path().canonicalize().unwrap();
    let worktree = base.join("worktree");
    std::fs::create_dir(&worktree).unwrap();
    ok(&worktree, &["init", "-q"]);
    ok(&worktree, &["commit", "-q", "--allow-empty", "-m", "base"]);
    let planted = worktree.join("planted");
    std::fs::create_dir(&planted).unwrap();
    ok(&planted, &["init", "-q"]);
    std::fs::write(planted.join("notes.txt"), "first\n").unwrap();
    ok(&planted, &["add", "notes.txt"]);
    ok(&planted, &["commit", "-q", "-m", "planted"]);
    std::fs::write(
        worktree.join(".gitmodules"),
        "[submodule \"planted\"]\n\tpath = planted\n\turl = ./planted\n",
    )
    .unwrap();
    ok(&worktree, &["add", ".gitmodules", "planted"]);
    ok(&worktree, &["commit", "-q", "-m", "gitlink"]);
    let marker = base.join("fsmonitor-ran");
    let hook = base.join("fsmonitor.sh");
    std::fs::write(
        &hook,
        format!("#!/bin/sh\n: > '{}'\nexit 1\n", marker.display()),
    )
    .unwrap();
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o700)).unwrap();
    ok(
        &planted,
        &["config", "core.fsmonitor", hook.to_str().unwrap()],
    );
    std::fs::write(planted.join("notes.txt"), "changed\n").unwrap();

    let guarded = git(&worktree, &CONFIG_ENV, &["status", "--porcelain"]);
    assert!(guarded.status.success());
    assert_eq!(String::from_utf8_lossy(&guarded.stdout), "");
    assert!(
        !marker.exists(),
        "the planted repository's fsmonitor never runs under the IDE's git configuration"
    );
    let guarded_diff = git(&worktree, &CONFIG_ENV, &["diff", "--stat"]);
    assert_eq!(String::from_utf8_lossy(&guarded_diff.stdout), "");
    assert!(!marker.exists());

    let open = git(&worktree, &[], &["status", "--porcelain"]);
    assert!(open.status.success());
    assert_eq!(String::from_utf8_lossy(&open.stdout), " M planted\n");
    assert!(
        marker.exists(),
        "without it, git enters the planted repository and runs its fsmonitor"
    );
}

#[test]
fn the_ide_git_settings_turn_off_nested_repository_and_submodule_detection() {
    assert_eq!(
        Value::Object(workspace()),
        serde_json::json!({
            "git.detectSubmodules": false,
            "git.autoRepositoryDetection": false,
        })
    );
    let count: usize = CONFIG_ENV[0].1.parse().unwrap();
    assert_eq!(CONFIG_ENV.len(), 1 + 2 * count);
    assert!(
        CONFIG_ENV
            .iter()
            .all(|(name, _)| name.starts_with("GIT_CONFIG_"))
    );
}

/// Writes an executable script that records that it ran by creating `marker`, then passes
/// its input through. Inputs: the script path and the marker. Output: none.
fn planted(script: &Path, marker: &Path) {
    std::fs::write(
        script,
        format!("#!/bin/sh\n: > '{}'\nexec cat\n", marker.display()),
    )
    .unwrap();
    std::fs::set_permissions(script, std::fs::Permissions::from_mode(0o700)).unwrap();
}

#[test]
fn the_ide_git_configuration_runs_no_planted_program_and_refuses_a_bare_looking_folder() {
    let directory = tempfile::tempdir().unwrap();
    let base = directory.path().canonicalize().unwrap();
    let repo = base.join("repo");
    std::fs::create_dir(&repo).unwrap();
    ok(&repo, &["init", "-q"]);
    ok(&repo, &["commit", "-q", "--allow-empty", "-m", "base"]);
    let marks = base.join("marks");
    std::fs::create_dir(&marks).unwrap();
    let hooks = repo.join(".git").join("hooks");
    std::fs::create_dir_all(&hooks).unwrap();
    for name in ["pre-commit", "post-index-change"] {
        planted(&hooks.join(name), &marks.join(name));
    }
    planted(&base.join("fsmonitor.sh"), &marks.join("fsmonitor"));
    planted(&base.join("sign.sh"), &marks.join("sign"));
    ok(
        &repo,
        &[
            "config",
            "core.fsmonitor",
            base.join("fsmonitor.sh").to_str().unwrap(),
        ],
    );
    ok(
        &repo,
        &[
            "config",
            "gpg.program",
            base.join("sign.sh").to_str().unwrap(),
        ],
    );
    std::fs::write(repo.join("notes.txt"), "first\n").unwrap();
    let ran = |name: &str| marks.join(name).exists();

    let added = git(&repo, &CONFIG_ENV, &["add", "notes.txt"]);
    assert!(
        added.status.success(),
        "{}",
        String::from_utf8_lossy(&added.stderr)
    );
    let committed = git(&repo, &CONFIG_ENV, &["commit", "-q", "-m", "guarded"]);
    assert!(
        committed.status.success(),
        "{}",
        String::from_utf8_lossy(&committed.stderr)
    );
    let signed = git(
        &repo,
        &CONFIG_ENV,
        &["commit", "-q", "-S", "--allow-empty", "-m", "signed"],
    );
    assert!(!signed.status.success());
    for name in ["pre-commit", "post-index-change", "fsmonitor", "sign"] {
        assert!(!ran(name), "{name} ran under the IDE's git configuration");
    }

    std::fs::write(repo.join("notes.txt"), "second\n").unwrap();
    assert!(git(&repo, &[], &["status", "--porcelain"]).status.success());
    assert!(git(&repo, &[], &["add", "notes.txt"]).status.success());
    assert!(
        git(&repo, &[], &["commit", "-q", "-m", "open"])
            .status
            .success()
    );
    assert!(
        !git(
            &repo,
            &[],
            &["commit", "-q", "-S", "--allow-empty", "-m", "signed"]
        )
        .status
        .success()
    );
    for name in ["pre-commit", "post-index-change", "fsmonitor", "sign"] {
        assert!(ran(name), "without it, git runs the planted {name}");
    }

    let bare = base.join("agent");
    std::fs::create_dir(&bare).unwrap();
    ok(&bare, &["init", "-q", "--bare"]);
    ok(&bare, &["config", "core.bare", "false"]);
    ok(&bare, &["config", "core.worktree", bare.to_str().unwrap()]);
    planted(
        &base.join("bare-fsmonitor.sh"),
        &marks.join("bare-fsmonitor"),
    );
    ok(
        &bare,
        &[
            "config",
            "core.fsmonitor",
            base.join("bare-fsmonitor.sh").to_str().unwrap(),
        ],
    );
    std::fs::write(bare.join("notes.txt"), "planted\n").unwrap();
    let guarded = git(&bare, &CONFIG_ENV, &["status", "--porcelain"]);
    assert!(!guarded.status.success());
    assert!(
        String::from_utf8_lossy(&guarded.stderr).contains("cannot use bare repository"),
        "{}",
        String::from_utf8_lossy(&guarded.stderr)
    );
    assert!(!ran("bare-fsmonitor"));
    assert!(git(&bare, &[], &["status", "--porcelain"]).status.success());
    assert!(
        ran("bare-fsmonitor"),
        "without it, git uses the bare-looking folder and runs its fsmonitor"
    );
}

#[test]
fn the_shim_lets_git_run_hooks_in_the_project_folder_alone() {
    assert_eq!(shim_text(None).unwrap(), SHIM_TEXT);
    for refused in ["relative/project", "/project/it's", "/project/line\nbreak"] {
        assert!(shim_text(Some(Path::new(refused))).is_err(), "{refused:?}");
    }
    let directory = tempfile::tempdir().unwrap();
    let base = directory.path().canonicalize().unwrap();
    let project = base.join("project");
    let agent = project
        .join(".crazyagent")
        .join("worktrees")
        .join("agent-1");
    std::fs::create_dir_all(&agent).unwrap();
    ok(&project, &["init", "-q"]);
    let on = base.join("on");
    let off = base.join("off");
    for (shim, text) in [
        (&on, shim_text(Some(&project)).unwrap()),
        (&off, shim_text(None).unwrap()),
    ] {
        assert!(text.starts_with("#!/bin/sh\n") && text.ends_with("exec git \"$@\"\n"));
        std::fs::write(shim, text).unwrap();
        std::fs::set_permissions(shim, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let hooks_path = |shim: &Path, dir: &Path| {
        let output = crate::spawn_gate::output(
            Command::new(shim)
                .current_dir(dir)
                .env_clear()
                .env("PATH", "/usr/bin:/bin")
                .env("HOME", dir)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .envs(CONFIG_ENV.iter().copied())
                .args(["config", "--get", "core.hooksPath"])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped()),
        )
        .unwrap();
        String::from_utf8(output.stdout).unwrap()
    };
    assert_eq!(hooks_path(&on, &project), "");
    assert_eq!(hooks_path(&on, &agent), "/dev/null\n");
    assert_eq!(hooks_path(&on, &project.join(".git")), "/dev/null\n");
    assert_eq!(hooks_path(&off, &project), "/dev/null\n");
}

/// INT1-1: a git that cannot read an agent folder's `.git` treats it as missing and walks
/// up to the project's repository. With the project's ceiling, git in an agent folder of
/// either agents root finds no repository, while the project folder and its other folders
/// keep theirs; a folder holding a `:` or a relative one is refused.
#[test]
fn the_ceiling_stops_git_at_the_agents_roots_and_keeps_the_project() {
    assert!(ceiling(Path::new("relative/project")).is_err());
    assert!(ceiling(Path::new("/projects/a:b")).is_err());
    let directory = tempfile::tempdir().unwrap();
    let base = directory.path().canonicalize().unwrap();
    let project = base.join("project");
    let inner = project
        .join(".crazyagent")
        .join("worktrees")
        .join("agent-1");
    let outer = project.join("worktrees").join("agent-2");
    let own = project.join("src");
    for folder in [&inner, &outer, &own] {
        std::fs::create_dir_all(folder).unwrap();
    }
    ok(&project, &["init", "-q"]);
    let value = ceiling(&project).unwrap();
    assert_eq!(
        value,
        std::ffi::OsString::from(format!(
            "{}:{}",
            project.join(".crazyagent/worktrees").display(),
            project.join("worktrees").display()
        ))
    );
    let top = |dir: &Path, bounded: bool| {
        let env: Vec<(&str, &str)> = if bounded {
            vec![(CEILING_VARIABLE, value.to_str().unwrap())]
        } else {
            Vec::new()
        };
        git(dir, &env, &["rev-parse", "--show-toplevel"])
    };
    for agent in [&inner, &outer] {
        let open = top(agent, false);
        assert!(open.status.success());
        assert_eq!(
            String::from_utf8_lossy(&open.stdout).trim(),
            project.to_str().unwrap(),
            "without it, git in an agent folder acts on the project"
        );
        let bounded = top(agent, true);
        assert!(!bounded.status.success());
        assert!(
            String::from_utf8_lossy(&bounded.stderr).contains("not a git repository"),
            "{}",
            String::from_utf8_lossy(&bounded.stderr)
        );
    }
    for dir in [&project, &own] {
        let bounded = top(dir, true);
        assert!(bounded.status.success(), "{}", dir.display());
        assert_eq!(
            String::from_utf8_lossy(&bounded.stdout).trim(),
            project.to_str().unwrap()
        );
    }
}
