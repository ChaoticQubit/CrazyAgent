use super::native_auth::{NativeAuthMethod, NativeAuthScope};
use crate::config::{ModelAuthentication, ModelConnection};
use crate::engine::EngineError;
use crate::protocol::{ConfigId, CredentialRef, RequestId};
use serde::{Deserialize, Serialize};
use std::fmt;

pub struct AccountSecret(Vec<u8>);

impl AccountSecret {
    pub fn new(bytes: Vec<u8>) -> Result<Self, EngineError> {
        if bytes.is_empty()
            || bytes.len() > 8192
            || bytes.iter().any(|byte| matches!(byte, 0 | b'\r' | b'\n'))
            || std::str::from_utf8(&bytes).is_err()
        {
            return Err(EngineError("account_secret_invalid".into()));
        }
        Ok(Self(bytes))
    }

    pub(in crate::engine) fn into_bytes(self) -> Vec<u8> {
        self.0
    }
}

impl fmt::Debug for AccountSecret {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output.write_str("AccountSecret([REDACTED])")
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccountProfileRequest {
    pub request_id: RequestId,
    pub scope: NativeAuthScope,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccountProfileReceipt {
    pub config_id: ConfigId,
    pub source_hash: String,
    pub connection: ModelConnection,
    pub profile_ref: CredentialRef,
    pub custody_id: String,
    pub profile_config_sha256: String,
    pub profile_generation: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum AccountProfileOutcome {
    ConfiguredOnly {
        receipt: AccountProfileReceipt,
        historical_replay: bool,
    },
    NotRecorded,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccountProfileReply {
    pub request_id: RequestId,
    pub database_instance: String,
    pub engine_epoch: String,
    pub outcome: AccountProfileOutcome,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum ProducerCapability {
    Available,
    Unavailable { code: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManagedAuthCapability {
    pub method: NativeAuthMethod,
    pub producer: ProducerCapability,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccountAuthCapabilitiesReply {
    pub request_id: RequestId,
    pub database_instance: String,
    pub engine_epoch: String,
    pub config_id: ConfigId,
    pub source_hash: String,
    pub connection: ModelConnection,
    pub managed: Vec<ManagedAuthCapability>,
    pub configured: ProducerCapability,
}

pub(crate) fn authorize_account_profile_scope(
    scope: &NativeAuthScope,
    database_instance: &str,
    config_id: &ConfigId,
    source_hash: &str,
    connection: &ModelConnection,
) -> Result<(), EngineError> {
    if scope.expected_database_instance != database_instance {
        return Err(EngineError("native_auth_database_mismatch".into()));
    }
    if &scope.expected_config_id != config_id || scope.expected_source_hash != source_hash {
        return Err(EngineError("account_profile_config_changed".into()));
    }
    if scope.connection != connection.id {
        return Err(EngineError("account_profile_connection_changed".into()));
    }
    if !matches!(connection.auth, ModelAuthentication::ApiKey { .. } | ModelAuthentication::None {}) {
        return Err(EngineError("account_profile_auth_unsupported".into()));
    }
    if connection.harness.as_str() != "rcli" {
        return Err(EngineError("account_profile_harness_unsupported".into()));
    }
    let crate::config::ModelEndpoint::Http { url } = &connection.endpoint else {
        return Err(EngineError("account_profile_endpoint_unsupported".into()));
    };
    let url = url::Url::parse(url).map_err(|_| EngineError("account_profile_endpoint_unsupported".into()))?;
    if !matches!(url.scheme(), "http" | "https") || !url.has_host()
        || !url.username().is_empty() || url.password().is_some() || url.query().is_some() || url.fragment().is_some() {
        return Err(EngineError("account_profile_endpoint_unsupported".into()));
    }
    if matches!(connection.auth, ModelAuthentication::None {}) {
        let local = match url.host() {
            Some(url::Host::Ipv4(address)) => address.is_loopback(),
            Some(url::Host::Ipv6(address)) => address.is_loopback(),
            Some(url::Host::Domain(domain)) => domain == "localhost",
            None => false,
        };
        if !local { return Err(EngineError("account_profile_endpoint_unsupported".into())); }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
#[cfg(test)]
mod signature_checks;
