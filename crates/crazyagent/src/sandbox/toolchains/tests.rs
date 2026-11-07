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
