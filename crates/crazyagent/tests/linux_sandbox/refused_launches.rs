use super::*;

/// A launch on Linux whose account home is not the boundary's agent home is refused: the
/// boundary sets HOME to its own agent home, so the harness would otherwise run with a home
/// its account was not made in.
#[test]
fn a_linux_launch_with_another_home_than_its_boundary_is_refused() {
    let case = case();
    let other = case.outside.join("other-home");
    fs::create_dir_all(&other).unwrap();
    let request = linux_launch(
        &case,
        LaunchBoundary::Installed(Arc::new(team_boundary(&case))),
        &other,
    );
    let refused = crazyagent::harnesses::launch::command(&request).unwrap_err();
    assert_eq!(refused.0, "launch_environment_refused");
}

/// A Full access pane on Linux is refused before its command is built (rv-sec3 #709 F1,
/// rv-sec2 #710 C5): the pane path takes the same launch check as a headless start, so an
/// unconfined boundary never becomes a bare pane command that runs outside any sandbox.
#[test]
fn a_full_access_pane_on_linux_is_refused_before_its_command_is_built() {
    let case = case();
    let full = team_boundary(&case).unconfined();
    let mut request = linux_launch(&case, LaunchBoundary::Installed(Arc::new(full)), &case.home);
    request.harness = "claude-code".into();
    let before = tree(&case.home);
    let refused = crazyagent::harnesses::interactive::command(
        &request,
        None,
        &crazyagent::harnesses::interactive::Run {
            session_id: "s5a-full-access-pane",
            key_file: None,
            resume: false,
        },
    )
    .err()
    .unwrap_or_else(|| panic!("a Full access pane command was built on Linux"));
    assert_eq!(refused.0, "launch_sandbox_unavailable");
    assert_eq!(tree(&case.home), before);
}

/// A Full access launch on Linux is refused and builds nothing (rv-sec3 #709 F1, rv-sec2
/// #710 C5): Full access is only an installed boundary marked unconfined, and that boundary
/// refuses with the macOS-only line, so no request reaches the unconfined command here.
#[test]
fn a_full_access_launch_on_linux_is_refused_and_builds_nothing() {
    let case = case();
    let full = team_boundary(&case).unconfined();
    assert!(matches!(
        full.wrap(&Command::new("/usr/bin/true")),
        Err(SandboxError::Unavailable(reason))
            if reason == kernel_sandbox::profile::consts::FULL_ACCESS_NEEDS_MACOS
    ));
    let request = linux_launch(&case, LaunchBoundary::Installed(Arc::new(full)), &case.home);
    assert!(request.boundary.is_unconfined());
    let before = tree(&case.home);
    let refused = crazyagent::harnesses::launch::command(&request).unwrap_err();
    assert_eq!(refused.0, "launch_sandbox_unavailable");
    assert_eq!(tree(&case.home), before);
}

/// A confined agent whose project hides a folder is still refused on Linux, for its
/// confinement and not for the folder, since neither backend holds a confined agent yet:
/// team and research launches stay refused as they were (leader 12:25).
#[test]
fn a_confined_agent_with_a_hidden_folder_is_refused_as_confined_not_for_the_folder()
-> Result<(), Box<dyn std::error::Error>> {
    let checkouts = checkouts();
    let folder = checkouts.project.join("private");
    fs::create_dir(&folder)?;
    let profiles = checkouts.project.parent().unwrap().join("profiles");
    fs::create_dir_all(&profiles)?;
    let refused = InstalledSandbox::install(
        confined(&checkouts).hide(&folder).unwrap(),
        &profiles.join("agent.sb"),
        LinuxSupervisor::On,
        LinuxStrict::Off,
    )
    .err();
    assert!(
        matches!(&refused, Some(SandboxError::Unavailable(reason))
            if reason.contains("confined") && !reason.contains("hidden")),
        "{refused:?}"
    );
    Ok(())
}

#[test]
fn landlock_holds_the_project_boundary_on_this_kernel() {
    let case = case();
    let sandbox = pinned(policy(&case, SandboxNetwork::Open));
    assert!(
        matches!(sandbox.backend(), LinuxBackend::Landlock { .. }),
        "Linux test machines need Landlock file rules (Linux 6.2 or newer): {:?}",
        sandbox.backend()
    );
    assert_boundary(&case, &run(&sandbox, &case, &boundary_script(&case)));
}

#[path = "root_swap.rs"]
mod root_swap;
