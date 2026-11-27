use super::*;

/// AGENT-REF-ALLOW-NARROW r1 L1: the runtime hands both teammate ref rules the lead's branch
/// as the engine records it. A native lead in its own clone has its claimed task branch
/// named in its teammates' profile, and the setup git for a linked lead's teammates names
/// that lead's branch; neither profile keeps the old any-task teammate pattern.
#[cfg(target_os = "macos")]
#[test]
fn the_teammate_ref_rules_name_the_lead_s_recorded_branch() {
    let fixture = Fixture::new(1);
    own_branch_access(&fixture);
    let (runtime, id, plan) = native_lead_in(&fixture, Layout::Clone, false);
    let branch = crate::landing::agent_branch(
        &fixture.connection,
        &plan.working_dir,
        &crate::agent_git::git_dir(&plan.working_dir),
    )
    .unwrap()
    .expect("the clone lead's task branch is recorded");
    assert!(
        crate::agent_git::is_agent_branch(&branch) && branch.starts_with("crazyagent/"),
        "{branch}"
    );
    let teammates = std::fs::read_to_string(agent_teammates_profile(&runtime.state, id)).unwrap();
    assert!(
        teammates.contains(&format!("refs/heads/{branch}--[a-z0-9-]+(\\.lock)?$")),
        "the teammates' profile names {branch}"
    );
    assert!(!teammates.contains("refs/heads/crazyagent/[a-z0-9-]+--"));

    let fixture = Fixture::new(1);
    let (runtime, id, plan) = native_lead_in(&fixture, Layout::Linked, false);
    let link = std::fs::read_to_string(plan.working_dir.join(".git")).unwrap();
    let git_dir = PathBuf::from(link.trim().strip_prefix("gitdir: ").unwrap());
    let branch = crate::landing::agent_branch(&fixture.connection, &plan.working_dir, &git_dir)
        .unwrap()
        .expect("the linked lead's branch is named");
    assert!(crate::agent_git::is_agent_branch(&branch), "{branch}");
    let repository = fixture.base.join("project").canonicalize().unwrap();
    let (_, profile) = runtime
        .teammates_git(&repository, &plan.working_dir, &git_dir, id)
        .unwrap();
    assert_eq!(profile, agent_teammates_setup_profile(&runtime.state, id));
    let setup = std::fs::read_to_string(&profile).unwrap();
    assert!(
        setup.contains(&format!(
            "refs/heads/(crazyagent|{branch}--[a-z0-9-]+(\\.lock)?)$"
        )),
        "the setup profile names {branch}"
    );
    assert!(!setup.contains("refs/heads/crazyagent(/[a-z0-9-]+--"));
}

/// A linked lead's teammates setup git over a project folder that holds one of the engine
/// root's secrets, its account keys folder leading into the project, is refused before the
/// setup profile is written, so no teammate setup grant reaches the engine's keys.
#[test]
fn a_linked_leads_teammates_setup_over_a_project_holding_an_engine_secret_is_refused() {
    let fixture = Fixture::new(1);
    let (runtime, id, plan) = native_lead_in(&fixture, Layout::Linked, false);
    let link = std::fs::read_to_string(plan.working_dir.join(".git")).unwrap();
    let git_dir = PathBuf::from(link.trim().strip_prefix("gitdir: ").unwrap());
    let repository = fixture.base.join("project").canonicalize().unwrap();
    let state = runtime.state.canonicalize().unwrap();
    assert_eq!(state.parent(), Some(fixture.base.as_path()));
    let keys = fixture.base.join(crate::storage::accounts::ACCOUNT_KEYS);
    assert!(std::fs::symlink_metadata(&keys).is_err());
    std::fs::create_dir(repository.join("keys")).unwrap();
    std::os::unix::fs::symlink(repository.join("keys"), &keys).unwrap();
    let profile = agent_teammates_setup_profile(&runtime.state, id);
    assert!(!profile.exists());
    let refused = runtime.teammates_git(&repository, &plan.working_dir, &git_dir, id);
    assert_eq!(refused.map(|_| ()), Err("lead_sandbox_unavailable"));
    assert!(!profile.exists());
}

/// A linked lead's teammates setup git over a project folder that holds the engine root, its
/// state folder inside the project, is refused and writes no setup profile. The setup's own
/// confinement refuses it with no engine guard: `ProjectSandbox::confine_to_teammates_setup`
/// ends in `confinement_kept`, which refuses the agent's home sitting in the project outside
/// the confinement with its named reason.
#[test]
fn a_linked_leads_teammates_setup_over_a_project_holding_the_engine_root_is_refused() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new(1);
    let (mut runtime, id, plan) = native_lead_in(&fixture, Layout::Linked, false);
    let link = std::fs::read_to_string(plan.working_dir.join(".git")).unwrap();
    let git_dir = PathBuf::from(link.trim().strip_prefix("gitdir: ").unwrap());
    let repository = fixture.base.join("project").canonicalize().unwrap();
    let engine = repository.join("data");
    runtime.state = engine.join("state");
    let folders = [
        agent_home(&runtime.state, id),
        runtime.agent_folder(id).join("tmp"),
        runtime.state.join("profiles"),
    ];
    for folder in &folders {
        std::fs::create_dir_all(folder).unwrap();
        std::fs::set_permissions(folder, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    std::fs::write(engine.join("engine.db"), "engine secret\n").unwrap();
    let profile = agent_teammates_setup_profile(&runtime.state, id);
    assert!(!profile.exists());
    let branch = crate::landing::agent_branch(&runtime.connection, &plan.working_dir, &git_dir)
        .unwrap()
        .unwrap();
    let unguarded = ProjectSandbox::new(
        &crate::sandbox::PROFILE_HOOKS,
        &kernel_sandbox::paths::one_spelling(repository.clone()),
        &agent_home(&runtime.state, id),
        &runtime.agent_folder(id).join("tmp"),
        SandboxNetwork::Only(Vec::new()),
    )
    .unwrap()
    .confine_to_teammates_setup(&plan.working_dir, &branch)
    .map(|_| ());
    let kept = "a writable root reaches the project outside the agent's confinement";
    assert!(
        matches!(&unguarded, Err(SandboxError::Invalid(reason)) if reason == kept),
        "{unguarded:?}"
    );
    let refused = runtime.teammates_git(&repository, &plan.working_dir, &git_dir, id);
    let refused = refused.map(|_| ());
    assert!(
        matches!(
            refused,
            Err("teammates_setup_unavailable" | "lead_sandbox_unavailable")
        ),
        "{refused:?}"
    );
    assert!(!profile.exists());
}

/// A team launch with no engine root to guard is refused with the plain sandbox line and
/// its own code, as `with_code` writes it, so the code reaches the owner unwithheld.
#[test]
fn a_launch_with_no_engine_root_names_its_own_code() {
    assert_eq!(
        crate::sandbox::NO_ENGINE_ROOT,
        with_code(
            "the agent sandbox could not be set up",
            Some(crate::sandbox::ENGINE_ROOT_MISSING)
        )
    );
}
