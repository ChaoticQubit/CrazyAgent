use crate::error::{SandboxError, invalid};
#[cfg(unix)]
use crate::paths::clean_absolute;
#[cfg(unix)]
use crate::profile::command::{SESSION_SHIM, confined_command};
use crate::profile::consts::*;
use kernel_common::env_names::{REFUSED_PREFIXES, REFUSED_VARIABLES, agent_env_refused};
use serde::{Deserialize, Serialize};
use std::ffi::{OsStr, OsString};
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The environment variable that carries a launch's [`LineageToken`] to every process it
/// starts.
pub const LINEAGE_VARIABLE: &str = "CRAZYAGENT_LINEAGE";
const TOKEN_BYTES: usize = 16;

/// A random value every process of one launch inherits in [`LINEAGE_VARIABLE`], so the launch's
/// stop finds a process that left its session and lost its parent while it kept its environment.
/// Not a secret: any process of the owner's user can read it, so it only lets a same-user process
/// get itself named or, under End them, ended; an agent only ever learns its own launch's token.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct LineageToken(String);

impl LineageToken {
    /// Draws a new token. Output: the token, or none when the system gives no randomness.
    pub fn new() -> Option<Self> {
        let mut bytes = [0u8; TOKEN_BYTES];
        getrandom::fill(&mut bytes).ok()?;
        let token: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
        assert!(Self::valid(&token));
        Some(Self(token))
    }

    /// Tells whether `text` has a token's shape: `TOKEN_BYTES` bytes as lowercase hex.
    /// Input: the text. Output: true only then.
    pub fn valid(text: &str) -> bool {
        text.len() == TOKEN_BYTES * 2
            && text
                .bytes()
                .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
    }

    /// The token as its variable's value. Output: the hex text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for LineageToken {
    type Error = &'static str;

    fn try_from(text: String) -> Result<Self, Self::Error> {
        if Self::valid(&text) {
            Ok(Self(text))
        } else {
            Err("lineage_token_invalid")
        }
    }
}

impl From<LineageToken> for String {
    fn from(token: LineageToken) -> Self {
        token.0
    }
}

/// The variables that point an agent's tools at its tool cache: uv's cache, and the pnpm store, the
/// only store variable pnpm 12 reads. [`agent_process_env`] keeps a harness from replacing either
/// once the sandbox set it.
pub const TOOL_CACHE_ENV: [&str; 2] = ["UV_CACHE_DIR", "pnpm_config_store_dir"];

/// Tells whether one variable may reach an agent process with this value: a name
/// [`agent_env_refused`] accepts, a fixed [`AGENT_GIT_ENV`] pair, a model key name holding the
/// placeholder or the lineage token in its own variable; never a real key or relay token, which
/// other owner processes can read, save a launch's own token in [`Handed`] names on macOS and
/// Linux, where [`Handed::new`] states what keeps those reads out; elsewhere it is refused.
/// Inputs: name, value. Output: allowed.
pub fn agent_variable_allowed(name: &OsStr, value: &OsStr) -> bool {
    let token = kernel_common::relay_names::TOKEN_PREFIX.as_bytes();
    if value
        .as_encoded_bytes()
        .windows(token.len())
        .any(|window| window == token)
    {
        return false;
    }
    if name == LINEAGE_VARIABLE {
        return value.to_str().is_some_and(LineageToken::valid);
    }
    if AGENT_GIT_ENV
        .iter()
        .chain(AGENT_HOME_ENV.iter())
        .any(|(git, fixed)| name == *git && value == *fixed)
    {
        return true;
    }
    if name
        .to_str()
        .is_some_and(|name| AGENT_KEY_VARIABLES.contains(&name))
    {
        return value == kernel_common::relay_names::LOCAL_PLACEHOLDER;
    }
    !agent_env_refused(name)
}

/// The zsh temp-file prefix inside an agent's temp folder. zsh writes here-documents to
/// `$TMPPREFIX`, `/tmp/zsh` by default, which no agent sandbox can write, so every agent
/// shell gets its own. Input: the agent's temp folder. Output: the prefix path.
pub fn shell_tmp_prefix(tmp: &Path) -> PathBuf {
    tmp.join("zsh")
}

/// Builds the environment of every agent process in launch order: `base`, then the
/// harness's variables (a later value replaces an earlier one and None removes it, but
/// `HOME`, `TMPDIR` and `TMPPREFIX`, and the tool cache's [`TOOL_CACHE_ENV`] when `base`
/// sets them, always stay as `base` set them), then `TMPPREFIX` from
/// [`shell_tmp_prefix`] when `TMPDIR` is set, then [`AGENT_GIT_ENV`], [`AGENT_TLS_ENV`] and
/// [`AGENT_HOME_ENV`].
/// Inputs: the base and harness variables. Output: the variables, or `Invalid` when any
/// one fails [`agent_variable_allowed`].
pub fn agent_process_env<'a>(
    base: &[(&str, &OsStr)],
    harness: impl IntoIterator<Item = (&'a OsStr, Option<&'a OsStr>)>,
) -> Result<Vec<(OsString, OsString)>, SandboxError> {
    built_env(base, harness, &AGENT_HOME_ENV, None)
}

/// One launch's exception to the agent variable guard: the names a listed master's manifest gives
/// its account key, each allowed to hold only this launch's own relay token or the keyless
/// placeholder.
#[derive(Clone, Copy)]
pub struct Handed<'a> {
    names: &'a [&'a str],
    value: &'a str,
}

impl<'a> Handed<'a> {
    /// Builds one launch's exception, on the systems whose sandboxes keep every other agent
    /// from reading the launch's environment: on macOS the agent profile denies those reads;
    /// on Linux the bubblewrap and nested backends start each agent in its own process
    /// namespace with a fresh `/proc`, and under the Landlock backend, with no namespace,
    /// Landlock's ptrace scope keeps sibling domains from each other's `/proc/<pid>/environ`.
    /// Inputs: the names, from the manifest's accounts entry for the account's wire format,
    /// and the one value they may hold. Output: the exception, or `Invalid` for a value that
    /// is neither a relay token nor the keyless placeholder, a name refused for more than a
    /// key suffix, and off macOS and Linux.
    pub fn new(names: &'a [&'a str], value: &'a str) -> Result<Self, SandboxError> {
        let handed = kernel_common::relay_names::is_relay_token(value)
            || value == kernel_common::relay_names::LOCAL_PLACEHOLDER;
        if !cfg!(any(target_os = "macos", target_os = "linux")) || !handed {
            return Err(invalid("a launch hands only its own relay token"));
        }
        let other = |name: &&str| {
            let upper = name.to_ascii_uppercase();
            REFUSED_VARIABLES.contains(&upper.as_str())
                || REFUSED_PREFIXES
                    .iter()
                    .any(|prefix| upper.starts_with(prefix))
        };
        if names.iter().any(other) {
            return Err(invalid("a launch hands its value only under a key name"));
        }
        assert!(kernel_common::relay_names::is_placeholder(value));
        Ok(Self { names, value })
    }

    /// Tells whether this launch lets one harness variable hold a value: a name it lists
    /// holding exactly its one value. Inputs: the name and the value. Output: true when it
    /// does.
    fn allows(&self, name: &OsStr, value: &OsStr) -> bool {
        value == self.value && self.names.iter().any(|known| name == *known)
    }
}

/// Builds the environment [`agent_process_env`] does, letting the harness variables `handed` names
/// hold its one value; every other variable meets [`agent_variable_allowed`], and the
/// engine's fixed variables still come last and win. Inputs: the base and harness variables
/// and the launch's exception. Output: the variables, or `Invalid`.
pub fn agent_process_env_handing<'a>(
    base: &[(&str, &OsStr)],
    harness: impl IntoIterator<Item = (&'a OsStr, Option<&'a OsStr>)>,
    handed: Handed<'_>,
) -> Result<Vec<(OsString, OsString)>, SandboxError> {
    built_env(base, harness, &AGENT_HOME_ENV, Some(handed))
}

/// Tells whether one variable may reach an agent process of a launch with this exception:
/// [`agent_variable_allowed`], or a name the exception lists holding its one value. Inputs:
/// the launch's exception, if any, the name and the value. Output: true when allowed.
fn handed_allowed(handed: Option<Handed<'_>>, name: &OsStr, value: &OsStr) -> bool {
    agent_variable_allowed(name, value) || handed.is_some_and(|handed| handed.allows(name, value))
}

/// Builds the environment of the owner's confined IDE terminal as [`agent_process_env`] does for an
/// agent, but without [`AGENT_HOME_ENV`]: its home is the owner's own, kept across terminals, so
/// the global git config, Python user site and npm config the owner sets up there stay in use, and
/// its profile leaves their files writable ([`ProjectSandbox::keep_owner_config`]). Inputs: the
/// base and terminal variables. Output: the variables, or `Invalid` when any one fails
/// [`agent_variable_allowed`].
pub fn owner_terminal_env<'a>(
    base: &[(&str, &OsStr)],
    terminal: impl IntoIterator<Item = (&'a OsStr, Option<&'a OsStr>)>,
) -> Result<Vec<(OsString, OsString)>, SandboxError> {
    built_env(base, terminal, &[], None)
}

/// Whether two variable names name one variable: on Windows, which reads names in any
/// case, equal ignoring ASCII case; elsewhere equal. Inputs: the names. Output: the answer.
fn same_name(one: &OsStr, other: &OsStr) -> bool {
    if cfg!(windows) {
        one.eq_ignore_ascii_case(other)
    } else {
        one == other
    }
}

/// Builds the environment [`agent_process_env`] describes, setting `home` after the fixed git and
/// TLS settings. Inputs: the base and harness variables, the home settings and the launch's
/// exception, if any. Output: the variables, or `Invalid` when any one fails
/// [`agent_variable_allowed`] and is no variable the exception lets through.
fn built_env<'a>(
    base: &[(&str, &OsStr)],
    harness: impl IntoIterator<Item = (&'a OsStr, Option<&'a OsStr>)>,
    home: &[(&'static str, &'static str)],
    handed: Option<Handed<'_>>,
) -> Result<Vec<(OsString, OsString)>, SandboxError> {
    let refused = || invalid("agent environment holds an owner secret");
    let mut env: Vec<(OsString, OsString)> = Vec::new();
    let set = |env: &mut Vec<(OsString, OsString)>, name: &OsStr, value: Option<&OsStr>| {
        let at = env.iter().position(|(known, _)| same_name(known, name));
        match (at, value) {
            (Some(at), Some(value)) => env[at].1 = value.to_owned(),
            (Some(at), None) => {
                env.remove(at);
            }
            (None, Some(value)) => env.push((name.to_owned(), value.to_owned())),
            (None, None) => {}
        }
    };
    for (name, value) in base {
        if !agent_variable_allowed(OsStr::new(name), value) {
            return Err(refused());
        }
        set(&mut env, OsStr::new(name), Some(value));
    }
    for (name, value) in harness {
        if value.is_some_and(|value| !handed_allowed(handed, name, value)) {
            return Err(refused());
        }
        let pinned_cache = TOOL_CACHE_ENV
            .iter()
            .any(|pinned| same_name(name, OsStr::new(pinned)))
            && base
                .iter()
                .any(|(known, _)| same_name(name, OsStr::new(known)));
        let pinned = ["HOME", "TMPDIR", "TMPPREFIX"]
            .iter()
            .any(|fixed| same_name(name, OsStr::new(fixed)));
        if pinned || pinned_cache {
            continue;
        }
        set(&mut env, name, value);
    }
    let prefix = env
        .iter()
        .find(|(name, _)| same_name(name, OsStr::new("TMPDIR")))
        .map(|(_, tmp)| shell_tmp_prefix(Path::new(tmp)));
    if let Some(prefix) = prefix {
        set(&mut env, OsStr::new("TMPPREFIX"), Some(prefix.as_os_str()));
    }
    let fixed = AGENT_GIT_ENV
        .into_iter()
        .chain(AGENT_TLS_ENV)
        .chain(home.iter().copied());
    for (name, value) in fixed {
        set(&mut env, OsStr::new(name), Some(OsStr::new(value)));
    }
    if env.len() > AGENT_ENV_MAX {
        return Err(invalid("agent environment holds too many variables"));
    }
    assert!(
        env.iter()
            .all(|(name, value)| handed_allowed(handed, name, value))
    );
    Ok(env)
}

/// Builds the one command every sandboxed headless agent process starts from: `-f
/// <profile>`, the [`SESSION_SHIM`] and `<program>` under `sandbox-exec`, a cleared
/// environment holding exactly [`agent_process_env`], and its own process group. Inputs: the
/// installed profile, the absolute program, the base variables and the harness's variables
/// (None removes one). Output: the command, or `Invalid` when a variable fails
/// [`agent_variable_allowed`].
#[cfg(unix)]
pub fn agent_command<'a>(
    profile: &Path,
    program: &Path,
    base: &[(&str, &OsStr)],
    harness: impl IntoIterator<Item = (&'a OsStr, Option<&'a OsStr>)>,
) -> Result<Command, SandboxError> {
    let env = agent_process_env(base, harness)?;
    Ok(confined_command(profile, program, env))
}

/// Builds the command [`agent_command`] does, with the environment [`agent_process_env_handing`]
/// builds for one listed master's launch. Inputs: the installed profile, the absolute
/// program, the base and harness variables and the launch's exception. Output: the command,
/// or `Invalid` when a variable is refused.
#[cfg(unix)]
pub fn agent_command_handing<'a>(
    profile: &Path,
    program: &Path,
    base: &[(&str, &OsStr)],
    harness: impl IntoIterator<Item = (&'a OsStr, Option<&'a OsStr>)>,
    handed: Handed<'_>,
) -> Result<Command, SandboxError> {
    let env = agent_process_env_handing(base, harness, handed)?;
    Ok(confined_command(profile, program, env))
}

/// Builds the command a Full access agent process starts from: the [`SESSION_SHIM`] and `<program>`
/// with no `sandbox-exec` and no profile, so the agent has the owner's user permissions, yet the
/// same cleared environment holding exactly [`agent_process_env`] and its own process group as a
/// sandboxed agent. Inputs: the absolute program, the base variables and the harness's variables
/// (None removes one). Output: the command, or `Invalid` when a variable fails
/// [`agent_variable_allowed`].
#[cfg(unix)]
pub fn unconfined_command<'a>(
    program: &Path,
    base: &[(&str, &OsStr)],
    harness: impl IntoIterator<Item = (&'a OsStr, Option<&'a OsStr>)>,
) -> Result<Command, SandboxError> {
    if !clean_absolute(program) {
        return Err(invalid("agent program must be an absolute plain path"));
    }
    let env = agent_process_env(base, harness)?;
    let mut command = Command::new(SESSION_SHIM[0]);
    command
        .args(&SESSION_SHIM[1..])
        .arg(program)
        .env_clear()
        .envs(env.iter().map(|(name, value)| (name, value)))
        .process_group(0);
    assert_eq!(command.get_program(), SESSION_SHIM[0]);
    assert_eq!(
        command.get_args().nth(SESSION_SHIM.len() - 1),
        Some(program.as_os_str())
    );
    assert_eq!(command.get_envs().count(), env.len());
    Ok(command)
}

/// Refuses a Full access agent on Windows: the level is unavailable there, so no command starts
/// without the Windows boundary. Inputs: as the Unix version. Output: always `Unavailable` with
/// [`FULL_ACCESS_NEEDS_MACOS`].
#[cfg(target_os = "windows")]
pub fn unconfined_command<'a>(
    program: &Path,
    base: &[(&str, &OsStr)],
    harness: impl IntoIterator<Item = (&'a OsStr, Option<&'a OsStr>)>,
) -> Result<Command, SandboxError> {
    let _ = (program, base, harness.into_iter().count());
    Err(SandboxError::Unavailable(FULL_ACCESS_NEEDS_MACOS.into()))
}

/// The network a Linux that nests agents gives one limited to ports on any host
/// ([`ProjectSandbox::routed`]): `any`, the ports it leaves through on any host, and `bridged`, the
/// loopback ports carried to this computer as for a bridged agent, each list rising and each port
/// once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Routed {
    pub any: Vec<u16>,
    pub bridged: Vec<u16>,
}

#[cfg(all(
    test,
    any(target_os = "macos", target_os = "linux", target_os = "windows")
))]
mod handed_tests;
#[cfg(test)]
mod tests;
