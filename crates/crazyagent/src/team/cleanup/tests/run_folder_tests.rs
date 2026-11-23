use super::*;

/// #616 r2 leader 16:37: a deleted project's research run folder and roadmap status run
/// folder each hold a research agent's home, so removing the engine files a delete recorded
/// clears what that agent shut or pinned there, and both folders go.
#[cfg(target_os = "macos")]
#[test]
fn removing_a_deleted_project_s_run_folders_removes_what_their_agents_shut_and_pinned() {
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let mut runs = Vec::new();
    for name in ["job-1-1", "roadmap-status-p1-1"] {
        let folder = root.join("research").join("agents").join(name);
        let planted = plant_shut_and_pinned(&folder.join("home"));
        let inode =
            std::os::unix::fs::MetadataExt::ino(&std::fs::symlink_metadata(&folder).unwrap());
        runs.push((name, folder, planted, i64::try_from(inode).unwrap()));
    }
    let files: Vec<_> = runs
        .iter()
        .map(|(name, _, _, inode)| (name.to_string(), *inode))
        .collect();
    let refused = runs
        .iter()
        .all(|(_, _, planted, _)| std::fs::symlink_metadata(&planted.0).is_err());

    let done = crate::projects_home::remove_engine_files(&root.join("engine.db"), &files);
    let left: Vec<_> = runs
        .iter()
        .filter(|(_, folder, _, _)| folder.exists())
        .map(|(name, ..)| *name)
        .collect();
    for (_, folder, planted, _) in &runs {
        unplant(folder, planted);
    }

    assert!(refused, "a look at a shut folder was not refused");
    assert_eq!(done, ["job-1-1", "roadmap-status-p1-1"]);
    assert!(left.is_empty(), "these run folders stayed: {left:?}");
}
