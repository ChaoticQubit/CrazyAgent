use super::{EngineError, NativeProfileBinding, failure, is_hex_id};
use crate::config::ModelConnection;
use crate::protocol::{ConfigId, CredentialRef, Cursor, RequestId};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::time::Duration;

pub(crate) mod presentation;

#[cfg(test)]
mod presentation_tests;

pub(crate) const REQUEST_BYTES_MAX: usize = 8192;
pub(crate) const FACT_BYTES_MAX: usize = 16384;
pub(crate) const OPERATIONS_MAX: i64 = 1024;
pub const CODEX_DEVICE_CODE_LIFE: Duration = Duration::from_secs(15 * 60);
pub(crate) const CODEX_LOG_DROPPED_MAX: usize = 8;
pub(crate) const CODEX_LOG_NAME_BYTES_MAX: usize = 64;
pub(crate) const CODEX_LOG_STDERR_BYTES_MAX: usize = 512;

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
    CodexExisting,
    ClaudeOfficial,
    ClaudeExisting,
}

impl NativeAuthMethod {
    /// How long the engine keeps one sign-in of this method open: a minute past the life of a
    /// ChatGPT device code, so the engine never gives up on a code Codex and OpenAI still take,
    /// and ten minutes for every other method. Input: the method. Output: the window.
    pub(crate) fn sign_in_window(self) -> Duration {
        let window = match self {
            Self::CodexDeviceCode => CODEX_DEVICE_CODE_LIFE + Duration::from_secs(60),
            Self::CodexBrowser
            | Self::CodexExisting
            | Self::ClaudeOfficial
            | Self::ClaudeExisting => Duration::from_secs(600),
        };
        assert!(window >= Duration::from_secs(600));
        window
    }

    /// The method one harness runs for one way to sign in (DEC-141 PA-13), None where the
    /// harness has no such way: Claude Code has no sign-in code and a harness without managed
    /// sign-in, such as RCLI, has none at all. Inputs: the connection's harness (`codex`,
    /// `claude` or any other) and the way. Output: the method.
    pub fn for_way(harness: &str, way: SignInWay) -> Option<Self> {
        match (harness, way) {
            ("codex", SignInWay::Browser) => Some(Self::CodexBrowser),
            ("codex", SignInWay::Code) => Some(Self::CodexDeviceCode),
            ("codex", SignInWay::Check) => Some(Self::CodexExisting),
            ("claude", SignInWay::Browser) => Some(Self::ClaudeOfficial),
            ("claude", SignInWay::Check) => Some(Self::ClaudeExisting),
            _ => None,
        }
    }

    /// The way to sign in this method is for the owner. Input: the method. Output: the way.
    pub fn way(self) -> SignInWay {
        match self {
            Self::CodexBrowser | Self::ClaudeOfficial => SignInWay::Browser,
            Self::CodexDeviceCode => SignInWay::Code,
            Self::CodexExisting | Self::ClaudeExisting => SignInWay::Check,
        }
    }

    /// The connection harness this method signs in. Input: the method. Output: `codex` or
    /// `claude`.
    pub fn harness(self) -> &'static str {
        match self {
            Self::CodexBrowser | Self::CodexDeviceCode | Self::CodexExisting => "codex",
            Self::ClaudeOfficial | Self::ClaudeExisting => "claude",
        }
    }
}

/// One way the owner signs a subscription account in, the same app feature for every harness
/// (DEC-141 PA-13): in the browser, with a code entered on any device, or a check of the
/// sign-in the account already has in CrazyAgent's own profile for it. A check never reads a
/// sign-in outside that profile.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SignInWay {
    Browser,
    Code,
    Check,
}

impl SignInWay {
    /// Every way, in the order the clients offer them.
    pub const ALL: [Self; 3] = [Self::Browser, Self::Code, Self::Check];

    /// Why one harness cannot offer this way, as the sign-in screens say it. Input: the
    /// connection's harness. Output: the sentence, None when the harness offers the way.
    pub fn unavailable(self, harness: &str) -> Option<&'static str> {
        if NativeAuthMethod::for_way(harness, self).is_some() {
            return None;
        }
        Some(match (harness, self) {
            ("claude", Self::Code) => {
                "Claude Code has no sign-in code. It signs in through your browser."
            }
            _ => "This account kind has no managed sign-in.",
        })
    }
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

impl NativeAuthAction {
    /// The lock this action takes on the account's profile: a shared check for a check of
    /// an existing sign-in, which only reads or renews the account in place and so runs
    /// beside its agents, and exclusive for a sign-in or logout, which changes it.
    /// Input: the action. Output: its profile purpose.
    pub(crate) fn profile_purpose(&self) -> crate::engine::bootstrap::NativeProfileUsePurpose {
        use crate::engine::bootstrap::NativeProfileUsePurpose;
        match self {
            Self::SignIn {
                method: NativeAuthMethod::ClaudeExisting | NativeAuthMethod::CodexExisting,
            } => NativeProfileUsePurpose::Check,
            Self::SignIn { .. } => NativeProfileUsePurpose::Authentication,
            Self::Logout => NativeProfileUsePurpose::Logout,
        }
    }
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

    /// Tells whether an operation in this phase is still running and may yet change its
    /// account. Input: the phase. Output: true for starting, awaiting the owner, checking the
    /// account and cancelling; every other phase has ended or needs recovery.
    pub fn unfinished(self) -> bool {
        matches!(
            self,
            Self::Starting | Self::AwaitingOwner | Self::CheckingAccount | Self::Cancelling
        )
    }

    /// Names the phase an operation shows while its custodian still holds the profile after
    /// committing its outcome, so no reader takes the account as settled before it is free.
    /// Input: the committed phase. Output: cancelling for a cancellation, checking the
    /// account for an observed outcome or a failure, and any other phase unchanged.
    pub(crate) fn releasing(self) -> Self {
        let shown = match self {
            Self::Cancelled => Self::Cancelling,
            Self::AuthenticatedObserved | Self::UnauthenticatedObserved | Self::Failed => {
                Self::CheckingAccount
            }
            other => other,
        };
        assert!(shown == self || shown.unfinished());
        shown
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

pub struct NativeAuthChallenge {
    codex: crate::adapters::codex::LoginChallenge,
    left: Option<Duration>,
}

impl NativeAuthChallenge {
    /// Pairs a Codex sign-in challenge with the time its device code has left, which only a
    /// device code carries. Inputs: the challenge and the time left. Output: the challenge.
    pub(crate) fn new(
        codex: crate::adapters::codex::LoginChallenge,
        left: Option<Duration>,
    ) -> Self {
        let left = match codex {
            crate::adapters::codex::LoginChallenge::DeviceCode { .. } => left,
            crate::adapters::codex::LoginChallenge::Browser { .. } => None,
        };
        assert!(left.is_none_or(|left| left <= CODEX_DEVICE_CODE_LIFE));
        Self { codex, left }
    }

    pub fn codex(&self) -> &crate::adapters::codex::LoginChallenge {
        &self.codex
    }

    /// How long the device code on screen still works, as the engine counted it when it
    /// answered. Input: the challenge. Output: the time left, or none for a browser page.
    pub fn left(&self) -> Option<Duration> {
        self.left
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
    pub claude_challenge:
        Option<crate::engine::native_auth_host::claude_auth::types::ClaudeAuthChallenge>,
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
            .field(
                "claude_challenge",
                &self.claude_challenge.as_ref().map(|_| "[REDACTED]"),
            )
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
    LoginCompleted {
        login_id: Option<String>,
        success: bool,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        code_ran_out: bool,
    },
    AccountRead {
        id: i64,
        account: Option<CodexObservedAccount>,
        requires_openai_auth: bool,
    },
    LoginCanceled {
        id: i64,
        canceled: bool,
    },
    LoggedOut {
        id: i64,
    },
    RequestFailed {
        id: i64,
        method: String,
    },
    HostFailed {
        reason: CodexHostFailure,
    },
    HostCancelled,
}

/// What the custodian saw of Codex when a sign-in failed or ran out of time, for the engine
/// log: the names of the notifications it dropped and a masked tail of Codex stderr, never
/// their params or any code.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CodexLog {
    pub(crate) dropped: Vec<String>,
    pub(crate) stderr: String,
}

impl CodexLog {
    /// Checks the log stays within its bounds and holds only plain printable text, so the
    /// engine log never takes a control byte or an unbounded line. Input: the log. Output:
    /// done, or `native_auth_codex_log_invalid`.
    pub(crate) fn validate(&self) -> Result<(), EngineError> {
        let name = |name: &String| {
            !name.is_empty()
                && name.len() <= CODEX_LOG_NAME_BYTES_MAX
                && name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"/_.-*".contains(&byte))
        };
        if self.dropped.len() > CODEX_LOG_DROPPED_MAX
            || !self.dropped.iter().all(name)
            || self.stderr.len() > CODEX_LOG_STDERR_BYTES_MAX
            || !self
                .stderr
                .bytes()
                .all(|byte| byte.is_ascii_graphic() || byte == b' ')
        {
            return Err(failure("native_auth_codex_log_invalid"));
        }
        Ok(())
    }
}

/// Why the custodian saw a started Codex child end without an app-server outcome: it
/// exited (or closed its input) first, the sign-in ran out of time, it broke the
/// app-server protocol, or its pipes failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CodexHostFailure {
    ExitedBeforeObservation,
    Deadline,
    ProtocolInvalid,
    IoFailed,
}

impl CodexHostFailure {
    /// The failure code the sign-in settles with. Output: the code.
    pub(crate) fn code(self) -> &'static str {
        match self {
            Self::ExitedBeforeObservation => "native_auth_codex_exited_before_observation",
            Self::Deadline => "native_auth_codex_deadline",
            Self::ProtocolInvalid => "native_auth_codex_protocol_invalid",
            Self::IoFailed => "native_auth_codex_io_failed",
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase", deny_unknown_fields)]
pub(crate) enum CodexObservedAccount {
    ApiKey,
    Chatgpt {
        email: Option<String>,
        #[serde(rename = "planType")]
        plan_type: String,
    },
    AmazonBedrock {
        #[serde(rename = "usesCodexManagedCredentials")]
        uses_codex_managed_credentials: bool,
    },
}

impl CodexObservedAccount {
    #[cfg(unix)]
    fn from_native(account: crate::adapters::codex::AuthAccount) -> Self {
        use crate::adapters::codex::AuthAccount;
        match account {
            AuthAccount::ApiKey => Self::ApiKey,
            AuthAccount::Chatgpt { email, plan_type } => Self::Chatgpt {
                email: email.map(|email| email.expose().to_owned()),
                plan_type,
            },
            AuthAccount::AmazonBedrock {
                uses_codex_managed_credentials,
            } => Self::AmazonBedrock {
                uses_codex_managed_credentials,
            },
        }
    }

    fn native(&self) -> Result<crate::adapters::codex::AuthAccount, EngineError> {
        let value =
            serde_json::to_value(self).map_err(|_| failure("native_auth_observation_invalid"))?;
        crate::adapters::codex::auth_account(&value)
            .map_err(|_| failure("native_auth_observation_invalid"))
    }
}

impl CodexAuthObservation {
    #[cfg(unix)]
    pub(crate) fn from_native(
        event: crate::adapters::codex::AuthControl,
    ) -> Result<Self, EngineError> {
        use crate::adapters::codex::{AuthControl, LoginCancelStatus, LoginChallenge};
        let observation = match event {
            AuthControl::LoginStarted { id, challenge } => match challenge {
                LoginChallenge::Browser { login_id, auth_url } => Self::LoginStarted {
                    id,
                    login_id,
                    url: auth_url.expose().to_owned(),
                    user_code: None,
                },
                LoginChallenge::DeviceCode {
                    login_id,
                    user_code,
                    verification_url,
                } => Self::LoginStarted {
                    id,
                    login_id,
                    url: verification_url.expose().to_owned(),
                    user_code: Some(user_code.expose().to_owned()),
                },
            },
            AuthControl::LoginCompleted {
                login_id, success, ..
            } => Self::LoginCompleted {
                login_id,
                success,
                code_ran_out: false,
            },
            AuthControl::AccountRead {
                id,
                account,
                requires_openai_auth,
            } => Self::AccountRead {
                id,
                account: account.map(CodexObservedAccount::from_native),
                requires_openai_auth,
            },
            AuthControl::LoginCanceled { id, status } => Self::LoginCanceled {
                id,
                canceled: status == LoginCancelStatus::Canceled,
            },
            AuthControl::LoggedOut { id } => Self::LoggedOut { id },
            AuthControl::RequestFailed { id, method, .. } => Self::RequestFailed {
                id,
                method: method.to_owned(),
            },
        };
        observation.validate()?;
        Ok(observation)
    }

    pub(crate) fn request_id(&self) -> Option<i64> {
        match self {
            Self::LoginCompleted { .. } | Self::HostFailed { .. } | Self::HostCancelled => None,
            Self::LoginStarted { id, .. }
            | Self::AccountRead { id, .. }
            | Self::LoginCanceled { id, .. }
            | Self::LoggedOut { id }
            | Self::RequestFailed { id, .. } => Some(*id),
        }
    }

    pub(crate) fn validate(&self) -> Result<(), EngineError> {
        if self.request_id().is_some_and(|id| id <= 0) {
            return Err(failure("native_auth_observation_invalid"));
        }
        match self {
            Self::LoginStarted { .. } => {
                self.challenge()?;
            }
            Self::AccountRead {
                account: Some(account),
                ..
            } => {
                account.native()?;
            }
            Self::LoginCompleted {
                login_id: Some(id), ..
            } if id.is_empty()
                || id.len() > 256
                || id.trim().is_empty()
                || id.chars().any(char::is_control) =>
            {
                return Err(failure("native_auth_observation_invalid"));
            }
            Self::RequestFailed { method, .. }
                if !matches!(
                    method.as_str(),
                    "account/login/start"
                        | "account/login/cancel"
                        | "account/read"
                        | "account/logout"
                ) =>
            {
                return Err(failure("native_auth_observation_invalid"));
            }
            _ => {}
        }
        Ok(())
    }

    pub(crate) fn challenge(
        &self,
    ) -> Result<Option<crate::adapters::codex::LoginChallenge>, EngineError> {
        match self {
            Self::LoginStarted {
                login_id,
                url,
                user_code,
                ..
            } => crate::adapters::codex::owner_login_challenge(login_id, url, user_code.as_deref())
                .map(Some)
                .map_err(|_| failure("native_auth_observation_invalid")),
            _ => Ok(None),
        }
    }

    pub(crate) fn account(
        &self,
    ) -> Result<Option<crate::adapters::codex::AuthAccount>, EngineError> {
        match self {
            Self::AccountRead {
                account: Some(account),
                ..
            } => account.native().map(Some),
            _ => Ok(None),
        }
    }

    pub(crate) fn apply(&self, fact: &mut NativeAuthFact) -> Result<(), EngineError> {
        self.validate()?;
        self.validate_action(fact)?;
        if matches!(
            fact.phase,
            NativeAuthPhase::Uncertain | NativeAuthPhase::Unavailable
        ) {
            return Err(failure("native_auth_recovery_required"));
        }
        if matches!(
            fact.phase,
            NativeAuthPhase::Failed | NativeAuthPhase::Cancelled
        ) && matches!(
            self,
            Self::AccountRead { .. }
                | Self::LoginCompleted { success: false, .. }
                | Self::HostFailed { .. }
                | Self::HostCancelled
        ) {
            return Ok(());
        }
        if fact.phase == NativeAuthPhase::Cancelling
            && !matches!(
                self,
                Self::LoginCanceled { .. } | Self::RequestFailed { .. } | Self::HostCancelled
            )
        {
            return Ok(());
        }
        let (phase, code) = match self {
            Self::LoginStarted { .. } if fact.phase == NativeAuthPhase::Starting => {
                (NativeAuthPhase::AwaitingOwner, None)
            }
            Self::LoginCompleted { success: true, .. }
                if fact.phase == NativeAuthPhase::AwaitingOwner =>
            {
                (NativeAuthPhase::CheckingAccount, None)
            }
            Self::LoginCompleted {
                code_ran_out: true, ..
            } if fact.phase == NativeAuthPhase::AwaitingOwner
                && fact.action
                    == (NativeAuthAction::SignIn {
                        method: NativeAuthMethod::CodexDeviceCode,
                    }) =>
            {
                (
                    NativeAuthPhase::Failed,
                    Some("native_auth_sign_in_timed_out"),
                )
            }
            Self::LoginCompleted { .. } if fact.phase == NativeAuthPhase::AwaitingOwner => {
                (NativeAuthPhase::Failed, Some("native_auth_login_failed"))
            }
            Self::AccountRead { account, .. }
                if matches!(
                    fact.phase,
                    NativeAuthPhase::Starting | NativeAuthPhase::CheckingAccount
                ) =>
            {
                if account.is_some() {
                    (NativeAuthPhase::AuthenticatedObserved, None)
                } else {
                    (NativeAuthPhase::UnauthenticatedObserved, None)
                }
            }
            Self::LoggedOut { .. } if fact.phase == NativeAuthPhase::Starting => {
                (NativeAuthPhase::CheckingAccount, None)
            }
            Self::LoginCanceled { canceled: true, .. }
                if fact.phase == NativeAuthPhase::Cancelling =>
            {
                (NativeAuthPhase::Cancelled, None)
            }
            Self::LoginCanceled {
                canceled: false, ..
            } if fact.phase == NativeAuthPhase::Cancelling => (NativeAuthPhase::Cancelling, None),
            Self::LoginCanceled { canceled: true, .. }
                if fact.phase == NativeAuthPhase::AwaitingOwner
                    && fact.action
                        == (NativeAuthAction::SignIn {
                            method: NativeAuthMethod::CodexBrowser,
                        }) =>
            {
                (
                    NativeAuthPhase::Failed,
                    Some("native_auth_sign_in_page_timed_out"),
                )
            }
            Self::LoginCanceled { canceled: true, .. }
                if fact.phase == NativeAuthPhase::AwaitingOwner =>
            {
                (
                    NativeAuthPhase::Failed,
                    Some("native_auth_sign_in_timed_out"),
                )
            }
            Self::RequestFailed { .. }
                if matches!(
                    fact.phase,
                    NativeAuthPhase::Starting
                        | NativeAuthPhase::AwaitingOwner
                        | NativeAuthPhase::CheckingAccount
                        | NativeAuthPhase::Cancelling
                ) =>
            {
                (
                    NativeAuthPhase::Failed,
                    Some("native_auth_native_request_failed"),
                )
            }
            Self::HostFailed { reason }
                if matches!(
                    fact.phase,
                    NativeAuthPhase::Starting
                        | NativeAuthPhase::AwaitingOwner
                        | NativeAuthPhase::CheckingAccount
                ) =>
            {
                (NativeAuthPhase::Failed, Some(reason.code()))
            }
            Self::HostCancelled
                if matches!(
                    fact.phase,
                    NativeAuthPhase::Starting
                        | NativeAuthPhase::AwaitingOwner
                        | NativeAuthPhase::CheckingAccount
                        | NativeAuthPhase::Cancelling
                ) =>
            {
                (NativeAuthPhase::Cancelled, None)
            }
            _ => {
                if std::env::var_os("CRAZYAGENT_DEBUG_CUSTODIAN").is_some() {
                    let event = match self {
                        Self::LoginStarted { .. } => "login_started",
                        Self::LoginCompleted { success: true, .. } => "login_completed_success",
                        Self::LoginCompleted { .. } => "login_completed_failure",
                        Self::AccountRead { .. } => "account_read",
                        Self::LoggedOut { .. } => "logged_out",
                        Self::LoginCanceled { canceled: true, .. } => "login_canceled",
                        Self::LoginCanceled { .. } => "login_cancel_not_found",
                        Self::RequestFailed { .. } => "request_failed",
                        Self::HostFailed { .. } => "host_failed",
                        Self::HostCancelled => "host_cancelled",
                    };
                    eprintln!(
                        "native_auth_observation_order phase={} event={event}",
                        fact.phase.name()
                    );
                }
                return Err(failure("native_auth_observation_order"));
            }
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
                matches!(
                    (user_code.is_some(), method),
                    (false, NativeAuthMethod::CodexBrowser)
                        | (true, NativeAuthMethod::CodexDeviceCode)
                )
            }
            (
                Self::LoginCompleted { .. } | Self::LoginCanceled { .. },
                NativeAuthAction::SignIn {
                    method: NativeAuthMethod::CodexBrowser | NativeAuthMethod::CodexDeviceCode,
                },
            ) => true,
            (Self::LoggedOut { .. }, NativeAuthAction::Logout) => true,
            (
                Self::AccountRead { .. }
                | Self::RequestFailed { .. }
                | Self::HostFailed { .. }
                | Self::HostCancelled,
                NativeAuthAction::Logout
                | NativeAuthAction::SignIn {
                    method:
                        NativeAuthMethod::CodexBrowser
                        | NativeAuthMethod::CodexDeviceCode
                        | NativeAuthMethod::CodexExisting,
                },
            ) => true,
            _ => false,
        };
        if !valid {
            return Err(failure("native_auth_method_mismatch"));
        }
        Ok(())
    }
}

/// Tells whether a ChatGPT device code shown this long has run out, with half a minute of
/// slack for Codex, which starts the code's 15 minutes a moment before the engine records it.
/// Input: how long the code has been shown. Output: true once it has run out.
pub(crate) fn code_ran_out(shown: Duration) -> bool {
    shown + Duration::from_secs(30) >= CODEX_DEVICE_CODE_LIFE
}

pub(crate) fn apply_codex_presentation(
    reply: &mut NativeAuthReply,
    challenge: Option<crate::adapters::codex::LoginChallenge>,
    shown: Option<Duration>,
    account: Option<crate::adapters::codex::AuthAccount>,
) -> Result<(), EngineError> {
    let left = shown.map(|shown| CODEX_DEVICE_CODE_LIFE.saturating_sub(shown));
    reply.challenge = challenge.map(|codex| NativeAuthChallenge::new(codex, left));
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

/// Tells whether an engine refusal of a read or cancel ends a native sign-in for good: the
/// engine no longer runs it, or its scope no longer names a managed profile of this
/// database. A settings conflict is not one: the engine answers it while it still runs the
/// sign-in. Input: the refusal code. Output: true for such a definite answer.
pub fn ends_operation(code: &str) -> bool {
    matches!(
        code,
        "native_auth_not_active"
            | "native_auth_operation_missing"
            | "native_auth_database_mismatch"
            | "native_profile_connection_missing"
            | "native_profile_not_managed"
    )
}

pub(crate) fn outcome_unknown() -> EngineError {
    failure("native_auth_outcome_unknown_read_same_operation")
}

fn public_error(code: &str) -> bool {
    matches!(
        code,
        "owner_busy"
            | "native_auth_request_bounds"
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

#[cfg(test)]
mod late_completion_tests {
    use super::*;

    fn codex_fact(phase: NativeAuthPhase) -> NativeAuthFact {
        let connection: ModelConnection = toml::from_str(
            r#"id = "codex-connection"
provider = "openai"
harness = "codex"
protocol = "native"
account_ref = "codex-account"
endpoint = { kind = "native" }
model = { id = "native-default", author = "openai" }
auth = { method = "native_managed", profile_ref = "codex-profile" }"#,
        )
        .unwrap();
        NativeAuthFact {
            operation_id: RequestId::new("auth-operation").unwrap(),
            action: NativeAuthAction::SignIn {
                method: NativeAuthMethod::CodexDeviceCode,
            },
            binding: NativeProfileBinding {
                database_instance: "a".repeat(64),
                engine_epoch: "b".repeat(64),
                config_id: ConfigId::new("c".repeat(64)).unwrap(),
                source_hash: "d".repeat(64),
                at_revision: Cursor::new(1),
                connection,
                custody_id: "e".repeat(64),
            },
            generation: 1,
            phase,
            code: None,
        }
    }

    #[test]
    fn an_existing_sign_in_check_accepts_only_an_account_read() {
        let mut fact = codex_fact(NativeAuthPhase::Starting);
        fact.action = NativeAuthAction::SignIn {
            method: NativeAuthMethod::CodexExisting,
        };
        let started = CodexAuthObservation::LoginStarted {
            id: 2,
            login_id: "login-1".into(),
            url: "https://auth.openai.com/codex/device".into(),
            user_code: Some("ABCD-12345".into()),
        };
        assert_eq!(
            started.apply(&mut fact.clone()),
            Err(failure("native_auth_method_mismatch"))
        );
        let read = CodexAuthObservation::AccountRead {
            id: 2,
            account: Some(CodexObservedAccount::Chatgpt {
                email: None,
                plan_type: "plus".into(),
            }),
            requires_openai_auth: true,
        };
        read.apply(&mut fact).unwrap();
        assert_eq!(fact.phase, NativeAuthPhase::AuthenticatedObserved);
        let mut signed_out = codex_fact(NativeAuthPhase::Starting);
        signed_out.action = NativeAuthAction::SignIn {
            method: NativeAuthMethod::CodexExisting,
        };
        CodexAuthObservation::AccountRead {
            id: 2,
            account: None,
            requires_openai_auth: true,
        }
        .apply(&mut signed_out)
        .unwrap();
        assert_eq!(signed_out.phase, NativeAuthPhase::UnauthenticatedObserved);
    }

    #[test]
    fn a_failed_login_completion_after_cancel_keeps_the_cancelled_outcome() {
        let late = CodexAuthObservation::LoginCompleted {
            login_id: Some("login-1".into()),
            success: false,
            code_ran_out: false,
        };
        for phase in [NativeAuthPhase::Cancelled, NativeAuthPhase::Failed] {
            let mut fact = codex_fact(phase);
            late.apply(&mut fact).unwrap();
            assert_eq!(fact.phase, phase);
            assert_eq!(fact.code, None);
        }
        let timed_out = CodexAuthObservation::LoginCanceled {
            id: 3,
            canceled: true,
        };
        let mut fact = codex_fact(NativeAuthPhase::AwaitingOwner);
        timed_out.apply(&mut fact).unwrap();
        assert_eq!(fact.phase, NativeAuthPhase::Failed);
        assert_eq!(fact.code.as_deref(), Some("native_auth_sign_in_timed_out"));
        late.apply(&mut fact).unwrap();
        assert_eq!(fact.phase, NativeAuthPhase::Failed);
        let succeeded = CodexAuthObservation::LoginCompleted {
            login_id: Some("login-1".into()),
            success: true,
            code_ran_out: false,
        };
        let mut fact = codex_fact(NativeAuthPhase::Cancelled);
        assert_eq!(
            succeeded.apply(&mut fact),
            Err(failure("native_auth_observation_order"))
        );
    }

    #[test]
    fn every_harness_kind_maps_each_way_to_sign_in_or_says_it_has_none() {
        let mut seen = Vec::new();
        for harness in ["claude", "codex", "rcli"] {
            let managed = harness != "rcli";
            for way in SignInWay::ALL {
                let method = NativeAuthMethod::for_way(harness, way);
                match (managed, way) {
                    (false, _) => assert_eq!(method, None, "{harness} {way:?}"),
                    (true, SignInWay::Browser | SignInWay::Check) => {
                        assert!(method.is_some(), "{harness} {way:?}")
                    }
                    (true, SignInWay::Code) => {
                        assert_eq!(method.is_some(), harness == "codex", "{harness} {way:?}")
                    }
                }
                if let Some(method) = method {
                    assert_eq!(method.way(), way);
                    assert_eq!(method.harness(), harness);
                    assert!(!seen.contains(&method), "{method:?} maps twice");
                    seen.push(method);
                }
            }
        }
        assert_eq!(seen.len(), 5);
        assert_eq!(
            NativeAuthMethod::for_way("claude", SignInWay::Check),
            Some(NativeAuthMethod::ClaudeExisting)
        );
        assert_eq!(
            NativeAuthMethod::for_way("codex", SignInWay::Check),
            Some(NativeAuthMethod::CodexExisting)
        );
        assert!(SignInWay::Code.unavailable("claude").is_some());
        assert!(SignInWay::Code.unavailable("codex").is_none());
        assert!(SignInWay::Check.unavailable("rcli").is_some());
    }

    #[test]
    fn only_a_chatgpt_device_code_sign_in_waits_past_the_code_s_fifteen_minutes() {
        assert_eq!(CODEX_DEVICE_CODE_LIFE, Duration::from_secs(15 * 60));
        assert!(NativeAuthMethod::CodexDeviceCode.sign_in_window() > CODEX_DEVICE_CODE_LIFE);
        for method in [
            NativeAuthMethod::CodexBrowser,
            NativeAuthMethod::CodexExisting,
            NativeAuthMethod::ClaudeOfficial,
            NativeAuthMethod::ClaudeExisting,
        ] {
            assert_eq!(
                method.sign_in_window(),
                Duration::from_secs(600),
                "{method:?}"
            );
        }
    }

    #[test]
    fn a_codex_log_holds_only_bounded_plain_text() {
        let log = |dropped: Vec<&str>, stderr: &str| CodexLog {
            dropped: dropped.into_iter().map(str::to_owned).collect(),
            stderr: stderr.to_owned(),
        };
        assert!(
            log(
                vec!["account/login/x", "*"],
                "ERROR codex_login: * timed out"
            )
            .validate()
            .is_ok()
        );
        assert!(
            log(vec![], &"a".repeat(CODEX_LOG_STDERR_BYTES_MAX))
                .validate()
                .is_ok()
        );
        let invalid = failure("native_auth_codex_log_invalid");
        for bad in [
            log(vec![], &"a".repeat(CODEX_LOG_STDERR_BYTES_MAX + 1)),
            log(vec![], "line\nbreak"),
            log(vec![], "escape \u{1b}[0m"),
            log(vec!["a"; CODEX_LOG_DROPPED_MAX + 1], ""),
            log(vec![""], ""),
            log(vec!["has space"], ""),
            log(vec![&"m".repeat(CODEX_LOG_NAME_BYTES_MAX + 1)], ""),
        ] {
            assert_eq!(bad.validate(), Err(invalid.clone()));
        }
    }

    #[test]
    fn the_engine_s_own_give_up_names_the_page_for_a_browser_sign_in() {
        let give_up = CodexAuthObservation::LoginCanceled {
            id: 3,
            canceled: true,
        };
        for (method, code) in [
            (
                NativeAuthMethod::CodexDeviceCode,
                "native_auth_sign_in_timed_out",
            ),
            (
                NativeAuthMethod::CodexBrowser,
                "native_auth_sign_in_page_timed_out",
            ),
        ] {
            let mut fact = codex_fact(NativeAuthPhase::AwaitingOwner);
            fact.action = NativeAuthAction::SignIn { method };
            give_up.apply(&mut fact).unwrap();
            assert_eq!(fact.phase, NativeAuthPhase::Failed);
            assert_eq!(fact.code.as_deref(), Some(code), "{method:?}");
        }
    }

    #[test]
    fn a_device_code_that_ran_out_before_approval_reads_as_timed_out() {
        assert!(!code_ran_out(Duration::from_secs(869)));
        assert!(code_ran_out(Duration::from_secs(870)));
        assert!(code_ran_out(Duration::from_secs(901)));
        let failed = |code_ran_out| CodexAuthObservation::LoginCompleted {
            login_id: Some("login-1".into()),
            success: false,
            code_ran_out,
        };
        for (method, ran_out, code) in [
            (
                NativeAuthMethod::CodexDeviceCode,
                true,
                "native_auth_sign_in_timed_out",
            ),
            (
                NativeAuthMethod::CodexDeviceCode,
                false,
                "native_auth_login_failed",
            ),
            (
                NativeAuthMethod::CodexBrowser,
                true,
                "native_auth_login_failed",
            ),
        ] {
            let mut fact = codex_fact(NativeAuthPhase::AwaitingOwner);
            fact.action = NativeAuthAction::SignIn { method };
            failed(ran_out).apply(&mut fact).unwrap();
            assert_eq!(fact.phase, NativeAuthPhase::Failed);
            assert_eq!(fact.code.as_deref(), Some(code), "{method:?} {ran_out}");
        }
    }
}
