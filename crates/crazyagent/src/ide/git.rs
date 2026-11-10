use crate::engine::EngineError;
use crate::engine::owner::failure;
use serde_json::{Map, Value};
use std::ffi::OsString;
use std::path::Path;

#[cfg(all(test, unix))]
mod tests;

const KEYS: usize = 14;

/// The git configuration every git the IDE server starts runs with, as `GIT_CONFIG_COUNT`
/// and numbered `GIT_CONFIG_KEY_n` and `GIT_CONFIG_VALUE_n` variables, which outrank every
/// config file (INT-1). `diff.ignoreSubmodules=all` keeps a status or diff in a section out
/// of a repository an agent planted below its worktree; `safe.bareRepository=explicit`
/// refuses a folder an agent made look like a bare repository; and every key that names a
/// program git runs and that git lets the environment force is forced off: fsmonitor, the
/// ssh command, the pager, credential helpers, the signing programs and, last so the shim
/// can leave it out for the project folder alone ([`shim_text`]), the hooks folder. A gc
/// never prunes a worktree, since the IDE cannot read an agent worktree's `.git` and so
/// sees it as missing (IN1-FN1), and every signing switch is off, so a project that signs
/// every commit or tag still commits and tags from the IDE, unsigned (INT1-3). The
/// server's environment adds them after its allowlist.
pub const CONFIG_ENV: [(&str, &str); 1 + 2 * KEYS] = [
    ("GIT_CONFIG_COUNT", "14"),
    ("GIT_CONFIG_KEY_0", "diff.ignoreSubmodules"),
    ("GIT_CONFIG_VALUE_0", "all"),
    ("GIT_CONFIG_KEY_1", "safe.bareRepository"),
    ("GIT_CONFIG_VALUE_1", "explicit"),
    ("GIT_CONFIG_KEY_2", "core.fsmonitor"),
    ("GIT_CONFIG_VALUE_2", "false"),
    ("GIT_CONFIG_KEY_3", "core.sshCommand"),
    ("GIT_CONFIG_VALUE_3", "false"),
    ("GIT_CONFIG_KEY_4", "core.pager"),
    ("GIT_CONFIG_VALUE_4", "cat"),
    ("GIT_CONFIG_KEY_5", "credential.helper"),
    ("GIT_CONFIG_VALUE_5", ""),
    ("GIT_CONFIG_KEY_6", "gpg.program"),
    ("GIT_CONFIG_VALUE_6", "false"),
    ("GIT_CONFIG_KEY_7", "gpg.ssh.program"),
    ("GIT_CONFIG_VALUE_7", "false"),
    ("GIT_CONFIG_KEY_8", "gpg.x509.program"),
    ("GIT_CONFIG_VALUE_8", "false"),
    ("GIT_CONFIG_KEY_9", "gc.worktreePruneExpire"),
    ("GIT_CONFIG_VALUE_9", "never"),
    ("GIT_CONFIG_KEY_10", "commit.gpgSign"),
    ("GIT_CONFIG_VALUE_10", "false"),
    ("GIT_CONFIG_KEY_11", "tag.gpgSign"),
    ("GIT_CONFIG_VALUE_11", "false"),
    ("GIT_CONFIG_KEY_12", "tag.forceSignAnnotated"),
    ("GIT_CONFIG_VALUE_12", "false"),
    ("GIT_CONFIG_KEY_13", "core.hooksPath"),
    ("GIT_CONFIG_VALUE_13", "/dev/null"),
];

/// The `GIT_CONFIG_COUNT` the shim sets for the project folder when the owner lets the IDE
/// run the project's hooks: every [`CONFIG_ENV`] key but the last, `core.hooksPath`.
const PROJECT_HOOKS_COUNT: &str = "13";

/// The variable that ends git's search for a repository at the project's agents root
/// (INT1-1): git in an agent folder, whose `.git` the IDE sandbox cannot read, then finds no
/// repository instead of walking up to the project's and acting on it. The server's
/// environment adds it.
pub const CEILING_VARIABLE: &str = "GIT_CEILING_DIRECTORIES";

/// The [`CEILING_VARIABLE`] value for one project: the agents root its layout uses
/// (INT1-R2-1), `<root>/.crazyagent/worktrees` when the project keeps its state in
/// `.crazyagent` and `<root>/worktrees` otherwise, as [`crate::projects_home::state_folder`]
/// decides for the agents' exclude line. When an engine-made worktree of the project, a
/// team agent's or a research job's, is still in `<root>/worktrees` after the project gained
/// a `.crazyagent` folder, that older root follows the layout's (INT1-R3-1, INT1R3-L1). Git
/// never searches from a listed root upwards; the project folder and the owner's own
/// folders, a `.crazyagent` project's top-level `worktrees` among them while no engine-made
/// worktree is there, keep their repository. Inputs:
/// the canonical project folder and whether an engine-made worktree is in its older
/// agents root ([`super::workspace::older_agents`]). Output: the value, or
/// `ide_project_folder_invalid` for a folder that is not absolute or holds a `:`, which git
/// reads as a list separator.
pub fn ceiling(root: &Path, older_agents: bool) -> Result<OsString, EngineError> {
    if !root.is_absolute() || root.as_os_str().as_encoded_bytes().contains(&b':') {
        return Err(failure("ide_project_folder_invalid"));
    }
    let layout = crate::projects_home::state_folder(root).join("worktrees");
    let older = root.join("worktrees");
    let both = older_agents && older != layout;
    let mut value = layout.clone().into_os_string();
    if both {
        value.push(":");
        value.push(&older);
    }
    assert!(layout.starts_with(root) && older.starts_with(root));
    let separators = value
        .as_encoded_bytes()
        .iter()
        .filter(|byte| **byte == b':')
        .count();
    assert_eq!(separators, usize::from(both));
    Ok(value)
}

/// The file in a project's IDE folder that every git of its IDE server reads as its user-level
/// config, written by the engine at each launch.
pub const GLOBAL_CONFIG: &str = "gitconfig";

#[cfg(unix)]
const GLOBAL_TEMPORARY: &str = "gitconfig.tmp";

/// The variables that keep a git of the IDE server off every config its sandbox denies: the
/// user-level config named by `config`, and no system config. Git dies on a denied config and
/// only skips a missing one. A Full access server keeps the owner's own config and gets none
/// of these ([`super::env::ServerGit`]). Input: the config's absolute path. Output: the variables.
pub fn global_env(config: &Path) -> [(OsString, OsString); 2] {
    assert!(config.is_absolute());
    let (name, value) = crate::sandbox::AGENT_GIT_ENV[0];
    assert_eq!((name, value), ("GIT_CONFIG_NOSYSTEM", "1"));
    [
        (
            OsString::from("GIT_CONFIG_GLOBAL"),
            config.as_os_str().to_owned(),
        ),
        (OsString::from(name), OsString::from(value)),
    ]
}

/// The text of the user-level git config an IDE git reads: the per-user ignore and attributes
/// files set to nothing, so git never warns on the denied defaults in the owner's home, then
/// the owner's name and email when git has them. Input: the name and email, each free of
/// control characters. Output: the text.
#[cfg(unix)]
fn global_text(identity: Option<&(String, String)>) -> String {
    let mut text =
        String::from("[core]\n\texcludesFile = /dev/null\n\tattributesFile = /dev/null\n");
    if let Some((name, email)) = identity {
        let (name, email) = (quoted(name), quoted(email));
        text.push_str(&format!("[user]\n\tname = {name}\n\temail = {email}\n"));
    }
    assert!(text.ends_with('\n'));
    assert!(text.starts_with("[core]\n"));
    text
}

/// A value as git config reads it back whole: in quotes, with its backslashes and quotes
/// escaped. Input: a value free of control characters. Output: the quoted value.
#[cfg(unix)]
fn quoted(value: &str) -> String {
    assert!(!value.chars().any(char::is_control));
    let text = format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""));
    assert!(text.len() >= value.len() + 2);
    text
}

/// Writes a project's [`GLOBAL_CONFIG`] whole, replacing what is there and never following a
/// link, with the owner's name and email as git reads them for the repository, so an IDE
/// commit carries them; git keeps its own default when it has none. Inputs: the project's IDE
/// folder, its repository and the owner's home, if known. Output: Ok, or
/// `ide_settings_unwritable`.
#[cfg(unix)]
pub fn write_global(
    folder: &Path,
    repository: &Path,
    owner_home: Option<&Path>,
) -> Result<(), EngineError> {
    let identity =
        owner_home.and_then(|home| crate::landing::owner_git_identity(repository, home).ok());
    let text = global_text(identity.as_ref());
    let target = folder.join(GLOBAL_CONFIG);
    assert!(target.is_absolute());
    super::settings::replace(folder, GLOBAL_TEMPORARY, &target, text.as_bytes(), 0o600)
}

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
/// engine as `ide.git` (DEC-098 addendum 13); before the extension starts, and in a terminal,
/// which never has the forwarder, they fail with a plain message. `worktree prune` and `worktree remove` are refused, since the IDE sees an
/// agent worktree as missing and would drop it (IN1-FN1). Every other command runs the
/// `git` on the server's PATH, as stock VS Code does, where the engine resolved no real git
/// ([`shim_text`]), without the relay's secret, so a project's hook under it does not hold it.
pub const SHIM_TEXT: &str = "#!/bin/sh\n\
case \"$1\" in\n\
push|pull|fetch|ls-remote)\n  \
if [ -n \"$CRAZYAGENT_IDE_GIT_NODE\" ] && [ -n \"$CRAZYAGENT_IDE_GIT_FORWARD\" ]; then\n    \
exec \"$CRAZYAGENT_IDE_GIT_NODE\" \"$CRAZYAGENT_IDE_GIT_FORWARD\" \"$@\"\n  \
fi\n  \
echo \"fatal: CrazyAgent runs git $1 in its engine. Use Source Control in the IDE once it has connected\" >&2\n  \
exit 128\n  \
;;\n\
worktree)\n  \
case \"$2\" in\n  \
prune|remove)\n    \
echo \"fatal: CrazyAgent keeps its agents' worktrees, so the IDE does not run git worktree $2\" >&2\n    \
exit 128\n    \
;;\n  \
esac\n  \
;;\n\
esac\n\
unset CRAZYAGENT_IDE_GIT_SECRET\n\
exec git \"$@\"\n";

const SHIM_EXEC: &str = "exec git \"$@\"\n";

/// The shim for one IDE server start: [`SHIM_TEXT`] whose last line runs the real git the
/// engine resolved, so the IDE never runs the `/usr/bin/git` xcrun shim, whose cache write
/// its profile denies (FZ-7). For `/usr/bin/git` itself, or a path holding a quote or a
/// control character, it keeps the PATH's git. On, with the project folder the owner let run
/// its own hooks, it first sets `GIT_CONFIG_COUNT` to [`PROJECT_HOOKS_COUNT`] when its
/// physical working directory is exactly that folder, so git there runs the project's hooks
/// and every other [`CONFIG_ENV`] key stays forced; an agent folder below it is never that
/// folder. Inputs: the canonical project folder, or None for Off, and the absolute git
/// ([`crate::research::runner::sandboxed_git`]). Output: the text, or `ide_settings_invalid`
/// for a folder that is not absolute, not UTF-8 or holds a quote or a control character.
pub fn shim_text(project_hooks: Option<&Path>, git: &Path) -> Result<String, EngineError> {
    assert_eq!(CONFIG_ENV[0].1.parse::<usize>(), Ok(KEYS));
    assert_eq!(PROJECT_HOOKS_COUNT.parse::<usize>(), Ok(KEYS - 1));
    assert_eq!(
        CONFIG_ENV[2 * KEYS - 1],
        ("GIT_CONFIG_KEY_13", "core.hooksPath")
    );
    assert!(
        git.is_absolute(),
        "the engine resolves git to an absolute path"
    );
    let exec = git
        .to_str()
        .filter(|text| {
            *text != crate::research::runner::GIT
                && !text.chars().any(|c| c.is_control() || c == '\'')
        })
        .map_or_else(
            || SHIM_EXEC.to_owned(),
            |text| format!("exec '{text}' \"$@\"\n"),
        );
    let head = SHIM_TEXT
        .strip_suffix(SHIM_EXEC)
        .expect("the shim ends by running git");
    let Some(project) = project_hooks else {
        return Ok(format!("{head}{exec}"));
    };
    let folder = project
        .to_str()
        .filter(|text| project.is_absolute() && !text.chars().any(|c| c.is_control() || c == '\''))
        .ok_or_else(|| failure("ide_settings_invalid"))?;
    let text = format!(
        "{head}if [ \"$(pwd -P)\" = '{folder}' ]; then\n  GIT_CONFIG_COUNT={PROJECT_HOOKS_COUNT}\n  export GIT_CONFIG_COUNT\nfi\n{exec}"
    );
    assert!(text.starts_with("#!/bin/sh\n") && text.ends_with(&exec));
    Ok(text)
}

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
