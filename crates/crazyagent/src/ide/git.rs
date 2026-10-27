use serde_json::{Map, Value};

#[cfg(all(test, unix))]
mod tests;

/// The git configuration every git the IDE server starts runs with, as the
/// `GIT_CONFIG_COUNT`, `GIT_CONFIG_KEY_0` and `GIT_CONFIG_VALUE_0` variables, which outrank
/// every config file: `diff.ignoreSubmodules=all`, so a status or diff in a section never
/// enters a repository an agent planted below its worktree and never runs that
/// repository's fsmonitor or hooks. The server's environment adds them after its allowlist.
pub const CONFIG_ENV: [(&str, &str); 3] = [
    ("GIT_CONFIG_COUNT", "1"),
    ("GIT_CONFIG_KEY_0", "diff.ignoreSubmodules"),
    ("GIT_CONFIG_VALUE_0", "all"),
];

/// The workspace keys that keep the IDE's git to the repository at each section's root: no
/// submodule detection, and no scan for repositories below a section or around an open
/// file. Output: the keys and their values, for the generated workspace file.
pub fn workspace() -> Map<String, Value> {
    Map::from_iter([
        ("git.detectSubmodules".to_owned(), Value::Bool(false)),
        ("git.autoRepositoryDetection".to_owned(), Value::Bool(false)),
    ])
}
