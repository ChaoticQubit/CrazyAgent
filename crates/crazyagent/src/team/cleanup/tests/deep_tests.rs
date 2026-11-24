use super::*;

const TOO_DEEP: &str = "cleanup_folder_too_deep";

/// Makes `depth` nested folders named `d` below `top`, so the deepest sits `depth` folders
/// below it. Inputs: the top folder and the depth. Output: the deepest folder.
fn nested(top: &Path, depth: usize) -> PathBuf {
    let deepest = (0..depth).fold(top.to_path_buf(), |at, _| at.join("d"));
    std::fs::create_dir_all(&deepest).unwrap();
    assert_eq!(
        deepest.components().count(),
        top.components().count() + depth
    );
    deepest
}

/// A home entry whose deepest folder sits [`HOME_DEPTH_MAX`] folders below it goes, and one
/// with a folder one deeper refuses with its own code and stays whole at its name, so the
/// owner can be told which folder to delete.
#[test]
fn a_home_entry_one_folder_past_the_depth_bound_refuses_as_too_deep_and_one_at_it_goes() {
    use std::os::fd::AsFd;
    let tmp = tempfile::tempdir().unwrap();
    let home = std::fs::canonicalize(tmp.path()).unwrap();
    nested(&home.join("at"), HOME_DEPTH_MAX);
    nested(&home.join("past"), HOME_DEPTH_MAX + 1);
    let parent = walk_handle(&home);

    let at = remove_home_entry(parent.as_fd(), "at").map_err(|problem| problem.0);
    let past = remove_home_entry(parent.as_fd(), "past").map_err(|problem| problem.0);

    assert_eq!(at, Ok(()));
    assert_eq!(past, Err(TOO_DEEP.to_string()));
    assert!(!home.join("at").exists() && home.join("past").is_dir());
}

/// An agent home's reset of an entry past the depth bound keeps its own refusal,
/// `agent_home_unwritable`, so only Clean up names the folder too deep to remove.
#[test]
fn a_home_reset_past_the_depth_bound_keeps_its_own_refusal() {
    let tmp = tempfile::tempdir().unwrap();
    let home = std::fs::canonicalize(tmp.path()).unwrap();
    nested(&home.join("tree"), HOME_DEPTH_MAX + 1);

    let refused = crate::agent_home::remove_entry(&home.join("tree")).map_err(|problem| problem.0);

    assert_eq!(refused, Err("agent_home_unwritable".to_string()));
    assert_eq!(
        home_code(&EngineError(TOO_DEEP.into()), "agent_home_unwritable"),
        "agent_home_unwritable"
    );
}

/// Clean up keeps an agent whose own tool cache holds a folder past the depth bound with
/// `cleanup_folder_too_deep` and the absolute cache folder the owner must delete, also when
/// it removed no agent; once the owner deletes that folder, Clean up removes the agent.
#[cfg(target_os = "macos")]
#[test]
fn clean_up_names_a_tool_cache_folder_too_deep_to_remove_and_takes_the_agent_once_it_is_gone() {
    use kernel_sandbox::policy::{AgentAccess, ToolCacheScope};
    use kernel_sandbox::tool_cache::{CacheShape, key, prepare, project_folder};
    let case = Case::new();
    case.agent(1, "sub", "stopped");
    let runtime = case.team.parent().unwrap();
    let project = case.repo.to_str().unwrap();
    let name = key(
        ToolCacheScope::PerAgent,
        AgentAccess::Sandboxed,
        CacheShape::Worktree,
        "1",
    )
    .unwrap();
    prepare(
        &crate::sandbox::TOOL_CACHE_HOOKS,
        runtime,
        project,
        &name,
        None,
    )
    .unwrap();
    let cache = project_folder(runtime, project).join(&name);
    nested(&cache.join("deep"), HOME_DEPTH_MAX);
    assert!(cache.is_absolute());

    let kept = case.remove(&[1]);

    assert_eq!(
        kept,
        Removal {
            agents: Vec::new(),
            kept: Some(TOO_DEEP.into()),
            folder: Some(cache.to_str().unwrap().to_owned()),
        }
    );
    assert!(cache.is_dir() && !case.worktree(1).exists());
    std::fs::remove_dir_all(&cache).unwrap();
    assert_eq!(case.remove(&[1]), removed(&[1]));
    assert!(case.history_kept(1));
}

/// T8 design 9: Clean up takes the tool caches One per agent gave the agent whose worktree
/// it removes, at each level, with its native teammates' and their priming records, and
/// keeps another agent's and the shared caches, since a removed agent never runs again.
#[cfg(target_os = "macos")]
#[test]
fn removing_an_agents_worktree_takes_its_own_tool_caches_and_keeps_the_shared_ones() {
    use kernel_sandbox::policy::{AgentAccess, ToolCacheScope};
    use kernel_sandbox::tool_cache::{CacheShape, key, prepare, project_folder};
    let case = Case::new();
    case.agent(1, "sub", "stopped");
    case.agent(2, "sub", "stopped");
    let runtime = case.team.parent().unwrap();
    let project = case.repo.to_str().unwrap();
    let names: Vec<String> = [
        (
            ToolCacheScope::PerAgent,
            AgentAccess::Sandboxed,
            CacheShape::Worktree,
            "1",
        ),
        (
            ToolCacheScope::PerAgent,
            AgentAccess::OwnBranch,
            CacheShape::Clone,
            "1",
        ),
        (
            ToolCacheScope::PerAgent,
            AgentAccess::Sandboxed,
            CacheShape::Teammate,
            "1-teammates",
        ),
        (
            ToolCacheScope::PerAgent,
            AgentAccess::Sandboxed,
            CacheShape::Worktree,
            "2",
        ),
        (
            ToolCacheScope::Shared,
            AgentAccess::Sandboxed,
            CacheShape::Worktree,
            "1",
        ),
    ]
    .into_iter()
    .map(|(scope, access, shape, agent)| key(scope, access, shape, agent).unwrap())
    .collect();
    let folder = project_folder(runtime, project);
    for name in &names {
        prepare(
            &crate::sandbox::TOOL_CACHE_HOOKS,
            runtime,
            project,
            name,
            None,
        )
        .unwrap();
        write(&folder.join(format!("{name}.primed")), b"record\n");
    }
    assert_eq!(case.remove(&[1]), removed(&[1]));
    let left = |name: &str| std::fs::symlink_metadata(folder.join(name)).is_ok();
    for name in &names[..3] {
        assert!(!left(name) && !left(&format!("{name}.primed")), "{name}");
    }
    for name in &names[3..] {
        assert!(left(name) && left(&format!("{name}.primed")), "{name}");
    }
    assert!(case.history_kept(1) && case.kept(2));
}
