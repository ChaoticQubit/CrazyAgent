use super::configured_rcli::{ConfiguredRcliUse, LockedRoot};
use super::*;
use crate::engine::owner::AccountSecret;
use std::io::Write;

const HOMES: &str = "configured-sdk-homes";
const CONFIG: &str = "config.json";

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
        use crate::config::ModelEndpoint;
        let auth = match (&connection.auth, secret.is_some()) {
            (ModelAuthentication::ApiKey { .. }, true) => "apiKey",
            (ModelAuthentication::None {}, false) => "none",
            _ => return Err(failure("account_profile_secret_required_or_unexpected")),
        };
        if connection.harness.as_str() != "rcli" {
            return Err(failure("account_profile_harness_unsupported"));
        }
        let ModelEndpoint::Http { url } = &connection.endpoint else {
            return Err(failure("account_profile_endpoint_unsupported"));
        };
        let selection = crate::rcli_host::configured_sdk_connection(connection)
            .map_err(|_| failure("account_profile_config_invalid"))?;
        let bytes = serde_json::to_vec(&serde_json::json!({
            "model": { "provider": selection.provider.as_str(), "model": connection.model.id },
            "providers": [{
                "id": selection.provider.as_str(), "name": "Configured provider",
                "protocol": selection.protocol.as_str(), "baseUrl": url,
                "auth": { "type": auth }
            }]
        }))
        .map_err(|_| failure("account_profile_config_invalid"))?;
        if bytes.is_empty() || bytes.len() > 65_536 {
            return Err(failure("account_profile_config_invalid"));
        }
        self.verify()?;
        let (root, root_manifest, root_identity) = self.root(host)?;
        lock(&root_manifest.file)?;
        let locked = LockedRoot {
            root: &root,
            manifest: &root_manifest,
            identity: &root_identity,
        };
        let key = digest(profile_ref.as_str().as_bytes());
        let slots = scan_slots(&root.file)?;
        let homes = self.homes()?;
        staging::sweep(&homes.file, STAGING)?;
        if slots.contains_key(&key) {
            let (profile, manifest, actual) =
                self.read_configured_slot(&root, &root_identity, &slots, &key, profile_ref)?;
            lock(&manifest.file)?;
            manifest.verify(&profile.file)?;
            drop(manifest);
            if actual.connection != *connection {
                let home = Directory::open(&homes.file, OsStr::new(&key))?;
                let sdk = Directory::open(&home.file, OsStr::new(".backboard"))?;
                let staged = Staging::create(&homes.file, STAGING)?;
                write_config(&staged.directory.file, &bytes)?;
                staged.replace_into(&homes.file, CONFIG, &sdk.file)?;
                sdk.verify(&home.file, OsStr::new(".backboard"))?;
                home.verify(&homes.file, OsStr::new(&key))?;
            }
        } else if slots.len() >= MAX_PROFILES {
            return Err(failure("native_profile_capacity"));
        } else if rustix::fs::statat(&homes.file, key.as_str(), AtFlags::SYMLINK_NOFOLLOW).is_ok() {
            require_generated(&homes, &key, &bytes)?;
        } else {
            let staged = Staging::create(&homes.file, STAGING)?;
            let sdk = Directory::create(&staged.directory.file, OsStr::new(".backboard"))?;
            step()?;
            write_config(&sdk.file, &bytes)?;
            staged.commit(&homes.file, &key)?;
        }
        homes.verify(&self.anchor.directory, OsStr::new(HOMES))?;
        let path = self.anchor.path.join(HOMES).join(&key);
        let captured = ConfiguredRcliUse::capture(&path, connection)?;
        self.register_locked(host, connection, profile_ref, captured, &locked)
    }
}
