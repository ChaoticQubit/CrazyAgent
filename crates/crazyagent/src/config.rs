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
    #[serde(default, skip_serializing_if = "PluginsPolicy::is_default")]
    pub plugins: PluginsPolicy,
}

/// The owner's plugin settings: what turning a plugin off does to runs holding it, and
/// whether essential bundled code plugins may be turned off.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PluginsPolicy {
    pub when_turned_off: kernel_plugin_host::usage::TurnOff,
    pub essential_unlocked: bool,
}

impl PluginsPolicy {
    /// Whether every plugin setting holds its default. No input. Output: the answer.
    pub fn is_default(&self) -> bool {
        self == &Self::default()
    }
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
            plugins: PluginsPolicy::default(),
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
/// The owner timeout_ms values the engine accepts: in its settings, and in the owner
/// timeout file of the generation a client discovers it through.
/// Values: milliseconds, both ends included.
pub const TIMEOUT_MS_BOUNDS: std::ops::RangeInclusive<u64> = 10..=30000;
/// The settings file version this engine reads and writes. A file with a higher
/// `version` came from a newer engine and is refused, never rewritten.
pub const SETTINGS_VERSION: u16 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ModelAccessEdit {
    AddAccount {
        account_ref: CredentialRef,
    },
    RemoveAccount {
        account_ref: CredentialRef,
    },
    CreateConnection {
        connection: ModelConnection,
    },
    ReplaceConnection {
        connection: ModelConnection,
    },
    RemoveConnection {
        id: CredentialRef,
    },
    CreatePool {
        pool: AccountPool,
    },
    ReplacePool {
        pool: AccountPool,
    },
    RemovePool {
        id: CredentialRef,
    },
    AddConnectedAccount {
        connection: ModelConnection,
    },
    RemoveConnectedAccount {
        id: CredentialRef,
    },
    AssignProjectPool {
        project: CredentialRef,
        pool: Option<CredentialRef>,
    },
}

impl ModelAccessEdit {
    /// Tells whether this edit changes the account of a connection. Pool and project
    /// assignments leave the account and its profile as they are. Input: the connection.
    /// Output: true when the edit names its account, its id, or a connection on the same
    /// account.
    pub fn touches(&self, connection: &ModelConnection) -> bool {
        match self {
            Self::AddAccount { account_ref } | Self::RemoveAccount { account_ref } => {
                account_ref == &connection.account_ref
            }
            Self::CreateConnection { connection: other }
            | Self::ReplaceConnection { connection: other }
            | Self::AddConnectedAccount { connection: other } => same_account(connection, other),
            Self::RemoveConnection { id } | Self::RemoveConnectedAccount { id } => {
                id == &connection.id
            }
            Self::CreatePool { .. }
            | Self::ReplacePool { .. }
            | Self::RemovePool { .. }
            | Self::AssignProjectPool { .. } => false,
        }
    }

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

/// Tells whether two connections share an account: the same connection, the same account
/// reference, or the same native profile. Inputs: the two connections.
pub fn same_account(left: &ModelConnection, right: &ModelConnection) -> bool {
    left.id == right.id
        || left.account_ref == right.account_ref
        || matches!(
            (&left.auth, &right.auth),
            (
                ModelAuthentication::NativeManaged { profile_ref: held },
                ModelAuthentication::NativeManaged { profile_ref: named },
            ) if held == named
        )
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
        let plugins = self
            .descriptors
            .keys()
            .any(|name| name.starts_with("plugins."));
        let model_access = self.settings.model_access.is_some();
        let shown = Shown {
            routing,
            knowledge,
            plugins,
            model_access,
        };
        let defaults = setting_values(&Settings::default(), shown);
        let values = setting_values(&self.settings, shown);
        if (!routing && !self.settings.routing.is_default())
            || (!knowledge && !self.settings.knowledge.is_default())
            || (!plugins && !self.settings.plugins.is_default())
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
    plugins: Option<PluginsFile>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct PluginsFile {
    when_turned_off: Option<kernel_plugin_host::usage::TurnOff>,
    essential_unlocked: Option<bool>,
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
    let plugins = origins.keys().any(|name| name.starts_with("plugins."));
    let model_access = settings.model_access.is_some();
    let shown = Shown {
        routing: true,
        knowledge,
        plugins,
        model_access,
    };
    let defaults = setting_values(&Settings::default(), shown);
    let values = setting_values(&settings, shown);
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
                "plugins.when_turned_off" | "plugins.essential_unlocked" => EffectTiming::Immediate,
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
        17 + 8 * usize::from(knowledge) + usize::from(plugins) + usize::from(model_access)
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

/// Refuses a connection being saved whose address is plain HTTP to another machine, or
/// on another machine uses a cloud metadata port, with the same rule as an added account.
/// Input: the connection. Output: Ok, `account_url_invalid` or `account_url_port_refused`.
fn saved_endpoint(connection: &ModelConnection) -> Result<(), ConfigError> {
    match &connection.endpoint {
        ModelEndpoint::Http { url } => crate::accounts::account_url(url)
            .map(|_| ())
            .map_err(|error| ConfigError(error.0)),
        ModelEndpoint::Native {} => Ok(()),
    }
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
    if let ModelAccessEdit::CreateConnection { connection }
    | ModelAccessEdit::ReplaceConnection { connection }
    | ModelAccessEdit::AddConnectedAccount { connection } = edit
    {
        saved_endpoint(connection)?;
    }
    let previous = resolve_config_bytes(global, project)?
        .settings
        .model_access
        .unwrap_or_default();
    let mut catalog = previous.clone();
    for connection in registered {
        if catalog.connections.iter().any(|c| c.id == connection.id) {
            continue;
        }
        saved_endpoint(connection)?;
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
            catalog.pools.remove(index);
        }
        ModelAccessEdit::AssignProjectPool { project, pool } => {
            assign_project_pool(&mut catalog.pools, project, pool.as_ref())?;
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
            leave_pools(&mut catalog.pools, id);
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
    approve_assigned_accounts(&mut document, &previous, &catalog)?;
    let bytes = document.to_string();
    let config = resolve_config_bytes(&bytes, project)?;
    assert_eq!(config.settings.model_access, Some(catalog));
    Ok((bytes, config))
}

/// Sets what happens to running work when a plugin is turned off in the global settings
/// text, changing only that value so every other byte, comment and key stays as written.
/// Inputs: the global and project texts and the choice. Output: the new global text and the
/// config both texts resolve to; a global text that does not resolve today is refused.
pub(crate) fn edit_when_turned_off(
    global: &str,
    project: &str,
    when_off: kernel_plugin_host::usage::TurnOff,
) -> Result<(String, EffectiveConfig), ConfigError> {
    resolve_config_bytes(global, project)?;
    let shape = || ConfigError("plugins_shape".into());
    let mut document = global
        .parse::<toml_edit::DocumentMut>()
        .map_err(|_| ConfigError("invalid_configuration_format".into()))?;
    let word = match when_off {
        kernel_plugin_host::usage::TurnOff::RefuseNewCalls => "refuse_new_calls",
        kernel_plugin_host::usage::TurnOff::StopRuns => "stop_runs",
    };
    let bytes = match document.get_mut("plugins") {
        None => {
            let gap = if global.is_empty() {
                ""
            } else if global.ends_with('\n') {
                "\n"
            } else {
                "\n\n"
            };
            format!("{global}{gap}[plugins]\nwhen_turned_off = \"{word}\"\n")
        }
        Some(item) => {
            let plugins = item.as_table_like_mut().ok_or_else(shape)?;
            match plugins.get_mut("when_turned_off") {
                Some(item) => {
                    let value = item.as_value_mut().ok_or_else(shape)?;
                    let decor = value.decor().clone();
                    *value = toml_edit::Value::from(word);
                    *value.decor_mut() = decor;
                }
                None => {
                    plugins.insert("when_turned_off", toml_edit::value(word));
                }
            }
            document.to_string()
        }
    };
    let config = resolve_config_bytes(&bytes, project)?;
    let from_project = config
        .descriptors
        .get("plugins.when_turned_off")
        .is_some_and(|descriptor| descriptor.origin == Origin::Project);
    assert!(from_project || config.settings.plugins.when_turned_off == when_off);
    Ok((bytes, config))
}

/// The most accounts one pool may hold when it is created or changed.
pub const POOL_MEMBERS_MAX: usize = 15;
/// The most projects one pool may serve.
pub const POOL_PROJECTS_MAX: usize = 64;
/// The role a project's own pool carries so the project's master can use it.
pub const LEADER_ROLE: &str = "project_leader";

/// Gives one project exactly one pool, or none: the project leaves every other pool and
/// joins the chosen one with the leader role. Inputs: the pools, the project and the chosen
/// pool, None for no pool. Output: Ok, `pool_not_found`, or `pool_project_limit` when the
/// chosen pool already serves the most projects.
fn assign_project_pool(
    pools: &mut [AccountPool],
    project: &CredentialRef,
    chosen: Option<&CredentialRef>,
) -> Result<(), ConfigError> {
    let target = match chosen {
        Some(id) => Some(
            pools
                .iter()
                .position(|pool| pool.id == *id)
                .ok_or_else(|| ConfigError("pool_not_found".into()))?,
        ),
        None => None,
    };
    if let Some(index) = target
        && !pools[index].projects.contains(project)
        && pools[index].projects.len() >= POOL_PROJECTS_MAX
    {
        return Err(ConfigError("pool_project_limit".into()));
    }
    for (index, pool) in pools.iter_mut().enumerate() {
        if Some(index) != target {
            pool.projects.retain(|held| held != project);
        }
    }
    if let Some(index) = target {
        let leader = CredentialRef::new(LEADER_ROLE).expect("the leader role is a valid reference");
        let pool = &mut pools[index];
        if !pool.projects.contains(project) {
            pool.projects.push(project.clone());
        }
        if !pool.roles.contains(&leader) {
            pool.roles.push(leader);
        }
    }
    assert!(
        pools
            .iter()
            .filter(|pool| pool.projects.contains(project))
            .count()
            == usize::from(chosen.is_some())
    );
    Ok(())
}

/// Takes one account out of every pool. A pool left with no account is removed with it,
/// so the projects it served have no pool. Inputs: the pools and the account's connection
/// id. Output: the pools without that account.
fn leave_pools(pools: &mut Vec<AccountPool>, id: &CredentialRef) {
    for pool in pools.iter_mut() {
        pool.members.retain(|member| member != id);
    }
    pools.retain(|pool| !pool.members.is_empty());
    assert!(pools.iter().all(|pool| !pool.members.contains(id)));
}
/// The most characters of a pool display name.
pub const POOL_NAME_CHARS_MAX: usize = 64;

/// Checks the owner-facing limits of a pool being created or changed.
/// Input: the proposed pool. Output: `pool_member_limit` above 15 members,
/// `pool_name_bounds` for a blank, overlong or control-character name, else Ok.
fn validate_pool_edit(pool: &AccountPool) -> Result<(), ConfigError> {
    if pool.members.len() > POOL_MEMBERS_MAX {
        return Err(ConfigError("pool_member_limit".into()));
    }
    if pool.members.is_empty() {
        return Err(ConfigError("pool_needs_account".into()));
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

/// The sentence for `owner_busy`: a request the owner made was not sent because too many
/// were already waiting, so nothing changed.
pub const OWNER_BUSY_LINE: &str =
    "CrazyAgent is busy with other requests. Nothing changed; try again in a moment.";

/// The sentence for `owner_queue_not_submitted`: the engine's request queue was full, so the
/// request was refused before it was queued, nothing changed and a retry is safe.
pub const OWNER_QUEUE_LINE: &str =
    "CrazyAgent had too many requests waiting, so nothing changed. It's safe to do it again now.";

/// The sentence for `storage_starting_not_submitted`: the request reached the engine before
/// its database took requests and was not sent on, so nothing changed and a retry is safe.
pub const STORAGE_STARTING_LINE: &str =
    "CrazyAgent's database was still starting, so nothing changed. It's safe to do it again now.";

/// The sentence for `storage_busy_not_submitted`: the request waited behind earlier database
/// work past its bound and was withdrawn unrun, so nothing changed and a retry is safe.
pub const STORAGE_BUSY_LINE: &str = "CrazyAgent's database was busy with earlier work, so nothing changed. It's safe to do it again now.";

/// The sentence for `access_change_pending`: a change that narrows access waited past its
/// bound and stays queued or running. It takes effect unless the engine stops before
/// storage reaches it, since the queue does not outlive the engine.
pub const ACCESS_CHANGE_PENDING_LINE: &str = "CrazyAgent's database is busy, so this change is waiting its turn and takes effect when the database gets to it. If CrazyAgent stops before then, make the change again.";

/// The sentence for `engine_version_mismatch`: the running engine and this app speak
/// different versions, read from a version check or from a reply the app could not read.
pub const ENGINE_VERSION_LINE: &str =
    "CrazyAgent's engine is a different version from this app. Restart the engine.";

/// Turns a pool, approval-limit, busy-owner or version-mismatch code into the sentence shown
/// to the owner. Input: an engine rejection code. Output: the plain sentence for such a refusal,
/// or None for any other code.
pub fn plain_problem(code: &str) -> Option<&'static str> {
    match code {
        "owner_busy" => Some(OWNER_BUSY_LINE),
        "owner_queue_not_submitted" => Some(OWNER_QUEUE_LINE),
        "storage_starting_not_submitted" => Some(STORAGE_STARTING_LINE),
        "storage_busy_not_submitted" => Some(STORAGE_BUSY_LINE),
        "access_change_pending" => Some(ACCESS_CHANGE_PENDING_LINE),
        "engine_version_mismatch" => Some(ENGINE_VERSION_LINE),
        "pool_member_limit" => Some("A pool can hold up to 15 accounts"),
        "pool_needs_account" => Some(
            "A pool needs at least one account. Add another account first, or delete the pool.",
        ),
        "pool_project_limit" => {
            Some("A pool can serve up to 64 projects. Pick another pool for this project.")
        }
        "pool_name_bounds" => {
            Some("Use a pool name of 1 to 64 characters, without control characters.")
        }
        "pool_exists" => Some("A pool with this id already exists."),
        "pool_not_found" => Some("This pool no longer exists. Refresh and try again."),
        "project_not_found" => Some("This project no longer exists. Refresh and try again."),
        "pool_users_changed" => {
            Some("The projects using this pool changed. Review them, then delete again.")
        }
        "approved_accounts_limit" => Some(
            "Projects can use up to 64 approved accounts. Remove an account you no longer use, then try again.",
        ),
        "allowed_destinations_limit" => Some(
            "Up to 64 sign-ins and model endpoints can be allowed. Remove a connection you no longer use, then try again.",
        ),
        "account_not_found" => Some("This account no longer exists. Refresh and try again."),
        "model_access_read_only" => {
            Some("Settings are read-only in this engine. Start it with writable settings.")
        }
        "model_access_edit_bounds" => {
            Some("This change is too large to save at once. Make it in smaller steps.")
        }
        "linux_limited_network_off" => Some(
            "The Linux limited-network sockets setting is 'Do not start limited-network agents', so this master does not start. Pick another Linux limited-network sockets option in Settings.",
        ),
        "linux_resolver_needs_udp" => Some(
            "This harness cannot look up names over TCP, which the Linux setting 'TCP only' needs. Choose 'TCP ports only' for Linux limited-network sockets in Settings, which lets UDP and other sockets reach any host.",
        ),
        "linux_harness_unchecked" => Some(
            "This harness starts through a script CrazyAgent cannot check for the Linux setting 'TCP only', so it does not start. Start it from a program, or a script whose first line names a program by its full path, or choose 'TCP ports only' for Linux limited-network sockets in Settings, which lets UDP and other sockets reach any host.",
        ),
        "chatgpt_sign_in_expired" => {
            Some(crate::model_gateway::relay::chatgpt::SIGN_IN_EXPIRED_LINE)
        }
        "sandbox_full_unavailable_windows" => {
            Some("Full access isn't available on Windows in this version.")
        }
        "model_loopback_unavailable_windows" => Some(
            "A model on this computer, such as a local server or the relay, can't be reached by agents on Windows in this version, so this agent does not start. Use a model that runs elsewhere.",
        ),
        "linux_sandbox_not_full" => Some(
            "This Linux cannot give this agent the full sandbox and the setting 'Refuse agents without the full Linux sandbox' is on, so it does not start. The lines under Agent access say what this Linux lacks. Turn the setting off in Settings to start agents with what this Linux holds.",
        ),
        "linux_hidden_needs_namespaces" => Some(
            "This project hides folders, and on Linux an agent with hidden folders starts only when bubblewrap can give it namespaces of its own, which this computer does not allow, so the agent does not start. Install bubblewrap and allow unprivileged user namespaces, then start it again, or take the folders off Hidden folders in the project's settings.",
        ),
        "linux_hidden_mounts_not_private" => Some(
            "This project hides folders, and on Linux an agent with hidden folders starts only when its mounts can be cut off from this computer's, so that no later mount on this computer reaches the folders. This computer does not let the sandbox helper make user namespaces of its own inside bubblewrap, so the agent does not start. Allow user namespaces for the programs bubblewrap starts, then start it again, or take the folders off Hidden folders in the project's settings.",
        ),
        "linux_hidden_mounted_again" => Some(
            "A folder this project hides sits on a disk that is mounted a second time where it shows that folder, so an agent could reach the folder there and the agent does not start. To start it, remove that mount or take the folder off Hidden folders in the project's settings.",
        ),
        "team_master_no_terminal" => Some(
            "This master harness has no terminal app. Its conversation stays on the Conversation tab.",
        ),
        "stale_revision" => {
            Some("This changed in another window, so yours was not saved. The saved one shows now.")
        }
        _ => None,
    }
}

const LOCAL_DESTINATION: &str = "local";

/// The most entries `routing.approved_accounts` or `allowed_destinations` may hold.
const APPROVALS_MAX: usize = 64;

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

/// The allowed master target a connection gets when its pool is assigned: the connection
/// itself when signed in or stored for a harness that runs listed masters and fits it
/// (DEC-180 Task 20 Q8), or its own profile reference for an RCLI HTTP endpoint.
pub(crate) fn connection_destination(connection: &ModelConnection) -> Option<CredentialRef> {
    match connection.auth {
        ModelAuthentication::NativeManaged { .. } => Some(connection.id.clone()),
        _ if connection.harness.as_str() == "rcli"
            && matches!(connection.endpoint, ModelEndpoint::Http { .. }) =>
        {
            configured_profile_ref(connection)
        }
        _ if crate::master_session::listed_master(connection.harness.as_str())
            .is_ok_and(|harness| harness.account_fits(connection)) =>
        {
            Some(connection.id.clone())
        }
        _ => None,
    }
}

/// Treats an explicit pool-to-project assignment as owner approval of the pool's accounts,
/// and keeps those approvals in step with the catalog.
/// Input: the edited global document, the catalog before the edit and the validated catalog
/// it now holds. Output: `routing.approved_accounts` loses accounts the edit removed and gains
/// every account of an assigned pool; `allowed_destinations` loses the master target of a
/// removed or replaced connection, an empty list gains the local execution target, and every
/// signed-in, RCLI HTTP or listed master connection of an assigned pool gets its own target;
/// more than 64 of either is refused, `approved_accounts_limit` or `allowed_destinations_limit`.
fn approve_assigned_accounts(
    document: &mut toml_edit::DocumentMut,
    previous: &ModelAccess,
    catalog: &ModelAccess,
) -> Result<(), ConfigError> {
    let shape = || ConfigError("routing_approval_shape".into());
    let removed_accounts: Vec<&CredentialRef> = previous
        .accounts
        .iter()
        .filter(|account| !catalog.accounts.contains(account))
        .collect();
    let current: Vec<CredentialRef> = catalog
        .connections
        .iter()
        .filter_map(connection_destination)
        .collect();
    let replaced: Vec<CredentialRef> = previous
        .connections
        .iter()
        .filter_map(connection_destination)
        .filter(|destination| !current.contains(destination))
        .collect();
    if let Some(approved) = document
        .get_mut("routing")
        .and_then(|routing| routing.as_table_like_mut())
        .and_then(|routing| routing.get_mut("approved_accounts"))
    {
        approved.as_array_mut().ok_or_else(shape)?.retain(|value| {
            !removed_accounts
                .iter()
                .any(|account| value.as_str() == Some(account.as_str()))
        });
    }
    if let Some(destinations) = document.get_mut("allowed_destinations") {
        destinations
            .as_array_mut()
            .ok_or_else(shape)?
            .retain(|value| {
                !replaced
                    .iter()
                    .any(|destination| value.as_str() == Some(destination.as_str()))
            });
    }
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
            catalog
                .connections
                .iter()
                .filter(|connection| pool.members.contains(&connection.id))
        })
        .filter_map(connection_destination)
        .collect();
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
    if approved.len() > APPROVALS_MAX {
        return Err(ConfigError("approved_accounts_limit".into()));
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
    if destinations.len() > APPROVALS_MAX {
        return Err(ConfigError("allowed_destinations_limit".into()));
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
    if let Some(plugins) = file.plugins {
        if let Some(when_turned_off) = plugins.when_turned_off {
            settings.plugins.when_turned_off = when_turned_off;
            origins.insert("plugins.when_turned_off".into(), origin.clone());
        }
        if let Some(essential_unlocked) = plugins.essential_unlocked {
            settings.plugins.essential_unlocked = essential_unlocked;
            origins.insert("plugins.essential_unlocked".into(), origin.clone());
        }
    }
}

/// Which setting groups an effective config lists one by one: routing, knowledge and
/// plugins only when set in a file, model access when present.
#[derive(Debug, Clone, Copy)]
struct Shown {
    routing: bool,
    knowledge: bool,
    plugins: bool,
    model_access: bool,
}

fn setting_values(settings: &Settings, shown: Shown) -> BTreeMap<String, serde_json::Value> {
    let Shown {
        routing: include_routing,
        knowledge: include_knowledge,
        plugins: include_plugins,
        model_access: include_model_access,
    } = shown;
    let mut values = serde_json::to_value(settings)
        .unwrap()
        .as_object()
        .unwrap()
        .clone();
    values.remove("routing");
    values.remove("knowledge");
    values.remove("model_access");
    values.remove("plugins");
    if include_plugins {
        values.insert(
            "plugins.when_turned_off".into(),
            serde_json::to_value(settings.plugins.when_turned_off).unwrap(),
        );
        values.insert(
            "plugins.essential_unlocked".into(),
            serde_json::Value::Bool(settings.plugins.essential_unlocked),
        );
    }
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
        || !TIMEOUT_MS_BOUNDS.contains(&settings.timeout_ms)
        || !(1..=256).contains(&settings.events_retained)
        || !(1..=1024).contains(&settings.bulk_queue_capacity)
        || settings.allowed_destinations.len() > APPROVALS_MAX
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
    assert!(descriptors.len() <= 27 + usize::from(settings.model_access.is_some()));
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
#[path = "config/when_off_tests.rs"]
mod when_off_tests;

#[cfg(test)]
mod tests;
