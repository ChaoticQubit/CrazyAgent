use super::*;

/// Links the running `crazyagent` into a folder on the user's PATH so a shell finds it:
/// `~/.local/bin/crazyagent` on macOS and Linux. A link already there is replaced; any
/// other file is refused. Inputs: the executable and the bin folder. Output: the link.
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
        return Err(failure(format!(
            "{}: a file that is not a link is already there; move it away first",
            link.display()
        )));
    }
    let staged = bin.join(format!(".crazyagent.link.{}", std::process::id()));
    let _ = std::fs::remove_file(&staged);
    std::os::unix::fs::symlink(executable, &staged).map_err(|error| fail(&staged, error))?;
    std::fs::rename(&staged, &link).map_err(|error| fail(&link, error))?;
    Ok(link)
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
