#[cfg(target_os = "macos")]
use kernel_sandbox::toolchains::plain_path;
use kernel_sandbox::toolchains::{GIT_FOLDER, GIT_PROGRAM, ToolchainHooks};
#[cfg(unix)]
use kernel_sandbox::toolchains::{GitCopy, git_script, write_git};
use sha2::{Digest, Sha256};
use std::fs;
use std::io;
#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

/// The system temp folder by its real path, which the agent short temp folders sit in.
#[cfg(target_os = "macos")]
pub(crate) const TMP_PARENT: &str = "/private/tmp";
/// The system temp folder, which the agent short temp folders sit in.
#[cfg(not(target_os = "macos"))]
pub(crate) const TMP_PARENT: &str = "/tmp";
const TMP_PREFIX: &str = "ca-";
const TMP_HASH_BYTES: usize = 8;
/// The folder of a native lead's short temp folder that is the lead's own temp folder.
const NATIVE_LEAD_TMP: &str = "lead";
/// The folder of an agent's launch folder ([`crate::launch_context::STATE`]) that holds its
/// own `getconf` ([`write_getconf`]), and that program.
const GETCONF_FOLDER: &str = "bin";
const GETCONF_PROGRAM: &str = "getconf";
/// The most bytes of an agent's `getconf` script.
#[cfg(target_os = "macos")]
const GETCONF_SCRIPT_BYTES_MAX: usize = 8192;
/// The script an agent's own `getconf` holds, with `@MATES@` for a native lead's teammates'
/// home and `@SHARED@` and `@OWN@` for their temp folder and the agent's own, each quoted,
/// the folders ending in a slash as the system answer does: DARWIN_USER_TEMP_DIR names the
/// caller's TMPDIR, where Seatbelt lets it write, and every other call runs the system one.
#[cfg(target_os = "macos")]
const GETCONF_SCRIPT: &str = r#"#!/bin/sh
if [ "$#" -eq 1 ] && [ "$1" = DARWIN_USER_TEMP_DIR ]; then
  case $HOME in
    @MATES@) printf '%s\n' @SHARED@ ;;
    *) printf '%s\n' @OWN@ ;;
  esac
  exit 0
fi
exec /usr/bin/getconf "$@"
"#;
/// The short temp folder of the agent whose home is `home`: `ca-` and 16 hex digits of the
/// home path's SHA-256 in [`TMP_PARENT`] (`/private/tmp` on macOS, `/tmp` elsewhere), short enough for Unix socket paths. Input: the agent
/// home. Output: the path, which may not exist.
pub fn tmp_path(home: &Path) -> PathBuf {
    let digest = Sha256::digest(home.as_os_str().as_encoded_bytes());
    let hash: String = digest[..TMP_HASH_BYTES]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let path = Path::new(TMP_PARENT).join(format!("{TMP_PREFIX}{hash}"));
    assert_eq!(
        path.as_os_str().len(),
        TMP_PARENT.len() + 1 + TMP_PREFIX.len() + 2 * TMP_HASH_BYTES
    );
    path
}

/// Whether a path lies in an agent's short temp folder ([`tmp_path`]): the folder in
/// [`TMP_PARENT`] named `ca-` and 16 lowercase hex digits, which the agent writes though
/// this user owns it. A folder named otherwise there, such as a test's scratch, is not one.
/// Input: the path. Output: the answer.
pub(crate) fn in_agent_tmp(path: &Path) -> bool {
    let Ok(rest) = path.strip_prefix(TMP_PARENT) else {
        return false;
    };
    let Some(std::path::Component::Normal(name)) = rest.components().next() else {
        return false;
    };
    name.to_str()
        .and_then(|name| name.strip_prefix(TMP_PREFIX))
        .is_some_and(|hash| {
            hash.len() == 2 * TMP_HASH_BYTES
                && hash
                    .bytes()
                    .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
        })
}

/// Makes the agent's short temp folder with mode 0700, or reuses the one this user already
/// owns there and makes it private again, and writes the agent's own `git` into it (see
/// [`with_agent_git`]) as the worker copy that refuses `crazyagent/main` too, which
/// [`kernel_sandbox::toolchains::catch_up_git`] widens while "Catch up with main" is on, so a copy that cannot be
/// widened keeps the refusal; a `git` that cannot be written is logged and the agent keeps
/// the system one. Input: the agent home. Output: the folder, or a refusal when the path is a
/// link, a file or another user's folder.
#[cfg(unix)]
pub fn make_tmp(hooks: &ToolchainHooks, home: &Path) -> io::Result<PathBuf> {
    let path = tmp_path(home);
    match fs::DirBuilder::new().mode(0o700).create(&path) {
        Err(error) if error.kind() != io::ErrorKind::AlreadyExists => return Err(error),
        _ => {}
    }
    if !own_folder(&path) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "the agent temp path is not a folder of this user",
        ));
    }
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700))?;
    assert!(private_folder(&path));
    if let Some(script) = git_script((hooks.sandboxed_git)(), GitCopy::Worker, false)
        && let Err(error) = write_git(&path, &script)
    {
        eprintln!("agent_git_not_written kind={:?}", error.kind());
    }
    Ok(path)
}

/// The script of an agent's own `getconf` ([`GETCONF_SCRIPT`]). Inputs: a native lead's
/// teammates' home, their temp folder and the agent's own. Output: the script, or none when
/// a path is not plain or the script is too long.
#[cfg(target_os = "macos")]
fn getconf_script(mates: &Path, shared: &Path, own: &Path) -> Option<String> {
    let script = GETCONF_SCRIPT
        .replace("@MATES@", &format!("'{}'", plain_path(mates)?))
        .replace("@SHARED@", &format!("'{}/'", plain_path(shared)?))
        .replace("@OWN@", &format!("'{}/'", plain_path(own)?));
    assert!(
        ["@MATES@", "@SHARED@", "@OWN@"]
            .iter()
            .all(|mark| !script.contains(mark))
    );
    (script.len() <= GETCONF_SCRIPT_BYTES_MAX).then_some(script)
}

/// Writes the agent's own `getconf` into `bin` of the launch folder the engine has just
/// made for one launch, which neither the agent nor a native lead's teammates may write
/// (LL-2, TPH). context-mode makes its script folders under `getconf DARWIN_USER_TEMP_DIR`,
/// ignoring TMPDIR, and Seatbelt denies the system answer, so this one names where the
/// caller may write: its short temp folder, or the lead's own one in it for a native lead
/// (T3-15/18). A research job makes its short temp folder after the install, so it calls this
/// again then (R7-CTX F1): a launch folder that already holds this same `getconf` is kept,
/// and one that holds anything else is refused. Inputs: the agent home and whether it leads a
/// native team. Output: done, also when the agent has no short temp folder or a path is not
/// plain and none is written, or the write failure.
#[cfg(target_os = "macos")]
pub(crate) fn write_getconf(
    hooks: &ToolchainHooks,
    home: &Path,
    native_lead: bool,
) -> io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let Some(shared) = existing_tmp(home) else {
        return Ok(());
    };
    let own = if native_lead {
        native_lead_tmp(&shared)
    } else {
        shared.clone()
    };
    let mates = (hooks.mates_home)(home);
    let Some(script) = getconf_script(&mates, &shared, &own) else {
        return Ok(());
    };
    let folder = getconf_folder(hooks, home);
    let path = folder.join(GETCONF_PROGRAM);
    if let Err(error) = fs::DirBuilder::new().mode(0o700).create(&folder) {
        if error.kind() == io::ErrorKind::AlreadyExists && holds_getconf(&folder, &path, &script) {
            return Ok(());
        }
        return Err(error);
    }
    let written = fs::set_permissions(&folder, fs::Permissions::from_mode(0o700))
        .and_then(|()| {
            fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o700)
                .custom_flags(
                    (rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::CLOEXEC).bits() as i32,
                )
                .open(&path)
        })
        .and_then(|mut file| {
            file.set_permissions(fs::Permissions::from_mode(0o700))?;
            file.write_all(script.as_bytes())?;
            file.sync_all()?;
            file.metadata()
        });
    match written {
        Ok(metadata)
            if metadata.mode() & 0o777 == 0o700 && metadata.len() == script.len() as u64 =>
        {
            Ok(())
        }
        outcome => {
            let _ = fs::remove_file(&path);
            let _ = fs::remove_dir(&folder);
            Err(outcome
                .err()
                .unwrap_or_else(|| io::Error::other("launch_getconf_unsafe")))
        }
    }
}

/// Whether the launch folder already holds this exact `getconf`: `bin` a real folder and
/// `getconf` in it a real file of mode 0700 with the script's bytes, opened without following
/// a link. Inputs: the folder, the file and the script. Output: the answer.
#[cfg(target_os = "macos")]
fn holds_getconf(folder: &Path, path: &Path, script: &str) -> bool {
    use std::io::Read;
    use std::os::unix::fs::OpenOptionsExt;
    if !fs::symlink_metadata(folder).is_ok_and(|metadata| metadata.is_dir()) {
        return false;
    }
    let Ok(file) = fs::OpenOptions::new()
        .read(true)
        .custom_flags((rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::CLOEXEC).bits() as i32)
        .open(path)
    else {
        return false;
    };
    let plain = file
        .metadata()
        .is_ok_and(|metadata| metadata.is_file() && metadata.mode() & 0o777 == 0o700);
    let mut held = Vec::with_capacity(script.len());
    plain
        && file
            .take(GETCONF_SCRIPT_BYTES_MAX as u64 + 1)
            .read_to_end(&mut held)
            .is_ok()
        && held == script.as_bytes()
}

/// The folder of the agent's launch folder that holds its own `getconf`. Input: the agent
/// home. Output: the path, which may not exist.
fn getconf_folder(hooks: &ToolchainHooks, home: &Path) -> PathBuf {
    home.join(hooks.launch_state).join(GETCONF_FOLDER)
}

/// Puts the agent's own `getconf` (see [`write_getconf`]) and then its own `git` (see
/// [`make_tmp`]) at the front of a harness's search path, ahead of every toolchain and
/// system folder, so the agent's git calls under Seatbelt run the real git, not the
/// `/usr/bin/git` shim, and context-mode makes its temp folders where the agent may write.
/// Only a harness's own path gets them, never a command the engine runs under the profile,
/// since the agent can change its `git` folder. Inputs: the agent home and the path.
/// Output: the path, without each program the agent does not have.
pub fn with_agent_git(hooks: &ToolchainHooks, home: &Path, path: String) -> String {
    let path = match existing_tmp(home) {
        Some(tmp) => in_front(&tmp.join(GIT_FOLDER), GIT_PROGRAM, path),
        None => path,
    };
    in_front(&getconf_folder(hooks, home), GETCONF_PROGRAM, path)
}

/// Puts a folder at the front of a search path when it holds the program as a file and its
/// name has no `:`. Inputs: the folder, the program and the path. Output: the path.
fn in_front(folder: &Path, program: &str, path: String) -> String {
    let present =
        fs::symlink_metadata(folder.join(program)).is_ok_and(|metadata| metadata.is_file());
    match folder.to_str() {
        Some(text) if present && !text.contains(':') => format!("{text}:{path}"),
        _ => path,
    }
}

/// The agent's short temp folder when it exists as this user's private folder. Input: the
/// agent home. Output: the folder, or None.
#[cfg(unix)]
pub fn existing_tmp(home: &Path) -> Option<PathBuf> {
    let path = tmp_path(home);
    private_folder(&path).then_some(path)
}

/// A native lead's own temp folder, its `TMPDIR`: `lead` in its short temp folder, which its
/// teammates share for their tmux socket and temp files but never reach inside (TPH-2
/// residual). Input: the lead's short temp folder. Output: the path, which may not exist.
pub fn native_lead_tmp(scratch: &Path) -> PathBuf {
    scratch.join(NATIVE_LEAD_TMP)
}

/// Makes a native lead's own temp folder empty and private for one start: whatever holds
/// the name, a link, a file or a folder, is removed without following a link, then the
/// folder is made with mode 0700, so nothing a teammate left there under an earlier build
/// reaches the lead. Input: the lead's short temp folder. Output: the folder, or the error,
/// also when something takes the name in between or the temp folder is not private.
#[cfg(unix)]
pub fn fresh_native_lead_tmp(scratch: &Path) -> io::Result<PathBuf> {
    if !private_folder(scratch) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "the agent temp path is not a private folder of this user",
        ));
    }
    let path = native_lead_tmp(scratch);
    match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.is_dir() => fs::remove_dir_all(&path)?,
        Ok(_) => fs::remove_file(&path)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    fs::DirBuilder::new().mode(0o700).create(&path)?;
    seal_new_folder(&path)?;
    Ok(path)
}

/// Checks the folder just made at `path` with mode 0700 through one descriptor opened
/// without following a link: it must be empty and still the private folder the name holds,
/// so a link or a folder a teammate swaps in meanwhile is refused, never followed, changed
/// or kept. Input: the new folder's path. Output: done, or the refusal.
#[cfg(unix)]
fn seal_new_folder(path: &Path) -> io::Result<()> {
    use rustix::fs::{CWD, Dir, Mode, OFlags};
    use rustix::io::Errno;
    let changed = || {
        io::Error::new(
            io::ErrorKind::PermissionDenied,
            "the lead's own temp folder changed while it was made",
        )
    };
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let folder = match rustix::fs::openat(CWD, path, flags, Mode::empty()) {
        Ok(folder) => fs::File::from(folder),
        Err(Errno::LOOP | Errno::NOTDIR) => return Err(changed()),
        Err(error) => return Err(error.into()),
    };
    for entry in Dir::read_from(&folder)? {
        if !matches!(entry?.file_name().to_bytes(), b"." | b"..") {
            return Err(changed());
        }
    }
    let metadata = folder.metadata()?;
    if private_folder_pin(path) != Some((metadata.dev(), metadata.ino())) {
        return Err(changed());
    }
    Ok(())
}

/// Removes the agent's short temp folder with everything in it, as Clean up removes an
/// agent's tree ([`agent_tree_gone`]), so a folder it shut or a file it made immutable does
/// not keep it. Input: the agent home. Output: done when it is gone or never existed, or a
/// refusal when the path is not this user's folder.
#[cfg(unix)]
pub fn remove_tmp(hooks: &ToolchainHooks, home: &Path) -> io::Result<()> {
    let path = tmp_path(home);
    match fs::symlink_metadata(&path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
        Ok(_) if !own_folder(&path) => {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "the agent temp path is not a folder of this user",
            ));
        }
        Ok(_) => {}
    }
    agent_tree_gone(hooks, &path)
}

/// Removes a folder an agent wrote, with all it holds, through
/// [`crate::team::cleanup::remove_agent_tree`], which opens up what the agent shut and clears
/// its immutable flags without following a link. Input: the folder. Output: done once nothing
/// has its name, or the error naming why it stays.
#[cfg(unix)]
fn agent_tree_gone(hooks: &ToolchainHooks, folder: &Path) -> io::Result<()> {
    (hooks.remove_agent_tree)(folder)
}

/// Removes everything in the agent's short temp folder but the socket named `kept`, which
/// stays there with the folder; with no such socket in this user's folder the folder goes
/// as [`remove_tmp`] removes it. A native lead's caller socket is kept this way as the lead
/// ends, so a teammate still running asks a socket no engine answers on and is refused,
/// never finding no socket (rv-secreview RH2R2-1). Input: the agent home and the kept name.
/// Output: as [`remove_tmp`].
#[cfg(unix)]
pub fn remove_tmp_keeping(hooks: &ToolchainHooks, home: &Path, kept: &str) -> io::Result<()> {
    let path = tmp_path(home);
    let keeps = own_folder(&path)
        && fs::symlink_metadata(path.join(kept))
            .is_ok_and(|metadata| std::os::unix::fs::FileTypeExt::is_socket(&metadata.file_type()));
    if !keeps {
        return remove_tmp(hooks, home);
    }
    for entry in fs::read_dir(&path)? {
        let entry = entry?;
        if entry.file_name() == kept {
            continue;
        }
        if entry.file_type()?.is_dir() {
            agent_tree_gone(hooks, &entry.path())?;
        } else {
            fs::remove_file(entry.path())?;
        }
    }
    Ok(())
}

/// Whether `path` is itself one short temp folder ([`tmp_path`]) this user owns privately,
/// so the engine may remove it with all it holds: exactly a `ca-` and 16 lowercase hex
/// digits name in [`TMP_PARENT`], a real folder, not a link, with no access for others.
/// Input: the path. Output: the answer.
#[cfg(unix)]
pub(crate) fn own_short_tmp(path: &Path) -> bool {
    in_agent_tmp(path)
        && path.parent() == Some(Path::new(TMP_PARENT))
        && path.as_os_str().len() == TMP_PARENT.len() + 1 + TMP_PREFIX.len() + 2 * TMP_HASH_BYTES
        && private_folder(path)
}

#[cfg(unix)]
fn own_folder(path: &Path) -> bool {
    fs::symlink_metadata(path)
        .is_ok_and(|metadata| metadata.is_dir() && metadata.uid() == nix::unistd::getuid().as_raw())
}

#[cfg(unix)]
fn private_folder(path: &Path) -> bool {
    private_folder_pin(path).is_some()
}

/// The (device, inode) of a folder that is this user's own private folder, read in one
/// look that never follows a link. Input: the path. Output: the pin, or None for a link, a
/// file, another user's folder or one open to others.
#[cfg(unix)]
pub fn private_folder_pin(path: &Path) -> Option<(u64, u64)> {
    let metadata = fs::symlink_metadata(path).ok()?;
    (metadata.is_dir()
        && metadata.uid() == nix::unistd::getuid().as_raw()
        && metadata.mode() & 0o077 == 0)
        .then(|| (metadata.dev(), metadata.ino()))
}

/// The short temp folder of one agent run that ends inside a single call, removed with
/// everything in it when dropped.
#[derive(Debug)]
pub struct ScopedTmp {
    hooks: &'static ToolchainHooks,
    home: PathBuf,
    path: PathBuf,
}

impl ScopedTmp {
    /// Makes the folder as [`make_tmp`] does. Inputs: the hooks, kept to remove the folder
    /// when dropped, and the agent home. Output: the scoped folder, or the refusal.
    pub fn make(hooks: &'static ToolchainHooks, home: &Path) -> io::Result<Self> {
        let path = make_tmp(hooks, home)?;
        Ok(Self {
            hooks,
            home: home.to_path_buf(),
            path,
        })
    }

    /// The folder, the run's `TMPDIR`.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for ScopedTmp {
    fn drop(&mut self) {
        let _ = remove_tmp(self.hooks, &self.home);
    }
}

/// No short temp folder outside Unix; agents keep the temp dir their launch sets. Takes the
/// agent home, unused. Returns None.
#[cfg(not(unix))]
pub fn existing_tmp(_home: &Path) -> Option<PathBuf> {
    None
}

/// No short temp folder outside Unix, where no agent sandbox runs. Takes the agent home,
/// unused. Returns `Unsupported`.
#[cfg(not(unix))]
pub fn make_tmp(_hooks: &ToolchainHooks, _home: &Path) -> io::Result<PathBuf> {
    Err(io::ErrorKind::Unsupported.into())
}

/// No native lead's own temp folder outside Unix, where no native team runs. Takes the
/// short temp folder, unused. Returns `Unsupported`.
#[cfg(not(unix))]
pub fn fresh_native_lead_tmp(_scratch: &Path) -> io::Result<PathBuf> {
    Err(io::ErrorKind::Unsupported.into())
}

/// Nothing to remove outside Unix. Takes the agent home, unused. Returns done.
#[cfg(not(unix))]
pub fn remove_tmp(_hooks: &ToolchainHooks, _home: &Path) -> io::Result<()> {
    Ok(())
}

/// Nothing to remove outside Unix. Takes the agent home and the kept name, unused. Returns
/// done.
#[cfg(not(unix))]
pub fn remove_tmp_keeping(_hooks: &ToolchainHooks, _home: &Path, _kept: &str) -> io::Result<()> {
    Ok(())
}

#[cfg(all(test, unix))]
mod tests;
