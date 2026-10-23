use crate::config::{ModelAuthentication, ModelConnection, ModelEndpoint};
use crate::protocol::{CredentialRef, ProjectId, ProtocolError, RequestId, SessionId};
use rustix::fs::{Mode, OFlags, open};
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::fs::{self, File, Metadata};
use std::io::{Read, Seek, SeekFrom};
use std::os::unix::fs::MetadataExt;
use std::os::unix::process::CommandExt;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};

const CONFIG_BYTES_MAX: u64 = 65_536;
const ARTIFACT_BYTES_MAX: u64 = 536_870_912;

#[cfg(test)]
#[path = "configured_selection_tests.rs"]
mod configured_selection_tests;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RcliMasterBinding {
    pub project_id: ProjectId,
    pub session_id: SessionId,
    pub instance_id: RequestId,
    pub connection: ModelConnection,
    pub target_profile_ref: CredentialRef,
}

#[derive(Clone, Debug)]
pub struct RcliMasterLaunch {
    pub binding: RcliMasterBinding,
    pub executable: PathBuf,
    pub artifact_sha256: String,
    pub home_dir: PathBuf,
    pub cwd: PathBuf,
    pub profile_config_sha256: String,
}

#[derive(Deserialize)]
struct ProfileConfig {
    model: ProfileModel,
    providers: Vec<ProfileProvider>,
    expert: Option<ExpertMode>,
}

#[derive(Deserialize)]
struct ProfileModel {
    provider: String,
    model: String,
}

#[derive(Deserialize)]
struct ExpertMode {
    #[serde(default)]
    enabled: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProfileProvider {
    id: String,
    name: String,
    protocol: String,
    base_url: String,
    enabled: Option<bool>,
    auth: Option<ProfileAuth>,
    models: Option<Vec<ProfileProviderModel>>,
    headers: Option<Value>,
    extra_args: Option<Value>,
    models_path: Option<String>,
    discover_models: Option<bool>,
}

#[derive(Deserialize)]
struct ProfileAuth {
    #[serde(rename = "type")]
    kind: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProfileProviderModel {
    id: String,
    enabled: Option<bool>,
    extra_args: Option<Value>,
}

pub(super) fn error(code: &str) -> ProtocolError {
    ProtocolError(code.into())
}

impl RcliMasterLaunch {
    pub fn command(&self) -> Result<Command, ProtocolError> {
        self.validate_paths()?;
        pinned_file(&self.executable, &self.artifact_sha256, true)?;
        self.validate_profile()?;
        let mut command = Command::new(&self.executable);
        command
            .args(["--session-id", self.binding.session_id.as_str()])
            .args(["--instance-id", self.binding.instance_id.as_str()])
            .arg("--home-dir")
            .arg(&self.home_dir)
            .arg("--cwd")
            .arg(&self.cwd)
            .env_clear()
            .env("HOME", &self.home_dir)
            .current_dir(&self.cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);
        assert_eq!(command.get_program(), self.executable.as_os_str());
        assert_eq!(command.get_args().count(), 8);
        Ok(command)
    }

    fn validate_paths(&self) -> Result<(), ProtocolError> {
        let code = "rcli_master_paths_rejected";
        private_directory(&self.home_dir)?;
        private_directory(&self.cwd)?;
        if self.home_dir.starts_with(&self.cwd) || self.cwd.starts_with(&self.home_dir) {
            return Err(error(code));
        }
        let anchor = fs::symlink_metadata(self.cwd.join(".git")).map_err(|_| error(code))?;
        if anchor.uid() != rustix::process::geteuid().as_raw()
            || !anchor.is_file() && !anchor.is_dir()
        {
            return Err(error(code));
        }
        if self.executable.starts_with(&self.cwd) || self.executable.starts_with(&self.home_dir) {
            return Err(error(code));
        }
        assert_ne!(self.cwd, self.home_dir);
        assert!(!anchor.file_type().is_symlink());
        Ok(())
    }

    pub(super) fn validate_profile(&self) -> Result<(), ProtocolError> {
        checked_profile_config(
            &self.home_dir,
            &self.binding.connection,
            Some(&self.profile_config_sha256),
        )?;
        if matches!(self.binding.connection.auth, ModelAuthentication::ApiKey { .. }) {
            let path = self.home_dir.join(".backboard/keys.json");
            let metadata = fs::symlink_metadata(&path)
                .map_err(|_| error("rcli_master_profile_mismatch"))?;
            validate_file(&path, &metadata, false)
                .map_err(|_| error("rcli_master_profile_mismatch"))?;
        }
        Ok(())
    }
}

/// Projects a logical connection into the supported isolated SDK selection.
/// Takes the complete connection without configuration or project revisions.
/// Returns a physical selection while retaining the original connection.
pub(crate) fn configured_sdk_connection(
    connection: &ModelConnection,
) -> Result<ModelConnection, ProtocolError> {
    let mut selection = connection.clone();
    match connection.protocol.as_str() {
        "openai-compatible" => {
            let bytes = serde_json::to_vec(connection)
                .map_err(|_| error("rcli_master_profile_mismatch"))?;
            selection.provider = CredentialRef::new(hash_bytes(&bytes))?;
            selection.protocol = CredentialRef::new("openai-chat")?;
            assert_eq!(selection.provider.as_str().len(), 64);
        }
        "openai-chat" | "openai-responses" | "anthropic-messages" => {}
        _ => return Err(error("rcli_master_profile_mismatch")),
    }
    assert_eq!(selection.id, connection.id);
    assert_eq!(selection.account_ref, connection.account_ref);
    Ok(selection)
}

pub(crate) fn checked_configured_profile(
    home: &Path,
    connection: &ModelConnection,
) -> Result<String, ProtocolError> {
    private_directory(home)?;
    private_directory(&home.join(".backboard"))?;
    checked_profile_config(home, connection, None)
}

fn checked_profile_config(
    home: &Path,
    connection: &ModelConnection,
    expected: Option<&str>,
) -> Result<String, ProtocolError> {
    let code = "rcli_master_profile_changed";
    let path = home.join(".backboard/config.json");
    let (mut file, digest) = match expected {
        Some(expected) => (pinned_file(&path, expected, false)?, expected.to_owned()),
        None => checked_file(&path, false)?,
    };
    let metadata = file.metadata().map_err(|_| error(code))?;
    file.seek(SeekFrom::Start(0)).map_err(|_| error(code))?;
    let mut bytes = Vec::new();
    file.by_ref()
        .take(CONFIG_BYTES_MAX + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| error(code))?;
    if bytes.len() as u64 > CONFIG_BYTES_MAX
        || hash_bytes(&bytes) != digest
        || stamp(&file.metadata().map_err(|_| error(code))?) != stamp(&metadata)
        || stamp(&fs::symlink_metadata(&path).map_err(|_| error(code))?) != stamp(&metadata)
    {
        return Err(error(code));
    }
    let profile: ProfileConfig = serde_json::from_slice(&bytes)
        .map_err(|_| error("rcli_master_profile_mismatch"))?;
    let selection = configured_sdk_connection(connection)?;
    validate_selection(&profile, &selection)?;
    assert!(bytes.len() as u64 <= CONFIG_BYTES_MAX);
    assert_eq!(profile.model.model, connection.model.id);
    Ok(digest)
}

fn private_directory(path: &Path) -> Result<(), ProtocolError> {
    let code = "rcli_master_paths_rejected";
    canonical_path(path).map_err(|_| error(code))?;
    let metadata = fs::symlink_metadata(path).map_err(|_| error(code))?;
    if !metadata.is_dir()
        || metadata.uid() != rustix::process::geteuid().as_raw()
        || metadata.mode() & 0o077 != 0
    {
        return Err(error(code));
    }
    assert!(metadata.is_dir());
    assert!(!metadata.file_type().is_symlink());
    Ok(())
}

fn canonical_path(path: &Path) -> Result<(), ProtocolError> {
    if !path.is_absolute()
        || path.parent().is_none()
        || path.as_os_str().len() > 4096
        || path.components().any(|part| matches!(part, Component::CurDir | Component::ParentDir))
        || fs::canonicalize(path).map_err(|_| error("rcli_master_paths_rejected"))? != path
    {
        return Err(error("rcli_master_paths_rejected"));
    }
    assert!(path.is_absolute());
    assert!(path.parent().is_some());
    Ok(())
}

fn validate_file(path: &Path, metadata: &Metadata, executable: bool) -> Result<(), ProtocolError> {
    canonical_path(path)?;
    let maximum = if executable { ARTIFACT_BYTES_MAX } else { CONFIG_BYTES_MAX };
    let forbidden = if executable { 0o022 } else { 0o077 };
    if !metadata.is_file()
        || metadata.uid() != rustix::process::geteuid().as_raw()
        || metadata.mode() & forbidden != 0
        || executable && metadata.mode() & 0o100 == 0
        || metadata.nlink() != 1
        || !(1..=maximum).contains(&metadata.len())
    {
        return Err(error("rcli_master_file_rejected"));
    }
    assert!(!metadata.file_type().is_symlink());
    assert!(metadata.len() <= maximum);
    Ok(())
}

fn stamp(metadata: &Metadata) -> (u64, u64, u64, u32, i64, i64, i64, i64) {
    (
        metadata.dev(), metadata.ino(), metadata.len(), metadata.mode(),
        metadata.mtime(), metadata.mtime_nsec(), metadata.ctime(), metadata.ctime_nsec(),
    )
}

fn pinned_file(path: &Path, expected: &str, executable: bool) -> Result<File, ProtocolError> {
    let code = if executable { "rcli_master_artifact_changed" } else { "rcli_master_profile_changed" };
    if expected.len() != 64 || !expected.bytes().all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()) {
        return Err(error(code));
    }
    let (file, digest) = checked_file(path, executable)?;
    if digest != expected {
        return Err(error(code));
    }
    Ok(file)
}

fn checked_file(path: &Path, executable: bool) -> Result<(File, String), ProtocolError> {
    let code = if executable { "rcli_master_artifact_changed" } else { "rcli_master_profile_changed" };
    let metadata = fs::symlink_metadata(path).map_err(|_| error(code))?;
    validate_file(path, &metadata, executable).map_err(|_| error(code))?;
    let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK;
    let mut file = File::from(open(path, flags, Mode::empty()).map_err(|_| error(code))?);
    let mut digest = Sha256::new();
    let mut buffer = [0; 32_768];
    let mut total = 0_u64;
    loop {
        let count = file.read(&mut buffer).map_err(|_| error(code))?;
        if count == 0 { break; }
        total += count as u64;
        if total > metadata.len() { return Err(error(code)); }
        digest.update(&buffer[..count]);
    }
    let digest = format!("{:x}", digest.finalize());
    if total != metadata.len()
        || stamp(&file.metadata().map_err(|_| error(code))?) != stamp(&metadata)
        || stamp(&fs::symlink_metadata(path).map_err(|_| error(code))?) != stamp(&metadata)
    {
        return Err(error(code));
    }
    assert_eq!(total, metadata.len());
    assert!(file.metadata().map_err(|_| error(code))?.is_file());
    Ok((file, digest))
}

fn hash_bytes(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn validate_selection(profile: &ProfileConfig, connection: &ModelConnection) -> Result<(), ProtocolError> {
    let code = "rcli_master_profile_mismatch";
    if connection.harness.as_str() != "rcli"
        || profile.model.provider != connection.provider.as_str()
        || profile.model.model != connection.model.id
        || profile.model.model.is_empty()
        || profile.model.model.len() > 256
        || profile.expert.as_ref().is_some_and(|expert| expert.enabled)
        || profile.providers.len() != 1
    {
        return Err(error(code));
    }
    let provider = &profile.providers[0];
    let ModelEndpoint::Http { url } = &connection.endpoint else { return Err(error(code)); };
    validate_endpoint(url)?;
    if provider.id != profile.model.provider
        || provider.id.len() > 64
        || !provider.id.bytes().next().is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        || !provider.id.bytes().all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"._-".contains(&byte))
        || ["openai", "anthropic", "google", "openrouter", "gemini", "google-gemini"].contains(&provider.id.as_str())
        || provider.name.trim().is_empty()
        || provider.enabled == Some(false)
        || provider.protocol != connection.protocol.as_str()
        || !["openai-chat", "openai-responses", "anthropic-messages"].contains(&provider.protocol.as_str())
        || provider.base_url.trim_end_matches('/') != url.trim_end_matches('/')
        || provider.headers.as_ref().is_some_and(nonempty)
        || provider.extra_args.as_ref().is_some_and(nonempty)
        || provider.models_path.is_some()
        || provider.discover_models == Some(true)
    {
        return Err(error(code));
    }
    let auth = provider.auth.as_ref().map(|auth| auth.kind.as_str()).unwrap_or("apiKey");
    if !matches!((&connection.auth, auth), (ModelAuthentication::None {}, "none") | (ModelAuthentication::ApiKey { .. }, "apiKey")) {
        return Err(error(code));
    }
    if let Some(models) = &provider.models {
        let selected: Vec<_> = models.iter().filter(|model| model.id == connection.model.id).collect();
        if models.len() > 128 || selected.len() != 1 || selected[0].enabled == Some(false)
            || selected[0].extra_args.as_ref().is_some_and(nonempty)
        {
            return Err(error(code));
        }
    }
    assert_eq!(provider.id, connection.provider.as_str());
    assert_eq!(profile.model.model, connection.model.id);
    Ok(())
}

fn nonempty(value: &Value) -> bool {
    !value.is_null() && value.as_object().is_none_or(|object| !object.is_empty())
}

fn validate_endpoint(endpoint: &str) -> Result<(), ProtocolError> {
    let code = "rcli_master_profile_mismatch";
    let url = url::Url::parse(endpoint).map_err(|_| error(code))?;
    let local = match url.host() {
        Some(url::Host::Ipv4(address)) => address.is_loopback(),
        Some(url::Host::Ipv6(address)) => address.is_loopback(),
        Some(url::Host::Domain(domain)) => domain == "localhost",
        None => false,
    };
    if endpoint.len() > 2048
        || endpoint.trim() != endpoint
        || endpoint.contains("${")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.host().is_none()
        || url.scheme() != "https" && !(url.scheme() == "http" && local)
    {
        return Err(error(code));
    }
    assert!(url.host().is_some());
    assert!(url.query().is_none());
    Ok(())
}
