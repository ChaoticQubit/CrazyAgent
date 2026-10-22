use super::master_runtime::MasterHostLaunch;
use super::{EngineError, Identity, OwnerRuntimeHandle, Root, failure};
use crate::engine::bootstrap::NativeProfileUse;
use crate::engine::io_error;
use crate::engine::owner::{is_hex_id, random_id};
use crate::master_session::MasterBackend;
use crate::protocol::RequestId;
use crate::storage::PreparedMasterTurn;
use crate::worktrees::LeaseState;
use rustix::fs::{AtFlags, Mode};
use std::fs::File;
use std::os::unix::fs::MetadataExt;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};

const CAPTURE_ALLOCATION_ATTEMPTS: usize = 4;
const ACCOUNT_DEFAULT_MODEL: &str = "native-default";
const MASTER_CONFIG: [&str; 14] = [
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
        require_master_tool_policy()?;
        validate_master_binding(prepared)?;
        let binding = prepared.profile.binding();
        let host = self.identity();
        if binding.database_instance != host.db_instance
            || binding.engine_epoch != host.engine_epoch
        {
            return Err(failure("owner_runtime_stale"));
        }
        self.authority.root.verify()?;
        let executable = self.registered_codex_executable()?;
        let home = profile.profile_path()?;
        let command = master_command(
            executable.verified_path()?,
            &home,
            &prepared.cwd,
            &prepared.attempt.connection.model.id,
        )?;
        let artifact_sha256 = executable.verified_sha256()?;
        if !is_hex_id(&artifact_sha256) {
            return Err(failure("master_runtime_binding"));
        }
        if profile.profile_path()? != home {
            return Err(failure("master_runtime_binding"));
        }
        let (runtime_ref, capture_ref, capture) = allocate_master_capture(&self.authority.root)?;
        assert_eq!(command.get_current_dir(), Some(prepared.cwd.as_path()));
        assert_ne!(runtime_ref, capture_ref);
        Ok(MasterHostLaunch {
            command,
            artifact_sha256,
            runtime_ref,
            capture_ref,
            capture,
        })
    }
}

/// Confirms the master launch keeps the subscription harness contained under DEC-076:
/// read-only sandbox, approvals never granted, and no shell, patch, web or MCP tools.
/// Input: none, the policy is the fixed launch configuration. Output: Ok when every
/// containment setting is present, otherwise the unsupported-backend refusal.
fn require_master_tool_policy() -> Result<(), EngineError> {
    let required = [
        "sandbox_mode=\"read-only\"",
        "approval_policy=\"never\"",
        "features.shell_tool=false",
        "features.apply_patch_freeform=false",
        "features.unified_exec=false",
        "web_search=\"disabled\"",
        "mcp_servers={}",
    ];
    if required.iter().all(|setting| MASTER_CONFIG.contains(setting)) {
        Ok(())
    } else {
        Err(failure("master_runtime_backend_unsupported"))
    }
}

fn validate_master_binding(prepared: &PreparedMasterTurn) -> Result<(), EngineError> {
    let binding = prepared.profile.binding();
    let lease = &prepared.lease;
    if prepared.session.selection.backend != MasterBackend::Codex {
        return Err(failure("master_runtime_backend_unsupported"));
    }
    if prepared.session.current_turn.is_none()
        || prepared.attempt.completion.is_some()
        || prepared.attempt.request.project_id != prepared.session.project_id
        || prepared.attempt.request.task_id != prepared.session.bootstrap_task_id
        || prepared.attempt.config_id != prepared.session.config_id
        || binding.config_id != prepared.session.config_id
        || binding.source_hash != prepared.session.source_hash
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

pub(super) fn allocate_master_capture(root: &Root) -> Result<(RequestId, RequestId, File), EngineError> {
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

#[cfg(test)]
mod tests {
    use super::super::Root;
    use super::*;
    use std::collections::BTreeMap;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    use std::path::Path;

    fn private_root() -> tempfile::TempDir {
        let root = tempfile::Builder::new()
            .prefix("ca-master-launch-")
            .tempdir_in("/private/tmp")
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
        assert!(!args.iter().any(|arg| arg.starts_with("model=")), "{args:?}");
        assert!(args.windows(2).any(|pair| pair == ["-c", "sandbox_mode=\"read-only\""]));
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
