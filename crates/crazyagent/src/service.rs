use crate::engine::owner::{OwnerClient, OwnerClientKind};
use crate::paths::{Layout, Platform, RootEnvironment};
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

const LAUNCHCTL: &str = "/bin/launchctl";
const SYSTEMCTL: &str = "systemctl";
const SCHTASKS: &str = "schtasks";
const WINDOWS_SHELL: &str = r"%SystemRoot%\System32\cmd.exe";
const SERVICE_PATH_EXTRAS: [&str; 6] = [
    "/opt/homebrew/bin",
    "/usr/local/bin",
    "/usr/bin",
    "/bin",
    "/usr/sbin",
    "/sbin",
];
const POLL: Duration = Duration::from_millis(100);
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);
const UNLOAD_TIMEOUT: Duration = Duration::from_secs(35);
const LOG_SCAN_BYTES: u64 = 4 << 20;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceError(pub String);

impl std::fmt::Display for ServiceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ServiceError {}

impl From<crate::paths::PathsError> for ServiceError {
    fn from(error: crate::paths::PathsError) -> Self {
        Self(error.0)
    }
}

fn failure(text: impl Into<String>) -> ServiceError {
    ServiceError(text.into())
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
    pub definition: PathBuf,
    pub user: Option<String>,
}

/// What a service manager reports about the engine service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    pub installed: bool,
    pub running: bool,
    pub pid: Option<u32>,
}

/// Builds the PATH the service gives the engine for its harness lookup: the absolute
/// entries of the installing shell's PATH, then `~/.local/bin` and the usual system
/// folders, each once. Inputs: the current PATH and home folder. Output: the joined PATH.
pub fn service_path(current: Option<&OsStr>, home: Option<&Path>) -> OsString {
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
        if folder.is_absolute() && !folders.contains(&folder) {
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
    /// The engine service for the current user on one platform, named for the layout's data
    /// root by [`service_name`]. Inputs: the platform, the absolute executable the service
    /// runs and the engine's layout. Output: the service with its name and definition path.
    pub fn current_user(
        platform: Platform,
        executable: PathBuf,
        layout: &Layout,
    ) -> Result<Self, ServiceError> {
        let home = std::env::var_os("HOME")
            .filter(|home| !home.is_empty())
            .map(PathBuf::from);
        let default_root = crate::paths::data_root(
            platform,
            &RootEnvironment {
                home_override: None,
                ..RootEnvironment::current()
            },
        )
        .ok();
        let name = service_name(platform, layout.root(), default_root.as_deref());
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
            Platform::Windows => layout.root().join("crazyagent-engine-task.xml"),
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
            root: layout.root().to_path_buf(),
            path: service_path(std::env::var_os("PATH").as_deref(), home.as_deref()),
            definition,
            user: user.filter(|_| platform == Platform::Windows),
        })
    }

    fn layout(&self) -> Result<Layout, ServiceError> {
        Ok(Layout::new(self.root.clone())?)
    }

    fn log(&self) -> Result<PathBuf, ServiceError> {
        Ok(self.layout()?.engine_log())
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
        Ok(())
    }

    /// The text of the service definition: a launchd property list, a systemd unit or a
    /// Task Scheduler XML document. It runs `<executable> engine --home <root>`, appends
    /// output to `<root>/logs/engine.log` (through `cmd.exe` on Windows, whose task has no
    /// output redirection) and restarts after a crash.
    pub fn definition_text(&self) -> Result<String, ServiceError> {
        self.checked()?;
        let executable = self.executable.to_string_lossy();
        let root = self.root.to_string_lossy();
        let log = self.log()?;
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
\t<key>StandardOutPath</key>\n\
\t<string>{log}</string>\n\
\t<key>StandardErrorPath</key>\n\
\t<string>{log}</string>\n\
</dict>\n\
</plist>\n",
                label = xml(&self.name),
                executable = xml(&executable),
                root = xml(&root),
                path = xml(&self.path.to_string_lossy()),
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
                executable = systemd_quoted(&executable),
                root = systemd_quoted(&root),
                path = systemd_quoted(&format!("PATH={}", self.path.to_string_lossy())),
                log = systemd_specifiers(&log),
            ),
            Platform::Windows => {
                let trigger_user = self
                    .user
                    .as_deref()
                    .map(|user| format!("<UserId>{}</UserId>", xml(user)))
                    .unwrap_or_default();
                format!(
                    "<?xml version=\"1.0\" encoding=\"UTF-16\"?>\r\n\
<Task version=\"1.2\" xmlns=\"http://schemas.microsoft.com/windows/2004/02/mit/task\">\r\n\
  <RegistrationInfo><Description>CrazyAgent engine</Description></RegistrationInfo>\r\n\
  <Triggers><LogonTrigger><Enabled>true</Enabled>{trigger_user}</LogonTrigger></Triggers>\r\n\
  <Principals><Principal id=\"Author\"><LogonType>InteractiveToken</LogonType><RunLevel>LeastPrivilege</RunLevel></Principal></Principals>\r\n\
  <Settings><MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy><DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries><StopIfGoingOnBatteries>false</StopIfGoingOnBatteries><ExecutionTimeLimit>PT0S</ExecutionTimeLimit><RestartOnFailure><Interval>PT1M</Interval><Count>3</Count></RestartOnFailure><Hidden>true</Hidden></Settings>\r\n\
  <Actions Context=\"Author\"><Exec><Command>{WINDOWS_SHELL}</Command><Arguments>{arguments}</Arguments></Exec></Actions>\r\n\
</Task>\r\n",
                    arguments = xml(&format!(
                        "/d /s /c \"{} engine --home {} >> {} 2>&1\"",
                        windows_quoted(&executable),
                        windows_quoted(&root),
                        windows_quoted(&log),
                    )),
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
    /// name, so the engine starts now and at every login. Output: an error naming the
    /// failing step.
    pub fn install(&self) -> Result<(), ServiceError> {
        if !self.executable.is_file() {
            return Err(failure(format!(
                "{}: the engine executable is missing",
                self.executable.display()
            )));
        }
        self.layout()?.prepare()?;
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
    /// A service that is not installed is left as it is.
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
            for command in self.unregister_commands(current_uid()) {
                let _ = run_checked(&command);
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

    /// Starts the installed engine service. Output: an error when it is not installed.
    pub fn start(&self) -> Result<(), ServiceError> {
        if !self.definition.is_file() {
            return Err(failure(
                "the engine service is not installed; run: crazyagent service install",
            ));
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

fn xml(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&apos;"),
            other => escaped.push(other),
        }
    }
    escaped
}

fn systemd_specifiers(text: &str) -> String {
    text.replace('%', "%%")
}

fn systemd_quoted(text: &str) -> String {
    let mut quoted = String::from("\"");
    for character in text.chars() {
        match character {
            '\\' => quoted.push_str("\\\\"),
            '"' => quoted.push_str("\\\""),
            '%' => quoted.push_str("%%"),
            '$' => quoted.push_str("$$"),
            other => quoted.push(other),
        }
    }
    quoted.push('"');
    quoted
}

fn windows_quoted(text: &str) -> String {
    format!("\"{text}\"")
}

#[cfg(unix)]
fn current_uid() -> u32 {
    rustix::process::getuid().as_raw()
}

/// Names no user id where launchd domains do not exist. Output: zero, unused off macOS.
#[cfg(not(unix))]
fn current_uid() -> u32 {
    0
}

fn write_definition(path: &Path, text: &str, platform: Platform) -> Result<(), ServiceError> {
    let fail = |error: std::io::Error| failure(format!("{}: {error}", path.display()));
    let parent = path
        .parent()
        .ok_or_else(|| failure("the service definition has no folder"))?;
    std::fs::create_dir_all(parent).map_err(fail)?;
    let bytes = if platform == Platform::Windows {
        [0xff, 0xfe]
            .into_iter()
            .chain(text.encode_utf16().flat_map(u16::to_le_bytes))
            .collect()
    } else {
        text.as_bytes().to_vec()
    };
    let staged = parent.join(format!(
        ".{}.staged",
        path.file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default()
    ));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o644);
    let mut file = options.open(&staged).map_err(fail)?;
    std::io::Write::write_all(&mut file, &bytes)
        .and_then(|()| file.sync_all())
        .map_err(fail)?;
    drop(file);
    std::fs::rename(&staged, path).map_err(fail)
}

/// Runs a service manager query. A manager that is not on this computer, such as `systemctl`
/// on a host without systemd, has nothing installed or running, so it reads as a failed
/// query. Input: the command. Output: whether it succeeded and its standard output.
fn run_quiet(command: &[OsString]) -> Result<(bool, String), ServiceError> {
    let (program, args) = command
        .split_first()
        .ok_or_else(|| failure("empty service command"))?;
    let output = match crate::spawn_gate::output(
        std::process::Command::new(program)
            .args(args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped()),
    ) {
        Ok(output) => output,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok((false, String::new()));
        }
        Err(error) => return Err(failure(format!("{}: {error}", program.to_string_lossy()))),
    };
    Ok((
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
    ))
}

fn run_checked(command: &[OsString]) -> Result<(), ServiceError> {
    let (program, args) = command
        .split_first()
        .ok_or_else(|| failure("empty service command"))?;
    let output = crate::spawn_gate::output(
        std::process::Command::new(program)
            .args(args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped()),
    )
    .map_err(|error| failure(format!("{}: {error}", program.to_string_lossy())))?;
    if output.status.success() {
        return Ok(());
    }
    let shown: Vec<_> = command.iter().map(|part| part.to_string_lossy()).collect();
    let detail = String::from_utf8_lossy(&output.stderr);
    Err(failure(format!(
        "`{}` failed ({}): {}",
        shown.join(" "),
        output.status,
        detail.trim()
    )))
}

/// Says whether an engine listens on a socket now. Input: the ordinary socket path.
/// Output: true when a connection is accepted.
#[cfg(unix)]
pub fn answering(socket: &Path) -> bool {
    crate::spawn_gate::connect_unix_std(socket).is_ok()
}

/// Says no engine listens where the Unix socket transport does not exist. Input unused.
#[cfg(not(unix))]
pub fn answering(_socket: &Path) -> bool {
    false
}

/// What a client learns when it checks the engine before opening.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineCheck {
    Ready { version: String },
    Mismatch { version: String, message: String },
    Unavailable { reason: String },
}

/// Authenticates to the engine at a data root and compares its version with this app's.
/// Inputs: the root, the client kind and a bounded timeout. Output: ready, a version
/// mismatch with its plain message (also when a newer engine's handshake cannot be read),
/// or unavailable with the engine's reason.
pub async fn check_engine(root: &Path, kind: OwnerClientKind, timeout: Duration) -> EngineCheck {
    let reported = match OwnerClient::discover(root, kind, timeout).await {
        Ok((_, identity)) => identity.engine_version().to_owned(),
        Err(error) => match crate::engine::bootstrap::reported_engine_version(root) {
            Some(version) if version != crate::version::VERSION => version,
            _ => return EngineCheck::Unavailable { reason: error.0 },
        },
    };
    match crate::version::mismatch(&reported) {
        None => EngineCheck::Ready { version: reported },
        Some(message) => EngineCheck::Mismatch {
            version: reported,
            message,
        },
    }
}

/// How `ensure_running` found or started the engine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Started {
    AlreadyRunning,
    Service,
    Detached { pid: u32 },
}

/// Starts the engine for a client when none answers: through the installed service, or
/// else as a detached `<executable> engine --home <root>` with output in the engine log.
/// Inputs: the layout, the service, the executable, the PATH a detached engine gets and
/// the wait bound. Output: how the engine runs once it answers, or a clear error naming
/// the log after the bound or when the started engine stops.
pub async fn ensure_running(
    layout: &Layout,
    service: &Service,
    executable: &Path,
    path: Option<&OsStr>,
    bound: Duration,
) -> Result<Started, ServiceError> {
    if answering(&layout.socket()) {
        return Ok(Started::AlreadyRunning);
    }
    layout.prepare()?;
    let since = log_length(layout);
    let mut child = None;
    let started = if service.status()?.installed {
        service.start()?;
        Started::Service
    } else {
        let spawned = spawn_detached(layout, executable, path)?;
        let pid = spawned.id();
        child = Some(spawned);
        Started::Detached { pid }
    };
    wait_ready(layout, bound, child.as_mut(), since).await?;
    Ok(started)
}

fn spawn_detached(
    layout: &Layout,
    executable: &Path,
    path: Option<&OsStr>,
) -> Result<std::process::Child, ServiceError> {
    let log = layout.engine_log();
    let mut options = std::fs::OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let output = options
        .open(&log)
        .map_err(|error| failure(format!("{}: {error}", log.display())))?;
    let errors = output
        .try_clone()
        .map_err(|error| failure(format!("{}: {error}", log.display())))?;
    let mut command = std::process::Command::new(executable);
    command
        .arg("engine")
        .arg("--home")
        .arg(layout.root())
        .stdin(std::process::Stdio::null())
        .stdout(output)
        .stderr(errors);
    if let Some(path) = path {
        command.env("PATH", path);
    }
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut command, 0);
    crate::spawn_gate::spawn(&mut command)
        .map_err(|error| failure(format!("{}: {error}", executable.display())))
}

/// Stops an engine that runs without the service, such as one a client started detached:
/// it sends SIGTERM to the process that serves the engine socket, then waits until the
/// socket is silent and the engine has let go of its database, so a new engine can start on
/// the same data folder. Input: the layout. Output: the stopped engine's pid, none when no
/// engine answers, or an error naming the pid and the engine log after the bound.
#[cfg(unix)]
pub fn stop_detached(layout: &Layout) -> Result<Option<u32>, ServiceError> {
    let Ok(stream) = crate::spawn_gate::connect_unix_std(layout.socket()) else {
        return Ok(None);
    };
    let pid = serving_pid(&stream)?;
    drop(stream);
    let target = rustix::process::Pid::from_raw(pid)
        .ok_or_else(|| failure("the engine socket names no process"))?;
    rustix::process::kill_process(target, rustix::process::Signal::TERM)
        .map_err(|error| failure(format!("the engine (pid {pid}) can't be stopped: {error}")))?;
    let deadline = Instant::now() + UNLOAD_TIMEOUT;
    while answering(&layout.socket()) || !crate::storage::engine_lock_released(layout.root()) {
        if Instant::now() >= deadline {
            return Err(failure(format!(
                "the engine (pid {pid}) still ran {} s after it was asked to stop; see {}",
                UNLOAD_TIMEOUT.as_secs(),
                layout.engine_log().display()
            )));
        }
        std::thread::sleep(POLL);
    }
    Ok(Some(pid.unsigned_abs()))
}

/// Finds no engine to stop where the Unix socket transport does not exist. Input unused.
/// Output: none.
#[cfg(not(unix))]
pub fn stop_detached(_layout: &Layout) -> Result<Option<u32>, ServiceError> {
    Ok(None)
}

/// The process that serves a connected engine socket, which must belong to this user and
/// not be this process. Input: the connected stream. Output: its pid.
#[cfg(unix)]
fn serving_pid(stream: &std::os::unix::net::UnixStream) -> Result<i32, ServiceError> {
    let unknown =
        |error: nix::Error| failure(format!("the engine socket's process is unknown: {error}"));
    #[cfg(target_os = "macos")]
    let (uid, pid) = (
        nix::unistd::getpeereid(stream).map_err(unknown)?.0.as_raw(),
        nix::sys::socket::getsockopt(stream, nix::sys::socket::sockopt::LocalPeerPid)
            .map_err(unknown)?,
    );
    #[cfg(not(target_os = "macos"))]
    let (uid, pid) = {
        let peer = nix::sys::socket::getsockopt(stream, nix::sys::socket::sockopt::PeerCredentials)
            .map_err(unknown)?;
        (peer.uid(), peer.pid())
    };
    if uid != rustix::process::getuid().as_raw()
        || pid <= 0
        || pid.unsigned_abs() == std::process::id()
    {
        return Err(failure(
            "the engine socket is served by another user's process",
        ));
    }
    Ok(pid)
}

/// The engine log's length now, taken before a start so `wait_ready` reads only the
/// lines that start wrote. Input: the layout. Output: bytes, zero for no log yet.
pub fn log_length(layout: &Layout) -> u64 {
    std::fs::metadata(layout.engine_log()).map_or(0, |metadata| metadata.len())
}

fn logged_ready(layout: &Layout, since: u64) -> bool {
    use std::io::{Read, Seek};
    let Ok(mut log) = std::fs::File::open(layout.engine_log()) else {
        return false;
    };
    let length = log.metadata().map_or(0, |metadata| metadata.len());
    let start = if length < since { 0 } else { since };
    let mut text = String::new();
    log.seek(std::io::SeekFrom::Start(start)).is_ok()
        && log.take(LOG_SCAN_BYTES).read_to_string(&mut text).is_ok()
        && text.lines().any(|line| line == "ready")
}

/// Waits until the engine at a layout has written `ready` to its log after a start and
/// answers an authenticated read. Inputs: the layout, the bound, the detached engine to
/// watch, if any, and the log length before the start. Output: Ok once both hold, or an
/// error naming the engine log when the bound passes or the engine stops first.
pub async fn wait_ready(
    layout: &Layout,
    bound: Duration,
    mut child: Option<&mut std::process::Child>,
    since: u64,
) -> Result<(), ServiceError> {
    let deadline = Instant::now() + bound;
    loop {
        let probe = PROBE_TIMEOUT.min(bound.max(POLL));
        if logged_ready(layout, since)
            && answering(&layout.socket())
            && !matches!(
                check_engine(layout.root(), OwnerClientKind::Tui, probe).await,
                EngineCheck::Unavailable { .. }
            )
        {
            return Ok(());
        }
        if let Some(child) = child.as_deref_mut()
            && let Ok(Some(status)) = child.try_wait()
        {
            return Err(failure(format!(
                "the engine stopped while starting ({status}); see {}",
                layout.engine_log().display()
            )));
        }
        if Instant::now() >= deadline {
            return Err(failure(format!(
                "the engine did not answer within {} s; see {}",
                bound.as_secs(),
                layout.engine_log().display()
            )));
        }
        tokio::time::sleep(POLL).await;
    }
}

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
/// PowerShell, keeping every existing entry. Inputs: the executable and an unused bin
/// folder. Output: the folder added.
#[cfg(not(unix))]
pub fn link_cli(executable: &Path, _bin: &Path) -> Result<PathBuf, ServiceError> {
    let folder = executable
        .parent()
        .ok_or_else(|| failure("the executable has no folder"))?
        .to_path_buf();
    run_checked(&windows_path_command(&folder))?;
    Ok(folder)
}

/// The PowerShell command that appends a folder to the user's PATH once. Input: the
/// folder, passed as an argument rather than inside the script. Output: the command.
pub fn windows_path_command(folder: &Path) -> Vec<OsString> {
    let script = "$f=$args[0]; $p=[Environment]::GetEnvironmentVariable('Path','User'); \
if ($null -eq $p) { $p='' }; \
if (-not (($p -split ';') -contains $f)) { \
[Environment]::SetEnvironmentVariable('Path', (($p.TrimEnd(';'), $f) -join ';').TrimStart(';'), 'User') }";
    [
        "powershell.exe",
        "-NoProfile",
        "-NonInteractive",
        "-Command",
        script,
    ]
    .into_iter()
    .map(OsString::from)
    .chain(std::iter::once(folder.as_os_str().to_os_string()))
    .collect()
}

#[cfg(test)]
mod tests;
