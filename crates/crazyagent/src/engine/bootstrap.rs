#[cfg(unix)]
use super::Server;
use super::owner::OwnerRuntimeHandle;
#[cfg(unix)]
use super::owner::OwnerServer;
use super::owner::{
    OwnerClient, OwnerClientKind, OwnerData, OwnerOperation, OwnerRead, OwnerReply, OwnerRequest,
    OwnerResponse,
};
#[cfg(unix)]
use super::verify_peer;
use super::{Engine, EngineError, io_error};
use crate::config::EffectiveConfig;
use crate::protocol::{Cursor, PROTOCOL_VERSION, RequestId, SessionId};
#[cfg(unix)]
use rustix::fs::{Mode, OFlags};
use serde::{Deserialize, Serialize};
use std::fs::File;
#[cfg(unix)]
use std::io::Read;
#[cfg(unix)]
use std::os::unix::fs::{FileExt, FileTypeExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{oneshot, watch};
use tokio::task::JoinSet;

#[cfg(unix)]
mod native_profiles;
#[cfg(not(unix))]
#[path = "bootstrap/native_profiles_unavailable.rs"]
mod native_profiles;
#[cfg(all(test, unix))]
pub(crate) use native_profiles::MAX_PROFILES;
pub(crate) use native_profiles::{
    AgentCredential, NativeProfileCustody, NativeProfileUse, NativeProfileUsePurpose,
};

const LOCATOR: &str = "owner-current";
#[cfg(unix)]
const LOCATOR_MAX: usize = 2048;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OwnerConnectionIdentity {
    database_instance: String,
    engine_epoch: String,
    engine_version: String,
}

impl OwnerConnectionIdentity {
    pub fn database_instance(&self) -> &str {
        &self.database_instance
    }

    pub fn engine_epoch(&self) -> &str {
        &self.engine_epoch
    }

    /// The version the connected engine published at start; empty for an engine that
    /// predates the version handshake.
    pub fn engine_version(&self) -> &str {
        &self.engine_version
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Identity {
    device: u64,
    inode: u64,
}

impl Identity {
    /// A placeholder identity where the standard library has no stable device and inode.
    /// Takes the metadata, unused. Returns zeros; `safe_directory` and `safe_file` reject
    /// every Windows entry first, so no Windows decision ever rests on this value.
    #[cfg(not(unix))]
    fn of(_metadata: &std::fs::Metadata) -> Self {
        Self {
            device: 0,
            inode: 0,
        }
    }

    #[cfg(unix)]
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
    #[serde(default)]
    engine_version: String,
}

#[cfg(unix)]
const ENGINE_VERSION_BYTES_MAX: usize = 64;

#[cfg(unix)]
fn engine_version_valid(version: &str) -> bool {
    version.len() <= ENGINE_VERSION_BYTES_MAX && !version.chars().any(char::is_control)
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

#[cfg(unix)]
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
        || !engine_version_valid(&locator.engine_version)
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

/// Rejects every directory where the Unix owner and mode check does not exist.
/// Takes the metadata, unused. Returns false, so Windows trusts no anchor or generation.
#[cfg(not(unix))]
fn safe_directory(_m: &std::fs::Metadata) -> bool {
    false
}

/// Rejects every file where the Unix owner, mode and link check does not exist.
/// Takes the metadata, unused. Returns false, so Windows trusts no locator or database.
#[cfg(not(unix))]
fn safe_file(_m: &std::fs::Metadata) -> bool {
    false
}

#[cfg(unix)]
fn safe_directory(m: &std::fs::Metadata) -> bool {
    m.is_dir() && m.uid() == rustix::process::getuid().as_raw() && m.mode() & 0o7777 == 0o700
}

#[cfg(unix)]
fn safe_file(m: &std::fs::Metadata) -> bool {
    m.is_file()
        && m.uid() == rustix::process::getuid().as_raw()
        && m.mode() & 0o7777 == 0o600
        && m.nlink() == 1
}

impl Anchor {
    /// Refuses the engine root as an owner anchor without Unix directory-relative opens.
    /// Takes the root, unused.
    /// Returns `owner_anchor_unavailable` until Windows has owner-only directory checks.
    #[cfg(not(unix))]
    fn open(_input: &Path) -> Result<Self, EngineError> {
        Err(fail("owner_anchor_unavailable"))
    }

    #[cfg(unix)]
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
    #[cfg(unix)]
    file: File,
    #[cfg(unix)]
    identity: Identity,
    #[cfg(unix)]
    bytes: Vec<u8>,
    value: Locator,
}

impl PinnedLocator {
    /// Refuses to read the owner locator without Unix directory-relative opens.
    /// Takes the anchor, unused.
    /// Returns `owner_anchor_unavailable` until Windows has owner-only directory checks.
    #[cfg(not(unix))]
    fn read(_anchor: &Anchor) -> Result<Option<Self>, EngineError> {
        Err(fail("owner_anchor_unavailable"))
    }

    #[cfg(unix)]
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

    /// Refuses to confirm the owner locator without positioned Unix reads.
    /// Takes the anchor, unused.
    /// Returns `owner_anchor_unavailable` until Windows has owner-only directory checks.
    #[cfg(not(unix))]
    fn verify(&self, _anchor: &Anchor) -> Result<(), EngineError> {
        Err(fail("owner_anchor_unavailable"))
    }

    #[cfg(unix)]
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

/// Refuses an engine socket where Unix domain sockets are not used.
/// Takes the anchor, socket path and identity, all unused.
/// Returns `owner_anchor_unavailable` until Windows has owner-only directory checks.
#[cfg(not(unix))]
fn check_socket(_anchor: &Anchor, _name: &Path, _identity: Identity) -> Result<(), EngineError> {
    Err(fail("owner_anchor_unavailable"))
}

#[cfg(unix)]
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
    #[cfg(unix)]
    pub(in crate::engine) fn capture_native_profile_custody(
        &self,
    ) -> Result<NativeProfileCustody, EngineError> {
        self.verify()?;
        let metadata = self.anchor.metadata("engine.db")?;
        if Identity::of(&metadata) != self.pinned.value.database {
            return Err(fail("engine_database_replaced"));
        }
        let custody =
            NativeProfileCustody::capture(&self.anchor.path.join("engine.db"), &metadata)?;
        self.verify()?;
        Ok(custody)
    }

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
        engine_version: binding.pinned.value.engine_version.clone(),
    };
    Ok((client, identity, binding))
}

pub(super) fn runner_binding(
    input: &Path,
) -> Result<(PathBuf, OwnerConnectionIdentity, NativeBinding), EngineError> {
    let anchor = Anchor::open(input)?;
    let pinned = PinnedLocator::read(&anchor)?.ok_or_else(|| fail("owner_locator_missing"))?;
    let binding = NativeBinding { anchor, pinned };
    binding.verify()?;
    let generation = binding.anchor.path.join(&binding.pinned.value.generation);
    let identity = OwnerConnectionIdentity {
        database_instance: binding.pinned.value.db_instance.clone(),
        engine_epoch: binding.pinned.value.engine_epoch.clone(),
        engine_version: binding.pinned.value.engine_version.clone(),
    };
    Ok((generation, identity, binding))
}

/// Reads the version a running engine published in its locator, for the version message
/// only; nothing else is trusted from it, so it reads any future locator shape. Input:
/// the engine's data root. Output: the version, or none without a readable one.
#[cfg(unix)]
pub fn reported_engine_version(root: &Path) -> Option<String> {
    let mut bytes = Vec::new();
    File::open(root.join(LOCATOR))
        .ok()?
        .take(LOCATOR_MAX as u64 + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() > LOCATOR_MAX {
        return None;
    }
    let value: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    let version = value.get("engine_version")?.as_str()?;
    (!version.is_empty() && engine_version_valid(version)).then(|| version.to_owned())
}

/// Reports no engine version where no engine locator can exist yet. Input: the root,
/// unused. Output: none.
#[cfg(not(unix))]
pub fn reported_engine_version(_root: &Path) -> Option<String> {
    None
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
    #[cfg(unix)]
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

/// Refuses, before anything is created, a socket path that has no socket address even
/// through its short address. Input: the socket path. Output: Ok, or
/// `owner_socket_path_too_long`.
#[cfg(unix)]
fn check_path_length(path: &Path) -> Result<(), EngineError> {
    if !crate::socket_address::fits(path) {
        return Err(fail("owner_socket_path_too_long"));
    }
    Ok(())
}

/// Accepts every path where the engine binds no Unix socket. Input: the path, unused.
/// Output: Ok.
#[cfg(not(unix))]
fn check_path_length(_path: &Path) -> Result<(), EngineError> {
    Ok(())
}

impl EngineService {
    pub fn register_native_auth_host(&mut self, executable: &Path) -> Result<(), EngineError> {
        if self.closed {
            return Err(fail("engine_service_closed"));
        }
        self.live()?;
        let host = crate::engine::native_auth_host::RegisteredNativeAuthHost::capture(executable)?;
        self.owner_runtime
            .as_ref()
            .ok_or_else(|| fail("owner_runtime_missing"))?
            .register_native_auth_host(host)
    }

    pub fn register_codex_executable(&mut self, executable: &Path) -> Result<(), EngineError> {
        if self.closed {
            return Err(fail("engine_service_closed"));
        }
        self.live()?;
        let host = crate::engine::native_auth_host::RegisteredNativeAuthHost::capture(executable)?;
        self.owner_runtime
            .as_ref()
            .ok_or_else(|| fail("owner_runtime_missing"))?
            .register_codex_executable(host)
    }

    pub fn register_claude_executable(&mut self, executable: &Path) -> Result<(), EngineError> {
        if self.closed {
            return Err(fail("engine_service_closed"));
        }
        self.live()?;
        let host = crate::engine::native_auth_host::RegisteredNativeAuthHost::capture(executable)?;
        self.owner_runtime
            .as_ref()
            .ok_or_else(|| fail("owner_runtime_missing"))?
            .register_claude_executable(host)
    }

    /// Registers the native RCLI host for master turns and API-key writes.
    /// Input: absolute path to the bundled `crazyagent-rcli` executable.
    /// Output: Ok once both identities are captured before the runtime freezes.
    pub fn register_rcli_executable(&mut self, executable: &Path) -> Result<(), EngineError> {
        if self.closed {
            return Err(fail("engine_service_closed"));
        }
        self.live()?;
        let runtime = self
            .owner_runtime
            .as_ref()
            .ok_or_else(|| fail("owner_runtime_missing"))?;
        let master =
            crate::engine::native_auth_host::RegisteredNativeAuthHost::capture(executable)?;
        let writer =
            crate::engine::native_auth_host::RegisteredNativeAuthHost::capture(executable)?;
        runtime.register_rcli_master_executable(master)?;
        runtime.register_rcli_private_key_executable(writer)
    }

    pub async fn prepare_runner(
        &mut self,
        attempt: &crate::worker_dispatch::AttemptRef,
    ) -> Result<crate::worker_dispatch::PreparedRunner, EngineError> {
        self.runner_host_ready()?;
        if attempt.reservation_cursor.value() == 0 {
            return Err(fail("runner_attempt_cursor_zero"));
        }
        let prepared = self
            .owner_runtime
            .as_ref()
            .ok_or_else(|| fail("owner_runtime_missing"))?
            .prepare_runner(&self.anchor.path, attempt)
            .await?;
        self.runner_host_ready()?;
        Ok(prepared)
    }

    pub async fn attach_runner(
        &mut self,
        prepared: &crate::worker_dispatch::PreparedRunner,
    ) -> Result<crate::worker_dispatch::RunnerReady, EngineError> {
        self.runner_host_ready()?;
        if prepared.root().parent() != Some(self.anchor.path.as_path()) {
            return Err(fail("runner_registration_root"));
        }
        let ready = self
            .owner_runtime
            .as_ref()
            .ok_or_else(|| fail("owner_runtime_missing"))?
            .attach_runner(prepared)
            .await?;
        self.runner_host_ready()?;
        Ok(ready)
    }

    fn runner_host_ready(&mut self) -> Result<(), EngineError> {
        if self.closed {
            return Err(fail("engine_service_closed"));
        }
        self.live()?;
        let locator = self
            .locator
            .as_ref()
            .ok_or_else(|| fail("owner_locator_missing"))?;
        locator.verify(&self.anchor)?;
        check_database(&self.anchor, locator.value.database)?;
        check_generation(&self.anchor, &locator.value)
    }

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
            #[cfg(unix)]
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

    /// Refuses to start the owner and ordinary listeners without Unix domain sockets.
    /// Takes the socket name, settings, previous locator and shutdown signal, all unused.
    /// Returns `owner_anchor_unavailable` until Windows has owner-only directory checks.
    #[cfg(not(unix))]
    async fn start_inner(
        &mut self,
        _ordinary: &str,
        _config: &EffectiveConfig,
        _previous: Option<&PinnedLocator>,
        _shutdown: &watch::Receiver<bool>,
    ) -> Result<(), EngineError> {
        Err(fail("owner_anchor_unavailable"))
    }

    #[cfg(unix)]
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
            crate::spawn_gate::connect_unix(self.anchor.path.join(ordinary)),
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
            engine_version: crate::version::VERSION.into(),
        };
        self.publish(value, previous)?;
        self.sweep_generations();
        cancelled(shutdown)?;
        Ok(())
    }

    /// Removes the owner generation folders an earlier engine run left in the anchor after
    /// a crash or a failed shutdown, once the locator names this run's own generation; the
    /// engine database lock makes this the only run on the anchor. Input: none. Output:
    /// none; a folder that is not a private directory or cannot be removed is logged.
    #[cfg(unix)]
    fn sweep_generations(&self) {
        let Some((current, _)) = &self.generation else {
            return;
        };
        let Ok(entries) = std::fs::read_dir(&self.anchor.path) else {
            eprintln!("owner_generation_sweep_unavailable");
            return;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let generation = name.len() == 9
                && name.starts_with('o')
                && name[1..]
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
            if !generation || &name == current {
                continue;
            }
            let private = self
                .anchor
                .metadata(&name)
                .is_ok_and(|metadata| safe_directory(&metadata));
            if !private || std::fs::remove_dir_all(self.anchor.path.join(&name)).is_err() {
                eprintln!("owner_generation_sweep_kept");
            }
        }
    }

    #[cfg(unix)]
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

    #[cfg(unix)]
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
        match tokio::time::timeout(self.timeout, crate::spawn_gate::connect_unix(&path)).await {
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

    #[cfg(unix)]
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
        let reproof = self
            .owner_runtime
            .as_ref()
            .map(OwnerRuntimeHandle::reprove_in_background);
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
        if let Some(reproof) = reproof {
            reproof.abort();
            let _ = reproof.await;
        }
        combine(result, self.shutdown().await)
    }

    pub async fn shutdown(&mut self) -> Result<(), EngineError> {
        assert!(!self.closed);
        let mut errors = Vec::new();
        if let Some(owner) = self.owner_runtime.as_ref() {
            if let Err(error) = owner.invalidate_native_auth_dispatch() {
                errors.push(error.0);
            }
            if let Err(error) = owner.drain_native_auth_dispatch().await {
                errors.push(error.0);
            }
        }
        if let Some(locator) = self.locator.take() {
            let result = locator
                .verify(&self.anchor)
                .and_then(|()| remove_locator(&self.anchor));
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
                let path = self.anchor.path.join(&generation);
                for entry in std::fs::read_dir(&path).map_err(io_error)? {
                    let entry = entry.map_err(io_error)?;
                    if entry
                        .file_name()
                        .to_string_lossy()
                        .starts_with("master-runtime-")
                    {
                        std::fs::remove_dir_all(entry.path()).map_err(io_error)?;
                    }
                }
                std::fs::remove_dir(path).map_err(io_error)
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

/// Removes the published owner locator relative to the pinned engine root.
/// Takes the anchor. Returns once the locator is unlinked, or the I/O error.
#[cfg(unix)]
fn remove_locator(anchor: &Anchor) -> Result<(), EngineError> {
    rustix::fs::unlinkat(&anchor.directory, LOCATOR, rustix::fs::AtFlags::empty()).map_err(io_error)
}

/// Refuses to remove the owner locator without directory-relative Unix unlinks.
/// Takes the anchor, unused. Returns `owner_anchor_unavailable`.
#[cfg(not(unix))]
fn remove_locator(_anchor: &Anchor) -> Result<(), EngineError> {
    Err(fail("owner_anchor_unavailable"))
}

impl Drop for EngineService {
    /// Stops native sign-in dispatch and reaps its custodian child when the service is
    /// dropped without a completed shutdown, so no custodian outlives its engine.
    /// Inputs: the service being dropped. Output: none; an unconfirmed cleanup is logged.
    fn drop(&mut self) {
        if let Some(owner) = self.owner_runtime.as_ref()
            && let Err(error) = owner.invalidate_native_auth_dispatch()
        {
            eprintln!(
                "native_auth_dispatch_drop_cleanup_uncertain cause={}",
                error.0
            );
        }
    }
}

#[cfg(all(test, unix))]
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
            engine_version: crate::version::VERSION.into(),
        }
    }

    #[test]
    fn locator_carries_a_bounded_engine_version_and_tolerates_older_engines() {
        let parsed = parse_locator(&serde_json::to_vec(&sample()).unwrap()).unwrap();
        assert_eq!(parsed.engine_version, crate::version::VERSION);
        let mut older = serde_json::to_value(sample()).unwrap();
        older.as_object_mut().unwrap().remove("engine_version");
        let parsed = parse_locator(&serde_json::to_vec(&older).unwrap()).unwrap();
        assert_eq!(parsed.engine_version, "");
        for bad in ["0.2.0\n".to_owned(), "9".repeat(65)] {
            let mut value = sample();
            value.engine_version = bad;
            assert_eq!(
                parse_locator(&serde_json::to_vec(&value).unwrap())
                    .unwrap_err()
                    .0,
                "owner_locator_invalid"
            );
        }
    }

    #[test]
    fn reported_engine_version_reads_any_locator_shape_for_the_message_only() {
        let root = tempfile::tempdir().unwrap();
        assert_eq!(reported_engine_version(root.path()), None);
        let mut future = serde_json::to_value(sample()).unwrap();
        future["engine_version"] = "9.0.0".into();
        future["added_later"] = true.into();
        std::fs::write(
            root.path().join(LOCATOR),
            serde_json::to_vec(&future).unwrap(),
        )
        .unwrap();
        assert_eq!(
            reported_engine_version(root.path()).as_deref(),
            Some("9.0.0")
        );
        future["engine_version"] = "9.0.0\u{1b}[2J".into();
        std::fs::write(
            root.path().join(LOCATOR),
            serde_json::to_vec(&future).unwrap(),
        )
        .unwrap();
        assert_eq!(reported_engine_version(root.path()), None);
        std::fs::write(root.path().join(LOCATOR), vec![b' '; LOCATOR_MAX + 1]).unwrap();
        assert_eq!(reported_engine_version(root.path()), None);
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
