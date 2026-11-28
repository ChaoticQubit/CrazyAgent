use crate::error::{SandboxError, invalid};
use crate::paths::{canonical, clean_absolute, sits_in_folder};
use crate::profile::ProfileHooks;
use std::path::{Path, PathBuf};

/// A fresh private folder the engine made in its own engine folder for one run: the only
/// project root a sandbox may have at or under that folder
/// ([`crate::profile::ProjectSandbox::in_engine_scratch`]). Only [`EngineScratch::make`]
/// builds one, always as a new folder, so no path the owner chose ever becomes one.
///
/// ```
/// let _make = kernel_sandbox::engine_scratch::EngineScratch::make;
/// ```
///
/// ```compile_fail
/// let _scratch = kernel_sandbox::engine_scratch::EngineScratch {
///     root: std::path::PathBuf::new(),
///     engine: std::path::PathBuf::new(),
/// };
/// ```
#[derive(Debug)]
pub struct EngineScratch {
    root: PathBuf,
    engine: PathBuf,
}

impl EngineScratch {
    /// Makes `<engine>/<parent..>/<name>` as a new folder private to this user, each parent
    /// part made private when missing and judged private when there, and none of it ever
    /// in a record folder the engine never searches for sockets. Inputs: the hooks naming
    /// those folders, the engine folder, the parent parts and the new name. Output: the
    /// scratch, or `Invalid` for a bad name, an engine folder that is not plain, a part
    /// that is a link or not private, or a name already taken.
    pub fn make(
        hooks: &ProfileHooks,
        engine: &Path,
        parent: &[&str],
        name: &str,
    ) -> Result<EngineScratch, SandboxError> {
        if !parent.iter().chain([&name]).all(|part| one_name(part)) {
            return Err(invalid("engine scratch names must each be one plain name"));
        }
        let first = parent.first().unwrap_or(&name);
        assert!(
            !hooks.plain_record_folders.contains(first),
            "an engine scratch folder never sits in a record folder"
        );
        let engine = canonical(engine)?;
        if !clean_absolute(&engine) || !engine.is_dir() {
            return Err(invalid("engine folder must be an existing plain folder"));
        }
        let mut folder = engine.clone();
        for part in parent {
            folder.push(part);
            private_part(&folder, false)?;
        }
        folder.push(name);
        private_part(&folder, true)?;
        if canonical(&folder)? != folder {
            return Err(invalid(
                "engine scratch folder is not the plain folder made",
            ));
        }
        for kind in hooks.plain_record_folders {
            assert!(
                !sits_in_folder(&folder, &engine.join(kind))?,
                "an engine scratch folder never sits in a record folder"
            );
        }
        assert!(folder.starts_with(&engine) && folder != engine);
        Ok(EngineScratch {
            root: folder,
            engine,
        })
    }

    /// The scratch folder, the project root of the sandbox built on it. Output: the path.
    pub fn path(&self) -> &Path {
        &self.root
    }

    /// The canonical engine folder the scratch was made in, the only folder whose guard
    /// lets it be the project root. Output: the path.
    pub(crate) fn engine(&self) -> &Path {
        &self.engine
    }
}

/// Reads a scratch as the folder it is, so a caller passes it wherever a path is taken; no
/// path ever turns back into a scratch.
impl std::ops::Deref for EngineScratch {
    type Target = Path;

    fn deref(&self) -> &Path {
        &self.root
    }
}

/// Tells whether a part is one plain name: not empty, not `.` or `..`, no separator and no
/// control character. Input: the part. Output: the answer.
fn one_name(part: &str) -> bool {
    !part.is_empty()
        && part != "."
        && part != ".."
        && !part
            .chars()
            .any(|c| c.is_control() || matches!(c, '/' | '\\' | ':'))
}

/// Makes a folder private to this user, or with `fresh` false judges one already there:
/// a real folder, never a link, owned by this user, with no group or other access. A fresh
/// folder already there is refused, never reused. Inputs: the path and whether it must be
/// new. Output: unit, or the refusal.
#[cfg(unix)]
fn private_part(path: &Path, fresh: bool) -> Result<(), SandboxError> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
    match std::fs::DirBuilder::new().mode(0o700).create(path) {
        Ok(()) => {}
        Err(error) if !fresh && error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            return Err(invalid("engine scratch folder already exists"));
        }
        Err(error) => return Err(SandboxError::Io(error)),
    }
    let metadata = std::fs::symlink_metadata(path)?;
    let private = metadata.is_dir()
        && metadata.uid() == rustix::process::geteuid().as_raw()
        && metadata.permissions().mode() & 0o077 == 0;
    if !private {
        return Err(invalid("engine scratch folder is not private to this user"));
    }
    Ok(())
}

/// Makes a folder in the engine folder, or with `fresh` false judges one already there, then
/// judges it as W1 judges a private folder ([`crazyagent_win_sandbox::private::open_dir`]):
/// no link or junction, owned by this user, and no account but this user, SYSTEM and the
/// administrators with any right on it. It takes the engine folder's own list, with no label,
/// so a container granted on it still reads it. Inputs: the path and whether it must be new.
/// Output: unit, or the refusal.
#[cfg(windows)]
fn private_part(path: &Path, fresh: bool) -> Result<(), SandboxError> {
    match std::fs::create_dir(path) {
        Ok(()) => {}
        Err(error) if !fresh && error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            return Err(invalid("engine scratch folder already exists"));
        }
        Err(error) => return Err(SandboxError::Io(error)),
    }
    match crazyagent_win_sandbox::private::open_dir(path) {
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Err(SandboxError::Io(error)),
        Err(_) => Err(invalid("engine scratch folder is not private to this user")),
    }
}

#[cfg(test)]
mod tests;
