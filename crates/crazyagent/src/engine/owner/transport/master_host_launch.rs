#[cfg(unix)]
use super::Identity;
use super::master_runtime::MasterHostLaunch;
use super::{EngineError, OwnerRuntimeHandle, Root, failure};
use crate::config::ModelEndpoint;
use crate::engine::bootstrap::NativeProfileUse;
use crate::engine::io_error;
use crate::engine::owner::is_hex_id;
#[cfg(unix)]
use crate::engine::owner::random_id;
use crate::master_session::MasterBackend;
use crate::protocol::RequestId;
use crate::sandbox::InstalledSandbox;
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

#[cfg(unix)]
const CAPTURE_ALLOCATION_ATTEMPTS: usize = 4;
#[cfg(unix)]
const MASTER_INSTRUCTIONS: &str = "instructions.md";
#[cfg(unix)]
const MASTER_TMP: &str = "tmp";
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
        let model = &prepared.attempt.connection.model.id;
        let executable = match prepared.session.selection.backend {
            MasterBackend::Codex => {
                require_master_tool_policy()?;
                self.registered_codex_executable()?
            }
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
        let command = if prepared.session.selection.backend == MasterBackend::Claude {
            let instructions = write_master_instructions(
                &self.authority.root,
                &runtime_ref,
                &prepared.instructions,
            )?;
            crate::engine::claude_host::launch_command(
                executable.verified_path()?,
                &prepared.cwd,
                &home,
                model,
                Some(instructions.as_path()),
            )?
        } else {
            master_command(executable.verified_path()?, &home, &prepared.cwd, model)?
        };
        let sandbox = master_sandbox(
            &self.authority.root,
            &runtime_ref,
            &prepared.repository,
            &prepared.cwd,
            &home,
            executable.verified_path()?,
            &prepared.attempt.connection.endpoint,
        )?;
        assert_eq!(command.get_current_dir(), Some(prepared.cwd.as_path()));
        assert_ne!(runtime_ref, capture_ref);
        Ok(MasterHostLaunch {
            command,
            sandbox,
            artifact_sha256,
            runtime_ref,
            capture_ref,
            capture,
            folder,
        })
    }
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

/// Confirms the master launch keeps the subscription harness contained under DEC-076:
/// read-only sandbox, approvals never granted, and no shell, patch, web, goal or MCP tools.
/// Input: none, the policy is the fixed launch configuration. Output: Ok when every
/// containment setting is present, otherwise the unsupported-backend refusal.
fn require_master_tool_policy() -> Result<(), EngineError> {
    let required = [
        "sandbox_mode=\"read-only\"",
        "approval_policy=\"never\"",
        "features.shell_tool=false",
        "features.apply_patch_freeform=false",
        "features.unified_exec=false",
        "features.goals=false",
        "web_search=\"disabled\"",
        "mcp_servers={}",
    ];
    if required
        .iter()
        .all(|setting| MASTER_CONFIG.contains(setting))
    {
        Ok(())
    } else {
        Err(failure("master_runtime_backend_unsupported"))
    }
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
        || prepared.attempt.request.task_id != prepared.session.bootstrap_task_id
        || prepared.attempt.config_id != prepared.session.config_id
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

#[cfg(unix)]
fn master_command(
    executable: &Path,
    home: &Path,
    cwd: &Path,
    model: &str,
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
    for config in MASTER_CONFIG {
        command.arg("-c").arg(config);
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
) -> Result<Command, EngineError> {
    Err(failure("master_host_unavailable"))
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
    let sandbox = ProjectSandbox::new(repository, &home, &tmp, SandboxNetwork::Only(vec![remote]))
        .and_then(|sandbox| sandbox.allow_worktree(worktree))
        .and_then(|sandbox| sandbox.allow_read(&install))
        .and_then(|sandbox| sandbox.allow_read(&runtime.path))
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
                    model
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
                    "model"
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

        let master = master_command(&executable, &home, &worktree, "admitted-model").unwrap();
        let wrapped = sandbox.wrap(&master).unwrap();
        assert_eq!(wrapped.get_program(), crate::sandbox::SANDBOX_EXEC);
        let args = wrapped
            .get_args()
            .map(|arg| arg.to_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            args[..3],
            [
                "-f",
                sandbox.profile().to_str().unwrap(),
                executable.to_str().unwrap()
            ]
        );
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
        assert_eq!(
            claude_args[..3],
            [
                "-f",
                sandbox.profile().to_str().unwrap(),
                executable.to_str().unwrap()
            ]
        );
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
