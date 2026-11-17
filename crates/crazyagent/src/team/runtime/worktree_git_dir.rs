use super::*;

/// Reads where Git keeps a new agent worktree's metadata while no agent has run in it yet,
/// so the engine's later commits never rely on what the agent may write: for a linked
/// worktree a folder directly in the project's `.git/worktrees` whose `gitdir` file names
/// this worktree, both pointer files read through one open that follows no link and never
/// waits on a FIFO, kept only when regular and within `GIT_POINTER_BYTES_MAX` bytes, since
/// an agent with Full access can replace them; for an agent's own clone the real folder
/// `agent_git::git_dir` names. Inputs: the project root, the worktree and its layout.
/// Output: the root, worktree and metadata folder as text, or the plain reason.
pub(super) fn agent_git_dir(
    root: &Path,
    worktree: &Path,
    layout: Layout,
) -> Result<[String; 3], String> {
    if layout == Layout::Clone {
        let git_dir = crate::agent_git::git_dir(worktree);
        let plain = std::fs::symlink_metadata(&git_dir).is_ok_and(|found| found.is_dir());
        let text = |path: &Path| path.to_str().map(str::to_owned);
        let found = [text(root), text(worktree), text(&git_dir)];
        return match found {
            [Some(root), Some(worktree), Some(git_dir)] if plain => Ok([root, worktree, git_dir]),
            _ => Err(with_code(
                "the agent worktree could not be checked",
                Some("team_worktree_git_dir"),
            )),
        };
    }
    let refused = || {
        with_code(
            "the agent worktree could not be checked",
            Some("team_worktree_git_dir"),
        )
    };
    let read = |path: &Path| {
        crate::platform::read_plain_file(path, GIT_POINTER_BYTES_MAX).ok_or_else(refused)
    };
    let pointer = read(&worktree.join(".git"))?;
    let git_dir = pointer
        .strip_prefix("gitdir: ")
        .and_then(|path| path.strip_suffix('\n'))
        .map(PathBuf::from)
        .ok_or_else(refused)?;
    if git_dir.parent() != Some(root.join(".git").join("worktrees").as_path())
        || read(&git_dir.join("gitdir"))? != format!("{}\n", worktree.join(".git").display())
    {
        return Err(refused());
    }
    let text = |path: &Path| path.to_str().map(str::to_owned).ok_or_else(refused);
    let found = [text(root)?, text(worktree)?, text(&git_dir)?];
    assert!(found[2].starts_with(&found[0]) && found[1].starts_with(&found[0]));
    Ok(found)
}
