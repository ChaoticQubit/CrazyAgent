use crate::engine::{EngineError, io_error};
use rustix::fs::{Mode, OFlags};
use sha2::{Digest, Sha256};
use std::fs::{File, Metadata};
use std::os::fd::OwnedFd;
use std::os::unix::fs::{FileExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;

const EXECUTABLE_BYTES_MAX: u64 = 1024 * 1024 * 1024;
/// The PATH every sign-in or token renewal child runs with, no folder added: a script
/// harness starts through its resolved interpreter instead ([`launch::handed_command`],
/// [`sign_in_command`]).
pub(crate) const SIGN_IN_PATH: &str = "/usr/bin:/bin";

pub use launch::{HandedWord, handed_words};
pub(crate) use launch::{LaunchWord, handed_command};

/// Starts a token renewal child in the engine with no search path: the harness file itself,
/// or for a script that asks `env` for its interpreter, such as an npm-built Codex asking
/// for `node`, that interpreter's absolute file and then the script
/// ([`crate::harnesses::interactive::launch_words`]). A sign-in custodian never resolves:
/// it runs the words the engine handed it ([`launch`]). Input: the harness's real file.
/// Output: the command with those words, or the tool resolver's owner line.
pub(crate) fn sign_in_command(executable: &Path) -> Result<std::process::Command, EngineError> {
    let words = crate::harnesses::interactive::launch_words(executable)
        .map_err(|problem| EngineError(problem.0))?;
    let (program, script) = words
        .split_first()
        .ok_or_else(|| EngineError("native_auth_launch_invalid".into()))?;
    assert!(Path::new(program).is_absolute());
    let mut command = std::process::Command::new(program);
    command.args(script);
    Ok(command)
}

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
    launch: &[HandedWord<'_>],
) -> Result<(), EngineError> {
    entry::run_with_codex(runtime_dir, input, executable, expected_sha256, launch)
        .await
        .map(|_| ())
}

pub async fn run_native_auth_custodian_with_claude(
    runtime_dir: &Path,
    input: OwnedFd,
    executable: &Path,
    expected_sha256: &str,
    launch: &[HandedWord<'_>],
) -> Result<(), EngineError> {
    entry::run_with_claude(runtime_dir, input, executable, expected_sha256, launch)
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
    /// The identity of a file the engine may pin and run: a regular file of 1 byte to
    /// [`EXECUTABLE_BYTES_MAX`] the owner may run, owned by the engine's user or by root
    /// (only root can change a root file), with no group or world write. Input: the file's
    /// metadata. Output: the identity, or `native_auth_host_executable_unsafe`.
    fn of(metadata: &Metadata) -> Result<Self, EngineError> {
        let owner = metadata.uid();
        if !metadata.is_file()
            || (owner != rustix::process::geteuid().as_raw() && owner != 0)
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

/// The pin's file rule alone, so a harness search refuses exactly the files a pin would
/// ([`ExecutableIdentity::of`]). Input: the real file's metadata. Output: Ok, or
/// `native_auth_host_executable_unsafe`.
pub(crate) fn pinnable(metadata: &Metadata) -> Result<(), EngineError> {
    ExecutableIdentity::of(metadata).map(|_| ())
}

/// The identity a host's checks compare against: the captured one, or the one a re-hash
/// proved after only the change time moved ([`RegisteredNativeAuthHost::retime`]), with when
/// the checks first saw it off macOS.
#[derive(Clone, Copy)]
struct Pin {
    identity: ExecutableIdentity,
    #[cfg(not(target_os = "macos"))]
    seen: std::time::Instant,
}

pub(crate) struct RegisteredNativeAuthHost {
    path: PathBuf,
    file: File,
    identity: ExecutableIdentity,
    digest: [u8; 32],
    entry: Option<PathBuf>,
    retimed: std::sync::Mutex<Option<Pin>>,
    launch: Vec<LaunchWord>,
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
            entry: None,
            retimed: std::sync::Mutex::new(None),
            launch: Vec::new(),
            #[cfg(not(target_os = "macos"))]
            seen,
            #[cfg(not(target_os = "macos"))]
            settled: std::sync::atomic::AtomicBool::new(false),
            #[cfg(all(test, not(target_os = "macos")))]
            rehashes: std::sync::atomic::AtomicUsize::new(0),
        })
    }

    /// Captures a harness the engine found through a PATH entry: the entry, often a link
    /// into a versioned install, is resolved to its real file now and that file is pinned
    /// as `capture` pins it; the entry is kept for `current`. Input: the absolute entry.
    /// Output: the host, or `native_auth_host_unavailable` when no file is behind the entry.
    pub(crate) fn capture_entry(entry: &Path) -> Result<Self, EngineError> {
        let real = crate::paths::launch_executable(entry, std::env::var_os("PATH").as_deref())
            .map_err(|_| EngineError("native_auth_host_unavailable".into()))?;
        let mut host = Self::capture(&real)?;
        assert!(host.entry.is_none());
        host.entry = Some(entry.to_path_buf());
        Ok(host)
    }

    /// The host while its entry, if any, still leads to the pinned file and that file
    /// verifies, a change of its ctime alone included; with an entry, a fresh capture when it
    /// leads to another path or inode, or when other metadata of the pinned inode changed and
    /// its bytes still hash to the pin. A rewrite in place stays refused. Input: the shared
    /// host. Output: the host to use, the verify error, or `native_auth_host_unavailable`.
    pub(crate) fn current(self: &Arc<Self>) -> Result<Arc<Self>, EngineError> {
        let Some(entry) = self.entry.as_deref() else {
            self.verified_path()?;
            return Ok(self.clone());
        };
        let real = crate::paths::launch_executable(entry, std::env::var_os("PATH").as_deref())
            .map_err(|_| EngineError("native_auth_host_unavailable".into()))?;
        if real == self.path && self.verified_path().is_ok() {
            return Ok(self.clone());
        }
        let fresh = Self::capture_entry(entry)?;
        assert_eq!(fresh.entry.as_deref(), Some(entry));
        if fresh.path == self.path
            && (fresh.identity.device, fresh.identity.inode)
                == (self.identity.device, self.identity.inode)
            && fresh.digest != self.digest
        {
            return Err(EngineError("native_auth_host_executable_changed".into()));
        }
        Ok(Arc::new(fresh))
    }

    /// Whether the harness this host pins is gone from disk: the entry a launch resolves (the
    /// pinned path for a host with no entry) leads to nothing, so it answers NotFound. A file
    /// that is there but changed or unsafe is not gone and stays refused. Input: none.
    /// Output: true when the entry leads to nothing.
    pub(crate) fn vanished(&self) -> bool {
        let named = self.entry.as_deref().unwrap_or(&self.path);
        matches!(std::fs::metadata(named), Err(error) if error.kind() == std::io::ErrorKind::NotFound)
    }

    /// The PATH entry this host was captured through, which a launch resolves again each
    /// time. Input: none. Output: the entry, or none for a host captured by its real path.
    pub(crate) fn entry(&self) -> Option<&Path> {
        self.entry.as_deref()
    }

    /// Returns the captured path while the retained file, the file at that path and the path
    /// show the pinned identity; a change of the change time alone is hashed once and adopted
    /// ([`Self::retime`]). macOS moves ctime on every write by the fine clock, so equal identity
    /// means the hashed bytes; off macOS the coarse tick can hide a same-size write, so there
    /// the file is hashed again until a match [`settles`] it. Output: the path, or the refusal.
    pub(crate) fn verified_path(&self) -> Result<&Path, EngineError> {
        let pin = self.pin();
        let named = Self::open(&self.path)?;
        let seen = self.identities(&named)?;
        if seen != [pin.identity; 3] {
            self.retime(pin.identity, &named, seen)?;
            return Ok(&self.path);
        }
        #[cfg(not(target_os = "macos"))]
        if !self.settled.load(std::sync::atomic::Ordering::Acquire) {
            let started = std::time::Instant::now();
            let wall = std::time::SystemTime::now();
            #[cfg(test)]
            self.rehashes
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            if crate::engine::off_worker(|| Self::hash(&self.file, pin.identity.length))?
                != self.digest
            {
                return Err(EngineError("native_auth_host_executable_changed".into()));
            }
            if settles(pin.seen, started, wall, pin.identity.changed) {
                let held = self
                    .retimed
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if held.map_or(self.identity, |kept| kept.identity) == pin.identity {
                    self.settled
                        .store(true, std::sync::atomic::Ordering::Release);
                }
            }
        }
        Ok(&self.path)
    }

    /// The identity the checks compare against now: the latest one [`Self::retime`] proved,
    /// else the captured one, with when the checks first saw it off macOS. Input: none.
    /// Output: the pin.
    fn pin(&self) -> Pin {
        let retimed = *self
            .retimed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        retimed.unwrap_or(Pin {
            identity: self.identity,
            #[cfg(not(target_os = "macos"))]
            seen: self.seen,
        })
    }

    /// The identities of the retained file, the file opened at the path and the path itself,
    /// in that order. Input: the file opened at the path. Output: the three, or
    /// `native_auth_host_executable_unsafe` or the read error for the first that fails.
    fn identities(&self, named: &File) -> Result<[ExecutableIdentity; 3], EngineError> {
        Ok([
            ExecutableIdentity::of(&self.file.metadata().map_err(io_error)?)?,
            ExecutableIdentity::of(&named.metadata().map_err(io_error)?)?,
            ExecutableIdentity::of(&std::fs::symlink_metadata(&self.path).map_err(io_error)?)?,
        ])
    }

    /// Adopts what the pinned inode shows now when all three looks agree and differ from the
    /// pin in ctime alone, as a hard link, an xattr or a backup tool leaves it, and its bytes
    /// still hash to the pin with the same three looks after the hash. Anything else, a write
    /// during the hash included, is refused. Inputs: the pin, the file at the path and the
    /// three looks. Output: Ok once adopted, or `native_auth_host_executable_changed`.
    fn retime(
        &self,
        pin: ExecutableIdentity,
        named: &File,
        seen: [ExecutableIdentity; 3],
    ) -> Result<(), EngineError> {
        let changed = || EngineError("native_auth_host_executable_changed".into());
        let now = seen[0];
        let others = ExecutableIdentity {
            changed: pin.changed,
            ..now
        };
        if seen != [now; 3] || others != pin {
            return Err(changed());
        }
        assert_ne!(now.changed, pin.changed);
        assert_eq!(now.length, self.identity.length);
        if crate::engine::off_worker(|| Self::hash(&self.file, now.length))? != self.digest
            || self.identities(named)? != [now; 3]
        {
            return Err(changed());
        }
        let mut held = self
            .retimed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *held = Some(Pin {
            identity: now,
            #[cfg(not(target_os = "macos"))]
            seen: std::time::Instant::now(),
        });
        #[cfg(not(target_os = "macos"))]
        self.settled
            .store(false, std::sync::atomic::Ordering::Release);
        drop(held);
        Ok(())
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

/// Builds the sign-in custodian's command with a cleared environment: this engine's own
/// file, the runtime folder and, for a Codex or Claude Code sign-in, the pinned harness and
/// the launch the engine resolved for it ([`launch::hand_over`]). Inputs: the engine file,
/// the runtime folder and the harness kind, file and words. Output: the command, or the pin
/// error of a file that changed.
fn custodian_command(
    executable: &Path,
    runtime_dir: &Path,
    harness: Option<(&str, &RegisteredNativeAuthHost, &[LaunchWord])>,
) -> Result<std::process::Command, EngineError> {
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};
    let mut command = Command::new(executable);
    command.arg("native-auth-custodian").arg(runtime_dir);
    if let Some((kind, harness, launch)) = harness {
        launch::hand_over(&mut command, kind, harness, launch)?;
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

/// The code of a sign-in the engine refuses before any custodian runs, because the harness
/// it would run is older than that harness's sign-in minimum (DEC-166).
const TOO_OLD: &str = "native_auth_harness_too_old";

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

/// The harness a custodian gets with the launch the engine resolved for it, a sign-in
/// waiting for the first probe run when that is still going ([`launch::resolved_after_probe`]),
/// or none when it cannot start, so the custodian ends the action without effect and it
/// settles unavailable. Inputs: the pinned harness, its kind and the action. Output: both,
/// or none.
async fn handed(
    harness: Arc<RegisteredNativeAuthHost>,
    kind: &str,
    action: &crate::engine::owner::NativeAuthAction,
) -> Option<(Arc<RegisteredNativeAuthHost>, Vec<LaunchWord>)> {
    let may_wait = matches!(
        action,
        crate::engine::owner::NativeAuthAction::SignIn { .. }
    );
    match launch::resolved_after_probe(&harness, kind, may_wait).await {
        Some(words) => Some((harness, words)),
        None => {
            eprintln!("native_auth_harness_unresolved harness={kind}");
            None
        }
    }
}

pub(in crate::engine) async fn dispatch(
    work: crate::storage::NativeAuthWork,
    host: crate::engine::owner::transport::NativeAuthDispatchHost,
) -> Result<(), EngineError> {
    let executable = host.registered_executable()?;
    let kind = work.fact().binding.connection.harness.as_str().to_owned();
    let registered = match kind.as_str() {
        "codex" => host.registered_codex_executable()?,
        "claude" => host.registered_claude_executable()?,
        _ => None,
    };
    let harness = match registered {
        Some(harness) => handed(harness, &kind, &work.fact().action).await,
        None => None,
    };
    let too_old = match harness.as_ref().map(|(harness, _)| harness.verified_path()) {
        Some(Ok(runs)) => host.harness_too_old(&kind, runs).await,
        _ => false,
    };
    let runtime_dir = host.owner_runtime_dir()?;
    let launch = host.prepare_native_auth_custody(work).await?;
    if too_old {
        host.record_custodian_failure(TOO_OLD)?;
        drop(launch);
        return Err(EngineError(TOO_OLD.into()));
    }
    let mut command = custodian_command(
        executable.verified_path()?,
        &runtime_dir,
        harness
            .as_ref()
            .map(|(harness, words)| (kind.as_str(), harness.as_ref(), words.as_slice())),
    )?;
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
mod launch;
