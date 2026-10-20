use crate::config::{EffectiveConfig, validate_settings};
use crate::protocol::*;
use crate::storage::{Job, Storage};
use std::collections::{BTreeMap, BTreeSet};
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::oneshot;

pub mod bootstrap;
pub mod owner;

pub(crate) use bootstrap::NativeProfileCustody;

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

    pub async fn request(&self, request: WireRequest) -> Result<WireReply, ClientError> {
        request.validate().map_err(|e| EngineError(e.to_string()))?;
        if let Some(binding) = &self.binding {
            binding.verify()?;
        }
        verify_endpoint(&self.socket)?;
        tokio::time::timeout(self.timeout, async {
            let mut stream = UnixStream::connect(&self.socket).await.map_err(io_error)?;
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
        let listener = UnixListener::bind(socket).map_err(io_error)?;
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
        assert_eq!(
            std::fs::symlink_metadata(socket).map_err(io_error)?.mode() & 0o077,
            0
        );
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
                    if let Some(Err(error)) = result { break Err(io_error(error)); }
                }
                accepted = self.listener.accept() => {
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

async fn read_frame(stream: &mut UnixStream) -> Result<Vec<u8>, EngineError> {
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

async fn serve_client(mut stream: UnixStream, engine: std::sync::Arc<Engine>, timeout: Duration) {
    let exchange = async {
        verify_peer(&stream)?;
        let frame = read_frame(&mut stream).await?;
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

pub struct Engine {
    storage: Storage,
    input: Mutex<InputState>,
    timeout: Duration,
    owner_egress: tokio::sync::RwLock<()>,
    dispatch_drive: tokio::sync::Mutex<()>,
}

impl Engine {
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
                owner_egress: tokio::sync::RwLock::new(()),
                dispatch_drive: tokio::sync::Mutex::new(()),
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
            owner_egress: tokio::sync::RwLock::new(()),
            dispatch_drive: tokio::sync::Mutex::new(()),
        })
    }

    pub async fn open_readonly(path: &Path, config: EffectiveConfig) -> Result<Self, EngineError> {
        config.validate().map_err(|e| EngineError(e.to_string()))?;
        let timeout = Duration::from_millis(config.settings.timeout_ms);
        Ok(Self {
            storage: Storage::open(path, config, true).await?,
            input: Mutex::default(),
            timeout,
            owner_egress: tokio::sync::RwLock::new(()),
            dispatch_drive: tokio::sync::Mutex::new(()),
        })
    }

    pub async fn apply(&self, request: WireRequest) -> Result<WireReply, EngineError> {
        let _delivery = tokio::time::timeout(self.timeout, self.owner_egress.read())
            .await
            .map_err(|_| EngineError("delivery_barrier_timeout_not_submitted".into()))?;
        request.validate().map_err(|e| EngineError(e.to_string()))?;
        let (send, receive) = oneshot::channel();
        self.storage
            .sender
            .try_send(Job::Apply(request, send))
            .map_err(|e| EngineError(format!("queue unavailable: {e}")))?;
        tokio::time::timeout(self.timeout, receive)
            .await
            .map_err(|_| EngineError("outcome_unknown_retry_same_identity".into()))?
            .map_err(|_| EngineError("database worker stopped".into()))?
    }

    pub async fn update_config(&self, config: EffectiveConfig) -> Result<(), EngineError> {
        let _delivery = tokio::time::timeout(self.timeout, self.owner_egress.write())
            .await
            .map_err(|_| EngineError("delivery_barrier_timeout_not_submitted".into()))?;
        config.validate().map_err(|e| EngineError(e.to_string()))?;
        let (send, receive) = oneshot::channel();
        self.storage
            .sender
            .try_send(Job::Config(config, send))
            .map_err(|e| EngineError(format!("queue unavailable: {e}")))?;
        tokio::time::timeout(self.timeout, receive)
            .await
            .map_err(|_| EngineError("configuration_update_outcome_unknown".into()))?
            .map_err(|_| EngineError("database worker stopped".into()))?
    }

    pub async fn close(&self) -> Result<(), EngineError> {
        let _delivery = tokio::time::timeout(self.timeout, self.owner_egress.write())
            .await
            .map_err(|_| EngineError("delivery_barrier_timeout_not_submitted".into()))?;
        let (send, receive) = oneshot::channel();
        self.storage
            .sender
            .try_send(Job::Close(send))
            .map_err(|e| EngineError(format!("queue unavailable: {e}")))?;
        tokio::time::timeout(self.timeout, receive)
            .await
            .map_err(|_| EngineError("database close timeout".into()))?
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

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn peer_without_kernel_credentials_is_rejected() {
        let socket = nix::sys::socket::socket(
            nix::sys::socket::AddressFamily::Unix,
            nix::sys::socket::SockType::Stream,
            nix::sys::socket::SockFlag::empty(),
            None,
        )
        .unwrap();
        let stream = std::os::unix::net::UnixStream::from(socket);
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
}
