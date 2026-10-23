use super::{
    NativeAuthAccount, NativeAuthAction, NativeAuthChallenge, NativeAuthMethod, NativeAuthPhase,
    NativeAuthReply, failure,
};
use crate::adapters::{claude, codex};
use crate::config::{ModelAuthentication, ModelEndpoint};
use crate::engine::EngineError;
use serde::{Deserialize, Serialize};
use serde_json::json;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::engine) struct WirePresentation {
    challenge: Option<WireChallenge>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    claude_challenge: Option<crate::engine::native_auth_host::claude_auth::types::ClaudeAuthChallenge>,
    account: Option<WireAccount>,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum WireChallenge {
    CodexBrowser {
        login_id: String,
        auth_url: String,
    },
    CodexDeviceCode {
        login_id: String,
        user_code: String,
        verification_url: String,
    },
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum WireAccount {
    CodexApiKey,
    CodexChatgpt {
        email: Option<String>,
        plan_type: String,
    },
    CodexAmazonBedrock {
        uses_codex_managed_credentials: bool,
    },
    Claude(Box<ClaudeStatus>),
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ClaudeStatus {
    logged_in: bool,
    auth_method: String,
    api_provider: String,
    config_directory: String,
    email: StatusField,
    org_id: StatusField,
    org_name: StatusField,
    subscription_type: StatusField,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
enum StatusField {
    Missing,
    Null,
    Value { text: String },
}

fn invalid() -> EngineError {
    failure("native_auth_presentation_invalid")
}

pub(in crate::engine) fn encode(reply: &NativeAuthReply) -> Result<Option<WirePresentation>, EngineError> {
    validate(reply)?;
    if reply.challenge.is_none() && reply.claude_challenge.is_none() && reply.account.is_none() {
        return Ok(None);
    }
    let wire = WirePresentation {
        challenge: reply
            .challenge
            .as_ref()
            .map(|value| WireChallenge::from_native(value.codex())),
        claude_challenge: reply.claude_challenge.clone(),
        account: reply.account.as_ref().map(WireAccount::from_native),
    };
    if let Some(challenge) = &wire.challenge {
        challenge.materialize()?;
    }
    if let Some(account) = &wire.account {
        account.materialize()?;
    }
    Ok(Some(wire))
}

pub(super) fn apply(
    wire: Option<WirePresentation>,
    reply: &mut NativeAuthReply,
) -> Result<(), EngineError> {
    if let Some(wire) = wire {
        reply.claude_challenge = wire.claude_challenge;
        reply.challenge = wire
            .challenge
            .as_ref()
            .map(WireChallenge::materialize)
            .transpose()?;
        reply.account = wire
            .account
            .as_ref()
            .map(WireAccount::materialize)
            .transpose()?;
    }
    validate(reply)
}

fn validate(reply: &NativeAuthReply) -> Result<(), EngineError> {
    if reply.challenge.is_none() && reply.claude_challenge.is_none() && reply.account.is_none() {
        return Ok(());
    }
    let connection = &reply.fact.binding.connection;
    if (usize::from(reply.challenge.is_some()) + usize::from(reply.claude_challenge.is_some()) + usize::from(reply.account.is_some())) > 1
        || reply.fact.binding.engine_epoch != reply.engine_epoch
        || connection.protocol.as_str() != "native"
        || !matches!(connection.endpoint, ModelEndpoint::Native {})
        || !matches!(connection.auth, ModelAuthentication::NativeManaged { .. })
    {
        return Err(invalid());
    }
    if let NativeAuthAction::SignIn { method } = reply.fact.action {
        let expected = match method {
            NativeAuthMethod::CodexBrowser | NativeAuthMethod::CodexDeviceCode
            | NativeAuthMethod::CodexExisting => "codex",
            NativeAuthMethod::ClaudeOfficial => "claude",
        };
        if connection.harness.as_str() != expected {
            return Err(invalid());
        }
    }
    if let Some(challenge) = &reply.challenge {
        validate_challenge(reply, challenge)?;
    }
    if let Some(challenge) = &reply.claude_challenge {
        challenge.validate()?;
        if reply.fact.phase != NativeAuthPhase::AwaitingOwner
            || connection.harness.as_str() != "claude"
            || reply.fact.action != (NativeAuthAction::SignIn { method: NativeAuthMethod::ClaudeOfficial }) {
            return Err(invalid());
        }
    }
    if let Some(account) = &reply.account {
        validate_account(reply, account)?;
    }
    Ok(())
}

fn validate_challenge(
    reply: &NativeAuthReply,
    challenge: &NativeAuthChallenge,
) -> Result<(), EngineError> {
    let method = match challenge.codex() {
        codex::LoginChallenge::Browser { .. } => NativeAuthMethod::CodexBrowser,
        codex::LoginChallenge::DeviceCode { .. } => NativeAuthMethod::CodexDeviceCode,
    };
    if reply.fact.phase != NativeAuthPhase::AwaitingOwner
        || reply.fact.binding.connection.harness.as_str() != "codex"
        || reply.fact.action != (NativeAuthAction::SignIn { method })
    {
        return Err(invalid());
    }
    Ok(())
}

fn validate_account(
    reply: &NativeAuthReply,
    account: &NativeAuthAccount,
) -> Result<(), EngineError> {
    let (harness, phase) = match account {
        NativeAuthAccount::Codex(_) => ("codex", NativeAuthPhase::AuthenticatedObserved),
        NativeAuthAccount::Claude(status) => (
            "claude",
            if status.logged_in {
                NativeAuthPhase::AuthenticatedObserved
            } else {
                NativeAuthPhase::UnauthenticatedObserved
            },
        ),
    };
    if reply.fact.binding.connection.harness.as_str() != harness || reply.fact.phase != phase {
        return Err(invalid());
    }
    Ok(())
}

impl WireChallenge {
    fn from_native(value: &codex::LoginChallenge) -> Self {
        match value {
            codex::LoginChallenge::Browser { login_id, auth_url } => Self::CodexBrowser {
                login_id: login_id.clone(),
                auth_url: auth_url.expose().into(),
            },
            codex::LoginChallenge::DeviceCode {
                login_id,
                user_code,
                verification_url,
            } => Self::CodexDeviceCode {
                login_id: login_id.clone(),
                user_code: user_code.expose().into(),
                verification_url: verification_url.expose().into(),
            },
        }
    }

    fn materialize(&self) -> Result<NativeAuthChallenge, EngineError> {
        let (login_id, url, user_code) = match self {
            Self::CodexBrowser { login_id, auth_url } => (login_id, auth_url, None),
            Self::CodexDeviceCode {
                login_id,
                user_code,
                verification_url,
            } => (login_id, verification_url, Some(user_code.as_str())),
        };
        codex::owner_login_challenge(login_id, url, user_code)
            .map(NativeAuthChallenge)
            .map_err(|_| invalid())
    }
}

impl WireAccount {
    fn from_native(value: &NativeAuthAccount) -> Self {
        match value {
            NativeAuthAccount::Codex(codex::AuthAccount::ApiKey) => Self::CodexApiKey,
            NativeAuthAccount::Codex(codex::AuthAccount::Chatgpt { email, plan_type }) => {
                Self::CodexChatgpt {
                    email: email.as_ref().map(|value| value.expose().into()),
                    plan_type: plan_type.clone(),
                }
            }
            NativeAuthAccount::Codex(codex::AuthAccount::AmazonBedrock {
                uses_codex_managed_credentials,
            }) => Self::CodexAmazonBedrock {
                uses_codex_managed_credentials: *uses_codex_managed_credentials,
            },
            NativeAuthAccount::Claude(value) => {
                Self::Claude(Box::new(ClaudeStatus::from_native(value)))
            }
        }
    }

    fn materialize(&self) -> Result<NativeAuthAccount, EngineError> {
        let value = match self {
            Self::CodexApiKey => json!({"type": "apiKey"}),
            Self::CodexChatgpt { email, plan_type } => {
                json!({"type": "chatgpt", "email": email, "planType": plan_type})
            }
            Self::CodexAmazonBedrock {
                uses_codex_managed_credentials,
            } => json!({
                "type": "amazonBedrock",
                "usesCodexManagedCredentials": uses_codex_managed_credentials
            }),
            Self::Claude(value) => return value.materialize().map(NativeAuthAccount::Claude),
        };
        codex::auth_account(&value)
            .map(NativeAuthAccount::Codex)
            .map_err(|_| invalid())
    }
}

impl ClaudeStatus {
    pub(crate) fn from_native(value: &claude::AuthStatus) -> Self {
        Self {
            logged_in: value.logged_in,
            auth_method: method_name(&value.auth_method).into(),
            api_provider: value.api_provider.as_str().into(),
            config_directory: value.config_directory.as_str().into(),
            email: StatusField::from_native(&value.email),
            org_id: StatusField::from_native(&value.org_id),
            org_name: StatusField::from_native(&value.org_name),
            subscription_type: StatusField::from_native(&value.subscription_type),
        }
    }

    pub(crate) fn materialize(&self) -> Result<claude::AuthStatus, EngineError> {
        Ok(claude::AuthStatus {
            logged_in: self.logged_in,
            auth_method: method(&self.auth_method)?,
            api_provider: text(&self.api_provider)?,
            config_directory: text(&self.config_directory)?,
            email: self.email.materialize()?,
            org_id: self.org_id.materialize()?,
            org_name: self.org_name.materialize()?,
            subscription_type: self.subscription_type.materialize()?,
        })
    }
}

impl StatusField {
    fn from_native(value: &claude::AuthStatusField) -> Self {
        match value {
            claude::AuthStatusField::Missing => Self::Missing,
            claude::AuthStatusField::Null => Self::Null,
            claude::AuthStatusField::Value(value) => Self::Value {
                text: value.as_str().into(),
            },
        }
    }

    fn materialize(&self) -> Result<claude::AuthStatusField, EngineError> {
        Ok(match self {
            Self::Missing => claude::AuthStatusField::Missing,
            Self::Null => claude::AuthStatusField::Null,
            Self::Value { text: value } => claude::AuthStatusField::Value(text(value)?),
        })
    }
}

fn text(value: &str) -> Result<claude::AuthStatusText, EngineError> {
    claude::AuthStatusText::from_owner_wire(value.into()).map_err(|_| invalid())
}

fn method_name(value: &claude::AuthMethod) -> &str {
    match value {
        claude::AuthMethod::None => "none",
        claude::AuthMethod::ClaudeAi => "claude.ai",
        claude::AuthMethod::ApiKey => "api_key",
        claude::AuthMethod::OauthToken => "oauth_token",
        claude::AuthMethod::ApiKeyHelper => "api_key_helper",
        claude::AuthMethod::ThirdParty => "third_party",
        claude::AuthMethod::Unknown(value) => value.as_str(),
    }
}

fn method(value: &str) -> Result<claude::AuthMethod, EngineError> {
    Ok(match value {
        "none" => claude::AuthMethod::None,
        "claude.ai" => claude::AuthMethod::ClaudeAi,
        "api_key" => claude::AuthMethod::ApiKey,
        "oauth_token" => claude::AuthMethod::OauthToken,
        "api_key_helper" => claude::AuthMethod::ApiKeyHelper,
        "third_party" => claude::AuthMethod::ThirdParty,
        unknown => claude::AuthMethod::Unknown(text(unknown)?),
    })
}
