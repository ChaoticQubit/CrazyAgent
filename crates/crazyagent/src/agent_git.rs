use std::path::{Path, PathBuf};

const BRANCH_PREFIX: &str = "agent/";
pub(crate) const LINKED_PREFIX: &str = "crazyagent/";
const FOLDER_BYTES_MAX: usize = 64;
const TEAMMATE_SEPARATOR: &str = "--";
const NAME_BYTES_MAX: usize = 48;
/// The folder inside a harness's own clone that holds its teammates' worktrees, one
/// `<clone>/.worktrees/<teammate>` each, which landing leaves out of the harness's change
/// set and cleanup removes with the clone.
pub const TEAMMATES_FOLDER: &str = ".worktrees";
/// The line an agent clone's `info/exclude` keeps the project graph out of git with.
pub const GRAPH_EXCLUDE: &str = "/graphify-out/";
/// The most bytes of a native teammate's name, which names its worktree folder.
pub const WORKTREE_NAME_BYTES_MAX: usize = 64;
const CLAUDE_LEAD_NAME: &str = "team-lead";

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
    plain_folder(worktree).map(|folder| format!("{BRANCH_PREFIX}{folder}"))
}

/// The branch the engine checks out in an agent's linked worktree and commits its turns
/// on, named after the worktree folder, such as `crazyagent/agent-7`, so it shows in the
/// owner's `git branch`. Input: the worktree folder. Output: the branch, or None for a
/// folder name that is not plain ASCII letters, digits, dashes and underscores of at most
/// 64 bytes.
pub fn linked_branch(worktree: &Path) -> Option<String> {
    plain_folder(worktree).map(|folder| format!("{LINKED_PREFIX}{folder}"))
}

/// The worktree folder name when it is plain ASCII letters, digits, dashes and
/// underscores of at most 64 bytes, safe inside a branch name. Input: the worktree folder.
/// Output: the name, or None.
fn plain_folder(worktree: &Path) -> Option<&str> {
    let folder = worktree.file_name()?.to_str()?;
    let plain = !folder.is_empty()
        && folder.len() <= FOLDER_BYTES_MAX
        && folder
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'));
    plain.then_some(folder)
}

/// The task branch a harness works on, `crazyagent/<name>`, named after the delegate's
/// handle or task (DEC-100), in its own clone or, for a linked worktree, in the owner
/// repository. Input: the name, lower-case ASCII letters and digits joined by single
/// hyphens, at most 48 bytes. Output: the branch, or None for any other name.
pub fn task_branch(name: &str) -> Option<String> {
    plain_name(name).then(|| format!("{LINKED_PREFIX}{name}"))
}

/// Tells whether a stored branch is one the engine names for an agent: `crazyagent/` and a
/// plain name, as [`task_branch`] and [`linked_branch`] make, or a sub-agent's
/// `crazyagent/<task>--<name>` with both parts plain, as [`teammate_branch`] makes from a
/// task branch. Input: the branch. Output: the answer.
pub fn is_agent_branch(branch: &str) -> bool {
    branch.strip_prefix(LINKED_PREFIX).is_some_and(|name| {
        plain_name(name)
            || (!name.contains('/') && plain_folder(Path::new(name)).is_some())
            || name
                .split_once(TEAMMATE_SEPARATOR)
                .is_some_and(|(task, teammate)| plain_name(task) && plain_name(teammate))
    })
}

/// The branch one teammate of a harness works on inside the harness's clone,
/// `<task branch>--<teammate>`, so it never collides with the task branch git keeps as a
/// file (`<task branch>/<teammate>` would). Inputs: the task branch and the teammate's
/// name, any text; it is lower-cased and every run of other characters becomes one hyphen.
/// Output: the branch, or None when no letter or digit is left.
pub fn teammate_branch(task: &str, teammate: &str) -> Option<String> {
    let mut name = String::with_capacity(NAME_BYTES_MAX);
    for byte in teammate.bytes() {
        if name.len() >= NAME_BYTES_MAX {
            break;
        }
        if byte.is_ascii_alphanumeric() {
            name.push(char::from(byte.to_ascii_lowercase()));
        } else if !name.is_empty() && !name.ends_with('-') {
            name.push('-');
        }
    }
    let name = name.trim_end_matches('-');
    assert!(name.is_empty() || plain_name(name));
    (!name.is_empty()).then(|| format!("{task}{TEAMMATE_SEPARATOR}{name}"))
}

/// Tells whether a branch is one of a task branch's teammate branches,
/// `<task branch>--<name>` with a non-empty name. Inputs: the task branch and the branch.
/// Output: the answer.
pub fn is_teammate_branch(task: &str, branch: &str) -> bool {
    branch
        .strip_prefix(task)
        .and_then(|rest| rest.strip_prefix(TEAMMATE_SEPARATOR))
        .is_some_and(|name| !name.is_empty())
}

/// Tells whether a branch is the one a named native teammate works on inside a harness's
/// clone (DEC-111 Q17): `<task branch>--<name>` with the name as the teammate gives it, or
/// as [`teammate_branch`] spells it. Inputs: the task branch, the teammate's name and the
/// branch. Output: the answer.
pub fn is_teammates_own_branch(task: &str, teammate: &str, branch: &str) -> bool {
    branch
        .strip_prefix(task)
        .and_then(|rest| rest.strip_prefix(TEAMMATE_SEPARATOR))
        == Some(teammate)
        || teammate_branch(task, teammate).as_deref() == Some(branch)
}

/// Says whether a native teammate's name can name its worktree folder under
/// `TEAMMATES_FOLDER`: 1 to `WORKTREE_NAME_BYTES_MAX` bytes of letters, digits, `-`, `_`
/// and `.`, not starting with a dot, and not the name Claude Code gives the lead. Input:
/// the name. Output: the answer.
pub fn worktree_name_valid(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= WORKTREE_NAME_BYTES_MAX
        && !name.starts_with('.')
        && name != CLAUDE_LEAD_NAME
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

/// Tells whether a name is lower-case ASCII letters and digits joined by single hyphens,
/// at most `NAME_BYTES_MAX` bytes, safe in a branch name and never holding the teammate
/// separator. Input: the name. Output: the answer.
fn plain_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= NAME_BYTES_MAX
        && !name.starts_with('-')
        && !name.ends_with('-')
        && !name.contains(TEAMMATE_SEPARATOR)
        && name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

#[cfg(test)]
mod tests;

/// What the last line of one ignore file about the graph folder says, read as git reads it
/// with only trailing spaces trimmed (T3-L1): ignored, for a line that is one of the usual
/// spellings, or taken back into git, for any `!` line naming the folder in any pattern.
/// Input: the text. Output: the answer, none when no line is about the folder.
pub fn graph_rule(text: &str) -> Option<bool> {
    text.lines()
        .map(|line| line.trim_end_matches(' '))
        .filter_map(|line| match line.strip_prefix('!') {
            Some(pattern) => pattern.contains("graphify-out").then_some(false),
            None => matches!(
                line,
                "graphify-out" | "graphify-out/" | "/graphify-out" | "/graphify-out/"
            )
            .then_some(true),
        })
        .next_back()
}
