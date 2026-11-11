use crate::sandbox::policy::AgentAccess;
use std::ffi::{OsStr, OsString};
use std::path::Path;

/// The variable that hands the IDE server's extension host the path of its engine
/// capability file. Only the path travels in the environment, never the capability.
pub const CAPABILITY_VARIABLE: &str = super::bridge::CAPABILITY_ENV;

/// The variable that hands the IDE server's extension host the owner socket path its
/// engine bridge connects to; without it the built-in extension starts none of its features.
pub const SOCKET_VARIABLE: &str = super::bridge::SOCKET_ENV;

/// The variable that tells the IDE server's extension host the project's access level, for
/// the terminal menu to grey the levels above it. The engine decides what a terminal may do
/// from the level it read itself, so this only shapes the menu.
pub const ACCESS_VARIABLE: &str = "CRAZYAGENT_IDE_ACCESS";

/// Every variable the IDE server may inherit from the engine, besides the `LC_` locale
/// variables: what an owner shell needs, and nothing a provider, pool or engine secret or
/// the owner's credentials travel in. The owner's key agent socket is refused, as it is for
/// agents.
pub const ALLOWED: &[&str] = &[
    "PATH", "HOME", "USER", "LOGNAME", "SHELL", "LANG", "TERM", "TMPDIR",
];

const LOCALE_PREFIX: &str = "LC_";
const LOCALE_NAME_BYTES_MAX: usize = 32;
const LOCALES_MAX: usize = 16;
const VALUE_BYTES_MAX: usize = 16 * 1024;

/// The most variables the IDE server's environment holds.
pub const ENV_MAX: usize = ALLOWED.len() + LOCALES_MAX + 7 + super::git::CONFIG_ENV.len();

/// Tells whether the IDE server may inherit a variable of this name: one of [`ALLOWED`], or
/// `LC_` followed by capital letters. Input: the name. Output: the answer.
pub fn allowed(name: &OsStr) -> bool {
    let Some(name) = name.to_str() else {
        return false;
    };
    ALLOWED.contains(&name)
        || name.strip_prefix(LOCALE_PREFIX).is_some_and(|rest| {
            !rest.is_empty()
                && name.len() <= LOCALE_NAME_BYTES_MAX
                && rest.bytes().all(|byte| byte.is_ascii_uppercase())
        })
}

/// The git settings a confined IDE server starts with: git's search ceiling
/// ([`super::git::ceiling`]) and the engine-written user-level config it reads. A Full access
/// server has none and uses the owner's own git setup.
#[derive(Debug, Clone, Copy)]
pub struct ServerGit<'a> {
    pub ceiling: &'a OsStr,
    pub config: &'a Path,
}

/// The word the extension reads for a level. Input: the level. Output: the word.
fn access_name(access: AgentAccess) -> &'static str {
    match access {
        AgentAccess::Sandboxed => "sandboxed",
        AgentAccess::OwnBranch => "own_branch",
        AgentAccess::Full => "full",
    }
}

/// Builds the IDE server's whole environment from the engine's own: only the variables
/// [`allowed`] passes, each value at most 16 KiB, at most 16 locale variables, then
/// [`CAPABILITY_VARIABLE`] naming the capability file, [`SOCKET_VARIABLE`] naming the owner
/// socket, the AI lock's [`super::settings::LOCKED_VARIABLE`] naming the locked-keys file and
/// [`ACCESS_VARIABLE`] naming the project's level, then with `git` git's search ceiling [`super::git::CEILING_VARIABLE`], the IDE git
/// configuration [`super::git::CONFIG_ENV`] and the config path of
/// [`super::git::global_env`]; none is ever taken from the engine's own variables. The server
/// starts from a cleared environment holding exactly this. Inputs: the engine's variables,
/// such as `std::env::vars_os()`, the capability file path, the owner socket path, the
/// locked-keys file path, the project's level and the [`ServerGit`], or None for a Full access
/// server. Output: the variables in order.
pub fn server_env(
    source: impl IntoIterator<Item = (OsString, OsString)>,
    capability: &Path,
    socket: &Path,
    locked: &Path,
    access: AgentAccess,
    git: Option<ServerGit<'_>>,
) -> Vec<(OsString, OsString)> {
    let mut env: Vec<(OsString, OsString)> = Vec::new();
    let mut locales = 0;
    for (name, value) in source {
        if !allowed(&name)
            || value.len() > VALUE_BYTES_MAX
            || env.iter().any(|(known, _)| *known == name)
        {
            continue;
        }
        if !ALLOWED.contains(&name.to_str().unwrap_or_default()) {
            if locales == LOCALES_MAX {
                continue;
            }
            locales += 1;
        }
        env.push((name, value));
    }
    env.push((
        OsString::from(CAPABILITY_VARIABLE),
        capability.as_os_str().to_owned(),
    ));
    env.push((
        OsString::from(SOCKET_VARIABLE),
        socket.as_os_str().to_owned(),
    ));
    env.push((
        OsString::from(super::settings::LOCKED_VARIABLE),
        locked.as_os_str().to_owned(),
    ));
    env.push((
        OsString::from(ACCESS_VARIABLE),
        OsString::from(access_name(access)),
    ));
    let global = git.map(|git| super::git::global_env(git.config));
    if let (Some(git), Some(global)) = (git, &global) {
        env.push((
            OsString::from(super::git::CEILING_VARIABLE),
            git.ceiling.to_owned(),
        ));
        env.extend(
            super::git::CONFIG_ENV
                .iter()
                .map(|(name, value)| (OsString::from(name), OsString::from(value))),
        );
        env.extend(global.iter().cloned());
    }
    assert!(env.len() <= ENV_MAX);
    assert_eq!(
        git.is_some(),
        env.iter()
            .any(|(name, _)| name == super::git::CEILING_VARIABLE)
    );
    assert!(env.iter().all(|(name, _)| {
        allowed(name)
            || name == CAPABILITY_VARIABLE
            || name == SOCKET_VARIABLE
            || name == ACCESS_VARIABLE
            || name == super::settings::LOCKED_VARIABLE
            || name == super::git::CEILING_VARIABLE
            || global.iter().flatten().any(|(fixed, _)| name == fixed)
            || super::git::CONFIG_ENV
                .iter()
                .any(|(fixed, _)| name == *fixed)
    }));
    env
}
