use super::held::{Folder, HeldError, Kind, Name, Pin};
use super::{Anchor, Identity, hex};
#[cfg(unix)]
use super::{safe_directory, safe_file};
#[cfg(unix)]
use crate::config::{ModelAuthentication, ModelConnection};
use crate::engine::EngineError;
use crate::engine::owner::{HostStamp, NativeProfileBinding, PreparedNativeProfile};
#[cfg(unix)]
use crate::file_lock::HeldLock;
#[cfg(windows)]
use crate::platform::FileExt;
use crate::protocol::CredentialRef;
#[cfg(unix)]
use crate::protocol::RequestId;
#[cfg(unix)]
use rustix::fs::{AtFlags, Dir, FileType, FlockOperation, Mode, OFlags, flock};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fs::{File, Metadata};
use std::io::{Read, Write};
#[cfg(unix)]
use std::os::unix::fs::{FileExt, MetadataExt};
use std::path::Path;

#[cfg(unix)]
mod agent_copy;
#[cfg(unix)]
pub(crate) use agent_copy::AgentCredential;
#[cfg(unix)]
mod configured_rcli;
#[cfg(unix)]
mod generated_configured_rcli;
#[cfg(unix)]
mod renewal;
#[cfg(unix)]
mod retire;
mod staging;
#[cfg(unix)]
pub(crate) use configured_rcli::ConfiguredRcliProfileMetadata;
#[cfg(unix)]
use configured_rcli::lock;
#[cfg(unix)]
use staging::STAGING;
use staging::{ROOT_STAGING, Staging, step};
/// The sign-in, configured-account and agent-copy parts on Windows: each refuses with
/// `native_profile_custody_unavailable` until its slice ports it (DESIGN-0515 D5).
#[cfg(windows)]
mod refused_windows;
#[cfg(windows)]
pub(crate) use refused_windows::{
    AgentCredential, ConfiguredRcliProfileMetadata, NativeProfileUse, generated_home_needs_key,
};

const ROOT: &str = super::NATIVE_PROFILES_FOLDER;
#[cfg(unix)]
pub(crate) use retire::{generated_home_needs_key, remove_generated_home, remove_sign_in};

const MANIFEST: &str = "identity.json";
const MAX_MANIFEST: usize = 4096;
#[cfg(unix)]
pub(crate) const MAX_PROFILES: usize = 1024;

fn failure(code: &str) -> EngineError {
    EngineError(code.to_owned())
}

fn unsafe_custody(_: impl std::fmt::Display) -> EngineError {
    failure("native_profile_custody_unsafe")
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

#[cfg(unix)]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StableBinding {
    profile_ref: CredentialRef,
    account_ref: CredentialRef,
    provider: CredentialRef,
    harness: CredentialRef,
    protocol: CredentialRef,
}

#[cfg(unix)]
impl StableBinding {
    fn of(connection: &ModelConnection) -> Result<Self, EngineError> {
        let ModelAuthentication::NativeManaged { profile_ref } = &connection.auth else {
            return Err(failure("native_profile_not_managed"));
        };
        Ok(Self {
            profile_ref: profile_ref.clone(),
            account_ref: connection.account_ref.clone(),
            provider: connection.provider.clone(),
            harness: connection.harness.clone(),
            protocol: connection.protocol.clone(),
        })
    }

    fn key(&self) -> String {
        digest(self.profile_ref.as_str().as_bytes())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RootIdentity {
    version: u8,
    database_instance: String,
    state: Identity,
    database: Identity,
    directory: Identity,
    manifest: Identity,
}

impl RootIdentity {
    /// Tells whether a root manifest read from disk still describes the live root: the
    /// same version and database instance, and the same files (see `same_files`).
    /// Inputs: the pinned manifest and the live identity. Output: true when they match.
    fn matches(&self, live: &RootIdentity) -> bool {
        self.version == live.version
            && self.database_instance == live.database_instance
            && same_files(&self.files(), &live.files())
    }

    fn files(&self) -> [Identity; 4] {
        [self.state, self.database, self.directory, self.manifest]
    }
}

/// Applies the crate-wide whole-volume renumber rule (`file_identity::same_files`) to
/// custody identities. Inputs: pinned and live identities, in the same order. Output: true
/// when they name the same files.
fn same_files<const N: usize>(pinned: &[Identity; N], live: &[Identity; N]) -> bool {
    let pair = |identity: Identity| (identity.device, identity.inode);
    crate::file_identity::same_files(&pinned.map(pair), &live.map(pair))
}

#[cfg(unix)]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProfileIdentity {
    version: u8,
    root: RootIdentity,
    binding: StableBinding,
    directory: Identity,
    home: Identity,
    manifest: Identity,
}

#[cfg(unix)]
impl ProfileIdentity {
    /// Tells whether a profile manifest read from disk still describes the live profile:
    /// the same version, binding and root record, and the same files, its copy of the root's
    /// included, checked as one set, so every pin in the manifest must have moved together.
    /// Inputs: the pinned manifest and the live identity. Output: true when they match.
    fn matches(&self, live: &ProfileIdentity) -> bool {
        let [state, database, root, root_manifest] = self.root.files();
        let [live_state, live_database, live_root, live_root_manifest] = live.root.files();
        self.version == live.version
            && self.binding == live.binding
            && self.root.version == live.root.version
            && self.root.database_instance == live.root.database_instance
            && same_files(
                &[
                    state,
                    database,
                    root,
                    root_manifest,
                    self.directory,
                    self.home,
                    self.manifest,
                ],
                &[
                    live_state,
                    live_database,
                    live_root,
                    live_root_manifest,
                    live.directory,
                    live.home,
                    live.manifest,
                ],
            )
    }
}

/// One custody folder held through the held layer (DESIGN-0515 D1), with the identity the
/// manifests record for it: opened, made and checked only through its parent's held folder.
struct Directory {
    folder: Folder,
    identity: Identity,
}

/// A custody name as the held layer takes it; a name it refuses is unsafe custody. Input:
/// the name. Output: the held name, or `native_profile_custody_unsafe`.
fn held_name(name: &OsStr) -> Result<Name, EngineError> {
    Name::checked(name).map_err(held_failure)
}

/// The custody code for a failed held call: a missing entry is an incomplete profile, and
/// every other cause is unsafe custody. Input: the cause. Output: the error.
fn held_failure(error: HeldError) -> EngineError {
    match error {
        HeldError::Missing => failure("native_profile_incomplete"),
        HeldError::Unsafe | HeldError::InUse | HeldError::Unavailable | HeldError::Full => {
            failure("native_profile_custody_unsafe")
        }
    }
}

/// The identity a manifest records for a held entry's pin. Input: the pin. Output: the
/// identity.
fn identity_of(pin: Pin) -> Identity {
    assert_ne!(pin.inode, 0);
    Identity {
        device: pin.device,
        inode: pin.inode,
    }
}

impl Directory {
    /// Wraps one held folder with its identity. Input: the folder. Output: the directory.
    fn of(folder: Folder) -> Self {
        let identity = identity_of(folder.pin());
        assert_eq!(identity.inode, folder.pin().inode);
        Self { folder, identity }
    }

    fn open(parent: &Folder, name: &OsStr) -> Result<Self, EngineError> {
        let folder = parent.open_dir(&held_name(name)?).map_err(held_failure)?;
        Ok(Self::of(folder))
    }

    fn create(parent: &Folder, name: &OsStr) -> Result<Self, EngineError> {
        let folder = parent
            .make_dir(&held_name(name)?)
            .map_err(|_| failure("native_profile_custody_unsafe"))?;
        Ok(Self::of(folder))
    }

    fn verify(&self, parent: &Folder, name: &OsStr) -> Result<(), EngineError> {
        let pin = self.folder.pin();
        assert_eq!(identity_of(pin), self.identity);
        parent
            .verify(&held_name(name)?, self.folder.file(), pin, true)
            .map_err(|_| failure("native_profile_custody_unsafe"))
    }

    /// The held folder's handle, for the Unix-only parts that still call the system by
    /// descriptor. Output: the file.
    fn file(&self) -> &File {
        self.folder.file()
    }
}

#[cfg(unix)]
fn verify_named(
    parent: &File,
    name: &OsStr,
    file: &File,
    identity: Identity,
    directory: bool,
) -> Result<(), EngineError> {
    let metadata = file.metadata().map_err(unsafe_custody)?;
    let named =
        rustix::fs::statat(parent, name, AtFlags::SYMLINK_NOFOLLOW).map_err(unsafe_custody)?;
    let safe = if directory {
        safe_directory(&metadata)
    } else {
        safe_file(&metadata)
    };
    if !safe
        || identity.inode == 0
        || Identity::of(&metadata) != identity
        || named.st_ino != metadata.ino()
        || named.st_dev as u64 != metadata.dev()
        || named.st_uid != metadata.uid()
        || named.st_mode as u32 != metadata.mode()
        || (!directory && named.st_nlink as u64 != metadata.nlink())
    {
        return Err(failure("native_profile_custody_unsafe"));
    }
    Ok(())
}

struct Manifest {
    file: File,
    identity: Identity,
    bytes: Vec<u8>,
}

impl Manifest {
    fn open(parent: &Folder, create: bool) -> Result<Self, EngineError> {
        let name = Name::constant(MANIFEST);
        let opened = if create {
            parent.make_file(&name)
        } else {
            parent.open_read(&name)
        };
        let (file, pin) = opened.map_err(|error| match error {
            HeldError::Missing => failure("native_profile_incomplete"),
            _ => failure("native_profile_custody_unsafe"),
        })?;
        let identity = identity_of(pin);
        let length = file.metadata().map_err(unsafe_custody)?.len();
        let mut bytes = Vec::new();
        if !create {
            if length == 0 || length > MAX_MANIFEST as u64 {
                return Err(failure("native_profile_identity_bounds"));
            }
            (&file)
                .take((MAX_MANIFEST + 1) as u64)
                .read_to_end(&mut bytes)
                .map_err(unsafe_custody)?;
            if bytes.len() > MAX_MANIFEST {
                return Err(failure("native_profile_identity_bounds"));
            }
        }
        let manifest = Self {
            file,
            identity,
            bytes,
        };
        manifest.verify(parent)?;
        Ok(manifest)
    }

    fn write(&mut self, parent: &Folder, bytes: Vec<u8>) -> Result<(), EngineError> {
        if bytes.is_empty() || bytes.len() > MAX_MANIFEST {
            return Err(failure("native_profile_identity_bounds"));
        }
        self.file.write_all(&bytes).map_err(unsafe_custody)?;
        self.file.sync_all().map_err(unsafe_custody)?;
        parent
            .sync()
            .map_err(|_| failure("native_profile_custody_unsafe"))?;
        self.bytes = bytes;
        self.verify(parent)
    }

    fn verify(&self, parent: &Folder) -> Result<(), EngineError> {
        let name = Name::constant(MANIFEST);
        let pin = Pin {
            device: self.identity.device,
            inode: self.identity.inode,
        };
        let named = || {
            parent
                .verify(&name, &self.file, pin, false)
                .map_err(|_| failure("native_profile_custody_unsafe"))
        };
        named()?;
        if self.file.metadata().map_err(unsafe_custody)?.len() != self.bytes.len() as u64 {
            return Err(failure("native_profile_identity_changed"));
        }
        let mut bytes = vec![0; self.bytes.len()];
        self.file
            .read_exact_at(&mut bytes, 0)
            .map_err(unsafe_custody)?;
        if bytes != self.bytes {
            return Err(failure("native_profile_identity_changed"));
        }
        named()
    }
}

pub(crate) struct NativeProfileCustody {
    anchor: Anchor,
    anchor_folder: Folder,
    database: File,
    database_name: OsString,
    database_identity: Identity,
    root_hash: Option<String>,
    profiles: BTreeMap<String, String>,
    /// The config each undone account save put back this engine run, by generated SDK home,
    /// the one times-only change a master turn may re-pin (rv-sec3 G4 r2 F1).
    #[cfg(unix)]
    undone: BTreeMap<std::path::PathBuf, generated_configured_rcli::PutBack>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum NativeProfileUsePurpose {
    Authentication,
    #[allow(
        dead_code,
        reason = "Refresh requires the separately qualified native host"
    )]
    Refresh,
    Logout,
    #[allow(
        dead_code,
        reason = "Execution requires the separately qualified native host"
    )]
    Execution,
    Check,
}

#[cfg(unix)]
impl NativeProfileUsePurpose {
    /// Takes this purpose's lock on one profile file without waiting: shared for an agent's
    /// execution and for a check of an existing sign-in, so they run on one signed-in
    /// account at once, and exclusive for sign-in, refresh and logout, which wait until
    /// no agent or check holds it. Inputs: the purpose and the open file. Output: the held
    /// lock or `native_profile_in_use`.
    fn lock(self, file: &File) -> Result<(), EngineError> {
        let operation = if matches!(self, Self::Execution | Self::Check) {
            FlockOperation::NonBlockingLockShared
        } else {
            FlockOperation::NonBlockingLockExclusive
        };
        take_lock(file, operation)
    }
}

/// Takes one non-blocking lock on a profile file.
/// Inputs: the open file and the shared or exclusive operation. Output: the held lock,
/// `native_profile_in_use` while a conflicting lock is held, or the lock failure.
#[cfg(unix)]
fn take_lock(file: &File, operation: FlockOperation) -> Result<(), EngineError> {
    flock(file, operation).map_err(|error| {
        failure(
            if error == rustix::io::Errno::WOULDBLOCK || error == rustix::io::Errno::AGAIN {
                "native_profile_in_use"
            } else {
                "native_profile_lock_unavailable"
            },
        )
    })
}

/// The locks that show no custodian holds one native profile, taken while a stale sign-in
/// is settled: a shared lock on its manifest, which every sign-in and logout excludes, and
/// its renewal lock, which every check holds, so agents keep their shared leases and no
/// custodian can start meanwhile. Holds nothing when the profile does not exist. Dropping
/// it unlocks both.
#[cfg(unix)]
pub(crate) struct NativeProfileRelease(Option<(File, HeldLock)>);

#[cfg(unix)]
impl Drop for NativeProfileRelease {
    /// Releases the manifest lock on its open file description when the settlement ends,
    /// so a process that shares it can never keep the profile busy afterwards; the renewal
    /// lock ends with its own guard. Input: the release being dropped. Output: none.
    fn drop(&mut self) {
        if let Some((file, _)) = &self.0 {
            let _ = flock(file, FlockOperation::Unlock);
        }
    }
}

#[cfg(unix)]
pub(crate) struct NativeProfileUse {
    custody: NativeProfileCustody,
    root: Directory,
    root_manifest: Manifest,
    profile: Directory,
    home: Directory,
    manifest: Manifest,
    configured: Option<configured_rcli::ConfiguredRcliUse>,
    key: String,
    host: HostStamp,
    binding: NativeProfileBinding,
    operation_id: RequestId,
    generation: u64,
    purpose: NativeProfileUsePurpose,
    renewal: Option<HeldLock>,
}

#[cfg(unix)]
impl NativeProfileUse {
    /// Names the account this lease holds the profile of. Input: the lease. Output: the
    /// account's connection id.
    pub(crate) fn account(&self) -> &crate::protocol::CredentialRef {
        &self.binding.connection.id
    }

    /// Names the engine's data root this lease's custody is anchored at, the canonical
    /// folder of `engine.db`: a profile home the engine made lies below it, while one the
    /// owner chose may lie anywhere. Input: the lease. Output: the folder.
    pub(crate) fn engine_root(&self) -> &std::path::Path {
        &self.custody.anchor.path
    }

    pub(crate) fn profile_path(&self) -> Result<std::path::PathBuf, EngineError> {
        self.revalidate_retained()?;
        if let Some(configured) = &self.configured {
            return Ok(configured.path().to_path_buf());
        }
        Ok(self
            .custody
            .anchor
            .path
            .join(ROOT)
            .join(&self.key)
            .join("home"))
    }

    pub(crate) fn revalidate_retained(&self) -> Result<(), EngineError> {
        self.revalidate(
            &self.host,
            &self.binding,
            &self.operation_id,
            self.generation,
            self.purpose,
        )
    }

    pub(crate) fn revalidate(
        &self,
        host: &HostStamp,
        binding: &NativeProfileBinding,
        operation_id: &RequestId,
        generation: u64,
        purpose: NativeProfileUsePurpose,
    ) -> Result<(), EngineError> {
        if host.db_instance != self.host.db_instance
            || host.engine_epoch != self.host.engine_epoch
            || binding != &self.binding
            || operation_id != &self.operation_id
            || generation != self.generation
            || purpose != self.purpose
        {
            return Err(failure("native_profile_binding_conflict"));
        }
        self.custody.verify()?;
        self.root
            .verify(&self.custody.anchor_folder, OsStr::new(ROOT))?;
        self.root_manifest.verify(&self.root.folder)?;
        self.profile
            .verify(&self.root.folder, OsStr::new(&self.key))?;
        self.manifest.verify(&self.profile.folder)?;
        if let Some(configured) = &self.configured {
            configured.verify(&self.home, &binding.connection)?;
            configured_rcli::require_entries(self.profile.file())?;
        } else {
            self.home.verify(&self.profile.folder, OsStr::new("home"))?;
            require_profile_entries(self.profile.file())?;
        }
        let slots = scan_slots(self.root.file())?;
        if slots.get(&self.key) != Some(&self.profile.identity)
            || self
                .custody
                .profiles
                .keys()
                .any(|key| !slots.contains_key(key))
            || digest(&self.manifest.bytes) != binding.custody_id
        {
            return Err(failure("native_profile_identity_changed"));
        }
        self.profile
            .verify(&self.root.folder, OsStr::new(&self.key))?;
        self.root
            .verify(&self.custody.anchor_folder, OsStr::new(ROOT))?;
        self.custody.verify()
    }
}

#[cfg(unix)]
impl Drop for NativeProfileUse {
    /// Releases the profile locks on their open file descriptions when the lease
    /// ends, so a process that shares one of them, such as a child forked by another
    /// thread before its exec, can never keep the profile busy after the lease.
    fn drop(&mut self) {
        let _ = flock(&self.manifest.file, FlockOperation::Unlock);
        if self.configured.is_some() {
            let _ = flock(self.home.file(), FlockOperation::Unlock);
        }
    }
}

/// Holds the engine root through the held layer as the parent of every custody folder,
/// opened again at the anchor's real place and required to be the anchor's own folder, so a
/// root swapped between the anchor's open and this one is refused (rv-sec3 RULING-16:50 C3).
/// Input: the anchor. Output: the held root, or `native_profile_custody_unsafe`.
fn anchor_folder(anchor: &Anchor) -> Result<Folder, EngineError> {
    let folder =
        Folder::root(&anchor.path).map_err(|_| failure("native_profile_custody_unsafe"))?;
    if identity_of(folder.pin()) != anchor.identity {
        return Err(failure("native_profile_custody_unsafe"));
    }
    assert_eq!(folder.pin().inode, anchor.identity.inode);
    Ok(folder)
}

impl NativeProfileCustody {
    #[cfg(unix)]
    pub(crate) fn capture(path: &Path, opened: &Metadata) -> Result<Self, EngineError> {
        let parent = path
            .parent()
            .ok_or_else(|| failure("native_profile_custody_unsafe"))?;
        let anchor = Anchor::open(parent).map_err(unsafe_custody)?;
        let database_name = path
            .file_name()
            .ok_or_else(|| failure("native_profile_custody_unsafe"))?
            .to_owned();
        let database = File::from(
            rustix::fs::openat(
                &anchor.directory,
                &database_name,
                OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
                Mode::empty(),
            )
            .map_err(unsafe_custody)?,
        );
        let custody = Self {
            anchor_folder: anchor_folder(&anchor)?,
            anchor,
            database,
            database_name,
            database_identity: Identity::of(opened),
            root_hash: None,
            profiles: BTreeMap::new(),
            undone: BTreeMap::new(),
        };
        custody.verify()?;
        Ok(custody)
    }

    #[cfg(unix)]
    fn verify(&self) -> Result<(), EngineError> {
        self.anchor.verify().map_err(unsafe_custody)?;
        verify_named(
            &self.anchor.directory,
            &self.database_name,
            &self.database,
            self.database_identity,
            false,
        )
    }

    /// Refuses a capture from metadata on Windows, where a `Metadata` cannot name the file it
    /// came from (std has no stable file id), so it could not bind custody to the caller's
    /// database (QUESTION-1700 Q1, rv-sec3 RULING-16:50 C2). Inputs: the path and metadata,
    /// unused. Output: `native_profile_custody_unavailable`.
    #[cfg(windows)]
    pub(crate) fn capture(_path: &Path, _opened: &Metadata) -> Result<Self, EngineError> {
        Err(failure("native_profile_custody_unavailable"))
    }

    /// Captures custody of the database a caller holds open on Windows (QUESTION-1700 Q1,
    /// rv-sec3 RULING-16:50 C1-C4): the engine root is held as the W3-4a anchor and as a held
    /// folder at its real place, custody opens the database itself with W1's private read-only
    /// open, which follows no link, refuses a second link and never shares delete, and keeps
    /// that handle; the caller's handle must be the same file with the same nonzero identity.
    /// Inputs: the database path and the caller's handle. Output: the custody, or the refusal.
    #[cfg(windows)]
    pub(crate) fn capture_held(path: &Path, opened: &File) -> Result<Self, EngineError> {
        let parent = path
            .parent()
            .ok_or_else(|| failure("native_profile_custody_unsafe"))?;
        let anchor = Anchor::open(parent).map_err(unsafe_custody)?;
        let database_name = path
            .file_name()
            .ok_or_else(|| failure("native_profile_custody_unsafe"))?
            .to_owned();
        let anchor_folder = anchor_folder(&anchor)?;
        let (database, pin) = anchor_folder
            .open_read(&held_name(&database_name)?)
            .map_err(|_| failure("native_profile_custody_unsafe"))?;
        let same =
            crazyagent_win_sandbox::private::same(&database, opened).map_err(unsafe_custody)?;
        let caller = Identity::of_file(opened).map_err(unsafe_custody)?;
        if !same || pin.inode == 0 || caller.inode == 0 || caller != identity_of(pin) {
            return Err(failure("native_profile_custody_unsafe"));
        }
        let custody = Self {
            anchor,
            anchor_folder,
            database,
            database_name,
            database_identity: caller,
            root_hash: None,
            profiles: BTreeMap::new(),
        };
        custody.verify()?;
        Ok(custody)
    }

    /// Confirms custody on Windows, the twin of the Unix check (rv-sec3 RULING-16:50 C2, C3):
    /// the anchor still verifies, the stored identity is nonzero, and the database name, opened
    /// again through the held root, is still the held file with that identity, judged private.
    /// Output: Ok, or `native_profile_custody_unsafe`.
    #[cfg(windows)]
    fn verify(&self) -> Result<(), EngineError> {
        self.anchor.verify().map_err(unsafe_custody)?;
        if self.database_identity.inode == 0 {
            return Err(failure("native_profile_custody_unsafe"));
        }
        let pin = Pin {
            device: self.database_identity.device,
            inode: self.database_identity.inode,
        };
        self.anchor_folder
            .verify(&held_name(&self.database_name)?, &self.database, pin, false)
            .map_err(|_| failure("native_profile_custody_unsafe"))
    }

    /// Zeroes the stored database identity, for the row that requires `verify` to refuse it.
    #[cfg(all(test, windows))]
    fn forget_database_identity(&mut self) {
        self.database_identity = Identity {
            device: 0,
            inode: 0,
        };
        assert_eq!(self.database_identity.inode, 0);
    }

    /// Names the home directory of a native managed profile without taking it, for tests
    /// that check which home a lease or launch uses; no agent sandbox is given this home.
    /// Input: the profile reference. Output: the path.
    #[cfg(test)]
    pub(crate) fn profile_home(&self, profile_ref: &CredentialRef) -> std::path::PathBuf {
        self.anchor
            .path
            .join(ROOT)
            .join(digest(profile_ref.as_str().as_bytes()))
            .join("home")
    }

    #[cfg(unix)]
    fn retain(&self) -> Result<Self, EngineError> {
        self.verify()?;
        let retained = Self {
            anchor: Anchor {
                supplied: self.anchor.supplied.clone(),
                path: self.anchor.path.clone(),
                directory: self.anchor.directory.try_clone().map_err(unsafe_custody)?,
                identity: self.anchor.identity,
            },
            anchor_folder: anchor_folder(&self.anchor)?,
            database: self.database.try_clone().map_err(unsafe_custody)?,
            database_name: self.database_name.clone(),
            database_identity: self.database_identity,
            root_hash: self.root_hash.clone(),
            profiles: self.profiles.clone(),
            undone: self.undone.clone(),
        };
        retained.verify()?;
        Ok(retained)
    }

    #[cfg(unix)]
    fn retained_root(
        &self,
        host: &HostStamp,
    ) -> Result<(Directory, Manifest, RootIdentity), EngineError> {
        let root = Directory::open(&self.anchor_folder, OsStr::new(ROOT))?;
        let manifest = Manifest::open(&root.folder, false)?;
        let expected = RootIdentity {
            version: 1,
            database_instance: host.db_instance.clone(),
            state: self.anchor.identity,
            database: self.database_identity,
            directory: root.identity,
            manifest: manifest.identity,
        };
        let actual: RootIdentity = serde_json::from_slice(&manifest.bytes)
            .map_err(|_| failure("native_profile_identity_invalid"))?;
        if !actual.matches(&expected) {
            return Err(failure("native_profile_binding_conflict"));
        }
        if self
            .root_hash
            .as_ref()
            .is_some_and(|pinned| pinned != &digest(&manifest.bytes))
        {
            return Err(failure("native_profile_identity_changed"));
        }
        Ok((root, manifest, expected))
    }

    #[cfg(unix)]
    pub(crate) fn acquire_use(
        &mut self,
        host: &HostStamp,
        binding: &NativeProfileBinding,
        operation_id: &RequestId,
        generation: u64,
        purpose: NativeProfileUsePurpose,
    ) -> Result<NativeProfileUse, EngineError> {
        self.acquire_use_within(
            host,
            binding,
            operation_id,
            generation,
            purpose,
            Some(std::time::Duration::ZERO),
        )
    }

    /// Takes one use of a native profile like [`Self::acquire_use`], with a chosen hold on
    /// the account's renewal lock for a sign-in, refresh, logout or check: `None` takes
    /// none, for the engine admitting a custodian that holds it itself, and `Some(wait)`
    /// waits up to `wait` for a running renewal to end, which may outlive its agent's lease.
    /// An agent's execution never takes it. Inputs: the host, binding, operation,
    /// generation, purpose and that choice. Output: the use, or `native_profile_in_use`
    /// while the lock stays busy.
    #[cfg(unix)]
    pub(crate) fn acquire_use_within(
        &mut self,
        host: &HostStamp,
        binding: &NativeProfileBinding,
        operation_id: &RequestId,
        generation: u64,
        purpose: NativeProfileUsePurpose,
        renewal_wait: Option<std::time::Duration>,
    ) -> Result<NativeProfileUse, EngineError> {
        assert!(generation > 0);
        assert_eq!(host.db_instance, binding.database_instance);
        assert_eq!(host.engine_epoch, binding.engine_epoch);
        let custody = self.retain()?;
        let (root, root_manifest, root_identity) = self.retained_root(host)?;
        let stable = StableBinding::of(&binding.connection)?;
        let key = stable.key();
        let profile = Directory::open(&root.folder, OsStr::new(&key))?;
        let (home, manifest) = prepare_profile(&profile, &root_identity, stable)?;
        let hash = digest(&manifest.bytes);
        if binding.custody_id != hash
            || self
                .profiles
                .get(&key)
                .is_some_and(|pinned| pinned != &hash)
        {
            return Err(failure("native_profile_identity_changed"));
        }
        let mut guard = NativeProfileUse {
            custody,
            root,
            root_manifest,
            profile,
            home,
            manifest,
            configured: None,
            key: key.clone(),
            host: host.clone(),
            binding: binding.clone(),
            operation_id: operation_id.clone(),
            generation,
            purpose,
            renewal: None,
        };
        guard.revalidate(host, binding, operation_id, generation, purpose)?;
        purpose.lock(&guard.manifest.file)?;
        if purpose != NativeProfileUsePurpose::Execution
            && let Some(wait) = renewal_wait
        {
            let held = renewal::hold(guard.profile.file(), wait).map_err(|error| {
                match error.0.as_str() {
                    "native_profile_renewal_busy" => failure("native_profile_in_use"),
                    _ => error,
                }
            })?;
            guard.renewal = Some(held);
        }
        guard.revalidate(host, binding, operation_id, generation, purpose)?;
        self.root_hash = Some(digest(&guard.root_manifest.bytes));
        remember_profile(&mut self.profiles, key, hash);
        Ok(guard)
    }

    /// Takes the locks that show no custodian holds one native profile, so a stale sign-in
    /// can be settled while agents keep using the account and no custodian can start on it.
    /// Inputs: this engine's host and the profile key. Output: the held locks, an empty
    /// release when the profile does not exist, or `None` while a sign-in, a logout, a
    /// check or a renewal holds the profile.
    #[cfg(unix)]
    pub(crate) fn hold_released_profile(
        &self,
        host: &HostStamp,
        key: &str,
    ) -> Result<Option<NativeProfileRelease>, EngineError> {
        if key.len() != 64
            || !key
                .bytes()
                .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
        {
            return Err(failure("native_profile_identity_invalid"));
        }
        self.verify()?;
        let absent = |error: EngineError| {
            if error.0 == "native_profile_incomplete" {
                Ok(Some(NativeProfileRelease(None)))
            } else {
                Err(error)
            }
        };
        let root = match self.retained_root(host) {
            Ok((root, _, _)) => root,
            Err(error) => return absent(error),
        };
        let profile = match Directory::open(&root.folder, OsStr::new(key)) {
            Ok(profile) => profile,
            Err(error) => return absent(error),
        };
        let manifest = match Manifest::open(&profile.folder, false) {
            Ok(manifest) => manifest,
            Err(error) => return absent(error),
        };
        let renewal = match renewal::hold(profile.file(), std::time::Duration::ZERO) {
            Ok(held) => held,
            Err(error) if error.0 == "native_profile_renewal_busy" => return Ok(None),
            Err(error) => return Err(error),
        };
        match flock(&manifest.file, FlockOperation::NonBlockingLockShared) {
            Ok(()) => {}
            Err(error)
                if error == rustix::io::Errno::WOULDBLOCK || error == rustix::io::Errno::AGAIN =>
            {
                return Ok(None);
            }
            Err(_) => return Err(failure("native_profile_lock_unavailable")),
        }
        let release = NativeProfileRelease(Some((manifest.file, renewal)));
        profile.verify(&root.folder, OsStr::new(key))?;
        self.verify()?;
        Ok(Some(release))
    }

    fn root(
        &mut self,
        host: &HostStamp,
    ) -> Result<(Directory, Manifest, RootIdentity), EngineError> {
        let present = match self.anchor_folder.open_dir(&Name::constant(ROOT)) {
            Ok(folder) => Some(Directory::of(folder)),
            Err(HeldError::Missing) => None,
            Err(_) => return Err(failure("native_profile_custody_unsafe")),
        };
        if present.is_none() && self.root_hash.is_some() {
            return Err(failure("native_profile_identity_changed"));
        }
        let (directory, manifest) = match present {
            Some(directory) => {
                let manifest = Manifest::open(&directory.folder, false)?;
                (directory, manifest)
            }
            None => self.create_root(host)?,
        };
        let expected = RootIdentity {
            version: 1,
            database_instance: host.db_instance.clone(),
            state: self.anchor.identity,
            database: self.database_identity,
            directory: directory.identity,
            manifest: manifest.identity,
        };
        let actual: RootIdentity = serde_json::from_slice(&manifest.bytes)
            .map_err(|_| failure("native_profile_identity_invalid"))?;
        if !actual.matches(&expected) {
            return Err(failure("native_profile_binding_conflict"));
        }
        manifest.verify(&directory.folder)?;
        let hash = digest(&manifest.bytes);
        if self
            .root_hash
            .as_ref()
            .is_some_and(|pinned| pinned != &hash)
        {
            return Err(failure("native_profile_identity_changed"));
        }
        self.root_hash = Some(hash);
        Ok((directory, manifest, expected))
    }

    /// Builds the profile root with its identity manifest in a staging directory and
    /// renames it into place whole. Input: the engine host stamp. Output: the root and
    /// its manifest; an interruption leaves only a staging directory for the next sweep.
    fn create_root(&self, host: &HostStamp) -> Result<(Directory, Manifest), EngineError> {
        staging::sweep(&self.anchor_folder, ROOT_STAGING)?;
        let staged = Staging::create(&self.anchor_folder, ROOT_STAGING)?;
        let mut manifest = Manifest::open(&staged.directory.folder, true)?;
        step()?;
        let expected = RootIdentity {
            version: 1,
            database_instance: host.db_instance.clone(),
            state: self.anchor.identity,
            database: self.database_identity,
            directory: staged.directory.identity,
            manifest: manifest.identity,
        };
        manifest.write(
            &staged.directory.folder,
            serde_json::to_vec(&expected)
                .map_err(|_| failure("native_profile_identity_invalid"))?,
        )?;
        step()?;
        let written = manifest.identity;
        drop(manifest);
        let directory = staged.commit(&self.anchor_folder, ROOT)?;
        assert_eq!(directory.identity, expected.directory);
        let manifest = Manifest::open(&directory.folder, false)?;
        if manifest.identity != written {
            return Err(failure("native_profile_identity_changed"));
        }
        Ok((directory, manifest))
    }

    #[cfg(unix)]
    pub(crate) fn prepare(
        &mut self,
        host: &HostStamp,
        connection: &ModelConnection,
    ) -> Result<(String, String), EngineError> {
        let binding = StableBinding::of(connection)?;
        self.verify()?;
        let (root, root_manifest, root_identity) = self.root(host)?;
        let slots = scan_slots(root.file())?;
        if self.profiles.keys().any(|key| !slots.contains_key(key)) {
            return Err(failure("native_profile_identity_changed"));
        }
        let key = binding.key();
        let exists = slots.contains_key(&key);
        if !exists && slots.len() >= MAX_PROFILES {
            return Err(failure("native_profile_capacity"));
        }
        let profile = if exists {
            Directory::open(&root.folder, OsStr::new(&key))?
        } else {
            create_profile(&root.folder, &key, &root_identity, binding.clone())?
        };
        let (home, manifest) = prepare_profile(&profile, &root_identity, binding)?;
        let hash = digest(&manifest.bytes);
        if self
            .profiles
            .get(&key)
            .is_some_and(|pinned| pinned != &hash)
        {
            return Err(failure("native_profile_identity_changed"));
        }
        manifest.verify(&profile.folder)?;
        home.verify(&profile.folder, OsStr::new("home"))?;
        profile.verify(&root.folder, OsStr::new(&key))?;
        root_manifest.verify(&root.folder)?;
        root.verify(&self.anchor_folder, OsStr::new(ROOT))?;
        let current_slots = scan_slots(root.file())?;
        if current_slots.get(&key) != Some(&profile.identity)
            || slots
                .iter()
                .any(|(key, id)| current_slots.get(key) != Some(id))
        {
            return Err(failure("native_profile_identity_changed"));
        }
        self.verify()?;
        remember_profile(&mut self.profiles, key.clone(), hash.clone());
        Ok((hash, key))
    }

    pub(crate) fn receipt(
        binding: NativeProfileBinding,
        requested: &CredentialRef,
        profile: &CredentialRef,
        key: &str,
    ) -> PreparedNativeProfile {
        assert_eq!(&binding.connection.id, requested);
        assert_eq!(digest(profile.as_str().as_bytes()), key);
        PreparedNativeProfile::from_binding(binding)
    }
}

/// Builds a native profile slot with an empty home and its identity manifest in a
/// staging directory and renames it to its key whole. Inputs: the profile root, the
/// slot key, the root identity and the stable binding. Output: the slot directory.
#[cfg(unix)]
fn create_profile(
    root: &Folder,
    key: &str,
    root_identity: &RootIdentity,
    binding: StableBinding,
) -> Result<Directory, EngineError> {
    staging::sweep(root, STAGING)?;
    let staged = Staging::create(root, STAGING)?;
    let home = Directory::create(&staged.directory.folder, OsStr::new("home"))?;
    step()?;
    require_empty_home(home.file())?;
    let mut manifest = Manifest::open(&staged.directory.folder, true)?;
    step()?;
    let expected = ProfileIdentity {
        version: 1,
        root: root_identity.clone(),
        binding,
        directory: staged.directory.identity,
        home: home.identity,
        manifest: manifest.identity,
    };
    manifest.write(
        &staged.directory.folder,
        serde_json::to_vec(&expected).map_err(|_| failure("native_profile_identity_invalid"))?,
    )?;
    home.file().sync_all().map_err(unsafe_custody)?;
    step()?;
    let profile = staged.commit(root, key)?;
    assert_eq!(profile.identity, expected.directory);
    Ok(profile)
}

#[cfg(unix)]
fn prepare_profile(
    profile: &Directory,
    root: &RootIdentity,
    binding: StableBinding,
) -> Result<(Directory, Manifest), EngineError> {
    let home = Directory::open(&profile.folder, OsStr::new("home"))?;
    let manifest = Manifest::open(&profile.folder, false)?;
    let expected = ProfileIdentity {
        version: 1,
        root: root.clone(),
        binding,
        directory: profile.identity,
        home: home.identity,
        manifest: manifest.identity,
    };
    let actual: ProfileIdentity = serde_json::from_slice(&manifest.bytes)
        .map_err(|_| failure("native_profile_identity_invalid"))?;
    if !actual.matches(&expected) {
        return Err(failure("native_profile_binding_conflict"));
    }
    require_profile_entries(profile.file())?;
    Ok((home, manifest))
}

#[cfg(unix)]
fn scan_slots(root: &File) -> Result<BTreeMap<String, Identity>, EngineError> {
    let mut slots = BTreeMap::new();
    let entries = Dir::read_from(root).map_err(unsafe_custody)?;
    let mut visited = 0;
    let mut staged = 0;
    for entry in entries {
        visited += 1;
        if visited > MAX_PROFILES + staging::STAGING_MAX + 3 {
            return Err(failure("native_profile_capacity"));
        }
        let entry = entry.map_err(unsafe_custody)?;
        let name = entry.file_name();
        let bytes = name.to_bytes();
        if matches!(bytes, b"." | b".." | b"identity.json") {
            continue;
        }
        let staging = staging::is_staging(bytes, STAGING);
        let key = std::str::from_utf8(bytes).map_err(unsafe_custody)?;
        if !staging && !hex(key, 64) {
            return Err(failure("native_profile_custody_unsafe"));
        }
        if staging {
            staged += 1;
        }
        if slots.len() == MAX_PROFILES || staged > staging::STAGING_MAX {
            return Err(failure("native_profile_capacity"));
        }
        let metadata =
            rustix::fs::statat(root, name, AtFlags::SYMLINK_NOFOLLOW).map_err(unsafe_custody)?;
        if FileType::from_raw_mode(metadata.st_mode) != FileType::Directory
            || metadata.st_uid != rustix::process::getuid().as_raw()
            || metadata.st_mode & 0o7777 != 0o700
            || metadata.st_ino == 0
        {
            return Err(failure("native_profile_custody_unsafe"));
        }
        if staging {
            continue;
        }
        let identity = Identity {
            device: metadata.st_dev as u64,
            inode: metadata.st_ino,
        };
        if slots.insert(key.to_owned(), identity).is_some() {
            return Err(failure("native_profile_identity_changed"));
        }
    }
    Ok(slots)
}

#[cfg(unix)]
fn require_empty_home(home: &File) -> Result<(), EngineError> {
    let entries = Dir::read_from(home).map_err(unsafe_custody)?;
    for (index, entry) in entries.enumerate() {
        let entry = entry.map_err(unsafe_custody)?;
        if index > 2 || !matches!(entry.file_name().to_bytes(), b"." | b"..") {
            return Err(failure("native_profile_new_home_not_empty"));
        }
    }
    Ok(())
}

#[cfg(unix)]
fn require_profile_entries(profile: &File) -> Result<(), EngineError> {
    let entries = Dir::read_from(profile).map_err(unsafe_custody)?;
    let mut identity = false;
    let mut home = false;
    let mut lock = false;
    for (index, entry) in entries.enumerate() {
        let entry = entry.map_err(unsafe_custody)?;
        if index > 4 {
            return Err(failure("native_profile_custody_unsafe"));
        }
        match entry.file_name().to_bytes() {
            b"." | b".." => {}
            b"identity.json" if !identity => identity = true,
            b"home" if !home => home = true,
            b"home.lock" if !lock => {
                require_config_lock(profile)?;
                lock = true;
            }
            _ => return Err(failure("native_profile_custody_unsafe")),
        }
    }
    if !identity || !home {
        return Err(failure("native_profile_incomplete"));
    }
    Ok(())
}

/// Checks `home.lock`, the folder Claude Code makes beside its configuration folder while it
/// renews an access token and leaves behind when it is stopped mid-renewal, for the next run to
/// reclaim. Input: the profile folder. Output: done when the entry is a folder of this user that
/// no one else can write or when it is already gone, else `native_profile_custody_unsafe`.
#[cfg(unix)]
fn require_config_lock(profile: &File) -> Result<(), EngineError> {
    let metadata = match rustix::fs::statat(profile, "home.lock", AtFlags::SYMLINK_NOFOLLOW) {
        Ok(metadata) => metadata,
        Err(rustix::io::Errno::NOENT) => return Ok(()),
        Err(error) => return Err(unsafe_custody(error)),
    };
    if FileType::from_raw_mode(metadata.st_mode) != FileType::Directory
        || metadata.st_uid != rustix::process::getuid().as_raw()
        || metadata.st_mode & 0o022 != 0
    {
        return Err(failure("native_profile_custody_unsafe"));
    }
    Ok(())
}

#[cfg(unix)]
fn remember_profile(pins: &mut BTreeMap<String, String>, key: String, hash: String) {
    pins.insert(key, hash);
    assert!(pins.len() <= MAX_PROFILES);
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::config::{ModelEndpoint, ModelReference};
    use crate::protocol::{ConfigId, Cursor};

    fn reference(value: &str) -> CredentialRef {
        CredentialRef::new(value).unwrap()
    }

    fn binding() -> NativeProfileBinding {
        NativeProfileBinding {
            database_instance: "a".repeat(64),
            engine_epoch: "b".repeat(64),
            config_id: ConfigId::new("c".repeat(64)).unwrap(),
            source_hash: "d".repeat(64),
            at_revision: Cursor::new(0),
            connection: ModelConnection {
                id: reference("connection"),
                provider: reference("provider"),
                harness: reference("codex"),
                protocol: reference("native"),
                account_ref: reference("account"),
                endpoint: ModelEndpoint::Native {},
                model: ModelReference {
                    id: "model".into(),
                    author: reference("author"),
                },
                auth: ModelAuthentication::NativeManaged {
                    profile_ref: reference("profile"),
                },
            },
            custody_id: "e".repeat(64),
        }
    }

    #[test]
    #[should_panic(expected = "assertion `left == right` failed")]
    fn assembled_receipt_cannot_change_the_selected_connection() {
        NativeProfileCustody::receipt(
            binding(),
            &reference("another"),
            &reference("profile"),
            &digest(b"profile"),
        );
    }

    #[test]
    #[should_panic(expected = "assertion `left == right` failed")]
    fn assembled_receipt_cannot_change_the_profile_key() {
        NativeProfileCustody::receipt(
            binding(),
            &reference("connection"),
            &reference("profile"),
            &digest(b"another"),
        );
    }

    #[test]
    #[should_panic(expected = "assertion failed: pins.len() <= MAX_PROFILES")]
    fn retained_pin_assembly_cannot_exceed_the_validated_capacity() {
        let mut pins = (0..MAX_PROFILES)
            .map(|index| (format!("{index:064x}"), "a".repeat(64)))
            .collect();
        remember_profile(&mut pins, "overflow".into(), "b".repeat(64));
    }
}

#[cfg(all(test, unix))]
mod use_tests;

#[cfg(all(test, unix))]
mod configured_rcli_tests;

#[cfg(all(test, windows))]
mod custody_windows_tests;
