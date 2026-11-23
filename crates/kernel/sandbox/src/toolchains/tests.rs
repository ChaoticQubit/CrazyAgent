use super::*;

/// F26: the xcode-select link names an Xcode app's developer folder only when it leads to a
/// real `<name>.app/Contents/Developer`; the Command Line Tools, another folder and a
/// dangling link name none.
#[cfg(unix)]
#[test]
fn the_xcode_select_link_names_only_an_xcode_app_developer_folder() {
    let scratch = tempfile::tempdir().unwrap();
    let root = scratch.path().canonicalize().unwrap();
    let xcode = root
        .join("Xcode-beta.app")
        .join("Contents")
        .join("Developer");
    fs::create_dir_all(&xcode).unwrap();
    let other = root.join("Tools").join("Developer");
    fs::create_dir_all(&other).unwrap();
    let link = |name: &str, target: &Path| {
        let link = root.join(name);
        std::os::unix::fs::symlink(target, &link).unwrap();
        link
    };
    assert_eq!(xcode_from_link(&link("app", &xcode)), Some(xcode.clone()));
    assert_eq!(xcode_from_link(&link("other", &other)), None);
    assert_eq!(
        xcode_from_link(&link("gone", &root.join("missing.app"))),
        None
    );
    assert_eq!(
        xcode_from_link(&link("command-line-tools", Path::new(DEVELOPER_TOOLS))),
        None
    );
}

/// A cargo cache name holding anything but the link to the owner's cache gives way only
/// through the hooks' removal: when that removal refuses, the cargo home is refused with the
/// code the hooks return for it, and nothing is linked over the name.
#[test]
fn a_cargo_cache_name_that_will_not_give_way_refuses_with_the_hooks_code() {
    fn refuse(
        _folder: std::os::fd::BorrowedFd<'_>,
        _name: &str,
        otherwise: &'static str,
    ) -> Result<(), &'static str> {
        Err(otherwise)
    }
    let hooks = ToolchainHooks {
        remove_home_entry: refuse,
        sandboxed_git: || Path::new("/usr/bin/git"),
        mates_home: |home| home.join("mates"),
        launch_state: "crazyagent",
        remove_agent_tree: |_folder| Ok(()),
    };
    let scratch = tempfile::tempdir().unwrap();
    let root = canonical(scratch.path()).unwrap();
    let owner = root.join("owner");
    fs::create_dir_all(owner.join(".cargo/registry")).unwrap();
    let home = root.join("agent");
    fs::create_dir_all(home.join(".cargo/registry")).unwrap();
    let refused = prepare_cargo_home(&hooks, &home, &ToolHomes::at(&owner)).unwrap_err();
    assert_eq!(refused.to_string(), "cargo cache name will not give way");
    assert!(
        fs::symlink_metadata(home.join(".cargo/registry"))
            .unwrap()
            .is_dir()
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
/// F26: a project's toolchains are found by the newer marker names (`.nvmrc`,
/// `.node-version`, `uv.lock`, `.python-version`, `requirements.txt`) and one folder down,
/// never inside a hidden folder, `node_modules`, `target` or the older agents root.
#[test]
fn toolchains_are_found_by_every_marker_name_and_one_folder_down() {
    let root = tempfile::tempdir().unwrap();
    let project = root.path();
    for toolchain in Toolchain::ALL {
        assert!(
            !toolchain.used_by(project),
            "{toolchain:?} in an empty project"
        );
    }
    for (name, toolchain) in [
        (".nvmrc", Toolchain::Node),
        (".node-version", Toolchain::Node),
        ("uv.lock", Toolchain::Python),
        (".python-version", Toolchain::Python),
        ("requirements.txt", Toolchain::Python),
        ("rust-toolchain.toml", Toolchain::Rust),
    ] {
        let one = tempfile::tempdir().unwrap();
        fs::write(one.path().join(name), "x\n").unwrap();
        assert!(toolchain.used_by(one.path()), "{name}");
    }
    for skipped in [
        ".crazyagent",
        "node_modules",
        "target",
        "worktrees",
        ".hidden",
    ] {
        fs::create_dir_all(project.join(skipped)).unwrap();
        fs::write(project.join(skipped).join("Cargo.toml"), "[package]\n").unwrap();
    }
    assert!(
        !Toolchain::Rust.used_by(project),
        "skipped folders hold no project marker"
    );
    fs::create_dir_all(project.join("bot")).unwrap();
    fs::write(project.join("bot").join("Cargo.toml"), "[package]\n").unwrap();
    assert!(
        Toolchain::Rust.used_by(project),
        "a crate one folder down is found"
    );
    fs::create_dir_all(project.join("web").join("app")).unwrap();
    fs::write(project.join("web").join("app").join("package.json"), "{}\n").unwrap();
    assert!(
        !Toolchain::Node.used_by(project),
        "two folders down is not looked at"
    );
}
/// F26: without the Command Line Tools' `cc`, an agent runs the picked Xcode app's compilers
/// and its macOS SDK, and that app's developer folder is granted read-only with every choice.
#[test]
fn without_the_command_line_tools_an_agent_uses_the_picked_xcode_compilers_and_sdk() {
    let scratch = tempfile::tempdir().unwrap();
    let root = scratch.path().canonicalize().unwrap();
    let home = root.join("home");
    let developer = root.join("developer");
    fs::create_dir_all(developer.join("usr").join("bin")).unwrap();
    let xcode = root.join("Xcode.app").join("Contents").join("Developer");
    let bin = below(&xcode, XCODE_TOOLCHAIN_BIN);
    let sdk = below(&xcode, XCODE_SDK);
    fs::create_dir_all(&bin).unwrap();
    fs::create_dir_all(&sdk).unwrap();
    fs::write(bin.join("cc"), "cc fixture\n").unwrap();
    let homes = ToolHomes::at(&home)
        .with_developer(&developer)
        .with_xcode(&xcode);
    let env = agent_env(&home, &homes);
    assert!(
        env.contains(&("SDKROOT", sdk.clone().into_os_string())),
        "{env:?}"
    );
    let path = agent_path(&homes, "/usr/bin:/bin");
    assert!(
        path.ends_with(&format!("{}:/usr/bin:/bin", bin.display())),
        "{path}"
    );
    let granted = Toolchains::grant(&homes, &[]);
    assert_eq!(granted.roots(), std::slice::from_ref(&xcode));
    fs::write(developer.join("usr").join("bin").join("cc"), "cc fixture\n").unwrap();
    let env = agent_env(&home, &homes);
    assert!(
        env.iter()
            .all(|(name, value)| *name != "SDKROOT" || *value != sdk.clone().into_os_string()),
        "the Command Line Tools win when they hold cc: {env:?}"
    );
}

/// A lead's copy of the agent's own `git` runs the git the hooks name: the script written to
/// `bin/git` of the agent's temp folder holds that path, marks the lead's copy, and is a
/// 0700 file of this user.
#[test]
fn a_leads_copy_runs_the_git_the_hooks_name() {
    use std::os::unix::fs::PermissionsExt;
    fn refuse(
        _folder: std::os::fd::BorrowedFd<'_>,
        _name: &str,
        otherwise: &'static str,
    ) -> Result<(), &'static str> {
        Err(otherwise)
    }
    let hooks = ToolchainHooks {
        remove_home_entry: refuse,
        sandboxed_git: || Path::new("/opt/stand-in/bin/git"),
        mates_home: |home| home.join("mates"),
        launch_state: "crazyagent",
        remove_agent_tree: |_folder| Ok(()),
    };
    let scratch = tempfile::tempdir().unwrap();
    let tmp = canonical(scratch.path()).unwrap();
    lead_git(&hooks, &tmp, false).unwrap();
    let written = tmp.join(GIT_FOLDER).join(GIT_PROGRAM);
    let script = fs::read_to_string(&written).unwrap();
    assert!(script.contains("git='/opt/stand-in/bin/git'\nlead=1\nfree=0\nmain=0\n"));
    let metadata = fs::symlink_metadata(&written).unwrap();
    assert!(metadata.is_file());
    assert_eq!(metadata.permissions().mode() & 0o7777, 0o700);
}

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

/// The agent's own `getconf` is looked for in the launch folder the hooks name: a harness's
/// search path gets that folder's `bin` at its front when it holds `getconf`, and nothing
/// when only another folder does.
#[test]
fn the_agent_getconf_is_looked_for_in_the_launch_folder_the_hooks_name() {
    fn refuse(
        _folder: std::os::fd::BorrowedFd<'_>,
        _name: &str,
        otherwise: &'static str,
    ) -> Result<(), &'static str> {
        Err(otherwise)
    }
    let hooks = ToolchainHooks {
        remove_home_entry: refuse,
        sandboxed_git: || Path::new("/usr/bin/git"),
        mates_home: |home| home.join("mates"),
        launch_state: "stand-in",
        remove_agent_tree: |_folder| Ok(()),
    };
    let scratch = tempfile::tempdir().unwrap();
    let home = canonical(scratch.path()).unwrap();
    let named = home.join("stand-in").join("bin");
    fs::create_dir_all(&named).unwrap();
    assert_eq!(
        with_agent_git(&hooks, &home, "/usr/bin".to_owned()),
        "/usr/bin"
    );
    fs::write(named.join("getconf"), "").unwrap();
    assert_eq!(
        with_agent_git(&hooks, &home, "/usr/bin".to_owned()),
        format!("{}:/usr/bin", named.display())
    );
    let other = home.join("crazyagent").join("bin");
    fs::create_dir_all(&other).unwrap();
    fs::write(other.join("getconf"), "").unwrap();
    fs::remove_file(named.join("getconf")).unwrap();
    assert_eq!(
        with_agent_git(&hooks, &home, "/usr/bin".to_owned()),
        "/usr/bin"
    );
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
