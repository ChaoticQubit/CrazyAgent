use super::*;

/// Links the running `crazyagent` into a folder on the user's PATH so a shell finds it:
/// `~/.local/bin/crazyagent` on macOS and Linux. An absent name gets the link made in
/// place, which never replaces a file that arrived first; a link already there is swapped
/// for the new one in one step ([`replace_link`]); any other file is refused and kept.
/// Inputs: the executable and the bin folder. Output: the link.
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
    if let Ok(metadata) = std::fs::symlink_metadata(&link)
        && !metadata.file_type().is_symlink()
    {
        return Err(not_a_link(&link));
    }
    #[cfg(test)]
    BEFORE_PUBLISH.with(|step| {
        if let Some(step) = step.borrow_mut().take() {
            step(&link);
        }
    });
    for _ in 0..PUBLISH_ROUNDS_MAX {
        match std::os::unix::fs::symlink(executable, &link) {
            Ok(()) => return Ok(link),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(fail(&link, error)),
        }
        if replace_link(executable, &link)? {
            return Ok(link);
        }
    }
    Err(not_a_link(&link))
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
/// swap refuses with the staged link removed. Inputs: the executable and the link's name.
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
    drop_staged();
    std::os::unix::fs::symlink(executable, &staged).map_err(|error| fail(&staged, error))?;
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
    run_process(&mut windows_path_command_under(&folder, key))?;
    Ok(folder)
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
/// user variable through .NET then announces the change. Inputs: the folder and an HKCU key,
/// both handed over in the environment so they are read as data, never as script. Output:
/// the command; it fails when either arrives empty.
fn windows_path_command_under(folder: &Path, key: &str) -> std::process::Command {
    assert!(key.starts_with(r"HKCU:\"), "{key}");
    assert!(key.len() > r"HKCU:\".len(), "{key}");
    let script = format!(
        "$ErrorActionPreference='Stop'; \
$f=$env:{WINDOWS_PATH_FOLDER_VARIABLE}; if ([string]::IsNullOrEmpty($f)) {{ exit 2 }}; \
$r=$env:{WINDOWS_PATH_KEY_VARIABLE}; if ([string]::IsNullOrEmpty($r)) {{ exit 2 }}; \
$k=Get-Item -LiteralPath $r; \
$p=''; $t=[Microsoft.Win32.RegistryValueKind]::ExpandString; \
if ($k.GetValueNames() -contains 'Path') {{ \
$p=[string]$k.GetValue('Path','',[Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames); \
$t=$k.GetValueKind('Path') }}; \
if (-not ((($p -split ';') | ForEach-Object {{ [Environment]::ExpandEnvironmentVariables($_) }}) -contains $f)) {{ \
$v=(($p.TrimEnd(';'), $f) -join ';').TrimStart(';'); \
Set-ItemProperty -LiteralPath $r -Name 'Path' -Value $v -Type $t; \
[Environment]::SetEnvironmentVariable('CRAZYAGENT_PATH_REFRESH', $null, 'User') }}"
    );
    assert!(!script.contains("HKCU:"));
    let mut command = std::process::Command::new("powershell.exe");
    command
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .env(WINDOWS_PATH_FOLDER_VARIABLE, folder)
        .env(WINDOWS_PATH_KEY_VARIABLE, key);
    command
}
