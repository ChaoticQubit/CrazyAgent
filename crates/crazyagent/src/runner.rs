use crate::protocol::{
    ControllerEpoch, FRAME_BYTES_MAX, PROTOCOL_VERSION, RequestId, encode_frame,
};
use crazyagent::adapters::NativeAction;
use crazyagent::worker_dispatch::{
    AdmissionCheckOffer, DeniedReceipt, FenceOrder, FenceReceipt,
    RunnerBootIdentity as RunnerIdentity, RunnerReady,
};
use rustix::fs::{FlockOperation, Mode, OFlags, flock, open, openat};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{Mutex, Notify};
use tokio::task::JoinSet;

#[path = "runner/dispatch.rs"]
mod dispatch;

const OPERATIONS_MAX: usize = 128;
const EPOCHS_MAX: usize = 128;
const CONNECTIONS_MAX: usize = 16;
const TIMEOUT: Duration = Duration::from_secs(1);
const JOURNAL_BYTES_MAX: u64 = 1_048_576;

#[derive(Deserialize, Serialize)]
#[serde(tag = "record", rename_all = "snake_case", deny_unknown_fields)]
enum Record {
    Started {
        protocol_version: u16,
        identity: RunnerIdentity,
    },
    Endpoint {
        device: u64,
        inode: u64,
    },
    Claimed {
        generation: u64,
        epoch: ControllerEpoch,
    },
    Unavailable {
        operation: RequestId,
        epoch: ControllerEpoch,
        action: NativeAction,
    },
    DispatchDenied {
        offer: Box<AdmissionCheckOffer>,
        receipt: Box<DeniedReceipt>,
    },
    DispatchFence {
        order: FenceOrder,
        receipt: Box<FenceReceipt>,
    },
}

struct Journal {
    file: File,
    directory: File,
    root: PathBuf,
    bytes: u64,
    sequence: u64,
}

impl Journal {
    fn open(directory: &File, root: &Path) -> io::Result<(Self, Vec<Record>)> {
        let mut file = private_file(directory, "control.log")?;
        flock(&file, FlockOperation::NonBlockingLockExclusive)?;
        let mut bytes = Vec::new();
        Read::take(&mut file, JOURNAL_BYTES_MAX + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > JOURNAL_BYTES_MAX {
            return Err(invalid("journal_exceeds_capacity"));
        }
        let records = decode_records(&bytes)?;
        directory.sync_all()?;
        let journal = Self {
            file,
            directory: directory.try_clone()?,
            root: root.to_owned(),
            bytes: bytes.len() as u64,
            sequence: records.len() as u64,
        };
        journal.verify()?;
        assert!(journal.bytes <= JOURNAL_BYTES_MAX);
        assert!(records.len() <= bytes.len() / 5);
        Ok((journal, records))
    }

    fn verify(&self) -> io::Result<()> {
        let actual = self.file.metadata()?;
        let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK;
        let named = File::from(openat(
            &self.directory,
            "control.log",
            flags,
            Mode::empty(),
        )?)
        .metadata()?;
        let root = std::fs::symlink_metadata(&self.root)?;
        let directory = self.directory.metadata()?;
        if !actual.is_file()
            || actual.mode() & 0o777 != 0o600
            || actual.nlink() != 1
            || actual.len() != self.bytes
            || actual.dev() != named.dev()
            || actual.ino() != named.ino()
            || root.dev() != directory.dev()
            || root.ino() != directory.ino()
            || !root.is_dir()
            || root.mode() & 0o777 != 0o700
        {
            return Err(invalid("journal_identity_or_length_changed"));
        }
        assert_eq!(actual.len(), self.bytes);
        assert_eq!(actual.ino(), named.ino());
        Ok(())
    }

    fn append(&mut self, record: &Record) -> io::Result<()> {
        self.verify()?;
        let bytes = encode_frame(record).map_err(invalid)?;
        let next = self
            .bytes
            .checked_add(bytes.len() as u64)
            .ok_or_else(|| invalid("journal_overflow"))?;
        if next > JOURNAL_BYTES_MAX {
            return Err(invalid("journal_capacity"));
        }
        let sequence = self.sequence.checked_add(1).ok_or_else(|| invalid("journal_sequence_overflow"))?;
        self.file.write_all(&bytes)?;
        self.file.sync_all()?;
        self.bytes = next;
        self.sequence = sequence;
        self.verify()?;
        assert!(self.bytes <= JOURNAL_BYTES_MAX);
        assert_eq!(self.file.metadata()?.len(), next);
        Ok(())
    }
}

fn decode_records(bytes: &[u8]) -> io::Result<Vec<Record>> {
    let mut position = 0usize;
    let mut records = Vec::new();
    while position < bytes.len() {
        let header = bytes
            .get(position..position + 4)
            .ok_or_else(|| invalid("torn_journal_header"))?;
        let size = u32::from_be_bytes(header.try_into().map_err(invalid)?) as usize;
        if size == 0 || size > FRAME_BYTES_MAX - 4 {
            return Err(invalid("invalid_journal_frame_size"));
        }
        let end = position + 4 + size;
        let payload = bytes
            .get(position + 4..end)
            .ok_or_else(|| invalid("torn_journal_record"))?;
        records.push(serde_json::from_slice(payload).map_err(invalid)?);
        position = end;
        assert!(position <= bytes.len());
        assert!(records.len() <= bytes.len() / 5);
    }
    assert_eq!(position, bytes.len());
    Ok(records)
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    protocol_version: u16,
    command: Command,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Command {
    Status,
    AttachEngine {
        registration_id: RequestId,
        boot: RunnerIdentity,
    },
    Claim {
        runner_id: RequestId,
        expected_generation: u64,
        controller_epoch: ControllerEpoch,
    },
    Operate {
        runner_id: RequestId,
        controller_epoch: ControllerEpoch,
        operation_id: RequestId,
        action: NativeAction,
    },
    Deliver {
        dispatch_id: RequestId,
    },
    Reconcile {
        dispatch_id: RequestId,
    },
    DispatchStatus {
        dispatch_id: RequestId,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Reply {
    Status {
        identity: RunnerIdentity,
        controller_epoch: Option<ControllerEpoch>,
        generation: u64,
        operation_count: usize,
        native_admission: &'static str,
        native_identity: Option<RequestId>,
        engine_registration: Option<RunnerReady>,
    },
    EngineReady {
        identity: RunnerReady,
    },
    Claimed {
        generation: u64,
        controller_epoch: ControllerEpoch,
    },
    Operation {
        operation_id: RequestId,
        state: &'static str,
        reason: &'static str,
        native_identity: Option<RequestId>,
    },
    DispatchStatus {
        dispatch_id: RequestId,
        state: &'static str,
        receipt: Option<Box<DeniedReceipt>>,
        fence: Option<Box<FenceReceipt>>,
    },
    Error {
        code: &'static str,
    },
}

struct State {
    identity: RunnerIdentity,
    generation: u64,
    controller: Option<ControllerEpoch>,
    epochs: BTreeSet<ControllerEpoch>,
    operations: BTreeMap<RequestId, NativeAction>,
    denials: BTreeMap<(RequestId, u64), (AdmissionCheckOffer, DeniedReceipt)>,
    fences: BTreeMap<(RequestId, u64), (FenceOrder, FenceReceipt)>,
    journal: Journal,
    endpoint: Option<(u64, u64)>,
    failed: bool,
    engine_registration: Option<RunnerReady>,
}

impl State {
    fn new(journal: Journal, records: Vec<Record>) -> io::Result<Self> {
        let identity = fresh_identity()?;
        let mut state = Self {
            identity: identity.clone(),
            generation: 0,
            controller: None,
            epochs: BTreeSet::new(),
            operations: BTreeMap::new(),
            denials: BTreeMap::new(),
            fences: BTreeMap::new(),
            journal,
            endpoint: None,
            failed: false,
            engine_registration: None,
        };
        state.restore(records)?;
        state.identity = identity.clone();
        state.controller = None;
        state.journal.append(&Record::Started {
            protocol_version: PROTOCOL_VERSION,
            identity,
        })?;
        state.assert_consistent();
        assert!(state.controller.is_none());
        assert_eq!(state.identity.pid, std::process::id());
        Ok(state)
    }

    fn restore(&mut self, records: Vec<Record>) -> io::Result<()> {
        let mut boots = BTreeSet::new();
        for (index, record) in records.into_iter().enumerate() {
            if boots.is_empty() && !matches!(record, Record::Started { .. }) {
                return Err(invalid("journal_missing_boot_identity"));
            }
            match record {
                Record::Started {
                    protocol_version,
                    identity,
                } => {
                    if protocol_version != PROTOCOL_VERSION
                        || identity.validate().is_err()
                        || !boots.insert(identity.runner_id.clone())
                    {
                        return Err(invalid("invalid_journal_boot"));
                    }
                    self.identity = identity;
                    self.controller = None;
                }
                Record::Endpoint { device, inode } => self.endpoint = Some((device, inode)),
                Record::Claimed { generation, epoch } => {
                    if generation != self.generation + 1
                        || self.epochs.len() + self.fences.len() >= EPOCHS_MAX
                        || !self.epochs.insert(epoch.clone())
                    {
                        return Err(invalid("invalid_journal_epoch"));
                    }
                    self.generation = generation;
                    self.controller = Some(epoch);
                }
                Record::Unavailable {
                    operation,
                    epoch,
                    action,
                } => {
                    if self.controller.as_ref() != Some(&epoch)
                        || self.operation_count() >= OPERATIONS_MAX
                        || self.operations.insert(operation, action).is_some()
                    {
                        return Err(invalid("invalid_journal_operation"));
                    }
                }
                Record::DispatchDenied { offer, receipt } => {
                    self.restore_denied(*offer, *receipt, index as u64 + 1)?;
                }
                Record::DispatchFence { order, receipt } => {
                    self.restore_fence(order, *receipt, index as u64 + 1)?;
                }
            }
            self.assert_consistent();
            assert!(!boots.is_empty());
            assert!(boots.contains(&self.identity.runner_id));
        }
        Ok(())
    }

    fn assert_consistent(&self) {
        assert!(self.identity.validate().is_ok());
        if let Some(ready) = &self.engine_registration {
            assert_eq!(ready.boot, self.identity);
        }
        assert_eq!(self.generation, self.epochs.len() as u64);
        assert!(self.epochs.len() <= EPOCHS_MAX);
        assert!(self.operation_count() <= OPERATIONS_MAX);
        self.dispatch_invariants();
        if let Some(epoch) = &self.controller {
            assert!(self.epochs.contains(epoch));
        }
    }

    fn apply(&mut self, request: Request) -> io::Result<Reply> {
        self.assert_consistent();
        if self.failed || self.journal.verify().is_err() {
            self.failed = true;
            return Err(invalid("journal_unavailable"));
        }
        if request.protocol_version != PROTOCOL_VERSION {
            return Ok(Reply::Error {
                code: "unsupported_version",
            });
        }
        let reply = match request.command {
            Command::Status => Ok(Reply::Status {
                identity: self.identity.clone(),
                controller_epoch: self.controller.clone(),
                generation: self.generation,
                operation_count: self.operation_count(),
                native_admission: "unavailable",
                native_identity: None,
                engine_registration: self.engine_registration.clone(),
            }),
            Command::AttachEngine { .. } => Ok(Reply::Error {
                code: "engine_registration_requires_authenticated_callback",
            }),
            Command::Deliver { .. } | Command::Reconcile { .. } => Ok(Reply::Error {
                code: "dispatch_requires_authenticated_callback",
            }),
            Command::DispatchStatus { dispatch_id } => Ok(self.dispatch_status(&dispatch_id)),
            Command::Claim {
                runner_id,
                expected_generation,
                controller_epoch,
            } => self.claim(runner_id, expected_generation, controller_epoch),
            Command::Operate {
                runner_id,
                controller_epoch,
                operation_id,
                action,
            } => self.operate(runner_id, controller_epoch, operation_id, action),
        };
        if reply.is_err() {
            self.failed = true;
        }
        self.assert_consistent();
        reply
    }

    fn claim(
        &mut self,
        runner: RequestId,
        generation: u64,
        epoch: ControllerEpoch,
    ) -> io::Result<Reply> {
        if runner != self.identity.runner_id {
            return Ok(Reply::Error {
                code: "runner_identity_mismatch",
            });
        }
        if generation != self.generation {
            return Ok(Reply::Error {
                code: "stale_generation",
            });
        }
        if self.epochs.contains(&epoch) {
            return Ok(Reply::Error {
                code: "retired_controller",
            });
        }
        if self.epochs.len() + self.fences.len() == EPOCHS_MAX {
            return Ok(Reply::Error {
                code: "epoch_capacity",
            });
        }
        self.journal.append(&Record::Claimed {
            generation: generation + 1,
            epoch: epoch.clone(),
        })?;
        let inserted = self.epochs.insert(epoch.clone());
        assert!(inserted);
        self.controller = Some(epoch.clone());
        self.generation += 1;
        self.assert_consistent();
        assert_eq!(self.generation, generation + 1);
        Ok(Reply::Claimed {
            generation: self.generation,
            controller_epoch: epoch,
        })
    }

    fn operate(
        &mut self,
        runner: RequestId,
        epoch: ControllerEpoch,
        operation: RequestId,
        action: NativeAction,
    ) -> io::Result<Reply> {
        if runner != self.identity.runner_id {
            return Ok(Reply::Error {
                code: "runner_identity_mismatch",
            });
        }
        if self.controller.as_ref() != Some(&epoch) {
            return Ok(Reply::Error {
                code: "stale_controller",
            });
        }
        if let Some(original) = self.operations.get(&operation) {
            if original != &action {
                return Ok(Reply::Error {
                    code: "operation_conflict",
                });
            }
        } else {
            if self.operation_count() == OPERATIONS_MAX {
                return Ok(Reply::Error {
                    code: "operation_capacity",
                });
            }
            self.journal.append(&Record::Unavailable {
                operation: operation.clone(),
                epoch,
                action: action.clone(),
            })?;
            let previous = self.operations.insert(operation.clone(), action);
            assert!(previous.is_none());
        }
        self.assert_consistent();
        assert!(self.operations.contains_key(&operation));
        Ok(Reply::Operation {
            operation_id: operation,
            state: "unavailable",
            reason: "native_runtime_admission_unavailable",
            native_identity: None,
        })
    }
}

fn fresh_identity() -> io::Result<RunnerIdentity> {
    let mut entropy = [0; 16];
    File::open("/dev/urandom")?.read_exact(&mut entropy)?;
    let runner_id =
        RequestId::new(format!("r-{:032x}", u128::from_be_bytes(entropy))).map_err(invalid)?;
    let identity = RunnerIdentity {
        runner_id,
        pid: std::process::id(),
        started_at_unix_ns: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(invalid)?
            .as_nanos()
            .to_string(),
    };
    assert_eq!(identity.pid, std::process::id());
    assert_eq!(identity.runner_id.as_str().len(), 34);
    Ok(identity)
}

fn invalid(error: impl std::fmt::Display) -> io::Error {
    io::Error::other(error.to_string())
}

fn private_directory(path: &Path) -> io::Result<File> {
    if !path.is_absolute() || path.as_os_str().len() > 80 || path.components().count() > 32 {
        return Err(invalid("invalid_private_directory_path"));
    }
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let mut directory = File::from(open("/", flags, Mode::empty())?);
    for component in path.components() {
        match component {
            Component::RootDir => {}
            Component::Normal(name) => {
                directory = File::from(openat(&directory, name, flags, Mode::empty())?)
            }
            _ => return Err(invalid("invalid_private_directory_component")),
        }
    }
    let metadata = directory.metadata()?;
    if metadata.uid() != rustix::process::getuid().as_raw() || metadata.mode() & 0o777 != 0o700 {
        return Err(invalid("runtime_directory_requires_owner_mode_0700"));
    }
    assert!(metadata.is_dir());
    assert_eq!(metadata.mode() & 0o777, 0o700);
    Ok(directory)
}

fn private_file(directory: &File, name: &str) -> io::Result<File> {
    let flags = OFlags::RDWR
        | OFlags::CREATE
        | OFlags::APPEND
        | OFlags::NOFOLLOW
        | OFlags::CLOEXEC
        | OFlags::NONBLOCK;
    let file = File::from(openat(directory, name, flags, Mode::RUSR | Mode::WUSR)?);
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.uid() != rustix::process::getuid().as_raw()
        || metadata.mode() & 0o777 != 0o600
        || metadata.nlink() != 1
    {
        return Err(invalid("state_requires_private_regular_owner_file"));
    }
    assert!(metadata.is_file());
    assert_eq!(metadata.nlink(), 1);
    Ok(file)
}

fn verify_peer(stream: &UnixStream) -> io::Result<()> {
    #[cfg(target_os = "macos")]
    let uid = nix::unistd::getpeereid(stream)?.0.as_raw();
    #[cfg(not(target_os = "macos"))]
    let uid = stream.peer_cred()?.uid();
    if uid != rustix::process::getuid().as_raw() {
        return Err(invalid("peer_owner_mismatch"));
    }
    assert_eq!(uid, rustix::process::getuid().as_raw());
    Ok(())
}

async fn read_request(stream: &mut UnixStream) -> io::Result<Request> {
    let size = stream.read_u32().await? as usize;
    if size == 0 || size > FRAME_BYTES_MAX - 4 {
        return Err(invalid("invalid_frame_size"));
    }
    let mut bytes = vec![0; size];
    stream.read_exact(&mut bytes).await?;
    assert_eq!(bytes.len(), size);
    assert!(bytes.len() < FRAME_BYTES_MAX);
    serde_json::from_slice(&bytes).map_err(invalid)
}

async fn attach_engine(
    state: Arc<Mutex<State>>,
    protocol_version: u16,
    registration_id: RequestId,
    boot: RunnerIdentity,
) -> Reply {
    if protocol_version != PROTOCOL_VERSION {
        return Reply::Error {
            code: "unsupported_version",
        };
    }
    let (root, observed) = {
        let mut state = state.lock().await;
        if !matches!(
            state.apply(Request {
                protocol_version: PROTOCOL_VERSION,
                command: Command::Status,
            }),
            Ok(Reply::Status { .. })
        ) {
            return Reply::Error {
                code: "journal_unavailable",
            };
        }
        if state.identity != boot {
            return Reply::Error {
                code: "runner_identity_mismatch",
            };
        }
        if state
            .engine_registration
            .as_ref()
            .is_some_and(|ready| ready.registration_id != registration_id)
        {
            return Reply::Error {
                code: "engine_registration_conflict",
            };
        }
        (state.journal.root.clone(), state.identity.clone())
    };
    let ready =
        match crazyagent::engine::owner::runner_ready(&root, &registration_id, &observed, TIMEOUT)
            .await
        {
            Ok(ready) => ready,
            Err(_) => {
                return Reply::Error {
                    code: "engine_registration_unavailable",
                };
            }
        };
    let mut state = state.lock().await;
    if !matches!(
        state.apply(Request {
            protocol_version: PROTOCOL_VERSION,
            command: Command::Status,
        }),
        Ok(Reply::Status { .. })
    ) {
        return Reply::Error {
            code: "journal_unavailable",
        };
    }
    state.register_engine(&registration_id, ready)
}

async fn exchange(mut stream: UnixStream, state: Arc<Mutex<State>>) -> io::Result<()> {
    verify_peer(&stream)?;
    let reply = match read_request(&mut stream).await {
        Ok(Request {
            protocol_version,
            command:
                Command::AttachEngine {
                    registration_id,
                    boot,
                },
        }) => attach_engine(state.clone(), protocol_version, registration_id, boot).await,
        Ok(Request {
            protocol_version,
            command: Command::Deliver { dispatch_id },
        }) => dispatch::deliver(state.clone(), protocol_version, dispatch_id).await,
        Ok(Request {
            protocol_version,
            command: Command::Reconcile { dispatch_id },
        }) => dispatch::reconcile(state.clone(), protocol_version, dispatch_id).await,
        Ok(request) => state.lock().await.apply(request).unwrap_or(Reply::Error {
            code: "journal_unavailable",
        }),
        Err(_) => Reply::Error {
            code: "invalid_request",
        },
    };
    let frame = encode_frame(&reply).map_err(invalid)?;
    assert!(frame.len() <= FRAME_BYTES_MAX);
    assert!(frame.len() >= 4);
    stream.write_all(&frame).await?;
    Ok(())
}

pub async fn run(root: &Path) -> io::Result<()> {
    let directory =
        private_directory(root).map_err(|error| invalid(format!("runner_directory: {error}")))?;
    let _bootstrap = crazyagent::engine::owner::runner_bootstrap(root).map_err(invalid)?;
    let lock = private_file(&directory, "owner.lock")
        .map_err(|error| invalid(format!("runner_lock_file: {error}")))?;
    flock(&lock, FlockOperation::NonBlockingLockExclusive)
        .map_err(|error| invalid(format!("runner_lock: {error}")))?;
    let (journal, records) = Journal::open(&directory, root)?;
    let mut state = State::new(journal, records)?;
    let path = root.join("control.sock");
    if std::fs::symlink_metadata(&path).is_ok() {
        let (device, inode) = state
            .endpoint
            .ok_or_else(|| invalid("unrecognized_control_endpoint"))?;
        remove_endpoint(&path, device, inode)?;
    }
    let listener =
        UnixListener::bind(&path).map_err(|error| invalid(format!("runner_bind: {error}")))?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    let metadata = std::fs::symlink_metadata(&path)?;
    assert!(metadata.file_type().is_socket());
    assert_eq!(metadata.mode() & 0o777, 0o600);
    if let Err(error) = state.journal.append(&Record::Endpoint {
        device: metadata.dev(),
        inode: metadata.ino(),
    }) {
        remove_endpoint(&path, metadata.dev(), metadata.ino())?;
        return Err(error);
    }
    let state = Arc::new(Mutex::new(state));
    let result = serve(listener, state).await;
    remove_endpoint(&path, metadata.dev(), metadata.ino())?;
    result
}

fn remove_endpoint(path: &PathBuf, device: u64, inode: u64) -> io::Result<()> {
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.file_type().is_socket() || metadata.dev() != device || metadata.ino() != inode {
        return Err(invalid("endpoint_identity_changed"));
    }
    assert_eq!(metadata.dev(), device);
    assert_eq!(metadata.ino(), inode);
    std::fs::remove_file(path)
}

async fn serve(listener: UnixListener, state: Arc<Mutex<State>>) -> io::Result<()> {
    let mut clients = JoinSet::new();
    let mut termination =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let shutdown = Arc::new(Notify::new());
    loop {
        tokio::select! {
            _ = termination.recv() => break,
            result = tokio::signal::ctrl_c() => { result?; break; }
            _ = shutdown.notified() => break,
            Some(result) = clients.join_next(), if !clients.is_empty() => { result.map_err(invalid)?; }
            incoming = listener.accept(), if clients.len() < CONNECTIONS_MAX => {
                let (stream, _) = incoming?;
                let state = state.clone();
                let shutdown = shutdown.clone();
                clients.spawn(async move {
                    let _result = tokio::time::timeout(TIMEOUT, exchange(stream, state.clone())).await;
                    if state.lock().await.failed {
                        shutdown.notify_one();
                    }
                });
            }
        }
        assert!(clients.len() <= CONNECTIONS_MAX);
    }
    clients.abort_all();
    while let Some(result) = clients.join_next().await {
        if let Err(error) = result
            && !error.is_cancelled()
        {
            return Err(invalid(error));
        }
    }
    assert!(clients.is_empty());
    if state.lock().await.failed {
        return Err(invalid("journal_unavailable"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn real_state() -> (tempfile::TempDir, State) {
        let root = tempfile::Builder::new()
            .prefix("ca-i-")
            .tempdir_in("/tmp")
            .unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = root.path().canonicalize().unwrap();
        let directory = private_directory(&path).unwrap();
        let (journal, records) = Journal::open(&directory, &path).unwrap();
        let state = State::new(journal, records).unwrap();
        (root, state)
    }

    #[test]
    #[should_panic(expected = "assertion failed: self.identity.validate().is_ok()")]
    fn internal_boot_invariant_detects_zero_pid() {
        let (_root, mut state) = real_state();
        state.identity.pid = 0;
        state.assert_consistent();
    }

    #[test]
    #[should_panic(expected = "assertion `left == right` failed")]
    fn internal_generation_invariant_detects_corrupted_epoch_count() {
        let (_root, mut state) = real_state();
        state.generation = 1;
        state.assert_consistent();
    }

    #[test]
    #[should_panic(expected = "assertion failed: self.epochs.contains(epoch)")]
    fn internal_controller_invariant_detects_an_orphaned_epoch() {
        let (_root, mut state) = real_state();
        state.controller = Some(ControllerEpoch::new("orphaned").unwrap());
        state.assert_consistent();
    }

    #[test]
    fn oversized_journal_is_rejected_without_truncating_evidence() {
        let (root, state) = real_state();
        let path = root.path().canonicalize().unwrap();
        drop(state);
        let journal = path.join("control.log");
        File::options()
            .write(true)
            .open(&journal)
            .unwrap()
            .set_len(JOURNAL_BYTES_MAX + 1)
            .unwrap();
        let directory = private_directory(&path).unwrap();
        let error = Journal::open(&directory, &path).err().unwrap();
        assert!(error.to_string().contains("journal_exceeds_capacity"));
        assert_eq!(
            std::fs::metadata(journal).unwrap().len(),
            JOURNAL_BYTES_MAX + 1
        );
    }

    #[test]
    fn journal_cannot_claim_a_native_running_or_successful_outcome() {
        let (root, state) = real_state();
        let path = root.path().canonicalize().unwrap();
        drop(state);
        let journal = path.join("control.log");
        let fabricated = serde_json::json!({"record":"running", "operation":"not-native-evidence"});
        File::options()
            .append(true)
            .open(&journal)
            .unwrap()
            .write_all(&encode_frame(&fabricated).unwrap())
            .unwrap();
        let before = std::fs::read(&journal).unwrap();
        let directory = private_directory(&path).unwrap();
        assert!(Journal::open(&directory, &path).is_err());
        assert_eq!(std::fs::read(journal).unwrap(), before);
    }
}
