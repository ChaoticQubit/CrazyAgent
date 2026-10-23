use crate::adapters::claude::{AuthMethod, AuthStatus};
use crate::engine::owner::native_auth::presentation::ClaudeStatus;
use crate::engine::owner::{AccountSecret, NativeAuthAction, NativeAuthFact, NativeAuthMethod, NativeAuthPhase};
use crate::engine::EngineError;
use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClaudeAuthChallenge {
    url: String,
    accepts_code: bool,
}

impl ClaudeAuthChallenge {
    pub(crate) fn new(url: String, accepts_code: bool) -> Result<Self, EngineError> {
        let query = url.strip_prefix("https://claude.com/cai/oauth/authorize?")
            .filter(|query| !query.is_empty())
            .ok_or_else(|| failure("native_auth_claude_destination_unsupported"))?;
        if url.len() > 8192 || query.bytes().any(|byte| !byte.is_ascii() || byte <= b' ' || matches!(byte, b'#' | b'\\' | 0x7f)) {
            return Err(failure("native_auth_claude_challenge_invalid"));
        }
        Ok(Self { url, accepts_code })
    }

    pub fn url(&self) -> &str { &self.url }

    pub fn accepts_code(&self) -> bool { self.accepts_code }

    pub fn prompt_id(&self) -> Option<u64> { self.accepts_code.then_some(1) }

    pub(crate) fn validate(&self) -> Result<(), EngineError> {
        Self::new(self.url.clone(), self.accepts_code).map(|_| ())
    }
}

impl fmt::Debug for ClaudeAuthChallenge {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output.write_str("ClaudeAuthChallenge([REDACTED])")
    }
}

#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ClaudeAuthFailure {
    NativeExit,
    StatusInvalid,
    SubscriptionUnsupported,
    ProfileMismatch,
    OutputInvalid,
    Deadline,
    LaunchFailed,
    InputInvalid,
    DestinationUnsupported,
}

impl ClaudeAuthFailure {
    pub(crate) fn code(self) -> &'static str {
        match self {
            Self::NativeExit => "native_auth_claude_native_exit",
            Self::StatusInvalid => "native_auth_claude_status_invalid",
            Self::SubscriptionUnsupported => "native_auth_claude_subscription_unsupported",
            Self::ProfileMismatch => "native_auth_claude_profile_mismatch",
            Self::OutputInvalid => "native_auth_claude_output_invalid",
            Self::Deadline => "native_auth_claude_deadline",
            Self::LaunchFailed => "native_auth_claude_launch_failed",
            Self::InputInvalid => "native_auth_claude_input_invalid",
            Self::DestinationUnsupported => "native_auth_claude_destination_unsupported",
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum ClaudeAuthObservation {
    Challenge { challenge: ClaudeAuthChallenge },
    CheckingAccount,
    Status { status: ClaudeStatus, exit: i32 },
    Failed { reason: ClaudeAuthFailure },
    Cancelled,
}

impl fmt::Debug for ClaudeAuthObservation {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output.write_str("ClaudeAuthObservation([REDACTED])")
    }
}

impl ClaudeAuthObservation {
    pub(crate) fn status(status: &AuthStatus, exit: i32) -> Self {
        Self::Status { status: ClaudeStatus::from_native(status), exit }
    }

    pub(crate) fn account(&self) -> Result<Option<AuthStatus>, EngineError> {
        match self {
            Self::Status { status, .. } => status.materialize().map(Some),
            _ => Ok(None),
        }
    }

    pub(crate) fn apply(&self, fact: &mut NativeAuthFact) -> Result<(), EngineError> {
        if fact.binding.connection.harness.as_str() != "claude"
            || !matches!(fact.action, NativeAuthAction::SignIn { method: NativeAuthMethod::ClaudeOfficial } | NativeAuthAction::Logout) {
            return Err(failure("native_auth_method_mismatch"));
        }
        if matches!(fact.phase, NativeAuthPhase::Uncertain | NativeAuthPhase::Unavailable) {
            return Err(failure("native_auth_recovery_required"));
        }
        if fact.phase == NativeAuthPhase::Cancelling && !matches!(self, Self::Cancelled) {
            return Ok(());
        }
        let phase = match self {
            Self::Challenge { challenge } if matches!(fact.action, NativeAuthAction::SignIn { .. })
                && matches!(fact.phase, NativeAuthPhase::Starting | NativeAuthPhase::AwaitingOwner) => {
                challenge.validate()?;
                NativeAuthPhase::AwaitingOwner
            }
            Self::CheckingAccount if matches!(fact.phase, NativeAuthPhase::Starting | NativeAuthPhase::AwaitingOwner) => NativeAuthPhase::CheckingAccount,
            Self::Status { status, exit } if fact.phase == NativeAuthPhase::CheckingAccount => {
                let status = status.materialize()?;
                if status.logged_in && status.auth_method == AuthMethod::ClaudeAi
                    && status.api_provider.as_str() == "firstParty" && *exit == 0 {
                    if matches!(fact.action, NativeAuthAction::Logout) {
                        return Err(failure("native_auth_claude_logout_not_observed"));
                    }
                    NativeAuthPhase::AuthenticatedObserved
                } else if !status.logged_in && status.auth_method == AuthMethod::None && *exit == 1 {
                    NativeAuthPhase::UnauthenticatedObserved
                } else {
                    return Err(failure("native_auth_claude_status_invalid"));
                }
            }
            Self::Failed { .. } if matches!(fact.phase, NativeAuthPhase::Starting | NativeAuthPhase::AwaitingOwner | NativeAuthPhase::CheckingAccount) => NativeAuthPhase::Failed,
            Self::Cancelled if matches!(fact.phase, NativeAuthPhase::Starting | NativeAuthPhase::AwaitingOwner | NativeAuthPhase::CheckingAccount | NativeAuthPhase::Cancelling) => NativeAuthPhase::Cancelled,
            _ => return Err(failure("native_auth_observation_order")),
        };
        fact.phase = phase;
        fact.code = match self { Self::Failed { reason } => Some(reason.code().into()), _ => None };
        Ok(())
    }
}

pub(crate) fn completion_bytes(secret: AccountSecret) -> Result<Vec<u8>, EngineError> {
    let mut bytes = secret.into_bytes();
    let valid = bytes.len() <= 8192 && bytes.iter().all(|byte| byte.is_ascii_graphic())
        && bytes.iter().filter(|byte| **byte == b'#').count() == 1
        && bytes.first() != Some(&b'#') && bytes.last() != Some(&b'#');
    if !valid {
        bytes.fill(0);
        return Err(failure("native_auth_claude_input_invalid"));
    }
    bytes.push(b'\n');
    Ok(bytes)
}

fn failure(code: &str) -> EngineError { EngineError(code.into()) }
