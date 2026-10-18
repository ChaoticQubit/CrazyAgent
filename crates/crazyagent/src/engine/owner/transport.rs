use super::*;
use crate::engine::{Engine, io_error, read_frame, verify_peer};
use crate::storage::Job;
use rustix::fs::{Mode, OFlags};
use std::fs::File;
use std::os::unix::fs::{FileExt, FileTypeExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use subtle::ConstantTimeEq;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::oneshot;

const SOCKET: &str = "owner.sock";
const KEYS: [&str; 2] = ["owner-tui.cap", "owner-desktop.cap"];

#[derive(Clone, Copy, PartialEq, Eq)]
struct Identity {
    device: u64,
    inode: u64,
}

impl Identity {
    fn of(m: &std::fs::Metadata) -> Self {
        Self {
            device: m.dev(),
            inode: m.ino(),
        }
    }
}

struct Root {
    path: PathBuf,
    directory: File,
    identity: Identity,
}

impl Root {
    fn open(path: &Path) -> Result<Self, EngineError> {
        if !path.is_absolute() || std::fs::canonicalize(path).map_err(io_error)? != path {
            return Err(failure("owner_root_requires_canonical_trusted_path"));
        }
        let directory = File::from(
            rustix::fs::open(
                path,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(io_error)?,
        );
        let m = directory.metadata().map_err(io_error)?;
        if !m.is_dir()
            || m.uid() != rustix::process::getuid().as_raw()
            || m.mode() & 0o7777 != 0o700
        {
            return Err(failure("owner_root_requires_private_owner_directory"));
        }
        let root = Self {
            path: path.to_path_buf(),
            directory,
            identity: Identity::of(&m),
        };
        root.verify()?;
        Ok(root)
    }

    fn verify(&self) -> Result<(), EngineError> {
        let m = std::fs::symlink_metadata(&self.path).map_err(io_error)?;
        if !m.is_dir()
            || Identity::of(&m) != self.identity
            || m.uid() != rustix::process::getuid().as_raw()
            || m.mode() & 0o7777 != 0o700
        {
            return Err(failure("owner_root_replaced_or_unsafe"));
        }
        Ok(())
    }

    fn open_key(&self, name: &str, create: bool) -> Result<File, EngineError> {
        self.verify()?;
        let flags = if create {
            OFlags::RDWR | OFlags::CREATE | OFlags::EXCL
        } else {
            OFlags::RDONLY
        };
        let f = File::from(
            rustix::fs::openat(
                &self.directory,
                name,
                flags | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
                Mode::RUSR | Mode::WUSR,
            )
            .map_err(io_error)?,
        );
        Ok(f)
    }

    fn verify_key(&self, name: &str, file: &File, id: Identity) -> Result<(), EngineError> {
        self.verify()?;
        let m = file.metadata().map_err(io_error)?;
        let p = rustix::fs::statat(&self.directory, name, rustix::fs::AtFlags::SYMLINK_NOFOLLOW)
            .map_err(io_error)?;
        if !m.is_file()
            || Identity::of(&m) != id
            || m.uid() != rustix::process::getuid().as_raw()
            || m.mode() & 0o7777 != 0o600
            || m.nlink() != 1
            || m.len() != 32
            || p.st_ino != m.ino()
            || p.st_dev as u64 != m.dev()
        {
            return Err(failure("owner_capability_replaced_or_unsafe"));
        }
        Ok(())
    }

    fn socket_identity(&self) -> Result<Identity, EngineError> {
        self.verify()?;
        let m = std::fs::symlink_metadata(self.path.join(SOCKET)).map_err(io_error)?;
        if !m.file_type().is_socket()
            || m.uid() != rustix::process::getuid().as_raw()
            || m.mode() & 0o7777 != 0o600
            || m.nlink() != 1
        {
            return Err(failure("owner_socket_unsafe"));
        }
        Ok(Identity::of(&m))
    }

    fn unlink_owned(&self, name: &str, id: Identity, socket: bool) -> Result<(), EngineError> {
        self.verify()?;
        let m = std::fs::symlink_metadata(self.path.join(name)).map_err(io_error)?;
        if Identity::of(&m) != id
            || (socket && !m.file_type().is_socket())
            || (!socket && !m.is_file())
        {
            return Err(failure("owner_cleanup_replaced_artifact_preserved"));
        }
        rustix::fs::unlinkat(&self.directory, name, rustix::fs::AtFlags::empty()).map_err(io_error)
    }
}

struct Slot {
    registration: Registration,
    token: [u8; 32],
    file: File,
    identity: Identity,
}

struct Authority {
    root: Root,
    socket: Identity,
    host: HostStamp,
    slots: Mutex<[Slot; 2]>,
    usable: std::sync::atomic::AtomicBool,
}

impl Authority {
    fn authenticate(&self, packet: &[u8; 37]) -> Result<Admission, EngineError> {
        use std::sync::atomic::Ordering;
        if !self.usable.load(Ordering::SeqCst) || &packet[..4] != b"CAO1" {
            return Err(failure("owner_authentication_failed"));
        }
        let i = match packet[4] {
            0 => 0,
            1 => 1,
            _ => return Err(failure("owner_authentication_failed")),
        };
        let slots = self
            .slots
            .lock()
            .map_err(|_| failure("owner_authority_unavailable"))?;
        let slot = &slots[i];
        self.root.verify_key(KEYS[i], &slot.file, slot.identity)?;
        let mut observed = [0; 32];
        slot.file
            .read_exact_at(&mut observed, 0)
            .map_err(io_error)?;
        if !bool::from(slot.token.ct_eq(&observed)) {
            return Err(failure("owner_capability_content_changed"));
        }
        if self.root.socket_identity()? != self.socket {
            return Err(failure("owner_socket_replaced"));
        }
        if !slot.registration.active || !bool::from(slot.token.ct_eq(&packet[5..])) {
            return Err(failure("owner_authentication_failed"));
        }
        Ok(slot.registration.admission.clone())
    }

    fn publish(&self, registration: Registration) -> Result<(), EngineError> {
        let i = match registration.kind {
            OwnerClientKind::Tui => 0,
            OwnerClientKind::Desktop => 1,
        };
        let mut slots = self
            .slots
            .lock()
            .map_err(|_| failure("owner_authority_unavailable"))?;
        let slot = &mut slots[i];
        if slot.registration.admission == registration.admission
            && slot.registration.active == registration.active
        {
            return Ok(());
        }
        slot.registration.active = false;
        if registration.active {
            let mut token = [0; 32];
            getrandom::fill(&mut token).map_err(|_| failure("owner_entropy_unavailable"))?;
            self.root.verify_key(KEYS[i], &slot.file, slot.identity)?;
            slot.file.write_all_at(&token, 0).map_err(io_error)?;
            slot.file.sync_all().map_err(io_error)?;
            self.root.verify_key(KEYS[i], &slot.file, slot.identity)?;
            slot.token = token;
        }
        slot.registration = registration;
        Ok(())
    }

    fn cleanup(&self) -> Result<(), EngineError> {
        self.root.unlink_owned(SOCKET, self.socket, true)?;
        let slots = self
            .slots
            .lock()
            .map_err(|_| failure("owner_authority_unavailable"))?;
        for (i, slot) in slots.iter().enumerate() {
            self.root.unlink_owned(KEYS[i], slot.identity, false)?;
        }
        Ok(())
    }
}

pub struct OwnerRuntimeHandle {
    engine: Arc<Engine>,
    authority: Arc<Authority>,
}

impl OwnerRuntimeHandle {
    pub fn epoch(&self, kind: OwnerClientKind) -> Result<i64, EngineError> {
        let slots = self
            .authority
            .slots
            .lock()
            .map_err(|_| failure("owner_authority_unavailable"))?;
        Ok(slots
            .iter()
            .find(|s| s.registration.kind == kind)
            .ok_or_else(|| failure("owner_registration_missing"))?
            .registration
            .admission
            .owner_epoch)
    }

    /// Orders a trusted host grant change with actor work and response delivery.
    /// Takes an exact control identity, client kind and expected grant epoch.
    /// Returns committed status; prior delivered data cannot be recalled.
    pub async fn control(&self, control: OwnerControl) -> Result<OwnerControlReply, EngineError> {
        let _barrier = tokio::time::timeout(self.engine.timeout, self.engine.owner_egress.write())
            .await
            .map_err(|_| failure("owner_control_not_submitted"))?;
        let (result, registration) = self
            .engine
            .owner_job(|reply| Job::OwnerControl(self.authority.host.clone(), control, reply))
            .await?;
        self.authority
            .publish(registration)
            .map_err(|_| failure("owner_control_committed_publication_unavailable"))?;
        Ok(result)
    }

    /// Looks up an exact host control through the current storage actor.
    /// Takes the original control payload under this runtime incarnation.
    /// Returns historical status or absence at this barrier, not cancellation.
    pub async fn reconcile_control(
        &self,
        control: OwnerControl,
    ) -> Result<Option<OwnerControlReply>, EngineError> {
        let _barrier = tokio::time::timeout(self.engine.timeout, self.engine.owner_egress.write())
            .await
            .map_err(|_| failure("owner_read_not_submitted"))?;
        self.engine
            .owner_job(|reply| {
                Job::OwnerControlReconcile(self.authority.host.clone(), control, reply)
            })
            .await
    }
}

pub struct OwnerServer {
    listener: UnixListener,
    engine: Arc<Engine>,
    authority: Arc<Authority>,
    timeout: Duration,
}

impl OwnerServer {
    /// Publishes authenticated native owner access under a host trusted root.
    /// Requires canonical private root and protected ancestors, processes and DB.
    /// Returns server and host control; it does not qualify worker containment.
    pub async fn bind(
        path: &Path,
        engine: Arc<Engine>,
        config: &crate::config::EffectiveConfig,
    ) -> Result<(Self, OwnerRuntimeHandle), EngineError> {
        crate::config::validate_settings(&config.settings).map_err(io_error)?;
        let root = Root::open(path)?;
        for name in [SOCKET, KEYS[0], KEYS[1]] {
            match std::fs::symlink_metadata(root.path.join(name)) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                _ => return Err(failure("owner_bootstrap_artifact_already_exists")),
            }
        }
        let _barrier = tokio::time::timeout(engine.timeout, engine.owner_egress.write())
            .await
            .map_err(|_| failure("owner_start_not_submitted"))?;
        let started = engine.owner_job(Job::OwnerStart).await?;
        let host = started.host.clone();
        let result = (|| {
            let mut slots = Vec::new();
            for (i, registration) in started.owners.into_iter().enumerate() {
                let file = root.open_key(KEYS[i], true)?;
                let mut token = [0; 32];
                getrandom::fill(&mut token).map_err(|_| failure("owner_entropy_unavailable"))?;
                file.write_all_at(&token, 0).map_err(io_error)?;
                file.sync_all().map_err(io_error)?;
                let identity = Identity::of(&file.metadata().map_err(io_error)?);
                root.verify_key(KEYS[i], &file, identity)?;
                slots.push(Slot {
                    registration,
                    token,
                    file,
                    identity,
                });
            }
            root.verify()?;
            let listener = UnixListener::bind(root.path.join(SOCKET)).map_err(io_error)?;
            let created = std::fs::symlink_metadata(root.path.join(SOCKET)).map_err(io_error)?;
            if !created.file_type().is_socket()
                || created.uid() != rustix::process::getuid().as_raw()
                || created.nlink() != 1
            {
                return Err(failure("owner_socket_replaced"));
            }
            std::fs::set_permissions(
                root.path.join(SOCKET),
                std::os::unix::fs::PermissionsExt::from_mode(0o600),
            )
            .map_err(io_error)?;
            let socket = root.socket_identity()?;
            if socket != Identity::of(&created) {
                return Err(failure("owner_socket_replaced"));
            }
            let slots: [Slot; 2] = slots
                .try_into()
                .map_err(|_| failure("owner_registration_count"))?;
            let authority = Arc::new(Authority {
                root,
                socket,
                host: host.clone(),
                slots: Mutex::new(slots),
                usable: std::sync::atomic::AtomicBool::new(true),
            });
            Ok::<_, EngineError>((listener, authority))
        })();
        let (listener, authority) = match result {
            Ok(v) => v,
            Err(e) => {
                let _ = engine.owner_job(|reply| Job::OwnerStop(host, reply)).await;
                return Err(e);
            }
        };
        let server = Self {
            listener,
            engine: engine.clone(),
            authority: authority.clone(),
            timeout: Duration::from_millis(config.settings.timeout_ms),
        };
        let handle = OwnerRuntimeHandle {
            engine: engine.clone(),
            authority,
        };
        Ok((server, handle))
    }

    pub async fn run(self, mut stop: oneshot::Receiver<()>) -> Result<(), EngineError> {
        let mut clients = tokio::task::JoinSet::new();
        let result = loop {
            tokio::select! {
                _ = &mut stop => break Ok(()),
                result=clients.join_next(), if !clients.is_empty() => {
                    if let Some(Err(e))=result {break Err(io_error(e));}
                }
                accepted=self.listener.accept() => {
                    let (stream,_)=match accepted {Ok(v)=>v,Err(e)=>break Err(io_error(e))};
                    if clients.len()>=2 {drop(stream);continue;}
                    let engine=self.engine.clone();let authority=self.authority.clone();let timeout=self.timeout;
                    clients.spawn(async move { serve_owner(stream,engine,authority,timeout).await; });
                }
            }
            assert!(clients.len() <= 2);
        };
        self.authority
            .usable
            .store(false, std::sync::atomic::Ordering::SeqCst);
        clients.abort_all();
        while clients.join_next().await.is_some() {}
        let _barrier = tokio::time::timeout(self.timeout, self.engine.owner_egress.write())
            .await
            .map_err(|_| failure("owner_stop_outcome_unknown"))?;
        self.engine
            .owner_job(|reply| Job::OwnerStop(self.authority.host.clone(), reply))
            .await?;
        self.authority.cleanup()?;
        result
    }
}

impl Engine {
    async fn owner_job<T>(
        &self,
        build: impl FnOnce(oneshot::Sender<Result<T, EngineError>>) -> Job,
    ) -> Result<T, EngineError> {
        let (send, receive) = oneshot::channel();
        self.storage
            .sender
            .try_send(build(send))
            .map_err(|_| failure("owner_queue_not_submitted"))?;
        tokio::time::timeout(self.timeout, receive)
            .await
            .map_err(|_| failure("owner_outcome_unknown_reconcile_same_identity"))?
            .map_err(|_| failure("owner_actor_stopped_outcome_unknown"))?
    }

    async fn dispatch_owner(
        &self,
        a: Admission,
        request: OwnerRequest,
    ) -> Result<OwnerReply, EngineError> {
        request.validate()?;
        let id = request.request_id.clone();
        let response = match &request.operation {
            OwnerOperation::Apply { .. } => OwnerResponse::Applied {
                operation: self
                    .owner_job(|reply| Job::OwnerApply(a, request, reply))
                    .await?,
            },
            OwnerOperation::Read { query } => OwnerResponse::Read {
                state: self
                    .owner_job(|reply| Job::OwnerRead(a, query.clone(), reply))
                    .await?,
            },
            OwnerOperation::Reconcile {
                operation_id,
                expected_hash,
            } => {
                self.owner_job(|reply| {
                    Job::OwnerReconcile(a, operation_id.clone(), expected_hash.clone(), reply)
                })
                .await?
            }
        };
        Ok(OwnerReply {
            request_id: id,
            response,
        })
    }
}

async fn serve_owner(
    mut stream: UnixStream,
    engine: Arc<Engine>,
    authority: Arc<Authority>,
    timeout: Duration,
) {
    let exchange = async {
        verify_peer(&stream)?;
        let mut auth = [0; 37];
        stream.read_exact(&mut auth).await.map_err(io_error)?;
        let a = authority.authenticate(&auth)?;
        let frame = read_frame(&mut stream).await?;
        let request: OwnerRequest =
            serde_json::from_slice(&frame[4..]).map_err(|_| failure("owner_request_invalid"))?;
        request.validate()?;
        let id = request.request_id.clone();
        let mutation = matches!(request.operation, OwnerOperation::Apply { .. });
        let _barrier = engine.owner_egress.write().await;
        write_owner_reply(
            &mut stream,
            engine.dispatch_owner(a, request).await,
            id,
            mutation,
        )
        .await
    };
    let _ = tokio::time::timeout(timeout, exchange).await;
    let _ = stream.shutdown().await;
}

async fn write_owner_reply(
    stream: &mut UnixStream,
    result: Result<OwnerReply, EngineError>,
    id: RequestId,
    mutation: bool,
) -> Result<(), EngineError> {
    let reply = result.unwrap_or_else(|e| {
        let known = matches!(
            e.0.as_str(),
            "owner_grant_stale_or_revoked"
                | "owner_runtime_stale"
                | "owner_queue_not_submitted"
                | "operation_identity_conflict"
        );
        let response = if mutation && !known {
            OwnerResponse::OutcomeUnknown {
                operation_id: id.clone(),
            }
        } else {
            OwnerResponse::Rejected { code: e.0 }
        };
        OwnerReply {
            request_id: id,
            response,
        }
    });
    let frame = encode_frame(&reply).map_err(|_| failure("owner_response_bounds"))?;
    stream.write_all(&frame).await.map_err(io_error)
}

pub struct OwnerClient {
    root: Root,
    kind: OwnerClientKind,
    token: [u8; 32],
    socket: Identity,
    timeout: Duration,
}

impl OwnerClient {
    /// Loads one native capability from a host supplied trusted runtime root.
    /// Takes the canonical private root, fixed client kind and bounded timeout.
    /// Returns native access, without proving application identity or human consent.
    pub fn attach(
        path: &Path,
        kind: OwnerClientKind,
        timeout: Duration,
    ) -> Result<Self, EngineError> {
        if timeout.is_zero() || timeout > Duration::from_secs(30) {
            return Err(failure("owner_timeout_invalid"));
        }
        let root = Root::open(path)?;
        let i = match kind {
            OwnerClientKind::Tui => 0,
            OwnerClientKind::Desktop => 1,
        };
        let file = root.open_key(KEYS[i], false)?;
        let identity = Identity::of(&file.metadata().map_err(io_error)?);
        root.verify_key(KEYS[i], &file, identity)?;
        let mut token = [0; 32];
        file.read_exact_at(&mut token, 0).map_err(io_error)?;
        root.verify_key(KEYS[i], &file, identity)?;
        let socket = root.socket_identity()?;
        Ok(Self {
            root,
            kind,
            token,
            socket,
            timeout,
        })
    }

    /// Authenticates a fresh bounded connection for one closed owner operation.
    /// Takes a stable operation identity and returns live or historical state.
    /// Uncertain delivery requires reconciliation or retry of the same payload.
    pub async fn request(&self, request: OwnerRequest) -> Result<OwnerReply, EngineError> {
        request.validate()?;
        tokio::time::timeout(self.timeout, async {
            if self.root.socket_identity()? != self.socket {
                return Err(failure("owner_socket_replaced"));
            }
            let mut stream = UnixStream::connect(self.root.path.join(SOCKET))
                .await
                .map_err(io_error)?;
            verify_peer(&stream)?;
            if self.root.socket_identity()? != self.socket {
                return Err(failure("owner_socket_replaced"));
            }
            let mut auth = [0; 37];
            auth[..4].copy_from_slice(b"CAO1");
            auth[4] = match self.kind {
                OwnerClientKind::Tui => 0,
                OwnerClientKind::Desktop => 1,
            };
            auth[5..].copy_from_slice(&self.token);
            stream.write_all(&auth).await.map_err(io_error)?;
            stream
                .write_all(&encode_frame(&request).map_err(|_| failure("owner_request_bounds"))?)
                .await
                .map_err(|_| failure("owner_outcome_unknown_reconcile_same_identity"))?;
            let bytes = read_frame(&mut stream)
                .await
                .map_err(|_| failure("owner_outcome_unknown_reconcile_same_identity"))?;
            let reply: OwnerReply = serde_json::from_slice(&bytes[4..])
                .map_err(|_| failure("owner_outcome_unknown_reconcile_same_identity"))?;
            if reply.request_id != request.request_id {
                return Err(failure("owner_outcome_unknown_reconcile_same_identity"));
            }
            Ok(reply)
        })
        .await
        .map_err(|_| failure("owner_outcome_unknown_reconcile_same_identity"))?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn actual_socket_backpressure_holds_delivery_barrier_until_drained() {
        use crate::verification::*;
        let root = tempfile::tempdir_in("/private/tmp").unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let config =
            crate::config::resolve_config(&root.path().join("g"), &root.path().join("p")).unwrap();
        let engine = Arc::new(
            Engine::open(&root.path().join("db"), config.clone())
                .await
                .unwrap(),
        );
        let draft = BaselineDraft {
            version: 1,
            project_id: ProjectId::new("p").unwrap(),
            origin: BaselineOrigin::Imported,
            requirements: "r".repeat(4096),
            checks: vec![
                CheckSpec {
                    id: RequestId::new("code").unwrap(),
                    kind: CheckKind::Code,
                    minimum_risk: Risk::Low,
                    content: vec![127; 2500],
                },
                CheckSpec {
                    id: RequestId::new("integrity").unwrap(),
                    kind: CheckKind::TestIntegrity,
                    minimum_risk: Risk::Low,
                    content: vec![127; 2500],
                },
            ],
            rules: vec![],
            retry_limit: 0,
        };
        let id = draft.identity().unwrap();
        for (n, command) in [
            Command::CreateProject {
                project_id: draft.project_id.clone(),
                spec: ProjectSpec {
                    name: "Project".into(),
                    repositories: vec![],
                },
            },
            Command::ProposeBaseline { draft },
        ]
        .into_iter()
        .enumerate()
        {
            let req = WireRequest {
                protocol_version: PROTOCOL_VERSION,
                client_id: ClientId::new("host").unwrap(),
                request_id: RequestId::new(format!("setup-{n}")).unwrap(),
                expected_revision: Cursor::new(n as u64),
                command,
            };
            assert_eq!(engine.apply(req).await.unwrap().outcome, Outcome::Applied);
        }
        let (server, host) = OwnerServer::bind(root.path(), engine.clone(), &config)
            .await
            .unwrap();
        let mut client = UnixStream::connect(root.path().join(SOCKET)).await.unwrap();
        let (stream, _) = server.listener.accept().await.unwrap();
        nix::sys::socket::setsockopt(&stream, nix::sys::socket::sockopt::SndBuf, &1024usize)
            .unwrap();
        nix::sys::socket::setsockopt(&client, nix::sys::socket::sockopt::RcvBuf, &1024usize)
            .unwrap();
        let mut packet = [0; 37];
        packet[..4].copy_from_slice(b"CAO1");
        packet[5..].copy_from_slice(&std::fs::read(root.path().join(KEYS[0])).unwrap());
        client.write_all(&packet).await.unwrap();
        let req = OwnerRequest {
            protocol_version: PROTOCOL_VERSION,
            request_id: RequestId::new("large-read").unwrap(),
            expected_revision: Cursor::new(0),
            operation: OwnerOperation::Read {
                query: OwnerRead::Baseline { baseline_id: id },
            },
        };
        client
            .write_all(&encode_frame(&req).unwrap())
            .await
            .unwrap();
        let worker = tokio::spawn(serve_owner(
            stream,
            engine.clone(),
            server.authority.clone(),
            Duration::from_secs(2),
        ));
        let size = client.read_u32().await.unwrap() as usize;
        assert!(size > 16384 && size <= FRAME_BYTES_MAX);
        assert!(engine.owner_egress.try_write().is_err());
        assert!(engine.owner_egress.try_read().is_err());
        assert!(!worker.is_finished());
        let control = OwnerControl {
            operation_id: RequestId::new("revoke-drain").unwrap(),
            target: OwnerClientKind::Tui,
            expected_epoch: 1,
            decision: OwnerDecision::Revoke,
        };
        let mut revoke = Box::pin(host.control(control));
        assert!(
            tokio::time::timeout(Duration::from_millis(20), &mut revoke)
                .await
                .is_err()
        );
        let mut response = vec![0; size];
        client.read_exact(&mut response).await.unwrap();
        let reply: OwnerReply = serde_json::from_slice(&response).unwrap();
        assert!(matches!(reply.response, OwnerResponse::Read { .. }));
        assert!(!revoke.await.unwrap().active);
        worker.await.unwrap();
        let (_stop, rx) = oneshot::channel();
        drop(_stop);
        server.run(rx).await.unwrap();
        engine.close().await.unwrap();
    }
}
