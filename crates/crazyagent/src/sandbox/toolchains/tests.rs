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
            assert_eq!(git_script(Path::new(git), copy), None, "{git:?}");
        }
    }
    let real = Path::new("/Applications/X code.app/usr/bin/git");
    let worker = git_script(real, GitCopy::Worker).unwrap();
    let lead = git_script(real, GitCopy::Lead).unwrap();
    let free = git_script(real, GitCopy::Free).unwrap();
    for (script, flags) in [
        (&worker, ["lead=0", "free=0"]),
        (&lead, ["lead=1", "free=0"]),
        (&free, ["lead=0", "free=1"]),
    ] {
        assert!(script.starts_with("#!/bin/sh\ngit='/Applications/X code.app/usr/bin/git'\n"));
        for flag in flags {
            assert!(script.lines().any(|line| line == flag), "{flag}");
        }
        assert!(!script.contains("@GIT@") && !script.contains("@LEAD@"));
        assert!(!script.contains("@FREE@"));
        assert!(script.len() as u64 <= GIT_SCRIPT_BYTES_MAX);
    }
    assert_eq!(worker.replace("\nlead=0\n", "\nlead=1\n"), lead);
    assert_eq!(worker.replace("\nfree=0\n", "\nfree=1\n"), free);
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

#[test]
fn without_the_command_line_tools_cc_the_agent_path_and_variables_stay_as_they_were() {
    let scratch = tempfile::tempdir().unwrap();
    let home = scratch.path().join("home");
    let developer = scratch.path().join("developer");
    let bin = developer.join("usr").join("bin");
    let sdk = developer.join("SDKs").join("MacOSX.sdk");
    fs::create_dir_all(&bin).unwrap();
    fs::create_dir_all(&sdk).unwrap();
    let homes = ToolHomes::at(&home).with_developer(&developer);
    let proxies: String = RUSTUP_BINS
        .iter()
        .filter(|folder| Path::new(folder).join("cargo").is_file())
        .map(|folder| format!("{folder}:"))
        .collect();
    let base = "/opt/homebrew/bin:/usr/bin:/bin";
    let before = agent_env(&home, &homes);
    assert_eq!(agent_path(&homes, base), format!("{proxies}{base}"));
    assert!(before.iter().all(|(name, _)| *name != "SDKROOT"));

    fs::write(bin.join("cc"), "cc fixture\n").unwrap();
    let bin = bin.to_str().unwrap();
    assert_eq!(
        agent_path(&homes, base),
        format!("{proxies}/opt/homebrew/bin:{bin}:/usr/bin:/bin")
    );
    assert_eq!(
        agent_path(&homes, "/usr/bin:/bin:/usr/bin"),
        format!("{proxies}{bin}:/usr/bin:/bin:/usr/bin")
    );
    assert_eq!(agent_path(&homes, "/bin"), format!("{proxies}/bin"));
    let after = agent_env(&home, &homes);
    assert_eq!(after[..before.len()], before[..]);
    assert_eq!(after[before.len()..], [("SDKROOT", sdk.into_os_string())]);

    fs::remove_dir(developer.join("SDKs").join("MacOSX.sdk")).unwrap();
    assert_eq!(agent_env(&home, &homes), before);
}

#[cfg(target_os = "macos")]
#[test]
fn a_launch_getconf_written_again_is_kept_and_one_that_changed_is_refused() {
    let home = tempfile::tempdir().unwrap();
    let _scratch = ScopedTmp::make(home.path()).unwrap();
    fs::create_dir(home.path().join(crate::launch_context::STATE)).unwrap();
    let getconf = getconf_folder(home.path()).join(GETCONF_PROGRAM);
    write_getconf(home.path(), false).unwrap();
    let written = fs::read(&getconf).unwrap();
    let again = write_getconf(home.path(), false);
    assert!(again.is_ok(), "the same getconf written again: {again:?}");
    assert_eq!(fs::read(&getconf).unwrap(), written);
    let changed = [written.as_slice(), b"exit 0\n"].concat();
    fs::write(&getconf, &changed).unwrap();
    let refused = write_getconf(home.path(), false);
    assert!(
        matches!(&refused, Err(error) if error.kind() == io::ErrorKind::AlreadyExists),
        "a changed getconf: {refused:?}"
    );
    assert_eq!(fs::read(&getconf).unwrap(), changed);
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
