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

pub const ACCOUNTS_MAX: usize = 4096;
pub const CONFIG_BYTES_MAX: usize = 8 << 20;
/// The settings file version this engine reads and writes. A file with a higher
/// `version` came from a newer engine and is refused, never rewritten.
pub const SETTINGS_VERSION: u16 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ModelAccessEdit {
    AddAccount { account_ref: CredentialRef },
    RemoveAccount { account_ref: CredentialRef },
    CreateConnection { connection: ModelConnection },
    ReplaceConnection { connection: ModelConnection },
    RemoveConnection { id: CredentialRef },
    CreatePool { pool: AccountPool },
    ReplacePool { pool: AccountPool },
    RemovePool { id: CredentialRef },
    AddConnectedAccount { connection: ModelConnection },
    RemoveConnectedAccount { id: CredentialRef },
}

impl ModelAccessEdit {
    pub fn validate(&self) -> Result<(), ConfigError> {
        if serde_json::to_vec(self)
            .map_err(|_| ConfigError("model_access_encoding".into()))?
            .len()
            > 32768
        {
            return Err(ConfigError("model_access_edit_bounds".into()));
        }
        Ok(())
    }
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub members: Vec<CredentialRef>,
    pub projects: Vec<CredentialRef>,
    pub roles: Vec<CredentialRef>,
    pub fallback_allowed: bool,
    #[serde(default, skip_serializing_if = "ProviderQuotaPolicy::is_default")]
    pub provider_quota_policy: ProviderQuotaPolicy,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderQuotaPolicy {
    #[default]
    BoundedAttempt,
    Strict,
}

impl ProviderQuotaPolicy {
    fn is_default(&self) -> bool {
        *self == Self::BoundedAttempt
    }
}

impl ModelAccess {
    pub fn validate(&self) -> Result<(), ConfigError> {
        let accounts: BTreeSet<_> = self.accounts.iter().collect();
        if accounts.len() != self.accounts.len()
            || self.accounts.len() > ACCOUNTS_MAX
            || self.connections.len() > ACCOUNTS_MAX
            || self.pools.len() > 64
        {
            return Err(ConfigError("invalid_model_access".into()));
        }
        let connections: BTreeSet<_> = self.connections.iter().map(|c| &c.id).collect();
        let pools: BTreeSet<_> = self.pools.iter().map(|p| &p.id).collect();
        if connections.len() != self.connections.len()
            || pools.len() != self.pools.len()
            || self.connections.iter().any(|connection| {
                !accounts.contains(&connection.account_ref)
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
                    || !bounded_unique_refs(&pool.projects, false)
                    || !bounded_unique_refs(&pool.roles, false)
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
    resolve_files(read_config(global)?, read_config(project)?)
}

pub(crate) fn resolve_config_bytes(
    global: &str,
    project: &str,
) -> Result<EffectiveConfig, ConfigError> {
    resolve_files(parse_config(global)?, parse_config(project)?)
}

fn resolve_files(global: ConfigFile, project: ConfigFile) -> Result<EffectiveConfig, ConfigError> {
    let mut settings = Settings::default();
    let mut origins = BTreeMap::new();
    for (file, origin) in [(global, Origin::Global), (project, Origin::Project)] {
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
    file.take(CONFIG_BYTES_MAX as u64 + 1)
        .read_to_string(&mut bytes)
        .map_err(|e| ConfigError(e.to_string()))?;
    if bytes.len() > CONFIG_BYTES_MAX {
        return Err(ConfigError("configuration exceeds 8 MiB".into()));
    }
    parse_config(&bytes)
}

fn parse_config(bytes: &str) -> Result<ConfigFile, ConfigError> {
    if bytes.len() > CONFIG_BYTES_MAX {
        return Err(ConfigError("configuration exceeds 8 MiB".into()));
    }
    let config: ConfigFile =
        toml::from_str(bytes).map_err(|_| ConfigError("invalid_configuration_format".into()))?;
    if let Some(version) = config.version.filter(|v| *v > SETTINGS_VERSION) {
        return Err(ConfigError(format!(
            "settings_newer_than_engine: the settings file is version {version} and this \
             engine reads version {SETTINGS_VERSION}; install the newer CrazyAgent that wrote it"
        )));
    }
    if config.version.is_some_and(|v| v != SETTINGS_VERSION) {
        return Err(ConfigError("unsupported config version".into()));
    }
    assert!(bytes.len() <= CONFIG_BYTES_MAX);
    assert!(config.version.is_none_or(|v| v == SETTINGS_VERSION));
    Ok(config)
}

/// Applies one owner edit to the global settings text. `registered` carries saved accounts
/// that the edit's pool uses but the config does not hold yet; they are added first, in the
/// same publication. Inputs: global and project text, the edit and those accounts.
/// Output: the new global text and its resolved config, or the rejection code.
pub(crate) fn edit_model_access(
    global: &str,
    project: &str,
    edit: &ModelAccessEdit,
    registered: &[ModelConnection],
) -> Result<(String, EffectiveConfig), ConfigError> {
    edit.validate()?;
    let mut catalog = resolve_config_bytes(global, project)?
        .settings
        .model_access
        .unwrap_or_default();
    for connection in registered {
        if catalog.connections.iter().any(|c| c.id == connection.id) {
            continue;
        }
        if !catalog.accounts.contains(&connection.account_ref) {
            catalog.accounts.push(connection.account_ref.clone());
        }
        catalog.connections.push(connection.clone());
    }
    match edit {
        ModelAccessEdit::AddAccount { account_ref } => {
            if catalog.accounts.contains(account_ref) {
                return Err(ConfigError("account_exists".into()));
            }
            catalog.accounts.push(account_ref.clone());
        }
        ModelAccessEdit::RemoveAccount { account_ref } => {
            let index = catalog
                .accounts
                .iter()
                .position(|a| a == account_ref)
                .ok_or_else(|| ConfigError("account_not_found".into()))?;
            catalog.accounts.remove(index);
        }
        ModelAccessEdit::CreateConnection { connection } => {
            if catalog.connections.iter().any(|c| c.id == connection.id) {
                return Err(ConfigError("connection_exists".into()));
            }
            catalog.connections.push(connection.clone());
        }
        ModelAccessEdit::ReplaceConnection { connection } => {
            let value = catalog
                .connections
                .iter_mut()
                .find(|c| c.id == connection.id)
                .ok_or_else(|| ConfigError("connection_not_found".into()))?;
            *value = connection.clone();
        }
        ModelAccessEdit::RemoveConnection { id } => {
            let index = catalog
                .connections
                .iter()
                .position(|c| c.id == *id)
                .ok_or_else(|| ConfigError("connection_not_found".into()))?;
            catalog.connections.remove(index);
        }
        ModelAccessEdit::CreatePool { pool } => {
            validate_pool_edit(pool)?;
            if catalog.pools.iter().any(|p| p.id == pool.id) {
                return Err(ConfigError("pool_exists".into()));
            }
            catalog.pools.push(pool.clone());
        }
        ModelAccessEdit::ReplacePool { pool } => {
            validate_pool_edit(pool)?;
            let value = catalog
                .pools
                .iter_mut()
                .find(|p| p.id == pool.id)
                .ok_or_else(|| ConfigError("pool_not_found".into()))?;
            *value = pool.clone();
        }
        ModelAccessEdit::RemovePool { id } => {
            let index = catalog
                .pools
                .iter()
                .position(|p| p.id == *id)
                .ok_or_else(|| ConfigError("pool_not_found".into()))?;
            if !catalog.pools[index].projects.is_empty() {
                return Err(ConfigError("pool_in_use".into()));
            }
            catalog.pools.remove(index);
        }
        ModelAccessEdit::AddConnectedAccount { connection } => {
            if catalog.accounts.contains(&connection.account_ref) {
                return Err(ConfigError("account_exists".into()));
            }
            if catalog.connections.iter().any(|c| c.id == connection.id) {
                return Err(ConfigError("connection_exists".into()));
            }
            catalog.accounts.push(connection.account_ref.clone());
            catalog.connections.push(connection.clone());
        }
        ModelAccessEdit::RemoveConnectedAccount { id } => {
            let index = catalog
                .connections
                .iter()
                .position(|c| c.id == *id)
                .ok_or_else(|| ConfigError("account_not_found".into()))?;
            if catalog.pools.iter().any(|pool| pool.members.contains(id)) {
                return Err(ConfigError("account_in_pool".into()));
            }
            let removed = catalog.connections.remove(index);
            if !catalog
                .connections
                .iter()
                .any(|c| c.account_ref == removed.account_ref)
            {
                catalog
                    .accounts
                    .retain(|account| *account != removed.account_ref);
            }
        }
    }
    catalog.validate()?;
    let mut document = global
        .parse::<toml_edit::DocumentMut>()
        .map_err(|_| ConfigError("invalid_configuration_format".into()))?;
    let catalog_document = toml_edit::ser::to_document(&catalog)
        .map_err(|_| ConfigError("model_access_encoding".into()))?;
    document["model_access"] = toml_edit::Item::Table(catalog_document.into_table());
    approve_assigned_accounts(&mut document, &catalog)?;
    let bytes = document.to_string();
    let config = resolve_config_bytes(&bytes, project)?;
    assert_eq!(config.settings.model_access, Some(catalog));
    Ok((bytes, config))
}

/// The most accounts one pool may hold when it is created or changed.
pub const POOL_MEMBERS_MAX: usize = 15;
/// The most characters of a pool display name.
pub const POOL_NAME_CHARS_MAX: usize = 64;

/// Checks the owner-facing limits of a pool being created or changed.
/// Input: the proposed pool. Output: `pool_member_limit` above 15 members,
/// `pool_name_bounds` for a blank, overlong or control-character name, else Ok.
fn validate_pool_edit(pool: &AccountPool) -> Result<(), ConfigError> {
    if pool.members.len() > POOL_MEMBERS_MAX {
        return Err(ConfigError("pool_member_limit".into()));
    }
    if let Some(name) = &pool.name
        && (name.trim().is_empty()
            || name.chars().count() > POOL_NAME_CHARS_MAX
            || name.chars().any(char::is_control))
    {
        return Err(ConfigError("pool_name_bounds".into()));
    }
    assert!(pool.members.len() <= POOL_MEMBERS_MAX);
    Ok(())
}

/// Turns a pool refusal code into the sentence shown to the owner.
/// Input: an engine rejection code. Output: the plain sentence for a pool
/// refusal, or None for any other code.
pub fn plain_problem(code: &str) -> Option<&'static str> {
    match code {
        "pool_member_limit" => Some("A pool can hold up to 15 accounts"),
        "pool_in_use" => Some("This pool is used by a project. Remove it from the project first."),
        "pool_name_bounds" => {
            Some("Use a pool name of 1 to 64 characters, without control characters.")
        }
        "pool_exists" => Some("A pool with this id already exists."),
        "pool_not_found" => Some("This pool no longer exists. Refresh and try again."),
        _ => None,
    }
}

const LOCAL_DESTINATION: &str = "local";

/// Returns the profile reference that harness setup generates for an RCLI HTTP connection,
/// which is also its own master target. Input: the connection. Output: its API-key
/// credential reference, a digest-derived reference for a keyless endpoint, or none otherwise.
pub fn configured_profile_ref(connection: &ModelConnection) -> Option<CredentialRef> {
    match &connection.auth {
        ModelAuthentication::ApiKey { credential_ref } => Some(credential_ref.clone()),
        ModelAuthentication::None {} => {
            let bytes = serde_json::to_vec(connection).ok()?;
            CredentialRef::new(format!("configured-{:x}", Sha256::digest(bytes))).ok()
        }
        _ => None,
    }
}

/// Treats an explicit pool-to-project assignment as owner approval of the pool's accounts.
/// Input: the edited global document and the validated catalog it now holds.
/// Output: `routing.approved_accounts` gains every account of an assigned pool, an empty
/// `allowed_destinations` gains the local execution target and every signed-in connection
/// or RCLI HTTP connection of an assigned pool gets its own allowed master target; nothing
/// is ever removed.
fn approve_assigned_accounts(
    document: &mut toml_edit::DocumentMut,
    catalog: &ModelAccess,
) -> Result<(), ConfigError> {
    let assigned: Vec<&CredentialRef> = catalog
        .pools
        .iter()
        .filter(|pool| !pool.projects.is_empty())
        .flat_map(|pool| {
            catalog
                .connections
                .iter()
                .filter(|connection| pool.members.contains(&connection.id))
                .map(|connection| &connection.account_ref)
        })
        .collect();
    if assigned.is_empty() {
        return Ok(());
    }
    let signed_in: Vec<CredentialRef> = catalog
        .pools
        .iter()
        .filter(|pool| !pool.projects.is_empty())
        .flat_map(|pool| {
            catalog.connections.iter().filter(|connection| {
                pool.members.contains(&connection.id)
                    && matches!(connection.auth, ModelAuthentication::NativeManaged { .. })
            })
        })
        .map(|connection| connection.id.clone())
        .chain(
            catalog
                .pools
                .iter()
                .filter(|pool| !pool.projects.is_empty())
                .flat_map(|pool| {
                    catalog.connections.iter().filter(|connection| {
                        pool.members.contains(&connection.id)
                            && connection.harness.as_str() == "rcli"
                            && matches!(connection.endpoint, ModelEndpoint::Http { .. })
                    })
                })
                .filter_map(configured_profile_ref),
        )
        .collect();
    let shape = || ConfigError("routing_approval_shape".into());
    let routing = document
        .entry("routing")
        .or_insert_with(toml_edit::table)
        .as_table_like_mut()
        .ok_or_else(shape)?;
    let approved = routing
        .entry("approved_accounts")
        .or_insert_with(|| toml_edit::value(toml_edit::Array::new()))
        .as_array_mut()
        .ok_or_else(shape)?;
    for account in assigned {
        if !approved
            .iter()
            .any(|value| value.as_str() == Some(account.as_str()))
        {
            approved.push(account.as_str());
        }
    }
    let destinations = document
        .entry("allowed_destinations")
        .or_insert_with(|| toml_edit::value(toml_edit::Array::new()))
        .as_array_mut()
        .ok_or_else(shape)?;
    if destinations.is_empty() {
        destinations.push(LOCAL_DESTINATION);
    }
    for connection in signed_in {
        if !destinations
            .iter()
            .any(|value| value.as_str() == Some(connection.as_str()))
        {
            destinations.push(connection.as_str());
        }
    }
    assert!(!destinations.is_empty());
    Ok(())
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

    const ASSIGNMENT_GLOBAL: &str = r#"version = 1
timeout_ms = 2000
allowed_destinations = []

[model_access]
accounts = ["local-account"]
connections = [{ id = "local-connection", provider = "openai-compatible", harness = "rcli", protocol = "openai-chat", account_ref = "local-account", endpoint = { kind = "http", url = "http://127.0.0.1:11434/v1" }, model = { id = "qwen2.5:0.5b", author = "openai-compatible" }, auth = { method = "none" } }]
pools = [{ id = "local-pool", members = ["local-connection"], projects = [], roles = [], fallback_allowed = false }]
"#;

    fn assigned_pool(projects: &[&str]) -> ModelAccessEdit {
        ModelAccessEdit::ReplacePool {
            pool: AccountPool {
                id: CredentialRef::new("local-pool").unwrap(),
                name: None,
                members: vec![CredentialRef::new("local-connection").unwrap()],
                projects: projects
                    .iter()
                    .map(|p| CredentialRef::new(*p).unwrap())
                    .collect(),
                roles: vec![CredentialRef::new("project_leader").unwrap()],
                fallback_allowed: false,
                provider_quota_policy: ProviderQuotaPolicy::default(),
            },
        }
    }

    #[test]
    fn assigning_pools_approves_each_rcli_account_its_own_profile_destination() {
        let global = r#"version = 1
timeout_ms = 2000
allowed_destinations = []

[model_access]
accounts = ["key-account", "local-account"]
connections = [
  { id = "key-connection", provider = "openai-compatible", harness = "rcli", protocol = "openai-chat", account_ref = "key-account", endpoint = { kind = "http", url = "http://127.0.0.1:9/v1" }, model = { id = "m", author = "openai-compatible" }, auth = { method = "api_key", credential_ref = "key-ref" } },
  { id = "local-connection", provider = "openai-compatible", harness = "rcli", protocol = "openai-chat", account_ref = "local-account", endpoint = { kind = "http", url = "http://127.0.0.1:11434/v1" }, model = { id = "qwen2.5:0.5b", author = "openai-compatible" }, auth = { method = "none" } },
]
pools = [{ id = "pool", members = ["key-connection", "local-connection"], projects = [], roles = [], fallback_allowed = false }]
"#;
        let edit = ModelAccessEdit::ReplacePool {
            pool: AccountPool {
                id: CredentialRef::new("pool").unwrap(),
                name: None,
                members: vec![
                    CredentialRef::new("key-connection").unwrap(),
                    CredentialRef::new("local-connection").unwrap(),
                ],
                projects: vec![CredentialRef::new("demo").unwrap()],
                roles: vec![CredentialRef::new("project_leader").unwrap()],
                fallback_allowed: false,
                provider_quota_policy: ProviderQuotaPolicy::default(),
            },
        };
        let (_, config) = edit_model_access(global, "", &edit, &[]).unwrap();
        let access = config.settings.model_access.as_ref().unwrap();
        let destinations = &config.settings.allowed_destinations;
        assert!(destinations.contains(&CredentialRef::new("local").unwrap()));
        for connection in &access.connections {
            let profile = configured_profile_ref(connection).unwrap();
            assert!(destinations.contains(&profile), "{profile:?}");
        }
        assert_eq!(
            configured_profile_ref(&access.connections[0]),
            Some(CredentialRef::new("key-ref").unwrap())
        );
        assert_ne!(
            configured_profile_ref(&access.connections[0]),
            configured_profile_ref(&access.connections[1])
        );
    }

    #[test]
    fn assigning_a_pool_to_a_project_approves_its_accounts_and_a_local_destination() {
        let (bytes, config) =
            edit_model_access(ASSIGNMENT_GLOBAL, "", &assigned_pool(&["demo"]), &[]).unwrap();
        let account = CredentialRef::new("local-account").unwrap();
        assert_eq!(config.settings.routing.approved_accounts, vec![account]);
        let connection = &config.settings.model_access.as_ref().unwrap().connections[0];
        assert_eq!(
            config.settings.allowed_destinations,
            vec![
                CredentialRef::new("local").unwrap(),
                configured_profile_ref(connection).unwrap()
            ]
        );
        let (_, again) =
            edit_model_access(&bytes, "", &assigned_pool(&["demo", "other"]), &[]).unwrap();
        assert_eq!(again.settings.routing.approved_accounts.len(), 1);
        assert_eq!(
            again.settings.allowed_destinations,
            config.settings.allowed_destinations
        );
    }

    #[test]
    fn assigning_a_signed_in_pool_allows_its_connection_as_the_master_target() {
        let global = r#"version = 1
timeout_ms = 2000
allowed_destinations = []

[model_access]
accounts = ["codex-account"]
connections = [{ id = "codex-connection", provider = "openai", harness = "codex", protocol = "native", account_ref = "codex-account", endpoint = { kind = "native" }, model = { id = "native-default", author = "openai" }, auth = { method = "native_managed", profile_ref = "codex-profile" } }]
pools = [{ id = "codex-pool", members = ["codex-connection"], projects = [], roles = [], fallback_allowed = false }]
"#;
        let edit = ModelAccessEdit::ReplacePool {
            pool: AccountPool {
                id: CredentialRef::new("codex-pool").unwrap(),
                name: None,
                members: vec![CredentialRef::new("codex-connection").unwrap()],
                projects: vec![CredentialRef::new("demo").unwrap()],
                roles: vec![CredentialRef::new("project_leader").unwrap()],
                fallback_allowed: false,
                provider_quota_policy: ProviderQuotaPolicy::default(),
            },
        };
        let (bytes, config) = edit_model_access(global, "", &edit, &[]).unwrap();
        let connection = CredentialRef::new("codex-connection").unwrap();
        assert!(config.settings.allowed_destinations.contains(&connection));
        let (_, again) = edit_model_access(&bytes, "", &edit, &[]).unwrap();
        assert_eq!(
            again.settings.allowed_destinations,
            config.settings.allowed_destinations
        );
    }

    fn many_connections_global(count: usize) -> String {
        let connections: Vec<String> = (0..count)
            .map(|index| format!(r#"{{ id = "c{index}", provider = "openai-compatible", harness = "rcli", protocol = "openai-chat", account_ref = "local-account", endpoint = {{ kind = "http", url = "http://127.0.0.1:11434/v1" }}, model = {{ id = "qwen2.5:0.5b", author = "openai-compatible" }}, auth = {{ method = "none" }} }}"#))
            .collect();
        format!(
            "version = 1\ntimeout_ms = 2000\nallowed_destinations = []\n\n[model_access]\naccounts = [\"local-account\"]\nconnections = [{}]\npools = []\n",
            connections.join(", ")
        )
    }

    fn sized_pool(id: &str, members: usize, projects: &[&str]) -> AccountPool {
        AccountPool {
            id: CredentialRef::new(id).unwrap(),
            name: None,
            members: (0..members)
                .map(|index| CredentialRef::new(format!("c{index}")).unwrap())
                .collect(),
            projects: projects
                .iter()
                .map(|p| CredentialRef::new(*p).unwrap())
                .collect(),
            roles: vec![CredentialRef::new("project_leader").unwrap()],
            fallback_allowed: false,
            provider_quota_policy: ProviderQuotaPolicy::default(),
        }
    }

    #[test]
    fn a_pool_holds_at_most_fifteen_accounts() {
        let global = many_connections_global(16);
        let full = sized_pool("work", 15, &[]);
        let (bytes, config) = edit_model_access(
            &global,
            "",
            &ModelAccessEdit::CreatePool { pool: full },
            &[],
        )
        .unwrap();
        assert_eq!(
            config.settings.model_access.unwrap().pools[0].members.len(),
            15
        );
        let grown = ModelAccessEdit::ReplacePool {
            pool: sized_pool("work", 16, &[]),
        };
        assert_eq!(
            edit_model_access(&bytes, "", &grown, &[]).unwrap_err().0,
            "pool_member_limit"
        );
        let created = ModelAccessEdit::CreatePool {
            pool: sized_pool("other", 16, &[]),
        };
        assert_eq!(
            edit_model_access(&bytes, "", &created, &[]).unwrap_err().0,
            "pool_member_limit"
        );
        assert_eq!(
            plain_problem("pool_member_limit"),
            Some("A pool can hold up to 15 accounts")
        );
    }

    #[test]
    fn the_same_account_may_belong_to_many_pools() {
        let global = many_connections_global(2);
        let (bytes, _) = edit_model_access(
            &global,
            "",
            &ModelAccessEdit::CreatePool {
                pool: sized_pool("first", 2, &[]),
            },
            &[],
        )
        .unwrap();
        let (_, config) = edit_model_access(
            &bytes,
            "",
            &ModelAccessEdit::CreatePool {
                pool: sized_pool("second", 1, &["demo"]),
            },
            &[],
        )
        .unwrap();
        let pools = config.settings.model_access.unwrap().pools;
        assert_eq!(pools.len(), 2);
        assert!(
            pools
                .iter()
                .all(|pool| pool.members.contains(&CredentialRef::new("c0").unwrap()))
        );
    }

    #[test]
    fn a_pool_used_by_a_project_cannot_be_deleted() {
        let global = many_connections_global(1);
        let (bytes, _) = edit_model_access(
            &global,
            "",
            &ModelAccessEdit::CreatePool {
                pool: sized_pool("work", 1, &["demo"]),
            },
            &[],
        )
        .unwrap();
        let remove = ModelAccessEdit::RemovePool {
            id: CredentialRef::new("work").unwrap(),
        };
        assert_eq!(
            edit_model_access(&bytes, "", &remove, &[]).unwrap_err().0,
            "pool_in_use"
        );
        assert_eq!(
            plain_problem("pool_in_use"),
            Some("This pool is used by a project. Remove it from the project first.")
        );
        let (bytes, _) = edit_model_access(
            &bytes,
            "",
            &ModelAccessEdit::ReplacePool {
                pool: sized_pool("work", 1, &[]),
            },
            &[],
        )
        .unwrap();
        let (_, config) = edit_model_access(&bytes, "", &remove, &[]).unwrap();
        assert!(config.settings.model_access.unwrap().pools.is_empty());
    }

    #[test]
    fn a_pool_name_is_optional_bounded_and_renamable() {
        let global = many_connections_global(1);
        let mut pool = sized_pool("work", 1, &[]);
        let (bytes, config) = edit_model_access(
            &global,
            "",
            &ModelAccessEdit::CreatePool { pool: pool.clone() },
            &[],
        )
        .unwrap();
        assert_eq!(config.settings.model_access.unwrap().pools[0].name, None);
        assert!(!bytes.contains("name ="));
        pool.name = Some("Daily work".into());
        let (bytes, config) = edit_model_access(
            &bytes,
            "",
            &ModelAccessEdit::ReplacePool { pool: pool.clone() },
            &[],
        )
        .unwrap();
        let stored = &config.settings.model_access.unwrap().pools[0];
        assert_eq!(stored.name.as_deref(), Some("Daily work"));
        assert_eq!(stored.id.as_str(), "work");
        for invalid in ["", "   ", "tab\there", &"x".repeat(65)] {
            pool.name = Some(invalid.into());
            let edit = ModelAccessEdit::ReplacePool { pool: pool.clone() };
            assert_eq!(
                edit_model_access(&bytes, "", &edit, &[]).unwrap_err().0,
                "pool_name_bounds"
            );
        }
        pool.name = Some("é".repeat(64));
        edit_model_access(&bytes, "", &ModelAccessEdit::ReplacePool { pool }, &[]).unwrap();
    }

    #[test]
    fn unassigned_pools_approve_nothing() {
        let (_, config) =
            edit_model_access(ASSIGNMENT_GLOBAL, "", &assigned_pool(&[]), &[]).unwrap();
        assert!(config.settings.routing.approved_accounts.is_empty());
        assert!(config.settings.allowed_destinations.is_empty());
    }

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

    #[test]
    fn newer_settings_file_is_refused_as_newer_than_engine() {
        let root = tempfile::tempdir().unwrap();
        let global = root.path().join("global.toml");
        let project = root.path().join("project.toml");
        let newer = format!("version = {}\ntimeout_ms = 2000\n", SETTINGS_VERSION + 1);
        std::fs::write(&global, &newer).unwrap();
        let refused = resolve_config(&global, &project).unwrap_err().to_string();
        assert!(
            refused.starts_with("settings_newer_than_engine:"),
            "{refused}"
        );
        assert_eq!(std::fs::read_to_string(&global).unwrap(), newer);
        std::fs::write(&global, "version = 0\n").unwrap();
        assert_eq!(
            resolve_config(&global, &project).unwrap_err().to_string(),
            "unsupported config version"
        );
        std::fs::write(&global, format!("version = {SETTINGS_VERSION}\n")).unwrap();
        assert!(resolve_config(&global, &project).is_ok());
    }
}
