use super::*;
use crate::coordination::{PlanState, Role, WorkerBinding};
use crate::worker_dispatch::*;
use crate::worktrees::{LeaseState, WorktreeLease};
use serde::{Deserialize, Serialize};

const ROW_BYTES_MAX: usize = 16384;

pub(crate) enum DispatchJob {
    Inspect(Admission, AttemptRef, u8),
    Prepare(
        Admission,
        Box<OwnerRequest>,
        Box<DispatchSnapshot>,
        Option<RunnerReady>,
    ),
    Lookup(RequestId),
    Mark(RequestId, DispatchState),
    Register(HostStamp, RunnerReady),
    Offer(RunnerReady, RequestId, u64, Box<DispatchSnapshot>),
    Report(RunnerReady, Box<DeniedReceipt>),
    Fence(RunnerReady, RequestId),
    ReportFence(
        RunnerReady,
        Box<FenceReceipt>,
        Option<Box<DispatchSnapshot>>,
    ),
    Change(Admission, Box<OwnerRequest>, Option<Box<DispatchSnapshot>>),
}

pub(crate) enum DispatchResult {
    Snapshot(Box<DispatchSnapshot>),
    Prepared(Box<OwnerApplied>),
    Row(Option<Box<DispatchRecord>>),
    Reply(RunnerReply),
    Done,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DispatchSnapshot {
    pub(crate) attempt: AttemptRef,
    pub(crate) repository_slot: u8,
    pub(crate) scope: ExecutionContext,
    pub(crate) binding: WorkerBinding,
    pub(crate) seal: DispatchLeaseSeal,
    lease_body: String,
    admission_body: String,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RetiredGeneration {
    offer_digest: Option<String>,
    offer_engine_epoch: String,
    fence: FenceReceipt,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DispatchRecord {
    pub(crate) view: DispatchView,
    pub(crate) snapshot: DispatchSnapshot,
    original_owner: Admission,
    authorizer: Admission,
    pub(crate) ready: Option<RunnerReady>,
    pub(crate) offer: Option<AdmissionCheckOffer>,
    pub(crate) notification_attempted: bool,
    pub(crate) fenced: bool,
    retired: Vec<RetiredGeneration>,
}

#[derive(Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredAdmission {
    repository: String,
    root: LeaseIdentity,
    metadata: LeaseIdentity,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredLease {
    lease: WorktreeLease,
    admission: StoredAdmission,
    head: String,
    directory: Option<LeaseIdentity>,
    metadata: Option<LeaseIdentity>,
}

#[cfg(unix)]
pub(crate) fn initialize(db: &Connection) -> Result<(), EngineError> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS worker_runner_registrations
        (id TEXT PRIMARY KEY, attempt TEXT NOT NULL UNIQUE, body BLOB NOT NULL CHECK(length(body)<=2048), active INTEGER NOT NULL CHECK(active IN (0,1)));
        CREATE TABLE IF NOT EXISTS worker_dispatches
        (id TEXT PRIMARY KEY, plan TEXT NOT NULL, item TEXT NOT NULL, cursor INTEGER NOT NULL CHECK(cursor>0), lease_task TEXT NOT NULL, repository TEXT NOT NULL, body BLOB NOT NULL CHECK(length(body)<=16384),
         UNIQUE(plan,item,cursor), UNIQUE(lease_task,repository));").map_err(error)
}

fn limited_body(
    tx: &Transaction<'_>,
    table: &str,
    column: &str,
    id: &str,
) -> Result<Option<String>, EngineError> {
    let query = match (table, column) {
        ("coordination_plans", "id") => {
            "SELECT CASE WHEN length(CAST(body AS BLOB))<=65536 THEN body END FROM coordination_plans WHERE id=?1"
        }
        ("projects", "id") => {
            "SELECT CASE WHEN length(CAST(body AS BLOB))<=65536 THEN body END FROM projects WHERE id=?1"
        }
        ("configs", "id") => {
            "SELECT CASE WHEN length(CAST(body AS BLOB))<=33554432 THEN body END FROM configs WHERE id=?1"
        }
        _ => return Err(error("dispatch_internal_table")),
    };
    tx.query_row(query, [id], |row| row.get::<_, Option<String>>(0))
        .optional()
        .map_err(error)?
        .map(|body| body.ok_or_else(|| error("dispatch_body_capacity")))
        .transpose()
}

pub(crate) fn snapshot(
    tx: &Transaction<'_>,
    config: &EffectiveConfig,
    attempt: &AttemptRef,
    slot: u8,
) -> Result<DispatchSnapshot, EngineError> {
    validate_prepare(attempt, slot)?;
    let body = limited_body(tx, "coordination_plans", "id", attempt.plan_id.as_str())?
        .ok_or_else(|| error("dispatch_plan_missing"))?;
    let state: PlanState = serde_json::from_str(&body).map_err(error)?;
    let Outcome::Coordination { state: checked } = crate::coordination::execute(
        tx,
        config,
        &Command::CoordinationState {
            plan_id: attempt.plan_id.clone(),
        },
        owner_revision(tx)?,
    )?
    else {
        return Err(error("dispatch_plan_missing"));
    };
    if *checked != state {
        return Err(error("dispatch_plan_changed"));
    }
    if state.plan.id != attempt.plan_id || !state.approved || state.revoked {
        return Err(error("dispatch_plan_unavailable"));
    }
    let item = state
        .plan
        .tasks
        .iter()
        .find(|item| item.id == attempt.item_id)
        .ok_or_else(|| error("dispatch_item_missing"))?;
    let reservation = state
        .reservations
        .get(&attempt.item_id)
        .ok_or_else(|| error("dispatch_reservation_missing"))?;
    let binding = reservation
        .binding
        .as_ref()
        .ok_or_else(|| error("dispatch_binding_missing"))?;
    if item.role != Role::Worker
        || item.native_descendant.is_some()
        || reservation.cursor != attempt.reservation_cursor
        || binding.plan_id != attempt.plan_id
        || binding.task_id != attempt.item_id
        || binding.attempt_cursor != attempt.reservation_cursor
        || binding.session_id != item.session_id
        || binding.config_id != state.plan.config_id
        || state.plan.config_id != state.plan.scope.config_id
        || task_context(tx, &state.plan.scope.task_id)?.as_ref() != Some(&state.plan.scope)
        || state
            .plan
            .scope
            .allowed_destinations
            .iter()
            .any(|target| config.settings.revoked_destinations.contains(target))
        || !config
            .settings
            .routing
            .approved_accounts
            .contains(&binding.connection.account_ref)
    {
        return Err(error("dispatch_authority_changed"));
    }
    let pinned: EffectiveConfig = serde_json::from_str(
        &limited_body(tx, "configs", "id", binding.config_id.as_str())?
            .ok_or_else(|| error("dispatch_config_missing"))?,
    )
    .map_err(error)?;
    pinned.validate().map_err(error)?;
    if pinned.id != binding.config_id
        || !state
            .plan
            .scope
            .allowed_destinations
            .contains(&binding.target_profile_ref)
    {
        return Err(error("dispatch_pinned_config_mismatch"));
    }
    crate::routing::validate_pool_scope(
        &pinned,
        &binding.pool_ref,
        &state.plan.scope.project_id,
        &item.role,
        &binding.connection.account_ref,
    )
    .map_err(error)?;
    let model = pinned
        .settings
        .model_access
        .as_ref()
        .ok_or_else(|| error("dispatch_model_access_missing"))?;
    if !model.connections.contains(&binding.connection) {
        return Err(error("dispatch_connection_changed"));
    }
    let project: StoredProject = serde_json::from_str(
        &limited_body(tx, "projects", "id", state.plan.scope.project_id.as_str())?
            .ok_or_else(|| error("dispatch_project_missing"))?,
    )
    .map_err(error)?;
    let repository = project
        .spec
        .repositories
        .get(usize::from(slot))
        .ok_or_else(|| error("dispatch_repository_slot"))?;
    if project
        .spec
        .repositories
        .iter()
        .filter(|path| *path == repository)
        .count()
        != 1
    {
        return Err(error("dispatch_repository_ambiguous"));
    }
    let lease_body: Option<String> = tx.query_row("SELECT CASE WHEN length(CAST(body AS BLOB))<=16384 THEN body END FROM worktree_leases WHERE task=?1 AND repository=?2", params![state.plan.scope.task_id.as_str(), repository], |row| row.get(0)).map_err(error)?;
    let lease_body = lease_body.ok_or_else(|| error("dispatch_lease_capacity"))?;
    let stored: StoredLease = serde_json::from_str(&lease_body).map_err(error)?;
    let admission_body: Option<String> = tx.query_row("SELECT CASE WHEN length(CAST(body AS BLOB))<=16384 THEN body END FROM repository_admissions WHERE project=?1 AND repository=?2", params![state.plan.scope.project_id.as_str(), repository], |row| row.get(0)).map_err(error)?;
    let admission_body = admission_body.ok_or_else(|| error("dispatch_admission_capacity"))?;
    let admission: StoredAdmission = serde_json::from_str(&admission_body).map_err(error)?;
    if stored.lease.project_id != state.plan.scope.project_id
        || stored.lease.task_id != state.plan.scope.task_id
        || stored.lease.repository != *repository
        || stored.lease.owner_session != item.session_id
        || stored.lease.native_descendant.is_some()
        || stored.lease.state != LeaseState::Ready
        || stored.admission != admission
        || admission.repository != *repository
    {
        return Err(error("dispatch_lease_mismatch_or_unavailable"));
    }
    let seal = DispatchLeaseSeal {
        project_id: stored.lease.project_id,
        task_id: stored.lease.task_id,
        repository: stored.lease.repository,
        path: stored.lease.path,
        branch: stored.lease.branch,
        owner_session: stored.lease.owner_session,
        native_descendant: stored.lease.native_descendant,
        admission_root: admission.root,
        admission_metadata: admission.metadata,
        directory: stored
            .directory
            .ok_or_else(|| error("dispatch_directory_unsealed"))?,
        metadata: stored
            .metadata
            .ok_or_else(|| error("dispatch_metadata_unsealed"))?,
        head: stored.head,
    };
    assert_eq!(seal.task_id, state.plan.scope.task_id);
    assert_eq!(seal.owner_session, binding.session_id);
    Ok(DispatchSnapshot {
        attempt: attempt.clone(),
        repository_slot: slot,
        scope: state.plan.scope.clone(),
        binding: binding.clone(),
        seal,
        lease_body,
        admission_body,
    })
}

fn assert_record(record: &DispatchRecord) {
    assert_eq!(
        record.view.dispatch_id,
        dispatch_id(&record.original_owner.db_instance, &record.snapshot.attempt)
            .expect("stored dispatch identity")
    );
    assert_eq!(record.view.attempt, record.snapshot.attempt);
    assert!((1..=DISPATCH_GENERATIONS_MAX).contains(&record.view.delivery_generation));
    assert_eq!(record.snapshot.binding.task_id, record.view.attempt.item_id);
    assert_eq!(record.snapshot.seal.task_id, record.snapshot.scope.task_id);
    assert_eq!(
        record.snapshot.seal.owner_session,
        record.snapshot.binding.session_id
    );
    if let Some(receipt) = &record.view.receipt {
        assert_eq!(record.view.state, DispatchState::DeliveredDenied);
        assert!(receipt.native_identity.is_none());
        assert_eq!(receipt.dispatch_id, record.view.dispatch_id);
        assert_eq!(
            Some(&receipt.offer_digest),
            record.view.offer_digest.as_ref()
        );
    }
    if let Some(offer) = &record.offer {
        assert_eq!(offer.attempt, record.view.attempt);
        assert_eq!(offer.binding, record.snapshot.binding);
        assert_eq!(offer.lease_seal, record.snapshot.seal);
        assert_eq!(Some(&offer.offer_digest), record.view.offer_digest.as_ref());
    }
}

pub(crate) fn load(
    tx: &Transaction<'_>,
    id: &RequestId,
) -> Result<Option<DispatchRecord>, EngineError> {
    let bytes = tx
        .query_row(
            "SELECT CASE WHEN length(body)<=16384 THEN body END FROM worker_dispatches WHERE id=?1",
            [id.as_str()],
            |row| row.get::<_, Option<Vec<u8>>>(0),
        )
        .optional()
        .map_err(error)?
        .map(|bytes| bytes.ok_or_else(|| error("dispatch_row_capacity")))
        .transpose()?;
    bytes
        .map(|bytes| {
            let record: DispatchRecord = serde_json::from_slice(&bytes).map_err(error)?;
            if serde_json::to_vec(&record.view).map_err(error)?.len() > DISPATCH_VIEW_BYTES_MAX {
                return Err(error("dispatch_view_capacity"));
            }
            assert_record(&record);
            assert_eq!(&record.view.dispatch_id, id);
            Ok(record)
        })
        .transpose()
}

fn encode(record: &DispatchRecord) -> Result<Vec<u8>, EngineError> {
    assert_record(record);
    let bytes = serde_json::to_vec(record).map_err(error)?;
    if bytes.len() > ROW_BYTES_MAX
        || serde_json::to_vec(&record.view).map_err(error)?.len() > DISPATCH_VIEW_BYTES_MAX
    {
        return Err(error("dispatch_row_or_view_capacity"));
    }
    Ok(bytes)
}

fn save(tx: &Transaction<'_>, record: &DispatchRecord) -> Result<(), EngineError> {
    let changed = tx
        .execute(
            "UPDATE worker_dispatches SET body=?2 WHERE id=?1",
            params![record.view.dispatch_id.as_str(), encode(record)?],
        )
        .map_err(error)?;
    assert_eq!(changed, 1);
    Ok(())
}

fn event(
    tx: &Transaction<'_>,
    config: &EffectiveConfig,
    id: &RequestId,
    principal: &str,
) -> Result<u64, EngineError> {
    let next = owner_revision(tx)?
        .checked_add(1)
        .filter(|value| *value <= i64::MAX as u64)
        .ok_or_else(|| error("revision_exhausted"))?;
    persist_event_ids(
        tx,
        config,
        id,
        &ClientId::new(principal).map_err(error)?,
        next,
    )?;
    Ok(next)
}

fn registration(tx: &Transaction<'_>, ready: &RunnerReady) -> Result<(), EngineError> {
    check_owner_host(
        tx,
        &HostStamp {
            db_instance: ready.database_instance.clone(),
            engine_epoch: ready.engine_epoch.clone(),
        },
    )?;
    let stored: Option<Vec<u8>> = tx.query_row("SELECT CASE WHEN length(body)<=2048 THEN body END FROM worker_runner_registrations WHERE id=?1 AND active=1", [ready.registration_id.as_str()], |row| row.get(0)).optional().map_err(error)?.flatten();
    if stored.as_deref() != Some(serde_json::to_vec(ready).map_err(error)?.as_slice()) {
        return Err(error("dispatch_registration_unavailable"));
    }
    Ok(())
}

fn prepare(
    tx: &Transaction<'_>,
    config: &EffectiveConfig,
    admission: &Admission,
    request: &OwnerRequest,
    observed: &DispatchSnapshot,
    ready: Option<RunnerReady>,
) -> Result<OwnerApplied, EngineError> {
    request.validate()?;
    check_owner_admission(tx, admission)?;
    let revision = owner_revision(tx)?;
    let hash_text = request.mutation_hash()?;
    let bytes = serde_json::to_vec(request).map_err(error)?;
    let hash = Sha256::digest(&bytes);
    if let Some((provenance, result)) = owner_lookup(
        tx,
        "native_owner",
        &admission.owner_id,
        &request.request_id,
        Some(&hash_text),
    )? {
        return Ok(OwnerApplied {
            result: serde_json::from_slice(&result).map_err(error)?,
            provenance,
            historical_replay: true,
            current_revision: Cursor::new(revision),
        });
    }
    let OwnerOperation::Apply {
        mutation:
            OwnerMutation::PrepareWorkerDispatch {
                attempt,
                repository_slot,
            },
    } = &request.operation
    else {
        return Err(error("dispatch_prepare_required"));
    };
    let id = dispatch_id(&admission.db_instance, attempt)?;
    let existing = load(tx, &id)?;
    let result = if request.expected_revision.value() != revision {
        OwnerResult::Rejected {
            code: "stale_revision".into(),
        }
    } else {
        let actual = snapshot(tx, config, attempt, *repository_slot)?;
        if &actual != observed {
            return Err(error("dispatch_observation_stale"));
        }
        if let Some(existing) = &existing {
            if existing.snapshot != actual {
                return Err(error("dispatch_attempt_conflict"));
            }
        } else {
            let count: i64 = tx
                .query_row("SELECT COUNT(*) FROM worker_dispatches", [], |row| {
                    row.get(0)
                })
                .map_err(error)?;
            if count >= DISPATCH_OPERATIONS_MAX as i64 {
                return Err(error("dispatch_capacity"));
            }
            if let Some(ready) = &ready {
                registration(tx, ready)?;
                if ready.attempt != *attempt {
                    return Err(error("dispatch_registration_attempt"));
                }
            }
            let mut seal_bytes = b"crazyagent-dispatch-lease-v1".to_vec();
            seal_bytes.extend(serde_json::to_vec(&actual.seal).map_err(error)?);
            let record = DispatchRecord {
                view: DispatchView {
                    dispatch_id: id.clone(),
                    attempt: attempt.clone(),
                    state: if ready.is_some() {
                        DispatchState::Queued
                    } else {
                        DispatchState::RunnerUnavailable
                    },
                    delivery_generation: 1,
                    binding_digest: crate::memory::hash(
                        &serde_json::to_vec(&actual.binding).map_err(error)?,
                    ),
                    lease_digest: crate::memory::hash(&seal_bytes),
                    offer_digest: None,
                    cancellation: DispatchCancellation::None,
                    receipt: None,
                },
                snapshot: actual,
                original_owner: admission.clone(),
                authorizer: admission.clone(),
                ready,
                offer: None,
                notification_attempted: false,
                fenced: false,
                retired: vec![],
            };
            tx.execute(
                "INSERT INTO worker_dispatches VALUES (?1,?2,?3,?4,?5,?6,?7)",
                params![
                    id.as_str(),
                    attempt.plan_id.as_str(),
                    attempt.item_id.as_str(),
                    attempt.reservation_cursor.value() as i64,
                    record.snapshot.seal.task_id.as_str(),
                    record.snapshot.seal.repository,
                    encode(&record)?
                ],
            )
            .map_err(error)?;
        }
        OwnerResult::DispatchPrepared { dispatch_id: id }
    };
    let changed = existing.is_none() && matches!(result, OwnerResult::DispatchPrepared { .. });
    let next = if changed {
        event(
            tx,
            config,
            &request.request_id,
            &format!("owner:{}", admission.owner_id),
        )?
    } else {
        revision
    };
    let provenance = OwnerProvenance {
        origin: "native_owner".into(),
        owner_id: admission.owner_id.clone(),
        operation_id: request.request_id.clone(),
        request_hash: hash_text,
        engine_epoch: admission.engine_epoch.clone(),
        admitted_epoch: admission.owner_epoch,
        resulting_epoch: admission.owner_epoch,
        revision_before: Cursor::new(revision),
        revision_after: Cursor::new(next),
        event_cursor: changed.then(|| Cursor::new(next)),
    };
    owner_record(
        tx,
        &provenance,
        &hash,
        &serde_json::to_vec(&result).map_err(error)?,
    )?;
    Ok(OwnerApplied {
        result,
        provenance,
        historical_replay: false,
        current_revision: Cursor::new(next),
    })
}

fn offer(
    tx: &Transaction<'_>,
    config: &EffectiveConfig,
    ready: &RunnerReady,
    id: &RequestId,
    generation: u64,
    observed: &DispatchSnapshot,
) -> Result<RunnerReply, EngineError> {
    registration(tx, ready)?;
    let mut record = load(tx, id)?.ok_or_else(|| error("dispatch_missing"))?;
    if record.ready.as_ref() != Some(ready)
        || record.view.delivery_generation != generation
        || record.fenced
        || record.view.cancellation != DispatchCancellation::None
        || !matches!(
            record.view.state,
            DispatchState::Queued | DispatchState::Offered | DispatchState::OutcomeUnknown
        )
    {
        return Err(error("dispatch_offer_unavailable"));
    }
    check_owner_admission(tx, &record.authorizer)?;
    let actual = snapshot(
        tx,
        config,
        &record.view.attempt,
        record.snapshot.repository_slot,
    )?;
    if actual != record.snapshot || &actual != observed {
        return Err(error("dispatch_lease_or_binding_changed"));
    }
    if let Some(offer) = record.offer {
        return Ok(RunnerReply::Offer {
            offer: Box::new(offer),
        });
    }
    let mut offer = AdmissionCheckOffer {
        dispatch_id: id.clone(),
        attempt: record.view.attempt.clone(),
        delivery_generation: generation,
        database_instance: ready.database_instance.clone(),
        engine_epoch: ready.engine_epoch.clone(),
        registration_id: ready.registration_id.clone(),
        runner_boot_id: ready.boot.runner_id.clone(),
        binding: actual.binding,
        lease_seal: actual.seal,
        offer_digest: String::new(),
    };
    offer.offer_digest = offer.digest()?;
    offer.validate()?;
    record.view.offer_digest = Some(offer.offer_digest.clone());
    record.view.state = DispatchState::Offered;
    record.offer = Some(offer.clone());
    save(tx, &record)?;
    event(
        tx,
        config,
        id,
        &format!("runner:{}", ready.registration_id.as_str()),
    )?;
    Ok(RunnerReply::Offer {
        offer: Box::new(offer),
    })
}

fn report(
    tx: &Transaction<'_>,
    config: &EffectiveConfig,
    ready: &RunnerReady,
    receipt: &DeniedReceipt,
) -> Result<RunnerReply, EngineError> {
    registration(tx, ready)?;
    receipt.validate()?;
    let mut record = load(tx, &receipt.dispatch_id)?.ok_or_else(|| error("dispatch_missing"))?;
    let offer = record
        .offer
        .as_ref()
        .ok_or_else(|| error("dispatch_not_offered"))?;
    if record.ready.as_ref() != Some(ready)
        || receipt.attempt != offer.attempt
        || receipt.delivery_generation != offer.delivery_generation
        || receipt.database_instance != offer.database_instance
        || receipt.offer_engine_epoch != offer.engine_epoch
        || receipt.registration_id != offer.registration_id
        || receipt.runner_boot_id != offer.runner_boot_id
        || receipt.offer_digest != offer.offer_digest
    {
        return Err(error("dispatch_receipt_mismatch"));
    }
    if let Some(existing) = &record.view.receipt {
        if existing != receipt {
            return Err(error("dispatch_receipt_conflict"));
        }
    } else {
        record.view.receipt = Some(receipt.clone());
        record.view.state = DispatchState::DeliveredDenied;
        save(tx, &record)?;
        event(
            tx,
            config,
            &receipt.dispatch_id,
            &format!("runner:{}", ready.registration_id.as_str()),
        )?;
    }
    Ok(RunnerReply::DeniedRecorded {
        dispatch_id: receipt.dispatch_id.clone(),
    })
}

fn fence(
    tx: &Transaction<'_>,
    config: &EffectiveConfig,
    ready: &RunnerReady,
    id: &RequestId,
) -> Result<RunnerReply, EngineError> {
    registration(tx, ready)?;
    let mut record = load(tx, id)?.ok_or_else(|| error("dispatch_missing"))?;
    let original = record
        .ready
        .as_ref()
        .ok_or_else(|| error("dispatch_registration_missing"))?;
    if original.registration_id != ready.registration_id || original.boot != ready.boot {
        return Err(error("dispatch_fence_boot_mismatch"));
    }
    if !record.fenced {
        return Err(error("dispatch_fence_requires_owner_reconcile"));
    }
    if record.view.state != DispatchState::DeliveredDenied
        && record.view.state != DispatchState::CancelledBeforeOffer
    {
        record.view.state = DispatchState::ReconciliationRequired;
        save(tx, &record)?;
        event(
            tx,
            config,
            id,
            &format!("runner:{}", ready.registration_id.as_str()),
        )?;
    }
    Ok(RunnerReply::Fence {
        order: FenceOrder {
            dispatch_id: id.clone(),
            delivery_generation: record.view.delivery_generation,
            registration_id: original.registration_id.clone(),
            runner_boot_id: original.boot.runner_id.clone(),
            offer_digest: record.view.offer_digest.clone(),
        },
    })
}

fn report_fence(
    tx: &Transaction<'_>,
    config: &EffectiveConfig,
    ready: &RunnerReady,
    receipt: &FenceReceipt,
    observed: Option<&DispatchSnapshot>,
) -> Result<RunnerReply, EngineError> {
    registration(tx, ready)?;
    let mut record = load(tx, &receipt.dispatch_id)?.ok_or_else(|| error("dispatch_missing"))?;
    let original = record
        .ready
        .as_ref()
        .ok_or_else(|| error("dispatch_registration_missing"))?;
    if !record.fenced
        || ready.registration_id != original.registration_id
        || ready.attempt != record.view.attempt
        || receipt.delivery_generation != record.view.delivery_generation
        || receipt.registration_id != original.registration_id
        || receipt.runner_boot_id != original.boot.runner_id
        || ready.boot != original.boot
        || receipt.journal_sequence == 0
        || serde_json::to_vec(receipt).map_err(error)?.len() > DISPATCH_RECEIPT_BYTES_MAX
    {
        return Err(error("dispatch_fence_receipt_mismatch"));
    }
    match &receipt.outcome {
        FenceOutcome::Denied { receipt: denied } => {
            if denied.dispatch_id != receipt.dispatch_id
                || denied.delivery_generation != receipt.delivery_generation
                || denied.journal_sequence > receipt.journal_sequence
            {
                return Err(error("dispatch_fence_denial_mismatch"));
            }
            report(tx, config, ready, denied.as_ref())?;
        }
        FenceOutcome::NotRecordedAtBarrier => {
            if record.view.receipt.is_some() {
                return Err(error("dispatch_absence_conflicts_with_receipt"));
            }
            if let Some(last) = record
                .retired
                .iter()
                .find(|retired| retired.fence.delivery_generation == receipt.delivery_generation)
            {
                if last.fence != *receipt {
                    return Err(error("dispatch_fence_history_conflict"));
                }
            } else {
                record.retired.push(RetiredGeneration {
                    offer_digest: record.view.offer_digest.clone(),
                    offer_engine_epoch: original.engine_epoch.clone(),
                    fence: receipt.clone(),
                });
                let current = snapshot(
                    tx,
                    config,
                    &record.view.attempt,
                    record.snapshot.repository_slot,
                )
                .ok();
                record.view.state = if observed == Some(&record.snapshot)
                    && current.as_ref() == Some(&record.snapshot)
                {
                    DispatchState::NotConsumed
                } else {
                    DispatchState::BlockedRecovery
                };
                save(tx, &record)?;
                event(
                    tx,
                    config,
                    &receipt.dispatch_id,
                    &format!("runner:{}", ready.registration_id.as_str()),
                )?;
            }
        }
    }
    Ok(RunnerReply::FenceRecorded {
        dispatch_id: receipt.dispatch_id.clone(),
    })
}

fn change(
    tx: &Transaction<'_>,
    config: &EffectiveConfig,
    admission: &Admission,
    request: &OwnerRequest,
    observed: Option<&DispatchSnapshot>,
) -> Result<OwnerApplied, EngineError> {
    check_owner_admission(tx, admission)?;
    request.validate()?;
    let hash_text = request.mutation_hash()?;
    let hash = Sha256::digest(serde_json::to_vec(request).map_err(error)?);
    let revision = owner_revision(tx)?;
    if let Some((provenance, bytes)) = owner_lookup(
        tx,
        "native_owner",
        &admission.owner_id,
        &request.request_id,
        Some(&hash_text),
    )? {
        return Ok(OwnerApplied {
            result: serde_json::from_slice(&bytes).map_err(error)?,
            provenance,
            historical_replay: true,
            current_revision: Cursor::new(revision),
        });
    }
    if request.expected_revision.value() != revision {
        return Err(error("dispatch_stale_revision"));
    }
    let OwnerOperation::Apply { mutation } = &request.operation else {
        return Err(error("dispatch_owner_mutation_required"));
    };
    let id = match mutation {
        OwnerMutation::CancelWorkerDispatch { dispatch_id }
        | OwnerMutation::ReconcileWorkerDispatch { dispatch_id }
        | OwnerMutation::ResumeWorkerDispatch { dispatch_id, .. } => dispatch_id,
        _ => return Err(error("dispatch_owner_action_required")),
    };
    let mut record = load(tx, id)?.ok_or_else(|| error("dispatch_missing"))?;
    match mutation {
        OwnerMutation::CancelWorkerDispatch { .. } => {
            record.view.cancellation = DispatchCancellation::Requested;
            record.fenced = true;
            if record.offer.is_none() && !record.notification_attempted {
                record.view.state = DispatchState::CancelledBeforeOffer;
            } else if record.view.receipt.is_none() {
                record.view.state = DispatchState::ReconciliationRequired;
            }
        }
        OwnerMutation::ReconcileWorkerDispatch { .. } => {
            record.fenced = true;
            if record.view.receipt.is_none()
                && record.view.state != DispatchState::CancelledBeforeOffer
            {
                record.view.state = DispatchState::ReconciliationRequired;
            }
        }
        OwnerMutation::ResumeWorkerDispatch {
            expected_delivery_generation,
            ..
        } => {
            if *expected_delivery_generation != record.view.delivery_generation
                || record.view.state != DispatchState::NotConsumed
                || record.view.cancellation != DispatchCancellation::None
                || record.view.delivery_generation >= DISPATCH_GENERATIONS_MAX
                || !record.fenced
                || record
                    .retired
                    .last()
                    .map(|retired| retired.fence.delivery_generation)
                    != Some(*expected_delivery_generation)
            {
                return Err(error("dispatch_resume_requires_proven_absence"));
            }
            let actual = snapshot(
                tx,
                config,
                &record.view.attempt,
                record.snapshot.repository_slot,
            )?;
            if observed != Some(&actual) || actual != record.snapshot {
                return Err(error("dispatch_resume_observation_changed"));
            }
            let ready = record
                .ready
                .as_ref()
                .ok_or_else(|| error("dispatch_registration_missing"))?;
            registration(tx, ready)?;
            record.authorizer = admission.clone();
            record.view.delivery_generation += 1;
            record.view.state = DispatchState::Queued;
            record.view.offer_digest = None;
            record.offer = None;
            record.notification_attempted = false;
            record.fenced = false;
        }
        _ => return Err(error("dispatch_owner_action_required")),
    }
    save(tx, &record)?;
    let next = event(
        tx,
        config,
        &request.request_id,
        &format!("owner:{}", admission.owner_id),
    )?;
    let result = OwnerResult::DispatchPrepared {
        dispatch_id: id.clone(),
    };
    let provenance = OwnerProvenance {
        origin: "native_owner".into(),
        owner_id: admission.owner_id.clone(),
        operation_id: request.request_id.clone(),
        request_hash: hash_text,
        engine_epoch: admission.engine_epoch.clone(),
        admitted_epoch: admission.owner_epoch,
        resulting_epoch: admission.owner_epoch,
        revision_before: Cursor::new(revision),
        revision_after: Cursor::new(next),
        event_cursor: Some(Cursor::new(next)),
    };
    owner_record(
        tx,
        &provenance,
        &hash,
        &serde_json::to_vec(&result).map_err(error)?,
    )?;
    Ok(OwnerApplied {
        result,
        provenance,
        historical_replay: false,
        current_revision: Cursor::new(next),
    })
}

pub(crate) fn execute(
    db: &mut Connection,
    config: &EffectiveConfig,
    job: DispatchJob,
) -> Result<DispatchResult, EngineError> {
    settings::ensure_clear(db)?;
    let tx = db
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(error)?;
    let result = match job {
        DispatchJob::Inspect(admission, attempt, slot) => {
            check_owner_admission(&tx, &admission)?;
            DispatchResult::Snapshot(Box::new(snapshot(&tx, config, &attempt, slot)?))
        }
        DispatchJob::Prepare(admission, request, observed, ready) => {
            DispatchResult::Prepared(Box::new(prepare(
                &tx, config, &admission, &request, &observed, ready,
            )?))
        }
        DispatchJob::Lookup(id) => DispatchResult::Row(load(&tx, &id)?.map(Box::new)),
        DispatchJob::Register(host, ready) => {
            check_owner_host(&tx, &host)?;
            if host.db_instance != ready.database_instance
                || host.engine_epoch != ready.engine_epoch
            {
                return Err(error("dispatch_registration_host"));
            }
            let bytes = serde_json::to_vec(&ready).map_err(error)?;
            if bytes.len() > RUNNER_BOOTSTRAP_MAX {
                return Err(error("dispatch_registration_bounds"));
            }
            let existing: Option<Vec<u8>> = tx
                .query_row(
                    "SELECT body FROM worker_runner_registrations WHERE id=?1",
                    [ready.registration_id.as_str()],
                    |row| row.get(0),
                )
                .optional()
                .map_err(error)?;
            if let Some(existing) = existing {
                if existing != bytes {
                    return Err(error("dispatch_registration_conflict"));
                }
            } else {
                let count: i64 = tx
                    .query_row(
                        "SELECT COUNT(*) FROM worker_runner_registrations",
                        [],
                        |row| row.get(0),
                    )
                    .map_err(error)?;
                if count >= RUNNERS_MAX as i64 {
                    return Err(error("dispatch_registration_capacity"));
                }
                tx.execute(
                    "INSERT INTO worker_runner_registrations VALUES (?1,?2,?3,1)",
                    params![
                        ready.registration_id.as_str(),
                        serde_json::to_string(&ready.attempt).map_err(error)?,
                        bytes
                    ],
                )
                .map_err(error)?;
            }
            DispatchResult::Done
        }
        DispatchJob::Mark(id, requested) => {
            let mut record = load(&tx, &id)?.ok_or_else(|| error("dispatch_missing"))?;
            if record.view.receipt.is_none()
                && !matches!(
                    record.view.state,
                    DispatchState::CancelledBeforeOffer
                        | DispatchState::NotConsumed
                        | DispatchState::BlockedRecovery
                )
            {
                match requested {
                    DispatchState::Queued if !record.notification_attempted && !record.fenced => {
                        record.notification_attempted = true;
                        record.view.state = DispatchState::OutcomeUnknown;
                    }
                    DispatchState::RunnerUnavailable if !record.notification_attempted => {
                        record.view.state = DispatchState::RunnerUnavailable;
                    }
                    DispatchState::OutcomeUnknown
                        if matches!(
                            record.view.state,
                            DispatchState::Queued
                                | DispatchState::Offered
                                | DispatchState::OutcomeUnknown
                        ) =>
                    {
                        record.view.state = DispatchState::OutcomeUnknown;
                        record.fenced = true;
                    }
                    DispatchState::OutcomeUnknown => {}
                    _ => return Err(error("dispatch_notification_already_attempted")),
                }
                save(&tx, &record)?;
                event(&tx, config, &id, "trusted-dispatch-driver")?;
            }
            DispatchResult::Done
        }
        DispatchJob::Offer(ready, id, generation, observed) => {
            DispatchResult::Reply(offer(&tx, config, &ready, &id, generation, &observed)?)
        }
        DispatchJob::Report(ready, receipt) => {
            DispatchResult::Reply(report(&tx, config, &ready, &receipt)?)
        }
        DispatchJob::Fence(ready, id) => DispatchResult::Reply(fence(&tx, config, &ready, &id)?),
        DispatchJob::ReportFence(ready, receipt, observed) => DispatchResult::Reply(report_fence(
            &tx,
            config,
            &ready,
            &receipt,
            observed.as_deref(),
        )?),
        DispatchJob::Change(admission, request, observed) => DispatchResult::Prepared(Box::new(
            change(&tx, config, &admission, &request, observed.as_deref())?,
        )),
    };
    tx.commit().map_err(error)?;
    Ok(result)
}

#[cfg(unix)]
pub(crate) fn restart(db: &mut Connection, config: &EffectiveConfig) -> Result<(), EngineError> {
    let tx = db
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(error)?;
    tx.execute("UPDATE worker_runner_registrations SET active=0", [])
        .map_err(error)?;
    let ids = {
        let mut query = tx
            .prepare("SELECT id FROM worker_dispatches ORDER BY id LIMIT 129")
            .map_err(error)?;
        query
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(error)?
    };
    if ids.len() > DISPATCH_OPERATIONS_MAX {
        return Err(error("dispatch_capacity_corrupt"));
    }
    for id in ids {
        let id = RequestId::new(id).map_err(error)?;
        let mut record = load(&tx, &id)?.ok_or_else(|| error("dispatch_missing"))?;
        if record.view.receipt.is_none() && record.view.state != DispatchState::CancelledBeforeOffer
        {
            record.view.state = DispatchState::ReconciliationRequired;
            record.fenced = true;
            save(&tx, &record)?;
            event(&tx, config, &id, "trusted-dispatch-restart")?;
        }
    }
    tx.commit().map_err(error)
}
