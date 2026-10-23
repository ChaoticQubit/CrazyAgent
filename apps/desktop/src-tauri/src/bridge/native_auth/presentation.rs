use super::{BridgeError, NativeAuthReply, NativeAuthView, bounded, failure};
use crazyagent::adapters::{claude, codex};
use crazyagent::engine::owner::{
    NativeAuthAccount, NativeAuthAction, NativeAuthMethod, NativeAuthPhase,
};
use serde::Serialize;

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Challenge {
    CodexBrowser {
        auth_url: String,
    },
    CodexDeviceCode {
        user_code: String,
        verification_url: String,
    },
    ClaudeOfficial {
        auth_url: String,
        accepts_code: bool,
        prompt_id: Option<String>,
    },
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Account {
    CodexApiKey,
    CodexChatgpt {
        email: Option<String>,
        plan_type: String,
    },
    CodexAmazonBedrock {
        uses_codex_managed_credentials: bool,
    },
    Claude(Box<ClaudeAccount>),
}

#[derive(Serialize)]
pub struct ClaudeAccount {
    logged_in: bool,
    auth_method: String,
    api_provider: String,
    config_directory: String,
    email: StatusField,
    org_id: StatusField,
    org_name: StatusField,
    subscription_type: StatusField,
}

#[derive(Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
enum StatusField {
    Missing,
    Null,
    Value { text: String },
}

pub(super) fn project(ticket: &str, reply: NativeAuthReply) -> Result<NativeAuthView, BridgeError> {
    let projected = if let Some(value) = &reply.claude_challenge {
        if reply.challenge.is_none()
            && reply.account.is_none()
            && reply.fact.phase == NativeAuthPhase::AwaitingOwner
            && reply.fact.action
                == (NativeAuthAction::SignIn {
                    method: NativeAuthMethod::ClaudeOfficial,
                })
            && reply.fact.binding.connection.harness.as_str() == "claude"
            && safe_claude_url(value.url())
        {
            Some(Challenge::ClaudeOfficial {
                auth_url: value.url().into(),
                accepts_code: value.accepts_code(),
                prompt_id: value.prompt_id().map(|id| id.to_string()),
            })
        } else {
            None
        }
    } else {
        reply
            .challenge
            .as_ref()
            .and_then(|value| challenge(value.codex()))
    };
    let has_challenge = reply.challenge.is_some() || reply.claude_challenge.is_some();
    let mut view = NativeAuthView {
        ticket: ticket.into(),
        request_id: reply.request_id.as_str().into(),
        operation_id: reply.fact.operation_id.as_str().into(),
        database_instance: reply.database_instance,
        engine_epoch: reply.engine_epoch,
        connection: reply.fact.binding.connection.id.as_str().into(),
        config_id: reply.fact.binding.config_id.as_str().into(),
        source_hash: reply.fact.binding.source_hash,
        generation: reply.fact.generation.to_string(),
        action: reply.fact.action,
        phase: reply.fact.phase,
        code: reply.fact.code,
        challenge: projected,
        account: reply.account.as_ref().map(account),
        display_error: None,
    };
    if has_challenge && view.challenge.is_none() {
        view.display_error = Some("native_auth_cannot_display_safely");
    }
    let length = serde_json::to_vec(&view)
        .map_err(|_| failure("projection_failed"))?
        .len();
    if length > super::super::PROJECTION_BYTES_MAX {
        view.challenge = None;
        view.account = None;
        view.display_error = Some("native_auth_presentation_limit");
    }
    bounded(view)
}

fn safe_claude_url(value: &str) -> bool {
    value.len() <= 8192
        && value
            .strip_prefix("https://claude.com/cai/oauth/authorize?")
            .is_some_and(|query| {
                !query.is_empty()
                    && query
                        .bytes()
                        .all(|byte| byte.is_ascii_graphic() && !matches!(byte, b'#' | b'\\'))
            })
}

fn exact(value: &str) -> bool {
    !value.chars().any(|c| {
        c.is_control()
            || matches!(c, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}'
                | '\u{2066}'..='\u{2069}')
    })
}

fn challenge(value: &codex::LoginChallenge) -> Option<Challenge> {
    match value {
        codex::LoginChallenge::Browser { auth_url, .. } if exact(auth_url.expose()) => {
            Some(Challenge::CodexBrowser {
                auth_url: auth_url.expose().into(),
            })
        }
        codex::LoginChallenge::DeviceCode {
            user_code,
            verification_url,
            ..
        } if exact(user_code.expose()) && exact(verification_url.expose()) => {
            Some(Challenge::CodexDeviceCode {
                user_code: user_code.expose().into(),
                verification_url: verification_url.expose().into(),
            })
        }
        _ => None,
    }
}

fn account(value: &NativeAuthAccount) -> Account {
    match value {
        NativeAuthAccount::Codex(codex::AuthAccount::ApiKey) => Account::CodexApiKey,
        NativeAuthAccount::Codex(codex::AuthAccount::Chatgpt { email, plan_type }) => {
            Account::CodexChatgpt {
                email: email.as_ref().map(|value| value.expose().into()),
                plan_type: plan_type.clone(),
            }
        }
        NativeAuthAccount::Codex(codex::AuthAccount::AmazonBedrock {
            uses_codex_managed_credentials,
        }) => Account::CodexAmazonBedrock {
            uses_codex_managed_credentials: *uses_codex_managed_credentials,
        },
        NativeAuthAccount::Claude(value) => Account::Claude(Box::new(claude_account(value))),
    }
}

fn claude_account(value: &claude::AuthStatus) -> ClaudeAccount {
    ClaudeAccount {
        logged_in: value.logged_in,
        auth_method: method(&value.auth_method).into(),
        api_provider: value.api_provider.as_str().into(),
        config_directory: value.config_directory.as_str().into(),
        email: status_field(&value.email),
        org_id: status_field(&value.org_id),
        org_name: status_field(&value.org_name),
        subscription_type: status_field(&value.subscription_type),
    }
}

fn method(value: &claude::AuthMethod) -> &str {
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

fn status_field(value: &claude::AuthStatusField) -> StatusField {
    match value {
        claude::AuthStatusField::Missing => StatusField::Missing,
        claude::AuthStatusField::Null => StatusField::Null,
        claude::AuthStatusField::Value(value) => StatusField::Value {
            text: value.as_str().into(),
        },
    }
}
