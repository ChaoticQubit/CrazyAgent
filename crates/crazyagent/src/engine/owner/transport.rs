use super::*;
use crate::coordination::{Role, WorkerBinding};
use crate::engine::{Engine, io_error, read_frame, verify_peer};
use crate::storage::Job;
use crate::worker_dispatch::{
    AttemptRef, PreparedRunner, RUNNER_BOOTSTRAP_MAX, RUNNERS_MAX, RunnerBootIdentity,
    RunnerBootstrap, RunnerReady, RunnerReply, RunnerRequest,
};
use rustix::fs::{Mode, OFlags};
use std::fs::File;
use std::io::Read;
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
        self.socket_identity_at(SOCKET)
    }

    fn socket_identity_at(&self, name: &str) -> Result<Identity, EngineError> {
        self.verify()?;
        let m = std::fs::symlink_metadata(self.path.join(name)).map_err(io_error)?;
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
            || m.uid() != rustix::process::getuid().as_raw()
            || m.mode() & 0o7777 != 0o600
            || m.nlink() != 1
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

struct PinnedBootstrap {
    file: File,
    identity: Identity,
    bytes: Vec<u8>,
    value: RunnerBootstrap,
}

impl PinnedBootstrap {
    fn read(root: &Root) -> Result<Self, EngineError> {
        let file = root.open_key("engine-bootstrap", false)?;
        let mut bytes = Vec::new();
        (&file)
            .take((RUNNER_BOOTSTRAP_MAX + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(io_error)?;
        if bytes.is_empty() || bytes.len() > RUNNER_BOOTSTRAP_MAX {
            return Err(failure("runner_bootstrap_bounds"));
        }
        let value: RunnerBootstrap =
            serde_json::from_slice(&bytes).map_err(|_| failure("runner_bootstrap_invalid"))?;
        value.validate()?;
        let pinned = Self {
            identity: Identity::of(&file.metadata().map_err(io_error)?),
            file,
            bytes,
            value,
        };
        pinned.verify(root)?;
        Ok(pinned)
    }

    fn create(root: &Root, value: RunnerBootstrap) -> Result<Self, EngineError> {
        value.validate()?;
        let bytes = serde_json::to_vec(&value).map_err(io_error)?;
        if bytes.is_empty() || bytes.len() > RUNNER_BOOTSTRAP_MAX {
            return Err(failure("runner_bootstrap_bounds"));
        }
        let file = root.open_key("engine-bootstrap", true)?;
        file.write_all_at(&bytes, 0).map_err(io_error)?;
        file.sync_all().map_err(io_error)?;
        let pinned = Self {
            identity: Identity::of(&file.metadata().map_err(io_error)?),
            file,
            bytes,
            value,
        };
        pinned.verify(root)?;
        root.directory.sync_all().map_err(io_error)?;
        Ok(pinned)
    }

    fn verify(&self, root: &Root) -> Result<(), EngineError> {
        root.verify()?;
        let actual = self.file.metadata().map_err(io_error)?;
        let named = rustix::fs::statat(
            &root.directory,
            "engine-bootstrap",
            rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
        )
        .map_err(io_error)?;
        if !actual.is_file()
            || actual.uid() != rustix::process::getuid().as_raw()
            || actual.mode() & 0o7777 != 0o600
            || actual.nlink() != 1
            || actual.len() != self.bytes.len() as u64
            || Identity::of(&actual) != self.identity
            || named.st_ino != actual.ino()
            || named.st_dev as u64 != actual.dev()
        {
            return Err(failure("runner_bootstrap_replaced_or_unsafe"));
        }
        let mut bytes = vec![0; self.bytes.len()];
        self.file.read_exact_at(&mut bytes, 0).map_err(io_error)?;
        if bytes != self.bytes {
            return Err(failure("runner_bootstrap_content_changed"));
        }
        Ok(())
    }
}

struct RunnerKey {
    file: File,
    identity: Identity,
    token: [u8; 32],
}

struct RunnerSlot {
    prepared: PreparedRunner,
    root: Root,
    bootstrap: PinnedBootstrap,
    binding: WorkerBinding,
    key: Option<RunnerKey>,
    boot: Option<RunnerBootIdentity>,
    endpoint: Option<Identity>,
    active: bool,
    ready: Option<RunnerReady>,
}

impl RunnerSlot {
    fn assert_consistent(&self) {
        assert_eq!(self.prepared.root, self.root.path);
        assert_eq!(
            self.prepared.registration_id,
            self.bootstrap.value.registration_id
        );
        assert_eq!(self.prepared.attempt, self.bootstrap.value.attempt);
        assert_eq!(self.prepared.slot, self.bootstrap.value.slot);
        assert_eq!(self.binding.plan_id, self.prepared.attempt.plan_id);
        assert_eq!(self.binding.task_id, self.prepared.attempt.item_id);
        assert_eq!(
            self.binding.attempt_cursor,
            self.prepared.attempt.reservation_cursor
        );
        if self.active {
            assert!(self.key.is_some() && self.boot.is_some() && self.endpoint.is_some());
        }
        if let Some(ready) = &self.ready {
            assert!(self.active);
            assert_eq!(ready.registration_id, self.prepared.registration_id);
            assert_eq!(ready.attempt, self.prepared.attempt);
            assert_eq!(Some(&ready.boot), self.boot.as_ref());
        }
    }

    fn verify(&self, generation: &Root) -> Result<(), EngineError> {
        self.assert_consistent();
        self.bootstrap.verify(&self.root)?;
        if let Some(key) = &self.key {
            generation.verify_key(
                &format!("r{:02x}.cap", self.prepared.slot),
                &key.file,
                key.identity,
            )?;
            let mut observed = [0; 32];
            key.file.read_exact_at(&mut observed, 0).map_err(io_error)?;
            if !bool::from(key.token.ct_eq(&observed)) {
                return Err(failure("runner_capability_content_changed"));
            }
        }
        if let Some(endpoint) = self.endpoint
            && self.root.socket_identity_at("control.sock")? != endpoint
        {
            return Err(failure("runner_control_socket_replaced"));
        }
        Ok(())
    }
}

struct Authority {
    root: Root,
    socket: Identity,
    host: HostStamp,
    slots: Mutex<[Slot; 2]>,
    runners: Mutex<Vec<Option<RunnerSlot>>>,
    usable: std::sync::atomic::AtomicBool,
}

impl Authority {
    fn prepare_runner(
        &self,
        anchor: &Path,
        attempt: &AttemptRef,
        binding: WorkerBinding,
    ) -> Result<PreparedRunner, EngineError> {
        if !self.usable.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(failure("runner_runtime_unavailable"));
        }
        let parent = Root::open(anchor)?;
        self.root.verify()?;
        if self.root.path.parent() != Some(parent.path.as_path()) {
            return Err(failure("runner_registration_root"));
        }
        let mut runners = self
            .runners
            .lock()
            .map_err(|_| failure("runner_authority_unavailable"))?;
        assert!(runners.len() <= RUNNERS_MAX);
        for slot in runners.iter().flatten() {
            if &slot.prepared.attempt == attempt {
                slot.verify(&self.root)?;
                if slot.binding != binding {
                    return Err(failure("runner_attempt_changed"));
                }
                return Ok(slot.prepared.clone());
            }
            if slot.prepared.attempt.plan_id == attempt.plan_id
                && slot.prepared.attempt.item_id == attempt.item_id
            {
                return Err(failure("runner_attempt_already_registered"));
            }
        }
        if runners.len() == RUNNERS_MAX {
            return Err(failure("runner_registration_capacity"));
        }
        let index = runners.len();
        let name = format!("r{index:02x}");
        let path = parent.path.join(&name);
        if path.as_os_str().len() > 80 || path.components().count() > 32 {
            return Err(failure("runner_root_path_bounds"));
        }
        let registration_id =
            RequestId::new(format!("runner-{}", random_id()?)).map_err(io_error)?;
        runners.push(None);
        rustix::fs::mkdirat(&parent.directory, name.as_str(), Mode::RWXU).map_err(io_error)?;
        parent.verify()?;
        let root = Root::open(&path)?;
        let prepared = PreparedRunner {
            root: path,
            registration_id,
            attempt: attempt.clone(),
            slot: index as u8,
        };
        let bootstrap = PinnedBootstrap::create(
            &root,
            RunnerBootstrap {
                version: 1,
                registration_id: prepared.registration_id.clone(),
                attempt: prepared.attempt.clone(),
                slot: prepared.slot,
            },
        )?;
        let slot = RunnerSlot {
            prepared: prepared.clone(),
            root,
            bootstrap,
            binding,
            key: None,
            boot: None,
            endpoint: None,
            active: false,
            ready: None,
        };
        slot.verify(&self.root)?;
        parent.directory.sync_all().map_err(io_error)?;
        assert!(runners[index].is_none());
        runners[index] = Some(slot);
        assert!(runners.len() <= RUNNERS_MAX);
        Ok(prepared)
    }

    fn runner_prepared(&self, prepared: &PreparedRunner) -> Result<(), EngineError> {
        if !self.usable.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(failure("runner_runtime_unavailable"));
        }
        let runners = self
            .runners
            .lock()
            .map_err(|_| failure("runner_authority_unavailable"))?;
        let slot = runners
            .get(usize::from(prepared.slot))
            .and_then(Option::as_ref)
            .ok_or_else(|| failure("runner_registration_missing"))?;
        if slot.prepared != *prepared {
            return Err(failure("runner_prepared_identity_changed"));
        }
        slot.verify(&self.root)
    }

    fn observe_runner(
        &self,
        prepared: &PreparedRunner,
        boot: RunnerBootIdentity,
        endpoint: Identity,
        binding: &WorkerBinding,
    ) -> Result<(), EngineError> {
        self.runner_prepared(prepared)?;
        boot.validate()?;
        let mut runners = self
            .runners
            .lock()
            .map_err(|_| failure("runner_authority_unavailable"))?;
        let slot = runners[usize::from(prepared.slot)].as_mut().unwrap();
        if slot.binding != *binding {
            return Err(failure("runner_attempt_changed"));
        }
        if slot.boot.as_ref().is_some_and(|original| original != &boot) {
            return Err(failure("runner_boot_replacement_requires_reconciliation"));
        }
        if slot.key.is_some() && !slot.active {
            return Err(failure("runner_registration_unavailable"));
        }
        if slot.root.socket_identity_at("control.sock")? != endpoint {
            return Err(failure("runner_control_socket_replaced"));
        }
        if slot.key.is_none() {
            let mut token = [0; 32];
            getrandom::fill(&mut token).map_err(|_| failure("owner_entropy_unavailable"))?;
            let file = self
                .root
                .open_key(&format!("r{:02x}.cap", prepared.slot), true)?;
            slot.key = Some(RunnerKey {
                identity: Identity::of(&file.metadata().map_err(io_error)?),
                file,
                token,
            });
            let key = slot.key.as_ref().unwrap();
            key.file.write_all_at(&key.token, 0).map_err(io_error)?;
            key.file.sync_all().map_err(io_error)?;
            self.root.directory.sync_all().map_err(io_error)?;
        }
        slot.boot = Some(boot);
        slot.endpoint = Some(endpoint);
        slot.verify(&self.root)?;
        slot.active = true;
        slot.assert_consistent();
        Ok(())
    }

    fn retire_runner(&self, prepared: &PreparedRunner) -> Result<(), EngineError> {
        let mut runners = self
            .runners
            .lock()
            .map_err(|_| failure("runner_authority_unavailable"))?;
        let slot = runners
            .get_mut(usize::from(prepared.slot))
            .and_then(Option::as_mut)
            .ok_or_else(|| failure("runner_registration_missing"))?;
        if slot.prepared != *prepared {
            return Err(failure("runner_prepared_identity_changed"));
        }
        slot.active = false;
        slot.ready = None;
        slot.assert_consistent();
        Ok(())
    }

    fn authenticate_runner(&self, packet: &[u8; 37]) -> Result<PreparedRunner, EngineError> {
        if !self.usable.load(std::sync::atomic::Ordering::SeqCst) || &packet[..4] != b"CAR1" {
            return Err(failure("runner_authentication_failed"));
        }
        let runners = self
            .runners
            .lock()
            .map_err(|_| failure("runner_authority_unavailable"))?;
        let slot = runners
            .get(usize::from(packet[4]))
            .and_then(Option::as_ref)
            .ok_or_else(|| failure("runner_authentication_failed"))?;
        slot.verify(&self.root)?;
        let key = slot
            .key
            .as_ref()
            .ok_or_else(|| failure("runner_authentication_failed"))?;
        if !slot.active || !bool::from(key.token.ct_eq(&packet[5..])) {
            return Err(failure("runner_authentication_failed"));
        }
        if self.root.socket_identity()? != self.socket {
            return Err(failure("owner_socket_replaced"));
        }
        Ok(slot.prepared.clone())
    }

    fn ready_runner(
        &self,
        packet: &[u8; 37],
        request: RunnerRequest,
        binding: &WorkerBinding,
    ) -> Result<RunnerReady, EngineError> {
        let prepared = self.authenticate_runner(packet)?;
        let RunnerRequest::Ready {
            protocol_version,
            registration_id,
            boot,
        } = request;
        boot.validate()?;
        let mut runners = self
            .runners
            .lock()
            .map_err(|_| failure("runner_authority_unavailable"))?;
        let slot = runners[usize::from(prepared.slot)].as_mut().unwrap();
        if protocol_version != PROTOCOL_VERSION
            || registration_id != prepared.registration_id
            || slot.boot.as_ref() != Some(&boot)
            || slot.binding != *binding
        {
            return Err(failure("runner_ready_identity_mismatch"));
        }
        let ready = RunnerReady {
            registration_id,
            attempt: prepared.attempt,
            boot,
            database_instance: self.host.db_instance.clone(),
            engine_epoch: self.host.engine_epoch.clone(),
        };
        if slot
            .ready
            .as_ref()
            .is_some_and(|original| original != &ready)
        {
            return Err(failure("runner_ready_identity_mismatch"));
        }
        slot.ready = Some(ready.clone());
        slot.assert_consistent();
        Ok(ready)
    }

    fn runner_ready(
        &self,
        prepared: &PreparedRunner,
        ready: &RunnerReady,
    ) -> Result<(), EngineError> {
        self.runner_prepared(prepared)?;
        let runners = self
            .runners
            .lock()
            .map_err(|_| failure("runner_authority_unavailable"))?;
        let slot = runners[usize::from(prepared.slot)].as_ref().unwrap();
        if !slot.active || slot.ready.as_ref() != Some(ready) {
            return Err(failure("runner_ready_unconfirmed"));
        }
        Ok(())
    }

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
        let runners = self
            .runners
            .lock()
            .map_err(|_| failure("runner_authority_unavailable"))?;
        for slot in runners.iter().flatten() {
            if let Some(key) = &slot.key {
                self.root.unlink_owned(
                    &format!("r{:02x}.cap", slot.prepared.slot),
                    key.identity,
                    false,
                )?;
            }
        }
        Ok(())
    }
}

pub struct OwnerRuntimeHandle {
    engine: Arc<Engine>,
    authority: Arc<Authority>,
}

impl OwnerRuntimeHandle {
    pub(crate) async fn prepare_runner(
        &self,
        anchor: &Path,
        attempt: &AttemptRef,
    ) -> Result<PreparedRunner, EngineError> {
        let _barrier = tokio::time::timeout(self.engine.timeout, self.engine.owner_egress.write())
            .await
            .map_err(|_| failure("runner_prepare_not_submitted"))?;
        let binding = self.engine.runner_attempt(attempt).await?;
        self.authority.prepare_runner(anchor, attempt, binding)
    }

    pub(crate) async fn attach_runner(
        &self,
        prepared: &PreparedRunner,
    ) -> Result<RunnerReady, EngineError> {
        self.authority.runner_prepared(prepared)?;
        let root = Root::open(prepared.root())?;
        let endpoint = root.socket_identity_at("control.sock")?;
        let timeout = self.engine.timeout.min(Duration::from_secs(1));
        let status = runner_control(
            &root,
            endpoint,
            serde_json::json!({
                "protocol_version": PROTOCOL_VERSION,
                "command": { "kind": "status" },
            }),
            timeout,
        )
        .await?;
        if status["kind"] != "status" {
            return Err(failure("runner_status_invalid"));
        }
        let boot: RunnerBootIdentity = serde_json::from_value(status["identity"].clone())
            .map_err(|_| failure("runner_status_invalid"))?;
        boot.validate()?;
        {
            let _barrier =
                tokio::time::timeout(self.engine.timeout, self.engine.owner_egress.write())
                    .await
                    .map_err(|_| failure("runner_attach_not_submitted"))?;
            let binding = self.engine.runner_attempt(prepared.attempt()).await?;
            self.authority
                .observe_runner(prepared, boot.clone(), endpoint, &binding)?;
        }
        let result = async {
            let reply = runner_control(
                &root,
                endpoint,
                serde_json::json!({
                    "protocol_version": PROTOCOL_VERSION,
                    "command": {
                        "kind": "attach_engine",
                        "registration_id": prepared.registration_id(),
                        "boot": boot,
                    },
                }),
                timeout,
            )
            .await?;
            if reply["kind"] != "engine_ready" {
                return Err(failure("runner_ready_unconfirmed"));
            }
            let ready: RunnerReady = serde_json::from_value(reply["identity"].clone())
                .map_err(|_| failure("runner_ready_invalid"))?;
            let _barrier =
                tokio::time::timeout(self.engine.timeout, self.engine.owner_egress.write())
                    .await
                    .map_err(|_| failure("runner_attach_outcome_unknown"))?;
            self.engine.runner_attempt(prepared.attempt()).await?;
            self.authority.runner_ready(prepared, &ready)?;
            Ok(ready)
        }
        .await;
        if result.is_err() {
            self.authority.retire_runner(prepared)?;
        }
        result
    }

    pub(crate) fn identity(&self) -> HostStamp {
        self.authority.host.clone()
    }

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
        let rollback = Root {
            path: root.path.clone(),
            directory: root.directory.try_clone().map_err(io_error)?,
            identity: root.identity,
        };
        let _barrier = tokio::time::timeout(engine.timeout, engine.owner_egress.write())
            .await
            .map_err(|_| failure("owner_start_not_submitted"))?;
        let started = engine.owner_job(Job::OwnerStart).await?;
        let host = started.host.clone();
        let mut created_keys = Vec::new();
        let mut created_socket = None;
        let result = (|| {
            let mut slots = Vec::new();
            for (i, registration) in started.owners.into_iter().enumerate() {
                let file = root.open_key(KEYS[i], true)?;
                created_keys.push((KEYS[i], Identity::of(&file.metadata().map_err(io_error)?)));
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
            created_socket = Some(Identity::of(&created));
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
                runners: Mutex::new(Vec::new()),
                usable: std::sync::atomic::AtomicBool::new(true),
            });
            Ok::<_, EngineError>((listener, authority))
        })();
        let (listener, authority) = match result {
            Ok(v) => v,
            Err(e) => {
                let mut errors = vec![e.0];
                if let Err(error) = engine.owner_job(|reply| Job::OwnerStop(host, reply)).await {
                    errors.push(error.0);
                }
                if let Some(identity) = created_socket
                    && let Err(error) = rollback.unlink_owned(SOCKET, identity, true)
                {
                    errors.push(error.0);
                }
                for (name, identity) in created_keys {
                    if let Err(error) = rollback.unlink_owned(name, identity, false) {
                        errors.push(error.0);
                    }
                }
                return Err(failure(&errors.join("; ")));
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
    async fn runner_read(&self, command: Command) -> Result<Outcome, EngineError> {
        if !matches!(
            &command,
            Command::CoordinationState { .. } | Command::TaskContext { .. }
        ) {
            return Err(failure("runner_read_kind"));
        }
        let request_id = RequestId::new(crate::memory::hash(
            &serde_json::to_vec(&command).map_err(io_error)?,
        ))
        .map_err(io_error)?;
        let request = WireRequest {
            protocol_version: PROTOCOL_VERSION,
            client_id: ClientId::new("trusted-runner-host").map_err(io_error)?,
            request_id,
            expected_revision: Cursor::new(0),
            command,
        };
        Ok(self
            .owner_job(|reply| Job::Apply(request, reply))
            .await?
            .outcome)
    }

    async fn runner_attempt(&self, attempt: &AttemptRef) -> Result<WorkerBinding, EngineError> {
        if attempt.reservation_cursor.value() == 0 {
            return Err(failure("runner_attempt_cursor_zero"));
        }
        let Outcome::Coordination { state } = self
            .runner_read(Command::CoordinationState {
                plan_id: attempt.plan_id.clone(),
            })
            .await?
        else {
            return Err(failure("runner_plan_missing"));
        };
        if state.plan.id != attempt.plan_id || !state.approved || state.revoked {
            return Err(failure("runner_plan_unavailable"));
        }
        let item = state
            .plan
            .tasks
            .iter()
            .find(|item| item.id == attempt.item_id)
            .ok_or_else(|| failure("runner_work_item_missing"))?;
        let reservation = state
            .reservations
            .get(&attempt.item_id)
            .ok_or_else(|| failure("runner_reservation_missing"))?;
        let binding = reservation
            .binding
            .as_ref()
            .ok_or_else(|| failure("runner_worker_binding_missing"))?;
        if item.role != Role::Worker
            || reservation.cursor != attempt.reservation_cursor
            || binding.plan_id != attempt.plan_id
            || binding.task_id != attempt.item_id
            || binding.attempt_cursor != attempt.reservation_cursor
            || binding.session_id != item.session_id
            || binding.config_id != state.plan.scope.config_id
        {
            return Err(failure("runner_attempt_mismatch"));
        }
        let Outcome::Task { context } = self
            .runner_read(Command::TaskContext {
                task_id: state.plan.scope.task_id.clone(),
            })
            .await?
        else {
            return Err(failure("runner_context_missing"));
        };
        if context != state.plan.scope {
            return Err(failure("runner_context_changed"));
        }
        Ok(binding.clone())
    }

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
        if &auth[..4] == b"CAR1" {
            let prepared = authority.authenticate_runner(&auth)?;
            let frame = read_frame(&mut stream).await?;
            let request: RunnerRequest = serde_json::from_slice(&frame[4..])
                .map_err(|_| failure("runner_request_invalid"))?;
            let _barrier = engine.owner_egress.write().await;
            let binding = engine.runner_attempt(prepared.attempt()).await?;
            let identity = authority.ready_runner(&auth, request, &binding)?;
            let frame = encode_frame(&RunnerReply::Ready { identity })
                .map_err(|_| failure("runner_response_bounds"))?;
            return stream.write_all(&frame).await.map_err(io_error);
        }
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

async fn runner_control(
    root: &Root,
    endpoint: Identity,
    request: serde_json::Value,
    timeout: Duration,
) -> Result<serde_json::Value, EngineError> {
    tokio::time::timeout(timeout, async {
        if root.socket_identity_at("control.sock")? != endpoint {
            return Err(failure("runner_control_socket_replaced"));
        }
        let mut stream = UnixStream::connect(root.path.join("control.sock"))
            .await
            .map_err(io_error)?;
        verify_peer(&stream)?;
        if root.socket_identity_at("control.sock")? != endpoint {
            return Err(failure("runner_control_socket_replaced"));
        }
        let frame = encode_frame(&request).map_err(|_| failure("runner_control_bounds"))?;
        stream.write_all(&frame).await.map_err(io_error)?;
        let frame = read_frame(&mut stream).await?;
        let reply = serde_json::from_slice(&frame[4..])
            .map_err(|_| failure("runner_control_reply_invalid"))?;
        if root.socket_identity_at("control.sock")? != endpoint {
            return Err(failure("runner_control_socket_replaced"));
        }
        Ok(reply)
    })
    .await
    .map_err(|_| failure("runner_control_outcome_unknown"))?
}

pub fn runner_bootstrap(root: &Path) -> Result<Option<RequestId>, EngineError> {
    match std::fs::symlink_metadata(root.join("engine-bootstrap")) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(io_error(error)),
        Ok(_) => {}
    }
    let root = Root::open(root)?;
    let pinned = PinnedBootstrap::read(&root)?;
    if root.path.file_name() != Some(std::ffi::OsStr::new(&format!("r{:02x}", pinned.value.slot))) {
        return Err(failure("runner_bootstrap_root_mismatch"));
    }
    Ok(Some(pinned.value.registration_id))
}

pub async fn runner_ready(
    input: &Path,
    registration_id: &RequestId,
    boot: &RunnerBootIdentity,
    timeout: Duration,
) -> Result<RunnerReady, EngineError> {
    if timeout.is_zero() || timeout > Duration::from_secs(30) {
        return Err(failure("owner_timeout_invalid"));
    }
    boot.validate()?;
    tokio::time::timeout(timeout, async {
        let runtime = Root::open(input)?;
        let bootstrap = PinnedBootstrap::read(&runtime)?;
        if &bootstrap.value.registration_id != registration_id
            || runtime.path.file_name()
                != Some(std::ffi::OsStr::new(&format!(
                    "r{:02x}",
                    bootstrap.value.slot
                )))
        {
            return Err(failure("runner_bootstrap_identity_mismatch"));
        }
        let anchor = runtime
            .path
            .parent()
            .ok_or_else(|| failure("runner_registration_root"))?;
        let (path, engine, binding) = crate::engine::bootstrap::runner_binding(anchor)?;
        let generation = Root::open(&path)?;
        let name = format!("r{:02x}.cap", bootstrap.value.slot);
        let file = generation.open_key(&name, false)?;
        let identity = Identity::of(&file.metadata().map_err(io_error)?);
        generation.verify_key(&name, &file, identity)?;
        let mut token = [0; 32];
        file.read_exact_at(&mut token, 0).map_err(io_error)?;
        generation.verify_key(&name, &file, identity)?;
        let socket = generation.socket_identity()?;
        let mut stream = UnixStream::connect(generation.path.join(SOCKET))
            .await
            .map_err(io_error)?;
        verify_peer(&stream)?;
        if generation.socket_identity()? != socket {
            return Err(failure("owner_socket_replaced"));
        }
        bootstrap.verify(&runtime)?;
        binding.verify()?;
        let mut packet = [0; 37];
        packet[..4].copy_from_slice(b"CAR1");
        packet[4] = bootstrap.value.slot;
        packet[5..].copy_from_slice(&token);
        stream.write_all(&packet).await.map_err(io_error)?;
        let request = RunnerRequest::Ready {
            protocol_version: PROTOCOL_VERSION,
            registration_id: registration_id.clone(),
            boot: boot.clone(),
        };
        stream
            .write_all(&encode_frame(&request).map_err(|_| failure("runner_request_bounds"))?)
            .await
            .map_err(io_error)?;
        let frame = read_frame(&mut stream).await?;
        let RunnerReply::Ready { identity: ready } =
            serde_json::from_slice(&frame[4..]).map_err(|_| failure("runner_ready_invalid"))?;
        bootstrap.verify(&runtime)?;
        binding.verify()?;
        generation.verify_key(&name, &file, identity)?;
        let mut observed = [0; 32];
        file.read_exact_at(&mut observed, 0).map_err(io_error)?;
        if !bool::from(token.ct_eq(&observed)) || generation.socket_identity()? != socket {
            return Err(failure("runner_capability_changed"));
        }
        if ready.registration_id != *registration_id
            || ready.attempt != bootstrap.value.attempt
            || ready.boot != *boot
            || ready.database_instance != engine.database_instance()
            || ready.engine_epoch != engine.engine_epoch()
        {
            return Err(failure("runner_ready_identity_mismatch"));
        }
        Ok(ready)
    })
    .await
    .map_err(|_| failure("runner_ready_outcome_unknown"))?
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

    #[test]
    fn cleanup_preserves_hardlinked_capability() {
        let directory = tempfile::tempdir_in("/private/tmp").unwrap();
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let root = Root::open(directory.path()).unwrap();
        let file = root.open_key(KEYS[0], true).unwrap();
        file.write_all_at(&[1; 32], 0).unwrap();
        let identity = Identity::of(&file.metadata().unwrap());
        std::fs::hard_link(
            directory.path().join(KEYS[0]),
            directory.path().join("extra"),
        )
        .unwrap();
        assert_eq!(
            root.unlink_owned(KEYS[0], identity, false).unwrap_err().0,
            "owner_cleanup_replaced_artifact_preserved"
        );
        assert!(directory.path().join(KEYS[0]).exists());
        assert!(directory.path().join("extra").exists());
    }

    #[tokio::test]
    async fn partial_bind_failure_removes_created_keys_and_stops_owner() {
        let root = tempfile::tempdir_in("/private/tmp").unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let long = root.path().join("x".repeat(100));
        std::fs::create_dir(&long).unwrap();
        std::fs::set_permissions(&long, std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = long.join(SOCKET);
        let expected = UnixListener::bind(&path).err().unwrap().to_string();
        let config =
            crate::config::resolve_config(&root.path().join("g"), &root.path().join("p")).unwrap();
        let engine = Arc::new(
            Engine::open(&root.path().join("db"), config.clone())
                .await
                .unwrap(),
        );
        let error = OwnerServer::bind(&long, engine.clone(), &config)
            .await
            .err()
            .unwrap();
        assert_eq!(error.0, expected);
        assert_eq!(std::fs::read_dir(&long).unwrap().count(), 0);
        let (server, _) = OwnerServer::bind(root.path(), engine.clone(), &config)
            .await
            .unwrap();
        let (stop, stopped) = oneshot::channel();
        stop.send(()).unwrap();
        server.run(stopped).await.unwrap();
        engine.close().await.unwrap();
    }

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
