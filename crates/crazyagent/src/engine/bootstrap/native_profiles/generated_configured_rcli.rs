use super::configured_rcli::{
    ConfiguredProfileIdentity, ConfiguredRcliUse, LockedRoot, validate_connection,
};
use super::*;
use crate::engine::owner::AccountSecret;
use rustix::fs::RenameFlags;
use std::io::Write;

pub(super) const HOMES: &str = crate::engine::bootstrap::CONFIGURED_SDK_HOMES_FOLDER;
pub(super) const CONFIG: &str = "config.json";

/// Writes the generated SDK config as a new private file inside a staging directory.
/// Inputs: the staging directory and the config bytes. Output: Ok once the file holds
/// exactly those bytes and is synced; nothing outside the staging directory changes.
fn write_config(directory: &File, bytes: &[u8]) -> Result<(), EngineError> {
    let mut config = File::from(
        rustix::fs::openat(
            directory,
            CONFIG,
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::from_raw_mode(0o600),
        )
        .map_err(unsafe_custody)?,
    );
    step()?;
    config.write_all(bytes).map_err(unsafe_custody)?;
    config.sync_all().map_err(unsafe_custody)?;
    step()?;
    directory.sync_all().map_err(unsafe_custody)
}

/// Accepts a generated SDK home that has no profile slot yet, as a registration refused
/// after creating it leaves, only when it holds exactly the engine's own config for this
/// connection. Inputs: the homes directory, the key and the expected config bytes.
/// Output: Ok to register it; any other content is refused and kept.
fn require_generated(homes: &Directory, key: &str, bytes: &[u8]) -> Result<(), EngineError> {
    let unsafe_home = || failure("native_profile_custody_unsafe");
    let home = Directory::open(&homes.file, OsStr::new(key))?;
    let sdk = Directory::open(&home.file, OsStr::new(".backboard")).map_err(|_| unsafe_home())?;
    for (directory, only) in [
        (&home.file, &b".backboard"[..]),
        (&sdk.file, &b"config.json"[..]),
    ] {
        for entry in Dir::read_from(directory).map_err(unsafe_custody)? {
            let entry = entry.map_err(unsafe_custody)?;
            let name = entry.file_name().to_bytes();
            if !matches!(name, b"." | b"..") && name != only {
                return Err(unsafe_home());
            }
        }
    }
    let config = File::from(
        rustix::fs::openat(
            &sdk.file,
            CONFIG,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
            Mode::empty(),
        )
        .map_err(|_| unsafe_home())?,
    );
    let metadata = config.metadata().map_err(unsafe_custody)?;
    verify_named(
        &sdk.file,
        OsStr::new(CONFIG),
        &config,
        Identity::of(&metadata),
        false,
    )?;
    if metadata.len() != bytes.len() as u64 {
        return Err(unsafe_home());
    }
    let mut actual = vec![0; bytes.len()];
    config
        .read_exact_at(&mut actual, 0)
        .map_err(unsafe_custody)?;
    if actual != bytes {
        return Err(unsafe_home());
    }
    sdk.verify(&home.file, OsStr::new(".backboard"))?;
    home.verify(&homes.file, OsStr::new(key))
}

/// Accepts a slot-less generated SDK home that holds the engine's config for the
/// connection. A home holding exactly the config the engine wrote before DEC-150 addendum
/// 9a is rewritten to the current one first. Inputs: the homes directory, the key, the
/// current bytes and the old ones, if any. Output: Ok to register it; else refused.
fn accept_generated(
    homes: &Directory,
    key: &str,
    bytes: &[u8],
    legacy: Option<&[u8]>,
) -> Result<(), EngineError> {
    let refused = match require_generated(homes, key, bytes) {
        Ok(()) => return Ok(()),
        Err(refused) => refused,
    };
    match legacy {
        Some(legacy) if require_generated(homes, key, legacy).is_ok() => {
            rewrite_config(homes, key, bytes)?;
            require_generated(homes, key, bytes)
        }
        _ => Err(refused),
    }
}

/// Replaces the config of a generated SDK home with the engine's bytes through staging.
/// Inputs: the homes directory, the home's key and the bytes. Output: Ok once the home
/// and its SDK directory are still the ones opened; nothing else in the home changes.
fn rewrite_config(homes: &Directory, key: &str, bytes: &[u8]) -> Result<(), EngineError> {
    let home = Directory::open(&homes.file, OsStr::new(key))?;
    let sdk = Directory::open(&home.file, OsStr::new(".backboard"))?;
    let staged = Staging::create(&homes.file, STAGING)?;
    write_config(&staged.directory.file, bytes)?;
    staged.replace_into(&homes.file, CONFIG, &sdk.file)?;
    sdk.verify(&home.file, OsStr::new(".backboard"))?;
    home.verify(&homes.file, OsStr::new(key))
}

/// A changed connection's config swapped into a generated SDK home, with the file it
/// replaced kept in staging until the save is known to have registered or not.
struct Swapped {
    staged: Staging,
    sdk: Directory,
    replaced: bool,
}

impl Swapped {
    /// Puts the replaced config back, the same file, so the slot's pin on it still holds;
    /// with nothing replaced, takes the new config out again. No input. Output: Ok once
    /// the SDK directory names what it named before the swap.
    fn undo(&self) -> Result<(), EngineError> {
        let staged = &self.staged.directory.file;
        let (from, to, flags) = if self.replaced {
            (staged, &self.sdk.file, RenameFlags::EXCHANGE)
        } else {
            (&self.sdk.file, staged, RenameFlags::NOREPLACE)
        };
        rustix::fs::renameat_with(from, CONFIG, to, CONFIG, flags).map_err(unsafe_custody)?;
        self.sdk.file.sync_all().map_err(unsafe_custody)?;
        staged.sync_all().map_err(unsafe_custody)
    }

    /// Ends a swap once the save's outcome is known: keeps the new config, or first puts
    /// the replaced one back, then removes the staging directory. Inputs: the homes
    /// directory and whether to keep the new config. Output: none; a staging directory a
    /// failure here leaves, the replaced config in it after a failed undo included, goes
    /// at the next sweep.
    fn settle(self, homes: &File, keep: bool) {
        if keep || self.undo().is_ok() {
            let _ = self.staged.discard(homes);
        }
    }
}

/// Swaps the engine's config for a changed connection into a generated SDK home in one
/// rename, keeping the file it replaces in staging (RV-DS241-50). Inputs: the homes
/// directory, the home the caller holds locked, its key and the bytes. Output: the swap,
/// or the failure with the home's config as it was.
fn swap_config(
    homes: &Directory,
    home: &Directory,
    key: &str,
    bytes: &[u8],
) -> Result<Swapped, EngineError> {
    let sdk = Directory::open(&home.file, OsStr::new(".backboard"))?;
    let staged = Staging::create(&homes.file, STAGING)?;
    write_config(&staged.directory.file, bytes)?;
    let replaced = staged.exchange(CONFIG, &sdk.file)?;
    let swapped = Swapped {
        staged,
        sdk,
        replaced,
    };
    let checked = (|| {
        swapped.sdk.file.sync_all().map_err(unsafe_custody)?;
        let staged = &swapped.staged;
        staged.directory.file.sync_all().map_err(unsafe_custody)?;
        step()?;
        staged.verify_in(&homes.file)?;
        swapped.sdk.verify(&home.file, OsStr::new(".backboard"))?;
        home.verify(&homes.file, OsStr::new(key))
    })();
    match checked {
        Ok(()) => Ok(swapped),
        Err(error) => {
            swapped.settle(&homes.file, false);
            Err(error)
        }
    }
}

/// Tells whether an account's slot pins the connection a failed save was for, as when the
/// save stopped only after its new manifest was in place. Inputs: the slot and the
/// connection. Output: true only for a readable manifest naming that connection.
fn pins(profile: &Directory, connection: &ModelConnection) -> bool {
    Manifest::open(&profile.file, false)
        .ok()
        .and_then(|manifest| {
            serde_json::from_slice::<ConfiguredProfileIdentity>(&manifest.bytes).ok()
        })
        .is_some_and(|identity| identity.connection == *connection)
}

/// Builds the SDK config the engine generates for a configured RCLI connection, checked as
/// a written config is checked so nothing the custody would refuse is ever written
/// (RV-DS241-50), and the one it wrote before a keyed connection under an rcli builtin
/// provider id got a hashed id (DEC-150 addendum 9a). Inputs: the connection and its auth
/// type. Output: the bytes, and the old bytes when the hashed provider id is their only
/// difference.
fn generated_config(
    connection: &ModelConnection,
    auth: &str,
) -> Result<(Vec<u8>, Option<Vec<u8>>), EngineError> {
    use crate::config::ModelEndpoint;
    if connection.harness.as_str() != "rcli" {
        return Err(failure("account_profile_harness_unsupported"));
    }
    let ModelEndpoint::Http { url } = &connection.endpoint else {
        return Err(failure("account_profile_endpoint_unsupported"));
    };
    validate_connection(connection)?;
    let selection = crate::rcli_host::configured_sdk_connection(connection)
        .map_err(|_| failure("account_profile_config_invalid"))?;
    let config = |provider: &CredentialRef| -> Result<Vec<u8>, EngineError> {
        let bytes = serde_json::to_vec(&serde_json::json!({
            "model": { "provider": provider.as_str(), "model": connection.model.id },
            "providers": [{
                "id": provider.as_str(), "name": "Configured provider",
                "protocol": selection.protocol.as_str(), "baseUrl": url,
                "auth": { "type": auth }
            }]
        }))
        .map_err(|_| failure("account_profile_config_invalid"))?;
        if bytes.is_empty() || bytes.len() > 65_536 {
            return Err(failure("account_profile_config_invalid"));
        }
        Ok(bytes)
    };
    let legacy =
        selection.protocol == connection.protocol && selection.provider != connection.provider;
    let old = legacy.then(|| config(&connection.provider)).transpose()?;
    let bytes = config(&selection.provider)?;
    crate::rcli_host::checked_configured_config(&bytes, connection)
        .map_err(|_| failure("rcli_profile_configuration_invalid"))?;
    assert!(old.as_ref().is_none_or(|old| *old != bytes));
    Ok((bytes, old))
}

impl NativeProfileCustody {
    /// Returns the SDK home this custody generates for a configured RCLI profile reference.
    /// Input: the account setup profile reference. Output: the engine-owned home path,
    /// which may not exist until account setup has configured that profile.
    pub(crate) fn generated_configured_home(
        &self,
        profile_ref: &CredentialRef,
    ) -> std::path::PathBuf {
        let home = self
            .anchor
            .path
            .join(HOMES)
            .join(digest(profile_ref.as_str().as_bytes()));
        assert!(home.starts_with(&self.anchor.path));
        home
    }

    fn homes(&self) -> Result<Directory, EngineError> {
        match rustix::fs::statat(&self.anchor.directory, HOMES, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(_) => Directory::open(&self.anchor.directory, OsStr::new(HOMES)),
            Err(rustix::io::Errno::NOENT) => {
                Directory::create(&self.anchor.directory, OsStr::new(HOMES))
            }
            Err(error) => Err(unsafe_custody(error)),
        }
    }

    pub(crate) fn prepare_configured_rcli_profile(
        &mut self,
        host: &HostStamp,
        connection: &ModelConnection,
        profile_ref: &CredentialRef,
        secret: Option<&AccountSecret>,
    ) -> Result<ConfiguredRcliProfileMetadata, EngineError> {
        let auth = match (&connection.auth, secret.is_some()) {
            (ModelAuthentication::ApiKey { .. }, true) => "apiKey",
            (ModelAuthentication::None {}, false) => "none",
            _ => return Err(failure("account_profile_secret_required_or_unexpected")),
        };
        let (bytes, legacy) = generated_config(connection, auth)?;
        self.verify()?;
        let (root, root_manifest, root_identity) = self.root(host)?;
        let _root_lock = lock(&root_manifest.file)?;
        let key = digest(profile_ref.as_str().as_bytes());
        let slots = scan_slots(&root.file)?;
        let homes = self.homes()?;
        staging::sweep(&homes.file, STAGING)?;
        let mut held = Vec::new();
        let mut swapped = None;
        if slots.contains_key(&key) {
            let (profile, manifest, actual) =
                self.read_configured_slot(&root, &root_identity, &slots, &key, profile_ref)?;
            held.push(lock(&manifest.file)?);
            manifest.verify(&profile.file)?;
            drop(manifest);
            if actual.connection != *connection {
                let home = Directory::open(&homes.file, OsStr::new(&key))?;
                held.push(lock(&home.file)?);
                swapped = Some((swap_config(&homes, &home, &key, &bytes)?, profile));
            }
        } else if slots.len() >= MAX_PROFILES {
            return Err(failure("native_profile_capacity"));
        } else if rustix::fs::statat(&homes.file, key.as_str(), AtFlags::SYMLINK_NOFOLLOW).is_ok() {
            let home = Directory::open(&homes.file, OsStr::new(&key))?;
            held.push(lock(&home.file)?);
            accept_generated(&homes, &key, &bytes, legacy.as_deref())?;
        } else {
            let staged = Staging::create(&homes.file, STAGING)?;
            let sdk = Directory::create(&staged.directory.file, OsStr::new(".backboard"))?;
            step()?;
            write_config(&sdk.file, &bytes)?;
            staged.commit(&homes.file, &key)?;
        }
        let locked = LockedRoot {
            root: &root,
            manifest: &root_manifest,
            identity: &root_identity,
            held: &held,
        };
        let path = self.anchor.path.join(HOMES).join(&key);
        let registered = homes
            .verify(&self.anchor.directory, OsStr::new(HOMES))
            .and_then(|()| ConfiguredRcliUse::capture(&path, connection))
            .and_then(|captured| {
                self.register_locked(host, connection, profile_ref, captured, &locked)
            });
        if let Some((swapped, profile)) = swapped {
            swapped.settle(
                &homes.file,
                registered.is_ok() || pins(&profile, connection),
            );
        }
        registered
    }

    /// Repairs, before a master turn registers it, the generated SDK home a keyed account
    /// saved before DEC-150 addendum 9a left: one holding exactly the engine's old config
    /// naming the builtin provider id gets the current config. Inputs: host, connection and
    /// the account's profile reference. Output: Ok; any other home, or a connection the
    /// engine generates no config for, is left as it is for registration to judge. The
    /// rewrite holds the home's lock, so it never runs under a configured use (RV-DS241-51).
    pub(crate) fn repair_legacy_configured_home(
        &mut self,
        host: &HostStamp,
        connection: &ModelConnection,
        profile_ref: &CredentialRef,
    ) -> Result<(), EngineError> {
        let auth = match connection.auth {
            ModelAuthentication::ApiKey { .. } => "apiKey",
            ModelAuthentication::None {} => "none",
            _ => return Ok(()),
        };
        let Ok((bytes, Some(legacy))) = generated_config(connection, auth) else {
            return Ok(());
        };
        self.verify()?;
        let (_, root_manifest, _) = self.root(host)?;
        let _root_lock = lock(&root_manifest.file)?;
        let key = digest(profile_ref.as_str().as_bytes());
        let homes = self.homes()?;
        staging::sweep(&homes.file, STAGING)?;
        if require_generated(&homes, &key, &legacy).is_ok() {
            let home = Directory::open(&homes.file, OsStr::new(&key))?;
            let _home_lock = lock(&home.file)?;
            if require_generated(&homes, &key, &legacy).is_ok() {
                rewrite_config(&homes, &key, &bytes)?;
                require_generated(&homes, &key, &bytes)?;
            }
            home.verify(&homes.file, OsStr::new(&key))?;
        }
        homes.verify(&self.anchor.directory, OsStr::new(HOMES))
    }
}
