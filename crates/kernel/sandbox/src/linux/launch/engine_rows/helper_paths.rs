use super::*;

/// An engine whose own folder is not cargo's `deps` takes only the helper beside it, even
/// with none there and a helper in the folder above, and a `deps` folder further up the
/// path changes nothing (V243-LANDLOCK-TEST-HELPER-PATH).
#[test]
fn an_engine_outside_a_deps_folder_never_takes_another_helper() {
    let root = tempfile::tempdir().unwrap();
    for folder in ["opt/bin", "deps/bin", "depsx"] {
        let engine = root.path().join(folder).join("crazyagent");
        fs::create_dir_all(engine.parent().unwrap()).unwrap();
        fs::write(engine.parent().unwrap().with_file_name(HELPER_NAME), "").unwrap();
        assert_eq!(
            helper_for(&engine),
            engine.with_file_name(HELPER_NAME),
            "{folder}"
        );
    }
}

/// A debug test binary in cargo's `deps` folder with no helper beside it takes cargo's own
/// build of the helper in the folder above; one beside it wins, and with neither it keeps
/// the missing one beside it (V243-LANDLOCK-TEST-HELPER-PATH).
#[cfg(debug_assertions)]
#[test]
fn a_test_binary_in_cargos_deps_folder_takes_cargos_helper_build_only_when_none_is_beside_it() {
    let root = tempfile::tempdir().unwrap();
    let deps = root.path().join("debug").join("deps");
    fs::create_dir_all(&deps).unwrap();
    let engine = deps.join("linux_sandbox-0123abcd");
    let (beside, built) = (deps.join(HELPER_NAME), deps.with_file_name(HELPER_NAME));
    assert_eq!(
        helper_for(&engine),
        beside,
        "neither: the missing one beside it"
    );
    fs::write(&built, "").unwrap();
    assert_eq!(helper_for(&engine), built, "cargo's build, one folder up");
    fs::write(&beside, "").unwrap();
    assert_eq!(helper_for(&engine), beside, "the one beside it wins");
}

/// V243-LINUX-KEEP-FREE-REFUSAL-NAME: without the helper no agent starts on any network,
/// and the refusal says the helper is missing, not that the kernel is too old.
#[test]
fn no_agent_starts_without_the_helper_and_the_refusal_says_so() {
    for network in [
        SandboxNetwork::Open,
        SandboxNetwork::Only(vec![SandboxRemote::AnyHost(443)]),
        SandboxNetwork::Only(Vec::new()),
    ] {
        let (_absent, sandbox) = project(network.clone());
        let missing = Path::new("/nonexistent/crazyagent-landlock");
        assert_eq!(
            refusal(LinuxSandbox::detect(sandbox, missing)),
            HELPER_MISSING,
            "{network:?}"
        );
    }
}

#[test]
fn without_the_helper_bubblewrap_starts_no_agent() {
    let (_closed, sandbox) = project(SandboxNetwork::Only(Vec::new()));
    match LinuxSandbox::detect(sandbox, Path::new("/nonexistent/crazyagent-landlock")) {
        Err(SandboxError::Unavailable(reason)) => {
            assert!(reason.contains("helper"), "{reason}");
        }
        other => panic!("bubblewrap without the socket filter must be refused: {other:?}"),
    }
}

/// The owner's setting Off, and a host that only has bubblewrap, start no supervisor:
/// the launch stays as it was before the supervisor (DEC-179 add.2 Q1).
#[test]
fn the_supervisor_setting_off_and_bubblewrap_alone_start_no_supervisor() {
    let helper = std::env::current_exe().unwrap();
    let (_temp, sandbox) = project(SandboxNetwork::Only(Vec::new()));
    let landlock = LinuxBackend::Landlock {
        helper: helper.clone(),
    };
    let off = LinuxSandbox::new(sandbox.clone(), landlock).unwrap();
    assert_eq!(
        off.supervise(LinuxSupervisor::Off, None).supervision(),
        None
    );
    let bubblewrap = LinuxBackend::Bubblewrap {
        bwrap: helper.clone(),
        helper,
    };
    let alone = LinuxSandbox::new(sandbox, bubblewrap).unwrap();
    assert_eq!(
        alone.supervise(LinuxSupervisor::On, None).supervision(),
        None
    );
}

/// AppArmor keeps unprivileged programs out of user namespaces only where its setting
/// reads 1; any other value, or no file, leaves them free, so the AppArmor line shows
/// only where it is the reason nothing nests (DEC-179 SBX-3).
#[test]
fn only_an_apparmor_setting_of_one_restricts_user_namespaces() {
    assert_eq!(
        APPARMOR_USERNS,
        "/proc/sys/kernel/apparmor_restrict_unprivileged_userns"
    );
    assert!(restricts(Some("1\n")));
    assert!(restricts(Some("1")));
    for free in [
        Some("0\n"),
        Some(""),
        Some("2\n"),
        Some("11\n"),
        Some("1 0"),
        None,
    ] {
        assert!(!restricts(free), "{free:?}");
    }
}
