use super::*;

/// Checks that an Uncertain lease's worktree is whole: no cut-off Git writer left its index
/// lock, its index exists, and no tracked file differs from that index; staged changes are
/// its agent's or a turn end's. Read only: `status` honours `--no-optional-locks`, unlike
/// `diff`, which refreshes the index. Input: the record. Output: success, or the refusal.
#[cfg(unix)]
pub(super) fn worktree_matches_index(record: &LeaseRecord) -> Result<(), EngineError> {
    let metadata = worktree_metadata(record)?;
    match std::fs::symlink_metadata(metadata.join("index.lock")) {
        Ok(_) => return Err(error("worktree_index_locked")),
        Err(problem) if problem.kind() == std::io::ErrorKind::NotFound => {}
        Err(problem) => return Err(error(problem)),
    }
    if !std::fs::symlink_metadata(metadata.join("index"))
        .map_err(error)?
        .is_file()
    {
        return Err(error("worktree_index_missing"));
    }
    let gitdir = metadata.to_str().ok_or_else(|| error("non_utf8_gitdir"))?;
    let entries = git(
        Path::new(&record.lease.repository),
        &[
            "-c",
            "core.protectHFS=true",
            "-c",
            "core.protectNTFS=true",
            "--git-dir",
            gitdir,
            "--work-tree",
            &record.lease.path,
            "-C",
            &record.lease.path,
            "status",
            "--porcelain=v1",
            "-z",
            "--no-branch",
            "--no-renames",
            "--untracked-files=no",
            "--ignore-submodules=all",
        ],
    )?;
    let unstaged = entries
        .split(|byte| *byte == 0)
        .any(|entry| !entry.is_empty() && entry.get(1) != Some(&b' '));
    if unstaged {
        return Err(error("worktree_content_differs_from_index"));
    }
    Ok(())
}
