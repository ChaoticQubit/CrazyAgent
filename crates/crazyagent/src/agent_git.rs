use std::path::{Path, PathBuf};

const BRANCH_PREFIX: &str = "agent/";
const FOLDER_BYTES_MAX: usize = 64;

/// Where an agent keeps its git state, by the project's agent access level (DEC-102).
/// `Linked`: a worktree the project repository added, whose git folder sits in the
/// project's `.git/worktrees` and stays read-only to the agent (Sandboxed). `Clone`: the
/// agent's own clone in the same worktree folder, with its own git folder, refs and
/// branch, borrowing the project's objects read-only (Own branch and Full access).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layout {
    Linked,
    Clone,
}

/// The git folder of an agent's own clone: the one git folder the agent may write at the
/// Own branch level, while the project's `.git` stays read-only to it. Input: the agent's
/// worktree folder, `<project>/.crazyagent/worktrees/agent-<id>`. Output: the folder,
/// `<worktree>/.git`, which the engine creates as a real folder before the agent starts.
pub fn git_dir(worktree: &Path) -> PathBuf {
    worktree.join(".git")
}

/// The branch an agent commits on in its own clone, named after its worktree folder,
/// such as `agent/agent-7`, so a woken agent keeps its branch. Input: the worktree
/// folder. Output: the branch, or None for a folder name that is not plain ASCII letters,
/// digits, dashes and underscores of at most 64 bytes.
pub fn branch(worktree: &Path) -> Option<String> {
    let folder = worktree.file_name()?.to_str()?;
    let plain = !folder.is_empty()
        && folder.len() <= FOLDER_BYTES_MAX
        && folder
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'));
    plain.then(|| format!("{BRANCH_PREFIX}{folder}"))
}
