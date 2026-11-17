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
    run_process(&mut windows_path_command(&folder))?;
    Ok(folder)
}

/// The environment variable that hands the folder to the Windows PATH command. PowerShell
/// joins every argument after `-Command` into the script text, so the folder never goes
/// on the command line.
pub const WINDOWS_PATH_FOLDER_VARIABLE: &str = "CRAZYAGENT_PATH_FOLDER";

/// The PowerShell command that appends a folder to the user's PATH once. It reads the
/// stored value as written, %VAR% references unexpanded, and stores it back with the
/// registry kind it had (REG_EXPAND_SZ for a new value), so references keep following their
/// variables. Deleting a CrazyAgent-only user variable through .NET then announces the
/// change to running programs the way a PATH set through .NET would. Input: the folder,
/// handed over in [`WINDOWS_PATH_FOLDER_VARIABLE`] so it is read as data, never as script.
/// Output: the command; it fails when that variable arrives empty.
pub fn windows_path_command(folder: &Path) -> std::process::Command {
    let script = format!(
        "$ErrorActionPreference='Stop'; \
$f=$env:{WINDOWS_PATH_FOLDER_VARIABLE}; if ([string]::IsNullOrEmpty($f)) {{ exit 2 }}; \
$k=Get-Item -LiteralPath 'HKCU:\\Environment'; \
$p=''; $t=[Microsoft.Win32.RegistryValueKind]::ExpandString; \
if ($k.GetValueNames() -contains 'Path') {{ \
$p=[string]$k.GetValue('Path','',[Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames); \
$t=$k.GetValueKind('Path') }}; \
if (-not ((($p -split ';') | ForEach-Object {{ [Environment]::ExpandEnvironmentVariables($_) }}) -contains $f)) {{ \
$v=(($p.TrimEnd(';'), $f) -join ';').TrimStart(';'); \
Set-ItemProperty -LiteralPath 'HKCU:\\Environment' -Name 'Path' -Value $v -Type $t; \
[Environment]::SetEnvironmentVariable('CRAZYAGENT_PATH_REFRESH', $null, 'User') }}"
    );
    let mut command = std::process::Command::new("powershell.exe");
    command
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .env(WINDOWS_PATH_FOLDER_VARIABLE, folder);
    command
}
