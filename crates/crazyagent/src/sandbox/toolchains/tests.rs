use super::*;

#[test]
fn the_agent_git_script_is_refused_for_the_xcrun_shim_and_any_path_that_is_not_plain() {
    let system = Path::new("/usr/bin/git");
    assert!(xcrun_shim(system, true), "a Mac has the xcrun shim there");
    assert!(
        !xcrun_shim(system, false),
        "on Linux /usr/bin/git is the real git, so agents get their own git that runs it"
    );
    for real in [
        "/opt/homebrew/bin/git",
        "/usr/bin/git2",
        "/usr/local/bin/git",
    ] {
        assert!(!xcrun_shim(Path::new(real), true), "{real}");
        assert!(!xcrun_shim(Path::new(real), false), "{real}");
    }
    for copy in [GitCopy::Worker, GitCopy::Lead, GitCopy::Free] {
        let script = git_script(system, copy, true);
        assert_eq!(script.is_none(), cfg!(target_os = "macos"), "{copy:?}");
        assert!(
            script.is_none_or(|script| script.starts_with("#!/bin/sh\ngit='/usr/bin/git'\n")),
            "{copy:?}"
        );
    }
    for git in [
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

/// F19, DEC-107: with "Catch up with main" on, a lead's copy merges the landed main at
/// `crazyagent/main` and its own workers' branches, still refuses another branch, and its
/// refusal tells it how to catch up with main; with the setting off the lead's copy refuses
/// main too and says nothing of it.
#[test]
fn a_leads_copy_merges_main_and_its_own_workers_and_names_the_main_merge_when_refusing() {
    struct Removed(PathBuf);
    impl Drop for Removed {
        fn drop(&mut self) {
            let _ = remove_tmp(&self.0);
        }
    }
    let git = crate::research::runner::sandboxed_git();
    for catch_up in [true, false] {
        let home = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        for args in [
            &["init", "-q", "-b", "main"][..],
            &["commit", "-q", "--allow-empty", "-m", "start"],
            &["checkout", "-q", "-b", "landed"],
            &["commit", "-q", "--allow-empty", "-m", "landed work"],
            &["update-ref", "refs/crazyagent/main", "landed"],
            &["checkout", "-q", "-b", "other", "main"],
            &["commit", "-q", "--allow-empty", "-m", "other work"],
            &["checkout", "-q", "-b", "task--ann", "main"],
            &["commit", "-q", "--allow-empty", "-m", "ann work"],
            &["checkout", "-q", "-b", "task", "main"],
        ] {
            let done = git_run(git, repo.path(), home.path(), args);
            assert!(done.status.success(), "{args:?} {done:?}");
        }
        let tmp = make_tmp(home.path()).unwrap();
        let _removed = Removed(home.path().to_path_buf());
        lead_git(&tmp, catch_up).unwrap();
        let wrapper = tmp.join(GIT_FOLDER).join(GIT_PROGRAM);
        let main = git_run(
            &wrapper,
            repo.path(),
            home.path(),
            &["merge", "-q", "--no-edit", "crazyagent/main"],
        );
        assert_eq!(main.status.success(), catch_up, "{main:?}");
        let ann = git_run(
            &wrapper,
            repo.path(),
            home.path(),
            &["merge", "-q", "--no-ff", "--no-edit", "task--ann"],
        );
        assert!(
            ann.status.success(),
            "{}",
            String::from_utf8_lossy(&ann.stderr)
        );
        let other = git_run(
            &wrapper,
            repo.path(),
            home.path(),
            &["merge", "--no-edit", "other"],
        );
        assert!(!other.status.success());
        let stderr = String::from_utf8_lossy(&other.stderr);
        assert!(stderr.contains("git merge is refused"), "{stderr}");
        assert!(
            stderr.contains("A lead merges only the branches of its own workers"),
            "{stderr}"
        );
        assert_eq!(
            stderr.contains("git merge crazyagent/main"),
            catch_up,
            "{stderr}"
        );
    }
}

/// F19, DEC-107: a worker's merge of its own lead's branch stays refused, with Catch up with
/// main on or off, and the refusal tells the worker the path that works: its lead's work
/// reaches its branch through Update on its change set, so it tells its lead (HB CM#335).
#[test]
fn a_workers_refused_merge_of_its_leads_branch_names_update_on_its_change_set() {
    struct Removed(PathBuf);
    impl Drop for Removed {
        fn drop(&mut self) {
            let _ = remove_tmp(&self.0);
        }
    }
    let git = crate::research::runner::sandboxed_git();
    for catch_up in [false, true] {
        let home = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        for args in [
            &["init", "-q", "-b", "task"][..],
            &["commit", "-q", "--allow-empty", "-m", "start"],
            &["checkout", "-q", "-b", "task--ann"],
            &["commit", "-q", "--allow-empty", "-m", "ann work"],
            &["checkout", "-q", "task"],
            &["commit", "-q", "--allow-empty", "-m", "lead work"],
            &["checkout", "-q", "task--ann"],
        ] {
            let done = git_run(git, repo.path(), home.path(), args);
            assert!(done.status.success(), "{args:?} {done:?}");
        }
        let tmp = make_tmp(home.path()).unwrap();
        let _removed = Removed(home.path().to_path_buf());
        if catch_up {
            catch_up_git(&tmp).unwrap();
        }
        let wrapper = tmp.join(GIT_FOLDER).join(GIT_PROGRAM);
        let before = git_run(git, repo.path(), home.path(), &["rev-parse", "HEAD"]);
        let done = git_run(
            &wrapper,
            repo.path(),
            home.path(),
            &["merge", "--no-edit", "task"],
        );
        assert!(!done.status.success(), "the lead's branch was merged");
        let stderr = String::from_utf8_lossy(&done.stderr);
        assert!(stderr.contains("git merge is refused"), "{stderr}");
        assert!(
            stderr.contains(
                "Work from your lead's branch reaches yours through Update on your change set"
            ),
            "{stderr}"
        );
        assert!(stderr.contains("tell your lead what you need"), "{stderr}");
        let after = git_run(git, repo.path(), home.path(), &["rev-parse", "HEAD"]);
        assert_eq!(after.stdout, before.stdout);
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
