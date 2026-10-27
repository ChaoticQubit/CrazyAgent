use crate::engine::EngineError;
use crate::engine::owner::failure;
use serde_json::{Map, Value};
use std::path::Path;

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

/// The program VS Code's git runs as `git` in the IDE server, written by the engine into
/// the server's locked Remote settings folder before each start
/// ([`super::settings::write`]).
pub const SHIM: &str = "git";

/// The shim's text. The IDE sandbox has no network, so `push`, `pull`, `fetch` and
/// `ls-remote` go to the built-in extension's forwarder, which the extension names in
/// `CRAZYAGENT_IDE_GIT_NODE` and `CRAZYAGENT_IDE_GIT_FORWARD` and which hands them to the
/// engine as `ide.git` (DEC-098 addendum 13); before the extension starts they fail with a
/// plain message. Every other command runs the `git` on the server's PATH, as stock VS Code
/// does.
pub const SHIM_TEXT: &str = "#!/bin/sh\n\
case \"$1\" in\n\
push|pull|fetch|ls-remote)\n  \
if [ -n \"$CRAZYAGENT_IDE_GIT_NODE\" ] && [ -n \"$CRAZYAGENT_IDE_GIT_FORWARD\" ]; then\n    \
exec \"$CRAZYAGENT_IDE_GIT_NODE\" \"$CRAZYAGENT_IDE_GIT_FORWARD\" \"$@\"\n  \
fi\n  \
echo \"fatal: CrazyAgent runs git $1 in its engine, and the IDE has not connected to it yet\" >&2\n  \
exit 128\n  \
;;\n\
esac\n\
exec git \"$@\"\n";

/// The Remote settings key that points VS Code's git at the shim. `git.path` is a machine
/// setting, so no workspace or folder setting can point it elsewhere. Input: the shim's
/// path. Output: the key and its value, or `ide_settings_invalid` for a path that is not
/// UTF-8.
pub fn machine_settings(shim: &Path) -> Result<Map<String, Value>, EngineError> {
    let path = shim
        .to_str()
        .ok_or_else(|| failure("ide_settings_invalid"))?;
    assert!(shim.is_absolute() && shim.ends_with(SHIM));
    Ok(Map::from_iter([(
        "git.path".to_owned(),
        Value::String(path.to_owned()),
    )]))
}
