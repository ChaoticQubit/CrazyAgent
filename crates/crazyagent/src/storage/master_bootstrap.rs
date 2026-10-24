use super::{check_owner_admission, owner_lookup, owner_revision, random_id};
use crate::engine::EngineError;
use crate::engine::owner::{
    Admission, OwnerMutation, OwnerOperation, OwnerProvenance, OwnerRequest, OwnerResult, is_hex_id,
};
use crate::master_session::{
    MasterBootstrapAuth, MasterOwnerAction, MasterSessionPhase, MasterSessionView,
};
use crate::protocol::{
    ConfigId, Cursor, FRAME_BYTES_MAX, Outcome, PROTOCOL_VERSION, ProjectId, RequestId, SessionId,
    StoredProject, TaskId,
};
use crate::worktrees::{self, LeaseState, WorktreeLease};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::Path;

const PURPOSE: &str = "pre_plan_master_conversation";
const RECEIPTS_MAX: i64 = 128;
const RECEIPT_BYTES_MAX: usize = 32768;
const SESSION_BYTES_MAX: i64 = 16384;
const ADMISSION_BYTES_MAX: i64 = 16384;
const SCHEMA: &str = "CREATE TABLE bootstrap_workspaces (
    session TEXT PRIMARY KEY NOT NULL CHECK(length(CAST(session AS BLOB)) BETWEEN 1 AND 128),
    allocation TEXT NOT NULL UNIQUE CHECK(length(CAST(allocation AS BLOB)) BETWEEN 1 AND 128),
    body TEXT NOT NULL CHECK(length(CAST(body AS BLOB)) BETWEEN 1 AND 32768)
)";

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    allocation: RequestId,
    purpose: String,
    project_id: ProjectId,
    session_id: SessionId,
    bootstrap_task_id: TaskId,
    config_id: ConfigId,
    source_hash: String,
    owner: OwnerProvenance,
    at_revision: Cursor,
    proof: MasterBootstrapAuth,
    admission_sha256: String,
    lease: WorktreeLease,
}

fn error(value: impl std::fmt::Display) -> EngineError {
    EngineError(value.to_string())
}

pub(super) fn initialize(db: &Connection) -> Result<(), EngineError> {
    let tx = db.unchecked_transaction().map_err(error)?;
    {
        let mut statement = tx
            .prepare(
                "SELECT CASE WHEN length(CAST(name AS BLOB))<=128 THEN name END,
                 CASE WHEN length(CAST(sql AS BLOB))<=4096 THEN sql END
                 FROM sqlite_schema WHERE sql IS NOT NULL
                 AND (name='bootstrap_workspaces' OR tbl_name='bootstrap_workspaces')
                 ORDER BY name LIMIT 2",
            )
            .map_err(error)?;
        let actual = statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(error)?;
        if actual.is_empty() {
            tx.execute_batch(SCHEMA).map_err(error)?;
        } else if actual != [("bootstrap_workspaces".to_owned(), SCHEMA.to_owned())] {
            return Err(error("master_bootstrap_schema_conflict"));
        }
    }
    assert!(!tx.is_autocommit());
    tx.commit().map_err(error)?;
    assert!(db.is_autocommit());
    Ok(())
}

fn current_session(tx: &Transaction<'_>, session: &MasterSessionView) -> Result<(), EngineError> {
    let version = i64::try_from(session.version.value()).map_err(error)?;
    let created = i64::try_from(session.owner.revision_after.value()).map_err(error)?;
    let body: Option<String> = tx
        .query_row(
            "SELECT body FROM master_sessions
             WHERE id=?1 AND project=?2 AND phase='configured' AND version=?3 AND created=?4
             AND length(CAST(body AS BLOB)) BETWEEN 1 AND ?5",
            params![
                session.session_id.as_str(),
                session.project_id.as_str(),
                version,
                created,
                SESSION_BYTES_MAX,
            ],
            |row| row.get(0),
        )
        .optional()
        .map_err(error)?;
    let body = body.ok_or_else(|| error("master_bootstrap_scope_conflict"))?;
    let stored: MasterSessionView = serde_json::from_str(&body).map_err(error)?;
    let revision = owner_revision(tx)?;
    if stored != *session
        || session.phase != MasterSessionPhase::Configured
        || session.version.value() < session.owner.revision_after.value()
        || session.version.value() > revision
    {
        return Err(error("master_bootstrap_scope_conflict"));
    }
    let instance: String = tx
        .query_row(
            "SELECT db_instance FROM owner_meta WHERE singleton=1",
            [],
            |row| row.get(0),
        )
        .map_err(error)?;
    check_owner_admission(
        tx,
        &Admission {
            db_instance: instance,
            engine_epoch: session.owner.engine_epoch.clone(),
            owner_id: session.owner.owner_id.clone(),
            owner_epoch: session.owner.resulting_epoch,
        },
    )?;
    committed_open(tx, session)?;
    let task: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM tasks WHERE id=?1)",
            [session.bootstrap_task_id.as_str()],
            |row| row.get(0),
        )
        .map_err(error)?;
    if task {
        return Err(error("master_bootstrap_scope_conflict"));
    }
    assert_eq!(stored.session_id, session.session_id);
    assert_eq!(stored.bootstrap_task_id, session.bootstrap_task_id);
    Ok(())
}

fn committed_open(tx: &Transaction<'_>, session: &MasterSessionView) -> Result<(), EngineError> {
    let owner = &session.owner;
    let Some((actual, result)) = owner_lookup(
        tx,
        "native_owner",
        &owner.owner_id,
        &owner.operation_id,
        Some(&owner.request_hash),
    )?
    else {
        return Err(error("master_bootstrap_scope_conflict"));
    };
    let request = OwnerRequest {
        protocol_version: PROTOCOL_VERSION,
        request_id: owner.operation_id.clone(),
        expected_revision: owner.revision_before.clone(),
        operation: OwnerOperation::Apply {
            mutation: OwnerMutation::ProjectMaster {
                action: MasterOwnerAction::Open {
                    project_id: session.project_id.clone(),
                    expected_config_id: session.config_id.clone(),
                    expected_source_hash: session.source_hash.clone(),
                    selection: Box::new(session.selection.clone()),
                },
            },
        },
    };
    if actual != *owner
        || serde_json::from_slice::<OwnerResult>(&result).map_err(error)? != OwnerResult::Applied
        || request.mutation_hash()? != owner.request_hash
        || owner.revision_before.value().checked_add(1) != Some(owner.revision_after.value())
        || owner.event_cursor.as_ref() != Some(&owner.revision_after)
    {
        return Err(error("master_bootstrap_scope_conflict"));
    }
    assert_eq!(actual, *owner);
    assert_eq!(actual.origin, "native_owner");
    Ok(())
}

fn declared_repository(
    tx: &Transaction<'_>,
    session: &MasterSessionView,
    repository: &str,
) -> Result<(), EngineError> {
    if repository.is_empty() || repository.len() > 4096 || !Path::new(repository).is_absolute() {
        return Err(error("master_bootstrap_scope_conflict"));
    }
    let body: Option<String> = tx
        .query_row(
            "SELECT body FROM projects WHERE id=?1
             AND length(CAST(body AS BLOB)) BETWEEN 1 AND ?2",
            params![session.project_id.as_str(), FRAME_BYTES_MAX as i64],
            |row| row.get(0),
        )
        .optional()
        .map_err(error)?;
    let project: StoredProject =
        serde_json::from_str(&body.ok_or_else(|| error("master_bootstrap_scope_conflict"))?)
            .map_err(error)?;
    project.spec.validate().map_err(error)?;
    if project.project_id != session.project_id
        || !project
            .spec
            .repositories
            .iter()
            .any(|item| item == repository)
    {
        return Err(error("master_bootstrap_scope_conflict"));
    }
    assert_eq!(project.project_id, session.project_id);
    assert!(Path::new(repository).is_absolute());
    Ok(())
}

fn admission_hash(
    tx: &Transaction<'_>,
    session: &MasterSessionView,
    repository: &str,
) -> Result<Option<String>, EngineError> {
    let row: Option<Option<String>> = tx
        .query_row(
            "SELECT CASE WHEN length(CAST(body AS BLOB)) BETWEEN 1 AND ?3 THEN body END
             FROM repository_admissions WHERE project=?1 AND repository=?2",
            params![session.project_id.as_str(), repository, ADMISSION_BYTES_MAX],
            |row| row.get(0),
        )
        .optional()
        .map_err(error)?;
    row.map(|body| {
        let body = body.ok_or_else(|| error("master_bootstrap_receipt_conflict"))?;
        Ok(format!("{:x}", Sha256::digest(body.as_bytes())))
    })
    .transpose()
}

fn load_receipt(
    tx: &Transaction<'_>,
    session: &MasterSessionView,
) -> Result<Option<Receipt>, EngineError> {
    let row: Option<(Option<String>, Option<String>)> = tx
        .query_row(
            "SELECT CASE WHEN length(CAST(allocation AS BLOB)) BETWEEN 1 AND 128
                 THEN allocation END,
                 CASE WHEN length(CAST(body AS BLOB)) BETWEEN 1 AND ?2 THEN body END
             FROM bootstrap_workspaces WHERE session=?1",
            params![session.session_id.as_str(), RECEIPT_BYTES_MAX as i64],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(error)?;
    row.map(|(allocation, body)| {
        let allocation = allocation.ok_or_else(|| error("master_bootstrap_receipt_conflict"))?;
        let body = body.ok_or_else(|| error("master_bootstrap_receipt_conflict"))?;
        let receipt: Receipt =
            serde_json::from_str(&body).map_err(|_| error("master_bootstrap_receipt_conflict"))?;
        if receipt.allocation.as_str() != allocation {
            return Err(error("master_bootstrap_receipt_conflict"));
        }
        assert!(body.len() <= RECEIPT_BYTES_MAX);
        assert_eq!(receipt.allocation.as_str(), allocation);
        Ok(receipt)
    })
    .transpose()
}

fn only_lease(
    tx: &Transaction<'_>,
    session: &MasterSessionView,
) -> Result<WorktreeLease, EngineError> {
    let Outcome::Worktrees { mut leases } = worktrees::leases(tx, &session.bootstrap_task_id)?
    else {
        return Err(error("master_bootstrap_receipt_conflict"));
    };
    if leases.len() != 1 {
        return Err(error("master_bootstrap_receipt_conflict"));
    }
    let lease = leases.remove(0);
    if lease.project_id != session.project_id
        || lease.task_id != session.bootstrap_task_id
        || lease.owner_session != session.session_id
        || lease.native_descendant.is_some()
        || !Path::new(&lease.repository).is_absolute()
        || !Path::new(&lease.path).is_absolute()
    {
        return Err(error("master_bootstrap_receipt_conflict"));
    }
    assert!(leases.is_empty());
    assert!(lease.native_descendant.is_none());
    Ok(lease)
}

fn validate_receipt(
    tx: &Transaction<'_>,
    session: &MasterSessionView,
    proof: &MasterBootstrapAuth,
    receipt: &Receipt,
) -> Result<WorktreeLease, EngineError> {
    if receipt.purpose != PURPOSE
        || receipt.project_id != session.project_id
        || receipt.session_id != session.session_id
        || receipt.bootstrap_task_id != session.bootstrap_task_id
        || receipt.config_id != session.config_id
        || receipt.source_hash != session.source_hash
        || receipt.owner != session.owner
        || receipt.proof != *proof
        || receipt.at_revision.value() < session.owner.revision_after.value()
        || receipt.at_revision.value() > owner_revision(tx)?
        || receipt.lease.state != LeaseState::Reserved
        || !receipt
            .allocation
            .as_str()
            .strip_prefix("master-workspace-")
            .is_some_and(is_hex_id)
    {
        return Err(error("master_bootstrap_receipt_conflict"));
    }
    declared_repository(tx, session, &receipt.lease.repository)?;
    if admission_hash(tx, session, &receipt.lease.repository)?.as_ref()
        != Some(&receipt.admission_sha256)
    {
        return Err(error("master_bootstrap_receipt_conflict"));
    }
    let mut expected = receipt.lease.clone();
    let actual = only_lease(tx, session)?;
    expected.state = actual.state.clone();
    if expected != actual {
        return Err(error("master_bootstrap_receipt_conflict"));
    }
    assert!(receipt.proof == *proof);
    assert_eq!(expected, actual);
    Ok(actual)
}

pub(super) fn reserve(
    tx: &Transaction<'_>,
    runtime_dir: &Path,
    session: &MasterSessionView,
    repository: &str,
    proof: impl Into<MasterBootstrapAuth>,
) -> Result<RequestId, EngineError> {
    assert!(
        !tx.is_autocommit(),
        "bootstrap reservation requires an active transaction"
    );
    let proof = proof.into();
    super::master_session::validate_bootstrap_auth(tx, session, &proof)?;
    current_session(tx, session)?;
    declared_repository(tx, session, repository)?;
    if load_receipt(tx, session)?.is_some() {
        return Err(error("master_bootstrap_receipt_conflict"));
    }
    let count: i64 = tx
        .query_row("SELECT COUNT(*) FROM bootstrap_workspaces", [], |row| {
            row.get(0)
        })
        .map_err(error)?;
    if count >= RECEIPTS_MAX {
        return Err(error("master_bootstrap_capacity"));
    }
    if admission_hash(tx, session, repository)?.is_none() {
        require_applied(worktrees::admit(tx, &session.project_id, repository)?)?;
    }
    require_applied(worktrees::reserve_master_bootstrap(
        tx,
        runtime_dir,
        &session.project_id,
        &session.bootstrap_task_id,
        &session.session_id,
        repository,
    )?)?;
    let lease = only_lease(tx, session)?;
    if lease.state != LeaseState::Reserved || lease.repository != repository {
        return Err(error("master_bootstrap_receipt_conflict"));
    }
    let receipt = Receipt {
        allocation: RequestId::new(format!("master-workspace-{}", random_id()?)).map_err(error)?,
        purpose: PURPOSE.into(),
        project_id: session.project_id.clone(),
        session_id: session.session_id.clone(),
        bootstrap_task_id: session.bootstrap_task_id.clone(),
        config_id: session.config_id.clone(),
        source_hash: session.source_hash.clone(),
        owner: session.owner.clone(),
        at_revision: Cursor::new(owner_revision(tx)?),
        proof,
        admission_sha256: admission_hash(tx, session, repository)?
            .ok_or_else(|| error("master_bootstrap_receipt_conflict"))?,
        lease,
    };
    persist_receipt(tx, session, &receipt)?;
    assert!(!tx.is_autocommit());
    assert_eq!(receipt.lease.state, LeaseState::Reserved);
    Ok(receipt.allocation)
}

fn require_applied(outcome: Outcome) -> Result<(), EngineError> {
    match outcome {
        Outcome::Applied => Ok(()),
        Outcome::Rejected { code } => Err(error(code)),
        _ => Err(error("master_bootstrap_receipt_conflict")),
    }
}

fn persist_receipt(
    tx: &Transaction<'_>,
    session: &MasterSessionView,
    receipt: &Receipt,
) -> Result<(), EngineError> {
    let body = serde_json::to_string(receipt).map_err(error)?;
    if body.len() > RECEIPT_BYTES_MAX {
        return Err(error("master_bootstrap_receipt_bounds"));
    }
    let lease = validate_receipt(tx, session, &receipt.proof, receipt)?;
    assert_eq!(lease.state, LeaseState::Reserved);
    let inserted = tx
        .execute(
            "INSERT INTO bootstrap_workspaces(session,allocation,body) VALUES (?1,?2,?3)",
            params![
                session.session_id.as_str(),
                receipt.allocation.as_str(),
                body
            ],
        )
        .map_err(error)?;
    assert_eq!(inserted, 1);
    assert!(load_receipt(tx, session)?.as_ref() == Some(receipt));
    Ok(())
}

pub(super) fn ready(
    tx: &Transaction<'_>,
    session: &MasterSessionView,
    proof: impl Into<MasterBootstrapAuth>,
) -> Result<(RequestId, WorktreeLease), EngineError> {
    assert!(
        !tx.is_autocommit(),
        "bootstrap receipt read requires an active transaction"
    );
    let proof = proof.into();
    super::master_session::validate_bootstrap_auth(tx, session, &proof)?;
    current_session(tx, session)?;
    let receipt =
        load_receipt(tx, session)?.ok_or_else(|| error("master_bootstrap_receipt_missing"))?;
    let lease = validate_receipt(tx, session, &proof, &receipt)?;
    match &lease.state {
        LeaseState::Reserved => return Err(error("master_bootstrap_reserved")),
        LeaseState::Uncertain => return Err(error("master_bootstrap_uncertain")),
        LeaseState::RecoveryRequired => return Err(error("master_bootstrap_recovery_required")),
        LeaseState::Ready => {}
    }
    assert_eq!(lease.state, LeaseState::Ready);
    assert_eq!(lease.owner_session, session.session_id);
    Ok((receipt.allocation, lease))
}

#[cfg(test)]
mod tests;
