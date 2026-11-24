use crate::error::SandboxError;
use crate::policy::{AgentAccess, AgentFilesRule, AgentNetwork};
use crate::profile::ProfileHooks;
use kernel_processes::terminal::PaneCommand;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::Command;

/// What a confined IDE terminal shows on a system with no qualified sandbox for it.
pub const UNAVAILABLE: &str =
    "This terminal is unavailable: this system has no qualified sandbox for it.";

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
const NO_STAGING: &str =
    "This terminal can't stage or commit. Use Source Control, or a Full access terminal.";
const NO_STAGING_RAISE_ACCESS: &str = "This terminal can't stage or commit. Use Source Control, or a Full access terminal after raising project access.";
/// The words a pane puts in front of the owner's login shell: a plain shell prints the line
/// that follows them and becomes that login shell. The pane host repaints the terminal as it
/// attaches, so a line written from outside the pane would be wiped.
const SAYS_THEN_SHELL: [&str; 4] = [
    "/bin/sh",
    "-c",
    "printf '%s\\n' \"$1\"; shift; exec \"$@\"",
    "sh",
];
/// Where a pane's argument list names the shell: after `sandbox-exec -f <profile>`.
const SHELL_ARGUMENT: usize = 3;
#[cfg(target_os = "macos")]
const SHELLS: &str = "/etc/shells";
#[cfg(target_os = "macos")]
const SHELLS_BYTES_MAX: u64 = 16 * 1024;

/// One Sandboxed IDE terminal launch: the engine data root, the project and its folder,
/// where the terminal starts, the shell and its arguments (none starts a login shell), the
/// terminal variables the engine passes, the project's extra read-only folders, the level
/// the terminal runs at, the project's network choice, the owner's local model ports the
/// terminal keeps free and what it may change in agent folders.
#[derive(Debug, Clone, Copy)]
pub struct Launch<'a> {
    pub root: &'a Path,
    pub project: &'a str,
    pub folder: &'a Path,
    pub cwd: &'a Path,
    pub shell: &'a Path,
    pub arguments: &'a [OsString],
    pub incoming: &'a [(OsString, OsString)],
    pub extra_reads: &'a [PathBuf],
    pub level: AgentAccess,
    pub network: AgentNetwork,
    pub kept_ports: &'a [u16],
    pub agent_files: AgentFilesRule,
}

/// Builds an engine-hosted terminal's command: the shell under the project's agent sandbox
/// with the network its level allows, the engine runtime guarded (its IDE folder with each capability and
/// server token lies outside every allowed root), the project's toolchains and extra
/// folders readable, its own home and temp folders, the home cleared of every shell startup
/// file first (SA vid 12), and only the [`TERMINAL_VARIABLES`] given. The engine runs it
/// outside every sandbox, since macOS refuses a second Seatbelt profile inside a sandboxed
/// process such as the IDE server.
/// Its listeners stay off the owner's local model ports, and it keeps to the project's IDE
/// agent folders choice (DEC-124).
/// Input: the launch. Output: the command, or a refusal for an unlisted shell, a start
/// outside the project folder, an unsafe root or Full access, which only the IDE server's own
/// shell runs.
#[cfg(target_os = "macos")]
pub fn command(hooks: &ProfileHooks, launch: &Launch<'_>) -> Result<Command, SandboxError> {
    use crate::error::invalid;
    use crate::paths::{canonical, clean_absolute};
    use crate::profile::ProjectSandbox;
    use crate::profile::command::SANDBOX_EXEC;
    use crate::profile::env::{agent_variable_allowed, owner_terminal_env};
    use crate::profile::seatbelt::borrowed;
    use crate::toolchains;
    use crate::toolchains::{ToolHomes, Toolchains};
    if launch.level == AgentAccess::Full {
        return Err(invalid("a Full access terminal runs in the IDE server"));
    }
    if !kernel_common::names::single_component(launch.project) {
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
    let place = root.join(FOLDER).join(launch.project);
    let home = place.join("home");
    let tmp = place.join("tmp");
    for folder in [&home, &tmp] {
        private_folder(folder)?;
    }
    (hooks.toolchain.remove_shell_startup)(&home, &root)
        .map_err(|_| invalid("the terminal home keeps a shell startup file"))?;
    let homes = ToolHomes::owner()?;
    let scratch = toolchains::make_tmp(hooks.toolchain, &home)?;
    toolchains::prepare_cargo_home(hooks.toolchain, &home, &homes)?;
    let network = crate::policy::ide_network(launch.level, launch.network);
    let (sandbox, _) = ProjectSandbox::new(hooks, launch.folder, &home, &tmp, network)?
        .guard_engine(&root)?
        .keep_ports(launch.kept_ports)?
        .allow_tools(&Toolchains::detect(launch.folder, &homes))?
        .allow_scratch(&scratch)?
        .keep_owner_config()
        .protect_agent_files(launch.agent_files)
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
    let env = owner_terminal_env(&borrowed(&base), terminal)?;
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
pub fn command(_hooks: &ProfileHooks, _launch: &Launch<'_>) -> Result<Command, SandboxError> {
    Err(SandboxError::Unavailable(UNAVAILABLE.into()))
}

/// Builds the Sandboxed terminal as the engine's terminal host runs it in a pane: the same
/// argument list, working folder and environment as [`command`], as text, with a login shell
/// started through [`SAYS_THEN_SHELL`] so the owner reads that this terminal cannot stage or
/// commit. Inputs: the launch and the level the project is at, which picks those words.
/// Output: the pane command, or [`command`]'s refusal, or `Invalid` for a part that is not UTF-8.
pub fn interactive(
    hooks: &ProfileHooks,
    launch: &Launch<'_>,
    project_access: AgentAccess,
) -> Result<PaneCommand, SandboxError> {
    let command = command(hooks, launch)?;
    let text = |value: &OsStr| {
        value.to_str().map(str::to_owned).ok_or_else(|| {
            crate::error::invalid("the Sandboxed terminal command is not plain text")
        })
    };
    let mut argv = vec![text(command.get_program())?];
    for arg in command.get_args() {
        argv.push(text(arg)?);
    }
    let mut env = Vec::new();
    for (name, value) in command.get_envs() {
        let value = value
            .ok_or_else(|| crate::error::invalid("the Sandboxed terminal unsets a variable"))?;
        env.push((text(name)?, text(value)?));
    }
    let cwd = command
        .get_current_dir()
        .ok_or_else(|| crate::error::invalid("the Sandboxed terminal has no working folder"))?
        .to_path_buf();
    assert_eq!(argv.len(), command.get_args().count() + 1);
    assert_eq!(env.len(), command.get_envs().count());
    if launch.arguments.is_empty() {
        assert_eq!(OsStr::new(&argv[SHELL_ARGUMENT]), launch.shell.as_os_str());
        let says = SAYS_THEN_SHELL
            .into_iter()
            .chain([staging_line(project_access)])
            .map(str::to_owned);
        argv.splice(SHELL_ARGUMENT..SHELL_ARGUMENT, says);
    }
    Ok(PaneCommand { argv, cwd, env })
}

/// What a confined terminal says about staging. Input: the level the project is at. Output:
/// the line, which says to raise the project's access first unless it is at Full access.
fn staging_line(project_access: AgentAccess) -> &'static str {
    match project_access {
        AgentAccess::Full => NO_STAGING,
        AgentAccess::Sandboxed | AgentAccess::OwnBranch => NO_STAGING_RAISE_ACCESS,
    }
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
    if !crate::paths::clean_absolute(shell) {
        return Err(crate::error::invalid(
            "the shell must be an absolute plain path",
        ));
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
        return Err(crate::error::invalid(
            "the shell is not listed in /etc/shells",
        ));
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
        return Err(crate::error::invalid(
            "the terminal folder must be a real folder",
        ));
    }
    Ok(())
}
