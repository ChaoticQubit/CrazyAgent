use super::SandboxError;
use crate::harnesses::interactive::{InteractiveCommand, Transcript};
use crate::protocol::ProjectId;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

/// What a Sandboxed IDE terminal shows on a system with no qualified sandbox for it,
/// naming the IDE's own shell profile the owner can choose instead.
pub const UNAVAILABLE: &str = "The Sandboxed terminal is unavailable: this system has no qualified sandbox for it. \"IDE shell\" in the terminal menu runs inside the IDE's own sandbox instead.";

/// The variables a Sandboxed IDE terminal takes from the engine's own environment. One the
/// engine does not have is removed, and every other variable is dropped, so the IDE
/// capability and socket and the owner's tokens never reach the shell.
pub const TERMINAL_VARIABLES: [&str; 7] = [
    "TERM",
    "COLORTERM",
    "TERM_PROGRAM",
    "TERM_PROGRAM_VERSION",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
];

#[cfg(target_os = "macos")]
const SHELL_ARGUMENTS_MAX: usize = 16;
#[cfg(target_os = "macos")]
const FOLDER: &str = "ide-shell";
#[cfg(target_os = "macos")]
const SHELLS: &str = "/etc/shells";
#[cfg(target_os = "macos")]
const SHELLS_BYTES_MAX: u64 = 16 * 1024;

/// Where a project lives for its IDE terminal: its canonical folder and the extra
/// read-only folders the owner saved for its agents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Place {
    pub folder: PathBuf,
    pub extra_reads: Vec<PathBuf>,
}

/// One Sandboxed IDE terminal launch: the engine data root, the project and its folder,
/// where the terminal starts, the shell and its arguments (none starts a login shell), the
/// terminal variables the engine passes, the project's extra read-only folders and the text
/// of the user-level git config for the terminal's own home.
#[derive(Debug, Clone, Copy)]
pub struct Launch<'a> {
    pub root: &'a Path,
    pub project: &'a ProjectId,
    pub folder: &'a Path,
    pub cwd: &'a Path,
    pub shell: &'a Path,
    pub arguments: &'a [OsString],
    pub incoming: &'a [(OsString, OsString)],
    pub extra_reads: &'a [PathBuf],
    pub git_config: &'a str,
}

/// Reads the project's folder and its extra read-only folders from the engine database,
/// opened read-only so a terminal launch never writes engine state. Inputs: the engine
/// data root and the project. Output: the place, or an error naming what is missing.
pub fn project_place(root: &Path, project: &ProjectId) -> Result<Place, String> {
    use rusqlite::OpenFlags;
    let db = rusqlite::Connection::open_with_flags(
        root.join("engine.db"),
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )
    .map_err(|_| "the engine database could not be opened".to_owned())?;
    db.busy_timeout(Duration::from_secs(5))
        .map_err(|_| "the engine database is busy".to_owned())?;
    let setup = {
        let tx = db
            .unchecked_transaction()
            .map_err(|_| "the engine database could not be read".to_owned())?;
        crate::project_setup::load_setup(&tx, project).map_err(|error| error.to_string())?
    }
    .ok_or("the project is unknown")?;
    let folder = setup
        .project_folder()
        .canonicalize()
        .map_err(|_| "the project folder is missing".to_owned())?;
    let extra_reads = super::extra_paths::load(&db, project.as_str())
        .map_err(|_| "the project's read-only folders could not be read".to_owned())?;
    assert!(folder.is_absolute());
    Ok(Place {
        folder,
        extra_reads,
    })
}

/// Builds the Sandboxed terminal's command: the shell under the project's agent sandbox
/// with open network, the engine runtime guarded (its IDE folder with each capability and
/// server token lies outside every allowed root), the project's toolchains and extra
/// folders readable, its own home and temp folders, and only the
/// [`TERMINAL_VARIABLES`] given. The engine runs it outside every sandbox, since macOS
/// refuses a second Seatbelt profile inside a sandboxed process such as the IDE server.
/// Input: the launch. Output: the command, or a refusal for an unlisted shell, a start
/// outside the project folder or an unsafe root.
#[cfg(target_os = "macos")]
pub fn command(launch: &Launch<'_>) -> Result<Command, SandboxError> {
    use super::toolchains::{self, ToolHomes, Toolchains};
    use super::{
        ProjectSandbox, SANDBOX_EXEC, SandboxNetwork, agent_env, agent_variable_allowed, borrowed,
        canonical, clean_absolute, invalid,
    };
    if launch.project.as_str() == "." {
        return Err(invalid("the project id is not a folder name"));
    }
    if launch.arguments.len() > SHELL_ARGUMENTS_MAX {
        return Err(invalid("too many shell arguments"));
    }
    let root = canonical(launch.root)?;
    if root != launch.root || !clean_absolute(&root) {
        return Err(invalid("the engine data root must be a canonical folder"));
    }
    let cwd = canonical(launch.cwd)?;
    if !cwd.starts_with(launch.folder) {
        return Err(invalid("the terminal must start inside the project folder"));
    }
    listed_shell(launch.shell)?;
    let place = root.join(FOLDER).join(launch.project.as_str());
    let home = place.join("home");
    let tmp = place.join("tmp");
    for folder in [&home, &tmp] {
        private_folder(folder)?;
    }
    let homes = ToolHomes::owner()?;
    let scratch = toolchains::make_tmp(&home)?;
    toolchains::prepare_cargo_home(&home, &homes)?;
    let (sandbox, _) = ProjectSandbox::new(launch.folder, &home, &tmp, SandboxNetwork::Open)?
        .guard_engine(&root)?
        .allow_tools(&Toolchains::detect(launch.folder, &homes))?
        .allow_scratch(&scratch)?
        .allow_extra_reads(launch.extra_reads)?;
    let profile = place.join("shell.sb");
    sandbox.install(&profile)?;
    let base = sandbox.base_env(true);
    let terminal = TERMINAL_VARIABLES.iter().map(|name| {
        let value = launch
            .incoming
            .iter()
            .find(|(known, _)| known.as_os_str() == OsStr::new(name))
            .map(|(_, value)| value.as_os_str());
        (OsStr::new(*name), value)
    });
    let env = agent_env(&borrowed(&base), terminal)?;
    let login = [OsString::from("-l")];
    let arguments = if launch.arguments.is_empty() {
        &login[..]
    } else {
        launch.arguments
    };
    let mut command = Command::new(SANDBOX_EXEC);
    command
        .arg("-f")
        .arg(&profile)
        .arg(launch.shell)
        .args(arguments)
        .env_clear()
        .envs(env.iter().map(|(name, value)| (name, value)))
        .current_dir(&cwd);
    assert_eq!(command.get_program(), SANDBOX_EXEC);
    assert_eq!(command.get_args().nth(2), Some(launch.shell.as_os_str()));
    assert!(command.get_envs().all(|(name, value)| {
        value.is_some_and(|value| agent_variable_allowed(name, value))
            && !name.to_string_lossy().starts_with("CRAZYAGENT_")
    }));
    Ok(command)
}

/// Refuses the Sandboxed terminal where no sandbox is qualified for it, before anything
/// is written. Takes the launch, unused. Returns `Unavailable` with [`UNAVAILABLE`].
#[cfg(not(target_os = "macos"))]
pub fn command(_launch: &Launch<'_>) -> Result<Command, SandboxError> {
    Err(SandboxError::Unavailable(UNAVAILABLE.into()))
}

/// Builds the Sandboxed terminal as the engine's terminal host runs it in a pane: the same
/// argument list, working folder and environment as [`command`], as text. Input: the
/// launch. Output: the pane command, or [`command`]'s refusal, or `Invalid` for a part
/// that is not UTF-8.
pub fn interactive(launch: &Launch<'_>) -> Result<InteractiveCommand, SandboxError> {
    let command = command(launch)?;
    let text = |value: &OsStr| {
        value
            .to_str()
            .map(str::to_owned)
            .ok_or_else(|| super::invalid("the Sandboxed terminal command is not plain text"))
    };
    let mut argv = vec![text(command.get_program())?];
    for arg in command.get_args() {
        argv.push(text(arg)?);
    }
    let mut env = Vec::new();
    for (name, value) in command.get_envs() {
        let value =
            value.ok_or_else(|| super::invalid("the Sandboxed terminal unsets a variable"))?;
        env.push((text(name)?, text(value)?));
    }
    let cwd = command
        .get_current_dir()
        .ok_or_else(|| super::invalid("the Sandboxed terminal has no working folder"))?
        .to_path_buf();
    assert_eq!(argv.len(), command.get_args().count() + 1);
    assert_eq!(env.len(), command.get_envs().count());
    Ok(InteractiveCommand {
        argv,
        cwd,
        env,
        transcript: Transcript::Unavailable,
    })
}

/// Reads the owner's login shell from the account database, never from `SHELL`.
/// Output: the shell's path, or the plain reason it is unknown.
#[cfg(unix)]
pub fn owner_shell() -> Result<PathBuf, String> {
    nix::unistd::User::from_uid(nix::unistd::getuid())
        .map_err(|error| error.to_string())?
        .map(|user| user.shell)
        .ok_or_else(|| "the owner account is unknown".to_owned())
}

/// Accepts only a shell named in `/etc/shells`, as login shells are. Input: the shell.
/// Output: Ok, or `Invalid` for a relative, unusual or unlisted path.
#[cfg(target_os = "macos")]
fn listed_shell(shell: &Path) -> Result<(), SandboxError> {
    use std::io::Read;
    if !super::clean_absolute(shell) {
        return Err(super::invalid("the shell must be an absolute plain path"));
    }
    let mut text = String::new();
    std::fs::File::open(SHELLS)?
        .take(SHELLS_BYTES_MAX)
        .read_to_string(&mut text)?;
    let listed = text
        .lines()
        .map(str::trim)
        .any(|line| Path::new(line) == shell);
    if !listed {
        return Err(super::invalid("the shell is not listed in /etc/shells"));
    }
    Ok(())
}

/// Makes one of the terminal's own folders with mode 0700, or reuses it when it is a real
/// folder. Input: the path. Output: Ok, or the refusal for a link or a file there.
#[cfg(target_os = "macos")]
fn private_folder(path: &Path) -> Result<(), SandboxError> {
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)?;
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.is_dir() {
        return Err(super::invalid("the terminal folder must be a real folder"));
    }
    Ok(())
}
