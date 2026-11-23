use super::*;

/// An agent's own tool cache goes only through the hooks' tree removal: when that removal
/// refuses, removing the agent's caches fails with the hooks' error and the cache folder
/// stays.
#[test]
fn an_agents_own_cache_goes_only_through_the_hooks_removal() {
    fn unpin(_parent: std::os::fd::BorrowedFd<'_>, _name: &str) {}
    fn kept(_folder: &Path) -> std::io::Result<()> {
        Err(std::io::Error::other("kept by the stand-in"))
    }
    fn priming(_folder: &Path, _uv: &Path) -> Command {
        Command::new("/usr/bin/false")
    }
    let hooks = ToolCacheHooks {
        unpin,
        remove_agent_tree: kept,
        priming_command: priming,
    };
    let runtime = tempfile::tempdir().unwrap();
    let key = key(
        ToolCacheScope::PerAgent,
        AgentAccess::Sandboxed,
        CacheShape::Worktree,
        "mate",
    )
    .unwrap();
    let cache = project_folder(runtime.path(), "project").join(key);
    std::fs::create_dir_all(&cache).unwrap();
    let refused = remove_agent(&hooks, runtime.path(), "project", "mate").unwrap_err();
    assert_eq!(refused.to_string(), "kept by the stand-in");
    assert!(cache.is_dir());
}
