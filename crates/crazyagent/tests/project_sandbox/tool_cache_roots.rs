use super::*;

/// T8: a tool cache inside another root, or a second one, is refused.
#[test]
fn a_tool_cache_inside_another_root_or_a_second_one_is_refused() {
    let case = case();
    let cache = tool_cache(&case, false);
    let inside = case.project.join("cache");
    fs::create_dir(&inside).unwrap();
    let base = sandbox(&case, SandboxNetwork::Only(Vec::new()));
    assert!(base.clone().allow_tool_cache(&inside, false).is_err());
    assert!(base.clone().allow_tool_cache(&case.home, false).is_err());
    let once = base.allow_tool_cache(&cache.root, false).unwrap();
    assert!(once.allow_tool_cache(&cache.root, false).is_err());
}
