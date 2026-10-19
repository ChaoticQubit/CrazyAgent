use super::owner::{
    OwnerClient, OwnerClientKind, OwnerData, OwnerOperation, OwnerRead, OwnerReply, OwnerRequest,
    OwnerResponse,
};
use super::owner::{OwnerRuntimeHandle, OwnerServer};
use super::{Engine, EngineError, Server, io_error, verify_peer};
use crate::config::EffectiveConfig;
use crate::protocol::{Cursor, PROTOCOL_VERSION, RequestId, SessionId};
use rustix::fs::{Mode, OFlags};
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::Read;
use std::os::unix::fs::{FileExt, FileTypeExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::UnixStream;
use tokio::sync::{oneshot, watch};
use tokio::task::JoinSet;

const LOCATOR: &str = "owner-current";
const LOCATOR_MAX: usize = 2048;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OwnerConnectionIdentity {
    database_instance: String,
    engine_epoch: String,
}

impl OwnerConnectionIdentity {
    pub fn database_instance(&self) -> &str {
        &self.database_instance
    }

    pub fn engine_epoch(&self) -> &str {
        &self.engine_epoch
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Identity {
    device: u64,
    inode: u64,
}

impl Identity {
    fn of(metadata: &std::fs::Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Locator {
    version: u8,
    database: Identity,
    db_instance: String,
    engine_epoch: String,
    generation: String,
    generation_identity: Identity,
    owner_socket: Identity,
    ordinary_name: String,
    ordinary_socket: Identity,
}

fn fail(code: &str) -> EngineError {
    EngineError(code.into())
}

fn socket_name(name: &str) -> Result<(), EngineError> {
    SessionId::new(name).map_err(|_| fail("owner_socket_name_invalid"))?;
    if matches!(name, "." | "engine.db" | "engine.engine-lock" | LOCATOR)
        || (name.len() == 9 && name.starts_with('o') && hex(&name[1..], 8))
    {
        return Err(fail("owner_socket_name_reserved"));
    }
    Ok(())
}

fn hex(value: &str, len: usize) -> bool {
    value.len() == len
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn parse_locator(bytes: &[u8]) -> Result<Locator, EngineError> {
    if bytes.is_empty() || bytes.len() > LOCATOR_MAX {
        return Err(fail("owner_locator_bounds"));
    }
    let locator: Locator =
        serde_json::from_slice(bytes).map_err(|_| fail("owner_locator_invalid"))?;
    if locator.version != 1
        || !hex(&locator.db_instance, 64)
        || !hex(&locator.engine_epoch, 64)
        || locator.generation.len() != 9
        || !locator.generation.starts_with('o')
        || !hex(&locator.generation[1..], 8)
        || socket_name(&locator.ordinary_name).is_err()
        || [
            locator.database,
            locator.generation_identity,
            locator.owner_socket,
            locator.ordinary_socket,
        ]
        .iter()
        .any(|id| id.inode == 0)
    {
        return Err(fail("owner_locator_invalid"));
    }
    Ok(locator)
}

struct Anchor {
    supplied: PathBuf,
    path: PathBuf,
    directory: File,
    identity: Identity,
}

fn safe_directory(m: &std::fs::Metadata) -> bool {
    m.is_dir() && m.uid() == rustix::process::getuid().as_raw() && m.mode() & 0o7777 == 0o700
}

fn safe_file(m: &std::fs::Metadata) -> bool {
    m.is_file()
        && m.uid() == rustix::process::getuid().as_raw()
        && m.mode() & 0o7777 == 0o600
        && m.nlink() == 1
}

impl Anchor {
    fn open(input: &Path) -> Result<Self, EngineError> {
        let supplied: PathBuf = if input.is_absolute() {
            input.to_path_buf()
        } else {
            std::env::current_dir().map_err(io_error)?.join(input)
        }
        .components()
        .collect();
        let initial = std::fs::symlink_metadata(&supplied).map_err(io_error)?;
        if !safe_directory(&initial) {
            return Err(fail("owner_anchor_requires_private_directory"));
        }
        let path = std::fs::canonicalize(&supplied).map_err(io_error)?;
        let directory = File::from(
            rustix::fs::open(
                &path,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(io_error)?,
        );
        let anchor = Self {
            supplied,
            path,
            directory,
            identity: Identity::of(&initial),
        };
        anchor.verify()?;
        Ok(anchor)
    }

    fn verify(&self) -> Result<(), EngineError> {
        for metadata in [
            self.directory.metadata().map_err(io_error)?,
            std::fs::symlink_metadata(&self.path).map_err(io_error)?,
            std::fs::symlink_metadata(&self.supplied).map_err(io_error)?,
        ] {
            if !safe_directory(&metadata) || Identity::of(&metadata) != self.identity {
                return Err(fail("owner_anchor_replaced_or_unsafe"));
            }
        }
        Ok(())
    }

    fn metadata(&self, name: &str) -> Result<std::fs::Metadata, EngineError> {
        self.verify()?;
        std::fs::symlink_metadata(self.path.join(name)).map_err(io_error)
    }
}

struct PinnedLocator {
    file: File,
    identity: Identity,
    bytes: Vec<u8>,
    value: Locator,
}

impl PinnedLocator {
    fn read(anchor: &Anchor) -> Result<Option<Self>, EngineError> {
        anchor.verify()?;
        let file = match rustix::fs::openat(
            &anchor.directory,
            LOCATOR,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
            Mode::empty(),
        ) {
            Ok(fd) => File::from(fd),
            Err(rustix::io::Errno::NOENT) => return Ok(None),
            Err(_) => return Err(fail("owner_locator_unsafe")),
        };
        let metadata = file.metadata().map_err(io_error)?;
        if !safe_file(&metadata) {
            return Err(fail("owner_locator_unsafe"));
        }
        let mut bytes = Vec::new();
        (&file)
            .take((LOCATOR_MAX + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(io_error)?;
        let value = parse_locator(&bytes)?;
        let pinned = Self {
            file,
            identity: Identity::of(&metadata),
            bytes,
            value,
        };
        pinned.verify(anchor)?;
        Ok(Some(pinned))
    }

    fn verify(&self, anchor: &Anchor) -> Result<(), EngineError> {
        let current = anchor.metadata(LOCATOR)?;
        let metadata = self.file.metadata().map_err(io_error)?;
        if !safe_file(&current)
            || !safe_file(&metadata)
            || Identity::of(&current) != self.identity
            || Identity::of(&metadata) != self.identity
            || metadata.len() != self.bytes.len() as u64
        {
            return Err(fail("owner_locator_replaced_or_unsafe"));
        }
        let mut observed = vec![0; self.bytes.len()];
        self.file
            .read_exact_at(&mut observed, 0)
            .map_err(io_error)?;
        if observed != self.bytes {
            return Err(fail("owner_locator_replaced_or_unsafe"));
        }
        Ok(())
    }
}

fn check_socket(anchor: &Anchor, name: &Path, identity: Identity) -> Result<(), EngineError> {
    anchor.verify()?;
    let metadata = std::fs::symlink_metadata(name).map_err(io_error)?;
    if !metadata.file_type().is_socket()
        || metadata.uid() != rustix::process::getuid().as_raw()
        || metadata.mode() & 0o7777 != 0o600
        || metadata.nlink() != 1
        || Identity::of(&metadata) != identity
    {
        return Err(fail("owner_endpoint_replaced_or_unsafe"));
    }
    Ok(())
}

fn check_database(anchor: &Anchor, identity: Identity) -> Result<(), EngineError> {
    let metadata = anchor.metadata("engine.db")?;
    if !safe_file(&metadata) || Identity::of(&metadata) != identity {
        return Err(fail("owner_locator_database_mismatch"));
    }
    Ok(())
}

fn check_generation(anchor: &Anchor, value: &Locator) -> Result<(), EngineError> {
    let metadata = anchor.metadata(&value.generation)?;
    if !safe_directory(&metadata) || Identity::of(&metadata) != value.generation_identity {
        return Err(fail("owner_generation_replaced_or_unsafe"));
    }
    check_socket(
        anchor,
        &anchor.path.join(&value.generation).join("owner.sock"),
        value.owner_socket,
    )
}

async fn capabilities(client: &OwnerClient) -> Result<(), EngineError> {
    let reply: OwnerReply = client
        .request(OwnerRequest {
            protocol_version: PROTOCOL_VERSION,
            request_id: RequestId::new("bootstrap-capabilities").map_err(io_error)?,
            expected_revision: Cursor::new(0),
            operation: OwnerOperation::Read {
                query: OwnerRead::Capabilities,
            },
        })
        .await?;
    if !matches!(reply.response, OwnerResponse::Read { state } if matches!(state.data, OwnerData::Capabilities { authenticated_owner_transport: true, .. }))
    {
        return Err(fail("owner_bootstrap_capabilities_unavailable"));
    }
    Ok(())
}

pub(super) struct NativeBinding {
    anchor: Anchor,
    pinned: PinnedLocator,
}

impl NativeBinding {
    pub(super) fn socket(&self) -> PathBuf {
        self.anchor.path.join(&self.pinned.value.ordinary_name)
    }

    pub(super) fn verify(&self) -> Result<(), EngineError> {
        self.pinned.verify(&self.anchor)?;
        check_database(&self.anchor, self.pinned.value.database)?;
        check_generation(&self.anchor, &self.pinned.value)?;
        check_socket(
            &self.anchor,
            &self.socket(),
            self.pinned.value.ordinary_socket,
        )
    }
}

async fn discover_owner(
    input: &Path,
    kind: OwnerClientKind,
    timeout: Duration,
) -> Result<(OwnerClient, OwnerConnectionIdentity, NativeBinding), EngineError> {
    let anchor = Anchor::open(input)?;
    let pinned = PinnedLocator::read(&anchor)?.ok_or_else(|| fail("owner_locator_missing"))?;
    let binding = NativeBinding { anchor, pinned };
    binding.verify()?;
    let client = OwnerClient::attach(
        &binding.anchor.path.join(&binding.pinned.value.generation),
        kind,
        timeout,
    )?;
    capabilities(&client).await?;
    binding.verify()?;
    let identity = OwnerConnectionIdentity {
        database_instance: binding.pinned.value.db_instance.clone(),
        engine_epoch: binding.pinned.value.engine_epoch.clone(),
    };
    Ok((client, identity, binding))
}

pub(super) async fn discover_pair(
    socket: &Path,
    kind: OwnerClientKind,
    timeout: Duration,
) -> Result<(OwnerClient, OwnerConnectionIdentity, NativeBinding), EngineError> {
    let parent = socket
        .parent()
        .ok_or_else(|| fail("socket parent missing"))?;
    let (client, identity, binding) = discover_owner(parent, kind, timeout).await?;
    if socket.file_name() != Some(std::ffi::OsStr::new(&binding.pinned.value.ordinary_name)) {
        return Err(fail("owner_ordinary_socket_mismatch"));
    }
    Ok((client, identity, binding))
}

impl OwnerClient {
    /// Discovers authenticated native access under an explicitly trusted anchor.
    /// Takes a fixed client kind and returns client plus the same observed identity.
    /// Rechecks the pinned locator after authentication without reading client SQLite.
    pub async fn discover(
        input: &Path,
        kind: OwnerClientKind,
        timeout: Duration,
    ) -> Result<(Self, OwnerConnectionIdentity), EngineError> {
        let (client, identity, _) = discover_owner(input, kind, timeout).await?;
        Ok((client, identity))
    }
}

pub struct EngineService {
    anchor: Anchor,
    engine: Arc<Engine>,
    owner_runtime: Option<OwnerRuntimeHandle>,
    timeout: Duration,
    generation: Option<(String, Identity)>,
    temporary: Option<Identity>,
    locator: Option<PinnedLocator>,
    tasks: JoinSet<Result<(), EngineError>>,
    stops: Vec<oneshot::Sender<()>>,
    closed: bool,
}

fn combine(
    primary: Result<(), EngineError>,
    cleanup: Result<(), EngineError>,
) -> Result<(), EngineError> {
    match (primary, cleanup) {
        (Ok(()), result) | (result, Ok(())) => result,
        (Err(a), Err(b)) => Err(EngineError(format!("{}; cleanup: {}", a.0, b.0))),
    }
}

fn cancelled(shutdown: &watch::Receiver<bool>) -> Result<(), EngineError> {
    if *shutdown.borrow() {
        Err(fail("engine_startup_cancelled"))
    } else {
        Ok(())
    }
}

fn check_path_length(path: &Path) -> Result<(), EngineError> {
    use std::os::unix::ffi::OsStrExt;
    let limit = if cfg!(target_os = "macos") { 104 } else { 108 };
    if path.as_os_str().as_bytes().len() >= limit {
        return Err(fail("owner_socket_path_too_long"));
    }
    Ok(())
}

impl EngineService {
    pub async fn start(
        input: &Path,
        ordinary: &str,
        config: &EffectiveConfig,
        shutdown: &watch::Receiver<bool>,
    ) -> Result<Self, EngineError> {
        cancelled(shutdown)?;
        socket_name(ordinary)?;
        let anchor = Anchor::open(input)?;
        check_path_length(&anchor.path.join(ordinary))?;
        check_path_length(&anchor.path.join("o00000000/owner.sock"))?;
        let previous = PinnedLocator::read(&anchor)?;
        let engine = Arc::new(Engine::open(&anchor.path.join("engine.db"), config.clone()).await?);
        Self::start_engine(
            anchor,
            engine,
            ordinary,
            config,
            previous.as_ref(),
            shutdown,
        )
        .await
    }

    pub async fn start_with_settings_sources(
        input: &Path,
        ordinary: &str,
        global: &Path,
        project: &Path,
        shutdown: &watch::Receiver<bool>,
    ) -> Result<Self, EngineError> {
        cancelled(shutdown)?;
        socket_name(ordinary)?;
        let anchor = Anchor::open(input)?;
        check_path_length(&anchor.path.join(ordinary))?;
        check_path_length(&anchor.path.join("o00000000/owner.sock"))?;
        let previous = PinnedLocator::read(&anchor)?;
        let (engine, config) =
            Engine::open_settings(&anchor.path.join("engine.db"), global, project).await?;
        Self::start_engine(
            anchor,
            Arc::new(engine),
            ordinary,
            &config,
            previous.as_ref(),
            shutdown,
        )
        .await
    }

    async fn start_engine(
        anchor: Anchor,
        engine: Arc<Engine>,
        ordinary: &str,
        config: &EffectiveConfig,
        previous: Option<&PinnedLocator>,
        shutdown: &watch::Receiver<bool>,
    ) -> Result<Self, EngineError> {
        let mut service = Self {
            anchor,
            engine,
            owner_runtime: None,
            timeout: Duration::from_millis(config.settings.timeout_ms),
            generation: None,
            temporary: None,
            locator: None,
            tasks: JoinSet::new(),
            stops: Vec::new(),
            closed: false,
        };
        if let Err(error) = service
            .start_inner(ordinary, config, previous, shutdown)
            .await
        {
            let cleanup = service.shutdown().await;
            return Err(combine(Err(error), cleanup).unwrap_err());
        }
        Ok(service)
    }

    /// Borrows the existing trusted native host control for this service.
    /// Takes a service reference and returns its live optional handle.
    /// Shutdown removes the handle after listeners drain.
    pub fn owner_runtime(&self) -> Option<&OwnerRuntimeHandle> {
        self.owner_runtime.as_ref()
    }

    async fn start_inner(
        &mut self,
        ordinary: &str,
        config: &EffectiveConfig,
        previous: Option<&PinnedLocator>,
        shutdown: &watch::Receiver<bool>,
    ) -> Result<(), EngineError> {
        cancelled(shutdown)?;
        let database = Identity::of(&self.anchor.metadata("engine.db")?);
        check_database(&self.anchor, database)?;
        self.create_generation()?;
        let (name, generation_identity) = self.generation.as_ref().unwrap().clone();
        let generation = self.anchor.path.join(&name);
        let (owner, host) = OwnerServer::bind(&generation, self.engine.clone(), config).await?;
        let stamp = host.identity();
        assert!(self.owner_runtime.is_none());
        self.owner_runtime = Some(host);
        let (stop, stopped) = oneshot::channel();
        self.stops.push(stop);
        self.tasks.spawn(owner.run(stopped));
        cancelled(shutdown)?;
        if let Some(old) = previous {
            old.verify(&self.anchor)?;
            if old.value.database != database || old.value.db_instance != stamp.db_instance {
                return Err(fail("owner_locator_database_mismatch"));
            }
            check_generation(&self.anchor, &old.value)?;
        }
        self.retire_socket(ordinary, previous).await?;
        let server = Server::bind(
            &self.anchor.path.join(ordinary),
            self.engine.clone(),
            config,
        )?;
        let ordinary_socket = Identity::of(&self.anchor.metadata(ordinary)?);
        let (stop, stopped) = oneshot::channel();
        self.stops.push(stop);
        self.tasks.spawn(server.run(stopped));
        tokio::task::yield_now().await;
        self.live()?;
        cancelled(shutdown)?;
        check_socket(
            &self.anchor,
            &self.anchor.path.join(ordinary),
            ordinary_socket,
        )?;
        let stream = tokio::time::timeout(
            self.timeout,
            UnixStream::connect(self.anchor.path.join(ordinary)),
        )
        .await
        .map_err(|_| fail("ordinary_readiness_timeout"))?
        .map_err(io_error)?;
        verify_peer(&stream)?;
        check_socket(
            &self.anchor,
            &self.anchor.path.join(ordinary),
            ordinary_socket,
        )?;
        drop(stream);
        for kind in [OwnerClientKind::Tui, OwnerClientKind::Desktop] {
            let client = OwnerClient::attach(&generation, kind, self.timeout)?;
            capabilities(&client).await?;
            cancelled(shutdown)?;
        }
        self.live()?;
        let owner_socket = Identity::of(
            &std::fs::symlink_metadata(generation.join("owner.sock")).map_err(io_error)?,
        );
        let value = Locator {
            version: 1,
            database,
            db_instance: stamp.db_instance,
            engine_epoch: stamp.engine_epoch,
            generation: name,
            generation_identity,
            owner_socket,
            ordinary_name: ordinary.into(),
            ordinary_socket,
        };
        self.publish(value, previous)?;
        cancelled(shutdown)?;
        Ok(())
    }

    fn create_generation(&mut self) -> Result<(), EngineError> {
        for _ in 0..8 {
            self.anchor.verify()?;
            let mut random = [0; 4];
            getrandom::fill(&mut random).map_err(|_| fail("owner_entropy_unavailable"))?;
            let name = format!(
                "o{}",
                random
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect::<String>()
            );
            match rustix::fs::mkdirat(&self.anchor.directory, &name, Mode::RWXU) {
                Ok(()) => {
                    let metadata = self.anchor.metadata(&name)?;
                    if !safe_directory(&metadata) {
                        return Err(fail("owner_generation_replaced_or_unsafe"));
                    }
                    self.generation = Some((name, Identity::of(&metadata)));
                    return Ok(());
                }
                Err(rustix::io::Errno::EXIST) => continue,
                Err(error) => return Err(io_error(error)),
            }
        }
        Err(fail("owner_generation_collision_limit"))
    }

    async fn retire_socket(
        &self,
        ordinary: &str,
        previous: Option<&PinnedLocator>,
    ) -> Result<(), EngineError> {
        self.anchor.verify()?;
        let path = self.anchor.path.join(ordinary);
        match std::fs::symlink_metadata(&path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(io_error(error)),
            Ok(_) => {}
        }
        let old = previous
            .filter(|old| old.value.ordinary_name == ordinary)
            .ok_or_else(|| fail("socket_path_already_exists"))?;
        old.verify(&self.anchor)?;
        check_socket(&self.anchor, &path, old.value.ordinary_socket)?;
        match tokio::time::timeout(self.timeout, UnixStream::connect(&path)).await {
            Ok(Err(error)) if error.kind() == std::io::ErrorKind::ConnectionRefused => {}
            _ => return Err(fail("ordinary_socket_not_proven_stale")),
        }
        old.verify(&self.anchor)?;
        check_socket(&self.anchor, &path, old.value.ordinary_socket)?;
        rustix::fs::unlinkat(
            &self.anchor.directory,
            ordinary,
            rustix::fs::AtFlags::empty(),
        )
        .map_err(io_error)
    }

    fn publish(
        &mut self,
        value: Locator,
        previous: Option<&PinnedLocator>,
    ) -> Result<(), EngineError> {
        self.anchor.verify()?;
        let (generation, identity) = self.generation.as_ref().unwrap();
        let metadata = self.anchor.metadata(generation)?;
        if !safe_directory(&metadata) || Identity::of(&metadata) != *identity {
            return Err(fail("owner_generation_replaced_or_unsafe"));
        }
        let directory = File::from(
            rustix::fs::openat(
                &self.anchor.directory,
                generation.as_str(),
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(io_error)?,
        );
        if Identity::of(&directory.metadata().map_err(io_error)?) != *identity {
            return Err(fail("owner_generation_replaced_or_unsafe"));
        }
        let file = File::from(
            rustix::fs::openat(
                &directory,
                "locator",
                OFlags::RDWR | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::RUSR | Mode::WUSR,
            )
            .map_err(io_error)?,
        );
        let file_identity = Identity::of(&file.metadata().map_err(io_error)?);
        self.temporary = Some(file_identity);
        let bytes = serde_json::to_vec(&value).map_err(io_error)?;
        parse_locator(&bytes)?;
        file.write_all_at(&bytes, 0).map_err(io_error)?;
        file.sync_all().map_err(io_error)?;
        if let Some(old) = previous {
            old.verify(&self.anchor)?;
        } else {
            match std::fs::symlink_metadata(self.anchor.path.join(LOCATOR)) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                _ => return Err(fail("owner_locator_replaced_or_unsafe")),
            }
        }
        self.anchor.verify()?;
        let temporary =
            std::fs::symlink_metadata(self.anchor.path.join(generation).join("locator"))
                .map_err(io_error)?;
        if !safe_file(&temporary) || Identity::of(&temporary) != file_identity {
            return Err(fail("owner_locator_replaced_or_unsafe"));
        }
        rustix::fs::renameat(&directory, "locator", &self.anchor.directory, LOCATOR)
            .map_err(io_error)?;
        self.temporary = None;
        self.locator = Some(PinnedLocator {
            file,
            identity: file_identity,
            bytes,
            value,
        });
        self.locator.as_ref().unwrap().verify(&self.anchor)?;
        directory.sync_all().map_err(io_error)?;
        self.anchor.directory.sync_all().map_err(io_error)
    }

    pub fn ensure_ready(&mut self, shutdown: &watch::Receiver<bool>) -> Result<(), EngineError> {
        cancelled(shutdown)?;
        self.live()?;
        let locator = self
            .locator
            .as_ref()
            .ok_or_else(|| fail("owner_locator_missing"))?;
        locator.verify(&self.anchor)?;
        check_database(&self.anchor, locator.value.database)?;
        check_generation(&self.anchor, &locator.value)?;
        check_socket(
            &self.anchor,
            &self.anchor.path.join(&locator.value.ordinary_name),
            locator.value.ordinary_socket,
        )
    }

    fn live(&mut self) -> Result<(), EngineError> {
        if let Some(result) = self.tasks.try_join_next() {
            return match result {
                Ok(Err(error)) => Err(error),
                Err(error) => Err(io_error(error)),
                Ok(Ok(())) => Err(fail("engine_listener_stopped_before_ready")),
            };
        }
        assert_eq!(self.tasks.len(), 2);
        Ok(())
    }

    pub async fn run(mut self, mut shutdown: watch::Receiver<bool>) -> Result<(), EngineError> {
        let result = if *shutdown.borrow() {
            Ok(())
        } else {
            tokio::select! {
                changed = shutdown.changed() => changed.map_err(io_error),
                ended = self.tasks.join_next() => match ended {
                    Some(Ok(Err(error))) => Err(error),
                    Some(Err(error)) => Err(io_error(error)),
                    _ => Err(fail("engine_listener_stopped")),
                }
            }
        };
        combine(result, self.shutdown().await)
    }

    pub async fn shutdown(&mut self) -> Result<(), EngineError> {
        assert!(!self.closed);
        let mut errors = Vec::new();
        if let Some(locator) = self.locator.take() {
            let result = locator.verify(&self.anchor).and_then(|()| {
                rustix::fs::unlinkat(
                    &self.anchor.directory,
                    LOCATOR,
                    rustix::fs::AtFlags::empty(),
                )
                .map_err(io_error)
            });
            if let Err(error) = result {
                errors.push(error.0);
            }
        }
        for stop in self.stops.drain(..) {
            let _ = stop.send(());
        }
        while let Some(result) = self.tasks.join_next().await {
            match result {
                Ok(Ok(())) => {}
                Ok(Err(error)) => errors.push(error.0),
                Err(error) => errors.push(error.to_string()),
            }
        }
        drop(self.owner_runtime.take());
        if let Some((generation, identity)) = self.generation.take() {
            let clean = (|| {
                let metadata = self.anchor.metadata(&generation)?;
                if !safe_directory(&metadata) || Identity::of(&metadata) != identity {
                    return Err(fail("owner_generation_replaced_or_unsafe"));
                }
                if let Some(temporary) = self.temporary.take() {
                    let path = self.anchor.path.join(&generation).join("locator");
                    let metadata = std::fs::symlink_metadata(&path).map_err(io_error)?;
                    if !safe_file(&metadata) || Identity::of(&metadata) != temporary {
                        return Err(fail("owner_locator_replaced_or_unsafe"));
                    }
                    std::fs::remove_file(path).map_err(io_error)?;
                }
                std::fs::remove_dir(self.anchor.path.join(generation)).map_err(io_error)
            })();
            if let Err(error) = clean {
                errors.push(error.0);
            }
        }
        if let Err(error) = self.engine.close().await {
            errors.push(error.0);
        }
        self.closed = true;
        if errors.is_empty() {
            Ok(())
        } else {
            Err(EngineError(errors.join("; ")))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn sample() -> Locator {
        let id = Identity {
            device: 1,
            inode: 2,
        };
        Locator {
            version: 1,
            database: id,
            db_instance: "a".repeat(64),
            engine_epoch: "b".repeat(64),
            generation: "o01234567".into(),
            generation_identity: id,
            owner_socket: id,
            ordinary_name: "engine.sock".into(),
            ordinary_socket: id,
        }
    }

    #[test]
    fn locator_accepts_nine_byte_public_generation() {
        let mut value = sample();
        value.generation = "o01234567".into();
        let parsed = parse_locator(&serde_json::to_vec(&value).unwrap()).unwrap();
        assert_eq!(parsed.generation, "o01234567");
    }

    #[tokio::test]
    async fn observed_cancellation_before_ready_cleans_started_endpoints() {
        let root = tempfile::tempdir().unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let config =
            crate::config::resolve_config(&root.path().join("g"), &root.path().join("p")).unwrap();
        let (stop, stopped) = watch::channel(false);
        let mut service = EngineService::start(root.path(), "e.sock", &config, &stopped)
            .await
            .unwrap();
        stop.send_replace(true);
        assert_eq!(
            service.ensure_ready(&stopped).unwrap_err().0,
            "engine_startup_cancelled"
        );
        service.shutdown().await.unwrap();
        assert!(!root.path().join(LOCATOR).exists());
        assert!(!root.path().join("e.sock").exists());
        let engine = Engine::open(&root.path().join("engine.db"), config)
            .await
            .unwrap();
        engine.close().await.unwrap();
    }

    #[tokio::test]
    async fn actual_listener_completion_stops_sibling_and_releases_database() {
        let root = tempfile::tempdir().unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let config =
            crate::config::resolve_config(&root.path().join("g"), &root.path().join("p")).unwrap();
        let (_stop, stopped) = watch::channel(false);
        let mut service = EngineService::start(root.path(), "e.sock", &config, &stopped)
            .await
            .unwrap();
        service.stops.remove(0).send(()).unwrap();
        let error = service.run(stopped).await.unwrap_err();
        assert_eq!(error.0, "engine_listener_stopped");
        assert!(!root.path().join(LOCATOR).exists());
        assert!(!root.path().join("e.sock").exists());
        let engine = Engine::open(&root.path().join("engine.db"), config)
            .await
            .unwrap();
        engine.close().await.unwrap();
    }

    #[test]
    fn locator_validation_has_strict_bounds_and_components() {
        let valid = serde_json::to_vec(&sample()).unwrap();
        assert!(parse_locator(&valid).is_ok());
        assert_eq!(
            parse_locator(&vec![b' '; LOCATOR_MAX + 1]).unwrap_err().0,
            "owner_locator_bounds"
        );
        let mut extra = serde_json::to_value(sample()).unwrap();
        extra["external_path"] = "/foreign".into();
        assert_eq!(
            parse_locator(&serde_json::to_vec(&extra).unwrap())
                .unwrap_err()
                .0,
            "owner_locator_invalid"
        );
        for generation in [
            "../foreign",
            "/absolute",
            "o0123456G",
            "o012345é",
            "o0123456789abcdef",
        ] {
            let mut bad = sample();
            bad.generation = generation.into();
            assert_eq!(
                parse_locator(&serde_json::to_vec(&bad).unwrap())
                    .unwrap_err()
                    .0,
                "owner_locator_invalid"
            );
        }
        for ordinary in [
            ".",
            "engine.db",
            "engine.engine-lock",
            LOCATOR,
            "../foreign",
            "o01234567",
        ] {
            let mut bad = sample();
            bad.ordinary_name = ordinary.into();
            assert_eq!(
                parse_locator(&serde_json::to_vec(&bad).unwrap())
                    .unwrap_err()
                    .0,
                "owner_locator_invalid"
            );
        }
        let mut duplicate = String::from_utf8(valid).unwrap();
        duplicate.insert_str(1, "\"version\":1,");
        assert_eq!(
            parse_locator(duplicate.as_bytes()).unwrap_err().0,
            "owner_locator_invalid"
        );
    }

    #[test]
    fn locator_preserves_unsafe_files_and_detects_content_replacement() {
        let root = tempfile::tempdir().unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let anchor = Anchor::open(root.path()).unwrap();
        let path = root.path().join(LOCATOR);
        std::fs::write(&path, serde_json::to_vec(&sample()).unwrap()).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let pinned = PinnedLocator::read(&anchor).unwrap().unwrap();
        std::fs::hard_link(&path, root.path().join("link")).unwrap();
        assert_eq!(
            PinnedLocator::read(&anchor).err().unwrap().0,
            "owner_locator_unsafe"
        );
        assert!(pinned.verify(&anchor).is_err());
        std::fs::remove_file(root.path().join("link")).unwrap();
        let mut changed = sample();
        changed.engine_epoch = "c".repeat(64);
        std::fs::write(&path, serde_json::to_vec(&changed).unwrap()).unwrap();
        assert_eq!(
            pinned.verify(&anchor).unwrap_err().0,
            "owner_locator_replaced_or_unsafe"
        );
        assert!(path.exists());
    }
}
