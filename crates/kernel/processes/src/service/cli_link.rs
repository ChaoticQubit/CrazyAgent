use super::*;

/// Links the running `crazyagent` into a folder on the user's PATH so a shell finds it:
/// `~/.local/bin/crazyagent` on macOS and Linux. An absent name gets the link made in
/// place, which never replaces a file that arrived first; a link already there is kept
/// unwritten when it is this user's own and names the executable ([`kept_link`]), else
/// swapped for the new one in one step ([`replace_link`]) without a make tried first; any
/// other file is refused and kept. Inputs: the executable and the bin folder. Output: the
/// link.
#[cfg(unix)]
pub fn link_cli(executable: &Path, bin: &Path) -> Result<PathBuf, ServiceError> {
    let fail = |path: &Path, error: std::io::Error| failure(format!("{}: {error}", path.display()));
    if !executable.is_absolute() || !executable.is_file() {
        return Err(failure(format!(
            "{}: the executable to link is missing",
            executable.display()
        )));
    }
    persisted(executable)?;
    std::fs::create_dir_all(bin).map_err(|error| fail(bin, error))?;
    let link = bin.join("crazyagent");
    let found = std::fs::symlink_metadata(&link);
    let mut there = match &found {
        Ok(metadata) if !metadata.file_type().is_symlink() => return Err(not_a_link(&link)),
        Ok(_) => true,
        Err(_) => false,
    };
    #[cfg(test)]
    BEFORE_PUBLISH.with(|step| {
        if let Some(step) = step.borrow_mut().take() {
            step(&link);
        }
    });
    if let Ok(metadata) = &found
        && kept_link(&link, metadata, executable)
    {
        return Ok(link);
    }
    for _ in 0..PUBLISH_ROUNDS_MAX {
        if !there {
            match std::os::unix::fs::symlink(executable, &link) {
                Ok(()) => return Ok(link),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(fail(&link, error)),
            }
        }
        if replace_link(executable, &link)? {
            return Ok(link);
        }
        there = false;
    }
    Err(not_a_link(&link))
}

/// Whether the link already at the name may stay as it is: read without following it, it
/// names the executable by the same bytes, and `lstat` found it this user's own with no
/// access list that lets another user change it ([`keeps`]). Any read that fails says no,
/// so the link is swapped as before. Inputs: the link, what `lstat` read of it, and the
/// executable. Output: the answer.
#[cfg(unix)]
fn kept_link(link: &Path, metadata: &std::fs::Metadata, executable: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    assert!(metadata.file_type().is_symlink());
    let named = std::fs::read_link(link).ok();
    keeps(
        metadata.uid(),
        current_uid(),
        link_shared(link, metadata),
        named.as_deref(),
        executable,
    )
}

/// The rule [`kept_link`] applies to what it read: the link's owner is this user, its
/// access list lets no other user change it, and it names the executable by exactly the
/// same bytes, so another spelling of the same path is not kept. Inputs: the link's owner,
/// this user, the access-list answer, what the link names and the executable. Output:
/// whether the link stays.
#[cfg(unix)]
pub(super) fn keeps(
    owner: u32,
    me: u32,
    shared: Option<bool>,
    named: Option<&Path>,
    executable: &Path,
) -> bool {
    owner == me
        && shared == Some(false)
        && named.is_some_and(|named| named.as_os_str() == executable.as_os_str())
}

/// Whether a link's own access list lets another user change it, read without following
/// the link ([`super::acl::shared_all`]). Input: the link and what `lstat` read of it.
/// Output: the answer, none when it cannot be read.
#[cfg(target_os = "macos")]
fn link_shared(link: &Path, metadata: &std::fs::Metadata) -> Option<bool> {
    #[cfg(test)]
    ACL_READS.with(|reads| reads.set(reads.get() + 1));
    super::acl::shared_all(&[(link, metadata)])[0]
}

#[cfg(all(test, target_os = "macos"))]
thread_local! {
    static ACL_READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// How many link access lists [`link_cli`] has read on this test thread. Output: the count.
#[cfg(all(test, target_os = "macos"))]
pub(super) fn acl_reads() -> usize {
    ACL_READS.with(std::cell::Cell::get)
}

/// Whether a link's own access list lets another user change it. Linux keeps no access
/// list on a link, so the answer is always no. Inputs: unused. Output: the answer.
#[cfg(all(unix, not(target_os = "macos")))]
fn link_shared(_link: &Path, _metadata: &std::fs::Metadata) -> Option<bool> {
    Some(false)
}

/// The most rounds [`link_cli`] tries to publish when the name keeps going away between
/// its make and its swap, before it refuses.
#[cfg(unix)]
const PUBLISH_ROUNDS_MAX: usize = 3;
#[cfg(unix)]
const _: () = assert!(PUBLISH_ROUNDS_MAX >= 1 && PUBLISH_ROUNDS_MAX <= 8);

/// The refusal for a name that holds something other than a link. Input: the name.
/// Output: the error.
#[cfg(unix)]
fn not_a_link(link: &Path) -> ServiceError {
    failure(format!(
        "{}: a file that is not a link is already there; move it away first",
        link.display()
    ))
}

/// Swaps a new link for whatever is at `link` in one step (`RENAME_SWAP` on macOS,
/// `RENAME_EXCHANGE` on Linux), then reads what came out: a link is removed, anything else
/// is swapped back and refused, so a file another installer wrote is never lost; when the
/// swap back fails, the error names where that file now sits. A file system that cannot
/// swap refuses with the staged link removed. A link left at the staged name is removed
/// only when the staged make finds the name taken, and anything else there is kept and
/// refused. Inputs: the executable and the link's name.
/// Output: true when published, false when the name went away before the swap, or the
/// refusal.
#[cfg(unix)]
fn replace_link(executable: &Path, link: &Path) -> Result<bool, ServiceError> {
    use rustix::fs::{CWD, RenameFlags, renameat_with};
    use rustix::io::Errno;
    let folder = link.parent().expect("a link name has a folder");
    let staged = folder.join(format!(".crazyagent.link.{}", std::process::id()));
    let fail = |path: &Path, error: std::io::Error| failure(format!("{}: {error}", path.display()));
    let drop_staged = || {
        if std::fs::symlink_metadata(&staged)
            .is_ok_and(|metadata| metadata.file_type().is_symlink())
        {
            let _ = std::fs::remove_file(&staged);
        }
    };
    let made = match std::os::unix::fs::symlink(executable, &staged) {
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            drop_staged();
            std::os::unix::fs::symlink(executable, &staged)
        }
        made => made,
    };
    made.map_err(|error| fail(&staged, error))?;
    match renameat_with(CWD, &staged, CWD, link, RenameFlags::EXCHANGE) {
        Ok(()) => {}
        Err(Errno::NOENT) => {
            drop_staged();
            return Ok(false);
        }
        Err(Errno::INVAL | Errno::NOTSUP | Errno::NOSYS) => {
            drop_staged();
            return Err(failure(format!(
                "this file system cannot swap the link in one step; remove the link at {} first",
                link.display()
            )));
        }
        Err(errno) => {
            drop_staged();
            return Err(fail(link, errno.into()));
        }
    }
    let old = std::fs::symlink_metadata(&staged);
    if old.is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        std::fs::remove_file(&staged).map_err(|error| fail(&staged, error))?;
        return Ok(true);
    }
    match renameat_with(CWD, &staged, CWD, link, RenameFlags::EXCHANGE) {
        Ok(()) => {
            drop_staged();
            Err(not_a_link(link))
        }
        Err(errno) => Err(failure(format!(
            "{}: a file that is not a link arrived there and is kept at {}: {}",
            link.display(),
            staged.display(),
            std::io::Error::from(errno)
        ))),
    }
}

/// A step a test row runs on the link's name.
#[cfg(all(test, unix))]
type PublishStep = Box<dyn FnOnce(&Path)>;

#[cfg(all(test, unix))]
thread_local! {
    /// A step one test row runs on the link's name after [`link_cli`] checks it and before
    /// it publishes the link.
    static BEFORE_PUBLISH: std::cell::RefCell<Option<PublishStep>> =
        const { std::cell::RefCell::new(None) };
}

/// Sets the step the next [`link_cli`] on this test thread runs on the link's name between
/// its check and its publish. Input: the step.
#[cfg(all(test, unix))]
pub(super) fn before_publish(step: impl FnOnce(&Path) + 'static) {
    BEFORE_PUBLISH.with(|slot| *slot.borrow_mut() = Some(Box::new(step)));
}

/// Adds the folder holding `crazyagent.exe` to the user's PATH on Windows through
/// PowerShell, keeping every existing entry. A folder whose path holds ';' is refused with
/// the user's PATH untouched, since PATH splits its entries there. Inputs: the executable
/// and an unused bin folder. Output: the folder added, or the refusal.
#[cfg(not(unix))]
pub fn link_cli(executable: &Path, _bin: &Path) -> Result<PathBuf, ServiceError> {
    link_cli_under(executable, USER_ENVIRONMENT_KEY)
}

/// [`link_cli`] on the PATH value stored under one registry key: the user's environment key
/// in the product, a key of its own in each test row, so no row writes the PATH of the
/// machine running it. Inputs: the executable and the key. Output: the folder, or the refusal.
#[cfg(not(unix))]
pub(super) fn link_cli_under(executable: &Path, key: &str) -> Result<PathBuf, ServiceError> {
    let folder = executable
        .parent()
        .ok_or_else(|| failure("the executable has no folder"))?
        .to_path_buf();
    if folder.to_string_lossy().contains(';') {
        return Err(failure(format!(
            "{} holds a ';', where the Windows PATH splits its entries; the user PATH is unchanged",
            folder.display()
        )));
    }
    let mut command = windows_path_command_under(&folder, key);
    let output = crate::spawn_gate::output(
        command
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped()),
    )
    .map_err(|error| failure(format!("powershell.exe: {error}")))?;
    if output.status.code() == Some(PATH_BUSY_EXIT) {
        return Err(failure(
            "another install is changing the PATH; try again; the user PATH is unchanged",
        ));
    }
    if !output.status.success() {
        return Err(failure(format!(
            "the PATH command failed ({}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(folder)
}

/// The environment variable that hands the PATH lock's name ([`path_lock_name`]) to the
/// Windows PATH command.
const PATH_LOCK_VARIABLE: &str = "CRAZYAGENT_PATH_LOCK";

/// How long the Windows PATH command waits for another install to release the PATH lock.
const PATH_LOCK_WAIT_MS: u32 = 10_000;

/// The exit code of the Windows PATH command when the PATH lock stayed held past the
/// wait; no other path in the command exits with it.
const PATH_BUSY_EXIT: i32 = 3;

/// The name of the lock that serializes PATH changes under one registry key among this
/// user's installs in one logon session: `Local\CrazyAgent-path-<key>`, the key's `\` and
/// `:` read as `_`. `Local\` keeps other users from creating it first. Input: the key.
/// Output: the name.
pub(super) fn path_lock_name(key: &str) -> String {
    format!(r"Local\CrazyAgent-path-{}", key.replace(['\\', ':'], "_"))
}

/// The environment variable that hands the folder to the Windows PATH command. PowerShell
/// joins every argument after `-Command` into the script text, so the folder never goes
/// on the command line.
pub const WINDOWS_PATH_FOLDER_VARIABLE: &str = "CRAZYAGENT_PATH_FOLDER";

/// The environment variable that hands the registry key holding the PATH value to the
/// Windows PATH command, read as data like the folder.
const WINDOWS_PATH_KEY_VARIABLE: &str = "CRAZYAGENT_PATH_KEY";

/// The registry key of the user's stored environment, where the user's PATH value lives.
const USER_ENVIRONMENT_KEY: &str = r"HKCU:\Environment";

/// The PowerShell command that appends a folder to the user's PATH once
/// ([`windows_path_command_under`] on [`USER_ENVIRONMENT_KEY`]). Input: the folder.
/// Output: the command.
pub fn windows_path_command(folder: &Path) -> std::process::Command {
    windows_path_command_under(folder, USER_ENVIRONMENT_KEY)
}

/// Appends a folder once to the PATH value under a key, read as written (%VAR% unexpanded)
/// and stored back with its registry kind (REG_EXPAND_SZ when new); deleting a CrazyAgent-only
/// user variable through .NET then announces the change. The read and the write happen while
/// holding the key's PATH lock ([`path_lock_name`]), so two installs never drop each other's
/// folder; an abandoned lock is taken over, and one still held after [`PATH_LOCK_WAIT_MS`]
/// ends the command with [`PATH_BUSY_EXIT`] and the value unchanged. Inputs: the folder and
/// an HKCU key, handed over in the environment with the lock's name so they are read as
/// data, never as script. Output: the command; it exits 2 when any of them arrives empty.
fn windows_path_command_under(folder: &Path, key: &str) -> std::process::Command {
    assert!(key.starts_with(r"HKCU:\"), "{key}");
    assert!(key.len() > r"HKCU:\".len(), "{key}");
    let script = format!(
        "$ErrorActionPreference='Stop'; \
$f=$env:{WINDOWS_PATH_FOLDER_VARIABLE}; if ([string]::IsNullOrEmpty($f)) {{ exit 2 }}; \
$r=$env:{WINDOWS_PATH_KEY_VARIABLE}; if ([string]::IsNullOrEmpty($r)) {{ exit 2 }}; \
$n=$env:{PATH_LOCK_VARIABLE}; if ([string]::IsNullOrEmpty($n)) {{ exit 2 }}; \
$m=[System.Threading.Mutex]::new($false, $n); \
try {{ if (-not $m.WaitOne({PATH_LOCK_WAIT_MS})) {{ exit {PATH_BUSY_EXIT} }} }} \
catch [System.Threading.AbandonedMutexException] {{ }}; \
try {{ \
$k=Get-Item -LiteralPath $r; \
$p=''; $t=[Microsoft.Win32.RegistryValueKind]::ExpandString; \
if ($k.GetValueNames() -contains 'Path') {{ \
$p=[string]$k.GetValue('Path','',[Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames); \
$t=$k.GetValueKind('Path') }}; \
if (-not ((($p -split ';') | ForEach-Object {{ [Environment]::ExpandEnvironmentVariables($_) }}) -contains $f)) {{ \
$v=(($p.TrimEnd(';'), $f) -join ';').TrimStart(';'); \
Set-ItemProperty -LiteralPath $r -Name 'Path' -Value $v -Type $t; \
[Environment]::SetEnvironmentVariable('CRAZYAGENT_PATH_REFRESH', $null, 'User') }} \
}} finally {{ $m.ReleaseMutex() }}"
    );
    assert!(!script.contains("HKCU:"));
    let mut command = std::process::Command::new("powershell.exe");
    command
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .env(WINDOWS_PATH_FOLDER_VARIABLE, folder)
        .env(WINDOWS_PATH_KEY_VARIABLE, key)
        .env(PATH_LOCK_VARIABLE, path_lock_name(key));
    command
}
