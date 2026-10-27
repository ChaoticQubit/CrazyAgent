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
