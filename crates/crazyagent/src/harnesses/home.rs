use super::launch::{LaunchError, failure};
use super::manifest::{self, Manifest, Refused};
use crate::app_plugins::yaml::Value;
use crate::protocol::CredentialRef;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// The most environment variables one manifest's `home.env` sets.
pub const ENV_MAX: usize = manifest::LIST_MAX;

/// The placeholder a `home.env` value may start with: the private home itself.
const HOME: &str = "{home}";

/// Names a `home.env` may never set: the engine sets them itself, or they decide which code
/// a shell, a dynamic linker or a language runtime loads.
const ENGINE_NAMES: [&str; 13] = [
    "PATH",
    "HOME",
    "SHELL",
    "TMPDIR",
    "USER",
    "LOGNAME",
    "BASH_ENV",
    "ENV",
    "NODE_OPTIONS",
    "PYTHONPATH",
    "PYTHONSTARTUP",
    "PYTHONHOME",
    "RUBYOPT",
];

/// Starts of names a `home.env` may never set, for the same reasons as [`ENGINE_NAMES`].
const ENGINE_PREFIXES: [&str; 3] = ["DYLD_", "LD_", "CRAZYAGENT_"];

/// What a manifest's `home` block asks of the private home a harness runs in: the
/// environment variables it sets, each value plain text or a folder under `{home}`, and
/// whether HOME itself is set to the private home. `owner_folders` is read by the manifest
/// reader and the sandbox.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Home {
    pub env: Vec<(String, String)>,
    pub set_home: bool,
}

impl Home {
    /// The environment one launch gets from the home block. Input: the private home.
    /// Output: each `home.env` name with `{home}` filled in, then HOME when the block asks
    /// for it.
    pub fn environment(&self, home: &Path) -> Vec<(OsString, OsString)> {
        let mut environment: Vec<(OsString, OsString)> = self
            .env
            .iter()
            .map(|(name, value)| {
                let filled = match value.strip_prefix(HOME) {
                    Some(rest) => {
                        let mut path = home.as_os_str().to_owned();
                        path.push(rest);
                        path
                    }
                    None => OsString::from(value),
                };
                (OsString::from(name), filled)
            })
            .collect();
        if self.set_home {
            environment.push((OsString::from("HOME"), home.as_os_str().to_owned()));
        }
        assert!(environment.len() <= ENV_MAX + 1);
        environment
    }
}

fn refuse<T>(code: &'static str, message: String) -> Result<T, Refused> {
    Err(Refused { code, message })
}

/// Reads the `home` block of one manifest. Input: the manifest. Output: what it asks, the
/// default for a manifest with no block, or the refusal that names the setting.
pub fn read(manifest: &Manifest) -> Result<Home, Refused> {
    let Some(block) = manifest.block("home") else {
        return Ok(Home::default());
    };
    let Value::Map(entries) = block else {
        return refuse(
            manifest::TYPE,
            "home: this must be a block of settings.".into(),
        );
    };
    let mut home = Home::default();
    for (key, value) in entries {
        match key.as_str() {
            "owner_folders" => {}
            "env" => home.env = variables(value)?,
            "set_home" => {
                let Value::Bool(set) = value else {
                    return refuse(
                        manifest::TYPE,
                        "home.set_home: this must be true or false.".into(),
                    );
                };
                home.set_home = *set;
            }
            _ => {
                return refuse(
                    manifest::UNKNOWN_KEY,
                    format!("home.{key}: this key is not one the home block has."),
                );
            }
        }
    }
    assert!(home.env.len() <= ENV_MAX);
    Ok(home)
}

/// Reads `home.env`: at most [`ENV_MAX`] names of capital letters, digits and `_`, none the
/// engine owns, each with text in quotes that is plain or a folder under `{home}`. Input:
/// the block's value. Output: the names and values in file order, or the refusal.
fn variables(value: &Value) -> Result<Vec<(String, String)>, Refused> {
    let Value::Map(entries) = value else {
        return refuse(
            manifest::TYPE,
            "home.env: this must be a map of names to text in quotes.".into(),
        );
    };
    if entries.len() > ENV_MAX {
        return refuse(
            manifest::VALUE,
            format!("home.env: at most {ENV_MAX} names."),
        );
    }
    let mut variables = Vec::with_capacity(entries.len());
    for (name, value) in entries {
        let path = format!("home.env.{name}");
        if !variable_name(name) {
            return refuse(
                manifest::VALUE,
                format!("{path}: a name is capital letters, digits and _, starting with a letter."),
            );
        }
        if engine_owned(name) {
            return refuse(
                manifest::VALUE,
                format!("{path}: CrazyAgent sets this itself, so a manifest cannot."),
            );
        }
        let Value::Str(text) = value else {
            return refuse(
                manifest::TYPE,
                format!("{path}: this must be text in quotes."),
            );
        };
        if text.len() > manifest::SENTENCE_BYTES_MAX || text.chars().any(char::is_control) {
            return refuse(
                manifest::VALUE,
                format!("{path}: this is too long or holds a control character."),
            );
        }
        let other = manifest::PLACEHOLDERS
            .iter()
            .filter(|placeholder| **placeholder != "home")
            .any(|placeholder| text.contains(&format!("{{{placeholder}}}")));
        if other {
            return refuse(
                manifest::PLACEHOLDER,
                format!("{path}: only {{home}} is filled in here."),
            );
        }
        if text.contains(HOME) && !inside_home(text) {
            return refuse(
                manifest::VALUE,
                format!(
                    "{path}: {{home}} must start the value and be followed only by a folder inside it."
                ),
            );
        }
        variables.push((name.clone(), text.clone()));
    }
    assert!(variables.len() <= ENV_MAX);
    Ok(variables)
}

fn variable_name(name: &str) -> bool {
    name.len() <= manifest::NAME_BYTES_MAX
        && name.as_bytes().first().is_some_and(u8::is_ascii_uppercase)
        && name
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
}

fn engine_owned(name: &str) -> bool {
    ENGINE_NAMES.contains(&name) || ENGINE_PREFIXES.iter().any(|start| name.starts_with(start))
}

/// Whether a value that names `{home}` is the home itself or a folder inside it: `{home}`
/// once, at the start, then nothing or `/` and plain folder names with no `.` or `..`.
/// Input: the value. Output: the answer.
fn inside_home(text: &str) -> bool {
    match text.strip_prefix(HOME) {
        Some("") => true,
        Some(rest) => rest.strip_prefix('/').is_some_and(|rest| {
            !rest.contains(HOME)
                && rest
                    .split('/')
                    .all(|part| !part.is_empty() && part != "." && part != "..")
        }),
        None => false,
    }
}

/// Makes the private home one harness runs in for one account, `<base>/<harness id>/<account>`,
/// each folder 0700 and owned by this user, made through no-follow steps. Inputs: the
/// agent's own 0700 home, a harness id or alias and the account. Output: the home, or the
/// refusal.
#[cfg(unix)]
pub fn prepare(
    base: &Path,
    harness: &str,
    account: &CredentialRef,
) -> Result<PathBuf, LaunchError> {
    use rustix::fs::{Mode, OFlags};
    let id = super::registry::registry()
        .get(harness)
        .map(|found| found.id())
        .ok_or_else(|| failure("launch_harness_unsupported"))?;
    super::launch::validate_home(base)?;
    let unsafe_home = || failure("launch_account_home_unsafe");
    let folder = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let owner = rustix::process::geteuid().as_raw();
    let mut parent = rustix::fs::open(base, folder, Mode::empty()).map_err(|_| unsafe_home())?;
    for name in [id, account.as_str()] {
        match rustix::fs::mkdirat(&parent, name, Mode::RWXU) {
            Ok(()) | Err(rustix::io::Errno::EXIST) => {}
            Err(_) => return Err(unsafe_home()),
        }
        let child =
            rustix::fs::openat(&parent, name, folder, Mode::empty()).map_err(|_| unsafe_home())?;
        let stat = rustix::fs::fstat(&child).map_err(|_| unsafe_home())?;
        if stat.st_uid != owner || u32::from(stat.st_mode) & 0o7777 != 0o700 {
            return Err(unsafe_home());
        }
        parent = child;
    }
    let home = base.join(id).join(account.as_str());
    super::launch::validate_home(&home)?;
    assert!(home.starts_with(base) && home != base);
    Ok(home)
}

/// Refuses a private home where its Unix owner and 0700 mode cannot be checked. Inputs:
/// unused. Output: `launch_account_home_unavailable`.
#[cfg(not(unix))]
pub fn prepare(
    base: &Path,
    harness: &str,
    account: &CredentialRef,
) -> Result<PathBuf, LaunchError> {
    let _ = (base, harness, account);
    Err(failure("launch_account_home_unavailable"))
}

#[cfg(all(test, unix))]
#[path = "home_tests.rs"]
mod tests;
