use crate::engine::{EngineError, io_error};
use rustix::fs::{Mode, OFlags};
use sha2::{Digest, Sha256};
use std::fs::{File, Metadata};
use std::os::fd::OwnedFd;
use std::os::unix::fs::{FileExt, MetadataExt};
use std::path::{Path, PathBuf};

const EXECUTABLE_BYTES_MAX: u64 = 1024 * 1024 * 1024;

/// How long after a harness identity was first seen a matching hash must start to prove its
/// bytes for good off macOS. A write after that stamps a change time more than one clock tick
/// and the coarsest file system time grain (2 s) past the seen one, so it cannot keep the
/// identity, and from then on the identity check alone sees every write.
#[cfg(any(test, not(target_os = "macos")))]
const SETTLE_AFTER: std::time::Duration = std::time::Duration::from_secs(3);

/// Whether a matching hash proves a harness's bytes for good: it started [`SETTLE_AFTER`]
/// past the first look at the identity by the monotonic clock and past the identity's change
/// time by the wall clock, so a change time ahead of the clock keeps the harness hashing.
/// Inputs: the first look, the hash start on the monotonic and wall clocks, and the change
/// time as (seconds, nanoseconds). Output: true when the harness settles.
#[cfg(any(test, not(target_os = "macos")))]
fn settles(
    seen: std::time::Instant,
    started: std::time::Instant,
    wall: std::time::SystemTime,
    changed: (i64, i64),
) -> bool {
    let after = i64::try_from(SETTLE_AFTER.as_secs()).unwrap();
    started >= seen + SETTLE_AFTER
        && wall
            .duration_since(std::time::UNIX_EPOCH)
            .is_ok_and(|wall| {
                (
                    i64::try_from(wall.as_secs()).unwrap_or(i64::MAX),
                    i64::from(wall.subsec_nanos()),
                ) >= (changed.0.saturating_add(after), changed.1)
            })
}

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
    #[cfg(not(target_os = "macos"))]
    seen: std::time::Instant,
    #[cfg(not(target_os = "macos"))]
    settled: std::sync::atomic::AtomicBool,
    #[cfg(all(test, not(target_os = "macos")))]
    rehashes: std::sync::atomic::AtomicUsize,
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
        #[cfg(not(target_os = "macos"))]
        let seen = std::time::Instant::now();
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
            #[cfg(not(target_os = "macos"))]
            seen,
            #[cfg(not(target_os = "macos"))]
            settled: std::sync::atomic::AtomicBool::new(false),
            #[cfg(all(test, not(target_os = "macos")))]
            rehashes: std::sync::atomic::AtomicUsize::new(0),
        })
    }

    /// Returns the captured path while the retained file and the file at that path still
    /// have the identity hashed at capture. On macOS every content write moves ctime by the
    /// fine clock, so equal identity means the hashed bytes without re-reading a harness of
    /// hundreds of MB. Linux moves file times by the coarse clock tick, so a same-size write
    /// in that tick keeps the identity: there the retained file is hashed again, off the
    /// tokio worker, until a matching hash [`settles`] it. Input: none. Output: the path, or
    /// `native_auth_host_executable_changed`.
    pub(crate) fn verified_path(&self) -> Result<&Path, EngineError> {
        let named = Self::open(&self.path)?;
        if ExecutableIdentity::of(&self.file.metadata().map_err(io_error)?)? != self.identity
            || ExecutableIdentity::of(&named.metadata().map_err(io_error)?)? != self.identity
            || ExecutableIdentity::of(&std::fs::symlink_metadata(&self.path).map_err(io_error)?)?
                != self.identity
        {
            return Err(EngineError("native_auth_host_executable_changed".into()));
        }
        #[cfg(not(target_os = "macos"))]
        if !self.settled.load(std::sync::atomic::Ordering::Acquire) {
            let started = std::time::Instant::now();
            let wall = std::time::SystemTime::now();
            #[cfg(test)]
            self.rehashes
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            if crate::engine::off_worker(|| Self::hash(&self.file, self.identity.length))?
                != self.digest
            {
                return Err(EngineError("native_auth_host_executable_changed".into()));
            }
            if settles(self.seen, started, wall, self.identity.changed) {
                self.settled
                    .store(true, std::sync::atomic::Ordering::Release);
            }
        }
        Ok(&self.path)
    }

    pub(crate) fn verified_sha256(&self) -> Result<String, EngineError> {
        self.verified_path()?;
        Ok(self
            .digest
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect())
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
        .stderr(
            if std::env::var_os("CRAZYAGENT_DEBUG_CUSTODIAN").is_some() {
                Stdio::inherit()
            } else {
                Stdio::piped()
            },
        )
        .process_group(0);
    Ok(command)
}

/// The longest failure code a custodian may name: the bound of a stored sign-in fact's code.
const CAUSE_BYTES_MAX: usize = 128;

/// What a custodian wrote on its error stream, kept up to one code and its line end and read
/// without ever waiting, so a custodian that writes more never blocks on a full pipe.
struct CustodianCause {
    stream: OwnedFd,
    text: Vec<u8>,
}

impl CustodianCause {
    /// Takes the custodian's error stream and makes reads on it return at once.
    /// Input: the stream. Output: the cause, empty so far.
    fn new(stream: std::process::ChildStderr) -> Result<Self, EngineError> {
        let stream = OwnedFd::from(stream);
        let flags = rustix::fs::fcntl_getfl(&stream).map_err(io_error)?;
        rustix::fs::fcntl_setfl(&stream, flags | OFlags::NONBLOCK).map_err(io_error)?;
        Ok(Self {
            stream,
            text: Vec::new(),
        })
    }

    /// Reads what the stream holds now, keeping at most one code, its line end and one more
    /// byte, so a longer line still reads as too long. Output: done once nothing waits in it.
    fn read(&mut self) -> Result<(), EngineError> {
        let mut buffer = [0; 256];
        loop {
            match rustix::io::read(&self.stream, &mut buffer) {
                Ok(0) | Err(rustix::io::Errno::AGAIN) => return Ok(()),
                Ok(size) => {
                    let room = (CAUSE_BYTES_MAX + 2).saturating_sub(self.text.len());
                    self.text.extend_from_slice(&buffer[..size.min(room)]);
                }
                Err(rustix::io::Errno::INTR) => {}
                Err(error) => return Err(io_error(error)),
            }
        }
    }

    /// The code the custodian named: its one line when that line is a code a sign-in fact can
    /// carry, 1 to 128 lowercase letters and `_`. Output: the code, or None for any other text.
    fn code(&self) -> Option<&str> {
        let line = self.text.strip_suffix(b"\n").unwrap_or(&self.text);
        let plain = !line.is_empty()
            && line.len() <= CAUSE_BYTES_MAX
            && line
                .iter()
                .all(|byte| byte.is_ascii_lowercase() || *byte == b'_');
        plain.then(|| std::str::from_utf8(line).ok()).flatten()
    }
}

pub(in crate::engine) async fn dispatch(
    work: crate::storage::NativeAuthWork,
    host: crate::engine::owner::transport::NativeAuthDispatchHost,
) -> Result<(), EngineError> {
    let executable = host.registered_executable()?;
    let harness = work.fact().binding.connection.harness.as_str();
    let codex = if harness == "codex" {
        host.registered_codex_executable()?
    } else {
        None
    };
    let claude = if harness == "claude" {
        host.registered_claude_executable()?
    } else {
        None
    };
    let runtime_dir = host.owner_runtime_dir()?;
    let launch = host.prepare_native_auth_custody(work).await?;
    let mut command =
        custodian_command(executable.verified_path()?, &runtime_dir, codex.as_deref())?;
    if let Some(claude) = claude.as_ref() {
        command
            .arg("--claude-executable")
            .arg(claude.verified_path()?)
            .arg("--claude-sha256")
            .arg(claude.verified_sha256()?);
    }
    let mut child = crate::spawn_gate::spawn(&mut command).map_err(io_error)?;
    let cause = child.stderr.take().map(CustodianCause::new).transpose();
    let streams = match (child.stdin.take(), cause) {
        (Some(input), Ok(cause)) => Ok((input, cause)),
        (None, _) => Err(EngineError("native_auth_host_input_unavailable".into())),
        (_, Err(error)) => Err(error),
    };
    let (input, mut cause) = match streams {
        Ok(streams) => streams,
        Err(error) => {
            custody::cleanup_direct_child(
                &mut child,
                std::time::Instant::now() + std::time::Duration::from_secs(2),
            )
            .map_err(io_error)?;
            return Err(error);
        }
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
        if let Some(cause) = cause.as_mut() {
            cause.read()?;
        }
        if let Some(status) = host.try_wait_child()? {
            if status.success() {
                return Ok(());
            }
            let code = match cause.as_mut() {
                Some(cause) => {
                    cause.read()?;
                    cause.code()
                }
                None => None,
            };
            eprintln!(
                "native_auth_custodian_failed cause={}",
                code.unwrap_or("native_auth_custodian_cause_unnamed")
            );
            if let Some(code) = code {
                host.record_custodian_failure(code)?;
            }
            return Err(EngineError("native_auth_host_child_failed".into()));
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

pub(in crate::engine) mod claude_auth;

#[cfg(test)]
mod bootstrap_pipe_tests;
#[cfg(test)]
mod cause_tests;
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
