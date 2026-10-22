use super::*;
use crate::engine::owner::AccountSecret;
use std::io::Write;

const HOMES: &str = "configured-sdk-homes";

impl NativeProfileCustody {
    /// Returns the SDK home this custody generates for a configured RCLI profile reference.
    /// Input: the account setup profile reference. Output: the engine-owned home path,
    /// which may not exist until account setup has configured that profile.
    pub(crate) fn generated_configured_home(&self, profile_ref: &CredentialRef) -> std::path::PathBuf {
        let home = self
            .anchor
            .path
            .join(HOMES)
            .join(digest(profile_ref.as_str().as_bytes()));
        assert!(home.starts_with(&self.anchor.path));
        home
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
        self.verify()?;
        let (root, manifest, _) = self.root(host)?;
        lock(&manifest.file)?;
        let key = digest(profile_ref.as_str().as_bytes());
        let exists = scan_slots(&root.file)?.contains_key(&key);
        if exists {
            let profile = Directory::open(&root.file, OsStr::new(&key))?;
            let identity = Manifest::open(&profile.file, false)?;
            lock(&identity.file)?;
            identity.verify(&profile.file)?;
            profile.verify(&root.file, OsStr::new(&key))?;
            return self.configured_rcli_profile(host, connection, profile_ref);
        }
        drop(manifest);
        drop(root);
        let homes = match rustix::fs::statat(&self.anchor.directory, HOMES, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(_) => Directory::open(&self.anchor.directory, OsStr::new(HOMES))?,
            Err(rustix::io::Errno::NOENT) => Directory::create(&self.anchor.directory, OsStr::new(HOMES))?,
            Err(error) => return Err(unsafe_custody(error)),
        };
        let home = Directory::create(&homes.file, OsStr::new(&key))?;
        let sdk = Directory::create(&home.file, OsStr::new(".backboard"))?;
        let bytes = serde_json::to_vec(&serde_json::json!({
            "model": { "provider": selection.provider.as_str(), "model": connection.model.id },
            "providers": [{
                "id": selection.provider.as_str(), "name": "Configured provider",
                "protocol": selection.protocol.as_str(), "baseUrl": url,
                "auth": { "type": auth }
            }]
        })).map_err(|_| failure("account_profile_config_invalid"))?;
        if bytes.is_empty() || bytes.len() > 65_536 {
            return Err(failure("account_profile_config_invalid"));
        }
        let mut config = File::from(rustix::fs::openat(
            &sdk.file, "config.json",
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::from_raw_mode(0o600),
        ).map_err(unsafe_custody)?);
        config.write_all(&bytes).map_err(unsafe_custody)?;
        config.sync_all().map_err(unsafe_custody)?;
        sdk.file.sync_all().map_err(unsafe_custody)?;
        home.verify(&homes.file, OsStr::new(&key))?;
        homes.verify(&self.anchor.directory, OsStr::new(HOMES))?;
        self.register_configured_rcli_profile(
            host, connection, profile_ref, &self.anchor.path.join(HOMES).join(&key),
        )
    }
}
