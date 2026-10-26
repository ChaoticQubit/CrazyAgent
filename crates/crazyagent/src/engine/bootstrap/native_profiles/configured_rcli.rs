use super::*;
use crate::config::ModelEndpoint;
use std::path::{Component, PathBuf};

const CONFIG_BYTES_MAX: u64 = 65_536;
const CONFIGURATION_REQUIRED: &str = "rcli_profile_configuration_required";
const GENERATION_MAX: u64 = 9_007_199_254_740_991;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ConfiguredRcliProfileMetadata {
    pub(crate) profile_ref: CredentialRef,
    pub(crate) custody_id: String,
    pub(crate) profile_config_sha256: String,
    pub(crate) profile_generation: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfigStamp {
    identity: Identity,
    owner: u32,
    mode: u32,
    links: u64,
    length: u64,
    modified: (i64, i64),
    changed: (i64, i64),
}

impl ConfigStamp {
    fn of(metadata: &Metadata) -> Result<Self, EngineError> {
        if !safe_file(metadata) || !(1..=CONFIG_BYTES_MAX).contains(&metadata.len()) {
            return Err(failure("rcli_profile_config_unsafe"));
        }
        Ok(Self {
            identity: Identity::of(metadata),
            owner: metadata.uid(),
            mode: metadata.mode(),
            links: metadata.nlink(),
            length: metadata.len(),
            modified: (metadata.mtime(), metadata.mtime_nsec()),
            changed: (metadata.ctime(), metadata.ctime_nsec()),
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfiguredHomeIdentity {
    path: PathBuf,
    home: Identity,
    sdk: Identity,
    config: ConfigStamp,
    config_sha256: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ConfiguredProfileIdentity {
    version: u8,
    root: RootIdentity,
    pub(super) connection: ModelConnection,
    profile_ref: CredentialRef,
    directory: Identity,
    manifest: Identity,
    configured: ConfiguredHomeIdentity,
    generation: u64,
}

pub(super) struct ConfiguredRcliUse {
    anchor: Anchor,
    sdk: Directory,
    config: File,
    identity: ConfiguredHomeIdentity,
}

fn configuration_missing(error: EngineError) -> EngineError {
    if error.0 == "native_profile_incomplete" {
        failure(CONFIGURATION_REQUIRED)
    } else {
        error
    }
}

fn validate_connection(connection: &ModelConnection) -> Result<(), EngineError> {
    if connection.harness.as_str() != "rcli"
        || !matches!(
            connection.auth,
            ModelAuthentication::ApiKey { .. } | ModelAuthentication::None {}
        )
    {
        return Err(failure("rcli_profile_not_configurable"));
    }
    let ModelEndpoint::Http { url } = &connection.endpoint else {
        return Err(failure("rcli_profile_not_configurable"));
    };
    if matches!(connection.auth, ModelAuthentication::None {}) {
        let url = url::Url::parse(url).map_err(|_| failure("rcli_profile_not_configurable"))?;
        let local = match url.host() {
            Some(url::Host::Ipv4(address)) => address.is_loopback(),
            Some(url::Host::Ipv6(address)) => address.is_loopback(),
            Some(url::Host::Domain(domain)) => domain == "localhost",
            None => false,
        };
        if !local {
            return Err(failure("rcli_profile_not_configurable"));
        }
    }
    Ok(())
}

pub(super) fn lock(file: &File) -> Result<(), EngineError> {
    take_lock(file, FlockOperation::NonBlockingLockExclusive)
}

impl ConfiguredRcliUse {
    pub(super) fn capture(
        path: &Path,
        connection: &ModelConnection,
    ) -> Result<(Directory, Self), EngineError> {
        validate_connection(connection)?;
        if !path.is_absolute()
            || path.as_os_str().len() > 4096
            || path
                .components()
                .any(|part| matches!(part, Component::CurDir | Component::ParentDir))
        {
            return Err(failure("rcli_profile_home_unsafe"));
        }
        let canonical = std::fs::canonicalize(path).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                failure(CONFIGURATION_REQUIRED)
            } else {
                failure("rcli_profile_home_unsafe")
            }
        })?;
        if canonical != path {
            return Err(failure("rcli_profile_home_unsafe"));
        }
        let anchor = Anchor::open(path).map_err(unsafe_custody)?;
        let home = Directory {
            file: anchor.directory.try_clone().map_err(unsafe_custody)?,
            identity: anchor.identity,
        };
        let sdk =
            Directory::open(&home.file, OsStr::new(".backboard")).map_err(configuration_missing)?;
        let config = File::from(
            rustix::fs::openat(
                &sdk.file,
                "config.json",
                OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
                Mode::empty(),
            )
            .map_err(|error| {
                if error == rustix::io::Errno::NOENT {
                    failure(CONFIGURATION_REQUIRED)
                } else {
                    failure("rcli_profile_config_unsafe")
                }
            })?,
        );
        let stamp = ConfigStamp::of(&config.metadata().map_err(unsafe_custody)?)?;
        let mut bytes = vec![0; stamp.length as usize];
        config
            .read_exact_at(&mut bytes, 0)
            .map_err(unsafe_custody)?;
        let identity = ConfiguredHomeIdentity {
            path: anchor.path.clone(),
            home: home.identity,
            sdk: sdk.identity,
            config: stamp,
            config_sha256: digest(&bytes),
        };
        let retained = Self {
            anchor,
            sdk,
            config,
            identity,
        };
        retained.verify(&home, connection)?;
        Ok((home, retained))
    }

    pub(super) fn path(&self) -> &Path {
        &self.identity.path
    }

    fn verify_retained(&self, home: &Directory) -> Result<(), EngineError> {
        self.anchor.verify().map_err(unsafe_custody)?;
        if home.identity != self.identity.home || self.sdk.identity != self.identity.sdk {
            return Err(failure("rcli_profile_identity_changed"));
        }
        verify_named(
            &self.anchor.directory,
            OsStr::new("."),
            &home.file,
            self.identity.home,
            true,
        )?;
        self.sdk.verify(&home.file, OsStr::new(".backboard"))?;
        verify_named(
            &self.sdk.file,
            OsStr::new("config.json"),
            &self.config,
            self.identity.config.identity,
            false,
        )?;
        let named = self.identity.path.join(".backboard/config.json");
        if ConfigStamp::of(&self.config.metadata().map_err(unsafe_custody)?)?
            != self.identity.config
            || ConfigStamp::of(&std::fs::symlink_metadata(&named).map_err(unsafe_custody)?)?
                != self.identity.config
        {
            return Err(failure("rcli_profile_config_changed"));
        }
        let mut bytes = vec![0; self.identity.config.length as usize];
        self.config
            .read_exact_at(&mut bytes, 0)
            .map_err(unsafe_custody)?;
        if digest(&bytes) != self.identity.config_sha256
            || ConfigStamp::of(&self.config.metadata().map_err(unsafe_custody)?)?
                != self.identity.config
            || ConfigStamp::of(&std::fs::symlink_metadata(&named).map_err(unsafe_custody)?)?
                != self.identity.config
        {
            return Err(failure("rcli_profile_config_changed"));
        }
        self.sdk.verify(&home.file, OsStr::new(".backboard"))?;
        self.anchor.verify().map_err(unsafe_custody)
    }

    pub(super) fn verify(
        &self,
        home: &Directory,
        connection: &ModelConnection,
    ) -> Result<(), EngineError> {
        validate_connection(connection)?;
        self.verify_retained(home)?;
        let checked = crate::rcli_host::checked_configured_profile(self.path(), connection)
            .map_err(|_| failure("rcli_profile_configuration_invalid"))?;
        if checked != self.identity.config_sha256 {
            return Err(failure("rcli_profile_config_changed"));
        }
        self.verify_retained(home)
    }
}

struct LoadedProfile {
    root: Directory,
    root_manifest: Manifest,
    profile: Directory,
    manifest: Manifest,
    home: Directory,
    configured: ConfiguredRcliUse,
    key: String,
    generation: u64,
    profile_ref: CredentialRef,
}

impl LoadedProfile {
    fn metadata(&self) -> ConfiguredRcliProfileMetadata {
        ConfiguredRcliProfileMetadata {
            profile_ref: self.profile_ref.clone(),
            custody_id: digest(&self.manifest.bytes),
            profile_config_sha256: self.configured.identity.config_sha256.clone(),
            profile_generation: self.generation,
        }
    }
}

pub(super) fn require_entries(profile: &File) -> Result<(), EngineError> {
    let entries = Dir::read_from(profile).map_err(unsafe_custody)?;
    let mut identity = false;
    for (index, entry) in entries.enumerate() {
        if index > 2 {
            return Err(failure("native_profile_custody_unsafe"));
        }
        let entry = entry.map_err(unsafe_custody)?;
        match entry.file_name().to_bytes() {
            b"." | b".." => {}
            b"identity.json" if !identity => identity = true,
            _ => return Err(failure("native_profile_custody_unsafe")),
        }
    }
    if !identity {
        return Err(failure(CONFIGURATION_REQUIRED));
    }
    Ok(())
}

/// Tells whether a configured home differs from its pin only in file times, as after a
/// touch, a chmod back to 0600 or an extended attribute. Inputs: the pinned and the
/// current identity. Output: true when the path, every identity and the bytes match.
fn times_only(pinned: &ConfiguredHomeIdentity, current: &ConfiguredHomeIdentity) -> bool {
    pinned.path == current.path
        && pinned.home == current.home
        && pinned.sdk == current.sdk
        && pinned.config_sha256 == current.config_sha256
        && pinned.config.identity == current.config.identity
        && pinned.config.owner == current.config.owner
        && pinned.config.mode == current.config.mode
        && pinned.config.links == current.config.links
        && pinned.config.length == current.config.length
}

/// The profile root, its manifest and its identity while a registration holds the lock.
pub(super) struct LockedRoot<'a> {
    pub(super) root: &'a Directory,
    pub(super) manifest: &'a Manifest,
    pub(super) identity: &'a RootIdentity,
}

impl NativeProfileCustody {
    pub(super) fn read_configured_slot(
        &self,
        root: &Directory,
        root_identity: &RootIdentity,
        slots: &BTreeMap<String, Identity>,
        key: &str,
        target_profile_ref: &CredentialRef,
    ) -> Result<(Directory, Manifest, ConfiguredProfileIdentity), EngineError> {
        let profile =
            Directory::open(&root.file, OsStr::new(key)).map_err(configuration_missing)?;
        let manifest = Manifest::open(&profile.file, false).map_err(configuration_missing)?;
        let actual: ConfiguredProfileIdentity = serde_json::from_slice(&manifest.bytes)
            .map_err(|_| failure("rcli_profile_binding_conflict"))?;
        if actual.version != 2
            || actual.root != *root_identity
            || actual.profile_ref != *target_profile_ref
            || actual.directory != profile.identity
            || actual.manifest != manifest.identity
            || actual.generation == 0
            || slots.get(key) != Some(&profile.identity)
            || self
                .profiles
                .get(key)
                .is_some_and(|hash| hash != &digest(&manifest.bytes))
        {
            return Err(failure("rcli_profile_binding_conflict"));
        }
        Ok((profile, manifest, actual))
    }

    fn load_configured_rcli(
        &self,
        host: &HostStamp,
        connection: &ModelConnection,
        target_profile_ref: &CredentialRef,
    ) -> Result<LoadedProfile, EngineError> {
        validate_connection(connection)?;
        self.verify()?;
        let (root, root_manifest, root_identity) =
            self.retained_root(host).map_err(configuration_missing)?;
        let slots = scan_slots(&root.file)?;
        if self.profiles.keys().any(|key| !slots.contains_key(key)) {
            return Err(failure("native_profile_identity_changed"));
        }
        let key = digest(target_profile_ref.as_str().as_bytes());
        let (profile, manifest, actual) =
            self.read_configured_slot(&root, &root_identity, &slots, &key, target_profile_ref)?;
        if actual.connection != *connection {
            return Err(failure("rcli_profile_binding_conflict"));
        }
        self.validate_configured_home_path(&actual.configured.path)?;
        let (home, configured) = ConfiguredRcliUse::capture(&actual.configured.path, connection)?;
        if configured.identity != actual.configured {
            return Err(failure("rcli_profile_identity_changed"));
        }
        manifest.verify(&profile.file)?;
        require_entries(&profile.file)?;
        profile.verify(&root.file, OsStr::new(&key))?;
        root_manifest.verify(&root.file)?;
        root.verify(&self.anchor.directory, OsStr::new(ROOT))?;
        self.verify()?;
        Ok(LoadedProfile {
            root,
            root_manifest,
            profile,
            manifest,
            home,
            configured,
            key,
            generation: actual.generation,
            profile_ref: actual.profile_ref,
        })
    }

    fn validate_configured_home_path(&self, home: &Path) -> Result<(), EngineError> {
        if home.starts_with(self.anchor.path.join(ROOT)) || self.anchor.path.starts_with(home) {
            return Err(failure("rcli_profile_home_unsafe"));
        }
        Ok(())
    }

    /// Registers a configured RCLI home for an owner request: creates its slot, or
    /// re-pins an existing slot when the owner changed the connection or the home path,
    /// or when only the config file times changed. Inputs: host, connection, profile
    /// reference and home. Output: the slot metadata; a swapped config or home under an
    /// unchanged connection is still refused.
    pub(crate) fn register_configured_rcli_profile(
        &mut self,
        host: &HostStamp,
        connection: &ModelConnection,
        target_profile_ref: &CredentialRef,
        configured_home: &Path,
    ) -> Result<ConfiguredRcliProfileMetadata, EngineError> {
        self.verify()?;
        self.validate_configured_home_path(configured_home)?;
        let captured = ConfiguredRcliUse::capture(configured_home, connection)?;
        let (root, root_manifest, root_identity) = self.root(host)?;
        lock(&root_manifest.file)?;
        let locked = LockedRoot {
            root: &root,
            manifest: &root_manifest,
            identity: &root_identity,
        };
        self.register_locked(host, connection, target_profile_ref, captured, &locked)
    }

    pub(super) fn register_locked(
        &mut self,
        host: &HostStamp,
        connection: &ModelConnection,
        target_profile_ref: &CredentialRef,
        (home, configured): (Directory, ConfiguredRcliUse),
        locked: &LockedRoot<'_>,
    ) -> Result<ConfiguredRcliProfileMetadata, EngineError> {
        let root = locked.root;
        staging::sweep(&root.file, STAGING)?;
        let slots = scan_slots(&root.file)?;
        if self.profiles.keys().any(|key| !slots.contains_key(key)) {
            return Err(failure("native_profile_identity_changed"));
        }
        let key = digest(target_profile_ref.as_str().as_bytes());
        let (profile, manifest, previous) = if slots.contains_key(&key) {
            let (profile, manifest, actual) =
                self.read_configured_slot(root, locked.identity, &slots, &key, target_profile_ref)?;
            if actual.connection == *connection && actual.configured == configured.identity {
                let loaded = self.load_configured_rcli(host, connection, target_profile_ref)?;
                if loaded.configured.identity != configured.identity {
                    return Err(failure("rcli_profile_binding_conflict"));
                }
                let metadata = loaded.metadata();
                remember_profile(&mut self.profiles, key, metadata.custody_id.clone());
                return Ok(metadata);
            }
            if actual.connection == *connection
                && actual.configured.path == configured.identity.path
                && !times_only(&actual.configured, &configured.identity)
            {
                return Err(failure(
                    if actual.configured.config_sha256 != configured.identity.config_sha256 {
                        "rcli_profile_config_changed"
                    } else {
                        "rcli_profile_identity_changed"
                    },
                ));
            }
            lock(&manifest.file)?;
            (Some(profile), Some(manifest), Some(actual))
        } else {
            if slots.len() >= MAX_PROFILES {
                return Err(failure("native_profile_capacity"));
            }
            (None, None, None)
        };
        lock(&home.file)?;
        configured.verify(&home, connection)?;
        let generation = match &previous {
            Some(actual) => actual
                .generation
                .checked_add(1)
                .filter(|next| *next <= GENERATION_MAX)
                .ok_or_else(|| failure("rcli_profile_generation_exhausted"))?,
            None => 1,
        };
        let staged = Staging::create(&root.file, STAGING)?;
        let mut written = Manifest::open(&staged.directory.file, true)?;
        step()?;
        let identity = ConfiguredProfileIdentity {
            version: 2,
            root: locked.identity.clone(),
            connection: connection.clone(),
            profile_ref: target_profile_ref.clone(),
            directory: profile
                .as_ref()
                .map_or(staged.directory.identity, |profile| profile.identity),
            manifest: written.identity,
            configured: configured.identity.clone(),
            generation,
        };
        written.write(
            &staged.directory.file,
            serde_json::to_vec(&identity)
                .map_err(|_| failure("native_profile_identity_invalid"))?,
        )?;
        step()?;
        let profile = match profile {
            Some(profile) => {
                staged.replace_into(&root.file, MANIFEST, &profile.file)?;
                profile
            }
            None => staged.commit(&root.file, &key)?,
        };
        drop(manifest);
        assert_eq!(profile.identity, identity.directory);
        configured.verify(&home, connection)?;
        require_entries(&profile.file)?;
        written.verify(&profile.file)?;
        profile.verify(&root.file, OsStr::new(&key))?;
        locked.manifest.verify(&root.file)?;
        root.verify(&self.anchor.directory, OsStr::new(ROOT))?;
        let current_slots = scan_slots(&root.file)?;
        if current_slots.get(&key) != Some(&profile.identity)
            || slots
                .iter()
                .any(|(name, identity)| current_slots.get(name) != Some(identity))
        {
            return Err(failure("native_profile_identity_changed"));
        }
        self.verify()?;
        let metadata = ConfiguredRcliProfileMetadata {
            profile_ref: target_profile_ref.clone(),
            custody_id: digest(&written.bytes),
            profile_config_sha256: configured.identity.config_sha256.clone(),
            profile_generation: identity.generation,
        };
        remember_profile(&mut self.profiles, key, metadata.custody_id.clone());
        Ok(metadata)
    }

    pub(crate) fn configured_rcli_profile(
        &mut self,
        host: &HostStamp,
        connection: &ModelConnection,
        target_profile_ref: &CredentialRef,
    ) -> Result<ConfiguredRcliProfileMetadata, EngineError> {
        let loaded = self.load_configured_rcli(host, connection, target_profile_ref)?;
        let metadata = loaded.metadata();
        self.root_hash = Some(digest(&loaded.root_manifest.bytes));
        remember_profile(
            &mut self.profiles,
            loaded.key.clone(),
            metadata.custody_id.clone(),
        );
        Ok(metadata)
    }

    pub(crate) fn acquire_configured_rcli_use(
        &mut self,
        host: &HostStamp,
        binding: &NativeProfileBinding,
        target_profile_ref: &CredentialRef,
        operation_id: &RequestId,
        generation: u64,
    ) -> Result<NativeProfileUse, EngineError> {
        assert!(generation > 0);
        assert_eq!(host.db_instance, binding.database_instance);
        assert_eq!(host.engine_epoch, binding.engine_epoch);
        let loaded = self.load_configured_rcli(host, &binding.connection, target_profile_ref)?;
        if digest(&loaded.manifest.bytes) != binding.custody_id {
            return Err(failure("native_profile_binding_conflict"));
        }
        let guard = NativeProfileUse {
            custody: self.retain()?,
            root: loaded.root,
            root_manifest: loaded.root_manifest,
            profile: loaded.profile,
            home: loaded.home,
            manifest: loaded.manifest,
            configured: Some(loaded.configured),
            key: loaded.key,
            host: host.clone(),
            binding: binding.clone(),
            operation_id: operation_id.clone(),
            generation,
            purpose: NativeProfileUsePurpose::Execution,
        };
        guard.revalidate_retained()?;
        guard.purpose.lock(&guard.manifest.file)?;
        guard.purpose.lock(&guard.home.file)?;
        guard.revalidate_retained()?;
        self.root_hash = Some(digest(&guard.root_manifest.bytes));
        remember_profile(
            &mut self.profiles,
            guard.key.clone(),
            binding.custody_id.clone(),
        );
        Ok(guard)
    }
}
