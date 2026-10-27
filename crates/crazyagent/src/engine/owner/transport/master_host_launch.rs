#[cfg(unix)]
use super::Identity;
use super::master_runtime::MasterHostLaunch;
use super::{EngineError, OwnerRuntimeHandle, Root, failure};
use crate::config::{ModelConnection, ModelEndpoint};
use crate::engine::bootstrap::{AgentCredential, NativeProfileUse};
use crate::engine::io_error;
use crate::engine::owner::is_hex_id;
#[cfg(unix)]
use crate::engine::owner::random_id;
use crate::master_session::MasterBackend;
use crate::protocol::RequestId;
use crate::sandbox::InstalledSandbox;
#[cfg(unix)]
use crate::sandbox::toolchains::{ToolHomes, Toolchains};
#[cfg(unix)]
use crate::sandbox::{ProjectSandbox, SandboxError, SandboxNetwork, SandboxRemote};
use crate::storage::PreparedMasterTurn;
use crate::worktrees::LeaseState;
#[cfg(unix)]
use rustix::fs::{AtFlags, Mode};
use std::fs::File;
#[cfg(unix)]
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, MetadataExt};
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
#[cfg(unix)]
use std::process::Stdio;
use std::time::Duration;

#[cfg(unix)]
const CAPTURE_ALLOCATION_ATTEMPTS: usize = 4;
#[cfg(unix)]
const MASTER_INSTRUCTIONS: &str = "instructions.md";
#[cfg(unix)]
const MASTER_TMP: &str = "tmp";
#[cfg(unix)]
const MASTER_HOME: &str = "home";
#[cfg(unix)]
const MASTER_PROFILE: &str = "sandbox.sb";
#[cfg(unix)]
const HTTPS_PORT: u16 = 443;
#[cfg(unix)]
const ACCOUNT_DEFAULT_MODEL: &str = "native-default";
const MASTER_CONFIG: [&str; 15] = [
    "model_provider=\"openai\"",
    "cli_auth_credentials_store=\"file\"",
    "sandbox_mode=\"read-only\"",
    "approval_policy=\"never\"",
    "features.shell_tool=false",
    "features.apply_patch_freeform=false",
    "features.unified_exec=false",
    "features.view_image=false",
    "features.multi_agent=false",
    "features.code_mode=false",
    "features.apps=false",
    "features.plugins=false",
    "features.goals=false",
    "web_search=\"disabled\"",
    "mcp_servers={}",
];
const MASTER_TOOLS_CONFIG: [&str; 9] = [
    "model_provider=\"openai\"",
    "cli_auth_credentials_store=\"file\"",
    "sandbox_mode=\"danger-full-access\"",
    "approval_policy=\"never\"",
    "features.apps=false",
    "features.plugins=false",
    "features.goals=false",
    "features.multi_agent=false",
    "features.multi_agent_v2=false",
];
const DELEGATE_SERVERS: &str = "mcp_servers=";

impl OwnerRuntimeHandle {
    pub(super) fn allocate_master_capture(
        &self,
    ) -> Result<(RequestId, RequestId, File), EngineError> {
        allocate_master_capture(&self.authority.root)
    }

    pub(super) fn prepare_master_host(
        &self,
        prepared: &PreparedMasterTurn,
        profile: &NativeProfileUse,
    ) -> Result<MasterHostLaunch, EngineError> {
        validate_master_binding(prepared)?;
        let binding = prepared.profile.binding();
        let host = self.identity();
        if binding.database_instance != host.db_instance
            || binding.engine_epoch != host.engine_epoch
        {
            return Err(failure("owner_runtime_stale"));
        }
        self.authority.root.verify()?;
        let home = profile.profile_path()?;
        let executable = match prepared.session.selection.backend {
            MasterBackend::Codex => self.registered_codex_executable()?,
            MasterBackend::Claude => self.registered_claude_executable()?,
            MasterBackend::Rcli => return Err(failure("master_runtime_backend_unsupported")),
        };
        let artifact_sha256 = executable.verified_sha256()?;
        if !is_hex_id(&artifact_sha256) {
            return Err(failure("master_runtime_binding"));
        }
        if profile.profile_path()? != home {
            return Err(failure("master_runtime_binding"));
        }
        let (runtime_ref, capture_ref, capture) = allocate_master_capture(&self.authority.root)?;
        let folder = MasterRuntimeFolder::new(&self.authority.root, &runtime_ref);
        let place = MasterTurnPlace {
            root: &self.authority.root,
            runtime_ref: &runtime_ref,
            repository: &prepared.repository,
            worktree: &prepared.cwd,
        };
        let engine = if prepared.tools {
            Some(self.registered_engine_executable()?)
        } else {
            None
        };
        let wall_time =
            Duration::from_millis(prepared.attempt.request.route.resources.wall_time_ms);
        let delegate = engine.as_deref().map(delegate_engine).transpose()?;
        let turn = if prepared.session.selection.backend == MasterBackend::Claude {
            claude_master_turn(
                &place,
                profile,
                executable.verified_path()?,
                &prepared.attempt.connection,
                &prepared.instructions,
                wall_time,
                delegate,
            )?
        } else {
            codex_master_turn(
                &place,
                profile,
                executable.verified_path()?,
                &prepared.attempt.connection,
                wall_time,
                delegate,
            )?
        };
        require_master_delegate(prepared.session.selection.backend, &turn, engine.as_deref())?;
        let MasterTurnLaunch {
            command,
            sandbox,
            home,
            credential,
            tool_record,
        } = turn;
        assert_eq!(tool_record.is_some(), prepared.tools);
        assert_eq!(command.get_current_dir(), Some(prepared.cwd.as_path()));
        assert_ne!(runtime_ref, capture_ref);
        Ok(MasterHostLaunch {
            command,
            sandbox,
            home,
            credential,
            artifact_sha256,
            runtime_ref,
            capture_ref,
            capture,
            folder,
            tool_record,
        })
    }
}

/// Where one master turn runs: the owner root, the turn's runtime reference from
/// `allocate_master_capture`, and the project repository and leased worktree it works in.
pub(super) struct MasterTurnPlace<'a> {
    pub(super) root: &'a Root,
    pub(super) runtime_ref: &'a RequestId,
    pub(super) repository: &'a Path,
    pub(super) worktree: &'a Path,
}

/// One master turn's command and Seatbelt boundary, the home both name, the account
/// credential copy that lives in that home until it is dropped at the end of the turn, and
/// for a turn with tools on the drop file its `delegate` tool records proposals in.
pub(super) struct MasterTurnLaunch {
    pub(super) command: Command,
    pub(super) sandbox: InstalledSandbox,
    pub(super) home: PathBuf,
    pub(super) credential: AgentCredential,
    pub(super) tool_record: Option<PathBuf>,
}

/// Builds one Claude Code master turn: its startup instruction file, a private home in the
/// turn folder holding a no-refresh copy of the account's credential (DEC-090), and the
/// command and Seatbelt boundary on that home: tools off, or with `engine` the tools-on
/// command whose `delegate` tool that engine executable serves (DEC-093). The keychain stays
/// denied and the managed home is never given to the master. Inputs: where the turn runs, the
/// account lease, the registered executable, the attempt's connection, the instruction text,
/// the turn's wall time and the engine executable for tools on. Output: the launch parts with
/// the copy, or the refusal of the part that failed, `native_profile_renewal_failed`, a
/// transient refusal, when the token still lapses within a minute after a renewal.
pub(super) fn claude_master_turn(
    place: &MasterTurnPlace<'_>,
    profile: &NativeProfileUse,
    executable: &Path,
    connection: &ModelConnection,
    instructions: &str,
    wall_time: Duration,
    engine: Option<&Path>,
) -> Result<MasterTurnLaunch, EngineError> {
    let instructions = write_master_instructions(place.root, place.runtime_ref, instructions)?;
    let home = master_turn_home(place.root, place.runtime_ref)?;
    let credential = profile.hand_to_agent(&home, wall_time, executable)?;
    let (command, sandbox, tool_record) = match engine {
        None => (
            crate::engine::claude_host::launch_command(
                executable,
                place.worktree,
                &home,
                &connection.model.id,
                Some(instructions.as_path()),
            )?,
            master_sandbox(
                place.root,
                place.runtime_ref,
                place.repository,
                place.worktree,
                &home,
                executable,
                &connection.endpoint,
            )?,
            None,
        ),
        Some(engine) => {
            let sandbox = master_tools_sandbox(
                place.root,
                place.runtime_ref,
                place.repository,
                place.worktree,
                &home,
                executable,
                &connection.endpoint,
                engine,
            )?;
            let drop = sandbox
                .sandbox()
                .agent_tmp()
                .join(crate::team::tool::DROP_FILE);
            let command = crate::engine::claude_host::tools_launch_command(
                executable,
                place.worktree,
                &home,
                &connection.model.id,
                Some(instructions.as_path()),
                crate::engine::claude_host::MasterTools {
                    engine,
                    drop: &drop,
                },
            )?;
            (command, sandbox, Some(drop))
        }
    };
    Ok(MasterTurnLaunch {
        command,
        sandbox,
        home,
        credential,
        tool_record,
    })
}

/// Builds one Codex master turn in a private home in the turn folder holding a no-refresh
/// copy of the account's credential (DEC-090), so a master never refreshes the managed
/// home beside other holders of its shared lease (CR2-17). With tools off it is the
/// read-only app-server. With `engine` its tools are on (DEC-093): shell and patch tools
/// with Codex's own sandbox off because macOS refuses a Seatbelt profile inside the
/// master's, which keeps writes to the leased worktree and the turn's own folders, and only
/// the engine's `delegate` server. Inputs: where the turn runs, the account lease, the
/// registered executable, the attempt's connection, the turn's wall time and the engine
/// executable for tools on. Output: the launch parts with the copy, or the refusal of the
/// part that failed.
pub(super) fn codex_master_turn(
    place: &MasterTurnPlace<'_>,
    profile: &NativeProfileUse,
    executable: &Path,
    connection: &ModelConnection,
    wall_time: Duration,
    engine: Option<&Path>,
) -> Result<MasterTurnLaunch, EngineError> {
    let model = &connection.model.id;
    let home = master_turn_home(place.root, place.runtime_ref)?;
    let credential = profile.hand_to_agent(&home, wall_time, executable)?;
    let Some(engine) = engine else {
        return Ok(MasterTurnLaunch {
            command: master_command(executable, &home, place.worktree, model, None)?,
            sandbox: master_sandbox(
                place.root,
                place.runtime_ref,
                place.repository,
                place.worktree,
                &home,
                executable,
                &connection.endpoint,
            )?,
            home,
            credential,
            tool_record: None,
        });
    };
    let sandbox = master_tools_sandbox(
        place.root,
        place.runtime_ref,
        place.repository,
        place.worktree,
        &home,
        executable,
        &connection.endpoint,
        engine,
    )?;
    let drop = sandbox
        .sandbox()
        .agent_tmp()
        .join(crate::team::tool::DROP_FILE);
    let command = master_command(
        executable,
        &home,
        place.worktree,
        model,
        Some(crate::engine::claude_host::MasterTools {
            engine,
            drop: &drop,
        }),
    )?;
    Ok(MasterTurnLaunch {
        command,
        sandbox,
        home,
        credential,
        tool_record: Some(drop),
    })
}

/// Makes the private home of one master turn inside its turn folder, so everything the
/// harness writes there goes with the folder. Input: the owner root and the turn's
/// runtime reference. Output: the new 0700 home, or a refusal when the folder is not the
/// engine's or the home already exists.
#[cfg(unix)]
fn master_turn_home(root: &Root, runtime_ref: &RequestId) -> Result<PathBuf, EngineError> {
    let name = format!("master-runtime-{}", runtime_ref.as_str());
    let runtime = Root::open(&root.path.join(&name))?;
    verify_master_runtime(root, &runtime, &name)?;
    rustix::fs::mkdirat(&runtime.directory, MASTER_HOME, Mode::RWXU).map_err(io_error)?;
    let home = runtime.path.join(MASTER_HOME);
    let made = std::fs::symlink_metadata(&home).map_err(io_error)?;
    if !made.is_dir() || made.mode() & 0o7777 != 0o700 {
        return Err(failure("master_home_unsafe"));
    }
    verify_master_runtime(root, &runtime, &name)?;
    Ok(home)
}

/// Refuses a master turn home where the owner-only turn directory cannot be made.
/// Takes the owner root and turn reference, both unused. Returns `master_capture_unavailable`.
#[cfg(not(unix))]
fn master_turn_home(_root: &Root, _runtime_ref: &RequestId) -> Result<PathBuf, EngineError> {
    Err(failure("master_capture_unavailable"))
}

/// Owns one master turn's private folder in the owner generation, with its capture,
/// instructions, sandbox profile and temp folder, and removes it with whatever the turn
/// left there when the launch lets go of it, on every path out. Nothing reads the folder
/// afterwards: the reply is stored. Input: the owner root and the runtime reference.
pub(super) struct MasterRuntimeFolder {
    path: PathBuf,
}

impl MasterRuntimeFolder {
    pub(super) fn new(root: &Root, runtime_ref: &RequestId) -> Self {
        Self {
            path: root
                .path
                .join(format!("master-runtime-{}", runtime_ref.as_str())),
        }
    }
}

impl Drop for MasterRuntimeFolder {
    /// Removes the turn folder without following links inside it; a folder already gone
    /// is fine and a failure is logged by code, since engine shutdown and the next start
    /// remove what is left. Input: the folder being dropped. Output: none.
    fn drop(&mut self) {
        match std::fs::remove_dir_all(&self.path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => eprintln!("master_runtime_cleanup_failed"),
        }
    }
}

/// Confirms a Codex master command keeps the policy of its tools switch. Tools off (DEC-076):
/// read-only sandbox, approvals never granted, and no shell, patch, web, goal or MCP tools.
/// Tools on (DEC-093): approvals never granted, no apps, plugins, goals or Codex's own
/// sub-agents, Codex's own sandbox off inside the master's, and exactly one MCP server, the
/// engine's `delegate` server served by exactly the expected engine executable on the turn's
/// own drop file. Every `-c` setting must be one the switch allows. Input: the command and, for tools on, the expected delegate
/// server parts. Output: Ok, `master_delegate_unverified` for any other server, or the
/// unsupported-backend refusal.
fn require_master_tool_policy(
    command: &Command,
    tools: Option<crate::engine::claude_host::MasterTools<'_>>,
) -> Result<(), EngineError> {
    let refused = || failure("master_runtime_backend_unsupported");
    let required: &[&str] = if tools.is_some() {
        &[
            "sandbox_mode=\"danger-full-access\"",
            "approval_policy=\"never\"",
            "features.apps=false",
            "features.plugins=false",
            "features.goals=false",
            "features.multi_agent=false",
            "features.multi_agent_v2=false",
        ]
    } else {
        &[
            "sandbox_mode=\"read-only\"",
            "approval_policy=\"never\"",
            "features.shell_tool=false",
            "features.apply_patch_freeform=false",
            "features.unified_exec=false",
            "features.goals=false",
            "web_search=\"disabled\"",
            "mcp_servers={}",
        ]
    };
    let fixed: &[&str] = if tools.is_some() {
        &MASTER_TOOLS_CONFIG
    } else {
        &MASTER_CONFIG
    };
    let args = command
        .get_args()
        .map(|arg| arg.to_str())
        .collect::<Option<Vec<_>>>()
        .ok_or_else(refused)?;
    let mut settings = Vec::new();
    let mut rest = args.as_slice();
    while let ["-c", setting, tail @ ..] = rest {
        settings.push(*setting);
        rest = tail;
    }
    if rest != ["app-server", "--listen", "stdio://"] {
        return Err(refused());
    }
    let mut servers = 0_usize;
    for setting in &settings {
        match tools {
            Some(tools) if setting.starts_with(DELEGATE_SERVERS) => {
                servers += 1;
                if !delegate_only(setting, tools) {
                    return Err(failure("master_delegate_unverified"));
                }
            }
            _ if fixed.contains(setting) || setting.starts_with("model=\"") => {}
            _ => return Err(refused()),
        }
    }
    if servers != usize::from(tools.is_some())
        || required.iter().any(|setting| !fixed.contains(setting))
        || fixed.iter().any(|setting| !settings.contains(setting))
    {
        return Err(refused());
    }
    assert!(settings.len() >= fixed.len());
    Ok(())
}

/// Checks that a Codex `mcp_servers=` setting names only the engine's `delegate` server: one
/// `crazyagent-team` entry whose command is exactly the expected engine executable and whose
/// arguments are exactly the tool server command and the turn's drop file. Input: the setting
/// and the expected parts. Output: whether it does.
fn delegate_only(setting: &str, tools: crate::engine::claude_host::MasterTools<'_>) -> bool {
    let Ok(parsed) = toml::from_str::<toml::Table>(setting) else {
        return false;
    };
    let Some(servers) = parsed.get("mcp_servers").and_then(toml::Value::as_table) else {
        return false;
    };
    let Some(server) = servers
        .get(crate::team::tool::SERVER_NAME)
        .and_then(toml::Value::as_table)
    else {
        return false;
    };
    let named = |value: Option<&toml::Value>, path: &Path| {
        value
            .and_then(toml::Value::as_str)
            .is_some_and(|text| Some(text) == path.to_str())
    };
    let args = server.get("args").and_then(toml::Value::as_array);
    parsed.len() == 1
        && servers.len() == 1
        && server.len() == 2
        && named(server.get("command"), tools.engine)
        && args.is_some_and(|args| {
            args.len() == 2
                && args[0].as_str() == Some(crate::team::tool::SERVER_COMMAND)
                && named(args.get(1), tools.drop)
        })
}

/// Confirms a built master turn names exactly the delegate server the engine registered: for
/// tools on, the registered engine executable at its canonical path, outside every folder the
/// turn's sandbox lets it write, serving the drop file in the turn's own sandbox temporary
/// folder, and for tools off no server. Inputs: the backend, the turn and, for tools on, the
/// registered engine executable. Output: Ok, or the refusal code of the check that failed.
fn require_master_delegate(
    backend: MasterBackend,
    turn: &MasterTurnLaunch,
    engine: Option<&crate::engine::native_auth_host::RegisteredNativeAuthHost>,
) -> Result<(), EngineError> {
    let drop = turn
        .sandbox
        .sandbox()
        .agent_tmp()
        .join(crate::team::tool::DROP_FILE);
    let expected = match engine {
        Some(engine) => Some(crate::engine::claude_host::MasterTools {
            engine: delegate_engine(engine)?,
            drop: &drop,
        }),
        None => None,
    };
    if expected.is_some_and(|tools| turn.sandbox.sandbox().writes(tools.engine)) {
        return Err(failure("master_delegate_writable"));
    }
    match backend {
        MasterBackend::Codex => require_master_tool_policy(&turn.command, expected)?,
        MasterBackend::Claude => {
            crate::engine::claude_host::require_delegate(&turn.command, expected)?
        }
        MasterBackend::Rcli => return Err(failure("master_runtime_backend_unsupported")),
    }
    if turn.tool_record.as_deref() != expected.map(|tools| tools.drop) {
        return Err(failure("master_delegate_unverified"));
    }
    Ok(())
}

/// Names the engine executable a tools-on master's `delegate` server runs: the path the engine
/// registered, while the file there is still the one it captured and that path is canonical,
/// so no link in any component can redirect it. Input: the registered engine executable.
/// Output: its path, `native_auth_host_executable_changed`, or `master_delegate_unverified`.
fn delegate_engine(
    registered: &crate::engine::native_auth_host::RegisteredNativeAuthHost,
) -> Result<&Path, EngineError> {
    let path = registered.verified_path()?;
    let canonical =
        std::fs::canonicalize(path).map_err(|_| failure("master_delegate_unverified"))?;
    if canonical != path {
        return Err(failure("master_delegate_unverified"));
    }
    Ok(path)
}

fn validate_master_binding(prepared: &PreparedMasterTurn) -> Result<(), EngineError> {
    let binding = prepared.profile.binding();
    let lease = &prepared.lease;
    if !matches!(
        prepared.session.selection.backend,
        MasterBackend::Codex | MasterBackend::Claude
    ) {
        return Err(failure("master_runtime_backend_unsupported"));
    }
    if prepared.session.current_turn.is_none()
        || prepared.attempt.completion.is_some()
        || prepared.attempt.request.project_id != prepared.session.project_id
        || !prepared.attempt_on_current_turn()
        || !prepared
            .session
            .admits_attempt(&prepared.attempt.config_id, true)
        || binding.connection != prepared.attempt.connection
        || lease.state != LeaseState::Ready
        || lease.project_id != prepared.session.project_id
        || lease.task_id != prepared.session.bootstrap_task_id
        || lease.owner_session != prepared.session.session_id
        || lease.native_descendant.is_some()
        || Path::new(&lease.path) != prepared.cwd.as_path()
        || Path::new(&lease.repository) != prepared.repository.as_path()
        || !prepared.cwd.is_absolute()
        || !prepared.repository.is_absolute()
    {
        return Err(failure("master_runtime_binding"));
    }
    if std::fs::canonicalize(&prepared.cwd).map_err(io_error)? != prepared.cwd
        || std::fs::canonicalize(&prepared.repository).map_err(io_error)? != prepared.repository
        || !std::fs::symlink_metadata(&prepared.cwd)
            .map_err(io_error)?
            .is_dir()
    {
        return Err(failure("master_runtime_binding"));
    }
    Ok(())
}

/// Builds the Codex master app-server command: tools off with the fixed read-only settings,
/// or with `tools` the tools-on settings and one `mcp_servers` entry for the engine's
/// `delegate` server. Inputs: the registered executable, the home, the working directory, the
/// model and the tools-on parts. Output: the command, or `master_runtime_binding`.
#[cfg(unix)]
fn master_command(
    executable: &Path,
    home: &Path,
    cwd: &Path,
    model: &str,
    tools: Option<crate::engine::claude_host::MasterTools<'_>>,
) -> Result<Command, EngineError> {
    if !executable.is_absolute()
        || !home.is_absolute()
        || !cwd.is_absolute()
        || model.len() > 256
        || !model
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
        || !model
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_.:/@+".contains(&byte))
    {
        return Err(failure("master_runtime_binding"));
    }
    let mut command = Command::new(executable);
    if model != ACCOUNT_DEFAULT_MODEL {
        command.arg("-c").arg(format!("model=\"{model}\""));
    }
    let fixed: &[&str] = match tools {
        None => &MASTER_CONFIG,
        Some(_) => &MASTER_TOOLS_CONFIG,
    };
    for config in fixed {
        command.arg("-c").arg(config);
    }
    if let Some(tools) = tools {
        command.arg("-c").arg(delegate_servers(&tools)?);
    }
    command
        .args(["app-server", "--listen", "stdio://"])
        .env_clear()
        .env("HOME", home)
        .env("CODEX_HOME", home)
        .env("TMPDIR", home)
        .env("PATH", "/usr/bin:/bin")
        .env("LANG", "C")
        .env("LC_ALL", "C")
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    assert_eq!(command.get_program(), executable.as_os_str());
    assert_eq!(command.get_current_dir(), Some(cwd));
    Ok(command)
}

/// Refuses the master app-server command where the engine cannot give it its own
/// process group to stop as a whole. Takes the executable, home, working directory and
/// model, all unused. Returns `master_host_unavailable`.
#[cfg(not(unix))]
fn master_command(
    _executable: &Path,
    _home: &Path,
    _cwd: &Path,
    _model: &str,
    _tools: Option<crate::engine::claude_host::MasterTools<'_>>,
) -> Result<Command, EngineError> {
    Err(failure("master_host_unavailable"))
}

/// Renders the one `mcp_servers` setting of a tools-on Codex master: the engine executable
/// serving its `delegate` tool with the turn's drop file. Input: the tools-on parts. Output:
/// the setting, or `master_runtime_binding` for a relative path or one that is not text.
#[cfg(unix)]
fn delegate_servers(
    tools: &crate::engine::claude_host::MasterTools<'_>,
) -> Result<String, EngineError> {
    let text = |path: &Path| {
        path.to_str()
            .filter(|text| path.is_absolute() && !text.chars().any(char::is_control))
            .map(|text| toml_edit::Value::from(text).to_string())
            .ok_or_else(|| failure("master_runtime_binding"))
    };
    let setting = format!(
        "{DELEGATE_SERVERS}{{{}={{command={},args=[{},{}]}}}}",
        crate::team::tool::SERVER_NAME,
        text(tools.engine)?,
        toml_edit::Value::from(crate::team::tool::SERVER_COMMAND),
        text(tools.drop)?
    );
    assert!(delegate_only(&setting, *tools));
    Ok(setting)
}

/// Builds and installs the Seatbelt boundary for one master turn. The master writes only
/// in the project repository, its leased worktree, the account's managed home and the
/// turn's own temp folder; it reads the harness install folder and the turn directory,
/// and reaches only its model endpoint (HTTPS for a native account) because its tools
/// are off. Output: the installed sandbox, or `master_sandbox_unavailable` on a refusal.
#[cfg(unix)]
pub(super) fn master_sandbox(
    root: &Root,
    runtime_ref: &RequestId,
    repository: &Path,
    worktree: &Path,
    home: &Path,
    executable: &Path,
    endpoint: &ModelEndpoint,
) -> Result<InstalledSandbox, EngineError> {
    turn_sandbox(
        root,
        runtime_ref,
        repository,
        worktree,
        home,
        executable,
        endpoint,
        None,
    )
}

/// Builds and installs the Seatbelt boundary for one master turn with its tools on (DEC-093).
/// The master writes only in its leased worktree, its turn home and the turn's temp folder;
/// it reads the project repository, the harness install folder, the turn directory and the
/// folder of the engine executable that serves its `delegate` tool, and reaches only its
/// model endpoint. Output: the installed sandbox, or `master_sandbox_unavailable`.
#[cfg(unix)]
#[allow(clippy::too_many_arguments)]
pub(super) fn master_tools_sandbox(
    root: &Root,
    runtime_ref: &RequestId,
    repository: &Path,
    worktree: &Path,
    home: &Path,
    executable: &Path,
    endpoint: &ModelEndpoint,
    engine: &Path,
) -> Result<InstalledSandbox, EngineError> {
    turn_sandbox(
        root,
        runtime_ref,
        repository,
        worktree,
        home,
        executable,
        endpoint,
        Some(engine),
    )
}

#[cfg(unix)]
#[allow(clippy::too_many_arguments)]
fn turn_sandbox(
    root: &Root,
    runtime_ref: &RequestId,
    repository: &Path,
    worktree: &Path,
    home: &Path,
    executable: &Path,
    endpoint: &ModelEndpoint,
    engine: Option<&Path>,
) -> Result<InstalledSandbox, EngineError> {
    let refused = |_: SandboxError| failure("master_sandbox_unavailable");
    let remote = match endpoint {
        ModelEndpoint::Native {} => SandboxRemote::AnyHost(HTTPS_PORT),
        ModelEndpoint::Http { url } => SandboxRemote::for_endpoint(url).map_err(refused)?,
    };
    let name = format!("master-runtime-{}", runtime_ref.as_str());
    let runtime = Root::open(&root.path.join(&name))?;
    verify_master_runtime(root, &runtime, &name)?;
    let tmp = runtime.path.join(MASTER_TMP);
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&tmp)
        .map_err(io_error)?;
    let home = std::fs::canonicalize(home).map_err(io_error)?;
    let install = std::fs::canonicalize(executable)
        .map_err(io_error)?
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| failure("master_sandbox_unavailable"))?;
    let network = SandboxNetwork::Only(vec![remote]);
    let tools = Toolchains::detect(repository, &ToolHomes::owner().map_err(refused)?);
    let sandbox = match engine {
        None => ProjectSandbox::new(repository, &home, &tmp, network)
            .and_then(|sandbox| sandbox.allow_worktree(worktree)),
        Some(engine) => {
            let folder = std::fs::canonicalize(engine)
                .map_err(io_error)?
                .parent()
                .map(Path::to_path_buf)
                .ok_or_else(|| failure("master_sandbox_unavailable"))?;
            ProjectSandbox::new(worktree, &home, &tmp, network)
                .and_then(|sandbox| sandbox.allow_read(repository))
                .and_then(|sandbox| sandbox.allow_read(&folder))
        }
    }
    .and_then(|sandbox| sandbox.allow_read(&install))
    .and_then(|sandbox| sandbox.allow_read(&runtime.path))
    .and_then(|sandbox| sandbox.allow_tools(&tools))
    .and_then(|sandbox| sandbox.guard_engine(&root.path))
    .map_err(refused)?;
    let installed =
        InstalledSandbox::install(sandbox, &runtime.path.join(MASTER_PROFILE)).map_err(refused)?;
    verify_master_runtime(root, &runtime, &name)?;
    assert!(installed.profile().starts_with(&runtime.path));
    assert!(
        !installed
            .sandbox()
            .writable_roots()
            .any(|writable| installed.profile().starts_with(writable))
    );
    assert!(
        engine.is_none()
            || installed
                .sandbox()
                .writable_roots()
                .all(|writable| writable != repository)
    );
    Ok(installed)
}

/// Refuses the master turn boundary where the owner-only turn directory cannot be made.
/// Takes the owner root, turn reference, repository, worktree, home, executable and
/// endpoint, all unused. Returns `master_sandbox_unavailable`.
#[cfg(not(unix))]
pub(super) fn master_sandbox(
    _root: &Root,
    _runtime_ref: &RequestId,
    _repository: &Path,
    _worktree: &Path,
    _home: &Path,
    _executable: &Path,
    _endpoint: &ModelEndpoint,
) -> Result<InstalledSandbox, EngineError> {
    Err(failure("master_sandbox_unavailable"))
}

/// Refuses the tools-on master turn boundary where the owner-only turn directory cannot be
/// made. Takes the owner root, turn reference, repository, worktree, home, executable,
/// endpoint and engine executable, all unused. Returns `master_sandbox_unavailable`.
#[cfg(not(unix))]
#[allow(clippy::too_many_arguments)]
pub(super) fn master_tools_sandbox(
    _root: &Root,
    _runtime_ref: &RequestId,
    _repository: &Path,
    _worktree: &Path,
    _home: &Path,
    _executable: &Path,
    _endpoint: &ModelEndpoint,
    _engine: &Path,
) -> Result<InstalledSandbox, EngineError> {
    Err(failure("master_sandbox_unavailable"))
}

#[cfg(unix)]
pub(super) fn allocate_master_capture(
    root: &Root,
) -> Result<(RequestId, RequestId, File), EngineError> {
    for _ in 0..CAPTURE_ALLOCATION_ATTEMPTS {
        root.verify()?;
        let runtime_ref = RequestId::new(random_id()?).map_err(io_error)?;
        let capture_ref = RequestId::new(random_id()?).map_err(io_error)?;
        if runtime_ref == capture_ref {
            continue;
        }
        let name = format!("master-runtime-{}", runtime_ref.as_str());
        match rustix::fs::mkdirat(&root.directory, name.as_str(), Mode::RWXU) {
            Ok(()) => {}
            Err(rustix::io::Errno::EXIST) => continue,
            Err(error) => return Err(io_error(error)),
        }
        root.verify()?;
        let runtime = Root::open(&root.path.join(&name))?;
        verify_master_runtime(root, &runtime, &name)?;
        let capture_name = format!("capture-{}.jsonl", capture_ref.as_str());
        let capture = runtime.open_key(&capture_name, true)?;
        verify_master_capture(&runtime, &capture_name, &capture)?;
        capture.sync_all().map_err(io_error)?;
        runtime.directory.sync_all().map_err(io_error)?;
        root.directory.sync_all().map_err(io_error)?;
        verify_master_runtime(root, &runtime, &name)?;
        verify_master_capture(&runtime, &capture_name, &capture)?;
        assert_ne!(runtime_ref, capture_ref);
        assert_eq!(capture.metadata().map_err(io_error)?.len(), 0);
        return Ok((runtime_ref, capture_ref, capture));
    }
    Err(failure("master_capture_unavailable"))
}

/// Refuses a master capture where the owner-only turn directory cannot be made.
/// Takes the owner root, unused. Returns `master_capture_unavailable`.
#[cfg(not(unix))]
pub(super) fn allocate_master_capture(
    _root: &Root,
) -> Result<(RequestId, RequestId, File), EngineError> {
    Err(failure("master_capture_unavailable"))
}

/// Writes the master's startup instruction into its private turn directory as the 0600
/// file Claude Code reads through `--append-system-prompt-file`. Input: the owner root,
/// the turn's runtime reference from `allocate_master_capture` and the text. Output: the
/// file's absolute path, or a plain refusal when the directory or file is not the engine's.
#[cfg(unix)]
fn write_master_instructions(
    root: &Root,
    runtime_ref: &RequestId,
    text: &str,
) -> Result<PathBuf, EngineError> {
    if text.len() > crate::launch_context::MASTER_INSTRUCTIONS_BYTES_MAX {
        return Err(failure("master_instructions_too_large"));
    }
    let name = format!("master-runtime-{}", runtime_ref.as_str());
    let runtime = Root::open(&root.path.join(&name))?;
    verify_master_runtime(root, &runtime, &name)?;
    let mut file = runtime.open_key(MASTER_INSTRUCTIONS, true)?;
    file.write_all(text.as_bytes()).map_err(io_error)?;
    file.sync_all().map_err(io_error)?;
    runtime.directory.sync_all().map_err(io_error)?;
    let named = rustix::fs::statat(
        &runtime.directory,
        MASTER_INSTRUCTIONS,
        AtFlags::SYMLINK_NOFOLLOW,
    )
    .map_err(io_error)?;
    let held = file.metadata().map_err(io_error)?;
    if !held.is_file()
        || held.uid() != rustix::process::geteuid().as_raw()
        || held.mode() & 0o7777 != 0o600
        || held.nlink() != 1
        || held.len() != text.len() as u64
        || named.st_ino != held.ino()
        || named.st_dev as u64 != held.dev()
    {
        return Err(failure("master_instructions_identity"));
    }
    verify_master_runtime(root, &runtime, &name)?;
    Ok(runtime.path.join(MASTER_INSTRUCTIONS))
}

/// Refuses the master instruction file where the owner-only turn directory cannot be made.
/// Takes the owner root, turn reference and text, all unused.
/// Returns `master_capture_unavailable`.
#[cfg(not(unix))]
fn write_master_instructions(
    _root: &Root,
    _runtime_ref: &RequestId,
    _text: &str,
) -> Result<PathBuf, EngineError> {
    Err(failure("master_capture_unavailable"))
}

#[cfg(unix)]
fn verify_master_runtime(root: &Root, runtime: &Root, name: &str) -> Result<(), EngineError> {
    root.verify()?;
    runtime.verify()?;
    let named =
        rustix::fs::statat(&root.directory, name, AtFlags::SYMLINK_NOFOLLOW).map_err(io_error)?;
    let held = runtime.directory.metadata().map_err(io_error)?;
    if !held.is_dir()
        || Identity::of(&held) != runtime.identity
        || held.uid() != rustix::process::geteuid().as_raw()
        || held.mode() & 0o7777 != 0o700
        || named.st_ino != held.ino()
        || named.st_dev as u64 != held.dev()
    {
        return Err(failure("master_capture_identity"));
    }
    Ok(())
}

#[cfg(unix)]
fn verify_master_capture(runtime: &Root, name: &str, capture: &File) -> Result<(), EngineError> {
    runtime.verify()?;
    let named = rustix::fs::statat(&runtime.directory, name, AtFlags::SYMLINK_NOFOLLOW)
        .map_err(io_error)?;
    let held = capture.metadata().map_err(io_error)?;
    if !held.is_file()
        || held.uid() != rustix::process::geteuid().as_raw()
        || held.mode() & 0o7777 != 0o600
        || held.nlink() != 1
        || held.len() != 0
        || named.st_ino != held.ino()
        || named.st_dev as u64 != held.dev()
    {
        return Err(failure("master_capture_identity"));
    }
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::super::Root;
    use super::*;
    use std::collections::BTreeMap;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    use std::path::{Path, PathBuf};

    fn private_root() -> tempfile::TempDir {
        let root = tempfile::Builder::new()
            .prefix("ca-master-launch-")
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        root
    }

    #[test]
    fn master_command_pins_model_and_read_only_stdio_without_spawning() {
        let command = master_command(
            Path::new("/registered/codex"),
            Path::new("/private/profile"),
            Path::new("/leased/worktree"),
            "admitted-model",
            None,
        )
        .unwrap();
        assert_eq!(command.get_program(), "/registered/codex");
        assert_eq!(
            command.get_current_dir(),
            Some(Path::new("/leased/worktree"))
        );
        let args = command
            .get_args()
            .map(|arg| arg.to_str().unwrap())
            .collect::<Vec<_>>();
        for required in [
            "model=\"admitted-model\"",
            "model_provider=\"openai\"",
            "cli_auth_credentials_store=\"file\"",
            "sandbox_mode=\"read-only\"",
            "approval_policy=\"never\"",
            "features.shell_tool=false",
            "features.apply_patch_freeform=false",
            "features.unified_exec=false",
            "web_search=\"disabled\"",
            "mcp_servers={}",
        ] {
            assert!(
                args.windows(2).any(|pair| pair == ["-c", required]),
                "{required}"
            );
        }
        assert!(args.ends_with(&["app-server", "--listen", "stdio://"]));
    }

    #[test]
    fn master_command_leaves_the_account_default_model_unpinned() {
        let command = master_command(
            Path::new("/registered/codex"),
            Path::new("/private/profile"),
            Path::new("/leased/worktree"),
            ACCOUNT_DEFAULT_MODEL,
            None,
        )
        .unwrap();
        let args = command
            .get_args()
            .map(|arg| arg.to_str().unwrap())
            .collect::<Vec<_>>();
        assert!(
            !args.iter().any(|arg| arg.starts_with("model=")),
            "{args:?}"
        );
        assert!(
            args.windows(2)
                .any(|pair| pair == ["-c", "sandbox_mode=\"read-only\""])
        );
        assert!(args.ends_with(&["app-server", "--listen", "stdio://"]));
    }

    #[test]
    fn master_command_uses_only_explicit_private_profile_environment() {
        let command = master_command(
            Path::new("/registered/codex"),
            Path::new("/private/profile"),
            Path::new("/leased/worktree"),
            "admitted-model",
            None,
        )
        .unwrap();
        let environment = command
            .get_envs()
            .map(|(key, value)| (key.to_str().unwrap(), value.unwrap().to_str().unwrap()))
            .collect::<BTreeMap<_, _>>();
        assert_eq!(
            environment,
            BTreeMap::from([
                ("CODEX_HOME", "/private/profile"),
                ("HOME", "/private/profile"),
                ("LANG", "C"),
                ("LC_ALL", "C"),
                ("PATH", "/usr/bin:/bin"),
                ("TMPDIR", "/private/profile"),
            ])
        );
    }

    #[test]
    fn master_command_rejects_invalid_model_or_nonabsolute_path() {
        for model in [
            "",
            "model\"\nfeatures.shell_tool=true",
            "--dangerously-bypass-approvals-and-sandbox",
        ] {
            assert!(
                master_command(
                    Path::new("/registered/codex"),
                    Path::new("/private/profile"),
                    Path::new("/leased/worktree"),
                    model,
                    None
                )
                .is_err()
            );
        }
        for (executable, home, cwd) in [
            ("codex", "/private/profile", "/leased/worktree"),
            ("/registered/codex", "profile", "/leased/worktree"),
            ("/registered/codex", "/private/profile", "worktree"),
        ] {
            assert!(
                master_command(
                    Path::new(executable),
                    Path::new(home),
                    Path::new(cwd),
                    "model",
                    None
                )
                .is_err()
            );
        }
    }

    #[test]
    fn master_instructions_are_one_private_file_in_the_turn_directory() {
        let temporary = private_root();
        let root = Root::open(temporary.path()).unwrap();
        let (runtime_ref, _, _) = allocate_master_capture(&root).unwrap();
        let text = "# Project context from CrazyAgent\n\nThe codename is BLUE-HERON-77.\n";
        let path = write_master_instructions(&root, &runtime_ref, text).unwrap();
        assert_eq!(
            path,
            temporary
                .path()
                .join(format!("master-runtime-{}", runtime_ref.as_str()))
                .join("instructions.md")
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
        let metadata = std::fs::symlink_metadata(&path).unwrap();
        assert!(metadata.is_file());
        assert_eq!(metadata.mode() & 0o7777, 0o600);
        assert_eq!(metadata.nlink(), 1);
        assert_eq!(metadata.uid(), rustix::process::geteuid().as_raw());
        assert!(write_master_instructions(&root, &runtime_ref, "replacement").is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
        let (other, _, _) = allocate_master_capture(&root).unwrap();
        let oversized = "x".repeat(crate::launch_context::MASTER_INSTRUCTIONS_BYTES_MAX + 1);
        assert!(write_master_instructions(&root, &other, &oversized).is_err());
        assert!(
            !temporary
                .path()
                .join(format!("master-runtime-{}", other.as_str()))
                .join("instructions.md")
                .exists()
        );
    }

    #[test]
    fn a_master_turn_folder_goes_with_everything_its_turn_wrote_when_the_launch_lets_go() {
        use std::process::{Command, Stdio};
        let temporary = private_root();
        let base = temporary.path().canonicalize().unwrap();
        let root = Root::open(&base).unwrap();
        let repository = base.join("repository");
        let worktree = base.join("worktrees").join("lease");
        let home = base.join("profile-home");
        let install = base.join("install").join("bin");
        for directory in [&repository, &worktree, &home, &install] {
            private_directory(directory);
        }
        let executable = install.join("codex");
        std::fs::write(&executable, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
        let kept = repository.join("keep.txt");
        std::fs::write(&kept, "kept\n").unwrap();
        let (runtime_ref, _, capture) = allocate_master_capture(&root).unwrap();
        let folder = MasterRuntimeFolder::new(&root, &runtime_ref);
        write_master_instructions(&root, &runtime_ref, "instructions").unwrap();
        let sandbox = master_sandbox(
            &root,
            &runtime_ref,
            &repository,
            &worktree,
            &home,
            &executable,
            &ModelEndpoint::Native {},
        )
        .unwrap();
        let turn = base.join(format!("master-runtime-{}", runtime_ref.as_str()));
        let tmp = turn.join(MASTER_TMP);
        let mut harness = Command::new("/bin/sh");
        harness
            .arg("-c")
            .arg("mkdir \"$0/work\" && echo agent > \"$0/work/scratch.txt\" && ln -s \"$1\" \"$0/link\"")
            .arg(&tmp)
            .arg(&kept)
            .current_dir(&worktree);
        let mut child = sandbox.wrap(&harness).unwrap();
        child
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        assert!(crate::spawn_gate::status(&mut child).unwrap().success());
        assert!(tmp.join("work/scratch.txt").is_file());
        assert!(
            std::fs::symlink_metadata(tmp.join("link"))
                .unwrap()
                .is_symlink()
        );
        let (other, _, _) = allocate_master_capture(&root).unwrap();
        drop((capture, sandbox, folder));
        assert!(!turn.exists());
        assert_eq!(std::fs::read_to_string(&kept).unwrap(), "kept\n");
        assert!(
            base.join(format!("master-runtime-{}", other.as_str()))
                .is_dir()
        );
        drop(MasterRuntimeFolder::new(&root, &runtime_ref));
    }

    fn private_directory(path: &Path) {
        std::fs::create_dir_all(path).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
    }

    #[test]
    fn master_sandbox_lets_a_real_child_write_only_in_the_project_worktree_and_its_own_dirs() {
        use std::process::{Command, Stdio};
        let temporary = private_root();
        let base = temporary.path().canonicalize().unwrap();
        let root = Root::open(&base).unwrap();
        let repository = base.join("repository");
        let worktree = base.join("worktrees").join("lease");
        let home = base.join("profile-home");
        let install = base.join("install").join("bin");
        for directory in [&repository, &worktree, &home, &install] {
            private_directory(directory);
        }
        let executable = install.join("codex");
        std::fs::write(&executable, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
        let (runtime_ref, capture_ref, capture) = allocate_master_capture(&root).unwrap();
        let native = ModelEndpoint::Native {};
        let sandbox = master_sandbox(
            &root,
            &runtime_ref,
            &repository,
            &worktree,
            &home,
            &executable,
            &native,
        )
        .unwrap();
        let turn = base.join(format!("master-runtime-{}", runtime_ref.as_str()));
        let tmp = turn.join(MASTER_TMP);
        assert_eq!(sandbox.profile(), turn.join(MASTER_PROFILE));
        assert_eq!(
            std::fs::read_to_string(sandbox.profile()).unwrap(),
            sandbox.sandbox().profile()
        );
        assert_eq!(
            std::fs::symlink_metadata(&tmp).unwrap().mode() & 0o7777,
            0o700
        );
        assert_eq!(
            sandbox
                .sandbox()
                .writable_roots()
                .map(PathBuf::as_path)
                .collect::<Vec<_>>(),
            [
                repository.as_path(),
                home.as_path(),
                tmp.as_path(),
                worktree.as_path()
            ]
        );
        assert!(
            sandbox
                .sandbox()
                .profile()
                .contains("(allow network-outbound (remote ip \"*:443\"))")
        );
        assert!(!sandbox.sandbox().profile().contains("(allow network*)"));

        let master = master_command(&executable, &home, &worktree, "admitted-model", None).unwrap();
        let wrapped = sandbox.wrap(&master).unwrap();
        assert_eq!(wrapped.get_program(), crate::sandbox::SANDBOX_EXEC);
        let args = wrapped
            .get_args()
            .map(|arg| arg.to_str().unwrap())
            .collect::<Vec<_>>();
        let mut start = vec!["-f", sandbox.profile().to_str().unwrap()];
        start.extend(crate::sandbox::SESSION_SHIM);
        start.push(executable.to_str().unwrap());
        assert_eq!(args[..start.len()], start);
        assert!(
            args.windows(2)
                .any(|pair| pair == ["-c", "sandbox_mode=\"read-only\""])
        );
        assert!(args.ends_with(&["app-server", "--listen", "stdio://"]));
        assert_eq!(wrapped.get_current_dir(), Some(worktree.as_path()));
        let environment = wrapped
            .get_envs()
            .map(|(key, value)| (key.to_str().unwrap(), value.unwrap().to_str().unwrap()))
            .collect::<BTreeMap<_, _>>();
        let mut expected = BTreeMap::from([
            ("CODEX_HOME", home.to_str().unwrap()),
            ("HOME", home.to_str().unwrap()),
            ("LANG", "C"),
            ("LC_ALL", "C"),
            ("PATH", "/usr/bin:/bin"),
            ("TMPDIR", tmp.to_str().unwrap()),
        ]);
        expected.extend(crate::sandbox::AGENT_GIT_ENV);
        expected.extend(crate::sandbox::AGENT_TLS_ENV);
        assert_eq!(environment, expected);

        let claude = crate::engine::claude_host::launch_command(
            &executable,
            &worktree,
            &home,
            ACCOUNT_DEFAULT_MODEL,
            None,
        )
        .unwrap();
        let claude = sandbox.wrap(&claude).unwrap();
        assert_eq!(claude.get_program(), crate::sandbox::SANDBOX_EXEC);
        let claude_args = claude
            .get_args()
            .map(|arg| arg.to_str().unwrap())
            .collect::<Vec<_>>();
        let mut start = vec!["-f", sandbox.profile().to_str().unwrap()];
        start.extend(crate::sandbox::SESSION_SHIM);
        start.push(executable.to_str().unwrap());
        assert_eq!(claude_args[..start.len()], start);
        assert!(claude_args.windows(2).any(|pair| pair == ["--tools", ""]));
        assert_eq!(claude.get_current_dir(), Some(worktree.as_path()));
        let claude_environment = claude
            .get_envs()
            .map(|(key, value)| (key.to_str().unwrap(), value.unwrap().to_str().unwrap()))
            .collect::<BTreeMap<_, _>>();
        let mut expected_claude = BTreeMap::from([
            ("CLAUDE_CONFIG_DIR", home.to_str().unwrap()),
            ("CLAUDE_SECURESTORAGE_CONFIG_DIR", home.to_str().unwrap()),
            ("HOME", home.to_str().unwrap()),
            ("LANG", "C"),
            ("LC_ALL", "C"),
            (
                "PATH",
                "/usr/bin:/bin:/usr/sbin:/sbin:/opt/homebrew/bin:/usr/local/bin",
            ),
            ("TERM", "dumb"),
            ("TMPDIR", tmp.to_str().unwrap()),
        ]);
        expected_claude.extend(crate::sandbox::AGENT_GIT_ENV);
        expected_claude.extend(crate::sandbox::AGENT_TLS_ENV);
        assert_eq!(claude_environment, expected_claude);

        let run = |target: &Path| {
            let mut harness = Command::new("/bin/sh");
            harness
                .arg("-c")
                .arg("echo agent >> \"$0\"")
                .arg(target)
                .current_dir(&worktree);
            let mut child = sandbox.wrap(&harness).unwrap();
            child
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            crate::spawn_gate::status(&mut child).unwrap().success()
        };
        for allowed in [
            worktree.join("work.txt"),
            repository.join("shared.txt"),
            home.join("account.txt"),
            tmp.join("scratch.txt"),
        ] {
            assert!(run(&allowed), "{}", allowed.display());
            assert_eq!(std::fs::read_to_string(&allowed).unwrap(), "agent\n");
        }
        let profile_text = std::fs::read(sandbox.profile()).unwrap();
        for refused in [
            base.join("escape.txt"),
            turn.join("escape.txt"),
            turn.join(format!("capture-{}.jsonl", capture_ref.as_str())),
            turn.join(MASTER_PROFILE),
            install.join("codex"),
        ] {
            assert!(!run(&refused), "{}", refused.display());
        }
        assert!(!base.join("escape.txt").exists());
        assert!(!turn.join("escape.txt").exists());
        assert_eq!(capture.metadata().unwrap().len(), 0);
        assert_eq!(std::fs::read(sandbox.profile()).unwrap(), profile_text);
        assert_eq!(std::fs::read_to_string(&executable).unwrap(), "#!/bin/sh\n");

        assert!(
            master_sandbox(
                &root,
                &runtime_ref,
                &repository,
                &worktree,
                &home,
                &executable,
                &native
            )
            .is_err()
        );
        let ftp = ModelEndpoint::Http {
            url: "ftp://127.0.0.1/v1".into(),
        };
        for (repository, worktree, endpoint) in [
            (base.join("missing"), worktree.clone(), &native),
            (repository.clone(), base.join("missing"), &native),
            (
                repository.clone(),
                base.join("..").join("worktrees"),
                &native,
            ),
            (repository.clone(), worktree.clone(), &ftp),
        ] {
            let (fresh, _, _) = allocate_master_capture(&root).unwrap();
            assert_eq!(
                master_sandbox(
                    &root,
                    &fresh,
                    &repository,
                    &worktree,
                    &home,
                    &executable,
                    endpoint
                )
                .unwrap_err()
                .0,
                "master_sandbox_unavailable"
            );
            assert!(
                !base
                    .join(format!("master-runtime-{}", fresh.as_str()))
                    .join(MASTER_PROFILE)
                    .exists()
            );
        }
        let (local, _, _) = allocate_master_capture(&root).unwrap();
        let endpoint = ModelEndpoint::Http {
            url: "http://127.0.0.1:11434/v1".into(),
        };
        let text = master_sandbox(
            &root,
            &local,
            &repository,
            &worktree,
            &home,
            &executable,
            &endpoint,
        )
        .unwrap()
        .sandbox()
        .profile();
        assert!(text.contains("(allow network-outbound (remote ip \"localhost:11434\"))"));
        assert!(!text.contains("*:443"));
        assert!(!text.contains("(allow network*)"));
    }

    #[test]
    fn a_tools_on_master_writes_only_in_its_worktree_and_own_folders_and_runs_the_engine() {
        use std::process::{Command, Stdio};
        let temporary = private_root();
        let base = temporary.path().canonicalize().unwrap();
        let root = Root::open(&base).unwrap();
        let repository = base.join("repository");
        let worktree = base.join("worktrees").join("lease");
        let home = base.join("profile-home");
        let install = base.join("install").join("bin");
        let engine_bin = base.join("engine").join("bin");
        let elsewhere = base.join("elsewhere");
        for directory in [
            &repository,
            &worktree,
            &home,
            &install,
            &engine_bin,
            &elsewhere,
        ] {
            private_directory(directory);
        }
        let script = |path: &Path, body: &str| {
            std::fs::write(
                path,
                format!("#!/bin/sh\n[ \"$1\" = first-exec ] && exit 3\n{body}"),
            )
            .unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
            let first = crate::spawn_gate::first_run(
                Command::new(path).arg("first-exec").current_dir(&base),
            );
            assert_eq!(first.code(), Some(3));
        };
        let executable = install.join("claude");
        script(&executable, "exit 0\n");
        let engine = engine_bin.join("crazyagent");
        script(
            &engine,
            "[ \"$1\" = team-mcp ] || exit 2\necho '{\"task\":\"probe\"}' >> \"$2\"\n",
        );
        let stray = elsewhere.join("tool");
        script(&stray, "exit 0\n");
        let main = repository.join("main.txt");
        std::fs::write(&main, "owner\n").unwrap();
        let (runtime_ref, capture_ref, capture) = allocate_master_capture(&root).unwrap();
        let sandbox = master_tools_sandbox(
            &root,
            &runtime_ref,
            &repository,
            &worktree,
            &home,
            &executable,
            &ModelEndpoint::Native {},
            &engine,
        )
        .unwrap();
        let turn = base.join(format!("master-runtime-{}", runtime_ref.as_str()));
        let tmp = turn.join(MASTER_TMP);
        assert_eq!(
            sandbox
                .sandbox()
                .writable_roots()
                .map(PathBuf::as_path)
                .collect::<Vec<_>>(),
            [worktree.as_path(), home.as_path(), tmp.as_path()]
        );
        let profile = sandbox.sandbox().profile();
        let owner = PathBuf::from(std::env::var_os("HOME").unwrap())
            .canonicalize()
            .unwrap();
        for denied in [".claude", ".codex", "Library/Keychains"] {
            assert!(
                profile.contains(&format!("(subpath \"{}\")", owner.join(denied).display())),
                "{denied}"
            );
        }
        assert!(profile.contains("(global-name \"com.apple.SecurityServer\")"));
        assert!(profile.contains("(allow network-outbound (remote ip \"*:443\"))"));
        assert!(!profile.contains("(allow network*)"));
        let run = |program: &Path, args: &[&Path]| {
            let mut child = Command::new(program);
            child.args(args).current_dir(&worktree);
            let mut child = sandbox.wrap(&child).unwrap();
            child
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            crate::spawn_gate::status(&mut child).unwrap().success()
        };
        let write = |target: &Path| {
            run(
                Path::new("/bin/sh"),
                &[
                    Path::new("-c"),
                    Path::new("echo agent >> \"$1\""),
                    Path::new("sh"),
                    target,
                ],
            )
        };
        for allowed in [
            worktree.join("work.txt"),
            home.join("account.txt"),
            tmp.join("scratch.txt"),
        ] {
            assert!(write(&allowed), "{}", allowed.display());
            assert_eq!(std::fs::read_to_string(&allowed).unwrap(), "agent\n");
        }
        for refused in [
            main.clone(),
            repository.join("new.txt"),
            base.join("escape.txt"),
            turn.join("escape.txt"),
            turn.join(format!("capture-{}.jsonl", capture_ref.as_str())),
            engine_bin.join("planted"),
        ] {
            assert!(!write(&refused), "{}", refused.display());
        }
        assert_eq!(std::fs::read_to_string(&main).unwrap(), "owner\n");
        assert!(!repository.join("new.txt").exists());
        assert_eq!(capture.metadata().unwrap().len(), 0);
        let drop = tmp.join(crate::team::tool::DROP_FILE);
        assert!(run(&engine, &[Path::new("team-mcp"), &drop]));
        assert_eq!(
            std::fs::read_to_string(&drop).unwrap(),
            "{\"task\":\"probe\"}\n"
        );
        assert!(!run(&stray, &[]));
    }

    #[test]
    fn capture_allocates_distinct_actual_private_artifacts() {
        let temporary = private_root();
        let root = Root::open(temporary.path()).unwrap();
        let (first_runtime, first_capture, first) = allocate_master_capture(&root).unwrap();
        let (second_runtime, second_capture, second) = allocate_master_capture(&root).unwrap();
        assert_ne!(first_runtime, second_runtime);
        assert_ne!(first_capture, second_capture);
        assert_ne!(first_runtime, first_capture);
        assert_ne!(
            first.metadata().unwrap().ino(),
            second.metadata().unwrap().ino()
        );
        for (runtime_ref, capture_ref, capture) in [
            (first_runtime, first_capture, first),
            (second_runtime, second_capture, second),
        ] {
            let runtime = temporary
                .path()
                .join(format!("master-runtime-{}", runtime_ref.as_str()));
            let path = runtime.join(format!("capture-{}.jsonl", capture_ref.as_str()));
            let named = std::fs::symlink_metadata(path).unwrap();
            let held = capture.metadata().unwrap();
            assert_eq!(
                std::fs::symlink_metadata(runtime).unwrap().mode() & 0o7777,
                0o700
            );
            assert_eq!(named.ino(), held.ino());
            assert_eq!(named.dev(), held.dev());
            assert_eq!(held.mode() & 0o7777, 0o600);
            assert_eq!(held.nlink(), 1);
            assert_eq!(held.len(), 0);
            assert_eq!(held.uid(), rustix::process::geteuid().as_raw());
        }
    }

    #[test]
    fn capture_refuses_replaced_owner_root_without_writing_replacement() {
        let temporary = private_root();
        let path = temporary.path().join("runtime");
        std::fs::create_dir(&path).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        let root = Root::open(&path).unwrap();
        std::fs::rename(&path, temporary.path().join("retained")).unwrap();
        std::fs::create_dir(&path).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(allocate_master_capture(&root).is_err());
        assert_eq!(std::fs::read_dir(path).unwrap().count(), 0);
        assert_eq!(
            std::fs::read_dir(temporary.path().join("retained"))
                .unwrap()
                .count(),
            0
        );
    }

    #[test]
    fn capture_refuses_owner_root_permission_drift() {
        let temporary = private_root();
        let root = Root::open(temporary.path()).unwrap();
        std::fs::set_permissions(temporary.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(allocate_master_capture(&root).is_err());
        assert_eq!(std::fs::read_dir(temporary.path()).unwrap().count(), 0);
    }
}

#[cfg(all(test, target_os = "macos"))]
#[path = "master_host_launch/dec_090_tests.rs"]
mod dec_090_tests;

#[cfg(all(test, target_os = "macos"))]
#[path = "master_host_launch/live_claude_tests.rs"]
mod live_claude_tests;
#[cfg(all(test, target_os = "macos"))]
#[path = "master_host_launch/live_codex_tests.rs"]
mod live_codex_tests;

#[cfg(all(test, target_os = "macos"))]
#[path = "master_host_launch/claude_turn_tests.rs"]
mod claude_turn_tests;

#[cfg(all(test, target_os = "macos"))]
#[path = "master_host_launch/codex_turn_tests.rs"]
mod codex_turn_tests;
