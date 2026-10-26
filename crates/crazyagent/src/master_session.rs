use crate::config::{EffectiveConfig, ModelConnection, ModelEndpoint};
use crate::coordination::Role;
use crate::engine::EngineError;
use crate::engine::owner::{NativeAuthFact, NativeProfileBinding, OwnerProvenance, is_hex_id};
use crate::model_gateway::Attempt;
use crate::protocol::{ConfigId, CredentialRef, Cursor, ProjectId, RequestId, SessionId, TaskId};
use crate::routing::{RouteRequest, validate_pool_scope};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub(crate) const SESSIONS_MAX: usize = 64;
pub(crate) const TURNS_MAX: usize = 1024;
pub(crate) const METADATA_BYTES_MAX: usize = 16384;
pub(crate) const PAGE_BYTES_MAX: usize = 32768;
pub(crate) const PAGE_ITEMS_MAX: u8 = 32;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RcliConfiguredProfileProof {
    pub(crate) registration_id: RequestId,
    pub(crate) generation: u64,
    pub(crate) database_instance: String,
    pub(crate) engine_epoch: String,
    pub(crate) config_id: ConfigId,
    pub(crate) source_hash: String,
    pub(crate) at_revision: Cursor,
    pub(crate) connection: ModelConnection,
    pub(crate) profile_ref: CredentialRef,
    pub(crate) custody_id: String,
    pub(crate) profile_config_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) enum MasterBootstrapAuth {
    NativeSignIn(NativeAuthFact),
    RcliConfigured(RcliConfiguredProfileProof),
}

impl MasterBootstrapAuth {
    pub(crate) fn binding(&self) -> NativeProfileBinding {
        match self {
            Self::NativeSignIn(fact) => fact.binding.clone(),
            Self::RcliConfigured(proof) => NativeProfileBinding {
                database_instance: proof.database_instance.clone(),
                engine_epoch: proof.engine_epoch.clone(),
                config_id: proof.config_id.clone(),
                source_hash: proof.source_hash.clone(),
                at_revision: proof.at_revision.clone(),
                connection: proof.connection.clone(),
                custody_id: proof.custody_id.clone(),
            },
        }
    }
}

impl From<&NativeAuthFact> for MasterBootstrapAuth {
    fn from(fact: &NativeAuthFact) -> Self {
        Self::NativeSignIn(fact.clone())
    }
}

impl From<&MasterBootstrapAuth> for MasterBootstrapAuth {
    fn from(proof: &MasterBootstrapAuth) -> Self {
        proof.clone()
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MasterBackend {
    #[default]
    Rcli,
    Codex,
    Claude,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MasterSessionPhase {
    Configured,
    Closing,
    Closed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MasterTurnPhase {
    Queued,
    Blocked,
    Prepared,
    Delivering,
    Running,
    WaitingApproval,
    Succeeded,
    Failed,
    Cancelled,
    Uncertain,
}

impl MasterTurnPhase {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Blocked => "blocked",
            Self::Prepared => "prepared",
            Self::Delivering => "delivering",
            Self::Running => "running",
            Self::WaitingApproval => "waiting_approval",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Uncertain => "uncertain",
        }
    }

    pub(crate) fn outstanding(self) -> bool {
        matches!(
            self,
            Self::Queued
                | Self::Prepared
                | Self::Delivering
                | Self::Running
                | Self::WaitingApproval
                | Self::Uncertain
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MasterReason {
    ReadinessUnknown,
    ReadinessStale,
    InvalidAuthentication,
    QuotaUnknown,
    QuotaExhausted,
    RateLimited,
    CooldownActive,
    ConnectionChanged,
    ScopeRevoked,
    CapacityUnavailable,
    BudgetExhausted,
    ProfileBusy,
    ApprovalRequired,
    RuntimeUnavailable,
    CaptureIncomplete,
    OutputLimit,
    UnsupportedCapability,
    RepositoryMissing,
    RepositoryConfigRefused,
    Cancelled,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MasterSelection {
    #[serde(default)]
    pub backend: MasterBackend,
    pub pool_ref: CredentialRef,
    pub target_profile_ref: CredentialRef,
    pub route: RouteRequest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MasterSessionView {
    pub project_id: ProjectId,
    pub session_id: SessionId,
    pub bootstrap_task_id: TaskId,
    pub selection: MasterSelection,
    pub config_id: ConfigId,
    pub source_hash: String,
    pub phase: MasterSessionPhase,
    pub version: Cursor,
    pub current_turn: Option<RequestId>,
    pub owner: OwnerProvenance,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RcliProfileRegistration {
    pub expected_session: MasterSessionView,
    pub expected_connection: ModelConnection,
    pub configured_home: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RcliProfileRegistrationMetadata {
    pub project_id: ProjectId,
    pub session_id: SessionId,
    pub profile_ref: CredentialRef,
    pub registration_id: RequestId,
    pub generation: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MasterRuntimeView {
    pub runtime_ref: RequestId,
    pub artifact_sha256: String,
    pub native_session_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MasterReplyProvenance {
    pub attempt_id: RequestId,
    pub runtime_ref: RequestId,
    pub capture_ref: RequestId,
    pub capture_sequence: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MasterReply {
    pub text: String,
    pub provenance: MasterReplyProvenance,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MasterTurnView {
    pub id: RequestId,
    pub session_id: SessionId,
    pub ordinal: u64,
    pub input_cursor: Cursor,
    pub phase: MasterTurnPhase,
    pub reason: Option<MasterReason>,
    pub attempt: Option<Attempt>,
    pub runtime: Option<MasterRuntimeView>,
    pub reply: Option<MasterReply>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub accounts: Vec<MasterAccountState>,
}

/// One pool account named on a master turn that no account of its harness could run.
/// Fields: the account, its connection, the connection's harness and why it cannot run
/// the turn; a reason of None marks a ready account of another harness in the pool.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MasterAccountState {
    pub account_ref: CredentialRef,
    pub connection: CredentialRef,
    pub harness: CredentialRef,
    pub reason: Option<MasterReason>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MasterTurnPage {
    pub project_id: ProjectId,
    pub session_id: SessionId,
    pub turns: Vec<MasterTurnView>,
    pub next: Option<MasterCursor>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MasterCursor {
    pub db_instance: String,
    pub owner_id: String,
    pub owner_epoch: i64,
    pub engine_epoch: String,
    pub project_id: ProjectId,
    pub session_id: SessionId,
    pub config_id: ConfigId,
    pub source_hash: String,
    pub version: Cursor,
    pub after_ordinal: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum MasterOwnerAction {
    Open {
        project_id: ProjectId,
        expected_config_id: ConfigId,
        expected_source_hash: String,
        selection: Box<MasterSelection>,
    },
    Submit {
        project_id: ProjectId,
        session_id: SessionId,
        text: String,
    },
    Cancel {
        project_id: ProjectId,
        session_id: SessionId,
        turn_id: RequestId,
    },
    Close {
        project_id: ProjectId,
        session_id: SessionId,
    },
    RegisterRcliProfile {
        registration: Box<RcliProfileRegistration>,
    },
}

pub(crate) fn error(value: impl std::fmt::Display) -> EngineError {
    EngineError(value.to_string())
}

pub(crate) fn validate_action(action: &MasterOwnerAction) -> Result<(), EngineError> {
    match action {
        MasterOwnerAction::Open {
            expected_config_id,
            expected_source_hash,
            selection,
            ..
        } => {
            if !is_hex_id(expected_config_id.as_str()) || !is_hex_id(expected_source_hash) {
                return Err(error("master_source_basis"));
            }
            selection.route.validate().map_err(error)?;
        }
        MasterOwnerAction::Submit { text, .. } => {
            if text.trim().is_empty() || text.len() > 8192 || text.contains('\0') {
                return Err(error("master_text_bounds"));
            }
        }
        MasterOwnerAction::Cancel { .. }
        | MasterOwnerAction::Close { .. }
        | MasterOwnerAction::RegisterRcliProfile { .. } => {}
    }
    Ok(())
}

pub(crate) fn validate_page(cursor: Option<&MasterCursor>, limit: u8) -> Result<(), EngineError> {
    if !(1..=PAGE_ITEMS_MAX).contains(&limit) {
        return Err(error("master_page_limit"));
    }
    if let Some(cursor) = cursor
        && (!is_hex_id(&cursor.db_instance)
            || !is_hex_id(&cursor.owner_id)
            || !is_hex_id(&cursor.engine_epoch)
            || !is_hex_id(cursor.config_id.as_str())
            || !is_hex_id(&cursor.source_hash)
            || cursor.owner_epoch < 1
            || cursor.version.value() > i64::MAX.unsigned_abs()
            || cursor.after_ordinal == 0
            || cursor.after_ordinal > u64::try_from(TURNS_MAX).map_err(error)?)
    {
        return Err(error("master_cursor_bounds"));
    }
    Ok(())
}

pub(crate) fn validate_selection(
    config: &EffectiveConfig,
    project: &ProjectId,
    selection: &MasterSelection,
) -> Result<(), EngineError> {
    selection.route.validate().map_err(error)?;
    validate_pool_scope(
        config,
        &selection.pool_ref,
        project,
        &Role::ProjectLeader,
        &selection.route.account_ref,
    )
    .map_err(error)?;
    let authorized = |profile: &CredentialRef| {
        config.settings.allowed_destinations.contains(profile)
            && !config.settings.revoked_destinations.contains(profile)
    };
    if !authorized(&selection.target_profile_ref)
        || [
            &selection.route.preferred_target,
            &selection.route.required_target,
        ]
        .into_iter()
        .flatten()
        .any(|profile| !authorized(profile))
        || selection
            .route
            .required_target
            .as_ref()
            .is_some_and(|profile| profile != &selection.target_profile_ref)
        || !selection
            .route
            .resources
            .fits(&config.settings.routing.task_capacity)
    {
        return Err(error("master_scope_or_budget_denied"));
    }
    let catalog = config
        .settings
        .model_access
        .as_ref()
        .ok_or_else(|| error("master_pool_missing"))?;
    let pool = catalog
        .pools
        .iter()
        .find(|pool| pool.id == selection.pool_ref)
        .ok_or_else(|| error("master_pool_missing"))?;
    if !catalog.connections.iter().any(|connection| {
        connection.account_ref == selection.route.account_ref
            && pool.members.contains(&connection.id)
            && backend_member(selection.backend, connection)
    }) {
        return Err(error("master_backend_account_denied"));
    }
    Ok(())
}

/// Tells whether a connection can run a master of this backend.
/// Inputs: the backend and the connection. Output: true for an RCLI HTTP connection, or
/// for a native connection of the Codex or Claude harness the backend names.
pub(crate) fn backend_member(backend: MasterBackend, connection: &ModelConnection) -> bool {
    match backend {
        MasterBackend::Rcli => {
            connection.harness.as_str() == "rcli"
                && matches!(connection.endpoint, ModelEndpoint::Http { .. })
        }
        MasterBackend::Codex => {
            connection.harness.as_str() == "codex"
                && connection.protocol.as_str() == "native"
                && matches!(connection.endpoint, ModelEndpoint::Native {})
        }
        MasterBackend::Claude => {
            matches!(connection.harness.as_str(), "claude" | "claude-code")
                && connection.protocol.as_str() == "native"
                && matches!(connection.endpoint, ModelEndpoint::Native {})
        }
    }
}
