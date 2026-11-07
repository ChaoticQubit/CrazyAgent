use super::*;

#[test]
fn the_agent_git_script_is_refused_for_the_xcrun_shim_and_any_path_that_is_not_plain() {
    for git in [
        "/usr/bin/git",
        "usr/bin/git",
        "/opt/it's/git",
        "/opt/a\"b/git",
        "/opt/$HOME/git",
        "/opt/a`b`/git",
        "/opt/new\nline/git",
        "/opt/semi;colon/git",
    ] {
        for copy in [GitCopy::Worker, GitCopy::Lead, GitCopy::Free] {
            for main in [true, false] {
                assert_eq!(git_script(Path::new(git), copy, main), None, "{git:?}");
            }
        }
    }
    let real = Path::new("/Applications/X code.app/usr/bin/git");
    let worker = git_script(real, GitCopy::Worker, true).unwrap();
    let lead = git_script(real, GitCopy::Lead, true).unwrap();
    let free = git_script(real, GitCopy::Free, true).unwrap();
    let strict = git_script(real, GitCopy::Worker, false).unwrap();
    for (script, flags) in [
        (&worker, ["lead=0", "free=0", "main=1"]),
        (&lead, ["lead=1", "free=0", "main=1"]),
        (&free, ["lead=0", "free=1", "main=1"]),
        (&strict, ["lead=0", "free=0", "main=0"]),
    ] {
        assert!(script.starts_with("#!/bin/sh\ngit='/Applications/X code.app/usr/bin/git'\n"));
        for flag in flags {
            assert!(script.lines().any(|line| line == flag), "{flag}");
        }
        assert!(!script.contains("@GIT@") && !script.contains("@LEAD@"));
        assert!(!script.contains("@FREE@") && !script.contains("@MAIN@"));
        assert!(script.len() as u64 <= GIT_SCRIPT_BYTES_MAX);
    }
    assert_eq!(worker.replace("\nlead=0\n", "\nlead=1\n"), lead);
    assert_eq!(worker.replace("\nfree=0\n", "\nfree=1\n"), free);
    assert_eq!(worker.replace("\nmain=1\n", "\nmain=0\n"), strict);
}

#[test]
fn a_link_swapped_in_for_the_lead_s_new_temp_folder_is_refused_and_never_followed() {
    let scratch = tempfile::tempdir().unwrap();
    let victim = scratch.path().join("victim");
    fs::create_dir(&victim).unwrap();
    fs::set_permissions(&victim, fs::Permissions::from_mode(0o755)).unwrap();
    let lead = native_lead_tmp(scratch.path());
    std::os::unix::fs::symlink(&victim, &lead).unwrap();
    let sealed = std::panic::catch_unwind(|| seal_new_folder(&lead))
        .expect("a link swapped in for the new folder panicked instead of being refused");
    assert!(
        matches!(&sealed, Err(error) if error.kind() == io::ErrorKind::PermissionDenied),
        "{sealed:?}"
    );
    assert_eq!(
        fs::symlink_metadata(&victim).unwrap().mode() & 0o7777,
        0o755
    );
    assert!(
        fs::symlink_metadata(&lead)
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

#[test]
fn a_folder_swapped_in_for_the_lead_s_new_temp_folder_is_refused_with_what_it_holds() {
    let scratch = tempfile::tempdir().unwrap();
    let lead = native_lead_tmp(scratch.path());
    fs::create_dir(&lead).unwrap();
    fs::write(lead.join("planted"), "earlier").unwrap();
    fs::set_permissions(&lead, fs::Permissions::from_mode(0o777)).unwrap();
    let sealed = std::panic::catch_unwind(|| seal_new_folder(&lead))
        .expect("a folder swapped in for the new folder panicked instead of being refused");
    assert!(
        matches!(&sealed, Err(error) if error.kind() == io::ErrorKind::PermissionDenied),
        "{sealed:?}"
    );
}

/// Runs a git program in the test repository with no system or global config and the
/// test's own home. Inputs: the program, the repository, the home and the arguments.
/// Output: the program's output.
fn git_run(program: &Path, repo: &Path, home: &Path, args: &[&str]) -> std::process::Output {
    crate::spawn_gate::output(
        std::process::Command::new(program)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("LC_ALL", "C")
            .args(["-c", "user.name=T", "-c", "user.email=t@example.invalid"])
            .args(args)
            .current_dir(repo)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped()),
    )
    .unwrap()
}

/// The master at Full access gets the free copy of its own `git` (DEC-117 Q2): it merges
/// a branch the worker copy refuses under DEC-107.
#[test]
fn the_free_copy_merges_another_branch_that_the_worker_copy_refuses() {
    struct Removed(PathBuf);
    impl Drop for Removed {
        fn drop(&mut self) {
            let _ = remove_tmp(&self.0);
        }
    }
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    let git = crate::research::runner::sandboxed_git();
    for args in [
        &["init", "-q", "-b", "main"][..],
        &["commit", "-q", "--allow-empty", "-m", "start"],
        &["checkout", "-q", "-b", "other"],
        &["commit", "-q", "--allow-empty", "-m", "other work"],
        &["checkout", "-q", "main"],
    ] {
        let done = git_run(git, repo.path(), home.path(), args);
        assert!(done.status.success(), "{args:?} {done:?}");
    }
    let tmp = make_tmp(home.path()).unwrap();
    let _removed = Removed(home.path().to_path_buf());
    let wrapper = tmp.join(GIT_FOLDER).join(GIT_PROGRAM);
    let refused = git_run(
        &wrapper,
        repo.path(),
        home.path(),
        &["merge", "--no-edit", "other"],
    );
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("git merge is refused"));
    free_git(&tmp).unwrap();
    let merged = git_run(
        &wrapper,
        repo.path(),
        home.path(),
        &["merge", "--no-edit", "other"],
    );
    assert!(
        merged.status.success(),
        "{}",
        String::from_utf8_lossy(&merged.stderr)
    );
}

/// With "Catch up with main" on, the worker copy of an agent's own `git` merges the project
/// main the engine keeps at `crazyagent/main`, and still refuses another branch, a merge
/// naming another branch beside main, and pull, telling the agent how to catch up.
#[test]
fn the_worker_copy_merges_crazyagent_main_and_still_refuses_any_other_branch() {
    struct Removed(PathBuf);
    impl Drop for Removed {
        fn drop(&mut self) {
            let _ = remove_tmp(&self.0);
        }
    }
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    let git = crate::research::runner::sandboxed_git();
    for args in [
        &["init", "-q", "-b", "main"][..],
        &["commit", "-q", "--allow-empty", "-m", "start"],
        &["checkout", "-q", "-b", "landed"],
        &["commit", "-q", "--allow-empty", "-m", "landed work"],
        &["update-ref", "refs/crazyagent/main", "landed"],
        &["checkout", "-q", "-b", "other", "main"],
        &["commit", "-q", "--allow-empty", "-m", "other work"],
        &["checkout", "-q", "-b", "own", "main"],
    ] {
        let done = git_run(git, repo.path(), home.path(), args);
        assert!(done.status.success(), "{args:?} {done:?}");
    }
    let tmp = make_tmp(home.path()).unwrap();
    let _removed = Removed(home.path().to_path_buf());
    catch_up_git(&tmp).unwrap();
    let wrapper = tmp.join(GIT_FOLDER).join(GIT_PROGRAM);
    let merged = git_run(
        &wrapper,
        repo.path(),
        home.path(),
        &["merge", "--no-edit", "crazyagent/main"],
    );
    assert!(
        merged.status.success(),
        "{}",
        String::from_utf8_lossy(&merged.stderr)
    );
    let landed = git_run(git, repo.path(), home.path(), &["rev-parse", "landed"]);
    let head = git_run(git, repo.path(), home.path(), &["rev-parse", "HEAD"]);
    assert_eq!(head.stdout, landed.stdout);
    for (args, refused) in [
        (&["merge", "--no-edit", "other"][..], "git merge is refused"),
        (
            &["merge", "--no-edit", "crazyagent/main", "other"],
            "git merge is refused",
        ),
        (&["pull"], "git pull is refused"),
    ] {
        let done = git_run(&wrapper, repo.path(), home.path(), args);
        assert!(!done.status.success(), "{args:?}");
        let stderr = String::from_utf8_lossy(&done.stderr);
        assert!(stderr.contains(refused), "{args:?} {stderr}");
        assert!(
            stderr.contains("git merge crazyagent/main"),
            "{args:?} {stderr}"
        );
    }
}

/// The worker copy merges only the commit the engine keeps at `refs/crazyagent/main`: with
/// that ref missing, a branch the agent named `crazyagent/main` holding other work is
/// refused under either name and the agent's branch keeps its commit.
#[test]
fn the_worker_copy_refuses_a_crazyagent_main_that_is_not_the_engine_s_ref() {
    struct Removed(PathBuf);
    impl Drop for Removed {
        fn drop(&mut self) {
            let _ = remove_tmp(&self.0);
        }
    }
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    let git = crate::research::runner::sandboxed_git();
    for args in [
        &["init", "-q", "-b", "main"][..],
        &["commit", "-q", "--allow-empty", "-m", "start"],
        &["checkout", "-q", "-b", "crazyagent/main"],
        &["commit", "-q", "--allow-empty", "-m", "foreign work"],
        &["checkout", "-q", "-b", "own", "main"],
    ] {
        let done = git_run(git, repo.path(), home.path(), args);
        assert!(done.status.success(), "{args:?} {done:?}");
    }
    let tmp = make_tmp(home.path()).unwrap();
    let _removed = Removed(home.path().to_path_buf());
    catch_up_git(&tmp).unwrap();
    let wrapper = tmp.join(GIT_FOLDER).join(GIT_PROGRAM);
    let before = git_run(git, repo.path(), home.path(), &["rev-parse", "HEAD"]);
    for name in ["crazyagent/main", "refs/crazyagent/main"] {
        let done = git_run(
            &wrapper,
            repo.path(),
            home.path(),
            &["merge", "--no-edit", name],
        );
        assert!(!done.status.success(), "{name} was merged");
        let stderr = String::from_utf8_lossy(&done.stderr);
        assert!(stderr.contains("git merge is refused"), "{name} {stderr}");
    }
    let after = git_run(git, repo.path(), home.path(), &["rev-parse", "HEAD"]);
    assert_eq!(after.stdout, before.stdout);
}

/// The copy of an agent's own `git` that every agent gets first, before its launch knows
/// the project's "Catch up with main" choice, refuses `crazyagent/main`, so an agent whose
/// later copy could not be written never merges main against an Off.
#[test]
fn the_copy_every_agent_gets_first_refuses_crazyagent_main() {
    struct Removed(PathBuf);
    impl Drop for Removed {
        fn drop(&mut self) {
            let _ = remove_tmp(&self.0);
        }
    }
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    let git = crate::research::runner::sandboxed_git();
    for args in [
        &["init", "-q", "-b", "main"][..],
        &["commit", "-q", "--allow-empty", "-m", "start"],
        &["checkout", "-q", "-b", "landed"],
        &["commit", "-q", "--allow-empty", "-m", "landed work"],
        &["update-ref", "refs/crazyagent/main", "landed"],
        &["checkout", "-q", "-b", "own", "main"],
    ] {
        let done = git_run(git, repo.path(), home.path(), args);
        assert!(done.status.success(), "{args:?} {done:?}");
    }
    let tmp = make_tmp(home.path()).unwrap();
    let _removed = Removed(home.path().to_path_buf());
    let wrapper = tmp.join(GIT_FOLDER).join(GIT_PROGRAM);
    let done = git_run(
        &wrapper,
        repo.path(),
        home.path(),
        &["merge", "--no-edit", "crazyagent/main"],
    );
    assert!(
        !done.status.success(),
        "the first copy merged crazyagent/main"
    );
    let stderr = String::from_utf8_lossy(&done.stderr);
    assert!(stderr.contains("git merge is refused"), "{stderr}");
    assert!(!stderr.contains("crazyagent/main"), "{stderr}");
}

/// With "Catch up with main" off, the agent keeps the copy of its own `git` that
/// [`make_tmp`] wrote, which refuses `crazyagent/main` too, with the reason the worker copy
/// gave before the setting.
#[test]
fn the_strict_copy_refuses_crazyagent_main_with_the_plain_reason() {
    struct Removed(PathBuf);
    impl Drop for Removed {
        fn drop(&mut self) {
            let _ = remove_tmp(&self.0);
        }
    }
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    let git = crate::research::runner::sandboxed_git();
    for args in [
        &["init", "-q", "-b", "main"][..],
        &["commit", "-q", "--allow-empty", "-m", "start"],
        &["checkout", "-q", "-b", "landed"],
        &["commit", "-q", "--allow-empty", "-m", "landed work"],
        &["update-ref", "refs/crazyagent/main", "landed"],
        &["checkout", "-q", "-b", "own", "main"],
    ] {
        let done = git_run(git, repo.path(), home.path(), args);
        assert!(done.status.success(), "{args:?} {done:?}");
    }
    let tmp = make_tmp(home.path()).unwrap();
    let _removed = Removed(home.path().to_path_buf());
    let wrapper = tmp.join(GIT_FOLDER).join(GIT_PROGRAM);
    let refused = git_run(
        &wrapper,
        repo.path(),
        home.path(),
        &["merge", "--no-edit", "crazyagent/main"],
    );
    assert!(!refused.status.success());
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(stderr.contains("git merge is refused"), "{stderr}");
    assert!(
        stderr.contains("If you need work from another branch, stop and tell your lead"),
        "{stderr}"
    );
    assert!(!stderr.contains("crazyagent/main"), "{stderr}");
}
