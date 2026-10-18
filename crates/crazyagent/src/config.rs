use crate::protocol::{ConfigId, CredentialRef};
use crate::routing::{ApprovalMode, ResourceBudget, RoutingPolicy, SchedulingPolicy};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
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
    #[serde(default, skip_serializing_if = "RoutingPolicy::is_default")]
    pub routing: RoutingPolicy,
    #[serde(
        default,
        skip_serializing_if = "crate::memory::KnowledgePolicy::is_default"
    )]
    pub knowledge: crate::memory::KnowledgePolicy,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_access: Option<ModelAccess>,
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
            routing: RoutingPolicy::default(),
            knowledge: crate::memory::KnowledgePolicy::default(),
            model_access: None,
        }
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ModelAccess {
    pub accounts: Vec<CredentialRef>,
    pub connections: Vec<ModelConnection>,
    pub pools: Vec<AccountPool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelConnection {
    pub id: CredentialRef,
    pub provider: CredentialRef,
    pub harness: CredentialRef,
    pub protocol: CredentialRef,
    pub account_ref: CredentialRef,
    pub endpoint: ModelEndpoint,
    pub model: ModelReference,
    pub auth: ModelAuthentication,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelReference {
    pub id: String,
    pub author: CredentialRef,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ModelEndpoint {
    Http { url: String },
    Native {},
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "method", rename_all = "snake_case", deny_unknown_fields)]
pub enum ModelAuthentication {
    None {},
    ApiKey { credential_ref: CredentialRef },
    Oauth { credential_ref: CredentialRef },
    NativeManaged { profile_ref: CredentialRef },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccountPool {
    pub id: CredentialRef,
    pub members: Vec<CredentialRef>,
    pub projects: Vec<CredentialRef>,
    pub roles: Vec<CredentialRef>,
    pub fallback_allowed: bool,
}

impl ModelAccess {
    pub fn validate(&self) -> Result<(), ConfigError> {
        if !bounded_unique_refs(&self.accounts, false)
            || self.connections.len() > 64
            || self.pools.len() > 64
        {
            return Err(ConfigError("invalid_model_access".into()));
        }
        let connections: BTreeSet<_> = self.connections.iter().map(|c| &c.id).collect();
        let pools: BTreeSet<_> = self.pools.iter().map(|p| &p.id).collect();
        if connections.len() != self.connections.len()
            || pools.len() != self.pools.len()
            || self.connections.iter().any(|connection| {
                !self.accounts.contains(&connection.account_ref)
                    || connection.model.id.is_empty()
                    || connection.model.id.len() > 256
                    || !connection
                        .model
                        .id
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || b"-_.:/@+".contains(&byte))
                    || !connection
                        .endpoint
                        .valid_for(&connection.protocol, &connection.auth)
            })
            || self.pools.iter().any(|pool| {
                !bounded_unique_refs(&pool.members, true)
                    || !bounded_unique_refs(&pool.projects, true)
                    || !bounded_unique_refs(&pool.roles, true)
                    || pool.members.iter().any(|id| !connections.contains(id))
            })
        {
            return Err(ConfigError("invalid_model_access".into()));
        }
        Ok(())
    }
}

fn bounded_unique_refs(values: &[CredentialRef], required: bool) -> bool {
    values.len() <= 64
        && (!required || !values.is_empty())
        && values.iter().collect::<BTreeSet<_>>().len() == values.len()
}

impl ModelEndpoint {
    fn valid_for(&self, protocol: &CredentialRef, auth: &ModelAuthentication) -> bool {
        match self {
            Self::Native {} => protocol.as_str() == "native",
            Self::Http { url: value } => {
                if protocol.as_str() == "native"
                    || matches!(auth, ModelAuthentication::NativeManaged { .. })
                    || value.len() > 2048
                    || !value.contains("://")
                    || value
                        .chars()
                        .any(|c| c.is_control() || c.is_whitespace() || c == '\\')
                {
                    return false;
                }
                url::Url::parse(value).is_ok_and(|url| {
                    matches!(url.scheme(), "http" | "https")
                        && url.has_host()
                        && url.username().is_empty()
                        && url.password().is_none()
                        && url.query().is_none()
                        && url.fragment().is_none()
                })
            }
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
        let routing = self
            .descriptors
            .keys()
            .any(|name| name.starts_with("routing."));
        let knowledge = self
            .descriptors
            .keys()
            .any(|name| name.starts_with("knowledge."));
        let model_access = self.settings.model_access.is_some();
        let defaults = setting_values(&Settings::default(), routing, knowledge, model_access);
        let values = setting_values(&self.settings, routing, knowledge, model_access);
        if (!routing && !self.settings.routing.is_default())
            || (!knowledge && !self.settings.knowledge.is_default())
            || self.descriptors.len() != values.len()
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
        assert_eq!(self.descriptors.len(), values.len());
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
    routing: Option<RoutingFile>,
    knowledge: Option<KnowledgeFile>,
    model_access: Option<ModelAccess>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct KnowledgeFile {
    processing_destinations: Option<Vec<CredentialRef>>,
    text_bytes_max: Option<usize>,
    excerpt_bytes_max: Option<usize>,
    records_max: Option<usize>,
    queue_capacity: Option<usize>,
    batch_size: Option<usize>,
    asset_bytes_max: Option<usize>,
    asset_sources_max: Option<usize>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RoutingFile {
    preference_order: Option<Vec<CredentialRef>>,
    approved_accounts: Option<Vec<CredentialRef>>,
    fallback_allowed: Option<bool>,
    approval_mode: Option<ApprovalMode>,
    scheduling: Option<SchedulingPolicy>,
    priority_weight: Option<u16>,
    global_capacity: Option<ResourceBudget>,
    project_capacity: Option<ResourceBudget>,
    task_capacity: Option<ResourceBudget>,
    mailbox_capacity: Option<usize>,
}

pub fn resolve_config(global: &Path, project: &Path) -> Result<EffectiveConfig, ConfigError> {
    let mut settings = Settings::default();
    let mut origins = BTreeMap::new();
    for (path, origin) in [(global, Origin::Global), (project, Origin::Project)] {
        let file = read_config(path)?;
        if origin == Origin::Project && file.model_access.is_some() {
            return Err(ConfigError("model_access_requires_global_scope".into()));
        }
        if origin == Origin::Project
            && file
                .routing
                .as_ref()
                .is_some_and(|r| r.global_capacity.is_some() || r.scheduling.is_some())
        {
            return Err(ConfigError(
                "shared_routing_settings_require_global_scope".into(),
            ));
        }
        apply_file(&mut settings, file, origin, &mut origins);
    }
    validate_settings(&settings)?;
    let knowledge = origins.keys().any(|name| name.starts_with("knowledge."));
    let model_access = settings.model_access.is_some();
    let defaults = setting_values(&Settings::default(), true, knowledge, model_access);
    let values = setting_values(&settings, true, knowledge, model_access);
    let descriptors = values
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
                "routing.global_capacity" | "routing.scheduling" => EffectTiming::Immediate,
                "allowed_destinations" | "model_access" => EffectTiming::NewTask,
                n if n.starts_with("routing.") => EffectTiming::NewTask,
                "knowledge.processing_destinations" => EffectTiming::Immediate,
                n if n.starts_with("knowledge.") => EffectTiming::NewTask,
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
    assert!(origins.len() <= values.len());
    assert_eq!(
        values.len(),
        17 + 8 * usize::from(knowledge) + usize::from(model_access)
    );
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
    let config: ConfigFile =
        toml::from_str(&bytes).map_err(|_| ConfigError("invalid_configuration_format".into()))?;
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
    if let Some(model_access) = file.model_access {
        settings.model_access = Some(model_access);
        origins.insert("model_access".into(), origin.clone());
    }
    if let Some(revocations) = file.revoked_destinations {
        for destination in revocations {
            if !settings.revoked_destinations.contains(&destination) {
                settings.revoked_destinations.push(destination);
            }
        }
        origins.insert("revoked_destinations".into(), origin.clone());
    }
    if let Some(routing) = file.routing {
        macro_rules! apply_routing {
            ($($name:ident),+) => {$(if let Some(value) = routing.$name {
                settings.routing.$name = value;
                origins.insert(concat!("routing.", stringify!($name)).into(), origin.clone());
            })+};
        }
        apply_routing!(
            preference_order,
            approved_accounts,
            fallback_allowed,
            approval_mode,
            scheduling,
            priority_weight,
            global_capacity,
            project_capacity,
            task_capacity,
            mailbox_capacity
        );
    }
    if let Some(knowledge) = file.knowledge {
        macro_rules! apply_knowledge {
            ($($name:ident),+) => {$(if let Some(value) = knowledge.$name {
                settings.knowledge.$name = value;
                origins.insert(concat!("knowledge.", stringify!($name)).into(), origin.clone());
            })+};
        }
        apply_knowledge!(
            processing_destinations,
            text_bytes_max,
            excerpt_bytes_max,
            records_max,
            queue_capacity,
            batch_size,
            asset_bytes_max,
            asset_sources_max
        );
    }
}

fn setting_values(
    settings: &Settings,
    include_routing: bool,
    include_knowledge: bool,
    include_model_access: bool,
) -> BTreeMap<String, serde_json::Value> {
    let mut values = serde_json::to_value(settings)
        .unwrap()
        .as_object()
        .unwrap()
        .clone();
    values.remove("routing");
    values.remove("knowledge");
    values.remove("model_access");
    if include_model_access {
        values.insert(
            "model_access".into(),
            serde_json::to_value(&settings.model_access).unwrap(),
        );
    }
    if include_routing {
        let routing = serde_json::to_value(&settings.routing).unwrap();
        for (name, value) in routing.as_object().unwrap() {
            values.insert(format!("routing.{name}"), value.clone());
        }
    }
    if include_knowledge {
        let knowledge = serde_json::to_value(&settings.knowledge).unwrap();
        for (name, value) in knowledge.as_object().unwrap() {
            values.insert(format!("knowledge.{name}"), value.clone());
        }
    }
    values.into_iter().collect()
}

pub fn validate_settings(settings: &Settings) -> Result<(), ConfigError> {
    settings.routing.validate().map_err(ConfigError)?;
    settings.knowledge.validate().map_err(ConfigError)?;
    if let Some(model_access) = &settings.model_access {
        model_access.validate()?;
    }
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
    assert!(descriptors.len() <= 25 + usize::from(settings.model_access.is_some()));
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
    fn legacy_configuration_keeps_its_identity_and_default_routing() {
        let root = tempfile::tempdir().unwrap();
        let mut config = resolve_config(&root.path().join("g"), &root.path().join("p")).unwrap();
        config
            .descriptors
            .retain(|name, _| !name.starts_with("routing."));
        config.id =
            ConfigId::new("472693a858862960f058c909a411a19aa8f8d03e94240e0aa4ccb8c1ad4f3e0c")
                .unwrap();
        let mut stored = serde_json::to_value(&config).unwrap();
        assert!(stored["settings"].get("routing").is_none());
        let decoded: EffectiveConfig = serde_json::from_value(stored.clone()).unwrap();
        decoded.validate().unwrap();
        assert_eq!(decoded.settings.routing, RoutingPolicy::default());
        assert_eq!(serde_json::to_value(decoded).unwrap(), stored);
        stored["settings"]["routing"] = serde_json::to_value(RoutingPolicy {
            fallback_allowed: false,
            ..RoutingPolicy::default()
        })
        .unwrap();
        assert!(
            serde_json::from_value::<EffectiveConfig>(stored)
                .unwrap()
                .validate()
                .is_err()
        );
    }

    #[test]
    fn routing_fields_merge_with_provenance_and_shared_capacity_stays_global() {
        let root = tempfile::tempdir().unwrap();
        let global = root.path().join("g");
        let project = root.path().join("p");
        std::fs::write(&global,
            "[routing]\npreference_order=['beta','alpha']\napproved_accounts=['account']\nscheduling='StrictPriority'").unwrap();
        std::fs::write(
            &project,
            "[routing]\nmailbox_capacity=4\nfallback_allowed=false",
        )
        .unwrap();
        let config = resolve_config(&global, &project).unwrap();
        config.validate().unwrap();
        assert_eq!(config.settings.routing.preference_order[0].as_str(), "beta");
        assert_eq!(
            config.descriptors["routing.preference_order"].origin,
            Origin::Global
        );
        assert_eq!(
            config.descriptors["routing.mailbox_capacity"].origin,
            Origin::Project
        );
        assert_eq!(
            config.descriptors["routing.scheduling"].effect_timing,
            EffectTiming::Immediate
        );
        assert!(!config.settings.routing.fallback_allowed);
        std::fs::write(&project, "[routing]\nscheduling='FairShare'").unwrap();
        assert!(resolve_config(&global, &project).is_err());
        std::fs::write(&project,
            "[routing.global_capacity]\nslots=999\ncpu_millis=1000\nmemory_mib=512\nprocesses=4\noutput_bytes=1048576\nwall_time_ms=60000\nspend_microunits=0").unwrap();
        assert!(resolve_config(&global, &project).is_err());
    }

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
