use super::{EngineError, NativeProfileBinding, failure, is_hex_id};
use crate::config::ModelConnection;
use crate::protocol::{ConfigId, CredentialRef, Cursor, RequestId};
use serde::{Deserialize, Serialize};
use std::fmt;

pub(crate) mod presentation;

#[cfg(test)]
mod presentation_tests;

pub(crate) const REQUEST_BYTES_MAX: usize = 8192;
pub(crate) const FACT_BYTES_MAX: usize = 16384;
pub(crate) const OPERATIONS_MAX: i64 = 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeAuthScope {
    pub expected_database_instance: String,
    pub connection: CredentialRef,
    pub expected_config_id: ConfigId,
    pub expected_source_hash: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeAuthMethod {
    CodexBrowser,
    CodexDeviceCode,
    ClaudeOfficial,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeAuthStart {
    pub request_id: RequestId,
    pub scope: NativeAuthScope,
    pub method: NativeAuthMethod,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeAuthQuery {
    pub request_id: RequestId,
    pub scope: NativeAuthScope,
    pub operation_id: RequestId,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeAuthCancel {
    pub request_id: RequestId,
    pub scope: NativeAuthScope,
    pub operation_id: RequestId,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeAuthLogout {
    pub request_id: RequestId,
    pub scope: NativeAuthScope,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum NativeAuthAction {
    SignIn { method: NativeAuthMethod },
    Logout,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeAuthPhase {
    Starting,
    AwaitingOwner,
    CheckingAccount,
    AuthenticatedObserved,
    UnauthenticatedObserved,
    Cancelling,
    Failed,
    Cancelled,
    Unavailable,
    Uncertain,
}

impl NativeAuthPhase {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Starting => "starting",
            Self::AwaitingOwner => "awaiting_owner",
            Self::CheckingAccount => "checking_account",
            Self::AuthenticatedObserved => "authenticated_observed",
            Self::UnauthenticatedObserved => "unauthenticated_observed",
            Self::Cancelling => "cancelling",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Unavailable => "unavailable",
            Self::Uncertain => "uncertain",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeAuthFact {
    pub operation_id: RequestId,
    pub action: NativeAuthAction,
    #[serde(with = "binding_wire")]
    pub binding: NativeProfileBinding,
    pub generation: u64,
    pub phase: NativeAuthPhase,
    pub code: Option<String>,
}

pub struct NativeAuthChallenge(crate::adapters::codex::LoginChallenge);

impl NativeAuthChallenge {
    pub fn codex(&self) -> &crate::adapters::codex::LoginChallenge {
        &self.0
    }
}

impl fmt::Debug for NativeAuthChallenge {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("NativeAuthChallenge([REDACTED])")
    }
}

pub enum NativeAuthAccount {
    Codex(crate::adapters::codex::AuthAccount),
    Claude(crate::adapters::claude::AuthStatus),
}

impl fmt::Debug for NativeAuthAccount {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("NativeAuthAccount([REDACTED])")
    }
}

pub struct NativeAuthReply {
    pub request_id: RequestId,
    pub database_instance: String,
    pub engine_epoch: String,
    pub fact: NativeAuthFact,
    pub challenge: Option<NativeAuthChallenge>,
    pub claude_challenge: Option<crate::engine::native_auth_host::claude_auth::types::ClaudeAuthChallenge>,
    pub account: Option<NativeAuthAccount>,
}

impl fmt::Debug for NativeAuthReply {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NativeAuthReply")
            .field("request_id", &self.request_id)
            .field("database_instance", &self.database_instance)
            .field("engine_epoch", &self.engine_epoch)
            .field("fact", &self.fact)
            .field("challenge", &self.challenge.as_ref().map(|_| "[REDACTED]"))
            .field("claude_challenge", &self.claude_challenge.as_ref().map(|_| "[REDACTED]"))
            .field("account", &self.account.as_ref().map(|_| "[REDACTED]"))
            .finish()
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum CodexAuthObservation {
    LoginStarted {
        id: i64,
        login_id: String,
        url: String,
        user_code: Option<String>,
    },
    LoginCompleted { login_id: Option<String>, success: bool },
    AccountRead {
        id: i64,
        account: Option<CodexObservedAccount>,
        requires_openai_auth: bool,
    },
    LoginCanceled { id: i64, canceled: bool },
    LoggedOut { id: i64 },
    RequestFailed { id: i64, method: String },
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase", deny_unknown_fields)]
pub(crate) enum CodexObservedAccount {
    ApiKey,
    Chatgpt { email: Option<String>, #[serde(rename = "planType")] plan_type: String },
    AmazonBedrock {
        #[serde(rename = "usesCodexManagedCredentials")]
        uses_codex_managed_credentials: bool,
    },
}

impl CodexObservedAccount {
    fn from_native(account: crate::adapters::codex::AuthAccount) -> Self {
        use crate::adapters::codex::AuthAccount;
        match account {
            AuthAccount::ApiKey => Self::ApiKey,
            AuthAccount::Chatgpt { email, plan_type } => Self::Chatgpt {
                email: email.map(|email| email.expose().to_owned()), plan_type,
            },
            AuthAccount::AmazonBedrock { uses_codex_managed_credentials } => {
                Self::AmazonBedrock { uses_codex_managed_credentials }
            }
        }
    }

    fn native(&self) -> Result<crate::adapters::codex::AuthAccount, EngineError> {
        let value = serde_json::to_value(self).map_err(|_| failure("native_auth_observation_invalid"))?;
        crate::adapters::codex::auth_account(&value)
            .map_err(|_| failure("native_auth_observation_invalid"))
    }
}

impl CodexAuthObservation {
    pub(crate) fn from_native(event: crate::adapters::codex::AuthControl) -> Result<Self, EngineError> {
        use crate::adapters::codex::{AuthControl, LoginCancelStatus, LoginChallenge};
        let observation = match event {
            AuthControl::LoginStarted { id, challenge } => match challenge {
                LoginChallenge::Browser { login_id, auth_url } => Self::LoginStarted {
                    id, login_id, url: auth_url.expose().to_owned(), user_code: None,
                },
                LoginChallenge::DeviceCode { login_id, user_code, verification_url } => {
                    Self::LoginStarted { id, login_id, url: verification_url.expose().to_owned(),
                        user_code: Some(user_code.expose().to_owned()) }
                }
            },
            AuthControl::LoginCompleted { login_id, success, .. } => {
                Self::LoginCompleted { login_id, success }
            }
            AuthControl::AccountRead { id, account, requires_openai_auth } => Self::AccountRead {
                id, account: account.map(CodexObservedAccount::from_native), requires_openai_auth,
            },
            AuthControl::LoginCanceled { id, status } => Self::LoginCanceled {
                id, canceled: status == LoginCancelStatus::Canceled,
            },
            AuthControl::LoggedOut { id } => Self::LoggedOut { id },
            AuthControl::RequestFailed { id, method, .. } => Self::RequestFailed {
                id, method: method.to_owned(),
            },
        };
        observation.validate()?;
        Ok(observation)
    }

    pub(crate) fn request_id(&self) -> Option<i64> {
        match self {
            Self::LoginCompleted { .. } => None,
            Self::LoginStarted { id, .. } | Self::AccountRead { id, .. }
                | Self::LoginCanceled { id, .. } | Self::LoggedOut { id }
                | Self::RequestFailed { id, .. } => Some(*id),
        }
    }

    pub(crate) fn validate(&self) -> Result<(), EngineError> {
        if self.request_id().is_some_and(|id| id <= 0) {
            return Err(failure("native_auth_observation_invalid"));
        }
        match self {
            Self::LoginStarted { .. } => { self.challenge()?; }
            Self::AccountRead { account: Some(account), .. } => { account.native()?; }
            Self::LoginCompleted { login_id: Some(id), .. }
                if id.is_empty() || id.len() > 256 || id.trim().is_empty()
                    || id.chars().any(char::is_control) => {
                return Err(failure("native_auth_observation_invalid"));
            }
            Self::RequestFailed { method, .. } if !matches!(method.as_str(),
                "account/login/start" | "account/login/cancel" | "account/read" | "account/logout"
            ) => return Err(failure("native_auth_observation_invalid")),
            _ => {}
        }
        Ok(())
    }

    pub(crate) fn challenge(&self) -> Result<Option<crate::adapters::codex::LoginChallenge>, EngineError> {
        match self {
            Self::LoginStarted { login_id, url, user_code, .. } => {
                crate::adapters::codex::owner_login_challenge(login_id, url, user_code.as_deref())
                    .map(Some).map_err(|_| failure("native_auth_observation_invalid"))
            }
            _ => Ok(None),
        }
    }

    pub(crate) fn account(&self) -> Result<Option<crate::adapters::codex::AuthAccount>, EngineError> {
        match self {
            Self::AccountRead { account: Some(account), .. } => account.native().map(Some),
            _ => Ok(None),
        }
    }

    pub(crate) fn apply(&self, fact: &mut NativeAuthFact) -> Result<(), EngineError> {
        self.validate()?;
        self.validate_action(fact)?;
        if matches!(fact.phase, NativeAuthPhase::Uncertain | NativeAuthPhase::Unavailable) {
            return Err(failure("native_auth_recovery_required"));
        }
        if matches!(fact.phase, NativeAuthPhase::Failed | NativeAuthPhase::Cancelled)
            && matches!(self, Self::AccountRead { .. }) {
            return Ok(());
        }
        if fact.phase == NativeAuthPhase::Cancelling
            && !matches!(self, Self::LoginCanceled { .. } | Self::RequestFailed { .. }) {
            return Ok(());
        }
        let (phase, code) = match self {
            Self::LoginStarted { .. } if fact.phase == NativeAuthPhase::Starting => {
                (NativeAuthPhase::AwaitingOwner, None)
            }
            Self::LoginCompleted { success, .. } if fact.phase == NativeAuthPhase::AwaitingOwner => {
                if *success { (NativeAuthPhase::CheckingAccount, None) }
                else { (NativeAuthPhase::Failed, Some("native_auth_login_failed")) }
            }
            Self::AccountRead { account, .. }
                if matches!(fact.phase, NativeAuthPhase::Starting | NativeAuthPhase::CheckingAccount) => {
                if account.is_some() { (NativeAuthPhase::AuthenticatedObserved, None) }
                else { (NativeAuthPhase::UnauthenticatedObserved, None) }
            }
            Self::LoggedOut { .. } if fact.phase == NativeAuthPhase::Starting => {
                (NativeAuthPhase::CheckingAccount, None)
            }
            Self::LoginCanceled { canceled: true, .. } if fact.phase == NativeAuthPhase::Cancelling => {
                (NativeAuthPhase::Cancelled, None)
            }
            Self::LoginCanceled { canceled: false, .. } if fact.phase == NativeAuthPhase::Cancelling => {
                (NativeAuthPhase::Cancelling, None)
            }
            Self::RequestFailed { .. } if matches!(fact.phase, NativeAuthPhase::Starting
                | NativeAuthPhase::AwaitingOwner | NativeAuthPhase::CheckingAccount | NativeAuthPhase::Cancelling) => {
                (NativeAuthPhase::Failed, Some("native_auth_native_request_failed"))
            }
            _ => return Err(failure("native_auth_observation_order")),
        };
        fact.phase = phase;
        fact.code = code.map(str::to_owned);
        Ok(())
    }

    fn validate_action(&self, fact: &NativeAuthFact) -> Result<(), EngineError> {
        if fact.binding.connection.harness.as_str() != "codex" {
            return Err(failure("native_auth_method_mismatch"));
        }
        let valid = match (self, &fact.action) {
            (Self::LoginStarted { user_code, .. }, NativeAuthAction::SignIn { method }) => {
                matches!((user_code.is_some(), method),
                    (false, NativeAuthMethod::CodexBrowser) | (true, NativeAuthMethod::CodexDeviceCode))
            }
            (Self::LoginCompleted { .. } | Self::LoginCanceled { .. },
                NativeAuthAction::SignIn { method: NativeAuthMethod::CodexBrowser | NativeAuthMethod::CodexDeviceCode }) => true,
            (Self::LoggedOut { .. }, NativeAuthAction::Logout) => true,
            (Self::AccountRead { .. } | Self::RequestFailed { .. }, NativeAuthAction::Logout
                | NativeAuthAction::SignIn { method: NativeAuthMethod::CodexBrowser | NativeAuthMethod::CodexDeviceCode }) => true,
            _ => false,
        };
        if !valid { return Err(failure("native_auth_method_mismatch")); }
        Ok(())
    }
}

pub(crate) fn apply_codex_presentation(
    reply: &mut NativeAuthReply,
    challenge: Option<crate::adapters::codex::LoginChallenge>,
    account: Option<crate::adapters::codex::AuthAccount>,
) -> Result<(), EngineError> {
    reply.challenge = challenge.map(NativeAuthChallenge);
    reply.account = account.map(NativeAuthAccount::Codex);
    presentation::encode(reply)?;
    Ok(())
}

#[derive(Serialize, Deserialize)]
#[serde(
    tag = "command",
    content = "request",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub(crate) enum Command {
    Start(NativeAuthStart),
    Read(NativeAuthQuery),
    Cancel(NativeAuthCancel),
    Logout(NativeAuthLogout),
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Envelope {
    pub(crate) native_auth: Command,
}

impl Command {
    pub(crate) fn request_id(&self) -> &RequestId {
        match self {
            Self::Start(value) => &value.request_id,
            Self::Read(value) => &value.request_id,
            Self::Cancel(value) => &value.request_id,
            Self::Logout(value) => &value.request_id,
        }
    }

    pub(crate) fn operation_id(&self) -> &RequestId {
        match self {
            Self::Read(value) => &value.operation_id,
            Self::Cancel(value) => &value.operation_id,
            Self::Start(_) | Self::Logout(_) => self.request_id(),
        }
    }

    pub(crate) fn scope(&self) -> &NativeAuthScope {
        match self {
            Self::Start(value) => &value.scope,
            Self::Read(value) => &value.scope,
            Self::Cancel(value) => &value.scope,
            Self::Logout(value) => &value.scope,
        }
    }

    pub(crate) fn action(&self) -> Option<NativeAuthAction> {
        match self {
            Self::Start(value) => Some(NativeAuthAction::SignIn {
                method: value.method,
            }),
            Self::Logout(_) => Some(NativeAuthAction::Logout),
            Self::Read(_) | Self::Cancel(_) => None,
        }
    }

    pub(crate) fn request_bytes(&self) -> Result<Vec<u8>, EngineError> {
        let bytes = match self {
            Self::Start(value) => serde_json::to_vec(value),
            Self::Read(value) => serde_json::to_vec(value),
            Self::Cancel(value) => serde_json::to_vec(value),
            Self::Logout(value) => serde_json::to_vec(value),
        }
        .map_err(|_| failure("native_auth_request_bounds"))?;
        if bytes.len() > REQUEST_BYTES_MAX {
            return Err(failure("native_auth_request_bounds"));
        }
        Ok(bytes)
    }

    pub(crate) fn validate(&self) -> Result<(), EngineError> {
        self.request_bytes()?;
        if !is_hex_id(&self.scope().expected_source_hash) {
            return Err(failure("native_profile_source_invalid"));
        }
        Ok(())
    }
}

pub(crate) fn encode_fact(fact: &NativeAuthFact) -> Result<Vec<u8>, EngineError> {
    validate_fact(fact)?;
    let bytes = serde_json::to_vec(fact).map_err(|_| failure("native_auth_fact_invalid"))?;
    if bytes.len() > FACT_BYTES_MAX {
        return Err(failure("native_auth_fact_bounds"));
    }
    Ok(bytes)
}

pub(crate) fn decode_fact(bytes: &[u8]) -> Result<NativeAuthFact, EngineError> {
    if bytes.len() > FACT_BYTES_MAX {
        return Err(failure("native_auth_fact_bounds"));
    }
    let fact = serde_json::from_slice(bytes).map_err(|_| failure("native_auth_fact_invalid"))?;
    validate_fact(&fact)?;
    Ok(fact)
}

fn validate_fact(fact: &NativeAuthFact) -> Result<(), EngineError> {
    let binding = &fact.binding;
    if fact.generation == 0
        || !is_hex_id(&binding.database_instance)
        || !is_hex_id(&binding.engine_epoch)
        || !is_hex_id(&binding.source_hash)
        || !is_hex_id(&binding.custody_id)
        || fact.code.as_ref().is_some_and(|code| {
            code.len() > 128
                || !code
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte == b'_')
        })
    {
        return Err(failure("native_auth_fact_invalid"));
    }
    Ok(())
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
enum WireReply {
    Fact {
        request_id: RequestId,
        database_instance: String,
        engine_epoch: String,
        fact: Box<NativeAuthFact>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        presentation: Option<presentation::WirePresentation>,
    },
    Rejected {
        request_id: RequestId,
        code: String,
    },
    OutcomeUnknown {
        request_id: RequestId,
    },
}

pub(crate) fn encode_reply(
    result: Result<NativeAuthReply, EngineError>,
    request_id: RequestId,
) -> Result<Vec<u8>, EngineError> {
    let reply = match result {
        Ok(reply) => {
            encode_fact(&reply.fact)?;
            assert_eq!(reply.request_id, request_id);
            let presentation = presentation::encode(&reply)?;
            WireReply::Fact {
                request_id,
                database_instance: reply.database_instance,
                engine_epoch: reply.engine_epoch,
                fact: Box::new(reply.fact),
                presentation,
            }
        }
        Err(error) if public_error(&error.0) => WireReply::Rejected {
            request_id,
            code: error.0,
        },
        Err(_) => WireReply::OutcomeUnknown { request_id },
    };
    crate::protocol::encode_frame(&reply).map_err(|_| failure("native_auth_response_bounds"))
}

pub(crate) fn decode_reply(
    bytes: &[u8],
    command: &Command,
) -> Result<NativeAuthReply, EngineError> {
    if bytes.len() > crate::protocol::FRAME_BYTES_MAX {
        return Err(outcome_unknown());
    }
    let reply: WireReply = serde_json::from_slice(bytes).map_err(|_| outcome_unknown())?;
    match reply {
        WireReply::Fact {
            request_id,
            database_instance,
            engine_epoch,
            fact,
            presentation,
        } => {
            encode_fact(&fact).map_err(|_| outcome_unknown())?;
            let scope = command.scope();
            if &request_id != command.request_id()
                || database_instance != scope.expected_database_instance
                || !is_hex_id(&engine_epoch)
                || &fact.operation_id != command.operation_id()
                || fact.binding.database_instance != database_instance
                || fact.binding.connection.id != scope.connection
                || fact.binding.config_id != scope.expected_config_id
                || fact.binding.source_hash != scope.expected_source_hash
                || command.action().is_some_and(|action| action != fact.action)
            {
                return Err(outcome_unknown());
            }
            let mut reply = NativeAuthReply {
                request_id,
                database_instance,
                engine_epoch,
                fact: *fact,
                challenge: None,
                claude_challenge: None,
                account: None,
            };
            presentation::apply(presentation, &mut reply).map_err(|_| outcome_unknown())?;
            Ok(reply)
        }
        WireReply::Rejected { request_id, code }
            if &request_id == command.request_id() && public_error(&code) =>
        {
            Err(failure(&code))
        }
        WireReply::Rejected { .. } | WireReply::OutcomeUnknown { .. } => Err(outcome_unknown()),
    }
}

pub(crate) fn outcome_unknown() -> EngineError {
    failure("native_auth_outcome_unknown_read_same_operation")
}

fn public_error(code: &str) -> bool {
    matches!(
        code,
        "native_auth_request_bounds"
            | "native_auth_database_mismatch"
            | "native_auth_method_mismatch"
            | "native_auth_operation_conflict"
            | "native_auth_operation_scope_conflict"
            | "native_auth_operation_missing"
            | "native_auth_operation_capacity"
            | "native_auth_not_active"
            | "native_auth_recovery_required"
            | "native_auth_readonly"
            | "native_profile_source_invalid"
            | "native_profile_source_conflict"
            | "native_profile_connection_missing"
            | "native_profile_not_managed"
            | "native_profile_binding_conflict"
            | "native_profile_custody_unsafe"
            | "native_profile_incomplete"
            | "native_profile_identity_bounds"
            | "native_profile_identity_changed"
            | "native_profile_identity_invalid"
            | "native_profile_capacity"
            | "native_profile_new_home_not_empty"
            | "native_profile_in_use"
            | "native_profile_lock_unavailable"
            | "owner_grant_stale_or_revoked"
            | "owner_queue_not_submitted"
    )
}

mod binding_wire {
    use super::*;

    #[derive(Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Binding {
        database_instance: String,
        engine_epoch: String,
        config_id: ConfigId,
        source_hash: String,
        at_revision: Cursor,
        connection: ModelConnection,
        custody_id: String,
    }

    pub(super) fn serialize<S: serde::Serializer>(
        binding: &NativeProfileBinding,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        Binding {
            database_instance: binding.database_instance.clone(),
            engine_epoch: binding.engine_epoch.clone(),
            config_id: binding.config_id.clone(),
            source_hash: binding.source_hash.clone(),
            at_revision: binding.at_revision.clone(),
            connection: binding.connection.clone(),
            custody_id: binding.custody_id.clone(),
        }
        .serialize(serializer)
    }

    pub(super) fn deserialize<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> Result<NativeProfileBinding, D::Error> {
        let value = Binding::deserialize(deserializer)?;
        Ok(NativeProfileBinding {
            database_instance: value.database_instance,
            engine_epoch: value.engine_epoch,
            config_id: value.config_id,
            source_hash: value.source_hash,
            at_revision: value.at_revision,
            connection: value.connection,
            custody_id: value.custody_id,
        })
    }
}
