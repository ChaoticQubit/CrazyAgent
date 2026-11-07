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
        assert_eq!(git_script(Path::new(git), false), None, "{git:?}");
        assert_eq!(git_script(Path::new(git), true), None, "{git:?}");
    }
    let worker = git_script(Path::new("/Applications/X code.app/usr/bin/git"), false).unwrap();
    let lead = git_script(Path::new("/Applications/X code.app/usr/bin/git"), true).unwrap();
    for (script, flag) in [(&worker, "lead=0"), (&lead, "lead=1")] {
        assert!(script.starts_with("#!/bin/sh\ngit='/Applications/X code.app/usr/bin/git'\n"));
        assert!(script.lines().any(|line| line == flag), "{flag}");
        assert!(!script.contains("@GIT@") && !script.contains("@LEAD@"));
        assert!(script.len() as u64 <= GIT_SCRIPT_BYTES_MAX);
    }
    assert_eq!(worker.replace("\nlead=0\n", "\nlead=1\n"), lead);
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
