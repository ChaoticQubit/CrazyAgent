use super::{Anchor, Identity, hex, safe_directory, safe_file};
use crate::config::{ModelAuthentication, ModelConnection};
use crate::engine::EngineError;
use crate::engine::owner::{HostStamp, NativeProfileBinding, PreparedNativeProfile};
use crate::protocol::{CredentialRef, RequestId};
use rustix::fs::{AtFlags, Dir, FileType, FlockOperation, Mode, OFlags, flock};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fs::{File, Metadata};
use std::io::{Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{FileExt, MetadataExt};
use std::path::Path;

mod agent_copy;
mod configured_rcli;
mod generated_configured_rcli;
mod staging;
pub(crate) use configured_rcli::ConfiguredRcliProfileMetadata;
use configured_rcli::lock;
use staging::{ROOT_STAGING, STAGING, Staging, step};

const ROOT: &str = "native-profiles";
const MANIFEST: &str = "identity.json";
const MAX_MANIFEST: usize = 4096;
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StableBinding {
    profile_ref: CredentialRef,
    account_ref: CredentialRef,
    provider: CredentialRef,
    harness: CredentialRef,
    protocol: CredentialRef,
}

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

struct Directory {
    file: File,
    identity: Identity,
}

impl Directory {
    fn open(parent: &File, name: &OsStr) -> Result<Self, EngineError> {
        let file = File::from(
            rustix::fs::openat(
                parent,
                name,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(|error| {
                if error == rustix::io::Errno::NOENT {
                    failure("native_profile_incomplete")
                } else {
                    unsafe_custody(error)
                }
            })?,
        );
        let identity = Identity::of(&file.metadata().map_err(unsafe_custody)?);
        verify_named(parent, name, &file, identity, true)?;
        Ok(Self { file, identity })
    }

    fn create(parent: &File, name: &OsStr) -> Result<Self, EngineError> {
        rustix::fs::mkdirat(parent, name, Mode::from_raw_mode(0o700)).map_err(unsafe_custody)?;
        parent.sync_all().map_err(unsafe_custody)?;
        Self::open(parent, name)
    }

    fn verify(&self, parent: &File, name: &OsStr) -> Result<(), EngineError> {
        verify_named(parent, name, &self.file, self.identity, true)
    }
}

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
    fn open(parent: &File, create: bool) -> Result<Self, EngineError> {
        let access = if create {
            OFlags::RDWR | OFlags::CREATE | OFlags::EXCL
        } else {
            OFlags::RDONLY
        };
        let file = File::from(
            rustix::fs::openat(
                parent,
                MANIFEST,
                access | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
                Mode::from_raw_mode(0o600),
            )
            .map_err(|error| {
                if error == rustix::io::Errno::NOENT {
                    failure("native_profile_incomplete")
                } else {
                    unsafe_custody(error)
                }
            })?,
        );
        let metadata = file.metadata().map_err(unsafe_custody)?;
        let identity = Identity::of(&metadata);
        verify_named(parent, OsStr::new(MANIFEST), &file, identity, false)?;
        let mut bytes = Vec::new();
        if !create {
            if metadata.len() == 0 || metadata.len() > MAX_MANIFEST as u64 {
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

    fn write(&mut self, parent: &File, bytes: Vec<u8>) -> Result<(), EngineError> {
        if bytes.is_empty() || bytes.len() > MAX_MANIFEST {
            return Err(failure("native_profile_identity_bounds"));
        }
        self.file.write_all(&bytes).map_err(unsafe_custody)?;
        self.file.sync_all().map_err(unsafe_custody)?;
        parent.sync_all().map_err(unsafe_custody)?;
        self.bytes = bytes;
        self.verify(parent)
    }

    fn verify(&self, parent: &File) -> Result<(), EngineError> {
        verify_named(
            parent,
            OsStr::new(MANIFEST),
            &self.file,
            self.identity,
            false,
        )?;
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
        verify_named(
            parent,
            OsStr::new(MANIFEST),
            &self.file,
            self.identity,
            false,
        )
    }
}

pub(crate) struct NativeProfileCustody {
    anchor: Anchor,
    database: File,
    database_name: OsString,
    database_identity: Identity,
    root_hash: Option<String>,
    profiles: BTreeMap<String, String>,
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
}

impl NativeProfileUsePurpose {
    /// Takes this purpose's lock on one profile file without waiting: shared for an agent's
    /// execution, so any number of agents run on one signed-in account at once, and
    /// exclusive for sign-in, refresh and logout, which wait until no agent holds it.
    /// Inputs: the purpose and the open file. Output: the held lock or `native_profile_in_use`.
    fn lock(self, file: &File) -> Result<(), EngineError> {
        let operation = if self == Self::Execution {
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

/// The sign-in lock of one native profile, taken while a stale sign-in is settled so no
/// custodian can start on that profile meanwhile. Holds no file when the profile does not
/// exist. Dropping it unlocks the open file description.
pub(crate) struct NativeProfileRelease(Option<File>);

impl Drop for NativeProfileRelease {
    /// Releases the profile lock on its open file description when the settlement ends,
    /// so a process that shares it can never keep the profile busy afterwards.
    /// Input: the release being dropped. Output: none.
    fn drop(&mut self) {
        if let Some(file) = &self.0 {
            let _ = flock(file, FlockOperation::Unlock);
        }
    }
}

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
}

impl NativeProfileUse {
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
            .verify(&self.custody.anchor.directory, OsStr::new(ROOT))?;
        self.root_manifest.verify(&self.root.file)?;
        self.profile
            .verify(&self.root.file, OsStr::new(&self.key))?;
        self.manifest.verify(&self.profile.file)?;
        if let Some(configured) = &self.configured {
            configured.verify(&self.home, &binding.connection)?;
            configured_rcli::require_entries(&self.profile.file)?;
        } else {
            self.home.verify(&self.profile.file, OsStr::new("home"))?;
            require_profile_entries(&self.profile.file)?;
        }
        let slots = scan_slots(&self.root.file)?;
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
            .verify(&self.root.file, OsStr::new(&self.key))?;
        self.root
            .verify(&self.custody.anchor.directory, OsStr::new(ROOT))?;
        self.custody.verify()
    }
}

impl Drop for NativeProfileUse {
    /// Releases the profile locks on their open file descriptions when the lease
    /// ends, so a process that shares one of them, such as a child forked by another
    /// thread before its exec, can never keep the profile busy after the lease.
    fn drop(&mut self) {
        let _ = flock(&self.manifest.file, FlockOperation::Unlock);
        if self.configured.is_some() {
            let _ = flock(&self.home.file, FlockOperation::Unlock);
        }
    }
}

impl NativeProfileCustody {
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
            anchor,
            database,
            database_name,
            database_identity: Identity::of(opened),
            root_hash: None,
            profiles: BTreeMap::new(),
        };
        custody.verify()?;
        Ok(custody)
    }

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

    fn retain(&self) -> Result<Self, EngineError> {
        self.verify()?;
        let retained = Self {
            anchor: Anchor {
                supplied: self.anchor.supplied.clone(),
                path: self.anchor.path.clone(),
                directory: self.anchor.directory.try_clone().map_err(unsafe_custody)?,
                identity: self.anchor.identity,
            },
            database: self.database.try_clone().map_err(unsafe_custody)?,
            database_name: self.database_name.clone(),
            database_identity: self.database_identity,
            root_hash: self.root_hash.clone(),
            profiles: self.profiles.clone(),
        };
        retained.verify()?;
        Ok(retained)
    }

    fn retained_root(
        &self,
        host: &HostStamp,
    ) -> Result<(Directory, Manifest, RootIdentity), EngineError> {
        let root = Directory::open(&self.anchor.directory, OsStr::new(ROOT))?;
        let manifest = Manifest::open(&root.file, false)?;
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
        if actual != expected {
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

    pub(crate) fn acquire_use(
        &mut self,
        host: &HostStamp,
        binding: &NativeProfileBinding,
        operation_id: &RequestId,
        generation: u64,
        purpose: NativeProfileUsePurpose,
    ) -> Result<NativeProfileUse, EngineError> {
        assert!(generation > 0);
        assert_eq!(host.db_instance, binding.database_instance);
        assert_eq!(host.engine_epoch, binding.engine_epoch);
        let custody = self.retain()?;
        let (root, root_manifest, root_identity) = self.retained_root(host)?;
        let stable = StableBinding::of(&binding.connection)?;
        let key = stable.key();
        let profile = Directory::open(&root.file, OsStr::new(&key))?;
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
        let guard = NativeProfileUse {
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
        };
        guard.revalidate(host, binding, operation_id, generation, purpose)?;
        purpose.lock(&guard.manifest.file)?;
        guard.revalidate(host, binding, operation_id, generation, purpose)?;
        self.root_hash = Some(digest(&guard.root_manifest.bytes));
        remember_profile(&mut self.profiles, key, hash);
        Ok(guard)
    }

    /// Takes the sign-in lock of one native profile when no process holds it, so a stale
    /// sign-in can be settled while no custodian can start on that profile.
    /// Inputs: this engine's host and the profile key. Output: the held lock, an empty
    /// one when the profile does not exist, or `None` while another process holds it.
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
        let profile = match Directory::open(&root.file, OsStr::new(key)) {
            Ok(profile) => profile,
            Err(error) => return absent(error),
        };
        let manifest = match Manifest::open(&profile.file, false) {
            Ok(manifest) => manifest,
            Err(error) => return absent(error),
        };
        match flock(&manifest.file, FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => {}
            Err(error)
                if error == rustix::io::Errno::WOULDBLOCK || error == rustix::io::Errno::AGAIN =>
            {
                return Ok(None);
            }
            Err(_) => return Err(failure("native_profile_lock_unavailable")),
        }
        let release = NativeProfileRelease(Some(manifest.file));
        profile.verify(&root.file, OsStr::new(key))?;
        self.verify()?;
        Ok(Some(release))
    }

    fn root(
        &mut self,
        host: &HostStamp,
    ) -> Result<(Directory, Manifest, RootIdentity), EngineError> {
        let exists =
            match rustix::fs::statat(&self.anchor.directory, ROOT, AtFlags::SYMLINK_NOFOLLOW) {
                Ok(_) => true,
                Err(rustix::io::Errno::NOENT) => false,
                Err(error) => return Err(unsafe_custody(error)),
            };
        if !exists && self.root_hash.is_some() {
            return Err(failure("native_profile_identity_changed"));
        }
        let (directory, manifest) = if exists {
            let directory = Directory::open(&self.anchor.directory, OsStr::new(ROOT))?;
            let manifest = Manifest::open(&directory.file, false)?;
            (directory, manifest)
        } else {
            self.create_root(host)?
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
        if actual != expected {
            return Err(failure("native_profile_binding_conflict"));
        }
        manifest.verify(&directory.file)?;
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
        staging::sweep(&self.anchor.directory, ROOT_STAGING)?;
        let staged = Staging::create(&self.anchor.directory, ROOT_STAGING)?;
        let mut manifest = Manifest::open(&staged.directory.file, true)?;
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
            &staged.directory.file,
            serde_json::to_vec(&expected)
                .map_err(|_| failure("native_profile_identity_invalid"))?,
        )?;
        step()?;
        let directory = staged.commit(&self.anchor.directory, ROOT)?;
        assert_eq!(directory.identity, expected.directory);
        Ok((directory, manifest))
    }

    pub(crate) fn prepare(
        &mut self,
        host: &HostStamp,
        connection: &ModelConnection,
    ) -> Result<(String, String), EngineError> {
        let binding = StableBinding::of(connection)?;
        self.verify()?;
        let (root, root_manifest, root_identity) = self.root(host)?;
        let slots = scan_slots(&root.file)?;
        if self.profiles.keys().any(|key| !slots.contains_key(key)) {
            return Err(failure("native_profile_identity_changed"));
        }
        let key = binding.key();
        let exists = slots.contains_key(&key);
        if !exists && slots.len() >= MAX_PROFILES {
            return Err(failure("native_profile_capacity"));
        }
        let profile = if exists {
            Directory::open(&root.file, OsStr::new(&key))?
        } else {
            create_profile(&root.file, &key, &root_identity, binding.clone())?
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
        manifest.verify(&profile.file)?;
        home.verify(&profile.file, OsStr::new("home"))?;
        profile.verify(&root.file, OsStr::new(&key))?;
        root_manifest.verify(&root.file)?;
        root.verify(&self.anchor.directory, OsStr::new(ROOT))?;
        let current_slots = scan_slots(&root.file)?;
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
fn create_profile(
    root: &File,
    key: &str,
    root_identity: &RootIdentity,
    binding: StableBinding,
) -> Result<Directory, EngineError> {
    staging::sweep(root, STAGING)?;
    let staged = Staging::create(root, STAGING)?;
    let home = Directory::create(&staged.directory.file, OsStr::new("home"))?;
    step()?;
    require_empty_home(&home.file)?;
    let mut manifest = Manifest::open(&staged.directory.file, true)?;
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
        &staged.directory.file,
        serde_json::to_vec(&expected).map_err(|_| failure("native_profile_identity_invalid"))?,
    )?;
    home.file.sync_all().map_err(unsafe_custody)?;
    step()?;
    let profile = staged.commit(root, key)?;
    assert_eq!(profile.identity, expected.directory);
    Ok(profile)
}

fn prepare_profile(
    profile: &Directory,
    root: &RootIdentity,
    binding: StableBinding,
) -> Result<(Directory, Manifest), EngineError> {
    let home = Directory::open(&profile.file, OsStr::new("home"))?;
    let manifest = Manifest::open(&profile.file, false)?;
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
    if actual != expected {
        return Err(failure("native_profile_binding_conflict"));
    }
    require_profile_entries(&profile.file)?;
    Ok((home, manifest))
}

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

fn require_profile_entries(profile: &File) -> Result<(), EngineError> {
    let entries = Dir::read_from(profile).map_err(unsafe_custody)?;
    let mut identity = false;
    let mut home = false;
    for (index, entry) in entries.enumerate() {
        let entry = entry.map_err(unsafe_custody)?;
        if index > 3 {
            return Err(failure("native_profile_custody_unsafe"));
        }
        match entry.file_name().to_bytes() {
            b"." | b".." => {}
            b"identity.json" if !identity => identity = true,
            b"home" if !home => home = true,
            _ => return Err(failure("native_profile_custody_unsafe")),
        }
    }
    if !identity || !home {
        return Err(failure("native_profile_incomplete"));
    }
    Ok(())
}

fn remember_profile(pins: &mut BTreeMap<String, String>, key: String, hash: String) {
    pins.insert(key, hash);
    assert!(pins.len() <= MAX_PROFILES);
}

#[cfg(test)]
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

#[cfg(test)]
mod use_tests;

#[cfg(test)]
mod configured_rcli_tests;
