use super::{BridgeError, failure};
use serde::Deserialize;
use std::path::Path;
use std::process::{Command, Output, Stdio};

#[cfg(target_os = "macos")]
const OSASCRIPT: &str = "/usr/bin/osascript";
#[cfg(target_os = "linux")]
const LINUX_BIN_DIRS: &[&str] = &["/usr/bin", "/usr/local/bin", "/run/current-system/sw/bin"];
#[cfg(target_os = "linux")]
const LINUX_CANCELED: i32 = 1;
#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
#[cfg(target_os = "linux")]
const FOLDER_TITLE: &str = "Choose a skill folder. It holds a SKILL.md file.";
#[cfg(target_os = "linux")]
const PROJECT_FOLDER_TITLE: &str = "Choose the project folder to add.";
#[cfg(target_os = "linux")]
const PROJECTS_HOME_TITLE: &str = "Choose the folder that holds your projects.";
#[cfg(target_os = "linux")]
const DOCUMENTS_TITLE: &str = "Choose documents to add";
const REPLY_BYTES_MAX: usize = 64 * 1024;
const PATHS_MAX: usize = 64;
const CANCELED: &str = "(-128)";

/// The macOS folder chooser for a skill folder. Fixed text: nothing the owner or the web view
/// sends is ever placed into a script.
pub const FOLDER_SCRIPT: &str = "POSIX path of (choose folder with prompt \"Choose a skill folder. It holds a SKILL.md file.\")";

/// The macOS folder chooser for an existing project folder to add. Fixed text.
pub const PROJECT_FOLDER_SCRIPT: &str =
    "POSIX path of (choose folder with prompt \"Choose the project folder to add.\")";

/// The macOS folder chooser for the folder that holds the owner's projects. Fixed text.
pub const PROJECTS_HOME_SCRIPT: &str =
    "POSIX path of (choose folder with prompt \"Choose the folder that holds your projects.\")";

/// The macOS file chooser for project documents, one POSIX path per line. Fixed text.
pub const DOCUMENTS_SCRIPT: &str = "set picked to choose file with prompt \"Choose documents to add\" with multiple selections allowed
set reply to \"\"
repeat with chosen in picked
set reply to reply & POSIX path of chosen & linefeed
end repeat
return reply";

/// The Windows folder chooser, a fixed PowerShell script that prints the chosen folder.
/// Nothing the owner or the web view sends is ever placed into it.
pub const WINDOWS_FOLDER_SCRIPT: &str = "[Console]::OutputEncoding = [Text.Encoding]::UTF8; Add-Type -AssemblyName System.Windows.Forms; $d = New-Object System.Windows.Forms.FolderBrowserDialog; $d.Description = 'Choose a skill folder. It holds a SKILL.md file.'; if ($d.ShowDialog() -eq 'OK') { [Console]::Out.Write($d.SelectedPath + [char]10) }";

/// The Windows folder chooser for an existing project folder to add. Fixed text.
pub const WINDOWS_PROJECT_FOLDER_SCRIPT: &str = "[Console]::OutputEncoding = [Text.Encoding]::UTF8; Add-Type -AssemblyName System.Windows.Forms; $d = New-Object System.Windows.Forms.FolderBrowserDialog; $d.Description = 'Choose the project folder to add.'; if ($d.ShowDialog() -eq 'OK') { [Console]::Out.Write($d.SelectedPath + [char]10) }";

/// The Windows folder chooser for the folder that holds the owner's projects. Fixed text.
pub const WINDOWS_PROJECTS_HOME_SCRIPT: &str = "[Console]::OutputEncoding = [Text.Encoding]::UTF8; Add-Type -AssemblyName System.Windows.Forms; $d = New-Object System.Windows.Forms.FolderBrowserDialog; $d.Description = 'Choose the folder that holds your projects.'; if ($d.ShowDialog() -eq 'OK') { [Console]::Out.Write($d.SelectedPath + [char]10) }";

/// The Windows document chooser, a fixed PowerShell script that prints one chosen path per line.
pub const WINDOWS_DOCUMENTS_SCRIPT: &str = "[Console]::OutputEncoding = [Text.Encoding]::UTF8; Add-Type -AssemblyName System.Windows.Forms; $d = New-Object System.Windows.Forms.OpenFileDialog; $d.Title = 'Choose documents to add'; $d.Multiselect = $true; if ($d.ShowDialog() -eq 'OK') { foreach ($f in $d.FileNames) { [Console]::Out.Write($f + [char]10) } }";

/// What a folder chooser is for: a skill folder, an existing project folder to add, or the
/// folder that holds the owner's projects. The web view names one of these; each has its own
/// fixed prompt, so no text from the web view reaches a chooser.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FolderPurpose {
    Skill,
    Project,
    ProjectsHome,
}

impl FolderPurpose {
    /// Every purpose, in the order the choosers are listed.
    pub const ALL: [Self; 3] = [Self::Skill, Self::Project, Self::ProjectsHome];

    /// The fixed macOS script of this purpose's chooser; it also picks the Linux title and the
    /// Windows script. Input: the purpose. Output: one of the folder scripts.
    pub fn script(self) -> &'static str {
        match self {
            Self::Skill => FOLDER_SCRIPT,
            Self::Project => PROJECT_FOLDER_SCRIPT,
            Self::ProjectsHome => PROJECTS_HOME_SCRIPT,
        }
    }
}

/// What the owner did in a native chooser: closed it, or chose absolute paths.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pick {
    Canceled,
    Paths(Vec<String>),
}

/// Reads an osascript chooser reply. Inputs: whether osascript exited successfully, its
/// standard output and standard error. Output: Canceled, the chosen absolute paths in order
/// (folders without a trailing slash), or `picker_failed` for anything unexpected.
pub fn read_reply(success: bool, stdout: &[u8], stderr: &[u8]) -> Result<Pick, BridgeError> {
    if !success {
        let canceled = String::from_utf8_lossy(stderr).contains(CANCELED);
        return if canceled {
            Ok(Pick::Canceled)
        } else {
            Err(failure("picker_failed"))
        };
    }
    if stdout.len() > REPLY_BYTES_MAX {
        return Err(failure("picker_failed"));
    }
    let text = std::str::from_utf8(stdout).map_err(|_| failure("picker_failed"))?;
    let mut paths = Vec::new();
    for line in text.split('\n').filter(|line| !line.is_empty()) {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if !Path::new(line).is_absolute() || line.contains('\0') || paths.len() == PATHS_MAX {
            return Err(failure("picker_failed"));
        }
        let trimmed = line.trim_end_matches(std::path::is_separator);
        paths.push(if Path::new(trimmed).is_absolute() {
            trimmed.to_owned()
        } else {
            line.to_owned()
        });
    }
    assert!(paths.len() <= PATHS_MAX);
    Ok(if paths.is_empty() {
        Pick::Canceled
    } else {
        Pick::Paths(paths)
    })
}

/// The command for one fixed chooser on this system: osascript on macOS, zenity or kdialog
/// from a fixed folder on Linux, PowerShell with Windows Forms on Windows. Input: one of the
/// folder scripts or the documents script. Output: the command, or `picker_unavailable` when
/// this system has no chooser.
pub fn chooser_command(script: &'static str) -> Result<Command, BridgeError> {
    let purpose = FolderPurpose::ALL
        .into_iter()
        .find(|purpose| purpose.script() == script);
    assert!(purpose.is_some() || script == DOCUMENTS_SCRIPT);
    #[cfg(target_os = "macos")]
    {
        let _ = purpose;
        let mut command = Command::new(OSASCRIPT);
        command.arg("-e").arg(script);
        Ok(command)
    }
    #[cfg(target_os = "linux")]
    {
        let tool = |name: &str| {
            LINUX_BIN_DIRS
                .iter()
                .map(|dir| Path::new(dir).join(name))
                .find(|path| path.is_file())
        };
        if let Some(zenity) = tool("zenity") {
            let mut command = Command::new(zenity);
            command.arg("--file-selection");
            match purpose {
                Some(purpose) => {
                    command
                        .arg("--directory")
                        .arg(format!("--title={}", linux_title(purpose)));
                }
                None => {
                    command
                        .args(["--multiple", "--separator=\n"])
                        .arg(format!("--title={DOCUMENTS_TITLE}"));
                }
            }
            return Ok(command);
        }
        if let Some(kdialog) = tool("kdialog") {
            let mut command = Command::new(kdialog);
            if let Some(purpose) = purpose {
                command.args(["--getexistingdirectory", "."]);
                command.args(["--title", linux_title(purpose)]);
            } else {
                command.args(["--getopenfilename", ".", "--multiple", "--separate-output"]);
                command.args(["--title", DOCUMENTS_TITLE]);
            }
            return Ok(command);
        }
        Err(failure("picker_unavailable"))
    }
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        let root = std::env::var_os("SystemRoot")
            .map(std::path::PathBuf::from)
            .filter(|root| root.is_absolute())
            .unwrap_or_else(|| std::path::PathBuf::from(r"C:\Windows"));
        let mut command = Command::new(
            root.join("System32")
                .join("WindowsPowerShell")
                .join("v1.0")
                .join("powershell.exe"),
        );
        command
            .args(["-NoProfile", "-NonInteractive", "-STA", "-Command"])
            .arg(match purpose {
                Some(FolderPurpose::Skill) => WINDOWS_FOLDER_SCRIPT,
                Some(FolderPurpose::Project) => WINDOWS_PROJECT_FOLDER_SCRIPT,
                Some(FolderPurpose::ProjectsHome) => WINDOWS_PROJECTS_HOME_SCRIPT,
                None => WINDOWS_DOCUMENTS_SCRIPT,
            })
            .creation_flags(CREATE_NO_WINDOW);
        Ok(command)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        let _ = purpose;
        Err(failure("picker_unavailable"))
    }
}

#[cfg(target_os = "linux")]
fn linux_title(purpose: FolderPurpose) -> &'static str {
    match purpose {
        FolderPurpose::Skill => FOLDER_TITLE,
        FolderPurpose::Project => PROJECT_FOLDER_TITLE,
        FolderPurpose::ProjectsHome => PROJECTS_HOME_TITLE,
    }
}

fn read_output(output: &Output) -> Result<Pick, BridgeError> {
    #[cfg(target_os = "linux")]
    if output.status.code() == Some(LINUX_CANCELED) && output.stdout.is_empty() {
        return Ok(Pick::Canceled);
    }
    read_reply(output.status.success(), &output.stdout, &output.stderr)
}

/// Shows one fixed chooser and waits for the owner without blocking the async runtime.
/// Input: one of the fixed scripts. Output: the owner's pick, `picker_unavailable` or
/// `picker_failed`.
pub async fn choose(script: &'static str) -> Result<Pick, BridgeError> {
    run_chooser(chooser_command(script)?).await
}

/// Starts one chooser command through the engine's spawn gate, so it inherits no owner socket
/// or pipe another thread is creating, with no input and its output captured, and reads its
/// reply without blocking the async runtime. Input: the chooser command. Output: the owner's
/// pick or `picker_failed`.
pub async fn run_chooser(mut command: Command) -> Result<Pick, BridgeError> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let output = tokio::task::spawn_blocking(move || crazyagent::spawn_gate::output(&mut command))
        .await
        .map_err(|_| failure("picker_failed"))?
        .map_err(|_| failure("picker_failed"))?;
    read_output(&output)
}
