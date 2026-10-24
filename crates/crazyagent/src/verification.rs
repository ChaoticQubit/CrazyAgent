use crate::config::EffectiveConfig;
use crate::engine::EngineError;
use crate::protocol::*;
#[cfg(unix)]
use rusqlite::Connection;
use rusqlite::{OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerificationError(pub String);

impl std::fmt::Display for VerificationError {
    /// Formats the bounded verification failure code.
    /// Takes a formatter and returns its write result.
    /// Does not include protected check material.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for VerificationError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Risk {
    Low,
    Medium,
    High,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CheckKind {
    Code,
    TestIntegrity,
    Holdout,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckSpec {
    pub id: RequestId,
    pub kind: CheckKind,
    pub minimum_risk: Risk,
    pub content: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RiskRule {
    pub path_prefix: String,
    pub minimum: Risk,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum BaselineOrigin {
    Generated,
    HumanAuthored,
    Imported,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BaselineDraft {
    pub version: u16,
    pub project_id: ProjectId,
    pub origin: BaselineOrigin,
    pub requirements: String,
    pub checks: Vec<CheckSpec>,
    pub rules: Vec<RiskRule>,
    pub retry_limit: u8,
}

impl BaselineDraft {
    /// Imports bounded version-one JSON as untrusted material.
    /// Takes bytes and returns a validated draft, never approval.
    /// Rejects unknown fields and unsupported retry authority.
    pub fn import(bytes: &[u8]) -> Result<Self, VerificationError> {
        if bytes.len() > 32768 {
            return Err(failure("baseline_bounds"));
        }
        let draft: Self = serde_json::from_slice(bytes).map_err(|_| failure("baseline_json"))?;
        draft.validate()?;
        assert!(bytes.len() <= 32768);
        assert_eq!(draft.version, 1);
        Ok(draft)
    }

    /// Validates draft shape and mandatory verification floors.
    /// Takes untrusted baseline content and returns validation status.
    /// Approval and execution remain separate authorities.
    pub fn validate(&self) -> Result<(), VerificationError> {
        if self.version != 1
            || self.requirements.trim().is_empty()
            || self.requirements.len() > 4096
            || !(2..=32).contains(&self.checks.len())
            || self.rules.len() > 32
            || self.retry_limit != 0
            || self
                .checks
                .iter()
                .any(|c| c.content.is_empty() || c.content.len() > 4096)
            || self
                .checks
                .iter()
                .map(|c| &c.id)
                .collect::<BTreeSet<_>>()
                .len()
                != self.checks.len()
            || !self
                .checks
                .iter()
                .any(|c| c.kind == CheckKind::Code && c.minimum_risk == Risk::Low)
            || !self
                .checks
                .iter()
                .any(|c| c.kind == CheckKind::TestIntegrity && c.minimum_risk == Risk::Low)
            || self.rules.iter().any(|r| !safe_path(&r.path_prefix))
            || serde_json::to_vec(self)
                .map_err(|_| failure("baseline_json"))?
                .len()
                > 32768
        {
            return Err(failure("baseline_bounds_or_floor"));
        }
        assert!(self.checks.len() >= 2);
        assert_eq!(self.retry_limit, 0);
        Ok(())
    }

    /// Identifies the exact draft version by validated content.
    /// Takes draft content and returns its SHA-256 baseline ID.
    /// A matching identity does not establish human admission.
    pub fn identity(&self) -> Result<BaselineId, VerificationError> {
        self.validate()?;
        BaselineId::new(digest(
            &serde_json::to_vec(self).map_err(|_| failure("baseline_json"))?,
        ))
        .map_err(|_| failure("baseline_identity"))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CandidateSnapshot {
    pub files: BTreeMap<String, Vec<u8>>,
    pub head_digest: String,
    pub target_digest: String,
}

impl CandidateSnapshot {
    /// Validates bounded immutable source bytes and revision identities.
    /// Takes a snapshot manifest and returns its content identity.
    /// No paths are opened and no candidate code is executed.
    pub fn identity(&self) -> Result<SnapshotId, VerificationError> {
        if self.files.is_empty()
            || self.files.len() > 32
            || self.files.keys().any(|p| !safe_path(p))
            || self
                .files
                .values()
                .try_fold(0usize, |n, b| n.checked_add(b.len()))
                .is_none_or(|n| n > 16384)
            || !is_digest(&self.head_digest)
            || !is_digest(&self.target_digest)
        {
            return Err(failure("snapshot_bounds_or_identity"));
        }
        for path in self.files.keys() {
            if self
                .files
                .keys()
                .any(|other| other.starts_with(&format!("{path}/")))
            {
                return Err(failure("snapshot_path_conflict"));
            }
        }
        let bytes = serde_json::to_vec(self).map_err(|_| failure("snapshot_json"))?;
        if bytes.len() > 16384 {
            return Err(failure("snapshot_serialized_bounds"));
        }
        let id = SnapshotId::new(digest(&bytes)).map_err(|_| failure("snapshot_identity"))?;
        assert_eq!(id.as_str().len(), 64);
        assert!(self.files.len() <= 32);
        Ok(id)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RiskAssessment {
    pub deterministic_floor: Risk,
    pub raised_risk: Risk,
    pub reasons: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerifyRequest {
    pub snapshot_id: SnapshotId,
    pub baseline_id: BaselineId,
    pub config_id: ConfigId,
    pub environment_id: EnvironmentId,
    pub source_digest: String,
    pub head_digest: String,
    pub target_digest: String,
    pub checks: BTreeMap<RequestId, String>,
}

impl VerifyRequest {
    /// Validates explicit identities and the nonempty required check set.
    /// Takes a request and returns bounded structural validation.
    /// This does not authenticate measured environment or runner state.
    pub fn validate(&self) -> Result<(), VerificationError> {
        if !is_digest(self.snapshot_id.as_str())
            || !is_digest(self.baseline_id.as_str())
            || !is_digest(self.config_id.as_str())
            || !is_digest(&self.source_digest)
            || !is_digest(&self.head_digest)
            || !is_digest(&self.target_digest)
            || !(2..=32).contains(&self.checks.len())
            || self.checks.values().any(|h| !is_digest(h))
        {
            return Err(failure("request_identity_or_checks"));
        }
        assert!(self.checks.len() >= 2);
        assert!(self.checks.len() <= 32);
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CheckOutcome {
    Passed,
    Failed,
    Interrupted,
    TimedOut,
    Unknown,
    Unavailable,
    Conflicting,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerificationReceipt {
    pub request: VerifyRequest,
    pub outcomes: BTreeMap<RequestId, CheckOutcome>,
    pub artifacts: BTreeMap<String, Vec<u8>>,
    pub artifact_digests: BTreeMap<String, String>,
    pub completed_at: u64,
    #[serde(skip)]
    execution_proven: bool,
}

impl VerificationReceipt {
    /// Checks current inputs, artifact content, outcomes and provenance.
    /// Takes the current required request and returns acceptance status.
    /// Deserialized Passed data never establishes execution proof.
    pub fn accepts(&self, current: &VerifyRequest) -> Result<(), VerificationError> {
        current.validate()?;
        if &self.request != current {
            return Err(failure("stale_verification_inputs"));
        }
        if self.artifacts.is_empty()
            || self.artifacts.len() > 32
            || self.artifacts.len() != self.artifact_digests.len()
            || self
                .artifacts
                .values()
                .try_fold(0usize, |n, b| n.checked_add(b.len()))
                .is_none_or(|n| n > 16384)
            || self
                .artifacts
                .iter()
                .any(|(p, b)| !safe_path(p) || self.artifact_digests.get(p) != Some(&digest(b)))
        {
            return Err(failure("missing_or_tampered_artifact"));
        }
        if self.outcomes.len() != current.checks.len()
            || current
                .checks
                .keys()
                .any(|id| self.outcomes.get(id) != Some(&CheckOutcome::Passed))
        {
            return Err(failure("required_evidence_not_passed"));
        }
        if !self.execution_proven || self.completed_at == 0 {
            return Err(failure("unproven_execution"));
        }
        assert_eq!(self.outcomes.len(), current.checks.len());
        assert!(self.execution_proven);
        Ok(())
    }
}

/// Rejects verification execution until its runtime is qualified.
/// Takes a structurally valid request and returns unavailable.
/// No child process, retry or trusted success receipt is created.
pub fn verify(request: &VerifyRequest) -> Result<VerificationReceipt, VerificationError> {
    request.validate()?;
    Err(failure("runtime_unavailable"))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Candidate {
    pub id: RequestId,
    pub task_id: TaskId,
    pub snapshot: CandidateSnapshot,
    pub snapshot_id: SnapshotId,
    pub environment_id: EnvironmentId,
    pub pinned_baseline_id: BaselineId,
    pub config_id: ConfigId,
    pub escalation: Risk,
    pub full_suite: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "operation", deny_unknown_fields)]
pub enum OwnerVerification {
    Approve {
        baseline_id: BaselineId,
    },
    Revoke {
        baseline_id: BaselineId,
    },
    Register {
        id: RequestId,
        task_id: TaskId,
        snapshot: CandidateSnapshot,
        environment_id: EnvironmentId,
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
    Accept {
        candidate_id: RequestId,
        attempt_id: RequestId,
    },
    Inspect {
        candidate_id: RequestId,
    },
    History {
        candidate_id: RequestId,
        after: RequestId,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerificationState {
    pub candidate: Candidate,
    pub current: VerifyRequest,
    pub risk: RiskAssessment,
    pub attempts: Vec<(RequestId, VerificationReceipt)>,
    pub attempts_total: u16,
    pub attempts_next: Option<RequestId>,
}

/// Produces a bounded verification error without protected contents.
/// Takes a static code and returns a typed failure.
/// Callers keep engine and wire errors scoped to this code.
fn failure(code: &str) -> VerificationError {
    VerificationError(code.into())
}

/// Hashes exact bytes using the existing SHA-256 implementation.
/// Takes bounded content and returns lowercase hexadecimal identity.
/// This establishes identity, never execution or authorship.
fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// Recognizes canonical SHA-256 content identifiers.
/// Takes untrusted text and returns whether it is lowercase hex.
/// Rejects truncated, oversized and noncanonical identities.
fn is_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Checks portable relative paths without filesystem traversal.
/// Takes an untrusted path and returns whether it is admissible.
/// Empty, special, absolute and oversized components are rejected.
fn safe_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 256
        && path.split('/').all(|part| {
            !part.is_empty()
                && part != "."
                && part != ".."
                && part
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
        })
}

/// Creates additive authority tables inside the existing storage actor.
/// Takes an engine connection and returns schema initialization status.
/// Existing tables and records remain unchanged.
#[cfg(unix)]
pub(crate) fn initialize(connection: &Connection) -> Result<(), EngineError> {
    connection.execute_batch("CREATE TABLE IF NOT EXISTS verification_baselines
        (id TEXT PRIMARY KEY, project TEXT NOT NULL, body TEXT NOT NULL,
         approved INTEGER NOT NULL DEFAULT 0, revoked INTEGER NOT NULL DEFAULT 0);
        CREATE TABLE IF NOT EXISTS verification_current
        (project TEXT PRIMARY KEY, baseline TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS verification_candidates (id TEXT PRIMARY KEY, body TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS verification_attempts
        (id TEXT PRIMARY KEY, candidate TEXT NOT NULL, body TEXT NOT NULL);
        CREATE INDEX IF NOT EXISTS verification_attempt_candidate ON verification_attempts(candidate,id);").map_err(db_error)?;
    assert!(connection.is_autocommit());
    Ok(())
}

/// Maps storage or validation errors into the engine error channel.
/// Takes a displayable error and returns its engine representation.
/// Protected source contents are never added by this conversion.
fn db_error(error: impl std::fmt::Display) -> EngineError {
    EngineError(error.to_string())
}

/// Classifies authority commands for revision and replay enforcement.
/// Takes a command and returns whether it mutates protected state.
/// Inspection and acceptance always reevaluate live authority.
pub(crate) fn is_mutation(command: &Command) -> bool {
    matches!(command, Command::ProposeBaseline { .. })
        || matches!(command, Command::OwnerVerification { action }
            if !matches!(action, OwnerVerification::Inspect { .. } | OwnerVerification::History { .. } | OwnerVerification::Accept { .. }))
}

/// Executes baseline drafting or trusted host authority commands.
/// Takes an actor transaction, live config and command.
/// Returns an outcome without trusting deserialized execution claims.
pub(crate) fn execute(
    tx: &Transaction<'_>,
    config: &EffectiveConfig,
    command: &Command,
) -> Result<Outcome, EngineError> {
    match command {
        Command::ProposeBaseline { draft } => {
            let id = draft.identity().map_err(db_error)?;
            let project: bool = tx
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM projects WHERE id=?1)",
                    [draft.project_id.as_str()],
                    |r| r.get(0),
                )
                .map_err(db_error)?;
            if !project {
                return rejected("project_missing");
            }
            let exists: bool = tx
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM verification_baselines WHERE id=?1)",
                    [id.as_str()],
                    |r| r.get(0),
                )
                .map_err(db_error)?;
            if exists {
                let (stored, _, _) = baseline(tx, &id)?;
                if stored != *draft {
                    return Err(db_error("baseline_tampered"));
                }
                return Ok(Outcome::Applied);
            }
            capacity(tx, "verification_baselines")?;
            tx.execute(
                "INSERT OR IGNORE INTO verification_baselines(id,project,body) VALUES (?1,?2,?3)",
                params![
                    id.as_str(),
                    draft.project_id.as_str(),
                    serde_json::to_string(draft).map_err(db_error)?
                ],
            )
            .map_err(db_error)?;
            Ok(Outcome::Applied)
        }
        Command::OwnerVerification { action } => owner(tx, config, action),
        _ => Err(db_error("unsupported_verification_command")),
    }
}

/// Bounds persistent authority records before inserting new material.
/// Takes a transaction and an internal table name.
/// Returns an error at the fixed core storage ceiling.
fn capacity(tx: &Transaction<'_>, table: &str) -> Result<(), EngineError> {
    assert!(
        [
            "verification_baselines",
            "verification_candidates",
            "verification_attempts"
        ]
        .contains(&table)
    );
    let count: i64 = tx
        .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
        .map_err(db_error)?;
    if count >= 128 {
        return Err(db_error("verification_capacity"));
    }
    assert!(count < 128);
    Ok(())
}

/// Dispatches host-only baseline and candidate authority operations.
/// Takes live actor state and a typed host action.
/// Plan and merge approval are never changed here.
fn owner(
    tx: &Transaction<'_>,
    config: &EffectiveConfig,
    action: &OwnerVerification,
) -> Result<Outcome, EngineError> {
    match action {
        OwnerVerification::Approve { baseline_id } => approve(tx, baseline_id),
        OwnerVerification::Revoke { baseline_id } => {
            let changed = tx
                .execute(
                    "UPDATE verification_baselines SET revoked=1 WHERE id=?1",
                    [baseline_id.as_str()],
                )
                .map_err(db_error)?;
            if changed == 0 {
                return rejected("baseline_missing");
            }
            assert_eq!(changed, 1);
            Ok(Outcome::Applied)
        }
        OwnerVerification::Register {
            id,
            task_id,
            snapshot,
            environment_id,
        } => register(tx, config, id, task_id, snapshot, environment_id),
        OwnerVerification::Escalate { candidate_id, risk } => {
            let mut candidate = load_candidate(tx, candidate_id)?;
            candidate.escalation = candidate.escalation.max(*risk);
            save_candidate(tx, &candidate)?;
            Ok(Outcome::Applied)
        }
        OwnerVerification::FullSuite { candidate_id } => {
            let mut candidate = load_candidate(tx, candidate_id)?;
            candidate.full_suite = true;
            save_candidate(tx, &candidate)?;
            Ok(Outcome::Applied)
        }
        OwnerVerification::Verify {
            candidate_id,
            attempt_id,
        } => record_attempt(tx, config, candidate_id, attempt_id),
        OwnerVerification::Inspect { candidate_id } => Ok(Outcome::Verification {
            state: Box::new(state(tx, config, candidate_id, None)?),
        }),
        OwnerVerification::History {
            candidate_id,
            after,
        } => Ok(Outcome::Verification {
            state: Box::new(state(tx, config, candidate_id, Some(after))?),
        }),
        OwnerVerification::Accept {
            candidate_id,
            attempt_id,
        } => accept(tx, config, candidate_id, attempt_id),
    }
}

/// Creates a bounded rejected result without modifying authority.
/// Takes a stable failure code and returns the protocol outcome.
/// Protected content is excluded from feedback.
fn rejected(code: &str) -> Result<Outcome, EngineError> {
    Ok(Outcome::Rejected { code: code.into() })
}

/// Reads and validates a content-addressed baseline and its admission.
/// Takes a transaction and exact baseline ID.
/// Returns immutable content and separate approval/revocation flags.
fn baseline(
    tx: &Transaction<'_>,
    id: &BaselineId,
) -> Result<(BaselineDraft, bool, bool), EngineError> {
    let row: Option<(String, bool, bool)> = tx
        .query_row(
            "SELECT body,approved,revoked FROM verification_baselines WHERE id=?1",
            [id.as_str()],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .map_err(db_error)?;
    let (body, approved, revoked) = row.ok_or_else(|| db_error("baseline_missing"))?;
    let draft = BaselineDraft::import(body.as_bytes()).map_err(db_error)?;
    if draft.identity().map_err(db_error)? != *id {
        return Err(db_error("baseline_tampered"));
    }
    assert_eq!(draft.identity().map_err(db_error)?, *id);
    assert_eq!(draft.version, 1);
    Ok((draft, approved, revoked))
}

/// Admits one exact draft through host-only human authority.
/// Takes its content ID and atomically updates the current project contract.
/// Revoked versions cannot be silently readmitted.
fn approve(tx: &Transaction<'_>, id: &BaselineId) -> Result<Outcome, EngineError> {
    let exists: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM verification_baselines WHERE id=?1)",
            [id.as_str()],
            |r| r.get(0),
        )
        .map_err(db_error)?;
    if !exists {
        return rejected("baseline_missing");
    }
    let (draft, _, revoked) = baseline(tx, id)?;
    if revoked {
        return rejected("baseline_revoked");
    }
    let project: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM projects WHERE id=?1)",
            [draft.project_id.as_str()],
            |r| r.get(0),
        )
        .map_err(db_error)?;
    if !project {
        return rejected("project_missing");
    }
    tx.execute(
        "UPDATE verification_baselines SET approved=1 WHERE id=?1",
        [id.as_str()],
    )
    .map_err(db_error)?;
    tx.execute("INSERT INTO verification_current VALUES (?1,?2) ON CONFLICT(project) DO UPDATE SET baseline=excluded.baseline",
        params![draft.project_id.as_str(), id.as_str()]).map_err(db_error)?;
    assert!(baseline(tx, id)?.1);
    assert!(!tx.is_autocommit());
    Ok(Outcome::Applied)
}

/// Resolves the current admitted contract at an actor decision point.
/// Takes a project ID and returns a validated approved version.
/// Revocation blocks acceptance even for pinned running work.
fn current_baseline(
    tx: &Transaction<'_>,
    project: &ProjectId,
) -> Result<(BaselineId, BaselineDraft), EngineError> {
    let id: Option<String> = tx
        .query_row(
            "SELECT baseline FROM verification_current WHERE project=?1",
            [project.as_str()],
            |r| r.get(0),
        )
        .optional()
        .map_err(db_error)?;
    let id =
        BaselineId::new(id.ok_or_else(|| db_error("baseline_not_approved"))?).map_err(db_error)?;
    let (draft, approved, revoked) = baseline(tx, &id)?;
    if !approved || revoked || draft.project_id != *project {
        return Err(db_error("baseline_not_current_or_revoked"));
    }
    assert_eq!(draft.project_id, *project);
    assert!(!revoked);
    Ok((id, draft))
}

/// Registers immutable candidate bytes against a pinned task contract.
/// Takes host-captured source and environment identity plus task context.
/// Existing candidate IDs cannot replace source or targets.
fn register(
    tx: &Transaction<'_>,
    config: &EffectiveConfig,
    id: &RequestId,
    task: &TaskId,
    snapshot: &CandidateSnapshot,
    environment: &EnvironmentId,
) -> Result<Outcome, EngineError> {
    let context =
        crate::storage::task_context(tx, task)?.ok_or_else(|| db_error("task_missing"))?;
    let (pinned, approved, revoked) = baseline(tx, &context.baseline_id)?;
    if !approved
        || revoked
        || pinned.project_id != context.project_id
        || context.config_id != config.id
    {
        return rejected("candidate_not_authorized");
    }
    let (current, _) = current_baseline(tx, &context.project_id)?;
    if current != context.baseline_id {
        return rejected("task_contract_upgrade_required");
    }
    let candidate = Candidate {
        id: id.clone(),
        task_id: task.clone(),
        snapshot: snapshot.clone(),
        snapshot_id: snapshot.identity().map_err(db_error)?,
        environment_id: environment.clone(),
        pinned_baseline_id: context.baseline_id,
        config_id: context.config_id,
        escalation: Risk::Low,
        full_suite: false,
    };
    capacity(tx, "verification_candidates")?;
    let changed = tx
        .execute(
            "INSERT OR IGNORE INTO verification_candidates VALUES (?1,?2)",
            params![
                id.as_str(),
                serde_json::to_string(&candidate).map_err(db_error)?
            ],
        )
        .map_err(db_error)?;
    if changed == 0 {
        return rejected("candidate_exists");
    }
    assert_eq!(changed, 1);
    assert_eq!(load_candidate(tx, id)?, candidate);
    Ok(Outcome::Applied)
}

/// Loads candidate content and checks its immutable snapshot binding.
/// Takes a transaction and candidate ID and returns validated state.
/// Tampered stored source never reaches selection or acceptance.
fn load_candidate(tx: &Transaction<'_>, id: &RequestId) -> Result<Candidate, EngineError> {
    let body: Option<String> = tx
        .query_row(
            "SELECT body FROM verification_candidates WHERE id=?1",
            [id.as_str()],
            |r| r.get(0),
        )
        .optional()
        .map_err(db_error)?;
    let candidate: Candidate =
        serde_json::from_str(&body.ok_or_else(|| db_error("candidate_missing"))?)
            .map_err(db_error)?;
    if candidate.id != *id
        || candidate.snapshot.identity().map_err(db_error)? != candidate.snapshot_id
    {
        return Err(db_error("candidate_tampered"));
    }
    assert_eq!(candidate.id, *id);
    assert_eq!(
        candidate.snapshot.identity().map_err(db_error)?,
        candidate.snapshot_id
    );
    Ok(candidate)
}

/// Persists monotone host risk or full-suite updates.
/// Takes a validated candidate and updates only that record.
/// Immutable snapshot bytes remain bound to their original ID.
fn save_candidate(tx: &Transaction<'_>, candidate: &Candidate) -> Result<(), EngineError> {
    assert_eq!(
        candidate.snapshot.identity().map_err(db_error)?,
        candidate.snapshot_id
    );
    let changed = tx
        .execute(
            "UPDATE verification_candidates SET body=?1 WHERE id=?2",
            params![
                serde_json::to_string(candidate).map_err(db_error)?,
                candidate.id.as_str()
            ],
        )
        .map_err(db_error)?;
    assert_eq!(changed, 1);
    Ok(())
}

/// Computes deterministic floors and independently raised check depth.
/// Takes approved baseline rules and immutable candidate state.
/// Unknown classifications and full-suite requests select all checks.
fn selection(
    draft: &BaselineDraft,
    candidate: &Candidate,
) -> (RiskAssessment, BTreeMap<RequestId, String>) {
    let mut floor = Risk::Low;
    let mut reasons = vec![];
    for rule in &draft.rules {
        if candidate
            .snapshot
            .files
            .keys()
            .any(|p| p == &rule.path_prefix || p.starts_with(&format!("{}/", rule.path_prefix)))
        {
            floor = floor.max(rule.minimum);
            reasons.push(format!("rule:{}", rule.path_prefix));
        }
    }
    let raised = floor.max(candidate.escalation);
    let checks = draft
        .checks
        .iter()
        .filter(|c| candidate.full_suite || raised == Risk::Unknown || c.minimum_risk <= raised)
        .map(|c| (c.id.clone(), digest(&serde_json::to_vec(c).unwrap())))
        .collect::<BTreeMap<_, _>>();
    assert!(checks.len() >= 2);
    assert!(raised >= floor);
    (
        RiskAssessment {
            deterministic_floor: floor,
            raised_risk: raised,
            reasons,
        },
        checks,
    )
}

/// Recomputes live requirements and loads retained bounded attempts.
/// Takes current config and candidate identity inside one transaction.
/// Pinned task context remains historical rather than overwritten.
fn state(
    tx: &Transaction<'_>,
    config: &EffectiveConfig,
    id: &RequestId,
    after: Option<&RequestId>,
) -> Result<VerificationState, EngineError> {
    let candidate = load_candidate(tx, id)?;
    let context = crate::storage::task_context(tx, &candidate.task_id)?
        .ok_or_else(|| db_error("task_missing"))?;
    let (_, approved, revoked) = baseline(tx, &candidate.pinned_baseline_id)?;
    if !approved
        || revoked
        || context
            .allowed_destinations
            .iter()
            .any(|d| config.settings.revoked_destinations.contains(d))
    {
        return Err(db_error("candidate_authority_revoked"));
    }
    let (baseline_id, draft) = current_baseline(tx, &context.project_id)?;
    let (risk, checks) = selection(&draft, &candidate);
    let current = VerifyRequest {
        snapshot_id: candidate.snapshot_id.clone(),
        baseline_id,
        config_id: config.id.clone(),
        environment_id: candidate.environment_id.clone(),
        source_digest: digest(&serde_json::to_vec(&candidate.snapshot.files).map_err(db_error)?),
        head_digest: candidate.snapshot.head_digest.clone(),
        target_digest: candidate.snapshot.target_digest.clone(),
        checks,
    };
    current.validate().map_err(db_error)?;
    let (attempts, attempts_total, attempts_next) = attempt_page(tx, id, after)?;
    assert_eq!(current.config_id, config.id);
    assert_eq!(current.snapshot_id, candidate.snapshot_id);
    Ok(VerificationState {
        candidate,
        current,
        risk,
        attempts,
        attempts_total,
        attempts_next,
    })
}

type AttemptPage = (
    Vec<(RequestId, VerificationReceipt)>,
    u16,
    Option<RequestId>,
);

/// Reads one bounded page without deleting historical attempts.
/// Takes a candidate ID and optional exclusive lexical cursor.
/// Returns two receipts, a total count and an optional next cursor.
fn attempt_page(
    tx: &Transaction<'_>,
    id: &RequestId,
    after: Option<&RequestId>,
) -> Result<AttemptPage, EngineError> {
    let mut statement = tx
        .prepare(
            "SELECT id,body FROM verification_attempts WHERE candidate=?1 AND id>?2 ORDER BY id LIMIT 3",
        )
        .map_err(db_error)?;
    let rows = statement
        .query_map([id.as_str(), after.map_or("", RequestId::as_str)], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })
        .map_err(db_error)?;
    let mut attempts = vec![];
    for row in rows {
        let (attempt, body) = row.map_err(db_error)?;
        attempts.push((
            RequestId::new(attempt).map_err(db_error)?,
            serde_json::from_str(&body).map_err(db_error)?,
        ));
    }
    let more = attempts.len() > 2;
    attempts.truncate(2);
    let attempts_next = if more {
        attempts.last().map(|(id, _)| id.clone())
    } else {
        None
    };
    let attempts_total = tx
        .query_row(
            "SELECT COUNT(*) FROM verification_attempts WHERE candidate=?1",
            [id.as_str()],
            |r| r.get(0),
        )
        .map_err(db_error)?;
    assert!(attempts.len() <= 2);
    assert!(attempts_total <= 128);
    Ok((attempts, attempts_total, attempts_next))
}

/// Retains an actual unavailable execution attempt without spawning code.
/// Takes an immutable candidate and unique host attempt identity.
/// No success claim or automatic retry can replace this evidence.
fn record_attempt(
    tx: &Transaction<'_>,
    config: &EffectiveConfig,
    candidate: &RequestId,
    attempt: &RequestId,
) -> Result<Outcome, EngineError> {
    let state = state(tx, config, candidate, None)?;
    let unavailable =
        verify(&state.current).expect_err("unqualified runtime must remain unavailable");
    assert_eq!(unavailable.0, "runtime_unavailable");
    let completed_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(db_error)?
        .as_secs();
    let artifacts: BTreeMap<String, Vec<u8>> =
        BTreeMap::from([("runtime-status.txt".into(), unavailable.0.into_bytes())]);
    let receipt = VerificationReceipt {
        outcomes: state
            .current
            .checks
            .keys()
            .map(|id| (id.clone(), CheckOutcome::Unavailable))
            .collect(),
        request: state.current,
        artifact_digests: artifacts
            .iter()
            .map(|(p, b)| (p.clone(), digest(b)))
            .collect(),
        artifacts,
        completed_at,
        execution_proven: false,
    };
    capacity(tx, "verification_attempts")?;
    let changed = tx
        .execute(
            "INSERT OR IGNORE INTO verification_attempts VALUES (?1,?2,?3)",
            params![
                attempt.as_str(),
                candidate.as_str(),
                serde_json::to_string(&receipt).map_err(db_error)?
            ],
        )
        .map_err(db_error)?;
    if changed == 0 {
        return rejected("attempt_exists");
    }
    assert_eq!(changed, 1);
    assert!(!receipt.execution_proven);
    Ok(Outcome::Applied)
}

/// Evaluates stored evidence under current authority without mutation.
/// Takes candidate and attempt IDs and returns fail-closed eligibility.
/// Read replay always reevaluates revocation and current requirements.
fn accept(
    tx: &Transaction<'_>,
    config: &EffectiveConfig,
    candidate: &RequestId,
    attempt: &RequestId,
) -> Result<Outcome, EngineError> {
    let state = match state(tx, config, candidate, None) {
        Ok(state) => state,
        Err(error) => return rejected(&error.0),
    };
    let body: Option<String> = tx
        .query_row(
            "SELECT body FROM verification_attempts WHERE candidate=?1 AND id=?2",
            params![candidate.as_str(), attempt.as_str()],
            |r| r.get(0),
        )
        .optional()
        .map_err(db_error)?;
    let Some(body) = body else {
        return rejected("attempt_missing");
    };
    let receipt: VerificationReceipt = serde_json::from_str(&body).map_err(db_error)?;
    match receipt.accepts(&state.current) {
        Ok(()) => rejected("runtime_unavailable"),
        Err(error) => rejected(&error.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn candidate() -> Candidate {
        let snapshot = CandidateSnapshot {
            files: BTreeMap::from([("src/main.rs".into(), b"fn main() {}".to_vec())]),
            head_digest: "a".repeat(64),
            target_digest: "b".repeat(64),
        };
        Candidate {
            id: RequestId::new("candidate").unwrap(),
            task_id: TaskId::new("task").unwrap(),
            snapshot_id: snapshot.identity().unwrap(),
            snapshot,
            environment_id: EnvironmentId::new("fixture").unwrap(),
            pinned_baseline_id: BaselineId::new("c".repeat(64)).unwrap(),
            config_id: ConfigId::new("d".repeat(64)).unwrap(),
            escalation: Risk::Low,
            full_suite: false,
        }
    }

    #[test]
    fn ai_cannot_lower_risk_floor() {
        let baseline = BaselineDraft {
            version: 1,
            project_id: ProjectId::new("project").unwrap(),
            origin: BaselineOrigin::HumanAuthored,
            requirements: "Preserve required behavior".into(),
            checks: vec![
                CheckSpec {
                    id: RequestId::new("code").unwrap(),
                    kind: CheckKind::Code,
                    minimum_risk: Risk::Low,
                    content: b"code check".to_vec(),
                },
                CheckSpec {
                    id: RequestId::new("integrity").unwrap(),
                    kind: CheckKind::TestIntegrity,
                    minimum_risk: Risk::Low,
                    content: b"integrity check".to_vec(),
                },
            ],
            rules: vec![RiskRule {
                path_prefix: "src".into(),
                minimum: Risk::High,
            }],
            retry_limit: 0,
        };
        baseline.validate().unwrap();
        let (assessment, checks) = selection(&baseline, &candidate());
        assert_eq!(assessment.deterministic_floor, Risk::High);
        assert_eq!(assessment.raised_risk, Risk::High);
        assert_eq!(checks.len(), 2);
    }

    #[test]
    #[should_panic(expected = "assertion `left == right` failed")]
    fn internal_candidate_update_asserts_immutable_snapshot_identity() {
        let root = tempfile::tempdir().unwrap();
        let mut connection = Connection::open(root.path().join("db")).unwrap();
        let transaction = connection.transaction().unwrap();
        let mut candidate = candidate();
        candidate
            .snapshot
            .files
            .values_mut()
            .next()
            .unwrap()
            .push(0);
        save_candidate(&transaction, &candidate).unwrap();
    }
}
