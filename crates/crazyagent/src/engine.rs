use crate::config::EffectiveConfig;
#[cfg(unix)]
use crate::config::validate_settings;
#[cfg(unix)]
use crate::platform::unix_socket::UnixListener;
use crate::platform::unix_socket::UnixStream;
use crate::protocol::*;
use crate::storage::{Job, Storage};
use std::collections::{BTreeMap, BTreeSet};
#[cfg(unix)]
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;
use tokio::io::AsyncReadExt;
#[cfg(unix)]
use tokio::io::AsyncWriteExt;
use tokio::sync::oneshot;

pub mod bootstrap;
pub(crate) mod claude_host;
pub mod codex_host;
#[cfg(target_os = "macos")]
pub(crate) mod keychain;
#[cfg(all(test, unix))]
mod master_host_fixture;
#[cfg(unix)]
mod native_auth_host;
#[cfg(not(unix))]
#[path = "engine/native_auth_host_unavailable.rs"]
mod native_auth_host;
pub mod owner;
/// The `owner-trace` feature's one stderr line per owner exchange: request id, operation
/// kind, claim, end and reply, never a payload, path or name. Off by default; only the test
/// farm's build turns it on, and no release build may ([`owner_trace::connection`]).
pub(crate) mod owner_trace;

#[cfg(unix)]
pub use native_auth_host::{
    CUSTODIAN_ARGUMENTS_MAX, HandedWord, handed_words, run_native_auth_custodian,
    run_native_auth_custodian_with_claude, run_native_auth_custodian_with_codex,
};

pub(crate) use bootstrap::NativeProfileCustody;
pub(crate) use native_auth_host::{RegisteredNativeAuthHost, pinnable};

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InputLease {
    pub session_id: SessionId,
    pub client_id: ClientId,
    pub controller_epoch: ControllerEpoch,
}

#[derive(Default)]
struct InputState {
    epochs: BTreeMap<SessionId, ControllerEpoch>,
    leases: BTreeMap<SessionId, InputLease>,
    retired: BTreeSet<SessionId>,
    epochs_seen: BTreeSet<(SessionId, ControllerEpoch)>,
}

impl InputState {
    fn assert_consistent(&self) {
        assert!(self.epochs.len() <= 128);
        assert!(self.epochs_seen.len() <= 4096);
        assert!(self.leases.len() <= self.epochs.len());
        assert!(self.retired.len() <= self.epochs.len());
        for (session, lease) in &self.leases {
            assert_eq!(session, &lease.session_id);
            assert_eq!(self.epochs.get(session), Some(&lease.controller_epoch));
            assert!(!self.retired.contains(session));
            assert!(
                self.epochs_seen
                    .contains(&(session.clone(), lease.controller_epoch.clone()))
            );
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineError(pub String);

impl std::fmt::Display for EngineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for EngineError {}

pub type ClientError = EngineError;

pub struct Client {
    socket: std::path::PathBuf,
    timeout: Duration,
    binding: Option<bootstrap::NativeBinding>,
}

impl Client {
    pub fn new(socket: std::path::PathBuf, timeout: Duration) -> Result<Self, ClientError> {
        if timeout.is_zero() || timeout > Duration::from_secs(30) {
            return Err(EngineError("invalid_client_timeout".into()));
        }
        Ok(Self {
            socket,
            timeout,
            binding: None,
        })
    }

    /// Admits this configured endpoint with its authenticated native owner.
    /// Consumes the ordinary client and fixes the requested native client kind.
    /// Returns one bound client and the owner identity from the same discovery.
    pub async fn with_owner(
        mut self,
        kind: owner::OwnerClientKind,
    ) -> Result<(Self, owner::OwnerClient, bootstrap::OwnerConnectionIdentity), ClientError> {
        let (owner, identity, binding) =
            bootstrap::discover_pair(&self.socket, kind, self.timeout).await?;
        self.socket = binding.socket();
        self.binding = Some(binding);
        Ok((self, owner, identity))
    }

    pub(crate) fn timeout(&self) -> Duration {
        self.timeout
    }

    pub(crate) fn into_unbound_configuration(
        self,
    ) -> Result<(std::path::PathBuf, Duration), ClientError> {
        if self.binding.is_some() {
            return Err(EngineError(
                "tui_requires_original_unbound_configuration".into(),
            ));
        }
        Ok((self.socket, self.timeout))
    }

    /// Refuses an engine request where the owner-only Unix socket does not exist.
    /// Takes the request, which is still validated so bad input reports its own error.
    /// Returns `engine_socket_unavailable` until the Windows named-pipe transport exists.
    #[cfg(not(unix))]
    pub async fn request(&self, request: WireRequest) -> Result<WireReply, ClientError> {
        request.validate().map_err(|e| EngineError(e.to_string()))?;
        Err(EngineError("engine_socket_unavailable".into()))
    }

    #[cfg(unix)]
    pub async fn request(&self, request: WireRequest) -> Result<WireReply, ClientError> {
        request.validate().map_err(|e| EngineError(e.to_string()))?;
        if let Some(binding) = &self.binding {
            binding.verify()?;
        }
        verify_endpoint(&self.socket)?;
        tokio::time::timeout(self.timeout, async {
            let mut stream = crate::spawn_gate::connect_unix(&self.socket)
                .await
                .map_err(io_error)?;
            verify_peer(&stream)?;
            if let Some(binding) = &self.binding {
                binding.verify()?;
            }
            let frame = encode_frame(&request).map_err(io_error)?;
            stream.write_all(&frame).await.map_err(io_error)?;
            let bytes = read_frame(&mut stream).await?;
            let reply: WireReply = serde_json::from_slice(&bytes[4..]).map_err(io_error)?;
            if reply.request_id != request.request_id {
                return Err(EngineError("reply_identity_mismatch".into()));
            }
            Ok(reply)
        })
        .await
        .map_err(|_| EngineError("outcome_unknown_retry_same_identity".into()))?
    }
}

/// The engine's request listener; none can exist where the Unix socket transport does not.
/// `bind` reports `engine_socket_unavailable` until the Windows named-pipe transport exists.
#[cfg(not(unix))]
pub enum Server {}

#[cfg(not(unix))]
impl Server {
    pub fn bind(
        _socket: &Path,
        _engine: std::sync::Arc<Engine>,
        _config: &EffectiveConfig,
    ) -> Result<Self, EngineError> {
        Err(EngineError("engine_socket_unavailable".into()))
    }

    pub async fn run(self, _stop: oneshot::Receiver<()>) -> Result<(), EngineError> {
        match self {}
    }
}

#[cfg(unix)]
pub struct Server {
    listener: UnixListener,
    engine: std::sync::Arc<Engine>,
    timeout: Duration,
    clients_max: usize,
    socket: std::path::PathBuf,
    socket_inode: u64,
    socket_device: u64,
    directory_inode: u64,
    directory_device: u64,
}

#[cfg(unix)]
impl Server {
    pub fn bind(
        socket: &Path,
        engine: std::sync::Arc<Engine>,
        config: &EffectiveConfig,
    ) -> Result<Self, EngineError> {
        validate_settings(&config.settings).map_err(io_error)?;
        verify_directory(
            socket
                .parent()
                .ok_or_else(|| EngineError("socket parent missing".into()))?,
        )?;
        match std::fs::symlink_metadata(socket) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            _ => return Err(EngineError("socket_path_already_exists".into())),
        }
        let directory = std::fs::symlink_metadata(socket.parent().unwrap()).map_err(io_error)?;
        let listener = crate::spawn_gate::bind_unix(socket).map_err(io_error)?;
        let metadata = std::fs::symlink_metadata(socket).map_err(io_error)?;
        let checked = (|| {
            if !metadata.file_type().is_socket()
                || metadata.uid() != rustix::process::getuid().as_raw()
                || metadata.nlink() != 1
            {
                return Err(EngineError("socket_replaced_during_startup".into()));
            }
            let parent = std::fs::symlink_metadata(socket.parent().unwrap()).map_err(io_error)?;
            if parent.ino() != directory.ino() || parent.dev() != directory.dev() {
                return Err(EngineError("socket_directory_replaced".into()));
            }
            std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o600))
                .map_err(io_error)?;
            verify_endpoint(socket)?;
            let current = std::fs::symlink_metadata(socket).map_err(io_error)?;
            if current.ino() != metadata.ino() || current.dev() != metadata.dev() {
                return Err(EngineError("socket_replaced_during_startup".into()));
            }
            Ok(())
        })();
        if let Err(error) = checked {
            drop(listener);
            let cleanup = remove_server_socket(
                socket,
                metadata.dev(),
                metadata.ino(),
                directory.dev(),
                directory.ino(),
            );
            return match cleanup {
                Ok(()) => Err(error),
                Err(cleanup) => Err(EngineError(format!("{}; cleanup: {}", error.0, cleanup.0))),
            };
        }
        assert!(metadata.file_type().is_socket());
        Ok(Self {
            listener,
            engine,
            timeout: Duration::from_millis(config.settings.timeout_ms),
            clients_max: config.settings.clients_max,
            socket: socket.to_path_buf(),
            socket_inode: metadata.ino(),
            socket_device: metadata.dev(),
            directory_inode: directory.ino(),
            directory_device: directory.dev(),
        })
    }
    pub async fn run(self, mut stop: oneshot::Receiver<()>) -> Result<(), EngineError> {
        let mut clients = tokio::task::JoinSet::new();
        let mut result = loop {
            tokio::select! {
                _ = &mut stop => break Ok(()),
                result = clients.join_next(), if !clients.is_empty() => {
                    if let Some(Err(error)) = result && error.is_panic() { eprintln!("engine_client_panicked"); }
                }
                accepted = crate::spawn_gate::accept_unix(&self.listener) => {
                    let (stream, _) = match accepted { Ok(value) => value, Err(error) => break Err(io_error(error)) };
                    if clients.len() >= self.clients_max { drop(stream); continue; }
                    let engine = self.engine.clone();
                    let timeout = self.timeout;
                    clients.spawn(async move { serve_client(stream, engine, timeout).await; });
                }
            }
            assert!(clients.len() <= self.clients_max);
        };
        clients.abort_all();
        while let Some(joined) = clients.join_next().await {
            if let Err(error) = joined
                && !error.is_cancelled()
            {
                result = Err(io_error(error));
            }
        }
        assert!(clients.is_empty());
        drop(self.listener);
        let cleanup = remove_server_socket(
            &self.socket,
            self.socket_device,
            self.socket_inode,
            self.directory_device,
            self.directory_inode,
        );
        match (result, cleanup) {
            (Ok(()), result) | (result, Ok(())) => result,
            (Err(error), Err(cleanup)) => {
                Err(EngineError(format!("{}; cleanup: {}", error.0, cleanup.0)))
            }
        }
    }
}

#[cfg(unix)]
fn remove_server_socket(
    socket: &Path,
    device: u64,
    inode: u64,
    directory_device: u64,
    directory_inode: u64,
) -> Result<(), EngineError> {
    let parent = socket
        .parent()
        .ok_or_else(|| EngineError("socket parent missing".into()))?;
    verify_directory(parent)?;
    let directory = std::fs::symlink_metadata(parent).map_err(io_error)?;
    let metadata = std::fs::symlink_metadata(socket).map_err(io_error)?;
    if directory.dev() != directory_device
        || directory.ino() != directory_inode
        || metadata.dev() != device
        || metadata.ino() != inode
        || !metadata.file_type().is_socket()
        || metadata.uid() != rustix::process::getuid().as_raw()
        || metadata.mode() & 0o7777 != 0o600
        || metadata.nlink() != 1
    {
        return Err(EngineError("socket_replaced_during_shutdown".into()));
    }
    std::fs::remove_file(socket).map_err(io_error)
}

fn io_error(error: impl std::fmt::Display) -> EngineError {
    EngineError(error.to_string())
}

/// Runs blocking work, such as an account renewal of up to two minutes or a harness hash of
/// hundreds of MB, so it never stalls the async runtime: on a multi-thread tokio runtime the
/// worker first hands its other tasks to another thread; anywhere else the work runs in place.
/// Input: the work. Output: its result.
#[cfg(unix)]
fn off_worker<R>(work: impl FnOnce() -> R) -> R {
    match tokio::runtime::Handle::try_current() {
        Ok(runtime) if runtime.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread => {
            tokio::task::block_in_place(work)
        }
        _ => work(),
    }
}

#[cfg(unix)]
fn verify_directory(path: &Path) -> Result<(), EngineError> {
    let metadata = std::fs::symlink_metadata(path).map_err(io_error)?;
    if !metadata.is_dir()
        || metadata.uid() != rustix::process::getuid().as_raw()
        || metadata.mode() & 0o077 != 0
    {
        return Err(EngineError(
            "runtime_directory_requires_owner_mode_0700".into(),
        ));
    }
    assert!(metadata.is_dir());
    assert_eq!(metadata.mode() & 0o077, 0);
    Ok(())
}

#[cfg(unix)]
fn verify_endpoint(path: &Path) -> Result<(), EngineError> {
    verify_directory(
        path.parent()
            .ok_or_else(|| EngineError("socket parent missing".into()))?,
    )?;
    let metadata = std::fs::symlink_metadata(path).map_err(io_error)?;
    if !metadata.file_type().is_socket()
        || metadata.uid() != rustix::process::getuid().as_raw()
        || metadata.mode() & 0o077 != 0
    {
        return Err(EngineError("socket_requires_owner_mode_0600".into()));
    }
    assert!(metadata.file_type().is_socket());
    assert_eq!(metadata.mode() & 0o077, 0);
    Ok(())
}

/// Checks the peer of a stream where Unix domain sockets do not exist.
/// Takes the stream, which cannot exist here. Unreachable by construction.
#[cfg(not(unix))]
fn verify_peer(stream: &UnixStream) -> Result<(), EngineError> {
    match *stream {}
}

#[cfg(unix)]
fn verify_peer(stream: &UnixStream) -> Result<(), EngineError> {
    #[cfg(target_os = "macos")]
    let uid = nix::unistd::getpeereid(stream)
        .map_err(io_error)?
        .0
        .as_raw();
    #[cfg(not(target_os = "macos"))]
    let uid = stream.peer_cred().map_err(io_error)?.uid();
    if uid != rustix::process::getuid().as_raw() {
        return Err(EngineError("peer_owner_mismatch".into()));
    }
    Ok(())
}

pub(crate) async fn read_frame<S: tokio::io::AsyncRead + Unpin>(
    stream: &mut S,
) -> Result<Vec<u8>, EngineError> {
    let length = stream.read_u32().await.map_err(io_error)? as usize;
    if length == 0 || length > FRAME_BYTES_MAX {
        return Err(EngineError("invalid_frame_length".into()));
    }
    let mut frame = vec![0; length + 4];
    frame[..4].copy_from_slice(&(length as u32).to_be_bytes());
    stream.read_exact(&mut frame[4..]).await.map_err(io_error)?;
    assert_eq!(frame.len(), length + 4);
    assert!(frame.len() <= FRAME_BYTES_MAX + 4);
    Ok(frame)
}

#[cfg(unix)]
async fn serve_client(mut stream: UnixStream, engine: std::sync::Arc<Engine>, timeout: Duration) {
    let exchange = async {
        verify_peer(&stream)?;
        let frame = read_frame(&mut stream).await?;
        #[cfg(test)]
        if frame[4..] == *b"test_panic" {
            panic!("engine_test_panic");
        }
        let request = decode_request(&frame).map_err(io_error)?;
        let request_id = request.request_id.clone();
        let reply = match engine.apply(request).await {
            Ok(reply) => reply,
            Err(error) => WireReply {
                request_id,
                revision: Cursor::new(0),
                outcome: Outcome::Rejected {
                    code: error.to_string(),
                },
            },
        };
        stream
            .write_all(&encode_frame(&reply).map_err(io_error)?)
            .await
            .map_err(io_error)?;
        Ok::<(), EngineError>(())
    };
    match tokio::time::timeout(timeout, exchange).await {
        Ok(Ok(())) => {
            let _ = stream.shutdown().await;
        }
        Ok(Err(_)) | Err(_) => {
            drop(stream);
        }
    }
}

/// The terminal host the engine serves agent terminals through, once one is made: the tmux
/// program it was made for, none when tmux was not found, the host, and the newest look
/// that found that program, so an older look still running never replaces it.
#[cfg(unix)]
type TerminalHost = (
    Option<std::path::PathBuf>,
    std::sync::Arc<dyn crate::terminal::Host>,
    u64,
);

/// The refusal for an apply or settings change sent while the storage queue is full.
const QUEUE_FULL: &str = "queue unavailable: no available capacity";

pub struct Engine {
    storage: Storage,
    input: Mutex<InputState>,
    timeout: Duration,
    owner_egress: std::sync::Arc<tokio::sync::RwLock<()>>,
    dispatch_drive: tokio::sync::Mutex<()>,
    agents: Mutex<Option<std::sync::Arc<crate::agents::Agents>>>,
    #[cfg(unix)]
    terminal: Mutex<Option<TerminalHost>>,
    #[cfg(unix)]
    terminal_looks: std::sync::atomic::AtomicU64,
    harness_probe: std::sync::Arc<crate::harnesses::ProbeCache>,
    harness_updates: std::sync::Arc<crate::harnesses::update::Updates>,
    settings_files: Option<(std::path::PathBuf, std::path::PathBuf)>,
    master_activity: owner::transport::master_activity::Board,
}

const HARNESS_PROBE_FRESH: Duration = Duration::from_secs(10);

impl Engine {
    async fn native_auth_job<T>(
        &self,
        build: impl FnOnce(oneshot::Sender<Result<T, EngineError>>) -> Job,
    ) -> Result<T, EngineError> {
        self.storage_job_within(self.timeout, build).await
    }

    /// Runs one storage job and waits at most `wait` for its answer, for owner work whose
    /// client waits longer than the owner deadline. Inputs: the wait and the job.
    /// Output: the job's result, or the outcome-unknown error while storage has not
    /// answered in time or went away without answering.
    async fn storage_job_within<T>(
        &self,
        wait: Duration,
        build: impl FnOnce(oneshot::Sender<Result<T, EngineError>>) -> Job,
    ) -> Result<T, EngineError> {
        let (send, receive) = oneshot::channel();
        let submit_by = tokio::time::Instant::now() + self.timeout;
        let taken = self
            .storage
            .submit(build(send), submit_by, "owner_queue_not_submitted")
            .await?;
        tokio::time::timeout(wait, receive)
            .await
            .map_err(|_| taken.lost(owner::native_auth::outcome_unknown()))?
            .map_err(|_| owner::native_auth::outcome_unknown())?
    }

    /// Runs one storage job for a record that refers to files written before it, and hands
    /// storage's real answer to `settle` even when the owner stops waiting first, so a late
    /// commit never loses a file its record refers to. Inputs: the job and the settle step.
    /// Output: the settle step's error when it fails, else the job's result, or the
    /// outcome-unknown error while storage has not answered in time or went away without
    /// answering.
    async fn settled_job<T: Send + 'static>(
        &self,
        build: impl FnOnce(oneshot::Sender<Result<T, EngineError>>) -> Job,
        settle: impl FnOnce(&Result<T, EngineError>) -> Result<(), EngineError> + Send + 'static,
    ) -> Result<T, EngineError> {
        let (send, receive) = oneshot::channel();
        let submit_by = tokio::time::Instant::now() + self.timeout;
        let submitted = self
            .storage
            .submit(build(send), submit_by, "owner_queue_not_submitted")
            .await;
        let taken = match submitted {
            Ok(taken) => taken,
            Err(pending) if pending.0 == crate::storage::ACCESS_CHANGE_PENDING => {
                tokio::spawn(async move {
                    if let Ok(result) = receive.await {
                        let _ = settle(&result);
                    }
                });
                return Err(pending);
            }
            Err(refused) => {
                let refused = Err(refused);
                settle(&refused)?;
                return refused;
            }
        };
        let answered = tokio::spawn(async move {
            let result = receive
                .await
                .map_err(|_| owner::native_auth::outcome_unknown())?;
            settle(&result).and(result)
        });
        tokio::time::timeout(self.timeout, answered)
            .await
            .map_err(|_| taken.lost(owner::native_auth::outcome_unknown()))?
            .map_err(|_| owner::native_auth::outcome_unknown())?
    }

    /// The engine's two settings files, global then project, for what reads them itself at
    /// each launch. Output: the paths, or none for an engine opened without them.
    pub(crate) fn settings_files(&self) -> Option<(std::path::PathBuf, std::path::PathBuf)> {
        self.settings_files.clone()
    }

    /// The loopback ports of the owner's configured local model endpoints, read from the
    /// engine's settings files at each call, for a sandbox the engine starts to keep free
    /// of the listeners of what it runs. Output: the ports, none when the engine has no
    /// settings files, or [`crate::sandbox::OWNER_PORTS_UNREADABLE`] when they do not read,
    /// so that sandbox does not start (OWNER-PORTS-FAIL-OPEN).
    pub(crate) fn owner_ports(&self) -> Result<Vec<u16>, &'static str> {
        match &self.settings_files {
            Some((global, project)) => crate::sandbox::settings_owner_ports(global, project),
            None => Ok(Vec::new()),
        }
    }

    pub async fn open_with_settings_sources(
        path: &Path,
        global: &Path,
        project: &Path,
    ) -> Result<Self, EngineError> {
        Ok(Self::open_settings(path, global, project).await?.0)
    }

    async fn open_settings(
        path: &Path,
        global: &Path,
        project: &Path,
    ) -> Result<(Self, EffectiveConfig), EngineError> {
        let (storage, config) = Storage::open_with_settings_sources(path, global, project).await?;
        let timeout = Duration::from_millis(config.settings.timeout_ms);
        Ok((
            Self {
                storage,
                input: Mutex::default(),
                timeout,
                owner_egress: std::sync::Arc::new(tokio::sync::RwLock::new(())),
                dispatch_drive: tokio::sync::Mutex::new(()),
                agents: Mutex::default(),
                #[cfg(unix)]
                terminal: Mutex::default(),
                #[cfg(unix)]
                terminal_looks: Default::default(),
                harness_probe: crate::harnesses::ProbeCache::new(HARNESS_PROBE_FRESH),
                harness_updates: crate::harnesses::update::Updates::new(),
                settings_files: Some((global.to_path_buf(), project.to_path_buf())),
                master_activity: Default::default(),
            },
            config,
        ))
    }

    pub async fn open(path: &Path, config: EffectiveConfig) -> Result<Self, EngineError> {
        config.validate().map_err(|e| EngineError(e.to_string()))?;
        let timeout = Duration::from_millis(config.settings.timeout_ms);
        Ok(Self {
            storage: Storage::open(path, config, false).await?,
            input: Mutex::default(),
            timeout,
            owner_egress: std::sync::Arc::new(tokio::sync::RwLock::new(())),
            dispatch_drive: tokio::sync::Mutex::new(()),
            agents: Mutex::default(),
            #[cfg(unix)]
            terminal: Mutex::default(),
            #[cfg(unix)]
            terminal_looks: Default::default(),
            harness_probe: crate::harnesses::ProbeCache::new(HARNESS_PROBE_FRESH),
            harness_updates: crate::harnesses::update::Updates::new(),
            settings_files: None,
            master_activity: Default::default(),
        })
    }

    pub async fn open_readonly(path: &Path, config: EffectiveConfig) -> Result<Self, EngineError> {
        config.validate().map_err(|e| EngineError(e.to_string()))?;
        let timeout = Duration::from_millis(config.settings.timeout_ms);
        Ok(Self {
            storage: Storage::open(path, config, true).await?,
            input: Mutex::default(),
            timeout,
            owner_egress: std::sync::Arc::new(tokio::sync::RwLock::new(())),
            dispatch_drive: tokio::sync::Mutex::new(()),
            agents: Mutex::default(),
            #[cfg(unix)]
            terminal: Mutex::default(),
            #[cfg(unix)]
            terminal_looks: Default::default(),
            harness_probe: crate::harnesses::ProbeCache::new(HARNESS_PROBE_FRESH),
            harness_updates: crate::harnesses::update::Updates::new(),
            settings_files: None,
            master_activity: Default::default(),
        })
    }

    pub async fn apply(&self, request: WireRequest) -> Result<WireReply, EngineError> {
        let submit_by = tokio::time::Instant::now() + self.timeout;
        let _delivery = tokio::time::timeout_at(submit_by, self.owner_egress.read())
            .await
            .map_err(|_| EngineError("delivery_barrier_timeout_not_submitted".into()))?;
        request.validate().map_err(|e| EngineError(e.to_string()))?;
        let (send, receive) = oneshot::channel();
        let taken = self
            .storage
            .submit(Job::Apply(request, send), submit_by, QUEUE_FULL)
            .await?;
        tokio::time::timeout(self.timeout, receive)
            .await
            .map_err(|_| taken.lost(EngineError("outcome_unknown_retry_same_identity".into())))?
            .map_err(|_| EngineError("database worker stopped".into()))?
    }

    pub async fn update_config(&self, config: EffectiveConfig) -> Result<(), EngineError> {
        let submit_by = tokio::time::Instant::now() + self.timeout;
        let _delivery = tokio::time::timeout_at(submit_by, self.owner_egress.write())
            .await
            .map_err(|_| EngineError("delivery_barrier_timeout_not_submitted".into()))?;
        config.validate().map_err(|e| EngineError(e.to_string()))?;
        let (send, receive) = oneshot::channel();
        let taken = self
            .storage
            .submit(Job::Config(config, send), submit_by, QUEUE_FULL)
            .await?;
        tokio::time::timeout(self.timeout, receive)
            .await
            .map_err(|_| taken.lost(EngineError("configuration_update_outcome_unknown".into())))?
            .map_err(|_| EngineError("database worker stopped".into()))?
    }

    /// Closes the database worker after in-flight deliveries finish. Waits for
    /// the worker's own reply, sent once the database lock is released, since
    /// the final checkpoint and queued jobs are not bounded by the request
    /// timeout. Returns that result, or an error if the worker already stopped.
    pub async fn close(&self) -> Result<(), EngineError> {
        let _delivery = tokio::time::timeout(self.timeout, self.owner_egress.write())
            .await
            .map_err(|_| EngineError("delivery_barrier_timeout_not_submitted".into()))?;
        let (send, receive) = oneshot::channel();
        self.storage
            .sender
            .try_send(Job::Close(send))
            .map_err(|e| EngineError(format!("queue unavailable: {e}")))?;
        receive
            .await
            .map_err(|_| EngineError("database worker stopped".into()))?
    }

    pub fn set_controller_epoch(
        &self,
        session: SessionId,
        epoch: ControllerEpoch,
    ) -> Result<(), EngineError> {
        let mut input = self
            .input
            .lock()
            .map_err(|_| EngineError("input state poisoned".into()))?;
        input.assert_consistent();
        if input
            .epochs_seen
            .contains(&(session.clone(), epoch.clone()))
        {
            return Err(EngineError("controller_epoch_must_change".into()));
        }
        if input.epochs_seen.len() >= 4096 {
            return Err(EngineError("epoch_capacity".into()));
        }
        if input.epochs.len() >= 128 && !input.epochs.contains_key(&session) {
            return Err(EngineError("session_capacity".into()));
        }
        input.leases.remove(&session);
        input.retired.remove(&session);
        input.epochs_seen.insert((session.clone(), epoch.clone()));
        input.epochs.insert(session, epoch);
        assert!(input.epochs.len() <= 128);
        assert!(input.leases.len() <= input.epochs.len());
        input.assert_consistent();
        Ok(())
    }

    pub fn acquire_input(
        &self,
        session_id: &SessionId,
        client_id: &str,
        controller_epoch: &str,
    ) -> Result<InputLease, EngineError> {
        let client_id = ClientId::new(client_id).map_err(|e| EngineError(e.to_string()))?;
        let controller_epoch =
            ControllerEpoch::new(controller_epoch).map_err(|e| EngineError(e.to_string()))?;
        let mut input = self
            .input
            .lock()
            .map_err(|_| EngineError("input state poisoned".into()))?;
        if input.epochs.get(session_id) != Some(&controller_epoch)
            || input.retired.contains(session_id)
        {
            return Err(EngineError("stale_controller_epoch".into()));
        }
        if input
            .leases
            .get(session_id)
            .is_some_and(|lease| lease.client_id != client_id)
        {
            return Err(EngineError("input_owned_by_another_client".into()));
        }
        let lease = InputLease {
            session_id: session_id.clone(),
            client_id,
            controller_epoch,
        };
        input.leases.insert(session_id.clone(), lease.clone());
        assert_eq!(input.leases.get(session_id), Some(&lease));
        assert!(!input.retired.contains(session_id));
        input.assert_consistent();
        Ok(lease)
    }

    pub fn authorize_input(&self, lease: &InputLease) -> Result<(), EngineError> {
        let input = self
            .input
            .lock()
            .map_err(|_| EngineError("input state poisoned".into()))?;
        input.assert_consistent();
        if input.leases.get(&lease.session_id) != Some(lease)
            || input.epochs.get(&lease.session_id) != Some(&lease.controller_epoch)
        {
            return Err(EngineError("stale_input_lease".into()));
        }
        assert!(input.epochs.contains_key(&lease.session_id));
        assert!(!input.retired.contains(&lease.session_id));
        Ok(())
    }

    pub fn authorize_resize(
        &self,
        lease: &InputLease,
        rows: u16,
        columns: u16,
    ) -> Result<(), EngineError> {
        if rows == 0 || columns == 0 || rows > 1000 || columns > 1000 {
            return Err(EngineError("invalid_terminal_dimensions".into()));
        }
        self.authorize_input(lease)
    }

    pub fn release_input(&self, lease: &InputLease) -> Result<(), EngineError> {
        let mut input = self
            .input
            .lock()
            .map_err(|_| EngineError("input state poisoned".into()))?;
        if input.leases.get(&lease.session_id) != Some(lease) {
            return Err(EngineError("stale_input_lease".into()));
        }
        input.leases.remove(&lease.session_id);
        input.retired.insert(lease.session_id.clone());
        assert!(!input.leases.contains_key(&lease.session_id));
        assert!(input.epochs.contains_key(&lease.session_id));
        input.assert_consistent();
        Ok(())
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// One project create as a desktop client sends it. Input: the request and project id.
    /// Output: the request.
    fn create_request(id: &str) -> WireRequest {
        WireRequest {
            protocol_version: crate::protocol::PROTOCOL_VERSION,
            client_id: ClientId::new("desktop").unwrap(),
            request_id: crate::protocol::RequestId::new(id).unwrap(),
            expected_revision: crate::protocol::Cursor::new(0),
            command: crate::protocol::Command::CreateProject {
                project_id: crate::protocol::ProjectId::new(id).unwrap(),
                spec: crate::protocol::ProjectSpec {
                    name: "Inventory".into(),
                    repositories: vec!["/tmp/inventory".into()],
                },
            },
        }
    }

    /// Opens an engine whose storage queue holds one job, in a private folder.
    /// Inputs: the folder and any further settings lines. Output: the engine and its
    /// database path.
    async fn one_slot_engine(
        root: &std::path::Path,
        settings: &str,
    ) -> (Engine, std::path::PathBuf) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o700)).unwrap();
        let global = root.join("g");
        std::fs::write(&global, format!("queue_capacity = 1\n{settings}")).unwrap();
        let cfg = crate::config::resolve_config(&global, &root.join("p")).unwrap();
        let path = root.join("db");
        (Engine::open(&path, cfg).await.unwrap(), path)
    }

    /// Reads how many changes the closed engine's database holds.
    /// Input: the database path. Output: the stored revision.
    fn stored_revision(path: &std::path::Path) -> i64 {
        rusqlite::Connection::open(path)
            .unwrap()
            .query_row("SELECT revision FROM state WHERE id=1", [], |row| {
                row.get(0)
            })
            .unwrap()
    }

    /// Sends one create while the worker is held at its claim, lifts the hold `release`
    /// after the send, and waits for the engine to close. Inputs: the folder, the settings
    /// lines and the release delay. Output: the apply's revision or error code, and the
    /// stored revision after close.
    async fn apply_across_a_held_claim(
        root: &std::path::Path,
        settings: &str,
        release: Duration,
    ) -> (Result<crate::protocol::Cursor, String>, i64) {
        let path = root.join("db");
        let hold = crate::storage::hold_claims(&path);
        let (engine, opened) = one_slot_engine(root, settings).await;
        assert_eq!(opened, path);
        let engine = std::sync::Arc::new(engine);
        let raced = tokio::spawn({
            let engine = engine.clone();
            async move { engine.apply(create_request("raced")).await }
        });
        tokio::time::sleep(release).await;
        drop(hold);
        let outcome = raced
            .await
            .unwrap()
            .map(|reply| reply.revision)
            .map_err(|error| error.0);
        engine.close().await.unwrap();
        (outcome, stored_revision(&path))
    }

    #[tokio::test]
    async fn a_job_whose_bound_runs_out_at_the_claim_is_withdrawn_and_the_worker_skips_it() {
        let root = tempfile::tempdir().unwrap();
        let (outcome, stored) =
            apply_across_a_held_claim(root.path(), "", Duration::from_millis(1500)).await;
        assert_eq!(outcome, Err("storage_busy_not_submitted".to_owned()));
        assert_eq!(
            stored, 0,
            "the worker lost the claim and never ran the withdrawn job"
        );
    }

    #[tokio::test]
    async fn the_claim_race_at_the_bound_has_one_outcome_and_applies_at_most_once() {
        for round in 0..3 {
            for offset in [-20i64, -10, -5, -2, 0, 2, 5, 10, 20] {
                let root = tempfile::tempdir().unwrap();
                let release = Duration::from_millis(u64::try_from(100 + offset).unwrap());
                let (outcome, stored) =
                    apply_across_a_held_claim(root.path(), "timeout_ms = 100", release).await;
                let seen = format!("round {round} offset {offset}: {outcome:?}, stored {stored}");
                match outcome.as_ref() {
                    Err(code) if code == "storage_busy_not_submitted" => {
                        assert_eq!(stored, 0, "{seen}")
                    }
                    Ok(revision) => {
                        assert_eq!(*revision, crate::protocol::Cursor::new(1), "{seen}");
                        assert_eq!(stored, 1, "{seen}");
                    }
                    Err(code) if code == "outcome_unknown_retry_same_identity" => {
                        assert_eq!(stored, 1, "{seen}")
                    }
                    Err(_) => panic!("an outcome outside the claim's three: {seen}"),
                }
            }
        }
    }

    #[tokio::test]
    async fn an_apply_the_busy_worker_does_not_take_in_time_is_withdrawn_and_never_lands() {
        let root = tempfile::tempdir().unwrap();
        let (engine, _) = one_slot_engine(root.path(), "").await;
        let engine = std::sync::Arc::new(engine);
        let (release, held) = oneshot::channel();
        engine.storage.sender.try_send(Job::Hold(held)).unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        let queued = engine.apply(create_request("queued")).await;
        let next = tokio::spawn({
            let engine = engine.clone();
            async move { engine.apply(create_request("next")).await }
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        drop(release);
        let next = next.await.unwrap();
        assert_eq!(
            queued.map(|reply| reply.revision).map_err(|error| error.0),
            Err("storage_busy_not_submitted".to_owned()),
            "an apply still queued when its bound ran out is withdrawn and answered unsent"
        );
        assert_eq!(
            next.map(|reply| reply.revision).map_err(|error| error.0),
            Ok(crate::protocol::Cursor::new(1)),
            "the withdrawn create never landed, and its queue slot was free for the next apply"
        );
    }

    #[tokio::test]
    async fn an_apply_waits_for_a_storage_worker_that_starts_receiving_within_its_bound() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("db");
        let hold = crate::storage::hold_start(&path);
        let (engine, opened) = one_slot_engine(root.path(), "").await;
        assert_eq!(opened, path);
        let engine = std::sync::Arc::new(engine);
        let first = tokio::spawn({
            let engine = engine.clone();
            async move { engine.apply(create_request("first")).await }
        });
        tokio::time::sleep(engine.timeout / 2).await;
        drop(hold);
        assert_eq!(
            first
                .await
                .unwrap()
                .map(|reply| reply.revision)
                .map_err(|error| error.0),
            Ok(crate::protocol::Cursor::new(1)),
            "an apply sent while the worker starts is answered once the worker receives"
        );
    }

    #[tokio::test]
    async fn an_apply_before_the_storage_worker_receives_is_refused_unsent() {
        let root = tempfile::tempdir().unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let global = root.path().join("g");
        std::fs::write(&global, "queue_capacity = 1").unwrap();
        let cfg = crate::config::resolve_config(&global, &root.path().join("p")).unwrap();
        let path = root.path().join("db");
        let hold = crate::storage::hold_start(&path);
        let engine = Engine::open(&path, cfg).await.unwrap();
        let started = std::time::Instant::now();
        let first = engine.apply(create_request("first")).await;
        let waited = started.elapsed();
        drop(hold);
        let after = engine.apply(create_request("after")).await;
        assert_eq!(
            first.map(|reply| reply.revision).map_err(|error| error.0),
            Err("storage_starting_not_submitted".to_owned()),
            "an apply the worker cannot yet receive is refused, nothing queued, after {waited:?}"
        );
        assert!(
            waited >= engine.timeout,
            "it waited its own bound: {waited:?}"
        );
        assert_eq!(
            after.map(|reply| reply.revision).map_err(|error| error.0),
            Ok(crate::protocol::Cursor::new(1)),
            "the refused create never committed, so the next apply is the first revision"
        );
        engine.close().await.unwrap();
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn peer_without_kernel_credentials_is_rejected() {
        let stream = crate::spawn_gate::unconnected_unix_stream().unwrap();
        stream.set_nonblocking(true).unwrap();
        let stream = UnixStream::from_std(stream).unwrap();
        assert!(verify_peer(&stream).is_err());
    }

    #[test]
    #[should_panic]
    fn internal_input_invariant_detects_orphaned_lease() {
        let mut state = InputState::default();
        let session_id = SessionId::new("orphan").unwrap();
        state.leases.insert(
            session_id.clone(),
            InputLease {
                session_id,
                client_id: ClientId::new("client").unwrap(),
                controller_epoch: ControllerEpoch::new("epoch").unwrap(),
            },
        );
        state.assert_consistent();
    }

    #[tokio::test]
    async fn one_input_owner_across_clients() {
        let root = tempfile::tempdir().unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let cfg =
            crate::config::resolve_config(&root.path().join("g"), &root.path().join("p")).unwrap();
        let engine = Engine::open(&root.path().join("db"), cfg).await.unwrap();
        let session = SessionId::new("session").unwrap();
        engine
            .set_controller_epoch(session.clone(), ControllerEpoch::new("epoch1").unwrap())
            .unwrap();
        let first = engine.acquire_input(&session, "tui", "epoch1").unwrap();
        assert!(engine.acquire_input(&session, "desktop", "epoch1").is_err());
        assert_eq!(
            first,
            engine.acquire_input(&session, "tui", "epoch1").unwrap()
        );
        engine.authorize_resize(&first, 24, 80).unwrap();
        engine
            .set_controller_epoch(session.clone(), ControllerEpoch::new("epoch2").unwrap())
            .unwrap();
        assert!(engine.authorize_input(&first).is_err());
        assert!(engine.authorize_resize(&first, 24, 80).is_err());
        assert!(engine.acquire_input(&session, "tui", "epoch1").is_err());
        let second = engine.acquire_input(&session, "desktop", "epoch2").unwrap();
        assert!(engine.authorize_resize(&second, 0, 80).is_err());
        engine.release_input(&second).unwrap();
        assert!(engine.authorize_input(&second).is_err());
        assert!(engine.acquire_input(&session, "desktop", "epoch2").is_err());
        assert!(
            engine
                .set_controller_epoch(session, ControllerEpoch::new("epoch2").unwrap())
                .is_err()
        );
        engine.close().await.unwrap();
    }

    #[tokio::test]
    async fn a_panicking_client_leaves_the_engine_server_serving_the_next() {
        let root = tempfile::Builder::new()
            .prefix("ca-eng-panic-")
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let config =
            crate::config::resolve_config(&root.path().join("g"), &root.path().join("p")).unwrap();
        let engine = std::sync::Arc::new(
            Engine::open(&root.path().join("engine.db"), config.clone())
                .await
                .unwrap(),
        );
        let socket = root.path().join("e.sock");
        let server = Server::bind(&socket, engine.clone(), &config).unwrap();
        let (stop, stopped) = oneshot::channel();
        let service = tokio::spawn(server.run(stopped));

        let mut panicking = crate::spawn_gate::connect_unix(&socket).await.unwrap();
        let mut frame = 10u32.to_be_bytes().to_vec();
        frame.extend_from_slice(b"test_panic");
        panicking.write_all(&frame).await.unwrap();
        let mut answered = Vec::new();
        tokio::time::timeout(
            Duration::from_secs(10),
            panicking.read_to_end(&mut answered),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(answered.is_empty(), "the panicking request gets no reply");

        let reply = Client::new(socket.clone(), Duration::from_secs(5))
            .unwrap()
            .request(WireRequest {
                protocol_version: PROTOCOL_VERSION,
                client_id: ClientId::new("after-panic").unwrap(),
                request_id: RequestId::new("after-panic").unwrap(),
                expected_revision: Cursor::new(0),
                command: Command::Snapshot,
            })
            .await
            .unwrap_or_else(|error| panic!("after a client panic the engine answered {error}"));
        assert!(
            matches!(reply.outcome, Outcome::Snapshot { .. }),
            "{reply:?}"
        );
        assert!(socket.exists(), "the engine socket is still in place");
        stop.send(()).unwrap();
        service.await.unwrap().unwrap();
        engine.close().await.unwrap();
    }

    /// Closes an engine whose database worker is still on a write that waits out
    /// SQLite's busy timeout behind another connection's write lock, with a 10ms
    /// request timeout. Close must wait for the worker's reply, which comes once
    /// the database is released, so it succeeds and the engine opens again.
    #[tokio::test]
    async fn close_waits_for_the_database_worker_past_the_request_timeout() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let global = root.path().join("g");
        std::fs::write(&global, "version = 1\ntimeout_ms = 10\n").unwrap();
        std::fs::set_permissions(&global, std::fs::Permissions::from_mode(0o600)).unwrap();
        let cfg = crate::config::resolve_config(&global, &root.path().join("p")).unwrap();
        assert_eq!(cfg.settings.timeout_ms, 10);
        let path = root.path().join("db");
        let engine = Engine::open(&path, cfg.clone()).await.unwrap();
        let holder = rusqlite::Connection::open(&path).unwrap();
        holder.execute_batch("BEGIN IMMEDIATE").unwrap();
        let (send, started) = oneshot::channel();
        engine
            .storage
            .sender
            .try_send(Job::OwnerStart(send))
            .unwrap_or_else(|_| panic!("queue unavailable"));
        let began = std::time::Instant::now();
        assert_eq!(engine.close().await, Ok(()));
        assert!(began.elapsed() > engine.timeout);
        assert!(started.await.unwrap().is_err());
        holder.execute_batch("ROLLBACK").unwrap();
        drop(holder);
        Engine::open(&path, cfg)
            .await
            .unwrap()
            .close()
            .await
            .unwrap();
    }
}
