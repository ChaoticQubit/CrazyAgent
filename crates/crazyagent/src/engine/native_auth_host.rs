use crate::engine::{EngineError, io_error};
use rustix::fs::{Mode, OFlags};
use sha2::{Digest, Sha256};
use std::fs::{File, Metadata};
use std::os::fd::OwnedFd;
use std::os::unix::fs::{FileExt, MetadataExt};
use std::path::{Path, PathBuf};

const EXECUTABLE_BYTES_MAX: u64 = 1024 * 1024 * 1024;

pub async fn run_native_auth_custodian(
    runtime_dir: &Path,
    input: OwnedFd,
) -> Result<(), EngineError> {
    entry::run(runtime_dir, input).await.map(|_| ())
}

pub async fn run_native_auth_custodian_with_codex(
    runtime_dir: &Path,
    input: OwnedFd,
    executable: &Path,
    expected_sha256: &str,
) -> Result<(), EngineError> {
    entry::run_with_codex(runtime_dir, input, executable, expected_sha256)
        .await
        .map(|_| ())
}

pub async fn run_native_auth_custodian_with_claude(
    runtime_dir: &Path,
    input: OwnedFd,
    executable: &Path,
    expected_sha256: &str,
) -> Result<(), EngineError> {
    entry::run_with_claude(runtime_dir, input, executable, expected_sha256)
        .await
        .map(|_| ())
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct ExecutableIdentity {
    device: u64,
    inode: u64,
    mode: u32,
    owner: u32,
    length: u64,
    modified: (i64, i64),
    changed: (i64, i64),
}

impl ExecutableIdentity {
    fn of(metadata: &Metadata) -> Result<Self, EngineError> {
        if !metadata.is_file()
            || metadata.uid() != rustix::process::geteuid().as_raw()
            || metadata.mode() & 0o022 != 0
            || metadata.mode() & 0o100 == 0
            || !(1..=EXECUTABLE_BYTES_MAX).contains(&metadata.len())
        {
            return Err(EngineError("native_auth_host_executable_unsafe".into()));
        }
        Ok(Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            mode: metadata.mode(),
            owner: metadata.uid(),
            length: metadata.len(),
            modified: (metadata.mtime(), metadata.mtime_nsec()),
            changed: (metadata.ctime(), metadata.ctime_nsec()),
        })
    }
}

pub(crate) struct RegisteredNativeAuthHost {
    path: PathBuf,
    file: File,
    identity: ExecutableIdentity,
    digest: [u8; 32],
}

impl RegisteredNativeAuthHost {
    fn open(path: &Path) -> Result<File, EngineError> {
        if !path.is_absolute() {
            return Err(EngineError("native_auth_host_executable_unsafe".into()));
        }
        rustix::fs::open(
            path,
            OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK,
            Mode::empty(),
        )
        .map(File::from)
        .map_err(io_error)
    }

    fn hash(file: &File, length: u64) -> Result<[u8; 32], EngineError> {
        assert!((1..=EXECUTABLE_BYTES_MAX).contains(&length));
        let mut digest = Sha256::new();
        let mut buffer = [0; 16 * 1024];
        let mut offset = 0;
        while offset < length {
            let requested = (length - offset).min(buffer.len() as u64) as usize;
            let read = match file.read_at(&mut buffer[..requested], offset) {
                Ok(read) => read,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(io_error(error)),
            };
            if read == 0 {
                return Err(EngineError("native_auth_host_executable_changed".into()));
            }
            offset += read as u64;
            assert!(offset <= length);
            digest.update(&buffer[..read]);
        }
        Ok(digest.finalize().into())
    }

    pub(crate) fn capture(path: &Path) -> Result<Self, EngineError> {
        let file = Self::open(path)?;
        let identity = ExecutableIdentity::of(&file.metadata().map_err(io_error)?)?;
        let digest = Self::hash(&file, identity.length)?;
        if ExecutableIdentity::of(&file.metadata().map_err(io_error)?)? != identity
            || ExecutableIdentity::of(&std::fs::symlink_metadata(path).map_err(io_error)?)?
                != identity
        {
            return Err(EngineError("native_auth_host_executable_changed".into()));
        }
        Ok(Self {
            path: path.to_path_buf(),
            file,
            identity,
            digest,
        })
    }

    pub(crate) fn verified_path(&self) -> Result<&Path, EngineError> {
        let named = Self::open(&self.path)?;
        if ExecutableIdentity::of(&self.file.metadata().map_err(io_error)?)? != self.identity
            || ExecutableIdentity::of(&named.metadata().map_err(io_error)?)? != self.identity
            || Self::hash(&self.file, self.identity.length)? != self.digest
            || ExecutableIdentity::of(&self.file.metadata().map_err(io_error)?)? != self.identity
            || ExecutableIdentity::of(&std::fs::symlink_metadata(&self.path).map_err(io_error)?)?
                != self.identity
        {
            return Err(EngineError("native_auth_host_executable_changed".into()));
        }
        Ok(&self.path)
    }

    pub(crate) fn verified_sha256(&self) -> Result<String, EngineError> {
        self.verified_path()?;
        Ok(self.digest.iter().map(|byte| format!("{byte:02x}")).collect())
    }
}

fn custodian_command(
    executable: &Path,
    runtime_dir: &Path,
    codex: Option<&RegisteredNativeAuthHost>,
) -> Result<std::process::Command, EngineError> {
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};
    let mut command = Command::new(executable);
    command.arg("native-auth-custodian").arg(runtime_dir);
    if let Some(codex) = codex {
        command
            .arg("--codex-executable")
            .arg(codex.verified_path()?)
            .arg("--codex-sha256")
            .arg(codex.verified_sha256()?);
    }
    command
        .env_clear()
        .current_dir(runtime_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(if std::env::var_os("CRAZYAGENT_DEBUG_CUSTODIAN").is_some() {
            Stdio::inherit()
        } else {
            Stdio::null()
        })
        .process_group(0);
    Ok(command)
}

pub(in crate::engine) async fn dispatch(
    work: crate::storage::NativeAuthWork,
    host: crate::engine::owner::transport::NativeAuthDispatchHost,
) -> Result<(), EngineError> {
    let executable = host.registered_executable()?;
    let harness = work.fact().binding.connection.harness.as_str();
    let codex = if harness == "codex" { host.registered_codex_executable()? } else { None };
    let claude = if harness == "claude" { host.registered_claude_executable()? } else { None };
    let runtime_dir = host.owner_runtime_dir()?;
    let launch = host.prepare_native_auth_custody(work).await?;
    let mut command = custodian_command(executable.verified_path()?, &runtime_dir, codex.as_deref())?;
    if let Some(claude) = claude.as_ref() {
        command.arg("--claude-executable").arg(claude.verified_path()?)
            .arg("--claude-sha256").arg(claude.verified_sha256()?);
    }
    let mut child = command.spawn().map_err(io_error)?;
    let Some(input) = child.stdin.take() else {
        custody::cleanup_direct_child(
            &mut child,
            std::time::Instant::now() + std::time::Duration::from_secs(2),
        )
        .map_err(io_error)?;
        return Err(EngineError("native_auth_host_input_unavailable".into()));
    };
    if let Err((error, mut child)) = host.retain_child(child) {
        custody::cleanup_direct_child(
            &mut child,
            std::time::Instant::now() + std::time::Duration::from_secs(2),
        )
        .map_err(io_error)?;
        return Err(error);
    }
    launch.write_bootstrap(input)?;
    loop {
        if let Some(status) = host.try_wait_child()? {
            return if status.success() {
                Ok(())
            } else {
                Err(EngineError("native_auth_host_child_failed".into()))
            };
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

pub(in crate::engine) mod claude_auth;

pub(in crate::engine) mod codex_auth;
pub(super) mod custody;
#[cfg(test)]
mod custody_session_tests;
#[cfg(test)]
mod custody_tests;
pub(in crate::engine) mod entry;
#[cfg(test)]
mod entry_tests;
#[cfg(test)]
mod executable_tests;
#[cfg(test)]
mod hash_backend_tests;
