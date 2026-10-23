pub(crate) mod account_profile;

pub(crate) mod native_auth;
pub(crate) mod transport;

pub use account_profile::{
    AccountAuthCapabilitiesReply, AccountProfileOutcome, AccountProfileReceipt, AccountProfileReply,
    AccountProfileRequest, AccountSecret, ManagedAuthCapability, ProducerCapability,
};

pub use crate::engine::native_auth_host::claude_auth::types::ClaudeAuthChallenge;
pub use transport::native_auth_channel::claude::ClaudeAuthInput;

pub use native_auth::{
    NativeAuthAccount, NativeAuthAction, NativeAuthCancel, NativeAuthChallenge, NativeAuthFact,
    NativeAuthLogout, NativeAuthMethod, NativeAuthPhase, NativeAuthQuery, NativeAuthReply,
    NativeAuthScope, NativeAuthStart,
};

pub use transport::plugins::{ServerPage, SkillPage};
pub use transport::{
    OwnerClient, OwnerRuntimeHandle, OwnerServer, RunnerClient, runner_bootstrap, runner_ready,
};

use super::EngineError;
use crate::coordination::OwnerAction;
use crate::memory::{HumanReview, KnowledgeAction};
use crate::protocol::*;
use crate::verification::{BaselineDraft, OwnerVerification, Risk};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeProfileBinding {
    pub database_instance: String,
    pub engine_epoch: String,
    pub config_id: ConfigId,
    pub source_hash: String,
    pub at_revision: Cursor,
    pub connection: crate::config::ModelConnection,
    pub custody_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedNativeProfile {
    binding: NativeProfileBinding,
}

impl PreparedNativeProfile {
    pub fn binding(&self) -> &NativeProfileBinding {
        &self.binding
    }

    pub(crate) fn from_binding(binding: NativeProfileBinding) -> Self {
        Self { binding }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OwnerClientKind {
    Tui,
    Desktop,
}

impl OwnerClientKind {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Tui => "tui",
            Self::Desktop => "desktop",
        }
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Admission {
    pub(crate) db_instance: String,
    pub(crate) engine_epoch: String,
    pub(crate) owner_id: String,
    pub(crate) owner_epoch: i64,
}

#[derive(Clone)]
pub(crate) struct HostStamp {
    pub(crate) db_instance: String,
    pub(crate) engine_epoch: String,
}

pub(crate) struct Started {
    pub(crate) host: HostStamp,
    pub(crate) owners: [Registration; 2],
}

#[derive(Clone)]
pub(crate) struct Registration {
    pub(crate) kind: OwnerClientKind,
    pub(crate) admission: Admission,
    pub(crate) active: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", deny_unknown_fields)]
pub enum OwnerMutation {
    ProjectMaster {
        action: crate::master_session::MasterOwnerAction,
    },
    PrepareWorkerDispatch {
        attempt: crate::worker_dispatch::AttemptRef,
        repository_slot: u8,
    },
    CancelWorkerDispatch {
        dispatch_id: RequestId,
    },
    ReconcileWorkerDispatch {
        dispatch_id: RequestId,
    },
    ResumeWorkerDispatch {
        dispatch_id: RequestId,
        expected_delivery_generation: u64,
    },
    CreateProject {
        project_id: ProjectId,
        spec: ProjectSpec,
    },
    SetupProject {
        project_id: ProjectId,
        spec: ProjectSpec,
        setup: crate::project_setup::ProjectSetup,
    },
    SaveResearchToMemory {
        project_id: ProjectId,
        research: crate::project_setup::ResearchKind,
        section: Option<String>,
    },
    AddProjectDocuments {
        project_id: ProjectId,
        documents: Vec<String>,
    },
    DeleteProjectContext {
        project_id: ProjectId,
        path: String,
    },
    ModelAccess {
        expected_config_id: ConfigId,
        expected_source_hash: String,
        edit: crate::config::ModelAccessEdit,
    },
    Conversation {
        mutation: crate::conversation::ConversationMutation,
    },
    SaveOwnerProfile {
        profile: crate::owner_profile::OwnerProfile,
    },
    ProposeBaseline {
        draft: BaselineDraft,
    },
    Coordination {
        action: OwnerAction,
    },
    Knowledge {
        review: HumanReview,
    },
    ApproveBaseline {
        baseline_id: BaselineId,
    },
    RevokeBaseline {
        baseline_id: BaselineId,
    },
    Escalate {
        candidate_id: RequestId,
        risk: Risk,
    },
    FullSuite {
        candidate_id: RequestId,
    },
    Verify {
        candidate_id: RequestId,
        attempt_id: RequestId,
    },
}

impl OwnerMutation {
    pub(crate) fn command(&self) -> Result<Command, EngineError> {
        Ok(match self {
            Self::ProjectMaster { .. } => {
                return Err(failure("owner_master_requires_actor"));
            }
            Self::PrepareWorkerDispatch {
                attempt,
                repository_slot,
            } => {
                crate::worker_dispatch::validate_prepare(attempt, *repository_slot)?;
                return Err(failure("owner_dispatch_requires_host"));
            }
            Self::CancelWorkerDispatch { .. }
            | Self::ReconcileWorkerDispatch { .. }
            | Self::ResumeWorkerDispatch { .. } => {
                return Err(failure("owner_dispatch_requires_host"));
            }
            Self::CreateProject { project_id, spec } => {
                spec.validate().map_err(|e| failure(&e.0))?;
                Command::CreateProject {
                    project_id: project_id.clone(),
                    spec: spec.clone(),
                }
            }
            Self::SetupProject {
                project_id,
                spec,
                setup,
            } => {
                let action = crate::project_setup::SetupAction::Create {
                    project_id: project_id.clone(),
                    spec: spec.clone(),
                    setup: setup.clone(),
                };
                action.validate()?;
                Command::ProjectSetup { action }
            }
            Self::SaveResearchToMemory {
                project_id,
                research,
                section,
            } => {
                let action = crate::project_setup::SetupAction::SaveToMemory {
                    project_id: project_id.clone(),
                    research: *research,
                    section: section.clone(),
                };
                action.validate()?;
                Command::ProjectSetup { action }
            }
            Self::AddProjectDocuments {
                project_id,
                documents,
            } => {
                let action = crate::project_setup::SetupAction::AddDocuments {
                    project_id: project_id.clone(),
                    documents: documents.clone(),
                };
                action.validate()?;
                Command::ProjectSetup { action }
            }
            Self::DeleteProjectContext { project_id, path } => {
                let action = crate::project_setup::SetupAction::DeleteContext {
                    project_id: project_id.clone(),
                    path: path.clone(),
                };
                action.validate()?;
                Command::ProjectSetup { action }
            }
            Self::ModelAccess {
                expected_config_id,
                expected_source_hash,
                edit,
            } => {
                if !is_hex_id(expected_source_hash) {
                    return Err(failure("settings_source_hash"));
                }
                edit.validate().map_err(|e| failure(&e.0))?;
                Command::OwnerModelAccess {
                    expected_config_id: expected_config_id.clone(),
                    expected_source_hash: expected_source_hash.clone(),
                    edit: edit.clone(),
                }
            }
            Self::Conversation { mutation } => {
                mutation.validate()?;
                Command::OwnerConversation {
                    mutation: mutation.clone(),
                }
            }
            Self::SaveOwnerProfile { profile } => {
                profile.validate()?;
                Command::OwnerProfile {
                    profile: profile.clone(),
                }
            }
            Self::ProposeBaseline { draft } => Command::ProposeBaseline {
                draft: draft.clone(),
            },
            Self::Coordination { action } => Command::OwnerCoordination {
                action: action.clone(),
            },
            Self::Knowledge { review } => {
                if review_is_read(review) {
                    return Err(failure("owner_mutation_required"));
                }
                Command::Knowledge {
                    action: KnowledgeAction::Owner {
                        action: review.clone(),
                    },
                }
            }
            Self::ApproveBaseline { baseline_id } => Command::OwnerVerification {
                action: OwnerVerification::Approve {
                    baseline_id: baseline_id.clone(),
                },
            },
            Self::RevokeBaseline { baseline_id } => Command::OwnerVerification {
                action: OwnerVerification::Revoke {
                    baseline_id: baseline_id.clone(),
                },
            },
            Self::Escalate { candidate_id, risk } => Command::OwnerVerification {
                action: OwnerVerification::Escalate {
                    candidate_id: candidate_id.clone(),
                    risk: *risk,
                },
            },
            Self::FullSuite { candidate_id } => Command::OwnerVerification {
                action: OwnerVerification::FullSuite {
                    candidate_id: candidate_id.clone(),
                },
            },
            Self::Verify {
                candidate_id,
                attempt_id,
            } => Command::OwnerVerification {
                action: OwnerVerification::Verify {
                    candidate_id: candidate_id.clone(),
                    attempt_id: attempt_id.clone(),
                },
            },
        })
    }
}

fn review_is_read(review: &HumanReview) -> bool {
    matches!(
        review,
        HumanReview::Inbox { .. } | HumanReview::GlobalInbox { .. } | HumanReview::History { .. }
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OwnerView {
    Projects,
    Tasks,
    Baselines,
    Plans,
    Candidates,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OwnerPageCursor {
    pub db_instance: String,
    pub owner_id: String,
    pub engine_epoch: String,
    pub owner_epoch: i64,
    pub config_id: ConfigId,
    pub at_revision: Cursor,
    pub view: OwnerView,
    pub after_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", deny_unknown_fields)]
pub enum OwnerRead {
    ProjectMaster {
        project_id: ProjectId,
    },
    ProjectMasterTurns {
        project_id: ProjectId,
        session_id: SessionId,
        cursor: Option<crate::master_session::MasterCursor>,
        limit: u8,
    },
    WorkerDispatch {
        dispatch_id: RequestId,
    },
    ModelAccessPage {
        collection: ModelAccessCollection,
        cursor: Option<ModelAccessCursor>,
        limit: u8,
    },
    ModelAccessRecord {
        collection: ModelAccessCollection,
        id: CredentialRef,
    },
    Settings,
    Setting {
        name: String,
    },
    ConversationHistory {
        project_id: ProjectId,
        cursor: Option<crate::conversation::ConversationCursor>,
        limit: u8,
    },
    CurrentBrief {
        project_id: ProjectId,
    },
    ProjectCatalog {
        pool: Option<CredentialRef>,
        research: Vec<crate::project_setup::ResearchKind>,
    },
    ProjectSetup {
        project_id: ProjectId,
    },
    ResearchJobs {
        project_id: ProjectId,
    },
    ProjectContext {
        project_id: ProjectId,
    },
    ProjectContextItem {
        project_id: ProjectId,
        path: String,
        offset: u64,
    },
    OwnerProfile,
    PoolSummaries {
        offset: u8,
    },
    Page {
        view: OwnerView,
        cursor: Option<OwnerPageCursor>,
        limit: u8,
    },
    Baseline {
        baseline_id: BaselineId,
    },
    CoordinationState {
        plan_id: PlanId,
    },
    TaskPlan {
        task_id: TaskId,
    },
    TaskContext {
        task_id: TaskId,
    },
    TaskConfig {
        task_id: TaskId,
    },
    Knowledge {
        review: HumanReview,
    },
    Inspect {
        candidate_id: RequestId,
    },
    History {
        candidate_id: RequestId,
        after: RequestId,
    },
    Accept {
        candidate_id: RequestId,
        attempt_id: RequestId,
    },
    Events {
        after: Cursor,
    },
    Capabilities,
}

impl OwnerRead {
    pub(crate) fn command(&self) -> Result<Option<Command>, EngineError> {
        Ok(Some(match self {
            Self::WorkerDispatch { .. }
            | Self::ProjectMaster { .. }
            | Self::ProjectMasterTurns { .. }
            | Self::Settings
            | Self::ModelAccessPage { .. }
            | Self::ModelAccessRecord { .. }
            | Self::Page { .. }
            | Self::Baseline { .. }
            | Self::Capabilities
            | Self::ConversationHistory { .. }
            | Self::CurrentBrief { .. }
            | Self::ProjectSetup { .. }
            | Self::ResearchJobs { .. }
            | Self::ProjectContext { .. }
            | Self::OwnerProfile
            | Self::PoolSummaries { .. } => return Ok(None),
            Self::ProjectCatalog { research, .. } => {
                if research.len() > crate::project_setup::ResearchKind::ALL.len() {
                    return Err(failure("research_duplicate"));
                }
                return Ok(None);
            }
            Self::ProjectContextItem { path, .. } => {
                if path.is_empty() || path.len() > 512 || path.chars().any(char::is_control) {
                    return Err(failure("context_path_invalid"));
                }
                return Ok(None);
            }
            Self::Setting { name } => {
                if name.is_empty() || name.len() > 96 {
                    return Err(failure("owner_setting_name_bounds"));
                }
                return Ok(None);
            }
            Self::CoordinationState { plan_id } => Command::CoordinationState {
                plan_id: plan_id.clone(),
            },
            Self::TaskPlan { task_id } => Command::TaskPlan {
                task_id: task_id.clone(),
            },
            Self::TaskContext { task_id } => Command::TaskContext {
                task_id: task_id.clone(),
            },
            Self::TaskConfig { task_id } => Command::TaskConfig {
                task_id: task_id.clone(),
            },
            Self::Knowledge { review } => {
                if !review_is_read(review) {
                    return Err(failure("owner_read_required"));
                }
                Command::Knowledge {
                    action: KnowledgeAction::Owner {
                        action: review.clone(),
                    },
                }
            }
            Self::Inspect { candidate_id } => Command::OwnerVerification {
                action: OwnerVerification::Inspect {
                    candidate_id: candidate_id.clone(),
                },
            },
            Self::History {
                candidate_id,
                after,
            } => Command::OwnerVerification {
                action: OwnerVerification::History {
                    candidate_id: candidate_id.clone(),
                    after: after.clone(),
                },
            },
            Self::Accept {
                candidate_id,
                attempt_id,
            } => Command::OwnerVerification {
                action: OwnerVerification::Accept {
                    candidate_id: candidate_id.clone(),
                    attempt_id: attempt_id.clone(),
                },
            },
            Self::Events { after } => Command::Events {
                after: after.clone(),
            },
        }))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", deny_unknown_fields)]
pub enum OwnerOperation {
    Apply {
        mutation: OwnerMutation,
    },
    Read {
        query: OwnerRead,
    },
    Reconcile {
        operation_id: RequestId,
        expected_hash: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OwnerRequest {
    pub protocol_version: u16,
    pub request_id: RequestId,
    pub expected_revision: Cursor,
    pub operation: OwnerOperation,
}

impl OwnerRequest {
    pub fn validate(&self) -> Result<(), EngineError> {
        if self.protocol_version != PROTOCOL_VERSION {
            return Err(failure("owner_protocol_version"));
        }
        encode_frame(self).map_err(|_| failure("owner_request_bounds"))?;
        match &self.operation {
            OwnerOperation::Apply {
                mutation: OwnerMutation::ProjectMaster { action },
            } => crate::master_session::validate_action(action)?,
            OwnerOperation::Apply {
                mutation:
                    OwnerMutation::PrepareWorkerDispatch {
                        attempt,
                        repository_slot,
                    },
            } => {
                crate::worker_dispatch::validate_prepare(attempt, *repository_slot)?;
            }
            OwnerOperation::Apply {
                mutation:
                    OwnerMutation::CancelWorkerDispatch { .. }
                    | OwnerMutation::ReconcileWorkerDispatch { .. },
            } => {}
            OwnerOperation::Apply {
                mutation:
                    OwnerMutation::ResumeWorkerDispatch {
                        expected_delivery_generation,
                        ..
                    },
            } => {
                if !(1..crate::worker_dispatch::DISPATCH_GENERATIONS_MAX)
                    .contains(expected_delivery_generation)
                {
                    return Err(failure("dispatch_delivery_generation"));
                }
            }
            OwnerOperation::Apply { mutation } => {
                mutation.command()?;
            }
            OwnerOperation::Read { query } => {
                query.command()?;
                if let OwnerRead::ProjectMasterTurns { cursor, limit, .. } = query {
                    crate::master_session::validate_page(cursor.as_ref(), *limit)?;
                }
                if let OwnerRead::ModelAccessPage { limit, cursor, .. } = query {
                    if !(1..=16).contains(limit) {
                        return Err(failure("model_access_page_limit"));
                    }
                    if let Some(cursor) = cursor
                        && (!is_hex_id(&cursor.source_hash)
                            || !is_hex_id(&cursor.owner_id)
                            || !is_hex_id(&cursor.db_instance)
                            || !is_hex_id(&cursor.engine_epoch))
                    {
                        return Err(failure("model_access_cursor"));
                    }
                }
                if let OwnerRead::ConversationHistory {
                    project_id,
                    cursor,
                    limit,
                } = query
                {
                    crate::conversation::validate_history(project_id, cursor.as_ref(), *limit)?;
                }
                if let OwnerRead::Page { limit, cursor, .. } = query {
                    if !(1..=32).contains(limit) {
                        return Err(failure("owner_page_limit"));
                    }
                    if let Some(cursor) = cursor {
                        RequestId::new(cursor.after_id.clone())
                            .map_err(|_| failure("owner_page_cursor"))?;
                    }
                }
            }
            OwnerOperation::Reconcile {
                expected_hash: Some(hash),
                ..
            } if !is_hex_id(hash) => return Err(failure("owner_hash")),
            _ => {}
        }
        Ok(())
    }

    pub fn mutation_hash(&self) -> Result<String, EngineError> {
        self.validate()?;
        if !matches!(self.operation, OwnerOperation::Apply { .. }) {
            return Err(failure("owner_mutation_required"));
        }
        Ok(crate::memory::hash(
            &serde_json::to_vec(self).map_err(|_| failure("owner_request_encoding"))?,
        ))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", deny_unknown_fields)]
pub enum OwnerResult {
    Applied,
    DispatchPrepared { dispatch_id: RequestId },
    VerificationUnavailable { attempt_id: RequestId },
    RcliProfileRegistered {
        registration: Box<crate::master_session::RcliProfileRegistrationMetadata>,
    },
    Rejected { code: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OwnerProvenance {
    pub origin: String,
    pub owner_id: String,
    pub operation_id: RequestId,
    pub request_hash: String,
    pub engine_epoch: String,
    pub admitted_epoch: i64,
    pub resulting_epoch: i64,
    pub revision_before: Cursor,
    pub revision_after: Cursor,
    pub event_cursor: Option<Cursor>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OwnerApplied {
    pub result: OwnerResult,
    pub provenance: OwnerProvenance,
    pub historical_replay: bool,
    pub current_revision: Cursor,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", deny_unknown_fields)]
pub enum OwnerSummary {
    Project {
        project_id: ProjectId,
        name: String,
        repository_count: usize,
    },
    Task {
        task_id: TaskId,
        project_id: ProjectId,
        config_id: ConfigId,
        baseline_id: BaselineId,
        snapshot_id: SnapshotId,
    },
    Baseline {
        baseline_id: BaselineId,
        project_id: ProjectId,
        approved: bool,
        revoked: bool,
        current: bool,
    },
    Plan {
        plan_id: PlanId,
        task_id: TaskId,
    },
    Candidate {
        id: RequestId,
        task_id: TaskId,
        snapshot_id: SnapshotId,
        environment_id: EnvironmentId,
        pinned_baseline_id: BaselineId,
        config_id: ConfigId,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", deny_unknown_fields)]
pub enum OwnerData {
    ProjectMaster {
        session: Option<crate::master_session::MasterSessionView>,
    },
    ProjectMasterTurns {
        page: crate::master_session::MasterTurnPage,
    },
    WorkerDispatch {
        dispatch: Option<Box<crate::worker_dispatch::DispatchView>>,
    },
    ModelAccessPage {
        collection: ModelAccessCollection,
        source_hash: String,
        writable: bool,
        items: Vec<ModelAccessRecord>,
        next: Option<ModelAccessCursor>,
    },
    ModelAccessRecord {
        source_hash: String,
        writable: bool,
        record: ModelAccessRecord,
    },
    Settings {
        names: Vec<String>,
    },
    Setting {
        descriptor: Box<crate::config::SettingDescriptor>,
    },
    ConversationHistory {
        page: crate::conversation::ConversationPage,
    },
    CurrentBrief {
        project_id: ProjectId,
        brief: Option<crate::conversation::ConversationEntry>,
    },
    ProjectCatalog {
        catalog: Box<crate::project_setup::ProjectCatalog>,
    },
    ProjectSetup {
        project_id: ProjectId,
        setup: Option<Box<crate::project_setup::StoredSetup>>,
    },
    ResearchJobs {
        project_id: ProjectId,
        jobs: Vec<crate::project_setup::ResearchJob>,
    },
    ProjectContext {
        context: crate::project_setup::ProjectContext,
    },
    ProjectContextItem {
        chunk: crate::project_setup::ContextChunk,
    },
    OwnerProfile {
        profile: Option<crate::owner_profile::OwnerProfile>,
    },
    PoolSummaries {
        pools: Vec<crate::pools::PoolSummary>,
        next_offset: Option<u8>,
    },
    Page {
        items: Vec<OwnerSummary>,
        next: Option<OwnerPageCursor>,
    },
    Baseline {
        draft: crate::verification::BaselineDraft,
        approved: bool,
        revoked: bool,
        current: bool,
    },
    Domain {
        outcome: Box<Outcome>,
    },
    ResyncRequired,
    Capabilities {
        authenticated_owner_transport: bool,
        native_worker_admission: bool,
        human_consent_proven: bool,
        native_input: bool,
        merge_execution: bool,
        settings_persistence: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelAccessCollection {
    Accounts,
    Connections,
    Pools,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ModelAccessRecord {
    Account {
        account_ref: CredentialRef,
    },
    Connection {
        connection: crate::config::ModelConnection,
    },
    Pool {
        pool: crate::config::AccountPool,
    },
}

impl ModelAccessRecord {
    pub fn id(&self) -> &CredentialRef {
        match self {
            Self::Account { account_ref } => account_ref,
            Self::Connection { connection } => &connection.id,
            Self::Pool { pool } => &pool.id,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelAccessCursor {
    pub db_instance: String,
    pub owner_id: String,
    pub engine_epoch: String,
    pub owner_epoch: i64,
    pub config_id: ConfigId,
    pub at_revision: Cursor,
    pub source_hash: String,
    pub collection: ModelAccessCollection,
    pub after_id: CredentialRef,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OwnerReadReply {
    pub revision: Cursor,
    pub config_id: ConfigId,
    pub data: OwnerData,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", deny_unknown_fields)]
pub enum OwnerResponse {
    OutcomeUnknown { operation_id: RequestId },
    Applied { operation: OwnerApplied },
    Read { state: OwnerReadReply },
    Committed { operation: OwnerApplied },
    NotRecordedAtBarrier { revision: Cursor },
    Rejected { code: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OwnerReply {
    pub request_id: RequestId,
    pub response: OwnerResponse,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OwnerDecision {
    Revoke,
    Reactivate,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OwnerControl {
    pub operation_id: RequestId,
    pub target: OwnerClientKind,
    pub expected_epoch: i64,
    pub decision: OwnerDecision,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OwnerControlReply {
    pub owner_id: String,
    pub owner_epoch: i64,
    pub active: bool,
    pub epoch_exhausted_revoked: bool,
}

pub(crate) fn failure(code: &str) -> EngineError {
    EngineError(code.into())
}

pub(crate) fn is_hex_id(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

pub(crate) fn random_id() -> Result<String, EngineError> {
    let mut bytes = [0; 32];
    getrandom::fill(&mut bytes).map_err(|_| failure("owner_entropy_unavailable"))?;
    use std::fmt::Write;
    let mut id = String::with_capacity(64);
    for byte in bytes {
        write!(&mut id, "{byte:02x}").map_err(|_| failure("owner_identity_encoding"))?;
    }
    assert!(is_hex_id(&id));
    Ok(id)
}
