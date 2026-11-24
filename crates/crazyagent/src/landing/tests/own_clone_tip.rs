use super::*;

/// An agent in its own clone already on the tip hears nothing (DEC-147.6).
#[test]
fn an_own_clone_agent_on_the_tip_is_told_nothing() {
    let mut case = Case::own_branch();
    seat_team_agent(&mut case);
    assert!(!tell_main_moved(
        &case.db,
        &case.workspace.project,
        CLONE_AGENT
    ));
    assert!(engine_notes(&case).is_empty());
}

/// V243-FIRMLINK-SWEEP D5: an agent clone is never made inside the owner repository's
/// `.git`, whichever of the two macOS names the repository and the new folder use.
#[cfg(target_os = "macos")]
#[test]
fn an_agent_clone_inside_git_is_refused_under_either_data_volume_name() {
    let case = Case::new();
    let repo = std::fs::canonicalize(&case.repo).unwrap();
    let git = repo.join(".git");
    let long = |path: &std::path::Path| {
        std::path::PathBuf::from(format!("/System/Volumes/Data{}", path.display()))
    };
    assert!(long(&git).is_dir());
    for (repository, worktree) in [
        (repo.clone(), git.join("agent-one")),
        (repo.clone(), long(&git).join("agent-two")),
        (long(&repo), git.join("agent-three")),
    ] {
        let refused = add_agent_clone(&repository, &worktree).unwrap_err();
        assert_eq!(
            refused.0, "changes_workspace_invalid",
            "repository {repository:?}, clone {worktree:?}"
        );
        assert!(!worktree.exists(), "{worktree:?}");
    }
}
