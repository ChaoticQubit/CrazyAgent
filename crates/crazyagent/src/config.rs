use crate::protocol::{ConfigId, CredentialRef};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub queue_capacity: usize,
    pub clients_max: usize,
    pub timeout_ms: u64,
    pub events_retained: usize,
    pub bulk_queue_capacity: usize,
    pub allowed_destinations: Vec<CredentialRef>,
    pub revoked_destinations: Vec<CredentialRef>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            queue_capacity: 32,
            clients_max: 16,
            timeout_ms: 1000,
            events_retained: 128,
            bulk_queue_capacity: 64,
            allowed_destinations: vec![],
            revoked_destinations: vec![],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Origin {
    Default,
    Global,
    Project,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum EffectTiming {
    Restart,
    NewTask,
    Immediate,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SettingScope {
    Global,
    Project,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettingDescriptor {
    pub name: String,
    pub default: serde_json::Value,
    pub value: serde_json::Value,
    pub origin: Origin,
    pub scope: SettingScope,
    pub effect_timing: EffectTiming,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EffectiveConfig {
    pub id: ConfigId,
    pub settings: Settings,
    pub descriptors: BTreeMap<String, SettingDescriptor>,
}

impl EffectiveConfig {
    pub fn validate(&self) -> Result<(), ConfigError> {
        validate_settings(&self.settings)?;
        let defaults = serde_json::to_value(Settings::default()).unwrap();
        let values = serde_json::to_value(&self.settings).unwrap();
        if self.descriptors.len() != 7
            || self.descriptors.iter().any(|(name, descriptor)| {
                descriptor.name != *name
                    || values.get(name) != Some(&descriptor.value)
                    || defaults.get(name) != Some(&descriptor.default)
            })
        {
            return Err(ConfigError("invalid_setting_metadata".into()));
        }
        let expected = from_settings(self.settings.clone(), self.descriptors.clone())?;
        if self.id != expected.id {
            return Err(ConfigError("config_identity_mismatch".into()));
        }
        assert_eq!(self.id.as_str().len(), 64);
        assert_eq!(self.descriptors.len(), 7);
        Ok(())
    }
}

#[derive(Debug)]
pub struct ConfigError(pub String);

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ConfigError {}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfigFile {
    version: Option<u16>,
    queue_capacity: Option<usize>,
    clients_max: Option<usize>,
    timeout_ms: Option<u64>,
    events_retained: Option<usize>,
    bulk_queue_capacity: Option<usize>,
    allowed_destinations: Option<Vec<CredentialRef>>,
    revoked_destinations: Option<Vec<CredentialRef>>,
}

pub fn resolve_config(global: &Path, project: &Path) -> Result<EffectiveConfig, ConfigError> {
    let mut settings = Settings::default();
    let mut origins = BTreeMap::new();
    for (path, origin) in [(global, Origin::Global), (project, Origin::Project)] {
        let file = read_config(path)?;
        apply_file(&mut settings, file, origin, &mut origins);
    }
    validate_settings(&settings)?;
    let defaults = serde_json::to_value(Settings::default()).unwrap();
    let values = serde_json::to_value(&settings).unwrap();
    let descriptors = values
        .as_object()
        .unwrap()
        .iter()
        .map(|(name, value)| {
            let origin = origins.get(name).cloned().unwrap_or(Origin::Default);
            let scope = if origin == Origin::Project {
                SettingScope::Project
            } else {
                SettingScope::Global
            };
            let effect_timing = match name.as_str() {
                "revoked_destinations" => EffectTiming::Immediate,
                "allowed_destinations" => EffectTiming::NewTask,
                _ => EffectTiming::Restart,
            };
            (
                name.clone(),
                SettingDescriptor {
                    name: name.clone(),
                    default: defaults[name].clone(),
                    value: value.clone(),
                    origin,
                    scope,
                    effect_timing,
                },
            )
        })
        .collect();
    assert!(origins.len() <= 7);
    assert_eq!(values.as_object().unwrap().len(), 7);
    from_settings(settings, descriptors)
}

fn read_config(path: &Path) -> Result<ConfigFile, ConfigError> {
    use std::io::Read;
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(ConfigFile::default()),
        Err(e) => return Err(ConfigError(e.to_string())),
    };
    if !file
        .metadata()
        .map_err(|e| ConfigError(e.to_string()))?
        .is_file()
    {
        return Err(ConfigError("configuration must be a regular file".into()));
    }
    let mut bytes = String::new();
    file.take(65537)
        .read_to_string(&mut bytes)
        .map_err(|e| ConfigError(e.to_string()))?;
    if bytes.len() > 65536 {
        return Err(ConfigError("configuration exceeds 64 KiB".into()));
    }
    let config: ConfigFile = toml::from_str(&bytes).map_err(|e| ConfigError(e.to_string()))?;
    if config.version.is_some_and(|v| v != 1) {
        return Err(ConfigError("unsupported config version".into()));
    }
    assert!(bytes.len() <= 65536);
    assert!(config.version.is_none_or(|v| v == 1));
    Ok(config)
}

fn apply_file(
    settings: &mut Settings,
    file: ConfigFile,
    origin: Origin,
    origins: &mut BTreeMap<String, Origin>,
) {
    macro_rules! apply {
        ($($name:ident),+) => {$(if let Some(value) = file.$name {
            settings.$name = value;
            origins.insert(stringify!($name).into(), origin.clone());
        })+};
    }
    apply!(
        queue_capacity,
        clients_max,
        timeout_ms,
        events_retained,
        bulk_queue_capacity,
        allowed_destinations
    );
    if let Some(revocations) = file.revoked_destinations {
        for destination in revocations {
            if !settings.revoked_destinations.contains(&destination) {
                settings.revoked_destinations.push(destination);
            }
        }
        origins.insert("revoked_destinations".into(), origin);
    }
}

pub fn validate_settings(settings: &Settings) -> Result<(), ConfigError> {
    if !(1..=1024).contains(&settings.queue_capacity)
        || !(1..=128).contains(&settings.clients_max)
        || !(10..=30000).contains(&settings.timeout_ms)
        || !(1..=256).contains(&settings.events_retained)
        || !(1..=1024).contains(&settings.bulk_queue_capacity)
        || settings.allowed_destinations.len() > 64
        || settings.revoked_destinations.len() > 64
    {
        return Err(ConfigError("setting exceeds supported bounds".into()));
    }
    assert!(settings.queue_capacity > 0);
    assert!(settings.timeout_ms > 0);
    Ok(())
}

fn from_settings(
    settings: Settings,
    descriptors: BTreeMap<String, SettingDescriptor>,
) -> Result<EffectiveConfig, ConfigError> {
    let bytes =
        serde_json::to_vec(&(&settings, &descriptors)).map_err(|e| ConfigError(e.to_string()))?;
    let id = ConfigId::new(format!("{:x}", Sha256::digest(bytes)))
        .map_err(|e| ConfigError(e.to_string()))?;
    assert_eq!(id.as_str().len(), 64);
    assert!(descriptors.len() <= 7);
    Ok(EffectiveConfig {
        id,
        settings,
        descriptors,
    })
}

pub struct ConfigState {
    pub current: EffectiveConfig,
}

impl ConfigState {
    pub fn reload(&mut self, global: &Path, project: &Path) -> Result<(), ConfigError> {
        self.current = resolve_config(global, project)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn real_file_override_and_metadata() {
        let root = tempfile::tempdir().unwrap();
        let global = root.path().join("global.toml");
        let project = root.path().join("project.toml");
        std::fs::write(
            &global,
            "version = 1\ntimeout_ms = 2500\nqueue_capacity = 8",
        )
        .unwrap();
        std::fs::write(&project, "version = 1\nqueue_capacity = 3").unwrap();
        let cfg = resolve_config(&global, &project).unwrap();
        assert_eq!(cfg.settings.queue_capacity, 3);
        assert_eq!(cfg.settings.timeout_ms, 2500);
        assert_eq!(cfg.descriptors["queue_capacity"].origin, Origin::Project);
        assert_eq!(cfg.descriptors["timeout_ms"].origin, Origin::Global);
    }

    #[test]
    fn malformed_reload_preserves_last_valid_configuration() {
        let root = tempfile::tempdir().unwrap();
        let global = root.path().join("global.toml");
        let project = root.path().join("project.toml");
        let mut state = ConfigState {
            current: resolve_config(&global, &project).unwrap(),
        };
        let original = state.current.clone();
        for invalid in [
            "queue_capacity = 0",
            "timeout_ms = -1",
            "credential_value = 'secret'",
            "version = 2",
            "bad toml",
            "allowed_destinations = ['../token']",
        ] {
            std::fs::write(&project, invalid).unwrap();
            assert!(state.reload(&global, &project).is_err(), "{invalid}");
            assert_eq!(state.current, original);
        }
    }
}
