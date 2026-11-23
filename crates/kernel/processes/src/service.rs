use kernel_common::Platform;
use markup::{
    bounded_text, plist_string, systemd_environment_quoted, systemd_quoted, systemd_specifiers,
    systemd_unquoted, windows_quoted, windows_task_path, xml,
};
use sha2::{Digest, Sha256};
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// The launchd user agent label on macOS.
pub const LAUNCHD_LABEL: &str = "dev.crazyagent.engine";
/// The systemd user unit on Linux.
pub const SYSTEMD_UNIT: &str = "crazyagent-engine.service";
/// The logon scheduled task on Windows.
pub const WINDOWS_TASK: &str = "CrazyAgent Engine";
/// How long a client waits for a starting engine to answer.
pub const READY_TIMEOUT: Duration = Duration::from_secs(15);
/// How long a client waits for an engine while its service manager still runs the process
/// it started: a busy machine took close to a minute to bring the owner's engine up.
pub const START_TIMEOUT: Duration = Duration::from_secs(180);

/// The launchd plist keys that lift launchd's default resource limits off the engine: with
/// `ProcessType` unset, launchd throttles a job's CPU and I/O (`man launchd.plist`), and a
/// start on a busy machine took minutes (ENGINE-START-PRE-READY).
const PROCESS_TYPE: &str = "\t<key>ProcessType</key>\n\t<string>Interactive</string>\n";
const LAUNCHCTL: &str = "/bin/launchctl";
/// The systemd manager command a Linux service runs.
pub const SYSTEMCTL: &str = "systemctl";
/// The Task Scheduler command a Windows service runs.
pub const SCHTASKS: &str = "schtasks";
const WINDOWS_SHELL: &str = r"%SystemRoot%\System32\cmd.exe";
/// The most characters cmd reads in one command line, the Windows task's included.
const CMD_LINE_CHARS_MAX: usize = 8191;
const SERVICE_PATH_EXTRAS: [&str; 6] = [
    "/opt/homebrew/bin",
    "/usr/local/bin",
    "/usr/bin",
    "/bin",
    "/usr/sbin",
    "/sbin",
];
/// How often a wait on the service manager or the engine looks again.
pub const POLL: Duration = Duration::from_millis(100);
/// How long launchd may take to let a booted-out job go, past its `ExitTimeOut`.
pub const UNLOAD_TIMEOUT: Duration = Duration::from_secs(35);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceError(pub String);

impl std::fmt::Display for ServiceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ServiceError {}

/// Builds a service failure. Input: the sentence for the owner. Output: the failure.
pub fn failure(text: impl Into<String>) -> ServiceError {
    ServiceError(text.into())
}

/// The engine checks a service step needs and the kernel cannot make itself, passed in by
/// the engine as one static value. `reported_version(root)` is the version a running engine
/// reported under the data root, or none. `prepare_root(root)` creates the data root and its
/// logs folder private to this user, with the default settings files. `private_folder(path)`
/// creates a folder private to this user or refuses one that is not. `is_private_folder(path)`
/// says whether a path is a real folder private to this user. Each refusal reads as the
/// engine's own text.
pub struct Hooks {
    pub reported_version: fn(&Path) -> Option<String>,
    pub prepare_root: fn(&Path) -> Result<(), ServiceError>,
    pub private_folder: fn(&Path) -> Result<(), ServiceError>,
    pub is_private_folder: fn(&Path) -> bool,
}

/// One engine login service: what it runs, with which environment, and where its
/// definition file lives. `name` is the launchd label, the systemd unit or the task name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Service {
    pub platform: Platform,
    pub name: String,
    pub executable: PathBuf,
    pub root: PathBuf,
    pub path: OsString,
    /// The HOME of the process that defined the service. A launchd property list gives it to
    /// the engine, so the engine sees the HOME its CLI or app saw; none when that process had
    /// none, and only macOS writes it.
    pub home: Option<PathBuf>,
    pub definition: PathBuf,
    pub user: Option<String>,
}

/// What a client does with the installed engine service when no engine answers
/// ([`Service::repair`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Repair {
    /// Starts the installed definition as it is.
    Start,
    /// Writes and registers this service's definition in place of what is installed.
    Install,
    /// Neither: this engine is on a disk image, another disk or a temporary folder
    /// ([`transient`]), where a login service never points, and nothing usable is installed
    /// ([`Service::refuse`]).
    Refuse,
    /// Neither, for a forced reinstall: the installed definition runs another program that
    /// still exists and the app's engine may not replace it ([`Service::may_replace`]), as
    /// with a newer engine known at the data root or a dev build, so the definition is kept.
    Keep,
}

/// What a service manager reports about the engine service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    pub installed: bool,
    pub running: bool,
    pub pid: Option<u32>,
}

/// Builds the PATH the service gives the engine for its harness lookup: the absolute entries
/// of the installing shell's PATH, then `~/.local/bin` and the usual system folders, each
/// once, leaving out folders in the AppImage mount `appdir`, ones [`changeable_by_others`]
/// names and, on Windows, missing ones, where a drive added later could plant a harness.
/// Inputs: the current PATH, home folder and mount folder. Output: the joined PATH.
pub fn service_path(
    current: Option<&OsStr>,
    home: Option<&Path>,
    appdir: Option<&Path>,
) -> OsString {
    let appdir = appdir.filter(|folder| folder.is_absolute()).map(canonical);
    let mut folders: Vec<PathBuf> = Vec::new();
    let extras = home
        .map(|home| home.join(".local").join("bin"))
        .into_iter()
        .chain(SERVICE_PATH_EXTRAS.iter().map(PathBuf::from));
    for folder in current
        .map(|path| std::env::split_paths(path).collect::<Vec<_>>())
        .unwrap_or_default()
        .into_iter()
        .chain(extras)
    {
        if folder.is_absolute()
            && !folders.contains(&folder)
            && !appdir
                .as_ref()
                .is_some_and(|appdir| canonical(&folder).starts_with(appdir))
            && (!cfg!(windows) || folder.is_dir())
            && changeable_by_others(&folder).is_none()
        {
            folders.push(folder);
        }
    }
    std::env::join_paths(folders).unwrap_or_default()
}

fn canonical(path: &Path) -> PathBuf {
    let mut missing = Vec::new();
    let mut current = path;
    loop {
        if let Ok(found) = std::fs::canonicalize(current) {
            return missing
                .iter()
                .rev()
                .fold(found, |path, part| path.join(part));
        }
        match (current.parent(), current.file_name()) {
            (Some(parent), Some(part)) => {
                missing.push(part.to_owned());
                current = parent;
            }
            _ => return path.to_path_buf(),
        }
    }
}

/// The login service name for one engine data root: the standard name for the platform's
/// default root, else the standard name with the first 12 hex digits of the SHA-256 of the
/// canonical root, so an engine run with another `CRAZYAGENT_HOME` never shares the default
/// service. Inputs: the platform, the root and the default root when known. Output: the name.
pub fn service_name(platform: Platform, root: &Path, default_root: Option<&Path>) -> String {
    let root = canonical(root);
    if default_root.is_some_and(|default| canonical(default) == root) {
        return match platform {
            Platform::MacOs => LAUNCHD_LABEL,
            Platform::Linux => SYSTEMD_UNIT,
            Platform::Windows => WINDOWS_TASK,
        }
        .to_owned();
    }
    let short: String = Sha256::digest(root.as_os_str().as_encoded_bytes())
        .iter()
        .take(6)
        .map(|byte| format!("{byte:02x}"))
        .collect();
    assert_eq!(short.len(), 12);
    match platform {
        Platform::MacOs => format!("{LAUNCHD_LABEL}.{short}"),
        Platform::Linux => format!(
            "{}-{short}.service",
            SYSTEMD_UNIT.trim_end_matches(".service")
        ),
        Platform::Windows => format!("{WINDOWS_TASK} {short}"),
    }
}

impl Service {
    /// The engine service for the current user on one platform, named for the data root by
    /// [`service_name`]. Inputs: the platform, the absolute executable the service runs, the
    /// engine's data root and the platform's default data root, if any. Output: the service
    /// with its name and definition path.
    pub fn current_user(
        platform: Platform,
        executable: PathBuf,
        root: &Path,
        default_root: Option<&Path>,
    ) -> Result<Self, ServiceError> {
        let home = std::env::var_os("HOME")
            .filter(|home| !home.is_empty())
            .map(PathBuf::from);
        let name = service_name(platform, root, default_root);
        let definition = match platform {
            Platform::MacOs => home
                .as_ref()
                .ok_or_else(|| failure("HOME is not set"))?
                .join("Library")
                .join("LaunchAgents")
                .join(format!("{name}.plist")),
            Platform::Linux => match std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from) {
                Some(config) if config.is_absolute() => config,
                _ => home
                    .as_ref()
                    .ok_or_else(|| failure("HOME is not set"))?
                    .join(".config"),
            }
            .join("systemd")
            .join("user")
            .join(&name),
            Platform::Windows => root.join("crazyagent-engine-task.xml"),
        };
        let user = match (std::env::var("USERDOMAIN"), std::env::var("USERNAME")) {
            (Ok(domain), Ok(user)) if !domain.is_empty() && !user.is_empty() => {
                Some(format!("{domain}\\{user}"))
            }
            _ => None,
        };
        Ok(Self {
            platform,
            name,
            executable,
            root: root.to_path_buf(),
            path: service_path(
                std::env::var_os("PATH").as_deref(),
                home.as_deref(),
                std::env::var_os("APPDIR").as_deref().map(Path::new),
            ),
            home: home.clone(),
            definition,
            user: user.filter(|_| platform == Platform::Windows),
        })
    }

    fn log(&self) -> PathBuf {
        assert!(self.root.is_absolute());
        self.root.join("logs").join("engine.log")
    }

    fn checked(&self) -> Result<(), ServiceError> {
        let control = |text: &str| text.chars().any(char::is_control);
        let required = [
            self.name.clone(),
            self.executable.to_string_lossy().into_owned(),
            self.root.to_string_lossy().into_owned(),
            self.definition.to_string_lossy().into_owned(),
        ];
        if required.iter().any(|text| text.is_empty() || control(text))
            || control(&self.path.to_string_lossy())
            || (self.platform != Platform::Windows && self.path.is_empty())
        {
            return Err(failure(
                "service names and paths must be non-empty without control characters",
            ));
        }
        if self.platform == Platform::Windows
            && let Some(path) = required[1..].iter().find(|text| text.contains(['%', '"']))
        {
            return Err(failure(format!(
                "{path}: the Windows logon task cannot pass a path containing % or \"; \
                 move CrazyAgent or set CRAZYAGENT_HOME to a folder without them"
            )));
        }
        if !self.executable.is_absolute() || !self.root.is_absolute() {
            return Err(failure(
                "the service executable and data folder must be absolute",
            ));
        }
        if self.platform == Platform::MacOs
            && let Some(home) = &self.home
            && (!home.is_absolute() || control(&home.to_string_lossy()))
        {
            return Err(failure(
                "the service HOME must be absolute without control characters",
            ));
        }
        Ok(())
    }

    /// The text of the service definition: a launchd property list, a systemd unit or a
    /// Task Scheduler XML document. It runs `<executable> engine --home <root>` with the
    /// service PATH (and on macOS the defining HOME), appends output to
    /// `<root>/logs/engine.log` (through `cmd.exe` on Windows, whose task has no output
    /// redirection or environment, so cmd sets the PATH there) and restarts after a crash.
    /// A Windows task line past cmd's limit is refused.
    pub fn definition_text(&self) -> Result<String, ServiceError> {
        self.checked()?;
        let executable = self.executable.to_string_lossy();
        let root = self.root.to_string_lossy();
        let log = self.log();
        let log = log.to_string_lossy();
        let text = match self.platform {
            Platform::MacOs => format!(
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
<plist version=\"1.0\">\n\
<dict>\n\
\t<key>Label</key>\n\
\t<string>{label}</string>\n\
\t<key>ProgramArguments</key>\n\
\t<array>\n\
\t\t<string>{executable}</string>\n\
\t\t<string>engine</string>\n\
\t\t<string>--home</string>\n\
\t\t<string>{root}</string>\n\
\t</array>\n\
\t<key>EnvironmentVariables</key>\n\
\t<dict>\n\
\t\t<key>PATH</key>\n\
\t\t<string>{path}</string>\n\
{home}\
\t</dict>\n\
\t<key>RunAtLoad</key>\n\
\t<true/>\n\
\t<key>KeepAlive</key>\n\
\t<dict>\n\
\t\t<key>SuccessfulExit</key>\n\
\t\t<false/>\n\
\t</dict>\n\
\t<key>ExitTimeOut</key>\n\
\t<integer>30</integer>\n\
{PROCESS_TYPE}\
\t<key>StandardOutPath</key>\n\
\t<string>{log}</string>\n\
\t<key>StandardErrorPath</key>\n\
\t<string>{log}</string>\n\
</dict>\n\
</plist>\n",
                label = xml(&self.name),
                executable = self.written_executable(),
                root = xml(&root),
                path = xml(&self.path.to_string_lossy()),
                home = self.home.as_deref().map_or_else(String::new, |home| {
                    format!(
                        "\t\t<key>HOME</key>\n\t\t<string>{}</string>\n",
                        xml(&home.to_string_lossy())
                    )
                }),
                log = xml(&log),
            ),
            Platform::Linux => format!(
                "[Unit]\n\
Description=CrazyAgent engine\n\
\n\
[Service]\n\
ExecStart={executable} engine --home {root}\n\
Environment={path}\n\
Restart=on-failure\n\
RestartSec=5\n\
TimeoutStopSec=30\n\
StandardOutput=append:{log}\n\
StandardError=append:{log}\n\
\n\
[Install]\n\
WantedBy=default.target\n",
                executable = self.written_executable(),
                root = systemd_quoted(&root),
                path = systemd_environment_quoted(&format!("PATH={}", self.path.to_string_lossy())),
                log = systemd_specifiers(&log),
            ),
            Platform::Windows => {
                let trigger_user = self
                    .user
                    .as_deref()
                    .map(|user| format!("<UserId>{}</UserId>", xml(user)))
                    .unwrap_or_default();
                let line = format!(
                    "/d /s /c \"{}{} engine --home {} >> {} 2>&1\"",
                    windows_task_path(&self.path)
                        .map(|path| format!("set \"PATH={path}\" && "))
                        .unwrap_or_default(),
                    windows_quoted(&executable),
                    windows_quoted(&root),
                    windows_quoted(&log),
                );
                if WINDOWS_SHELL.len() + 1 + line.encode_utf16().count() >= CMD_LINE_CHARS_MAX {
                    return Err(failure(format!(
                        "the Windows logon task line is longer than cmd's \
                         {CMD_LINE_CHARS_MAX} characters; move CrazyAgent or set \
                         CRAZYAGENT_HOME to a shorter folder"
                    )));
                }
                format!(
                    "<?xml version=\"1.0\" encoding=\"UTF-16\"?>\r\n\
<Task version=\"1.2\" xmlns=\"http://schemas.microsoft.com/windows/2004/02/mit/task\">\r\n\
  <RegistrationInfo><Description>CrazyAgent engine</Description></RegistrationInfo>\r\n\
  <Triggers><LogonTrigger><Enabled>true</Enabled>{trigger_user}</LogonTrigger></Triggers>\r\n\
  <Principals><Principal id=\"Author\"><LogonType>InteractiveToken</LogonType><RunLevel>LeastPrivilege</RunLevel></Principal></Principals>\r\n\
  <Settings><MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy><DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries><StopIfGoingOnBatteries>false</StopIfGoingOnBatteries><ExecutionTimeLimit>PT0S</ExecutionTimeLimit><RestartOnFailure><Interval>PT1M</Interval><Count>3</Count></RestartOnFailure><Hidden>true</Hidden></Settings>\r\n\
  <Actions Context=\"Author\"><Exec><Command>{WINDOWS_SHELL}</Command><Arguments>{arguments}</Arguments></Exec></Actions>\r\n\
</Task>\r\n",
                    arguments = xml(&line),
                )
            }
        };
        assert!(text.contains("engine"));
        Ok(text)
    }

    fn launchd_domain(uid: u32) -> String {
        format!("gui/{uid}")
    }

    fn launchd_target(&self, uid: u32) -> String {
        format!("gui/{uid}/{}", self.name)
    }

    fn command(program: &str, args: &[&OsStr]) -> Vec<OsString> {
        #[cfg(test)]
        let program = uninstall_tests::program(program);
        std::iter::once(OsString::from(program))
            .chain(args.iter().map(|arg| arg.to_os_string()))
            .collect()
    }

    /// The command that loads the definition into the service manager and starts it at
    /// login and now. Input: the user id (launchd only). Output: program and arguments.
    pub fn register_commands(&self, uid: u32) -> Vec<Vec<OsString>> {
        let definition = self.definition.as_os_str();
        let name = OsStr::new(&self.name);
        match self.platform {
            Platform::MacOs => vec![Self::command(
                LAUNCHCTL,
                &[
                    "bootstrap".as_ref(),
                    Self::launchd_domain(uid).as_ref(),
                    definition,
                ],
            )],
            Platform::Linux => vec![
                Self::command(SYSTEMCTL, &["--user".as_ref(), "daemon-reload".as_ref()]),
                Self::command(
                    SYSTEMCTL,
                    &["--user".as_ref(), "enable".as_ref(), "--now".as_ref(), name],
                ),
            ],
            Platform::Windows => vec![
                Self::command(
                    SCHTASKS,
                    &[
                        "/Create".as_ref(),
                        "/TN".as_ref(),
                        name,
                        "/XML".as_ref(),
                        definition,
                        "/F".as_ref(),
                    ],
                ),
                Self::command(SCHTASKS, &["/Run".as_ref(), "/TN".as_ref(), name]),
            ],
        }
    }

    /// The commands that stop the engine and remove the service from the manager, before
    /// the definition file is deleted. Input: the user id (launchd only).
    pub fn unregister_commands(&self, uid: u32) -> Vec<Vec<OsString>> {
        let name = OsStr::new(&self.name);
        match self.platform {
            Platform::MacOs => vec![Self::command(
                LAUNCHCTL,
                &["bootout".as_ref(), self.launchd_target(uid).as_ref()],
            )],
            Platform::Linux => vec![Self::command(
                SYSTEMCTL,
                &[
                    "--user".as_ref(),
                    "disable".as_ref(),
                    "--now".as_ref(),
                    name,
                ],
            )],
            Platform::Windows => vec![
                Self::command(SCHTASKS, &["/End".as_ref(), "/TN".as_ref(), name]),
                Self::command(
                    SCHTASKS,
                    &["/Delete".as_ref(), "/TN".as_ref(), name, "/F".as_ref()],
                ),
            ],
        }
    }

    /// The command that starts an installed service. Inputs: the user id and whether
    /// launchd has the job loaded. Output: kickstart for a loaded job, bootstrap otherwise.
    pub fn start_command(&self, uid: u32, loaded: bool) -> Vec<OsString> {
        let name = OsStr::new(&self.name);
        match self.platform {
            Platform::MacOs if loaded => Self::command(
                LAUNCHCTL,
                &["kickstart".as_ref(), self.launchd_target(uid).as_ref()],
            ),
            Platform::MacOs => self.register_commands(uid).remove(0),
            Platform::Linux => {
                Self::command(SYSTEMCTL, &["--user".as_ref(), "start".as_ref(), name])
            }
            Platform::Windows => Self::command(SCHTASKS, &["/Run".as_ref(), "/TN".as_ref(), name]),
        }
    }

    /// The command that stops the engine and keeps the service installed. launchd unloads
    /// the job so `KeepAlive` cannot restart it; it loads again at the next login.
    pub fn stop_command(&self, uid: u32) -> Vec<OsString> {
        let name = OsStr::new(&self.name);
        match self.platform {
            Platform::MacOs => self.unregister_commands(uid).remove(0),
            Platform::Linux => {
                Self::command(SYSTEMCTL, &["--user".as_ref(), "stop".as_ref(), name])
            }
            Platform::Windows => Self::command(SCHTASKS, &["/End".as_ref(), "/TN".as_ref(), name]),
        }
    }

    /// The command whose output says whether the engine runs.
    pub fn status_command(&self, uid: u32) -> Vec<OsString> {
        let name = OsStr::new(&self.name);
        match self.platform {
            Platform::MacOs => Self::command(
                LAUNCHCTL,
                &["print".as_ref(), self.launchd_target(uid).as_ref()],
            ),
            Platform::Linux => {
                Self::command(SYSTEMCTL, &["--user".as_ref(), "is-active".as_ref(), name])
            }
            Platform::Windows => Self::command(
                SCHTASKS,
                &[
                    "/Query".as_ref(),
                    "/TN".as_ref(),
                    name,
                    "/FO".as_ref(),
                    "LIST".as_ref(),
                ],
            ),
        }
    }

    /// Reads the manager's status output. Inputs: whether the status command succeeded and
    /// its standard output. Output: (running, pid).
    pub fn parse_status(&self, succeeded: bool, output: &str) -> (bool, Option<u32>) {
        if !succeeded {
            return (false, None);
        }
        match self.platform {
            Platform::MacOs => {
                let field = |name: &str| {
                    output.lines().find_map(|line| {
                        line.trim()
                            .strip_prefix(name)
                            .and_then(|rest| rest.trim_start().strip_prefix('='))
                            .map(str::trim)
                    })
                };
                let pid = field("pid").and_then(|pid| pid.parse().ok());
                (field("state") == Some("running"), pid)
            }
            Platform::Linux => (output.trim() == "active", None),
            Platform::Windows => (
                output.lines().any(|line| {
                    line.split_once(':').is_some_and(|(key, value)| {
                        key.trim() == "Status" && value.trim() == "Running"
                    })
                }),
                None,
            ),
        }
    }

    /// Asks the service manager about the engine. Output: whether the definition is
    /// installed, whether the engine runs, and its process id when the manager says.
    pub fn status(&self) -> Result<Status, ServiceError> {
        let installed = self.definition.is_file();
        let (succeeded, output) = run_quiet(&self.status_command(current_uid()))?;
        let (running, pid) = self.parse_status(succeeded, &output);
        Ok(Status {
            installed,
            running,
            pid,
        })
    }

    /// Whether the installed definition file holds exactly what [`Service::install`] would
    /// write now, so a caller can replace one that runs another executable, such as an
    /// AppImage mount folder that is gone. Output: false when it is missing, unreadable,
    /// different or this service cannot be defined.
    pub fn current(&self) -> bool {
        self.definition_text().is_ok_and(|text| {
            std::fs::read(&self.definition)
                .is_ok_and(|bytes| bytes == definition_bytes(&text, self.platform))
        })
    }

    /// Whether a plain start must replace the installed definition instead of starting it:
    /// only when this service runs a private copy in place of the `bundled` engine and the
    /// file on disk is not [`Service::current`], as with a unit naming the copy with another
    /// PATH. A definition naming a gone or other program is decided before this in
    /// [`Service::repair`]. Any other installed service starts as it is unless it is
    /// [`Service::throttled`], so a launch PATH that differs never stops or restarts a
    /// running engine. Input: the bundled engine.
    pub fn stale(&self, bundled: &Path) -> bool {
        self.executable != bundled && !self.current()
    }

    /// Whether the installed definition is a launchd plist without the `ProcessType`
    /// Interactive key, as every install before 2.4.2 wrote, so launchd throttles the
    /// engine's CPU and I/O until it is rewritten. Output: false off macOS and when no
    /// readable definition is installed.
    pub fn throttled(&self) -> bool {
        self.platform == Platform::MacOs
            && bounded_text(&self.definition)
                .is_some_and(|installed| !installed.contains(PROCESS_TYPE))
    }

    /// The program the installed definition runs when it is gone or [`transient`], as after
    /// the app was moved or renamed, or first opened from a disk image or a Gatekeeper
    /// AppTranslocation folder: a definition naming it can never be started as it is.
    /// Output: none when nothing readable is installed or the program it names is usable.
    pub fn broken_program(&self) -> Option<PathBuf> {
        self.installed_program()
            .filter(|program| transient(program) || !program.is_file())
    }

    /// The program the installed definition runs, read back as it was written: the first
    /// `ProgramArguments` string of a plist or the first word of a systemd `ExecStart=`.
    /// Output: none when nothing readable is installed, the program is not an absolute path,
    /// or on Windows, whose task runs the program through `cmd.exe` and is UTF-16.
    pub fn installed_program(&self) -> Option<PathBuf> {
        let installed = bounded_text(&self.definition)?;
        let program = match self.platform {
            Platform::MacOs => installed
                .lines()
                .skip_while(|line| !line.contains("<key>ProgramArguments</key>"))
                .find_map(plist_string)?,
            Platform::Linux => systemd_unquoted(
                installed
                    .lines()
                    .find_map(|line| line.strip_prefix("ExecStart="))?,
            )?,
            Platform::Windows => return None,
        };
        Some(PathBuf::from(program)).filter(|program| program.is_absolute())
    }

    /// What to do with the engine service when no engine answers. Another program the
    /// installed definition runs that still exists and the `bundled` engine may not replace
    /// ([`Self::may_replace`], judged on the app's own engine and not on an AppImage's private
    /// copy) is kept on a forced reinstall and started as it is otherwise (RV-DS241-129, DS-118
    /// r2 F1). So is any usable definition while this service runs such a private copy, one
    /// path every AppImage shares, which keeps a newer engine's bytes while one is known there
    /// ([`newer_engine_at`], [`persistable`], DS-118 r3 F1). Else this definition is installed
    /// when asked, when none is installed, when the installed one names a broken program
    /// ([`Self::broken_program`]) or another program, or when it is [`Self::stale`] or
    /// [`Self::throttled`]; otherwise the installed one starts as it is. An engine that is
    /// itself [`transient`] is never installed: it starts a usable installed definition and
    /// is refused otherwise. Inputs:
    /// the bundled engine, whether a definition is installed and whether the owner asked for a
    /// reinstall, and the engine's checks. Output: the repair.
    pub fn repair(
        &self,
        bundled: &Path,
        installed: bool,
        reinstall: bool,
        hooks: &Hooks,
    ) -> Repair {
        let broken = installed && self.broken_program().is_some();
        let another = installed && !broken && self.runs_another_program();
        let newer_copy = installed
            && !broken
            && self.executable != bundled
            && newer_engine_at(&self.root, hooks);
        let replaceable = !another
            || Service {
                executable: bundled.to_path_buf(),
                ..self.clone()
            }
            .may_replace(hooks);
        if newer_copy || !replaceable {
            return if reinstall {
                Repair::Keep
            } else {
                Repair::Start
            };
        }
        let replace =
            reinstall || !installed || broken || another || self.stale(bundled) || self.throttled();
        if !replace {
            return Repair::Start;
        }
        if !transient(&self.executable) {
            return Repair::Install;
        }
        if installed && !broken && !reinstall {
            Repair::Start
        } else {
            Repair::Refuse
        }
    }

    /// The refusal of [`Repair::Refuse`], after removing an installed definition that names a
    /// broken program ([`Self::broken_program`]), so launchd or systemd no longer runs a gone,
    /// /Volumes or translocated path at login or on a restart; a usable definition stays.
    /// Output: the error saying to move the app to Applications, with any removal failure.
    pub fn refuse(&self) -> ServiceError {
        let refused = transient_refused(&self.executable);
        if self.broken_program().is_none() {
            return refused;
        }
        match self.uninstall() {
            Ok(()) => refused,
            Err(error) => failure(format!(
                "{}; the login service naming a gone or temporary path could not be removed: {}",
                refused.0, error.0
            )),
        }
    }

    /// The executable as [`Self::definition_text`] writes it on this platform: XML-escaped in
    /// a plist, quoted for systemd, quoted for cmd and then XML-escaped in a Windows task.
    /// Output: the text that names the executable in the definition.
    fn written_executable(&self) -> String {
        let executable = self.executable.to_string_lossy();
        match self.platform {
            Platform::MacOs => xml(&executable),
            Platform::Linux => systemd_quoted(&executable),
            Platform::Windows => xml(&windows_quoted(&executable)),
        }
    }

    /// Whether the installed definition runs another program than this service's engine, as
    /// an install script's layout or an older app location does: the line of
    /// [`Self::definition_text`] naming the executable, as written there, is not in the file.
    /// A different PATH or setting does not count. Output: false when nothing readable is
    /// installed.
    pub fn runs_another_program(&self) -> bool {
        let Ok(text) = self.definition_text() else {
            return false;
        };
        let written = self.written_executable();
        let own = text.lines().find(|line| line.contains(written.as_str()));
        assert!(own.is_some(), "a definition names its executable");
        std::fs::read_to_string(&self.definition)
            .is_ok_and(|installed| !installed.lines().any(|line| Some(line) == own))
    }

    /// Whether `service start` may replace an installed definition that runs another program
    /// with this service's engine: the engine is an installed desktop app's (an app bundle, a
    /// mounted AppImage or a Windows install folder) outside any cargo target folder, and no
    /// engine known at this data root is newer, neither the last one that ran here nor the app
    /// bundle the installed plist runs. Input: the engine's checks. Output: the answer; false
    /// for a dev or worktree build.
    pub fn may_replace(&self, hooks: &Hooks) -> bool {
        let own = canonical(&self.executable);
        let built = own
            .ancestors()
            .any(|folder| folder.join("CACHEDIR.TAG").is_file());
        if built || !crate::update::inside_app_bundle(&own) {
            return false;
        }
        [
            (hooks.reported_version)(&self.root),
            self.installed_bundle_version(),
        ]
        .into_iter()
        .flatten()
        .all(|known| !newer_than_this(&known))
    }

    /// The version of the app bundle the installed plist runs, from that bundle's Info.plist
    /// read as text. Output: none when either file is not plain text, is larger than its cap,
    /// or the plist names no app bundle.
    fn installed_bundle_version(&self) -> Option<String> {
        let program = self.installed_program()?;
        let bundle = program.ancestors().find(|folder| {
            folder
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("app"))
        })?;
        let info = bounded_text(&bundle.join("Contents").join("Info.plist"))?;
        info.lines()
            .skip_while(|line| !line.contains("<key>CFBundleShortVersionString</key>"))
            .nth(1)
            .and_then(plist_string)
    }

    fn loaded(&self) -> Result<bool, ServiceError> {
        Ok(run_quiet(&self.status_command(current_uid()))?.0)
    }

    /// Removes the job from launchd and waits until launchd lets it go. `bootout` returns
    /// while the engine is still shutting down, and a job in that state can be neither
    /// bootstrapped nor kickstarted. Output: an error when launchd still has it after the
    /// bound, which outlasts the definition's `ExitTimeOut`.
    fn launchd_unload(&self) -> Result<(), ServiceError> {
        assert_eq!(self.platform, Platform::MacOs);
        run_checked(&self.unregister_commands(current_uid())[0])?;
        let deadline = Instant::now() + UNLOAD_TIMEOUT;
        while self.loaded()? {
            if Instant::now() >= deadline {
                return Err(failure(format!(
                    "launchd still runs {} {} s after it was asked to stop it",
                    self.name,
                    UNLOAD_TIMEOUT.as_secs()
                )));
            }
            std::thread::sleep(POLL);
        }
        Ok(())
    }

    /// Writes the definition and registers it, replacing an earlier install of the same
    /// name, so the engine starts now and at every login. Input: the engine's checks.
    /// Output: an error naming the failing step.
    pub fn install(&self, hooks: &Hooks) -> Result<(), ServiceError> {
        if transient(&self.executable) {
            return Err(transient_refused(&self.executable));
        }
        if !self.executable.is_file() {
            return Err(failure(format!(
                "{}: the engine executable is missing",
                self.executable.display()
            )));
        }
        self.checked()?;
        persisted(&self.executable)?;
        (hooks.prepare_root)(&self.root)?;
        let text = self.definition_text()?;
        if self.platform == Platform::MacOs && self.loaded()? {
            self.launchd_unload()?;
        }
        write_definition(&self.definition, &text, self.platform)?;
        for command in self.register_commands(current_uid()) {
            run_checked(&command)?;
        }
        Ok(())
    }

    /// Stops the engine, removes the service from the manager and deletes the definition.
    /// A service that is not installed is left as it is. A Windows task still registered
    /// after its deletion is an error, and its definition is kept for a retry.
    pub fn uninstall(&self) -> Result<(), ServiceError> {
        let loaded = self.loaded()?;
        let installed = self.definition.is_file();
        if !loaded && !installed {
            return Ok(());
        }
        if self.platform == Platform::MacOs {
            if loaded {
                self.launchd_unload()?;
            }
        } else {
            let mut refused = None;
            for command in self.unregister_commands(current_uid()) {
                if let Err(error) = run_checked(&command) {
                    refused = Some(error.0);
                }
            }
            if self.platform == Platform::Windows && self.loaded()? {
                return Err(failure(format!(
                    "the engine task {} is still registered, so its definition is kept for a \
                     retry: {}",
                    self.name,
                    refused.unwrap_or_else(|| "no step reported an error".to_owned())
                )));
            }
        }
        match std::fs::remove_file(&self.definition) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(failure(format!("{}: {error}", self.definition.display())));
            }
        }
        if self.platform == Platform::Linux && installed {
            run_checked(&Self::command(
                SYSTEMCTL,
                &["--user".as_ref(), "daemon-reload".as_ref()],
            ))?;
        }
        Ok(())
    }

    /// Starts the installed engine service. Output: an error when it is not installed, or
    /// when it names a program that is gone or [`transient`] ([`Self::broken_program`]),
    /// which is never started as it is.
    pub fn start(&self) -> Result<(), ServiceError> {
        if !self.definition.is_file() {
            return Err(failure(
                "the engine service is not installed; run: crazyagent service install",
            ));
        }
        if let Some(program) = self.broken_program() {
            return Err(failure(format!(
                "the engine service runs {}, which is gone or on a disk image, another disk or \
                 a temporary folder; reinstall it: crazyagent service install",
                program.display()
            )));
        }
        let loaded = self.platform == Platform::MacOs && self.loaded()?;
        run_checked(&self.start_command(current_uid(), loaded))
    }

    /// Stops the engine and keeps the service installed. A stopped engine, and a service
    /// that is neither installed nor known to the manager, are left as they are.
    pub fn stop(&self) -> Result<(), ServiceError> {
        if self.platform == Platform::MacOs {
            return if self.loaded()? {
                self.launchd_unload()
            } else {
                Ok(())
            };
        }
        if !self.definition.is_file() && !self.loaded()? {
            return Ok(());
        }
        run_checked(&self.stop_command(current_uid()))
    }
}

/// Whether a path lies on a disk image or another disk (`/Volumes/...`) or in a Gatekeeper
/// AppTranslocation folder, which go away when the image is ejected, the disk is removed or
/// the app is moved, and which another disk of the same name could later take the place of.
/// Read as written, without touching the file system. Input: the path. Output: the answer.
pub fn transient(path: &Path) -> bool {
    path.starts_with("/Volumes")
        || path
            .components()
            .any(|part| part.as_os_str() == "AppTranslocation")
}

/// The refusal for a login service that would run an engine on a disk image, another disk
/// or a temporary folder ([`transient`]). Input: the engine. Output: the error saying to
/// move the app to Applications first.
pub fn transient_refused(executable: &Path) -> ServiceError {
    failure(format!(
        "{}: a login service never runs the engine from a disk image, another disk or a \
         temporary folder; move CrazyAgent to Applications, then open it again",
        executable.display()
    ))
}

/// Refuses an engine that a login service or the command line link would keep running
/// after this app exits when another user can change its path ([`changeable_by_others`]).
/// Input: the executable. Output: Ok, or an error naming the entry and the fix.
fn persisted(executable: &Path) -> Result<(), ServiceError> {
    match changeable_by_others(executable) {
        Some(entry) => Err(failure(format!(
            "{}: other users can change {}, so neither a login service nor the command line \
             link may run the engine from there; install CrazyAgent in a folder only you and \
             the system can change",
            executable.display(),
            entry.display()
        ))),
        None => Ok(()),
    }
}

/// The engine executable a login service or the command line link may keep naming: the
/// bundled `executable` itself, unless it sits inside the running AppImage's mount folder
/// `appdir`, which goes away when the app exits. Then it is a copy, private to this user,
/// of the release executables beside it ([`crate::update::BINARIES`]) in
/// `<data root>/setup/bin`, which the tool resolver trusts so the bundled tmux there is used,
/// one stable path across versions, each file refreshed by an atomic rename
/// whenever it differs: every changed file is staged first, a staging failure removes what
/// was staged and changes nothing, then the version file is raised, then the staged files
/// are renamed in, so a stop leaves a version file never older than the copies. The copy an older version kept in `<data root>/bin` stays, since a
/// login service may still name it; `service start` retires it after the service names this
/// one ([`retire_old_copies`]). The version written goes in `<data root>/setup/bin-version`,
/// and while a newer engine is known there ([`newer_engine_at`]) this user's private copy is
/// kept as it is, so an older AppImage never puts its bytes over the engine every AppImage
/// runs there (DS-118 r3 F1). Nothing is ever copied from another folder, so an engine other
/// users can change is returned as it is and refused where it would be kept. Inputs: the
/// bundled engine, the data root, the AppImage mount folder when there is one and the engine's
/// checks. Output: the executable to keep.
pub fn persistable(
    executable: &Path,
    root: &Path,
    appdir: Option<&Path>,
    hooks: &Hooks,
) -> Result<PathBuf, ServiceError> {
    let mounted = appdir
        .filter(|folder| folder.is_absolute())
        .is_some_and(|folder| canonical(executable).starts_with(canonical(folder)));
    if !mounted || !executable.is_file() {
        return Ok(executable.to_path_buf());
    }
    let (Some(source), Some(main)) = (executable.parent(), executable.file_name()) else {
        return Err(failure("the engine executable has no folder"));
    };
    let names: Vec<String> = crate::update::BINARIES
        .iter()
        .map(|name| format!("{name}{}", std::env::consts::EXE_SUFFIX))
        .filter(|name| source.join(name).is_file())
        .collect();
    if !names.iter().any(|name| OsStr::new(name) == main) {
        return Err(failure(format!(
            "{}: not a CrazyAgent engine executable",
            executable.display()
        )));
    }
    (hooks.prepare_root)(root)?;
    let copies = root.join("setup").join("bin");
    (hooks.private_folder)(&copies)?;
    let installed = copies.join(main);
    if newer_engine_at(root, hooks) && private_file(&installed) {
        return Ok(installed);
    }
    let unstage = |staged: &[(PathBuf, PathBuf)]| {
        for (file, _) in staged {
            let _ = std::fs::remove_file(file);
        }
    };
    let mut staged: Vec<(PathBuf, PathBuf)> = Vec::new();
    for name in &names {
        let copy = copies.join(name);
        if kept(&source.join(name), &copy) {
            continue;
        }
        let file = copies.join(format!(".{name}.{}", std::process::id()));
        let made = std::fs::symlink_metadata(&file)
            .map_or(Ok(()), |_| std::fs::remove_file(&file))
            .and_then(|()| copy_private(&source.join(name), &file));
        if let Err(error) = made {
            let _ = std::fs::remove_file(&file);
            unstage(&staged);
            return Err(failure(format!("{}: {error}", copy.display())));
        }
        staged.push((file, copy));
    }
    let recorded = copy_version_file(root);
    if bounded_text(&recorded).as_deref().map(str::trim) != Some(crate::version::VERSION) {
        let fail = |error: std::io::Error| failure(format!("{}: {error}", recorded.display()));
        let marker = recorded.with_file_name(format!(".bin-version.{}", std::process::id()));
        let written = std::fs::symlink_metadata(&marker)
            .map_or(Ok(()), |_| std::fs::remove_file(&marker))
            .and_then(|()| std::fs::write(&marker, crate::version::VERSION))
            .and_then(|()| std::fs::rename(&marker, &recorded));
        if let Err(error) = written {
            unstage(&staged);
            return Err(fail(error));
        }
    }
    for (index, (file, copy)) in staged.iter().enumerate() {
        if let Err(error) = std::fs::rename(file, copy) {
            unstage(&staged[index..]);
            return Err(failure(format!("{}: {error}", copy.display())));
        }
    }
    assert!(installed.starts_with(&copies) && installed.is_file());
    Ok(installed)
}

/// The file in which [`persistable`] records the version of the engine it last put in
/// `<root>/setup/bin`. Input: the data root. Output: the path.
fn copy_version_file(root: &Path) -> PathBuf {
    root.join("setup").join("bin-version")
}

/// Whether an engine newer than this one is known at the data root `root`: the one whose
/// locator is there while it runs ([`Hooks::reported_version`]), or
/// the one whose bytes [`persistable`] last put in `<root>/setup/bin`, as its version file says
/// after it stopped. A version file that holds no version counts for nothing. Inputs: the data
/// root and the engine's checks. Output: the answer.
fn newer_engine_at(root: &Path, hooks: &Hooks) -> bool {
    let recorded = bounded_text(&copy_version_file(root))
        .map(|text| text.trim().to_owned())
        .filter(|known| crate::update::is_newer(known, crate::version::VERSION).is_ok());
    [(hooks.reported_version)(root), recorded]
        .into_iter()
        .flatten()
        .any(|known| newer_than_this(&known))
}

/// Whether a known engine version is newer than this engine, or cannot be read as a version,
/// so this engine never takes its place. Input: the known version. Output: the answer.
fn newer_than_this(known: &str) -> bool {
    !crate::update::is_newer(known, crate::version::VERSION).is_ok_and(|newer| !newer)
}

/// Retires the copy an older version kept in `<data root>/bin` once the login service names
/// the engine [`persistable`] keeps in `<data root>/setup/bin`, with the command line link in
/// `~/.local/bin`. Inputs: the data root, the engine the service now runs and the engine's
/// checks.
pub fn retire_old_copies(root: &Path, installed: &Path, hooks: &Hooks) {
    let cli = std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map(|home| PathBuf::from(home).join(".local").join("bin"));
    retire_old_copies_with(root, installed, cli.as_deref(), hooks);
}

/// Retires the copy an older version kept in `<data root>/bin`, a folder the tool resolver
/// refuses as one agents can write, when `installed` is the copy in `<data root>/setup/bin`:
/// the command line link in `cli` that names the old engine is pointed at the new one first,
/// then every release file there is removed and the folder goes once empty. A link that
/// cannot be moved keeps the old copy, so it never dangles; anything CrazyAgent did not put
/// there stays. Only a real, private folder of this user's (700, as 2.4.0 made it) is
/// touched, so a linked or shared `bin` never leads a removal outside the data root (rv-code
/// F1). Inputs: the data root, the engine kept, the link folder and the engine's checks.
pub fn retire_old_copies_with(root: &Path, installed: &Path, cli: Option<&Path>, hooks: &Hooks) {
    let old = &root.join("bin");
    if !installed.starts_with(root.join("setup").join("bin")) || !(hooks.is_private_folder)(old) {
        return;
    }
    assert!(installed.is_absolute() && !installed.starts_with(old));
    let main = format!("crazyagent{}", std::env::consts::EXE_SUFFIX);
    if let Some(cli) = cli
        && std::fs::read_link(cli.join("crazyagent")).is_ok_and(|named| named == old.join(&main))
        && link_cli(installed, cli).is_err()
    {
        return;
    }
    for name in crate::update::BINARIES {
        let file = old.join(format!("{name}{}", std::env::consts::EXE_SUFFIX));
        if std::fs::symlink_metadata(&file).is_ok_and(|metadata| metadata.is_file()) {
            let _ = std::fs::remove_file(&file);
        }
    }
    let _ = std::fs::remove_dir(old);
}

/// Whether `copy` is a regular file private to this user where Unix modes exist, so no other
/// user has put or changed its bytes. Input: the copy. Output: the answer.
fn private_file(copy: &Path) -> bool {
    std::fs::symlink_metadata(copy).is_ok_and(|metadata| {
        #[cfg(unix)]
        let private = {
            use std::os::unix::fs::MetadataExt;
            metadata.uid() == current_uid() && metadata.mode() & 0o777 == 0o700
        };
        #[cfg(not(unix))]
        let private = true;
        metadata.is_file() && private
    })
}

/// Whether `copy` is a regular file with exactly the bytes of `source`, private to this
/// user where Unix modes exist. Inputs: the bundled file and its copy. Output: the answer.
fn kept(source: &Path, copy: &Path) -> bool {
    private_file(copy)
        && std::fs::metadata(source)
            .map(|metadata| metadata.len())
            .ok()
            == std::fs::metadata(copy).map(|metadata| metadata.len()).ok()
        && match (std::fs::read(source), std::fs::read(copy)) {
            (Ok(bundled), Ok(copied)) => bundled == copied,
            _ => false,
        }
}

/// Copies one file into a new file only this user can read, write or run (0700 on Unix).
/// Inputs: the source and the new path, which must not exist. Output: Ok once synced.
fn copy_private(source: &Path, copy: &Path) -> std::io::Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o700);
    let mut file = options.open(copy)?;
    std::io::copy(&mut std::fs::File::open(source)?, &mut file)?;
    file.sync_all()
}

#[cfg(unix)]
fn current_uid() -> u32 {
    rustix::process::getuid().as_raw()
}

/// The macOS admin group, whose members can already act as root.
#[cfg(target_os = "macos")]
const ADMIN_GROUP: u32 = 80;

/// The macOS daemon group, whose only member is root; it owns /private/var/run, where
/// nix-darwin's /run link leads (RV-DS241-140 r4).
#[cfg(target_os = "macos")]
const DAEMON_GROUP: u32 = 1;

/// Whether a group that can write a folder holds only users who may already act as root:
/// wheel, daemon and admin. Every macOS user is in staff, so a staff folder is not trusted.
/// Input: the group id. Output: the answer.
#[cfg(target_os = "macos")]
fn trusted_group(group: u32) -> bool {
    group == 0 || group == DAEMON_GROUP || group == ADMIN_GROUP
}

/// Whether a group that can write a folder holds only root or this user: root's group, or
/// this process's own group when it is this user's private group ([`own_group_private`]);
/// a shared primary group, whose other users can write the folder, is not trusted.
/// Input: the group id. Output: the answer.
#[cfg(all(unix, not(target_os = "macos")))]
fn trusted_group(group: u32) -> bool {
    group == 0 || (group == rustix::process::getegid().as_raw() && own_group_private())
}

/// The largest local account file read to find a group's primary accounts.
#[cfg(all(unix, not(target_os = "macos")))]
const ACCOUNTS_BYTES_MAX: u64 = 4 * 1024 * 1024;

/// Whether this process's own group is this user's private group ([`private_group`]), read
/// once through the system's user and group lookup, which may ask a directory server, and
/// the local account file for the accounts whose primary group it is ([`primary_users`]),
/// trusted only when no directory can add accounts it lacks ([`accounts_complete`]); a
/// failed lookup or read counts as not private, so its folders fall to the stricter rule.
/// Output: the answer.
#[cfg(all(unix, not(target_os = "macos")))]
pub fn own_group_private() -> bool {
    static PRIVATE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *PRIVATE.get_or_init(|| {
        let gid = nix::unistd::getegid();
        let user = nix::unistd::User::from_uid(nix::unistd::getuid())
            .ok()
            .flatten();
        let group = nix::unistd::Group::from_gid(gid).ok().flatten();
        let primary = local_accounts()
            .filter(|_| accounts_complete(Path::new("/etc")))
            .and_then(|text| primary_users(&text, gid.as_raw()));
        match (user, group, primary) {
            (Some(user), Some(group), Some(primary)) => {
                private_group(&group.name, &group.mem, &primary, &user.name)
            }
            _ => false,
        }
    })
}

/// Reads the local account file, `/etc/passwd`, at most [`ACCOUNTS_BYTES_MAX`]; a larger or
/// unreadable file gives none. Output: the text, or none.
#[cfg(all(unix, not(target_os = "macos")))]
fn local_accounts() -> Option<String> {
    use std::io::Read as _;
    let mut text = String::new();
    std::fs::File::open("/etc/passwd")
        .ok()?
        .take(ACCOUNTS_BYTES_MAX + 1)
        .read_to_string(&mut text)
        .ok()?;
    (text.len() as u64 <= ACCOUNTS_BYTES_MAX).then_some(text)
}

/// The largest user lookup configuration read to find where accounts come from.
#[cfg(any(test, all(unix, not(target_os = "macos"))))]
const LOOKUP_CONFIG_BYTES_MAX: u64 = 64 * 1024;

/// The most entries read from SSSD's drop-in folder before it counts as configured.
#[cfg(any(test, all(unix, not(target_os = "macos"))))]
const SSSD_DROP_INS_MAX: usize = 4096;

/// Whether the local account file holds every account the system's user lookup can give a
/// primary group: every `passwd` source in `nsswitch.conf` is `files`, `compat` or
/// `systemd`, or `sss` while SSSD has no configuration ([`sssd_unconfigured`]). Action
/// items and comments are skipped; a missing or unreadable file, a larger one or no
/// `passwd` line gives no. Input: the folder holding `nsswitch.conf` and `sssd`, `/etc`
/// outside rows. Output: the answer.
#[cfg(any(test, all(unix, not(target_os = "macos"))))]
fn accounts_complete(etc: &Path) -> bool {
    use std::io::Read as _;
    let mut text = String::new();
    let read = std::fs::File::open(etc.join("nsswitch.conf")).and_then(|file| {
        file.take(LOOKUP_CONFIG_BYTES_MAX + 1)
            .read_to_string(&mut text)
    });
    if read.is_err() || text.len() as u64 > LOOKUP_CONFIG_BYTES_MAX {
        return false;
    }
    let mut lines = 0;
    for line in text.lines() {
        let line = line.split('#').next().unwrap_or_default();
        let Some(sources) = line.trim_start().strip_prefix("passwd:") else {
            continue;
        };
        lines += 1;
        let mut in_action = false;
        for word in sources.split_whitespace() {
            if in_action || word.starts_with('[') {
                in_action = !word.ends_with(']');
                continue;
            }
            let local = match word {
                "files" | "compat" | "systemd" => true,
                "sss" => sssd_unconfigured(&etc.join("sssd")),
                _ => false,
            };
            if !local {
                return false;
            }
        }
    }
    assert!(lines <= text.len());
    lines > 0
}

/// Whether SSSD has no configuration, so it serves no directory: `sssd.conf` is missing and
/// the `conf.d` drop-in folder is missing or holds no `.conf` entry. Any other answer, such
/// as a folder this user cannot look into, counts as configured. Input: the SSSD folder.
/// Output: the answer.
#[cfg(any(test, all(unix, not(target_os = "macos"))))]
fn sssd_unconfigured(sssd: &Path) -> bool {
    let missing = |error: &std::io::Error| error.kind() == std::io::ErrorKind::NotFound;
    match std::fs::metadata(sssd.join("sssd.conf")) {
        Err(error) if missing(&error) => {}
        _ => return false,
    }
    let entries = match std::fs::read_dir(sssd.join("conf.d")) {
        Ok(entries) => entries,
        Err(error) => return missing(&error),
    };
    for (seen, entry) in entries.enumerate() {
        if seen == SSSD_DROP_INS_MAX {
            return false;
        }
        let Ok(entry) = entry else {
            return false;
        };
        if entry.file_name().as_encoded_bytes().ends_with(b".conf") {
            return false;
        }
    }
    true
}

/// Whether a group named `name`, listing `members`, is the user private group of the user
/// named `user`: it bears the user's name, lists no other member and is the primary group of
/// this user's account alone, as Debian, Ubuntu and Fedora make one per user; a shared
/// primary group such as openSUSE's `users` is not, and neither is one whose user the local
/// account file does not list. Inputs: the group's name, its listed members, the accounts
/// whose primary group it is and the user's name. Output: the answer.
#[cfg(any(test, all(unix, not(target_os = "macos"))))]
fn private_group(name: &str, members: &[String], primary: &[String], user: &str) -> bool {
    !user.is_empty()
        && name == user
        && members.iter().all(|member| member == user)
        && primary.iter().any(|account| account == user)
        && primary.iter().all(|account| account == user)
}

/// The accounts in a local account file (`/etc/passwd` lines) whose primary group is `gid`,
/// which the group's member list leaves out. Inputs: the file's text and the group id.
/// Output: the account names, or none when a line cannot be read or names accounts kept
/// elsewhere.
#[cfg(any(test, all(unix, not(target_os = "macos"))))]
fn primary_users(passwd: &str, gid: u32) -> Option<Vec<String>> {
    let mut accounts = Vec::new();
    for line in passwd.lines() {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('+') || line.starts_with('-') {
            return None;
        }
        let mut fields = line.split(':');
        let name = fields.next()?;
        let primary: u32 = fields.nth(2)?.parse().ok()?;
        if primary == gid {
            accounts.push(name.to_owned());
        }
    }
    assert!(accounts.len() <= passwd.len());
    Some(accounts)
}

/// Names no user id where launchd domains do not exist. Output: zero, unused off macOS.
#[cfg(not(unix))]
fn current_uid() -> u32 {
    0
}

/// The bytes of a definition file: UTF-16LE with a byte order mark for the Windows task
/// XML, UTF-8 elsewhere. Inputs: the definition text and platform. Output: the bytes.
fn definition_bytes(text: &str, platform: Platform) -> Vec<u8> {
    if platform == Platform::Windows {
        [0xff, 0xfe]
            .into_iter()
            .chain(text.encode_utf16().flat_map(u16::to_le_bytes))
            .collect()
    } else {
        text.as_bytes().to_vec()
    }
}

/// Writes a definition file through a staged copy and a rename, in the bytes its manager
/// reads ([`definition_bytes`]). The copy gets a name of its own per call, is created new
/// and never through a link, and is removed when the write fails. Inputs: the file, the
/// definition text and the platform. Output: Ok, or an error naming the file.
pub fn write_definition(path: &Path, text: &str, platform: Platform) -> Result<(), ServiceError> {
    let fail = |error: std::io::Error| failure(format!("{}: {error}", path.display()));
    let parent = path
        .parent()
        .ok_or_else(|| failure("the service definition has no folder"))?;
    std::fs::create_dir_all(parent).map_err(fail)?;
    let bytes = definition_bytes(text, platform);
    let unique = std::hash::BuildHasher::hash_one(
        &std::collections::hash_map::RandomState::new(),
        std::time::SystemTime::now(),
    );
    let staged = parent.join(format!(
        ".{}.{}.{unique:016x}.staged",
        path.file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default(),
        std::process::id()
    ));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let nofollow = i32::try_from(rustix::fs::OFlags::NOFOLLOW.bits())
            .expect("O_NOFOLLOW fits an open flag");
        options.mode(0o644).custom_flags(nofollow);
    }
    let mut file = options.open(&staged).map_err(fail)?;
    let written = std::io::Write::write_all(&mut file, &bytes).and_then(|()| file.sync_all());
    drop(file);
    let landed = written.and_then(|()| std::fs::rename(&staged, path));
    if landed.is_err() {
        let _ = std::fs::remove_file(&staged);
    }
    landed.map_err(fail)
}

#[cfg(unix)]
mod acl;
/// Whether someone other than this user or root can change an entry on a path, per platform:
/// owners, modes, groups and access lists on Unix, access lists on Windows. Input: a path.
/// Output: the first such entry, or none.
mod changeable;
#[cfg(test)]
mod uninstall_tests;
#[cfg(all(test, unix))]
use changeable::{Part, first_changeable_part};
pub use changeable::{changeable_by_others, changeable_past_sticky};
mod cli_link;
#[cfg(all(test, windows))]
use cli_link::link_cli_under;
pub use cli_link::{WINDOWS_PATH_FOLDER_VARIABLE, link_cli, windows_path_command};

/// The runs of service manager commands: quiet queries and checked commands whose failure
/// names the program, arguments, status and error text. Inputs: the commands. Outputs:
/// their answers.
mod run;
#[cfg(not(unix))]
use run::run_process;
use run::{run_checked, run_quiet};

/// The escapes and quotes each manager's definition format needs, and the bounded reads
/// that take a written definition or Info.plist apart again. Inputs: text or a path.
/// Outputs: the escaped, quoted or read-back text.
mod markup;

#[cfg(any(test, feature = "test-support"))]
pub mod test_support;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod windows_task_tests;
