use crate::protocol::{Cursor, PlanId, RequestId, TaskId};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttemptRef {
    pub plan_id: PlanId,
    pub item_id: TaskId,
    pub reservation_cursor: Cursor,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedRunner {
    pub(crate) root: PathBuf,
    pub(crate) registration_id: RequestId,
    pub(crate) attempt: AttemptRef,
    pub(crate) slot: u8,
}

impl PreparedRunner {
    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn registration_id(&self) -> &RequestId {
        &self.registration_id
    }

    pub fn attempt(&self) -> &AttemptRef {
        &self.attempt
    }

    pub fn slot(&self) -> u8 {
        self.slot
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunnerBootIdentity {
    pub runner_id: RequestId,
    pub pid: u32,
    pub started_at_unix_ns: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunnerReady {
    pub registration_id: RequestId,
    pub attempt: AttemptRef,
    pub boot: RunnerBootIdentity,
    pub database_instance: String,
    pub engine_epoch: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RunnerRequest {
    Ready {
        protocol_version: u16,
        registration_id: RequestId,
        boot: RunnerBootIdentity,
    },
    Offer {
        protocol_version: u16,
        dispatch_id: RequestId,
        delivery_generation: u64,
    },
    ReportDenied {
        protocol_version: u16,
        receipt: DeniedReceipt,
    },
    Reconcile {
        protocol_version: u16,
        dispatch_id: RequestId,
    },
    ReportFence {
        protocol_version: u16,
        receipt: FenceReceipt,
    },
}

pub(crate) const RUNNERS_MAX: usize = 128;
pub(crate) const RUNNER_BOOTSTRAP_MAX: usize = 2048;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RunnerBootstrap {
    pub version: u8,
    pub registration_id: RequestId,
    pub attempt: AttemptRef,
    pub slot: u8,
}

impl RunnerBootstrap {
    pub(crate) fn validate(&self) -> Result<(), crate::engine::EngineError> {
        if self.version != 1
            || usize::from(self.slot) >= RUNNERS_MAX
            || self.attempt.reservation_cursor.value() == 0
        {
            return Err(crate::engine::EngineError(
                "runner_bootstrap_invalid".into(),
            ));
        }
        Ok(())
    }
}

impl RunnerBootIdentity {
    pub fn validate(&self) -> Result<(), crate::engine::EngineError> {
        if self.pid == 0
            || self.started_at_unix_ns.len() > 39
            || !matches!(self.started_at_unix_ns.parse::<u128>(), Ok(value) if value > 0)
        {
            return Err(crate::engine::EngineError("runner_boot_invalid".into()));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RunnerReply {
    Ready { identity: RunnerReady },
    Offer { offer: Box<AdmissionCheckOffer> },
    DeniedRecorded { dispatch_id: RequestId },
    Fence { order: FenceOrder },
    FenceRecorded { dispatch_id: RequestId },
    Rejected { code: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DispatchState {
    Queued,
    RunnerUnavailable,
    Offered,
    OutcomeUnknown,
    ReconciliationRequired,
    NotConsumed,
    BlockedRecovery,
    CancelledBeforeOffer,
    DeliveredDenied,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DispatchCancellation {
    None,
    Requested,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeniedReason {
    NativeRuntimeAdmissionUnavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeniedReceipt {
    pub dispatch_id: RequestId,
    pub attempt: AttemptRef,
    pub delivery_generation: u64,
    pub database_instance: String,
    pub offer_engine_epoch: String,
    pub registration_id: RequestId,
    pub runner_boot_id: RequestId,
    pub controller_generation: u64,
    pub offer_digest: String,
    pub journal_sequence: u64,
    pub reason: DeniedReason,
    pub native_identity: Option<RequestId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DispatchView {
    pub dispatch_id: RequestId,
    pub attempt: AttemptRef,
    pub state: DispatchState,
    pub delivery_generation: u64,
    pub binding_digest: String,
    pub lease_digest: String,
    pub offer_digest: Option<String>,
    pub cancellation: DispatchCancellation,
    pub receipt: Option<DeniedReceipt>,
}

pub(crate) fn validate_prepare(
    attempt: &AttemptRef,
    repository_slot: u8,
) -> Result<(), crate::engine::EngineError> {
    if attempt.reservation_cursor.value() == 0 || repository_slot >= 64 {
        return Err(crate::engine::EngineError("worker_dispatch_input".into()));
    }
    Ok(())
}

pub fn dispatch_id(
    database_instance: &str,
    attempt: &AttemptRef,
) -> Result<RequestId, crate::engine::EngineError> {
    validate_prepare(attempt, 0)?;
    if !crate::engine::owner::is_hex_id(database_instance) {
        return Err(crate::engine::EngineError(
            "worker_dispatch_database".into(),
        ));
    }
    let mut bytes = b"crazyagent-worker-dispatch-v1".to_vec();
    bytes.extend(
        serde_json::to_vec(&(
            database_instance,
            &attempt.plan_id,
            &attempt.item_id,
            &attempt.reservation_cursor,
        ))
        .map_err(|error| crate::engine::EngineError(error.to_string()))?,
    );
    RequestId::new(crate::memory::hash(&bytes))
        .map_err(|error| crate::engine::EngineError(error.to_string()))
}

pub const DISPATCH_OPERATIONS_MAX: usize = 128;
pub const DISPATCH_GENERATIONS_MAX: u64 = 128;
pub const DISPATCH_OFFER_BYTES_MAX: usize = 8192;
pub const DISPATCH_RECEIPT_BYTES_MAX: usize = 2048;
pub const DISPATCH_VIEW_BYTES_MAX: usize = 4096;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeaseIdentity {
    pub device: u64,
    pub inode: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DispatchLeaseSeal {
    pub project_id: crate::protocol::ProjectId,
    pub task_id: TaskId,
    pub repository: String,
    pub path: String,
    pub branch: String,
    pub owner_session: crate::protocol::SessionId,
    pub native_descendant: Option<String>,
    pub admission_root: LeaseIdentity,
    pub admission_metadata: LeaseIdentity,
    pub directory: LeaseIdentity,
    pub metadata: LeaseIdentity,
    pub head: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdmissionCheckOffer {
    pub dispatch_id: RequestId,
    pub attempt: AttemptRef,
    pub delivery_generation: u64,
    pub database_instance: String,
    pub engine_epoch: String,
    pub registration_id: RequestId,
    pub runner_boot_id: RequestId,
    pub binding: crate::coordination::WorkerBinding,
    pub lease_seal: DispatchLeaseSeal,
    pub offer_digest: String,
}

impl AdmissionCheckOffer {
    pub fn digest(&self) -> Result<String, crate::engine::EngineError> {
        let mut bytes = b"crazyagent-admission-check-offer-v1".to_vec();
        bytes.extend(
            serde_json::to_vec(&(
                &self.dispatch_id,
                &self.attempt,
                self.delivery_generation,
                &self.database_instance,
                &self.engine_epoch,
                &self.registration_id,
                &self.runner_boot_id,
                &self.binding,
                &self.lease_seal,
            ))
            .map_err(|error| crate::engine::EngineError(error.to_string()))?,
        );
        Ok(crate::memory::hash(&bytes))
    }

    pub fn validate(&self) -> Result<(), crate::engine::EngineError> {
        if !(1..=DISPATCH_GENERATIONS_MAX).contains(&self.delivery_generation)
            || !crate::engine::owner::is_hex_id(&self.engine_epoch)
            || self.dispatch_id != dispatch_id(&self.database_instance, &self.attempt)?
            || self.binding.plan_id != self.attempt.plan_id
            || self.binding.task_id != self.attempt.item_id
            || self.binding.attempt_cursor != self.attempt.reservation_cursor
            || self.binding.session_id != self.lease_seal.owner_session
            || self.lease_seal.native_descendant.is_some()
            || self.offer_digest != self.digest()?
            || serde_json::to_vec(self)
                .map_err(|e| crate::engine::EngineError(e.to_string()))?
                .len()
                > DISPATCH_OFFER_BYTES_MAX
        {
            return Err(crate::engine::EngineError("dispatch_offer_invalid".into()));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FenceOrder {
    pub dispatch_id: RequestId,
    pub delivery_generation: u64,
    pub registration_id: RequestId,
    pub runner_boot_id: RequestId,
    pub offer_digest: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum FenceOutcome {
    Denied { receipt: DeniedReceipt },
    NotRecordedAtBarrier,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FenceReceipt {
    pub dispatch_id: RequestId,
    pub delivery_generation: u64,
    pub registration_id: RequestId,
    pub runner_boot_id: RequestId,
    pub journal_sequence: u64,
    pub outcome: FenceOutcome,
}

impl RunnerRequest {
    pub fn protocol_version(&self) -> u16 {
        match self {
            Self::Ready {
                protocol_version, ..
            }
            | Self::Offer {
                protocol_version, ..
            }
            | Self::ReportDenied {
                protocol_version, ..
            }
            | Self::Reconcile {
                protocol_version, ..
            }
            | Self::ReportFence {
                protocol_version, ..
            } => *protocol_version,
        }
    }
}

impl DeniedReceipt {
    pub fn validate(&self) -> Result<(), crate::engine::EngineError> {
        if !(1..=DISPATCH_GENERATIONS_MAX).contains(&self.delivery_generation)
            || self.controller_generation == 0
            || self.journal_sequence == 0
            || self.native_identity.is_some()
            || !crate::engine::owner::is_hex_id(&self.offer_engine_epoch)
            || !crate::engine::owner::is_hex_id(&self.offer_digest)
            || self.dispatch_id != dispatch_id(&self.database_instance, &self.attempt)?
            || serde_json::to_vec(self)
                .map_err(|e| crate::engine::EngineError(e.to_string()))?
                .len()
                > DISPATCH_RECEIPT_BYTES_MAX
        {
            return Err(crate::engine::EngineError(
                "dispatch_receipt_invalid".into(),
            ));
        }
        Ok(())
    }
}
