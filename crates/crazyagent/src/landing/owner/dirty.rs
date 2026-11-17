use super::*;

/// The paths staged in the owner's index against the tip, renames split into a delete and
/// an add. Inputs: the checkout and the tip. Output: the paths, sorted.
pub(super) fn staged_paths(owner: &Repo, tip: &str) -> Result<BTreeSet<String>, EngineError> {
    listed_names(
        owner,
        &[
            "diff-index",
            "--cached",
            "--name-only",
            "-z",
            "--no-renames",
            tip,
            "--",
        ],
    )
}

/// The owner's files in an Accept's way (F21): in `files` every path staged against the tip
/// and every unstaged edit to a file the squash changes, in `untracked` every untracked or
/// ignored file where the squash adds one; submodules count by their recorded commit.
/// Inputs: the checkout, its root, the tip and the squash tree. Output: both lists, sorted.
pub(super) fn dirty_paths(
    owner: &Repo,
    repository: &Path,
    tip: &str,
    tree: &str,
) -> Result<OwnerDirty, EngineError> {
    let staged = staged_paths(owner, tip)?;
    let mut tracked = staged.clone();
    let unstaged = listed_names(
        owner,
        &[
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--ignore-submodules=all",
            "--name-only",
            "-z",
            "--no-renames",
            "--",
        ],
    )?;
    if !unstaged.is_empty() {
        let changed = listed_names(
            owner,
            &[
                "diff-tree",
                "-r",
                "-z",
                "--name-only",
                "--no-renames",
                tip,
                tree,
            ],
        )?;
        tracked.extend(unstaged.into_iter().filter(|path| changed.contains(path)));
    }
    let added = listed_names(
        owner,
        &[
            "diff-tree",
            "-r",
            "-z",
            "--name-only",
            "--no-renames",
            "--diff-filter=A",
            tip,
            tree,
        ],
    )?;
    let mut untracked = BTreeSet::new();
    for path in added {
        if staged.contains(&path) {
            continue;
        }
        match std::fs::symlink_metadata(repository.join(&path)) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            _ => {
                untracked.insert(path);
            }
        }
    }
    assert!(untracked.iter().all(|path| !tracked.contains(path)));
    Ok(OwnerDirty {
        files: tracked.into_iter().collect(),
        untracked: untracked.into_iter().collect(),
    })
}
