use super::*;
use std::process::Command;

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

#[test]
fn a_folder_name_from_uv_is_lowercase_letters_digits_and_dashes_only() {
    for name in ["sdists-v10", "wheels-v6", "a", &"a".repeat(64)] {
        assert!(plain_name(name), "{name}");
    }
    for name in [
        "",
        ".git",
        ".GIT",
        ".Git",
        "..",
        ".",
        "Upper",
        "a/b",
        "a.b",
        "a b",
        &"a".repeat(65),
    ] {
        assert!(!plain_name(name), "{name}");
    }
}

/// T8 C2 (R2): no two level and shape pairs share a cache, Full access gets none, and under
/// One per agent each agent gets its own, apart from every shared one.
#[test]
fn shared_caches_differ_by_level_and_shape_and_full_access_gets_none() {
    let mut keys = std::collections::BTreeSet::new();
    for access in [AgentAccess::Sandboxed, AgentAccess::OwnBranch] {
        for shape in CacheShape::ALL {
            assert!(keys.insert(key(ToolCacheScope::Shared, access, shape, "35").unwrap()));
        }
        for agent in ["35", "52", "master"] {
            let own = key(
                ToolCacheScope::PerAgent,
                access,
                CacheShape::Worktree,
                agent,
            )
            .unwrap();
            assert_eq!(
                own,
                key(ToolCacheScope::PerAgent, access, CacheShape::Clone, agent).unwrap()
            );
            keys.insert(own);
        }
    }
    assert_eq!(keys.len(), 2 * CacheShape::ALL.len() + 6);
    for scope in [ToolCacheScope::Shared, ToolCacheScope::PerAgent] {
        assert_eq!(
            key(scope, AgentAccess::Full, CacheShape::ProjectRoot, "35"),
            None
        );
    }
}

/// T8 C3 (R3): a bucket named `.git` or over 64 bytes, or a 17th bucket, refuses the whole
/// fresh cache, and up to 16 plain buckets pass.
#[test]
fn a_bucket_name_or_count_out_of_bounds_refuses_the_cache() {
    let mark = |cache: &Path, name: &str| {
        std::fs::create_dir_all(cache.join(name)).unwrap();
        std::fs::write(cache.join(name).join(MARKER), "").unwrap();
    };
    for name in [".git", ".GIT", &"a".repeat(65), "Upper", "a.b"] {
        let temp = tempfile::tempdir().unwrap();
        mark(temp.path(), "sdists-v10");
        mark(temp.path(), name);
        assert_eq!(
            buckets_in(temp.path()),
            Err("tool_cache_bucket_refused"),
            "{name}"
        );
    }
    let temp = tempfile::tempdir().unwrap();
    for index in 0..BUCKETS_MAX {
        mark(temp.path(), &format!("bucket-{index}"));
    }
    std::fs::create_dir(temp.path().join("unmarked")).unwrap();
    std::fs::write(temp.path().join("CACHEDIR.TAG"), "tag").unwrap();
    assert_eq!(buckets_in(temp.path()).unwrap().len(), BUCKETS_MAX);
    mark(temp.path(), "bucket-16");
    assert_eq!(buckets_in(temp.path()), Err("tool_cache_bucket_refused"));
}
