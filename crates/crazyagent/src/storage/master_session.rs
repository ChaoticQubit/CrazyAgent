use super::{
    check_owner_admission, check_owner_host, native_profile_connection, owner_lookup, owner_record,
    owner_revision, persist_event_ids, random_id, settings,
};
use crate::config::{EffectiveConfig, ModelAuthentication, ModelConnection, ModelEndpoint};
use crate::conversation::ConversationMutation;
use crate::coordination::Role;
use crate::engine::bootstrap::{NativeProfileUse, NativeProfileUsePurpose};
use crate::engine::owner::account_profile::{AccountProfileReceipt, AccountProfileRequest};
use crate::engine::owner::{
    Admission, HostStamp, NativeAuthAction, NativeAuthPhase, NativeAuthStart, NativeProfileBinding,
    OwnerApplied, OwnerData, OwnerMutation, OwnerOperation, OwnerProvenance, OwnerRead,
    OwnerReadReply, OwnerRequest, OwnerResult, PreparedNativeProfile, is_hex_id,
};
use crate::engine::{EngineError, NativeProfileCustody};
use crate::master_session::{
    METADATA_BYTES_MAX, MasterAccountState, MasterBackend, MasterBootstrapAuth, MasterCursor,
    MasterOwnerAction, MasterReason, MasterReply, MasterRuntimeView, MasterSessionPhase,
    MasterSessionView, MasterTurnPage, MasterTurnPhase, MasterTurnView, PAGE_BYTES_MAX,
    RcliConfiguredProfileProof, RcliProfileRegistration, RcliProfileRegistrationMetadata,
    SESSIONS_MAX, TURNS_MAX, cause_code, error, validate_action, validate_page, validate_selection,
};
use crate::model_gateway::{
    self, Attempt, AttemptCompletion, AttemptOutcome, AttemptRequest,
    ConfiguredInitialConversationAuth, Effects, FailureKind, InitialConversationAuth,
};
use crate::protocol::{
    ClientId, ConfigId, Cursor, Outcome, PROTOCOL_VERSION, ProjectId, RequestId, SessionId,
    StoredProject, TaskId,
};
use crate::worktrees::{LeaseState, WorktreeLease};
use rusqlite::{Connection, OptionalExtension, Row, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
#[cfg(unix)]
use std::collections::BTreeMap;
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// The device and inode that name the engine database file.
/// Takes its metadata. Returns the pair, or `master_configured_database_unavailable` where
/// the platform has no stable file identity in the standard library.
#[cfg(unix)]
fn database_identity(metadata: &std::fs::Metadata) -> Result<(u64, u64), EngineError> {
    Ok((metadata.dev(), metadata.ino()))
}

/// The device and inode that name the engine database file.
/// Takes its metadata. Returns `master_configured_database_unavailable`, because the
/// standard library has no stable file identity here.
#[cfg(not(unix))]
fn database_identity(_metadata: &std::fs::Metadata) -> Result<(u64, u64), EngineError> {
    Err(error("master_configured_database_unavailable"))
}

pub(crate) enum MasterRuntimeCommand {
    #[cfg(all(test, unix))]
    RegisterRcliProfile {
        host: HostStamp,
        project_id: ProjectId,
        session_id: SessionId,
        configured_home: PathBuf,
    },
    Route {
        host: HostStamp,
        project_id: ProjectId,
        session_id: SessionId,
        turn_id: RequestId,
    },
    Prepare {
        host: HostStamp,
        project_id: ProjectId,
        session_id: SessionId,
        turn_id: RequestId,
    },
    Inspect {
        host: HostStamp,
        project_id: ProjectId,
        session_id: SessionId,
        turn_id: RequestId,
    },
    AcquireProfile {
        host: HostStamp,
        project_id: ProjectId,
        session_id: SessionId,
        turn_id: RequestId,
        attempt_id: RequestId,
    },
    Observe {
        host: HostStamp,
        project_id: ProjectId,
        session_id: SessionId,
        turn_id: RequestId,
        attempt_id: RequestId,
        observation: MasterRuntimeObservation,
    },
    Fail {
        host: HostStamp,
        project_id: ProjectId,
        session_id: SessionId,
        turn_id: RequestId,
        reason: MasterReason,
        cause: String,
    },
}

pub(crate) enum MasterRuntimeObservation {
    Running { runtime: MasterRuntimeView },
    Response { reply: MasterReply },
    Failed { reason: MasterReason, cause: String },
    Cancelled,
    Disconnected,
}

pub(crate) struct PreparedMasterTurn {
    pub(crate) text: String,
    pub(crate) instructions: String,
    pub(crate) session: MasterSessionView,
    pub(crate) attempt: Attempt,
    pub(crate) profile: PreparedNativeProfile,
    pub(crate) repository: PathBuf,
    pub(crate) cwd: PathBuf,
    pub(crate) worktree_lease: RequestId,
    pub(crate) lease: WorktreeLease,
}

impl PreparedMasterTurn {
    /// Tells whether the prepared attempt is on the gateway task of the session's current turn.
    /// Input: the prepared turn. Output: true only when the session has a current turn and the
    /// attempt's task is the one derived from that session and turn.
    pub(crate) fn attempt_on_current_turn(&self) -> bool {
        self.session.current_turn.as_ref().is_some_and(|turn| {
            turn_task_id(&self.session.session_id, turn)
                .is_ok_and(|task| task == self.attempt.request.task_id)
        })
    }
}

pub(crate) enum MasterRuntimeResult {
    #[cfg(all(test, unix))]
    ConfiguredProfile(Box<RcliConfiguredProfileProof>),
    Prepared(Box<PreparedMasterTurn>),
    Turn(Box<MasterTurnView>),
    Profile(Box<NativeProfileUse>),
}

#[cfg(all(test, unix))]
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct MasterStopBinding {
    db_instance: String,
    project_id: ProjectId,
    session_id: SessionId,
    turn_id: RequestId,
    input_cursor: Cursor,
    provenance: OwnerProvenance,
}

#[cfg(all(test, unix))]
impl MasterStopBinding {
    pub(crate) fn from_fresh_submit(
        host: &HostStamp,
        request: &OwnerRequest,
        applied: &OwnerApplied,
    ) -> Result<Self, EngineError> {
        let hash = request.mutation_hash()?;
        let OwnerOperation::Apply {
            mutation:
                OwnerMutation::ProjectMaster {
                    action:
                        MasterOwnerAction::Submit {
                            project_id,
                            session_id,
                            ..
                        },
                },
        } = &request.operation
        else {
            return Err(error("master_stop_binding_mismatch"));
        };
        let provenance = &applied.provenance;
        if applied.historical_replay
            || applied.result != OwnerResult::Applied
            || !is_hex_id(&host.db_instance)
            || !is_hex_id(&host.engine_epoch)
            || !is_hex_id(&provenance.owner_id)
            || provenance.origin != "native_owner"
            || provenance.engine_epoch != host.engine_epoch
            || provenance.operation_id != request.request_id
            || provenance.request_hash != hash
            || provenance.admitted_epoch < 1
            || provenance.resulting_epoch != provenance.admitted_epoch
            || provenance.revision_before != request.expected_revision
            || provenance.revision_before.value().checked_add(1)
                != Some(provenance.revision_after.value())
            || applied.current_revision != provenance.revision_after
            || provenance.event_cursor.as_ref() != Some(&provenance.revision_after)
        {
            return Err(error("master_stop_binding_mismatch"));
        }
        Ok(Self {
            db_instance: host.db_instance.clone(),
            project_id: project_id.clone(),
            session_id: session_id.clone(),
            turn_id: request.request_id.clone(),
            input_cursor: provenance.revision_after.clone(),
            provenance: provenance.clone(),
        })
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Preparation {
    attempt_id: RequestId,
    auth: MasterBootstrapAuth,
    worktree_lease: RequestId,
    lease: WorktreeLease,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfiguredRegistration {
    proof: RcliConfiguredProfileProof,
    session_id: SessionId,
    project_id: ProjectId,
    bootstrap_task_id: TaskId,
    owner: OwnerProvenance,
    global_source: PathBuf,
    project_source: PathBuf,
    database_path: PathBuf,
    database_identity: (u64, u64),
}

#[cfg(unix)]
const CONFIGURED_PROFILES: &str = "CREATE TABLE configured_master_profiles (
    id TEXT PRIMARY KEY CHECK(length(id) BETWEEN 1 AND 128),
    session TEXT NOT NULL UNIQUE CHECK(length(session) BETWEEN 1 AND 128),
    body TEXT NOT NULL CHECK(length(CAST(body AS BLOB)) BETWEEN 1 AND 16384)
)";

#[cfg(unix)]
const SESSIONS: &str = "CREATE TABLE master_sessions (
    id TEXT PRIMARY KEY CHECK(length(id) BETWEEN 1 AND 128),
    project TEXT NOT NULL CHECK(length(project) BETWEEN 1 AND 128),
    created INTEGER NOT NULL UNIQUE CHECK(created > 0),
    version INTEGER NOT NULL CHECK(version >= created),
    phase TEXT NOT NULL CHECK(phase IN ('configured','closed')),
    body TEXT NOT NULL CHECK(length(CAST(body AS BLOB)) BETWEEN 1 AND 16384)
)";
#[cfg(unix)]
const CURRENT: &str = "CREATE UNIQUE INDEX master_current_project ON master_sessions(project) WHERE phase='configured'";
#[cfg(unix)]
const LATEST: &str = "CREATE INDEX master_latest_project ON master_sessions(project,created DESC)";
#[cfg(unix)]
const LEGACY_TURNS: &str = "CREATE TABLE master_turns (
    session TEXT NOT NULL CHECK(length(session) BETWEEN 1 AND 128),
    ordinal INTEGER NOT NULL CHECK(ordinal BETWEEN 1 AND 1024),
    id TEXT NOT NULL CHECK(length(id) BETWEEN 1 AND 128),
    input INTEGER NOT NULL UNIQUE CHECK(input > 0),
    phase TEXT NOT NULL CHECK(phase IN ('queued','cancelled')),
    body TEXT NOT NULL CHECK(length(CAST(body AS BLOB)) BETWEEN 1 AND 16384),
    PRIMARY KEY(session,ordinal),
    UNIQUE(session,id)
)";
#[cfg(unix)]
const LEGACY_OUTSTANDING: &str =
    "CREATE UNIQUE INDEX master_outstanding ON master_turns(session) WHERE phase='queued'";
#[cfg(unix)]
const TURNS: &str = "CREATE TABLE master_turns (
    session TEXT NOT NULL CHECK(length(session) BETWEEN 1 AND 128),
    ordinal INTEGER NOT NULL CHECK(ordinal BETWEEN 1 AND 1024),
    id TEXT NOT NULL CHECK(length(id) BETWEEN 1 AND 128),
    input INTEGER NOT NULL UNIQUE CHECK(input > 0),
    phase TEXT NOT NULL CHECK(phase IN ('queued','blocked','prepared','delivering','running','waiting_approval','succeeded','failed','cancelled','uncertain')),
    body TEXT NOT NULL CHECK(length(CAST(body AS BLOB)) BETWEEN 1 AND 16384),
    PRIMARY KEY(session,ordinal),
    UNIQUE(session,id)
)";
#[cfg(unix)]
const OUTSTANDING: &str = "CREATE UNIQUE INDEX master_outstanding ON master_turns(session) WHERE phase IN ('queued','prepared','delivering','running','waiting_approval','uncertain')";
#[cfg(unix)]
const PREPARATIONS: &str = "CREATE TABLE master_preparations (
    session TEXT NOT NULL,
    turn TEXT NOT NULL,
    body TEXT NOT NULL CHECK(length(CAST(body AS BLOB)) BETWEEN 1 AND 16384),
    PRIMARY KEY(session,turn),
    FOREIGN KEY(session,turn) REFERENCES master_turns(session,id)
)";
const SESSION_COLUMNS: &str = "CASE WHEN length(CAST(id AS BLOB))<=128 THEN id END,CASE WHEN length(CAST(project AS BLOB))<=128 THEN project END,created,version,CASE WHEN length(phase)<=10 THEN phase END,CASE WHEN length(CAST(body AS BLOB))<=16384 THEN body END";
const TURN_COLUMNS: &str = "CASE WHEN length(CAST(session AS BLOB))<=128 THEN session END,ordinal,CASE WHEN length(CAST(id AS BLOB))<=128 THEN id END,input,CASE WHEN length(phase)<=16 THEN phase END,CASE WHEN length(CAST(body AS BLOB))<=16384 THEN body END";

#[cfg(unix)]
pub(super) fn initialize(db: &Connection) -> Result<(), EngineError> {
    let tx = db.unchecked_transaction().map_err(error)?;
    let expected: BTreeMap<_, _> = [
        ("master_sessions", SESSIONS),
        ("master_current_project", CURRENT),
        ("master_latest_project", LATEST),
        ("master_turns", TURNS),
        ("master_outstanding", OUTSTANDING),
        ("master_preparations", PREPARATIONS),
    ]
    .into_iter()
    .map(|(name, sql)| (name.to_owned(), sql.to_owned()))
    .collect();
    {
        let mut statement = tx.prepare("SELECT CASE WHEN length(name)<=128 THEN name END,CASE WHEN length(CAST(sql AS BLOB))<=4096 THEN sql END FROM sqlite_schema WHERE sql IS NOT NULL AND (name GLOB 'master_*' OR tbl_name IN ('master_sessions','master_turns','master_preparations')) ORDER BY name LIMIT 7").map_err(error)?;
        let actual = statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(error)?
            .collect::<Result<BTreeMap<_, _>, _>>()
            .map_err(error)?;
        if actual.is_empty() {
            for sql in [SESSIONS, CURRENT, LATEST, TURNS, OUTSTANDING, PREPARATIONS] {
                tx.execute_batch(sql).map_err(error)?;
            }
        } else if actual != expected {
            let mut legacy = expected.clone();
            legacy.remove("master_preparations");
            legacy.insert("master_turns".into(), LEGACY_TURNS.into());
            legacy.insert("master_outstanding".into(), LEGACY_OUTSTANDING.into());
            if actual != legacy {
                return Err(error("master_schema_conflict"));
            }
            tx.execute_batch("DROP INDEX master_outstanding; ALTER TABLE master_turns RENAME TO master_turns_legacy").map_err(error)?;
            tx.execute_batch(TURNS).map_err(error)?;
            tx.execute_batch("INSERT INTO master_turns SELECT session,ordinal,id,input,phase,body FROM master_turns_legacy; DROP TABLE master_turns_legacy").map_err(error)?;
            tx.execute_batch(OUTSTANDING).map_err(error)?;
            tx.execute_batch(PREPARATIONS).map_err(error)?;
        }
    }
    let schema: Option<String> = tx
        .query_row(
            "SELECT sql FROM sqlite_schema WHERE name='configured_master_profiles'",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(error)?;
    match schema {
        Some(schema) if schema != CONFIGURED_PROFILES => {
            return Err(error("master_configured_schema_conflict"));
        }
        Some(_) => {}
        None => tx.execute_batch(CONFIGURED_PROFILES).map_err(error)?,
    }
    tx.commit().map_err(error)
}

fn encode<T: Serialize>(value: &T) -> Result<String, EngineError> {
    let body = serde_json::to_string(value).map_err(error)?;
    if body.len() > METADATA_BYTES_MAX {
        return Err(error("master_metadata_bounds"));
    }
    Ok(body)
}

fn signed(value: u64) -> Result<i64, EngineError> {
    i64::try_from(value).map_err(|_| error("master_integer_bounds"))
}

fn unsigned(value: i64) -> Result<u64, EngineError> {
    u64::try_from(value).map_err(|_| error("master_integer_bounds"))
}

fn phase(session: &MasterSessionView) -> Result<&'static str, EngineError> {
    match session.phase {
        MasterSessionPhase::Configured => Ok("configured"),
        MasterSessionPhase::Closed => Ok("closed"),
        MasterSessionPhase::Closing => Err(error("master_runtime_state_unqualified")),
    }
}

fn committed(tx: &Transaction<'_>, provenance: &OwnerProvenance) -> Result<(), EngineError> {
    let Some((actual, result)) = owner_lookup(
        tx,
        "native_owner",
        &provenance.owner_id,
        &provenance.operation_id,
        Some(&provenance.request_hash),
    )?
    else {
        return Err(error("master_owner_provenance"));
    };
    if actual != *provenance
        || serde_json::from_slice::<OwnerResult>(&result).map_err(error)? != OwnerResult::Applied
    {
        return Err(error("master_owner_provenance"));
    }
    Ok(())
}

fn decode_session(
    tx: &Transaction<'_>,
    row: &Row<'_>,
    revision: u64,
) -> Result<MasterSessionView, EngineError> {
    let id: String = row.get(0).map_err(error)?;
    let project: String = row.get(1).map_err(error)?;
    let created = unsigned(row.get(2).map_err(error)?)?;
    let version = unsigned(row.get(3).map_err(error)?)?;
    let stored_phase: String = row.get(4).map_err(error)?;
    let body: String = row.get(5).map_err(error)?;
    let session: MasterSessionView = serde_json::from_str(&body).map_err(error)?;
    session.selection.route.validate().map_err(error)?;
    if session.session_id.as_str() != id
        || session.project_id.as_str() != project
        || created == 0
        || version < created
        || version > revision
        || session.version.value() != version
        || phase(&session)? != stored_phase
        || !is_hex_id(session.config_id.as_str())
        || !is_hex_id(&session.source_hash)
        || session.owner.origin != "native_owner"
        || session.owner.revision_after.value() != created
        || session.owner.revision_before.value().checked_add(1) != Some(created)
        || session.owner.event_cursor.as_ref().map(Cursor::value) != Some(created)
    {
        return Err(error("master_session_record"));
    }
    committed(tx, &session.owner)?;
    let original = OwnerRequest {
        protocol_version: PROTOCOL_VERSION,
        request_id: session.owner.operation_id.clone(),
        expected_revision: session.owner.revision_before.clone(),
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
    if original.mutation_hash()? != session.owner.request_hash {
        return Err(error("master_open_provenance"));
    }
    let mut statement = tx
        .prepare("SELECT CASE WHEN length(CAST(id AS BLOB))<=128 THEN id END FROM master_turns WHERE session=?1 AND phase IN ('queued','prepared','delivering','running','waiting_approval','uncertain') LIMIT 2")
        .map_err(error)?;
    let mut rows = statement
        .query([session.session_id.as_str()])
        .map_err(error)?;
    let pending = rows
        .next()
        .map_err(error)?
        .map(|row| row.get::<_, String>(0))
        .transpose()
        .map_err(error)?;
    if rows.next().map_err(error)?.is_some()
        || pending.as_deref() != session.current_turn.as_ref().map(RequestId::as_str)
        || (session.phase == MasterSessionPhase::Closed && pending.is_some())
    {
        return Err(error("master_current_turn_record"));
    }
    if let Some(id) = &session.current_turn {
        let turn = load_turn(tx, &session, id, revision)?
            .ok_or_else(|| error("master_current_turn_record"))?;
        if !turn.phase.outstanding() {
            return Err(error("master_current_turn_record"));
        }
    }
    Ok(session)
}

fn load_session(
    tx: &Transaction<'_>,
    id: &SessionId,
    revision: u64,
) -> Result<Option<MasterSessionView>, EngineError> {
    let sql = format!("SELECT {SESSION_COLUMNS} FROM master_sessions WHERE id=?1");
    let mut statement = tx.prepare(&sql).map_err(error)?;
    let mut rows = statement.query([id.as_str()]).map_err(error)?;
    rows.next()
        .map_err(error)?
        .map(|row| decode_session(tx, row, revision))
        .transpose()
}

fn latest(
    tx: &Transaction<'_>,
    project: &ProjectId,
    revision: u64,
) -> Result<Option<MasterSessionView>, EngineError> {
    let sql = format!(
        "SELECT {SESSION_COLUMNS} FROM master_sessions WHERE project=?1 ORDER BY created DESC LIMIT 1"
    );
    let mut statement = tx.prepare(&sql).map_err(error)?;
    let mut rows = statement.query([project.as_str()]).map_err(error)?;
    rows.next()
        .map_err(error)?
        .map(|row| decode_session(tx, row, revision))
        .transpose()
}

fn decode_turn(
    tx: &Transaction<'_>,
    session: &MasterSessionView,
    row: &Row<'_>,
    revision: u64,
) -> Result<MasterTurnView, EngineError> {
    let stored_session: String = row.get(0).map_err(error)?;
    let ordinal = unsigned(row.get(1).map_err(error)?)?;
    let id: String = row.get(2).map_err(error)?;
    let input = unsigned(row.get(3).map_err(error)?)?;
    let stored_phase: String = row.get(4).map_err(error)?;
    let body: String = row.get(5).map_err(error)?;
    let turn: MasterTurnView = serde_json::from_str(&body).map_err(error)?;
    if stored_session != session.session_id.as_str()
        || turn.session_id != session.session_id
        || turn.id.as_str() != id
        || turn.ordinal != ordinal
        || ordinal == 0
        || ordinal > u64::try_from(TURNS_MAX).map_err(error)?
        || input == 0
        || input > revision
        || input > session.version.value()
        || turn.input_cursor.value() != input
        || turn.phase.name() != stored_phase
    {
        return Err(error("master_turn_record"));
    }
    let input_owner: Option<(String, String)> = tx.query_row("SELECT CASE WHEN length(o.owner_id)=64 THEN o.owner_id END,CASE WHEN length(CAST(c.text AS BLOB))<=8192 THEN c.text END FROM conversation_entries c JOIN owner_operations o ON o.event_cursor=c.event_cursor AND o.origin='native_owner' WHERE c.project=?1 AND c.event_cursor=?2 AND c.kind='message' AND c.brief_version=0 AND o.operation_id=?3", params![session.project_id.as_str(), signed(input)?, turn.id.as_str()], |row| Ok((row.get(0)?, row.get(1)?))).optional().map_err(error)?;
    let (owner, text) = input_owner.ok_or_else(|| error("master_input_provenance"))?;
    ConversationMutation::AppendMessage {
        project_id: session.project_id.clone(),
        text: text.clone(),
    }
    .validate()?;
    let (provenance, result) = owner_lookup(tx, "native_owner", &owner, &turn.id, None)?
        .ok_or_else(|| error("master_input_provenance"))?;
    if provenance.event_cursor != Some(turn.input_cursor.clone())
        || provenance.revision_after != turn.input_cursor
        || provenance.revision_before.value().checked_add(1) != Some(input)
        || serde_json::from_slice::<OwnerResult>(&result).map_err(error)? != OwnerResult::Applied
    {
        return Err(error("master_input_provenance"));
    }
    let original = OwnerRequest {
        protocol_version: PROTOCOL_VERSION,
        request_id: turn.id.clone(),
        expected_revision: provenance.revision_before,
        operation: OwnerOperation::Apply {
            mutation: OwnerMutation::ProjectMaster {
                action: MasterOwnerAction::Submit {
                    project_id: session.project_id.clone(),
                    session_id: session.session_id.clone(),
                    text,
                },
            },
        },
    };
    if original.mutation_hash()? != provenance.request_hash {
        return Err(error("master_submit_provenance"));
    }
    validate_runtime_record(tx, session, &turn)?;
    Ok(turn)
}

fn load_turn(
    tx: &Transaction<'_>,
    session: &MasterSessionView,
    id: &RequestId,
    revision: u64,
) -> Result<Option<MasterTurnView>, EngineError> {
    let sql = format!("SELECT {TURN_COLUMNS} FROM master_turns WHERE session=?1 AND id=?2");
    let mut statement = tx.prepare(&sql).map_err(error)?;
    let mut rows = statement
        .query(params![session.session_id.as_str(), id.as_str()])
        .map_err(error)?;
    rows.next()
        .map_err(error)?
        .map(|row| decode_turn(tx, session, row, revision))
        .transpose()
}

fn save_session(tx: &Transaction<'_>, session: &MasterSessionView) -> Result<(), EngineError> {
    let changed = tx
        .execute(
            "UPDATE master_sessions SET version=?1,phase=?2,body=?3 WHERE id=?4",
            params![
                signed(session.version.value())?,
                phase(session)?,
                encode(session)?,
                session.session_id.as_str()
            ],
        )
        .map_err(error)?;
    assert_eq!(changed, 1);
    Ok(())
}

fn cancel_turn(
    tx: &Transaction<'_>,
    mut turn: MasterTurnView,
) -> Result<MasterTurnView, EngineError> {
    if !turn.phase.outstanding() {
        return Err(error("master_turn_not_queued"));
    }
    let queued = turn.phase == MasterTurnPhase::Queued;
    if queued {
        assert!(turn.attempt.is_none() && turn.runtime.is_none() && turn.reply.is_none());
    } else {
        complete_local(tx, &mut turn, MasterTurnPhase::Cancelled)?;
    }
    turn.phase = MasterTurnPhase::Cancelled;
    turn.reason = Some(MasterReason::Cancelled);
    if queued {
        let changed = tx.execute("UPDATE master_turns SET phase='cancelled',body=?1 WHERE session=?2 AND id=?3 AND phase='queued'", params![encode(&turn)?, turn.session_id.as_str(), turn.id.as_str()]).map_err(error)?;
        assert_eq!(changed, 1);
        assert!(turn.attempt.is_none() && turn.runtime.is_none() && turn.reply.is_none());
    } else {
        save_turn(tx, &turn)?;
    }
    Ok(turn)
}

fn preparation(
    tx: &Transaction<'_>,
    session: &MasterSessionView,
    turn: &MasterTurnView,
) -> Result<Preparation, EngineError> {
    let body: Option<String> = tx.query_row(
        "SELECT CASE WHEN length(CAST(body AS BLOB))<=16384 THEN body END FROM master_preparations WHERE session=?1 AND turn=?2",
        params![session.session_id.as_str(), turn.id.as_str()], |row| row.get(0),
    ).optional().map_err(error)?;
    let record: Preparation =
        serde_json::from_str(&body.ok_or_else(|| error("master_preparation_missing"))?)
            .map_err(error)?;
    let attempt = turn
        .attempt
        .as_ref()
        .ok_or_else(|| error("master_attempt_missing"))?;
    if record.attempt_id != attempt.request.id
        || record.auth.binding().connection != attempt.connection
        || matches!(record.auth, MasterBootstrapAuth::RcliConfigured(_))
            && (record.auth.binding().config_id != session.config_id
                || record.auth.binding().source_hash != session.source_hash)
        || record.lease.project_id != session.project_id
        || record.lease.task_id != session.bootstrap_task_id
        || record.lease.owner_session != session.session_id
        || record.lease.state != LeaseState::Ready
        || record.lease.native_descendant.is_some()
    {
        return Err(error("master_preparation_record"));
    }
    Ok(record)
}

fn validate_runtime(runtime: &MasterRuntimeView) -> Result<(), EngineError> {
    if !is_hex_id(&runtime.artifact_sha256)
        || runtime
            .native_session_id
            .as_ref()
            .is_some_and(|id| id.is_empty() || id.len() > 128 || id.chars().any(char::is_control))
    {
        return Err(error("master_runtime_provenance"));
    }
    Ok(())
}

fn validate_runtime_record(
    tx: &Transaction<'_>,
    session: &MasterSessionView,
    turn: &MasterTurnView,
) -> Result<(), EngineError> {
    let valid = match turn.phase {
        MasterTurnPhase::Queued => {
            turn.reason.is_none()
                && turn.attempt.is_none()
                && turn.runtime.is_none()
                && turn.reply.is_none()
        }
        MasterTurnPhase::Blocked => {
            turn.reason.is_some()
                && turn.attempt.is_none()
                && turn.runtime.is_none()
                && turn.reply.is_none()
        }
        MasterTurnPhase::Prepared | MasterTurnPhase::Delivering => {
            turn.reason.is_none()
                && turn.attempt.is_some()
                && turn.runtime.is_none()
                && turn.reply.is_none()
        }
        MasterTurnPhase::Running => {
            turn.reason.is_none()
                && turn.attempt.is_some()
                && turn.runtime.is_some()
                && turn.reply.is_none()
        }
        MasterTurnPhase::Succeeded => {
            turn.reason.is_none()
                && turn.attempt.is_some()
                && turn.runtime.is_some()
                && turn.reply.is_some()
        }
        MasterTurnPhase::Failed => {
            turn.reason.is_some() && turn.attempt.is_some() && turn.reply.is_none()
        }
        MasterTurnPhase::Cancelled => {
            turn.reason == Some(MasterReason::Cancelled) && turn.reply.is_none()
        }
        MasterTurnPhase::Uncertain => {
            turn.reason == Some(MasterReason::CaptureIncomplete)
                && turn.attempt.is_some()
                && turn.reply.is_none()
        }
        MasterTurnPhase::WaitingApproval => false,
    };
    if !valid
        || turn.accounts.len() > crate::config::POOL_MEMBERS_MAX
        || (!turn.accounts.is_empty() && turn.phase != MasterTurnPhase::Blocked)
    {
        return Err(error("master_runtime_state_unqualified"));
    }
    if let Some(attempt) = &turn.attempt {
        if model_gateway::read_attempt(tx, &attempt.request.id)?.as_ref() != Some(attempt)
            || attempt.config_id != session.config_id
            || attempt.request.project_id != session.project_id
            || attempt.request.task_id != turn_task_id(&session.session_id, &turn.id)?
            || attempt.request.role != Role::ProjectLeader
            || attempt.request.pool_ref != session.selection.pool_ref
            || (
                attempt.request.target_profile_ref.clone(),
                attempt.request.route.clone(),
            ) != member_request(session, &attempt.connection)
            || attempt.request.previous_attempt.is_some()
            || (matches!(
                turn.phase,
                MasterTurnPhase::Prepared | MasterTurnPhase::Delivering | MasterTurnPhase::Running
            ) && attempt.completion.is_some())
            || (matches!(
                turn.phase,
                MasterTurnPhase::Succeeded
                    | MasterTurnPhase::Failed
                    | MasterTurnPhase::Cancelled
                    | MasterTurnPhase::Uncertain
            ) && attempt.completion.is_none())
        {
            return Err(error("master_attempt_provenance"));
        }
        preparation(tx, session, turn)?;
        if let Some(runtime) = &turn.runtime {
            validate_runtime(runtime)?;
        }
        if let Some(reply) = &turn.reply {
            let runtime = turn
                .runtime
                .as_ref()
                .ok_or_else(|| error("master_runtime_missing"))?;
            if reply.text.is_empty()
                || reply.text.len() > 8192
                || reply.text.contains('\0')
                || reply.provenance.attempt_id != attempt.request.id
                || reply.provenance.runtime_ref != runtime.runtime_ref
                || reply.provenance.capture_sequence == 0
                || attempt.completion.as_ref().is_none_or(|completion| {
                    completion.outcome != AttemptOutcome::Succeeded
                        || completion.effects != Effects::Committed
                })
            {
                return Err(error("master_reply_provenance"));
            }
        }
    } else if turn.runtime.is_some() || turn.reply.is_some() {
        return Err(error("master_runtime_state_unqualified"));
    }
    Ok(())
}

fn now_ms() -> Result<u64, EngineError> {
    u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(error)?
            .as_millis(),
    )
    .map_err(error)
}

fn save_turn(tx: &Transaction<'_>, turn: &MasterTurnView) -> Result<(), EngineError> {
    let changed = tx
        .execute(
            "UPDATE master_turns SET phase=?1,body=?2 WHERE session=?3 AND id=?4",
            params![
                turn.phase.name(),
                encode(turn)?,
                turn.session_id.as_str(),
                turn.id.as_str()
            ],
        )
        .map_err(error)?;
    assert_eq!(changed, 1);
    Ok(())
}

fn persist_runtime(
    tx: &Transaction<'_>,
    config: &EffectiveConfig,
    session: &mut MasterSessionView,
    turn: &MasterTurnView,
) -> Result<(), EngineError> {
    let next = owner_revision(tx)?
        .checked_add(1)
        .ok_or_else(|| error("master_integer_bounds"))?;
    signed(next)?;
    if !turn.phase.outstanding() && session.current_turn.as_ref() == Some(&turn.id) {
        session.current_turn = None;
    }
    session.version = Cursor::new(next);
    save_turn(tx, turn)?;
    save_session(tx, session)?;
    persist_event_ids(
        tx,
        config,
        &turn.id,
        &ClientId::new("master-runtime").map_err(error)?,
        next,
    )?;
    assert!(!tx.is_autocommit());
    Ok(())
}

fn complete_local(
    tx: &Transaction<'_>,
    turn: &mut MasterTurnView,
    terminal: MasterTurnPhase,
) -> Result<(), EngineError> {
    let Some(attempt) = &turn.attempt else {
        return Ok(());
    };
    if attempt.completion.is_some() {
        return Ok(());
    }
    let completion = AttemptCompletion {
        outcome: AttemptOutcome::Failed(FailureKind::Unknown),
        effects: if turn.phase == MasterTurnPhase::Prepared
            && terminal != MasterTurnPhase::Uncertain
        {
            Effects::None
        } else {
            Effects::Uncertain
        },
        finished_at_ms: now_ms()?.max(attempt.selected_at_ms),
        retry_at_ms: None,
    };
    model_gateway::finish_attempt_without_observation(tx, &attempt.request.id, &completion)?;
    turn.attempt = model_gateway::read_attempt(tx, &attempt.request.id)?;
    assert!(
        turn.attempt
            .as_ref()
            .is_some_and(|attempt| attempt.completion.is_some())
    );
    Ok(())
}

fn load_runtime_turn(
    tx: &Transaction<'_>,
    host: &HostStamp,
    project: &ProjectId,
    session_id: &SessionId,
    turn_id: &RequestId,
) -> Result<(MasterSessionView, MasterTurnView), EngineError> {
    check_owner_host(tx, host)?;
    let revision = owner_revision(tx)?;
    let session =
        load_session(tx, session_id, revision)?.ok_or_else(|| error("master_session_missing"))?;
    if &session.project_id != project {
        return Err(error("master_session_scope_conflict"));
    }
    let turn =
        load_turn(tx, &session, turn_id, revision)?.ok_or_else(|| error("master_turn_missing"))?;
    Ok((session, turn))
}

#[cfg(all(test, unix))]
pub(super) fn settle_stopped_commit(
    db: &mut Connection,
    config: &EffectiveConfig,
    host: &HostStamp,
    binding: &MasterStopBinding,
) -> Result<MasterTurnView, EngineError> {
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(error)?;
    check_owner_host(&tx, host)?;
    let provenance = &binding.provenance;
    if binding.db_instance != host.db_instance
        || provenance.origin != "native_owner"
        || provenance.engine_epoch != host.engine_epoch
        || provenance.operation_id != binding.turn_id
        || provenance.revision_after != binding.input_cursor
        || provenance.event_cursor.as_ref() != Some(&binding.input_cursor)
        || provenance.revision_before.value().checked_add(1) != Some(binding.input_cursor.value())
        || provenance.admitted_epoch < 1
        || provenance.resulting_epoch != provenance.admitted_epoch
    {
        return Err(error("master_stop_binding_mismatch"));
    }
    let (stored, result) = owner_lookup(
        &tx,
        "native_owner",
        &provenance.owner_id,
        &provenance.operation_id,
        Some(&provenance.request_hash),
    )?
    .ok_or_else(|| error("master_stop_binding_mismatch"))?;
    if stored != *provenance
        || serde_json::from_slice::<OwnerResult>(&result).map_err(error)? != OwnerResult::Applied
    {
        return Err(error("master_stop_binding_mismatch"));
    }
    let (mut session, mut turn) = load_runtime_turn(
        &tx,
        host,
        &binding.project_id,
        &binding.session_id,
        &binding.turn_id,
    )?;
    if turn.input_cursor != binding.input_cursor {
        return Err(error("master_stop_binding_mismatch"));
    }
    if turn.phase.outstanding() && session.current_turn.as_ref() != Some(&turn.id) {
        return Err(error("master_turn_not_current"));
    }
    match turn.phase {
        MasterTurnPhase::Queued | MasterTurnPhase::Prepared => {
            turn = cancel_turn(&tx, turn)?;
        }
        MasterTurnPhase::Delivering | MasterTurnPhase::Running => {
            complete_local(&tx, &mut turn, MasterTurnPhase::Uncertain)?;
            turn.phase = MasterTurnPhase::Uncertain;
            turn.reason = Some(MasterReason::CaptureIncomplete);
        }
        MasterTurnPhase::Blocked
        | MasterTurnPhase::Succeeded
        | MasterTurnPhase::Failed
        | MasterTurnPhase::Cancelled
        | MasterTurnPhase::Uncertain => {
            tx.commit().map_err(error)?;
            return Ok(turn);
        }
        MasterTurnPhase::WaitingApproval => {
            return Err(error("master_runtime_state_unqualified"));
        }
    }
    persist_runtime(&tx, config, &mut session, &turn)?;
    tx.commit().map_err(error)?;
    Ok(turn)
}

fn current_scope(
    tx: &Transaction<'_>,
    config: &EffectiveConfig,
    sources: Option<&settings::SettingsSources>,
    host: &HostStamp,
    session: &MasterSessionView,
    turn: &MasterTurnView,
) -> Result<(), EngineError> {
    settings::ensure_clear(tx)?;
    check_owner_admission(
        tx,
        &Admission {
            db_instance: host.db_instance.clone(),
            engine_epoch: host.engine_epoch.clone(),
            owner_id: session.owner.owner_id.clone(),
            owner_epoch: session.owner.resulting_epoch,
        },
    )?;
    if session.owner.engine_epoch != host.engine_epoch
        || session.config_id != config.id
        || session.source_hash != settings::verified_source_hash(config, sources)?
    {
        return Err(error("master_source_conflict"));
    }
    validate_selection(config, &session.project_id, &session.selection)?;
    if turn.phase.outstanding()
        && (session.phase != MasterSessionPhase::Configured
            || session.current_turn.as_ref() != Some(&turn.id))
    {
        return Err(error("master_turn_not_current"));
    }
    Ok(())
}

fn configured_connection<'a>(
    config: &'a EffectiveConfig,
    session: &MasterSessionView,
) -> Result<&'a ModelConnection, EngineError> {
    validate_selection(config, &session.project_id, &session.selection)?;
    if session.selection.backend != MasterBackend::Rcli {
        return Err(error("master_runtime_backend_unsupported"));
    }
    let catalog = config
        .settings
        .model_access
        .as_ref()
        .ok_or_else(|| error("master_pool_missing"))?;
    let pool = catalog
        .pools
        .iter()
        .find(|pool| pool.id == session.selection.pool_ref)
        .ok_or_else(|| error("master_pool_missing"))?;
    let mut candidates = catalog.connections.iter().filter(|connection| {
        pool.members.contains(&connection.id)
            && connection.account_ref == session.selection.route.account_ref
            && connection.harness.as_str() == "rcli"
            && matches!(connection.endpoint, ModelEndpoint::Http { .. })
            && matches!(
                connection.auth,
                ModelAuthentication::ApiKey { .. } | ModelAuthentication::None {}
            )
    });
    let connection = candidates
        .next()
        .ok_or_else(|| error("rcli_profile_configuration_required"))?;
    if candidates.next().is_some() {
        return Err(error("master_rcli_connection_ambiguous"));
    }
    Ok(connection)
}

fn registration(
    tx: &Transaction<'_>,
    session: &SessionId,
) -> Result<Option<ConfiguredRegistration>, EngineError> {
    let row: Option<(String, String)> = tx.query_row(
        "SELECT id,CASE WHEN length(CAST(body AS BLOB))<=16384 THEN body END FROM configured_master_profiles WHERE session=?1",
        [session.as_str()], |row| Ok((row.get(0)?, row.get(1)?)),
    ).optional().map_err(error)?;
    row.map(|(id, body)| {
        let record: ConfiguredRegistration = serde_json::from_str(&body).map_err(error)?;
        if record.session_id != *session || record.proof.registration_id.as_str() != id {
            return Err(error("master_configured_receipt_conflict"));
        }
        Ok(record)
    })
    .transpose()
}

fn validate_registration(
    tx: &Transaction<'_>,
    session: &MasterSessionView,
    record: &ConfiguredRegistration,
) -> Result<(), EngineError> {
    let proof = &record.proof;
    let host = HostStamp {
        db_instance: proof.database_instance.clone(),
        engine_epoch: proof.engine_epoch.clone(),
    };
    check_owner_host(tx, &host)?;
    check_owner_admission(
        tx,
        &Admission {
            db_instance: host.db_instance.clone(),
            engine_epoch: host.engine_epoch.clone(),
            owner_id: session.owner.owner_id.clone(),
            owner_epoch: session.owner.resulting_epoch,
        },
    )?;
    settings::ensure_clear(tx)?;
    let committed = load_session(tx, &record.session_id, owner_revision(tx)?)?
        .ok_or_else(|| error("master_session_missing"))?;
    if committed != *session
        || session.phase != MasterSessionPhase::Configured
        || record.project_id != session.project_id
        || record.bootstrap_task_id != session.bootstrap_task_id
        || record.owner != session.owner
        || session.owner.engine_epoch != host.engine_epoch
        || proof.config_id != session.config_id
        || proof.source_hash != session.source_hash
        || proof.profile_ref != session.selection.target_profile_ref
        || session.selection.backend != MasterBackend::Rcli
        || proof.at_revision.value() < session.owner.revision_after.value()
        || proof.at_revision.value() > owner_revision(tx)?
        || proof.generation == 0
        || !is_hex_id(&proof.profile_config_sha256)
        || tx.path().map(Path::new) != Some(record.database_path.as_path())
    {
        return Err(error("master_configured_receipt_conflict"));
    }
    let metadata = std::fs::symlink_metadata(&record.database_path).map_err(error)?;
    if record.database_identity != database_identity(&metadata)? {
        return Err(error("master_configured_database_changed"));
    }
    let config = crate::config::resolve_config(&record.global_source, &record.project_source)
        .map_err(|failure| error(failure.0))?;
    if config.id != proof.config_id
        || configured_connection(&config, session)? != &proof.connection
        || settings::verified_source_hash_at(
            &config,
            &record.global_source,
            &record.project_source,
        )? != proof.source_hash
    {
        return Err(error("master_configured_receipt_stale"));
    }
    let mut custody = NativeProfileCustody::capture(&record.database_path, &metadata)?;
    let physical = custody.configured_rcli_profile(&host, &proof.connection, &proof.profile_ref)?;
    if physical.profile_ref != proof.profile_ref
        || physical.custody_id != proof.custody_id
        || physical.profile_config_sha256 != proof.profile_config_sha256
        || physical.profile_generation != proof.generation
    {
        return Err(error("master_configured_receipt_stale"));
    }
    Ok(())
}

fn configured_auth(
    tx: &Transaction<'_>,
    config: &EffectiveConfig,
    host: &HostStamp,
    session: &MasterSessionView,
) -> Result<RcliConfiguredProfileProof, EngineError> {
    let record = registration(tx, &session.session_id)?
        .ok_or_else(|| error("rcli_profile_configuration_required"))?;
    validate_registration(tx, session, &record)?;
    if record.proof.database_instance != host.db_instance
        || record.proof.engine_epoch != host.engine_epoch
        || record.proof.config_id != config.id
        || configured_connection(config, session)? != &record.proof.connection
        || settings::verified_source_hash_at(config, &record.global_source, &record.project_source)?
            != record.proof.source_hash
    {
        return Err(error("master_configured_receipt_stale"));
    }
    Ok(record.proof)
}

pub(super) fn validate_bootstrap_auth(
    tx: &Transaction<'_>,
    session: &MasterSessionView,
    auth: &MasterBootstrapAuth,
) -> Result<(), EngineError> {
    match auth {
        MasterBootstrapAuth::RcliConfigured(proof) => {
            let record = registration(tx, &session.session_id)?
                .ok_or_else(|| error("rcli_profile_configuration_required"))?;
            if record.proof != *proof {
                return Err(error("master_configured_receipt_conflict"));
            }
            validate_registration(tx, session, &record)
        }
        MasterBootstrapAuth::NativeSignIn(fact) => {
            match fact.phase {
                NativeAuthPhase::AuthenticatedObserved => {}
                NativeAuthPhase::Starting
                | NativeAuthPhase::AwaitingOwner
                | NativeAuthPhase::CheckingAccount
                | NativeAuthPhase::UnauthenticatedObserved
                | NativeAuthPhase::Cancelling
                | NativeAuthPhase::Failed
                | NativeAuthPhase::Cancelled
                | NativeAuthPhase::Unavailable
                | NativeAuthPhase::Uncertain => {
                    return Err(error("master_bootstrap_authentication_stale"));
                }
            }
            if !matches!(fact.action, NativeAuthAction::SignIn { .. })
                || fact.binding.engine_epoch != session.owner.engine_epoch
                || fact.binding.at_revision.value() > owner_revision(tx)?
                || !crate::master_session::backend_member(
                    session.selection.backend,
                    &fact.binding.connection,
                )
            {
                return Err(error("master_bootstrap_authentication_stale"));
            }
            let ModelAuthentication::NativeManaged { profile_ref } = &fact.binding.connection.auth
            else {
                return Err(error("master_bootstrap_authentication_stale"));
            };
            let key = crate::memory::hash(profile_ref.as_str().as_bytes());
            let actual: Option<Vec<u8>> = tx.query_row(
                "SELECT fact FROM native_auth_operations WHERE profile_key=?1 ORDER BY generation DESC LIMIT 1",
                [&key], |row| row.get(0),
            ).optional().map_err(error)?;
            let actual = crate::engine::owner::native_auth::decode_fact(
                &actual.ok_or_else(|| error("master_bootstrap_authentication_stale"))?,
            )?;
            let host = HostStamp {
                db_instance: fact.binding.database_instance.clone(),
                engine_epoch: fact.binding.engine_epoch.clone(),
            };
            check_owner_host(tx, &host)?;
            check_owner_admission(
                tx,
                &Admission {
                    db_instance: host.db_instance,
                    engine_epoch: host.engine_epoch,
                    owner_id: session.owner.owner_id.clone(),
                    owner_epoch: session.owner.resulting_epoch,
                },
            )?;
            let fenced: bool = tx
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM native_auth_fences WHERE profile_key=?1)",
                    [&key],
                    |row| row.get(0),
                )
                .map_err(error)?;
            if actual != *fact || fenced {
                return Err(error("master_bootstrap_authentication_stale"));
            }
            Ok(())
        }
    }
}

pub(crate) fn validate_configured_initial_conversation_receipt(
    tx: &Transaction<'_>,
    pinned: &EffectiveConfig,
    current: &EffectiveConfig,
    request: &AttemptRequest,
    proof: &RcliConfiguredProfileProof,
    binding: &NativeProfileBinding,
    generation: u64,
) -> Result<(), EngineError> {
    let row: Option<String> = tx
        .query_row(
            "SELECT session FROM configured_master_profiles WHERE id=?1",
            [proof.registration_id.as_str()],
            |row| row.get(0),
        )
        .optional()
        .map_err(error)?;
    let session_id = SessionId::new(row.ok_or_else(|| error("master_configured_receipt_missing"))?)
        .map_err(error)?;
    let session = load_session(tx, &session_id, owner_revision(tx)?)?
        .ok_or_else(|| error("master_session_missing"))?;
    let host = HostStamp {
        db_instance: proof.database_instance.clone(),
        engine_epoch: proof.engine_epoch.clone(),
    };
    let actual = configured_auth(tx, current, &host, &session)?;
    let turn_task = session
        .current_turn
        .as_ref()
        .map(|turn| turn_task_id(&session.session_id, turn))
        .transpose()?;
    if actual != *proof
        || pinned.id != current.id
        || pinned.id != proof.config_id
        || binding != &MasterBootstrapAuth::RcliConfigured(actual).binding()
        || generation != proof.generation
        || request.project_id != session.project_id
        || turn_task.as_ref() != Some(&request.task_id)
        || request.role != Role::ProjectLeader
        || request.pool_ref != session.selection.pool_ref
        || request.target_profile_ref != session.selection.target_profile_ref
        || request.route != session.selection.route
        || request.previous_attempt.is_some()
        || !request.route.required_capabilities.is_empty()
    {
        return Err(error("gateway_initial_authentication_stale"));
    }
    let turn_id = session
        .current_turn
        .clone()
        .ok_or_else(|| error("master_turn_missing"))?;
    let turn = load_turn(tx, &session, &turn_id, owner_revision(tx)?)?
        .ok_or_else(|| error("master_turn_missing"))?;
    let replay = model_gateway::read_attempt(tx, &request.id)?;
    let valid = match replay {
        Some(attempt) => {
            attempt.request == *request
                && attempt.config_id == pinned.id
                && attempt.connection == proof.connection
                && (turn.attempt.as_ref().map(|attempt| &attempt.request.id) == Some(&request.id)
                    || (turn.phase == MasterTurnPhase::Queued
                        && session.current_turn.as_ref() == Some(&turn.id)))
        }
        None => {
            turn.phase == MasterTurnPhase::Queued
                && turn.attempt.is_none()
                && session.current_turn.as_ref() == Some(&turn.id)
        }
    };
    if !valid {
        return Err(error("gateway_initial_authentication_stale"));
    }
    let prior: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM model_gateway_attempts WHERE task_id=?1 AND id<>?2)",
            params![request.task_id.as_str(), request.id.as_str()],
            |row| row.get(0),
        )
        .map_err(error)?;
    if prior {
        return Err(error("gateway_initial_authentication_stale"));
    }
    Ok(())
}

fn authenticated(
    tx: &Transaction<'_>,
    config: &EffectiveConfig,
    host: &HostStamp,
    session: &MasterSessionView,
    member: &ModelConnection,
    custody: &mut Result<NativeProfileCustody, EngineError>,
) -> Result<MasterBootstrapAuth, EngineError> {
    let fact = current_auth(tx, config, host, session, member)?;
    let custody = custody.as_mut().map_err(|error| error.clone())?;
    match &fact {
        MasterBootstrapAuth::NativeSignIn(fact) => {
            let (custody_id, _) = custody.prepare(host, &fact.binding.connection)?;
            if custody_id != fact.binding.custody_id {
                return Err(error("native_profile_identity_changed"));
            }
        }
        MasterBootstrapAuth::RcliConfigured(proof) => {
            let physical =
                custody.configured_rcli_profile(host, &proof.connection, &proof.profile_ref)?;
            if physical.profile_ref != proof.profile_ref
                || physical.custody_id != proof.custody_id
                || physical.profile_config_sha256 != proof.profile_config_sha256
                || physical.profile_generation != proof.generation
            {
                return Err(error("master_configured_receipt_stale"));
            }
        }
    }
    Ok(fact)
}

/// Names why the latest sign-in cannot authorize a master turn.
/// Input: that sign-in's phase. Output: None when authenticated, the
/// not-signed-in code when it finished without an account, else the stale code.
fn sign_in_gap(phase: NativeAuthPhase) -> Option<&'static str> {
    match phase {
        NativeAuthPhase::AuthenticatedObserved => None,
        NativeAuthPhase::UnauthenticatedObserved
        | NativeAuthPhase::Failed
        | NativeAuthPhase::Cancelled
        | NativeAuthPhase::Unavailable => Some("master_authentication_unknown"),
        NativeAuthPhase::Starting
        | NativeAuthPhase::AwaitingOwner
        | NativeAuthPhase::CheckingAccount
        | NativeAuthPhase::Cancelling
        | NativeAuthPhase::Uncertain => Some("master_authentication_stale"),
    }
}

/// Proves one member of the master's pool can authorize its turns now.
/// Inputs: the transaction, settings, host, session and member connection. Output: the
/// RCLI configured proof, or the member's sign-in while it is an allowed member of the
/// pool; the missing, unknown, stale or in-use code otherwise.
fn current_auth(
    tx: &Transaction<'_>,
    config: &EffectiveConfig,
    host: &HostStamp,
    session: &MasterSessionView,
    member: &ModelConnection,
) -> Result<MasterBootstrapAuth, EngineError> {
    if session.selection.backend == MasterBackend::Rcli {
        return configured_auth(tx, config, host, session).map(MasterBootstrapAuth::RcliConfigured);
    }
    if !master_members(config, session)
        .into_iter()
        .any(|(connection, gap, _)| connection == member && gap.is_none())
    {
        return Err(error("native_profile_connection_missing"));
    }
    let connection = native_profile_connection(config, &member.id)?;
    native_sign_in(tx, host, connection, Some(&member.account_ref))
        .map(MasterBootstrapAuth::NativeSignIn)
}

/// Lists the connections of a pool in pool order.
/// Inputs: the settings and the pool. Output: each member connection the catalog defines,
/// empty when the pool or catalog is gone.
fn pool_connections<'a>(
    config: &'a EffectiveConfig,
    pool_ref: &crate::protocol::CredentialRef,
) -> Vec<&'a ModelConnection> {
    let Some(catalog) = config.settings.model_access.as_ref() else {
        return Vec::new();
    };
    let Some(pool) = catalog.pools.iter().find(|pool| &pool.id == pool_ref) else {
        return Vec::new();
    };
    let members: Vec<_> = pool
        .members
        .iter()
        .filter_map(|id| catalog.connections.iter().find(|item| &item.id == id))
        .collect();
    assert!(members.len() <= crate::config::POOL_MEMBERS_MAX);
    members
}

fn allowed_destination(config: &EffectiveConfig, target: &crate::protocol::CredentialRef) -> bool {
    config.settings.allowed_destinations.contains(target)
        && !config.settings.revoked_destinations.contains(target)
}

/// Why one pool member cannot run a master turn: the reason the owner sees and the plain
/// refusal code a blocked turn stores as its cause.
type Gap = (MasterReason, String);

/// One pool member a master turn may try: its connection, the settings gap that rules it
/// out before any check (None when settings allow it) and whether the owner chose it.
type Member<'a> = (&'a ModelConnection, Option<Gap>, bool);

/// Lists the pool members that may run a native master's turns, the owner's chosen
/// connection first and then in pool order. Inputs: the settings and the session.
/// Output: each pool connection of the master harness, ScopeRevoked with the refusal code
/// when settings do not allow its destination or account, and whether it is the owner's.
fn master_members<'a>(config: &'a EffectiveConfig, session: &MasterSessionView) -> Vec<Member<'a>> {
    let mut members: Vec<_> = pool_connections(config, &session.selection.pool_ref)
        .into_iter()
        .filter(|connection| {
            crate::master_session::backend_member(session.selection.backend, connection)
        })
        .map(|connection| {
            let scope = crate::routing::validate_pool_scope(
                config,
                &session.selection.pool_ref,
                &session.project_id,
                &Role::ProjectLeader,
                &connection.account_ref,
            );
            let gap = if !allowed_destination(config, &connection.id) {
                Some("master_destination_not_allowed".to_owned())
            } else {
                scope.err().map(|refusal| cause_code(&refusal.0))
            };
            let chosen = connection.id == session.selection.target_profile_ref;
            (
                connection,
                gap.map(|code| (MasterReason::ScopeRevoked, code)),
                chosen,
            )
        })
        .collect();
    members.sort_by_key(|(_, _, chosen)| !chosen);
    assert!(members.iter().filter(|(_, _, chosen)| *chosen).count() <= 1);
    members
}

/// Lists the members a turn may try, in order: the one configured RCLI profile, or the
/// native pool members. Inputs: the settings and the session. Output: each member with
/// its settings gap and whether the owner chose it, or the RCLI configuration code.
fn candidates<'a>(
    config: &'a EffectiveConfig,
    session: &MasterSessionView,
) -> Result<Vec<Member<'a>>, EngineError> {
    if session.selection.backend == MasterBackend::Rcli {
        return Ok(vec![(configured_connection(config, session)?, None, true)]);
    }
    Ok(master_members(config, session))
}

/// The target and route a turn's attempt uses on one member: the owner's selection for
/// RCLI, and for a native master the member's connection and account in place of the
/// chosen ones. Inputs: the session and the member. Output: the target and route.
fn member_request(
    session: &MasterSessionView,
    member: &ModelConnection,
) -> (crate::protocol::CredentialRef, crate::routing::RouteRequest) {
    let route = &session.selection.route;
    if session.selection.backend == MasterBackend::Rcli {
        return (session.selection.target_profile_ref.clone(), route.clone());
    }
    let target = member.id.clone();
    let request = crate::routing::RouteRequest {
        preferred_target: route.preferred_target.as_ref().map(|_| target.clone()),
        required_target: route.required_target.as_ref().map(|_| target.clone()),
        account_ref: member.account_ref.clone(),
        ..route.clone()
    };
    (target, request)
}

/// Names why one pool member cannot run a master turn now.
/// Input: the failure of that member's launch check. Output: the reason for the turn's
/// account list with the failure's code, or None when the failure is not about that
/// member and stops the turn.
fn member_gap(failure: &EngineError) -> Option<Gap> {
    let reason = match failure.0.as_str() {
        "master_authentication_unknown" => MasterReason::ReadinessUnknown,
        "master_authentication_stale" | "native_profile_identity_changed" => {
            MasterReason::ReadinessStale
        }
        "native_profile_in_use" => MasterReason::ProfileBusy,
        "native_profile_connection_missing"
        | "native_profile_not_managed"
        | "native_profile_binding_conflict" => MasterReason::ConnectionChanged,
        _ => return None,
    };
    Some((reason, cause_code(&failure.0)))
}

fn fails_over(reason: MasterReason) -> bool {
    matches!(
        reason,
        MasterReason::ReadinessUnknown
            | MasterReason::ReadinessStale
            | MasterReason::InvalidAuthentication
            | MasterReason::QuotaExhausted
            | MasterReason::RateLimited
            | MasterReason::CooldownActive
    )
}

fn pauses(reason: MasterReason) -> bool {
    matches!(
        reason,
        MasterReason::InvalidAuthentication
            | MasterReason::QuotaExhausted
            | MasterReason::RateLimited
            | MasterReason::CooldownActive
    )
}

/// Tells whether a pool account is paused after a limit, the pause sub-agents honour.
/// Inputs: the transaction and the account's connection. Output: true while paused.
fn paused(
    tx: &Transaction<'_>,
    connection: &crate::protocol::CredentialRef,
) -> Result<bool, EngineError> {
    tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM team_paused_accounts WHERE connection=?1 AND until_ms>?2)",
        params![connection.as_str(), signed(now_ms()?)?],
        |row| row.get(0),
    )
    .map_err(error)
}

/// Pauses a pool account whose turn hit a limit or a rejected sign-in, for as long as a
/// sub-agent's account pauses. Inputs: the transaction and the account's connection.
/// Output: the pause is stored; a longer pause already set is kept.
fn pause_member(
    tx: &Transaction<'_>,
    connection: &crate::protocol::CredentialRef,
) -> Result<(), EngineError> {
    let pause = crate::team::runtime::TeamLimits::default().rate_limit_pause;
    let until =
        signed(now_ms()?)?.saturating_add(i64::try_from(pause.as_millis()).unwrap_or(i64::MAX));
    let changed = tx
        .execute(
            "INSERT INTO team_paused_accounts(connection,until_ms) VALUES (?1,?2)
             ON CONFLICT(connection) DO UPDATE SET until_ms=MAX(until_ms,excluded.until_ms)",
            params![connection.as_str(), until],
        )
        .map_err(error)?;
    assert_eq!(changed, 1);
    Ok(())
}

/// Checks one member before a turn: settings, pause, then its launch check.
/// Inputs: the transaction, settings, host, session, custody, member and settings gap.
/// Output: the member's proof, or why it cannot run; other failures stop the turn.
fn member_auth(
    tx: &Transaction<'_>,
    config: &EffectiveConfig,
    host: &HostStamp,
    session: &MasterSessionView,
    custody: &mut Result<NativeProfileCustody, EngineError>,
    member: &ModelConnection,
    gap: Option<Gap>,
) -> Result<Result<MasterBootstrapAuth, Gap>, EngineError> {
    if let Some(gap) = gap {
        return Ok(Err(gap));
    }
    if paused(tx, &member.id)? {
        return Ok(Err((
            MasterReason::CooldownActive,
            "master_account_paused".to_owned(),
        )));
    }
    match authenticated(tx, config, host, session, member, custody) {
        Ok(auth) => Ok(Ok(auth)),
        Err(failure) => member_gap(&failure).map(Err).ok_or(failure),
    }
}

/// Lists the accounts of other harnesses in the master's pool that could run now.
/// Inputs: the transaction, settings, host and session. Output: each allowed, unpaused
/// native member of another harness whose latest sign-in passes the launch check.
fn other_ready(
    tx: &Transaction<'_>,
    config: &EffectiveConfig,
    host: &HostStamp,
    session: &MasterSessionView,
) -> Result<Vec<MasterAccountState>, EngineError> {
    let mut ready = Vec::new();
    for connection in pool_connections(config, &session.selection.pool_ref) {
        if crate::master_session::backend_member(session.selection.backend, connection)
            || !matches!(connection.auth, ModelAuthentication::NativeManaged { .. })
            || !allowed_destination(config, &connection.id)
            || paused(tx, &connection.id)?
        {
            continue;
        }
        if native_sign_in(tx, host, connection, Some(&connection.account_ref)).is_ok() {
            ready.push(MasterAccountState {
                account_ref: connection.account_ref.clone(),
                connection: connection.id.clone(),
                harness: connection.harness.clone(),
                reason: None,
            });
        }
    }
    Ok(ready)
}

/// Blocks a queued turn that no member of its harness can run, naming each member and
/// why plus the ready accounts of other harnesses. Inputs: the transaction, settings,
/// host, session, turn and the members' gaps in member order. Output: the blocked turn,
/// persisted with the owner-chosen member's reason and code, or ConnectionChanged with
/// native_profile_connection_missing when that member is gone.
fn block_on_gaps(
    tx: &Transaction<'_>,
    config: &EffectiveConfig,
    host: &HostStamp,
    session: &mut MasterSessionView,
    turn: &mut MasterTurnView,
    gaps: &[(&ModelConnection, Gap, bool)],
) -> Result<(), EngineError> {
    assert_eq!(turn.phase, MasterTurnPhase::Queued);
    let (reason, cause) = gaps.iter().find(|(_, _, chosen)| *chosen).map_or_else(
        || {
            (
                MasterReason::ConnectionChanged,
                "native_profile_connection_missing".to_owned(),
            )
        },
        |(_, gap, _)| gap.clone(),
    );
    let mut accounts: Vec<_> = gaps
        .iter()
        .map(|(connection, (reason, _), _)| MasterAccountState {
            account_ref: connection.account_ref.clone(),
            connection: connection.id.clone(),
            harness: connection.harness.clone(),
            reason: Some(*reason),
        })
        .collect();
    accounts.extend(other_ready(tx, config, host, session)?);
    assert!(accounts.len() <= crate::config::POOL_MEMBERS_MAX);
    assert!(!cause.is_empty());
    turn.phase = MasterTurnPhase::Blocked;
    turn.reason = Some(reason);
    turn.cause = Some(cause);
    turn.accounts = accounts;
    persist_runtime(tx, config, session, turn)
}

/// Names the native connection whose sign-in a master session's turns launch on.
/// Inputs: the transaction and the session. Output: that connection, or none for a
/// missing session or a backend that launches without a native sign-in.
pub(super) fn native_target(
    tx: &Transaction<'_>,
    session: &SessionId,
) -> Result<Option<crate::protocol::CredentialRef>, EngineError> {
    let Some(session) = load_session(tx, session, owner_revision(tx)?)? else {
        return Ok(None);
    };
    Ok((session.selection.backend != MasterBackend::Rcli)
        .then(|| session.selection.target_profile_ref.clone()))
}

/// Proves the latest sign-in of a native managed connection can authorize a launch on
/// this engine run, the same check every master turn uses. Input: the transaction, the
/// active host, the connection and, for a master, the account its route names. Output:
/// the authenticated sign-in fact, or the unknown, stale or in-use code.
pub(crate) fn native_sign_in(
    tx: &Transaction<'_>,
    host: &HostStamp,
    connection: &ModelConnection,
    account_ref: Option<&crate::protocol::CredentialRef>,
) -> Result<crate::engine::owner::native_auth::NativeAuthFact, EngineError> {
    let ModelAuthentication::NativeManaged { profile_ref } = &connection.auth else {
        return Err(error("native_profile_not_managed"));
    };
    let key = crate::memory::hash(profile_ref.as_str().as_bytes());
    type OperationRow = (String, i64, String, Vec<u8>, Vec<u8>);
    let row: Option<OperationRow> = tx.query_row(
        "SELECT id,generation,phase,request,fact FROM native_auth_operations WHERE profile_key=?1 ORDER BY generation DESC LIMIT 1",
        [&key], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
    ).optional().map_err(error)?;
    let (id, generation, phase, request, bytes) =
        row.ok_or_else(|| error("master_authentication_unknown"))?;
    if request.len() > 8192 {
        return Err(error("native_auth_request_bounds"));
    }
    let fact = crate::engine::owner::native_auth::decode_fact(&bytes)?;
    if let Some(gap) = sign_in_gap(fact.phase) {
        return Err(error(gap));
    }
    if !matches!(fact.action, NativeAuthAction::SignIn { .. }) {
        return Err(error("master_authentication_unknown"));
    }
    let start: NativeAuthStart =
        serde_json::from_slice(&request).map_err(|_| error("master_authentication_stale"))?;
    if fact.operation_id.as_str() != id
        || signed(fact.generation)? != generation
        || fact.phase.name() != phase
        || fact.phase != NativeAuthPhase::AuthenticatedObserved
        || fact.action
            != (NativeAuthAction::SignIn {
                method: start.method,
            })
        || start.request_id != fact.operation_id
        || start.scope.expected_database_instance != host.db_instance
        || start.scope.connection != connection.id
        || fact.binding.database_instance != host.db_instance
        || fact.binding.engine_epoch != host.engine_epoch
        || fact.binding.connection != *connection
        || account_ref.is_some_and(|account| &fact.binding.connection.account_ref != account)
        || fact.binding.at_revision.value() > owner_revision(tx)?
    {
        return Err(error("master_authentication_stale"));
    }
    let fenced: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM native_auth_fences WHERE profile_key=?1)",
            [&key],
            |row| row.get(0),
        )
        .map_err(error)?;
    if fenced {
        return Err(error("native_profile_in_use"));
    }
    Ok(fact)
}

fn ready_lease(
    tx: &Transaction<'_>,
    session: &MasterSessionView,
    auth: &MasterBootstrapAuth,
) -> Result<(RequestId, WorktreeLease), EngineError> {
    let (allocation, lease) = super::master_bootstrap::ready(tx, session, auth)?;
    if lease.project_id != session.project_id
        || lease.task_id != session.bootstrap_task_id
        || lease.owner_session != session.session_id
        || lease.native_descendant.is_some()
        || lease.state != LeaseState::Ready
        || !Path::new(&lease.repository).is_absolute()
        || !Path::new(&lease.path).is_absolute()
    {
        return Err(error("master_worktree_provenance"));
    }
    Ok((allocation, lease))
}

fn checked_prompt(
    session_id: &SessionId,
    turn_id: &RequestId,
    message: &str,
    brief: Option<&str>,
) -> Result<String, EngineError> {
    let length = match brief {
        Some(brief) => brief
            .len()
            .checked_add(2)
            .and_then(|length| length.checked_add(message.len()))
            .ok_or_else(|| error("master_input_too_large"))?,
        None => message.len(),
    };
    if length > 16384 {
        return Err(error("master_input_too_large"));
    }
    let mut text = String::with_capacity(length);
    if let Some(brief) = brief {
        text.push_str(brief);
        text.push_str("\n\n");
    }
    text.push_str(message);
    assert_eq!(text.len(), length);
    let control: crate::rcli_host::Control = serde_json::from_value(serde_json::json!({
        "protocol_version": crate::rcli_host::HOST_PROTOCOL_VERSION,
        "session_id": session_id.as_str(),
        "request_id": "rcli-control-4096",
        "control": { "kind": "prompt", "turn_id": turn_id.as_str(), "text": &text },
    }))
    .map_err(error)?;
    crate::rcli_host::encode_control(&control).map_err(|failure| {
        if failure.0 == "invalid frame length" {
            error("master_input_too_large")
        } else {
            error(failure)
        }
    })?;
    Ok(text)
}

fn prompt_context(
    tx: &Transaction<'_>,
    session: &MasterSessionView,
) -> Result<Option<String>, EngineError> {
    let brief: Option<String> = tx.query_row(
        "SELECT CASE WHEN length(CAST(text AS BLOB))<=8192 THEN text END FROM conversation_entries WHERE project=?1 AND kind='brief' ORDER BY brief_version DESC LIMIT 1",
        [session.project_id.as_str()], |row| row.get(0),
    ).optional().map_err(error)?;
    let context = match (crate::owner_profile::load(tx)?, brief) {
        (Some(profile), Some(brief)) => Some(format!("{}\n\n{brief}", profile.preface())),
        (Some(profile), None) => Some(profile.preface()),
        (None, brief) => brief,
    };
    Ok(context)
}

fn current_prompt(
    tx: &Transaction<'_>,
    session: &MasterSessionView,
    turn_id: &RequestId,
    message: &str,
) -> Result<String, EngineError> {
    let context = prompt_context(tx, session)?;
    checked_prompt(&session.session_id, turn_id, message, context.as_deref())
}

fn turn_message(
    tx: &Transaction<'_>,
    session: &MasterSessionView,
    turn: &MasterTurnView,
) -> Result<String, EngineError> {
    tx.query_row("SELECT CASE WHEN length(CAST(text AS BLOB))<=8192 THEN text END FROM conversation_entries WHERE project=?1 AND event_cursor=?2 AND kind='message' AND brief_version=0", params![session.project_id.as_str(), signed(turn.input_cursor.value())?], |row| row.get(0)).map_err(error)
}

const HISTORY_HEADER: &str = "Conversation so far, oldest first:";
const HISTORY_OMITTED: &str = "(Earlier turns were left out to fit.)";

/// Reads the earlier turns of this conversation that reached a model, oldest first, as
/// the entries the next prompt carries. Inputs: the transaction, session and current
/// turn. Output: one "Owner:" entry per turn with its reply or a no-reply line.
fn prior_turns(
    tx: &Transaction<'_>,
    session: &MasterSessionView,
    turn: &MasterTurnView,
) -> Result<Vec<String>, EngineError> {
    let mut statement = tx
        .prepare("SELECT CASE WHEN length(CAST(body AS BLOB))<=16384 THEN body END FROM master_turns WHERE session=?1 AND ordinal<?2 ORDER BY ordinal LIMIT 1024")
        .map_err(error)?;
    let bodies = statement
        .query_map(
            params![session.session_id.as_str(), signed(turn.ordinal)?],
            |row| row.get::<_, Option<String>>(0),
        )
        .map_err(error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(error)?;
    let mut entries = Vec::new();
    for body in bodies {
        let body = body.ok_or_else(|| error("master_turn_bounds"))?;
        let earlier: MasterTurnView = serde_json::from_str(&body).map_err(error)?;
        if earlier.session_id != session.session_id || earlier.ordinal >= turn.ordinal {
            return Err(error("master_turn_order"));
        }
        if !matches!(
            earlier.phase,
            MasterTurnPhase::Succeeded | MasterTurnPhase::Failed | MasterTurnPhase::Uncertain
        ) {
            continue;
        }
        let message = turn_message(tx, session, &earlier)?;
        entries.push(match &earlier.reply {
            Some(reply) => format!("Owner: {message}\nYou: {}", reply.text),
            None => format!("Owner: {message}\n(No reply was received.)"),
        });
    }
    assert!(entries.len() < usize::try_from(turn.ordinal).map_err(error)?);
    Ok(entries)
}

/// Joins the prompt context with the newest earlier turns from `start` on.
/// Inputs: the context, the earlier entries oldest first and the first kept index.
/// Output: the context the current message follows, or the bare context with no turns.
fn with_history(context: Option<&str>, entries: &[String], start: usize) -> Option<String> {
    if start >= entries.len() {
        return context.map(str::to_owned);
    }
    let mut text = String::new();
    if let Some(context) = context {
        text.push_str(context);
        text.push_str("\n\n");
    }
    text.push_str(HISTORY_HEADER);
    if start > 0 {
        text.push('\n');
        text.push_str(HISTORY_OMITTED);
    }
    for entry in &entries[start..] {
        text.push_str("\n\n");
        text.push_str(entry);
    }
    Some(text)
}

fn prompt(
    tx: &Transaction<'_>,
    session: &MasterSessionView,
    turn: &MasterTurnView,
) -> Result<String, EngineError> {
    let message = turn_message(tx, session, turn)?;
    let context = prompt_context(tx, session)?;
    let entries = prior_turns(tx, session, turn)?;
    let fixed = context.as_ref().map_or(0, |context| context.len() + 2)
        + HISTORY_HEADER.len()
        + HISTORY_OMITTED.len()
        + message.len()
        + 3;
    let budget = 16384usize.saturating_sub(fixed);
    let mut start = entries.len();
    let mut used = 0;
    while start > 0 && used + entries[start - 1].len() + 2 <= budget {
        used += entries[start - 1].len() + 2;
        start -= 1;
    }
    loop {
        let framed = with_history(context.as_deref(), &entries, start);
        match checked_prompt(&session.session_id, &turn.id, &message, framed.as_deref()) {
            Err(failure) if failure.0 == "master_input_too_large" && start < entries.len() => {
                start += 1;
            }
            result => return result,
        }
    }
}

fn preflight_turn(
    tx: &Transaction<'_>,
    config: &EffectiveConfig,
    session: &mut MasterSessionView,
    turn: &mut MasterTurnView,
) -> Result<bool, EngineError> {
    match prompt(tx, session, turn) {
        Ok(_) => Ok(true),
        Err(failure) if failure.0 == "master_input_too_large" => {
            let phase = match turn.phase {
                MasterTurnPhase::Queued => MasterTurnPhase::Blocked,
                MasterTurnPhase::Prepared => {
                    complete_local(tx, turn, MasterTurnPhase::Failed)?;
                    MasterTurnPhase::Failed
                }
                _ => return Err(failure),
            };
            turn.phase = phase;
            turn.reason = Some(MasterReason::CapacityUnavailable);
            turn.cause = Some(cause_code(&failure.0));
            persist_runtime(tx, config, session, turn)?;
            Ok(false)
        }
        Err(failure) => Err(failure),
    }
}

fn bundle(
    tx: &Transaction<'_>,
    session: MasterSessionView,
    turn: &MasterTurnView,
) -> Result<MasterRuntimeResult, EngineError> {
    let record = preparation(tx, &session, turn)?;
    let attempt = turn
        .attempt
        .clone()
        .ok_or_else(|| error("master_attempt_missing"))?;
    let instructions = match session.selection.backend {
        MasterBackend::Rcli => String::new(),
        MasterBackend::Codex | MasterBackend::Claude => {
            crate::launch_context::for_master(tx, &session.project_id)
                .map_err(|problem| EngineError(problem.0))?
        }
    };
    let prepared = PreparedMasterTurn {
        text: prompt(tx, &session, turn)?,
        instructions,
        session,
        attempt,
        profile: PreparedNativeProfile::from_binding(record.auth.binding()),
        repository: PathBuf::from(&record.lease.repository),
        cwd: PathBuf::from(&record.lease.path),
        worktree_lease: record.worktree_lease,
        lease: record.lease,
    };
    assert_eq!(
        prepared.attempt.request.task_id,
        turn_task_id(&prepared.session.session_id, &turn.id)?
    );
    Ok(MasterRuntimeResult::Prepared(Box::new(prepared)))
}

fn admission_reason(code: &str) -> MasterReason {
    match code {
        "gateway_health_unknown" | "gateway_initial_authentication_required" => {
            MasterReason::ReadinessUnknown
        }
        "gateway_initial_readiness_stale" | "gateway_initial_authentication_stale" => {
            MasterReason::ReadinessStale
        }
        "gateway_invalid_authentication" => MasterReason::InvalidAuthentication,
        "gateway_quota_exhausted" => MasterReason::QuotaExhausted,
        "gateway_quota_unknown" => MasterReason::QuotaUnknown,
        "gateway_rate_limited" => MasterReason::RateLimited,
        "gateway_cooldown" => MasterReason::CooldownActive,
        "gateway_capability_unavailable" | "gateway_incompatible_connection" => {
            MasterReason::UnsupportedCapability
        }
        "gateway_connection_changed_or_denied" | "gateway_route_denied" => {
            MasterReason::ConnectionChanged
        }
        "gateway_attempt_active" | "gateway_attempt_limit" => MasterReason::CapacityUnavailable,
        _ => MasterReason::RuntimeUnavailable,
    }
}

fn verify_preparation(
    tx: &Transaction<'_>,
    config: &EffectiveConfig,
    host: &HostStamp,
    session: &MasterSessionView,
    turn: &MasterTurnView,
) -> Result<Preparation, EngineError> {
    let record = preparation(tx, session, turn)?;
    if current_auth(tx, config, host, session, &record.auth.binding().connection)? != record.auth {
        return Err(error("master_authentication_stale"));
    }
    let (allocation, lease) = ready_lease(tx, session, &record.auth)?;
    if allocation != record.worktree_lease || lease != record.lease {
        return Err(error("master_worktree_provenance"));
    }
    Ok(record)
}

/// Blocks a queued turn that cannot run until the owner acts: no current
/// sign-in, or a master opened before the settings or engine changed.
/// Input: the failure that stopped the turn. Output: true when the turn was
/// blocked and persisted, false when the failure needs no owner action.
fn block_unready(
    tx: &Transaction<'_>,
    config: &EffectiveConfig,
    session: &mut MasterSessionView,
    turn: &mut MasterTurnView,
    failure: &EngineError,
) -> Result<bool, EngineError> {
    let reason = match failure.0.as_str() {
        "master_authentication_unknown" => MasterReason::ReadinessUnknown,
        "master_authentication_stale" => MasterReason::ReadinessStale,
        "native_profile_connection_missing" | "master_source_conflict" => {
            MasterReason::ConnectionChanged
        }
        _ => return Ok(false),
    };
    assert_eq!(turn.phase, MasterTurnPhase::Queued);
    turn.phase = MasterTurnPhase::Blocked;
    turn.reason = Some(reason);
    turn.cause = Some(cause_code(&failure.0));
    persist_runtime(tx, config, session, turn)?;
    Ok(true)
}

fn prepare_turn(
    tx: &Transaction<'_>,
    config: &EffectiveConfig,
    sources: Option<&settings::SettingsSources>,
    custody: &mut Result<NativeProfileCustody, EngineError>,
    host: &HostStamp,
    session: &mut MasterSessionView,
    turn: &mut MasterTurnView,
) -> Result<MasterRuntimeResult, EngineError> {
    current_scope(tx, config, sources, host, session, turn)?;
    if turn.phase == MasterTurnPhase::Prepared {
        verify_preparation(tx, config, host, session, turn)?;
        if !preflight_turn(tx, config, session, turn)? {
            return Ok(MasterRuntimeResult::Turn(Box::new(turn.clone())));
        }
        return bundle(tx, session.clone(), turn);
    }
    if turn.phase != MasterTurnPhase::Queued {
        return Ok(MasterRuntimeResult::Turn(Box::new(turn.clone())));
    }
    if !preflight_turn(tx, config, session, turn)? {
        return Ok(MasterRuntimeResult::Turn(Box::new(turn.clone())));
    }
    let mut gaps = Vec::new();
    for (member, gap, chosen) in candidates(config, session)? {
        let auth = match member_auth(tx, config, host, session, custody, member, gap)? {
            Ok(auth) => auth,
            Err(gap) => {
                gaps.push((member, gap, chosen));
                continue;
            }
        };
        let (worktree_lease, lease) = ready_lease(tx, session, &auth)?;
        let attempt = match admit(tx, config, session, turn, member, &auth)? {
            Ok(attempt) => attempt,
            Err(gap) if fails_over(gap.0) => {
                if pauses(gap.0) {
                    pause_member(tx, &member.id)?;
                }
                gaps.push((member, gap, chosen));
                continue;
            }
            Err((reason, cause)) => {
                turn.phase = MasterTurnPhase::Blocked;
                turn.reason = Some(reason);
                turn.cause = Some(cause);
                persist_runtime(tx, config, session, turn)?;
                return Ok(MasterRuntimeResult::Turn(Box::new(turn.clone())));
            }
        };
        if attempt.connection != auth.binding().connection {
            return Err(error("master_attempt_connection_conflict"));
        }
        let record = Preparation {
            attempt_id: attempt.request.id.clone(),
            auth,
            worktree_lease,
            lease,
        };
        let inserted = tx
            .execute(
                "INSERT INTO master_preparations(session,turn,body) VALUES (?1,?2,?3)",
                params![
                    session.session_id.as_str(),
                    turn.id.as_str(),
                    encode(&record)?
                ],
            )
            .map_err(error)?;
        assert_eq!(inserted, 1);
        turn.attempt = Some(attempt);
        turn.phase = MasterTurnPhase::Prepared;
        persist_runtime(tx, config, session, turn)?;
        current_scope(tx, config, sources, host, session, turn)?;
        return bundle(tx, session.clone(), turn);
    }
    block_on_gaps(tx, config, host, session, turn, &gaps)?;
    Ok(MasterRuntimeResult::Turn(Box::new(turn.clone())))
}

/// Asks the model gateway to admit a turn on one member, in a savepoint rolled back when
/// the gateway refuses. Inputs: the transaction, settings, session, turn, member and its
/// proof. Output: the admitted attempt, or the reason and code the gateway refused with.
fn admit(
    tx: &Transaction<'_>,
    config: &EffectiveConfig,
    session: &MasterSessionView,
    turn: &MasterTurnView,
    member: &ModelConnection,
    auth: &MasterBootstrapAuth,
) -> Result<Result<Attempt, Gap>, EngineError> {
    let (target_profile_ref, route) = member_request(session, member);
    let request = AttemptRequest {
        id: RequestId::new(format!("master-attempt-{}", random_id()?)).map_err(error)?,
        task_id: turn_task_id(&session.session_id, &turn.id)?,
        project_id: session.project_id.clone(),
        role: Role::ProjectLeader,
        pool_ref: session.selection.pool_ref.clone(),
        target_profile_ref,
        route,
        previous_attempt: None,
    };
    tx.execute_batch("SAVEPOINT master_admission")
        .map_err(error)?;
    let admitted = if request.route.required_capabilities.is_empty() {
        match auth {
            MasterBootstrapAuth::NativeSignIn(fact) => {
                model_gateway::begin_initial_conversation_attempt(
                    tx,
                    config,
                    config,
                    &request,
                    InitialConversationAuth {
                        fact,
                        binding: &fact.binding,
                        generation: fact.generation,
                    },
                    now_ms()?,
                )
            }
            MasterBootstrapAuth::RcliConfigured(proof) => {
                model_gateway::begin_configured_initial_conversation_attempt(
                    tx,
                    config,
                    config,
                    &request,
                    ConfiguredInitialConversationAuth {
                        proof,
                        binding: &auth.binding(),
                        generation: proof.generation,
                    },
                    now_ms()?,
                )
            }
        }
    } else {
        model_gateway::begin_attempt(tx, config, config, &request, now_ms()?)
    };
    match admitted {
        Ok(start) => {
            tx.execute_batch("RELEASE master_admission")
                .map_err(error)?;
            Ok(Ok(start.attempt))
        }
        Err(failure) => {
            tx.execute_batch("ROLLBACK TO master_admission; RELEASE master_admission")
                .map_err(error)?;
            Ok(Err((admission_reason(&failure.0), cause_code(&failure.0))))
        }
    }
}

/// Names the gateway task of one owner turn, so every turn starts its own attempt chain
/// and a failed or stopped turn never blocks the next one.
/// Inputs: the master session id and the turn's request id.
/// Output: a stable task id derived from both, distinct for every turn of every session.
pub(crate) fn turn_task_id(session: &SessionId, turn: &RequestId) -> Result<TaskId, EngineError> {
    let digest = crate::memory::hash(format!("{}\n{}", session.as_str(), turn.as_str()).as_bytes());
    let task = TaskId::new(format!("master-turn-{digest}")).map_err(error)?;
    assert!(task.as_str().starts_with("master-turn-"));
    Ok(task)
}

fn observe_turn(
    tx: &Transaction<'_>,
    config: &EffectiveConfig,
    session: &mut MasterSessionView,
    turn: &mut MasterTurnView,
    observation: MasterRuntimeObservation,
) -> Result<(), EngineError> {
    if turn.phase == MasterTurnPhase::Cancelled {
        return Ok(());
    }
    match observation {
        MasterRuntimeObservation::Running { runtime } => {
            validate_runtime(&runtime)?;
            if let Some(previous) = &turn.runtime {
                if previous.runtime_ref != runtime.runtime_ref
                    || previous.artifact_sha256 != runtime.artifact_sha256
                    || previous
                        .native_session_id
                        .as_ref()
                        .is_some_and(|id| runtime.native_session_id.as_ref() != Some(id))
                {
                    return Err(error("master_runtime_conflict"));
                }
                if previous == &runtime && turn.phase == MasterTurnPhase::Running {
                    return Ok(());
                }
            }
            if !matches!(
                turn.phase,
                MasterTurnPhase::Delivering | MasterTurnPhase::Running
            ) {
                return Err(error("master_runtime_transition"));
            }
            turn.runtime = Some(runtime);
            turn.phase = MasterTurnPhase::Running;
        }
        MasterRuntimeObservation::Response { reply } => {
            if turn.phase == MasterTurnPhase::Succeeded && turn.reply.as_ref() == Some(&reply) {
                return Ok(());
            }
            if turn.phase != MasterTurnPhase::Running {
                return Err(error("master_runtime_transition"));
            }
            let attempt = turn
                .attempt
                .as_ref()
                .ok_or_else(|| error("master_attempt_missing"))?;
            let runtime = turn
                .runtime
                .as_ref()
                .ok_or_else(|| error("master_runtime_missing"))?;
            if reply.text.trim().is_empty()
                || reply.text.len() > 8192
                || reply.text.contains('\0')
                || reply.provenance.attempt_id != attempt.request.id
                || reply.provenance.runtime_ref != runtime.runtime_ref
                || reply.provenance.capture_sequence == 0
            {
                return Err(error("master_reply_provenance"));
            }
            let completion = AttemptCompletion {
                outcome: AttemptOutcome::Succeeded,
                effects: Effects::Committed,
                finished_at_ms: now_ms()?.max(attempt.selected_at_ms),
                retry_at_ms: None,
            };
            model_gateway::finish_attempt(tx, &attempt.request.id, &completion)?;
            turn.attempt = model_gateway::read_attempt(tx, &attempt.request.id)?;
            turn.reply = Some(reply);
            turn.phase = MasterTurnPhase::Succeeded;
        }
        MasterRuntimeObservation::Failed { reason, cause } => {
            if turn.phase == MasterTurnPhase::Failed && turn.reason == Some(reason) {
                return Ok(());
            }
            if !matches!(
                turn.phase,
                MasterTurnPhase::Prepared | MasterTurnPhase::Delivering | MasterTurnPhase::Running
            ) {
                return Err(error("master_runtime_transition"));
            }
            let provider = match reason {
                MasterReason::InvalidAuthentication => Some(FailureKind::InvalidAuthentication),
                MasterReason::QuotaExhausted => Some(FailureKind::QuotaExhausted),
                MasterReason::RateLimited => Some(FailureKind::RateLimited),
                _ => None,
            };
            if let Some(kind) = provider {
                let attempt = turn
                    .attempt
                    .as_ref()
                    .ok_or_else(|| error("master_attempt_missing"))?;
                let completion = AttemptCompletion {
                    outcome: AttemptOutcome::Failed(kind),
                    effects: Effects::Uncertain,
                    finished_at_ms: now_ms()?.max(attempt.selected_at_ms),
                    retry_at_ms: None,
                };
                model_gateway::finish_attempt(tx, &attempt.request.id, &completion)?;
                turn.attempt = model_gateway::read_attempt(tx, &attempt.request.id)?;
            } else {
                complete_local(tx, turn, MasterTurnPhase::Failed)?;
            }
            turn.phase = MasterTurnPhase::Failed;
            turn.reason = Some(reason);
            turn.cause = Some(cause_code(&cause));
            if pauses(reason)
                && let Some(attempt) = &turn.attempt
            {
                pause_member(tx, &attempt.connection.id)?;
            }
        }
        MasterRuntimeObservation::Cancelled => {
            if !matches!(
                turn.phase,
                MasterTurnPhase::Prepared
                    | MasterTurnPhase::Delivering
                    | MasterTurnPhase::Running
                    | MasterTurnPhase::Uncertain
            ) {
                return Err(error("master_runtime_transition"));
            }
            complete_local(tx, turn, MasterTurnPhase::Cancelled)?;
            turn.phase = MasterTurnPhase::Cancelled;
            turn.reason = Some(MasterReason::Cancelled);
        }
        MasterRuntimeObservation::Disconnected => {
            if turn.phase == MasterTurnPhase::Uncertain {
                return Ok(());
            }
            if !matches!(
                turn.phase,
                MasterTurnPhase::Prepared | MasterTurnPhase::Delivering | MasterTurnPhase::Running
            ) {
                return Err(error("master_runtime_transition"));
            }
            complete_local(tx, turn, MasterTurnPhase::Uncertain)?;
            turn.phase = MasterTurnPhase::Uncertain;
            turn.reason = Some(MasterReason::CaptureIncomplete);
        }
    }
    persist_runtime(tx, config, session, turn)
}

fn provision_bootstrap(
    db: &mut Connection,
    config: &EffectiveConfig,
    sources: Option<&settings::SettingsSources>,
    custody: &mut Result<NativeProfileCustody, EngineError>,
    path: &Path,
    command: &MasterRuntimeCommand,
) -> Result<(), EngineError> {
    let MasterRuntimeCommand::Prepare {
        host,
        project_id,
        session_id,
        turn_id,
    } = command
    else {
        return Ok(());
    };
    let tx = db.transaction().map_err(error)?;
    let (mut session, mut turn) = load_runtime_turn(&tx, host, project_id, session_id, turn_id)?;
    current_scope(&tx, config, sources, host, &session, &turn)?;
    if turn.phase != MasterTurnPhase::Queued {
        return Ok(());
    }
    if !preflight_turn(&tx, config, &mut session, &mut turn)? {
        tx.commit().map_err(error)?;
        return Ok(());
    }
    let mut gaps = Vec::new();
    let mut ready = None;
    for (member, gap, chosen) in candidates(config, &session)? {
        match member_auth(&tx, config, host, &session, custody, member, gap)? {
            Ok(auth) => {
                ready = Some(auth);
                break;
            }
            Err(gap) => gaps.push((member, gap, chosen)),
        }
    }
    let Some(auth) = ready else {
        block_on_gaps(&tx, config, host, &mut session, &mut turn, &gaps)?;
        tx.commit().map_err(error)?;
        return Ok(());
    };
    match ready_lease(&tx, &session, &auth) {
        Ok(_) => return Ok(()),
        Err(failure) if failure.0 == "master_bootstrap_receipt_missing" => {}
        Err(failure) => return Err(failure),
    }
    let repository = master_repository(&tx, project_id)?;
    let runtime_dir = path
        .parent()
        .ok_or_else(|| error("master_runtime_directory_missing"))?;
    super::master_bootstrap::reserve(&tx, runtime_dir, &session, &repository, &auth)?;
    tx.commit().map_err(error)?;
    assert!(db.is_autocommit());
    crate::worktrees::provision(db, &session.bootstrap_task_id, &repository)?;
    assert!(db.is_autocommit());
    Ok(())
}

/// Reads the repository a project's master works in: the first one the project lists.
/// Inputs: the transaction and the project. Output: the repository path, or
/// `master_repository_missing` when the project lists none.
fn master_repository(tx: &Transaction<'_>, project_id: &ProjectId) -> Result<String, EngineError> {
    let body: String = tx
        .query_row(
            "SELECT body FROM projects WHERE id=?1",
            [project_id.as_str()],
            |row| row.get(0),
        )
        .map_err(error)?;
    let project: StoredProject = serde_json::from_str(&body).map_err(error)?;
    project.spec.validate().map_err(error)?;
    if project.project_id != *project_id {
        return Err(error("master_project_scope_conflict"));
    }
    project
        .spec
        .repositories
        .first()
        .cloned()
        .ok_or_else(|| error("master_repository_missing"))
}

fn registration_session(
    tx: &Transaction<'_>,
    config: &EffectiveConfig,
    sources: Option<&settings::SettingsSources>,
    host: &HostStamp,
    project_id: &ProjectId,
    session_id: &SessionId,
) -> Result<MasterSessionView, EngineError> {
    check_owner_host(tx, host)?;
    settings::ensure_clear(tx)?;
    let session = load_session(tx, session_id, owner_revision(tx)?)?
        .ok_or_else(|| error("master_session_missing"))?;
    check_owner_admission(
        tx,
        &Admission {
            db_instance: host.db_instance.clone(),
            engine_epoch: host.engine_epoch.clone(),
            owner_id: session.owner.owner_id.clone(),
            owner_epoch: session.owner.resulting_epoch,
        },
    )?;
    if session.project_id != *project_id
        || session.phase != MasterSessionPhase::Configured
        || session.owner.engine_epoch != host.engine_epoch
        || session.config_id != config.id
        || session.source_hash != settings::verified_source_hash(config, sources)?
    {
        return Err(error("master_source_conflict"));
    }
    configured_connection(config, &session)?;
    Ok(session)
}

fn produce_rcli_profile(
    db: &Connection,
    custody: &mut Result<NativeProfileCustody, EngineError>,
    host: &HostStamp,
    session: &MasterSessionView,
    connection: ModelConnection,
    configured_home: &Path,
    at_revision: Cursor,
) -> Result<RcliConfiguredProfileProof, EngineError> {
    assert!(db.is_autocommit());
    let metadata = custody
        .as_mut()
        .map_err(|failure| failure.clone())?
        .register_configured_rcli_profile(
            host,
            &connection,
            &session.selection.target_profile_ref,
            configured_home,
        )?;
    if metadata.profile_ref != session.selection.target_profile_ref {
        return Err(error("master_configured_receipt_conflict"));
    }
    let proof = RcliConfiguredProfileProof {
        registration_id: RequestId::new(format!("master-profile-{}", random_id()?))
            .map_err(error)?,
        generation: metadata.profile_generation,
        database_instance: host.db_instance.clone(),
        engine_epoch: host.engine_epoch.clone(),
        config_id: session.config_id.clone(),
        source_hash: session.source_hash.clone(),
        at_revision,
        connection,
        profile_ref: metadata.profile_ref,
        custody_id: metadata.custody_id,
        profile_config_sha256: metadata.profile_config_sha256,
    };
    if proof.generation == 0 || !is_hex_id(&proof.profile_config_sha256) {
        return Err(error("master_configured_receipt_conflict"));
    }
    Ok(proof)
}

fn publish_rcli_profile(
    tx: &Transaction<'_>,
    config: &EffectiveConfig,
    sources: &settings::SettingsSources,
    host: &HostStamp,
    expected: &MasterSessionView,
    mut proof: RcliConfiguredProfileProof,
) -> Result<RcliConfiguredProfileProof, EngineError> {
    let session = registration_session(
        tx,
        config,
        Some(sources),
        host,
        &expected.project_id,
        &expected.session_id,
    )?;
    if session != *expected
        || proof.profile_ref != session.selection.target_profile_ref
        || configured_connection(config, &session)? != &proof.connection
    {
        return Err(error("master_configured_receipt_conflict"));
    }
    let (global_source, project_source) = sources.retained_paths();
    if let Some(record) = registration(tx, &session.session_id)? {
        validate_registration(tx, &session, &record)?;
        if record.proof.connection != proof.connection
            || record.proof.custody_id != proof.custody_id
            || record.proof.profile_config_sha256 != proof.profile_config_sha256
            || record.proof.generation != proof.generation
            || record.global_source != global_source
            || record.project_source != project_source
        {
            return Err(error("master_configured_receipt_conflict"));
        }
        return Ok(record.proof);
    }
    let counted = || -> Result<i64, EngineError> {
        tx.query_row(
            "SELECT COUNT(*) FROM configured_master_profiles",
            [],
            |row| row.get(0),
        )
        .map_err(error)
    };
    let mut count = counted()?;
    if count >= SESSIONS_MAX as i64 {
        tx.execute(
            "DELETE FROM configured_master_profiles WHERE rowid IN (SELECT p.rowid FROM configured_master_profiles p WHERE NOT EXISTS (SELECT 1 FROM master_sessions s WHERE s.id=p.session AND s.phase='configured') ORDER BY p.rowid LIMIT ?1)",
            [count - (SESSIONS_MAX as i64 - 1)],
        )
        .map_err(error)?;
        count = counted()?;
    }
    if count >= SESSIONS_MAX as i64 {
        return Err(error("master_configured_receipt_limit"));
    }
    let database_path = PathBuf::from(
        tx.path()
            .ok_or_else(|| error("master_runtime_directory_missing"))?,
    );
    let database_metadata = std::fs::symlink_metadata(&database_path).map_err(error)?;
    proof.at_revision = Cursor::new(owner_revision(tx)?);
    let record = ConfiguredRegistration {
        proof: proof.clone(),
        session_id: session.session_id.clone(),
        project_id: session.project_id.clone(),
        bootstrap_task_id: session.bootstrap_task_id.clone(),
        owner: session.owner.clone(),
        global_source,
        project_source,
        database_path,
        database_identity: database_identity(&database_metadata)?,
    };
    validate_registration(tx, &session, &record)?;
    let inserted = tx
        .execute(
            "INSERT INTO configured_master_profiles(id,session,body) VALUES (?1,?2,?3)",
            params![
                proof.registration_id.as_str(),
                session.session_id.as_str(),
                encode(&record)?
            ],
        )
        .map_err(error)?;
    assert_eq!(inserted, 1);
    Ok(proof)
}

fn adopt_account_profile(
    tx: &Transaction<'_>,
    config: &EffectiveConfig,
    sources: &settings::SettingsSources,
    custody: &mut Result<NativeProfileCustody, EngineError>,
    host: &HostStamp,
    session: &MasterSessionView,
) -> Result<(), EngineError> {
    if session.selection.backend != MasterBackend::Rcli {
        return Ok(());
    }
    let exists: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name='account_profile_operations')",
        [], |row| row.get(0),
    ).map_err(error)?;
    if !exists {
        return Ok(());
    }
    let connection = match configured_connection(config, session) {
        Ok(connection) => connection,
        Err(failure) if failure.0 == "rcli_profile_configuration_required" => return Ok(()),
        Err(failure) => return Err(failure),
    };
    let mut statement = tx.prepare(
        "SELECT request_id,CASE WHEN length(request)<=8192 THEN request END,CASE WHEN length(receipt)<=16384 THEN receipt END FROM account_profile_operations WHERE owner_id=?1 AND owner_epoch=?2 AND phase='committed' ORDER BY request_id LIMIT 1025",
    ).map_err(error)?;
    let mut rows = statement
        .query(params![
            session.owner.owner_id,
            session.owner.resulting_epoch
        ])
        .map_err(error)?;
    let mut receipt: Option<AccountProfileReceipt> = None;
    let mut count = 0;
    while let Some(row) = rows.next().map_err(error)? {
        count += 1;
        if count > 1024 {
            return Err(error("account_profile_operations_full"));
        }
        let id: String = row.get(0).map_err(error)?;
        let request_bytes: Vec<u8> = row.get(1).map_err(error)?;
        let receipt_bytes: Vec<u8> = row.get(2).map_err(error)?;
        let request: AccountProfileRequest =
            serde_json::from_slice(&request_bytes).map_err(error)?;
        let candidate: AccountProfileReceipt =
            serde_json::from_slice(&receipt_bytes).map_err(error)?;
        crate::engine::owner::account_profile::authorize_account_profile_scope(
            &request.scope,
            &host.db_instance,
            &candidate.config_id,
            &candidate.source_hash,
            &candidate.connection,
        )?;
        if request.request_id.as_str() != id
            || candidate.profile_generation == 0
            || !is_hex_id(candidate.config_id.as_str())
            || !is_hex_id(&candidate.source_hash)
            || !is_hex_id(&candidate.custody_id)
            || !is_hex_id(&candidate.profile_config_sha256)
        {
            return Err(error("account_profile_record_invalid"));
        }
        if candidate.connection != *connection
            || candidate.profile_ref != session.selection.target_profile_ref
        {
            continue;
        }
        if receipt.as_ref().is_some_and(|previous| {
            previous.profile_generation == candidate.profile_generation
                && (previous.custody_id != candidate.custody_id
                    || previous.profile_config_sha256 != candidate.profile_config_sha256)
        }) {
            return Err(error("master_configured_receipt_conflict"));
        }
        if receipt
            .as_ref()
            .is_some_and(|previous| previous.profile_generation >= candidate.profile_generation)
        {
            continue;
        }
        receipt = Some(candidate);
    }
    let Some(receipt) = receipt else {
        return Ok(());
    };
    let metadata = custody
        .as_mut()
        .map_err(|failure| failure.clone())?
        .configured_rcli_profile(host, connection, &session.selection.target_profile_ref)?;
    if metadata.profile_ref != receipt.profile_ref
        || metadata.custody_id != receipt.custody_id
        || metadata.profile_config_sha256 != receipt.profile_config_sha256
        || metadata.profile_generation != receipt.profile_generation
    {
        return Err(error("master_configured_receipt_stale"));
    }
    let proof = RcliConfiguredProfileProof {
        registration_id: RequestId::new(format!("master-profile-{}", random_id()?))
            .map_err(error)?,
        generation: metadata.profile_generation,
        database_instance: host.db_instance.clone(),
        engine_epoch: host.engine_epoch.clone(),
        config_id: session.config_id.clone(),
        source_hash: session.source_hash.clone(),
        at_revision: Cursor::new(owner_revision(tx)?),
        connection: connection.clone(),
        profile_ref: metadata.profile_ref,
        custody_id: metadata.custody_id,
        profile_config_sha256: metadata.profile_config_sha256,
    };
    publish_rcli_profile(tx, config, sources, host, session, proof)?;
    Ok(())
}

struct RcliRegistration<'a> {
    host: &'a HostStamp,
    project_id: &'a ProjectId,
    session_id: &'a SessionId,
    configured_home: &'a Path,
}

fn register_rcli_profile(
    db: &mut Connection,
    config: &EffectiveConfig,
    sources: Option<&settings::SettingsSources>,
    custody: &mut Result<NativeProfileCustody, EngineError>,
    registration: RcliRegistration<'_>,
) -> Result<RcliConfiguredProfileProof, EngineError> {
    let host = registration.host;
    let sources = sources.ok_or_else(|| error("master_settings_sources_required"))?;
    let tx = db.transaction().map_err(error)?;
    let original = registration_session(
        &tx,
        config,
        Some(sources),
        host,
        registration.project_id,
        registration.session_id,
    )?;
    let connection = configured_connection(config, &original)?.clone();
    let revision = Cursor::new(owner_revision(&tx)?);
    tx.commit().map_err(error)?;
    let candidate = produce_rcli_profile(
        db,
        custody,
        host,
        &original,
        connection,
        registration.configured_home,
        revision,
    )?;
    let tx = db.transaction().map_err(error)?;
    let proof = publish_rcli_profile(&tx, config, sources, host, &original, candidate)?;
    tx.commit().map_err(error)?;
    Ok(proof)
}

/// Registers the engine-generated SDK home for an unregistered configured RCLI master.
/// Inputs: the actor database, settings, custody and the session being prepared.
/// Output: Ok after registering, or Ok without change when the session is not an RCLI
/// master awaiting registration or account setup has not generated its home yet.
fn register_generated_rcli_profile(
    db: &mut Connection,
    config: &EffectiveConfig,
    sources: Option<&settings::SettingsSources>,
    custody: &mut Result<NativeProfileCustody, EngineError>,
    host: &HostStamp,
    project_id: &ProjectId,
    session_id: &SessionId,
) -> Result<(), EngineError> {
    let tx = db.transaction().map_err(error)?;
    let Some(session) = load_session(&tx, session_id, owner_revision(&tx)?)? else {
        return Ok(());
    };
    if session.project_id != *project_id
        || session.phase != MasterSessionPhase::Configured
        || session.selection.backend != MasterBackend::Rcli
        || registration(&tx, session_id)?.is_some()
    {
        return Ok(());
    }
    let Ok(connection) = configured_connection(config, &session) else {
        return Ok(());
    };
    let profile_ref = super::account_profile::configured_profile_ref(connection)?;
    drop(tx);
    let home = custody
        .as_ref()
        .map_err(|failure| failure.clone())?
        .generated_configured_home(&profile_ref);
    if !home.join(".backboard/config.json").is_file() {
        return Ok(());
    }
    let registration = RcliRegistration {
        host,
        project_id,
        session_id,
        configured_home: &home,
    };
    register_rcli_profile(db, config, sources, custody, registration)?;
    assert!(db.is_autocommit());
    Ok(())
}

fn rcli_registration_scope(
    tx: &Transaction<'_>,
    config: &EffectiveConfig,
    sources: &settings::SettingsSources,
    host: &HostStamp,
    admission: &Admission,
    registration: &RcliProfileRegistration,
) -> Result<MasterSessionView, EngineError> {
    let expected = &registration.expected_session;
    let session = registration_session(
        tx,
        config,
        Some(sources),
        host,
        &expected.project_id,
        &expected.session_id,
    )?;
    if session != *expected
        || session.owner.owner_id != admission.owner_id
        || session.owner.resulting_epoch != admission.owner_epoch
        || session.owner.engine_epoch != admission.engine_epoch
        || host.db_instance != admission.db_instance
        || host.engine_epoch != admission.engine_epoch
        || configured_connection(config, &session)? != &registration.expected_connection
    {
        return Err(error("master_configured_receipt_conflict"));
    }
    Ok(session)
}

fn record_rcli_registration(
    tx: &Transaction<'_>,
    config: &EffectiveConfig,
    admission: &Admission,
    request: &OwnerRequest,
    result: OwnerResult,
) -> Result<OwnerApplied, EngineError> {
    let registered = match &result {
        OwnerResult::RcliProfileRegistered { .. } => true,
        OwnerResult::Rejected { .. } => false,
        _ => return Err(error("master_mutation_result")),
    };
    let revision = owner_revision(tx)?;
    let next = if registered {
        let next = revision
            .checked_add(1)
            .ok_or_else(|| error("revision_exhausted"))?;
        signed(next)?;
        persist_event_ids(
            tx,
            config,
            &request.request_id,
            &ClientId::new(format!("owner:{}", admission.owner_id)).map_err(error)?,
            next,
        )?;
        next
    } else {
        revision
    };
    let provenance = OwnerProvenance {
        origin: "native_owner".into(),
        owner_id: admission.owner_id.clone(),
        operation_id: request.request_id.clone(),
        request_hash: request.mutation_hash()?,
        engine_epoch: admission.engine_epoch.clone(),
        admitted_epoch: admission.owner_epoch,
        resulting_epoch: admission.owner_epoch,
        revision_before: Cursor::new(revision),
        revision_after: Cursor::new(next),
        event_cursor: registered.then_some(Cursor::new(next)),
    };
    let hash = Sha256::digest(serde_json::to_vec(request).map_err(error)?);
    owner_record(
        tx,
        &provenance,
        &hash,
        &serde_json::to_vec(&result).map_err(error)?,
    )?;
    assert_eq!(owner_revision(tx)?, provenance.revision_after.value());
    Ok(OwnerApplied {
        result,
        provenance,
        historical_replay: false,
        current_revision: Cursor::new(next),
    })
}

pub(super) fn apply_rcli_profile_registration(
    db: &mut Connection,
    config: &EffectiveConfig,
    sources: Option<&settings::SettingsSources>,
    custody: &mut Result<NativeProfileCustody, EngineError>,
    host: &HostStamp,
    admission: &Admission,
    request: &OwnerRequest,
) -> Result<OwnerApplied, EngineError> {
    request.validate()?;
    let OwnerOperation::Apply {
        mutation: OwnerMutation::ProjectMaster { action },
    } = &request.operation
    else {
        return Err(error("master_action_required"));
    };
    let MasterOwnerAction::RegisterRcliProfile { registration } = action else {
        return Err(error("master_action_required"));
    };
    validate_action(action)?;
    let hash_text = request.mutation_hash()?;
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(error)?;
    check_owner_admission(&tx, admission)?;
    check_owner_host(&tx, host)?;
    let revision = owner_revision(&tx)?;
    if let Some((provenance, result)) = owner_lookup(
        &tx,
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
    if request.expected_revision.value() != revision {
        let applied = record_rcli_registration(
            &tx,
            config,
            admission,
            request,
            OwnerResult::Rejected {
                code: "stale_revision".into(),
            },
        )?;
        tx.commit().map_err(error)?;
        return Ok(applied);
    }
    let sources = sources.ok_or_else(|| error("master_settings_sources_required"))?;
    let preflight = (|| -> Result<MasterSessionView, EngineError> {
        let session = rcli_registration_scope(&tx, config, sources, host, admission, registration)?;
        let config_path = registration.configured_home.join(".backboard/config.json");
        match std::fs::symlink_metadata(&config_path) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(error("master_rcli_config_symlink"));
            }
            Ok(_) => {}
            Err(failure) if failure.kind() == std::io::ErrorKind::NotFound => {
                return Err(error("master_rcli_config_missing"));
            }
            Err(failure) => return Err(error(failure)),
        }
        if let Some(record) = self::registration(&tx, &session.session_id)? {
            validate_registration(&tx, &session, &record)?;
        }
        Ok(session)
    })();
    let original = match preflight {
        Ok(session) => session,
        Err(failure)
            if matches!(
                failure.0.as_str(),
                "master_source_conflict"
                    | "master_session_missing"
                    | "master_configured_receipt_conflict"
                    | "master_configured_receipt_stale"
                    | "master_configured_database_changed"
                    | "settings_source_conflict"
                    | "master_rcli_config_missing"
                    | "master_rcli_config_symlink"
                    | "rcli_profile_identity_changed"
                    | "rcli_profile_config_changed"
                    | "rcli_profile_binding_conflict"
            ) =>
        {
            let applied = record_rcli_registration(
                &tx,
                config,
                admission,
                request,
                OwnerResult::Rejected { code: failure.0 },
            )?;
            tx.commit().map_err(error)?;
            return Ok(applied);
        }
        Err(failure) => return Err(failure),
    };
    let connection = configured_connection(config, &original)?.clone();
    tx.commit().map_err(error)?;
    let candidate = produce_rcli_profile(
        db,
        custody,
        host,
        &original,
        connection,
        &registration.configured_home,
        Cursor::new(revision),
    )?;

    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(error)?;
    check_owner_admission(&tx, admission)?;
    check_owner_host(&tx, host)?;
    let revision = owner_revision(&tx)?;
    if let Some((provenance, result)) = owner_lookup(
        &tx,
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
    let prepared = (|| -> Result<OwnerApplied, EngineError> {
        let result = if request.expected_revision.value() != revision {
            OwnerResult::Rejected {
                code: "stale_revision".into(),
            }
        } else {
            let session =
                rcli_registration_scope(&tx, config, sources, host, admission, registration)?;
            let proof = publish_rcli_profile(&tx, config, sources, host, &session, candidate)?;
            OwnerResult::RcliProfileRegistered {
                registration: Box::new(RcliProfileRegistrationMetadata {
                    project_id: session.project_id,
                    session_id: session.session_id,
                    profile_ref: proof.profile_ref,
                    registration_id: proof.registration_id,
                    generation: proof.generation,
                }),
            }
        };
        record_rcli_registration(&tx, config, admission, request, result)
    })();
    let applied = match prepared {
        Ok(applied) => applied,
        Err(failure) => {
            tx.rollback()
                .map_err(|_| error("master_rollback_outcome_unknown"))?;
            return Err(failure);
        }
    };
    tx.commit().map_err(error)?;
    Ok(applied)
}

pub(super) fn runtime(
    db: &mut Connection,
    config: &EffectiveConfig,
    sources: Option<&settings::SettingsSources>,
    custody: &mut Result<NativeProfileCustody, EngineError>,
    path: &Path,
    command: MasterRuntimeCommand,
) -> Result<MasterRuntimeResult, EngineError> {
    #[cfg(all(test, unix))]
    if let MasterRuntimeCommand::RegisterRcliProfile {
        host,
        project_id,
        session_id,
        configured_home,
    } = command
    {
        let registration = RcliRegistration {
            host: &host,
            project_id: &project_id,
            session_id: &session_id,
            configured_home: &configured_home,
        };
        return register_rcli_profile(db, config, sources, custody, registration)
            .map(|proof| MasterRuntimeResult::ConfiguredProfile(Box::new(proof)));
    }
    if let MasterRuntimeCommand::Prepare {
        host,
        project_id,
        session_id,
        ..
    } = &command
    {
        register_generated_rcli_profile(
            db, config, sources, custody, host, project_id, session_id,
        )?;
    }
    provision_bootstrap(db, config, sources, custody, path, &command)?;
    let tx = db.transaction().map_err(error)?;
    let (host, project_id, session_id, turn_id) = match &command {
        #[cfg(all(test, unix))]
        MasterRuntimeCommand::RegisterRcliProfile { .. } => unreachable!(),
        MasterRuntimeCommand::Route {
            host,
            project_id,
            session_id,
            turn_id,
        }
        | MasterRuntimeCommand::Prepare {
            host,
            project_id,
            session_id,
            turn_id,
        }
        | MasterRuntimeCommand::Inspect {
            host,
            project_id,
            session_id,
            turn_id,
        }
        | MasterRuntimeCommand::AcquireProfile {
            host,
            project_id,
            session_id,
            turn_id,
            ..
        }
        | MasterRuntimeCommand::Observe {
            host,
            project_id,
            session_id,
            turn_id,
            ..
        }
        | MasterRuntimeCommand::Fail {
            host,
            project_id,
            session_id,
            turn_id,
            ..
        } => (host, project_id, session_id, turn_id),
    };
    let (mut session, mut turn) = load_runtime_turn(&tx, host, project_id, session_id, turn_id)?;
    match command {
        #[cfg(all(test, unix))]
        MasterRuntimeCommand::RegisterRcliProfile { .. } => unreachable!(),
        MasterRuntimeCommand::Route { ref host, .. } => {
            if let Err(failure) = current_scope(&tx, config, sources, host, &session, &turn) {
                if turn.phase != MasterTurnPhase::Queued
                    || !block_unready(&tx, config, &mut session, &mut turn, &failure)?
                {
                    return Err(failure);
                }
                tx.commit().map_err(error)?;
                return Ok(MasterRuntimeResult::Turn(Box::new(turn)));
            }
            tx.commit().map_err(error)?;
            Ok(MasterRuntimeResult::Turn(Box::new(turn)))
        }
        MasterRuntimeCommand::Prepare { ref host, .. } => {
            let result =
                prepare_turn(&tx, config, sources, custody, host, &mut session, &mut turn)?;
            tx.commit().map_err(error)?;
            Ok(result)
        }
        MasterRuntimeCommand::Inspect { ref host, .. } => {
            current_scope(&tx, config, sources, host, &session, &turn)?;
            if turn.attempt.is_some()
                && matches!(
                    turn.phase,
                    MasterTurnPhase::Prepared
                        | MasterTurnPhase::Delivering
                        | MasterTurnPhase::Running
                )
            {
                verify_preparation(&tx, config, host, &session, &turn)?;
            }
            tx.commit().map_err(error)?;
            Ok(MasterRuntimeResult::Turn(Box::new(turn)))
        }
        MasterRuntimeCommand::AcquireProfile {
            ref host,
            ref attempt_id,
            ..
        } => {
            if turn.attempt.as_ref().map(|attempt| &attempt.request.id) != Some(attempt_id) {
                return Err(error("master_attempt_identity_conflict"));
            }
            if turn.phase != MasterTurnPhase::Prepared {
                tx.commit().map_err(error)?;
                return Ok(MasterRuntimeResult::Turn(Box::new(turn)));
            }
            current_scope(&tx, config, sources, host, &session, &turn)?;
            let record = verify_preparation(&tx, config, host, &session, &turn)?;
            turn.phase = MasterTurnPhase::Delivering;
            persist_runtime(&tx, config, &mut session, &turn)?;
            tx.commit().map_err(error)?;
            assert!(db.is_autocommit());
            let custody = custody.as_mut().map_err(|error| error.clone())?;
            let guard = match &record.auth {
                MasterBootstrapAuth::RcliConfigured(proof) => custody.acquire_configured_rcli_use(
                    host,
                    &record.auth.binding(),
                    &proof.profile_ref,
                    &turn.id,
                    turn.ordinal,
                )?,
                MasterBootstrapAuth::NativeSignIn(fact) => custody.acquire_use(
                    host,
                    &fact.binding,
                    &turn.id,
                    turn.ordinal,
                    NativeProfileUsePurpose::Execution,
                )?,
            };
            Ok(MasterRuntimeResult::Profile(Box::new(guard)))
        }
        MasterRuntimeCommand::Observe {
            ref host,
            ref attempt_id,
            observation,
            ..
        } => {
            if turn.attempt.as_ref().map(|attempt| &attempt.request.id) != Some(attempt_id) {
                return Err(error("master_attempt_identity_conflict"));
            }
            let record = preparation(&tx, &session, &turn)?;
            if record.auth.binding().database_instance != host.db_instance
                || record.auth.binding().engine_epoch != host.engine_epoch
            {
                return Err(error("master_runtime_host_conflict"));
            }
            let stopping = matches!(
                observation,
                MasterRuntimeObservation::Failed {
                    reason: MasterReason::ScopeRevoked,
                    ..
                } | MasterRuntimeObservation::Cancelled
                    | MasterRuntimeObservation::Disconnected
            );
            if turn.phase != MasterTurnPhase::Cancelled && !stopping {
                current_scope(&tx, config, sources, host, &session, &turn)?;
                if turn.phase != MasterTurnPhase::Succeeded && turn.phase != MasterTurnPhase::Failed
                {
                    verify_preparation(&tx, config, host, &session, &turn)?;
                }
            }
            observe_turn(&tx, config, &mut session, &mut turn, observation)?;
            tx.commit().map_err(error)?;
            Ok(MasterRuntimeResult::Turn(Box::new(turn)))
        }
        MasterRuntimeCommand::Fail { reason, cause, .. } => {
            if matches!(
                turn.phase,
                MasterTurnPhase::Queued | MasterTurnPhase::Prepared
            ) && session.current_turn.as_ref() == Some(&turn.id)
            {
                turn.phase = if turn.phase == MasterTurnPhase::Queued {
                    MasterTurnPhase::Blocked
                } else {
                    complete_local(&tx, &mut turn, MasterTurnPhase::Failed)?;
                    MasterTurnPhase::Failed
                };
                turn.reason = Some(reason);
                turn.cause = Some(cause_code(&cause));
                if turn.phase == MasterTurnPhase::Failed
                    && pauses(reason)
                    && let Some(attempt) = &turn.attempt
                {
                    pause_member(&tx, &attempt.connection.id)?;
                }
                if reason == MasterReason::RepositoryConfigRefused {
                    let repository = master_repository(&tx, &session.project_id)?;
                    turn.detail = crate::worktrees::refused_config_line(&cause, &repository);
                }
                persist_runtime(&tx, config, &mut session, &turn)?;
                assert!(session.current_turn.is_none());
            }
            tx.commit().map_err(error)?;
            Ok(MasterRuntimeResult::Turn(Box::new(turn)))
        }
    }
}

fn reject(code: &str) -> Outcome {
    Outcome::Rejected { code: code.into() }
}

fn execute(
    tx: &Transaction<'_>,
    config: &EffectiveConfig,
    action: &MasterOwnerAction,
    source_hash: &str,
    provenance: &OwnerProvenance,
) -> Result<Outcome, EngineError> {
    let revision = provenance.revision_before.value();
    let next = provenance.revision_after.value();
    if let MasterOwnerAction::Open {
        project_id,
        expected_config_id,
        expected_source_hash,
        selection,
    } = action
    {
        if expected_config_id != &config.id || expected_source_hash != source_hash {
            return Ok(reject("master_source_conflict"));
        }
        let exists: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM projects WHERE id=?1)",
                [project_id.as_str()],
                |row| row.get(0),
            )
            .map_err(error)?;
        if !exists {
            return Ok(reject("project_missing"));
        }
        if let Err(reason) = validate_selection(config, project_id, selection) {
            return Ok(reject(&reason.0));
        }
        if latest(tx, project_id, revision)?
            .is_some_and(|session| session.phase != MasterSessionPhase::Closed)
        {
            return Ok(reject("master_session_active"));
        }
        let count: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM (SELECT 1 FROM master_sessions WHERE phase='configured' LIMIT 65)",
                [],
                |row| row.get(0),
            )
            .map_err(error)?;
        if count >= i64::try_from(SESSIONS_MAX).map_err(error)? {
            return Ok(reject("master_session_capacity"));
        }
        let session = MasterSessionView {
            project_id: project_id.clone(),
            session_id: SessionId::new(format!("master-{}", random_id()?)).map_err(error)?,
            bootstrap_task_id: TaskId::new(format!("bootstrap-{}", random_id()?)).map_err(error)?,
            selection: (**selection).clone(),
            config_id: config.id.clone(),
            source_hash: source_hash.into(),
            phase: MasterSessionPhase::Configured,
            version: Cursor::new(next),
            current_turn: None,
            owner: provenance.clone(),
        };
        let changed = tx.execute("INSERT INTO master_sessions(id,project,created,version,phase,body) VALUES (?1,?2,?3,?3,'configured',?4)", params![session.session_id.as_str(), project_id.as_str(), signed(next)?, encode(&session)?]).map_err(error)?;
        assert_eq!(changed, 1);
        return Ok(Outcome::Applied);
    }
    let (project, id) = match action {
        MasterOwnerAction::Submit {
            project_id,
            session_id,
            ..
        }
        | MasterOwnerAction::Cancel {
            project_id,
            session_id,
            ..
        }
        | MasterOwnerAction::Close {
            project_id,
            session_id,
        } => (project_id, session_id),
        MasterOwnerAction::Open { .. } | MasterOwnerAction::RegisterRcliProfile { .. } => {
            return Err(error("master_action_invalid"));
        }
    };
    let Some(mut session) = load_session(tx, id, revision)? else {
        return Ok(reject("master_session_missing"));
    };
    if &session.project_id != project || session.phase != MasterSessionPhase::Configured {
        return Ok(reject("master_session_not_active"));
    }
    match action {
        MasterOwnerAction::Submit { text, .. } => {
            if session.config_id != config.id
                || session.source_hash != source_hash
                || session.owner.engine_epoch != provenance.engine_epoch
            {
                return Ok(reject("master_source_conflict"));
            }
            if let Err(reason) = validate_selection(config, project, &session.selection) {
                return Ok(reject(&reason.0));
            }
            let superseded = match &session.current_turn {
                None => None,
                Some(current) => {
                    let turn = load_turn(tx, &session, current, revision)?
                        .ok_or_else(|| error("master_current_turn_record"))?;
                    if turn.phase != MasterTurnPhase::Uncertain {
                        return Ok(reject("master_turn_outstanding"));
                    }
                    Some(turn)
                }
            };
            let (count, maximum): (i64, i64) = tx.query_row("SELECT COUNT(*),COALESCE(MAX(ordinal),0) FROM (SELECT ordinal FROM master_turns WHERE session=?1 ORDER BY ordinal LIMIT 1025)", [id.as_str()], |row| Ok((row.get(0)?, row.get(1)?))).map_err(error)?;
            if count != maximum || count < 0 {
                return Err(error("master_turn_order"));
            }
            if count >= i64::try_from(TURNS_MAX).map_err(error)? {
                return Ok(reject("master_turn_capacity"));
            }
            if session.selection.backend == MasterBackend::Rcli
                && let Err(failure) = current_prompt(tx, &session, &provenance.operation_id, text)
            {
                if failure.0 == "master_input_too_large" {
                    return Ok(reject(&failure.0));
                }
                return Err(failure);
            }
            let input = ConversationMutation::AppendMessage {
                project_id: project.clone(),
                text: text.clone(),
            };
            let result = crate::conversation::apply(tx, &input, revision)?;
            if result != Outcome::Applied {
                return Ok(result);
            }
            if let Some(turn) = superseded {
                cancel_turn(tx, turn)?;
                session.current_turn = None;
            }
            let turn = MasterTurnView {
                id: provenance.operation_id.clone(),
                session_id: id.clone(),
                ordinal: unsigned(
                    count
                        .checked_add(1)
                        .ok_or_else(|| error("master_integer_bounds"))?,
                )?,
                input_cursor: Cursor::new(next),
                phase: MasterTurnPhase::Queued,
                reason: None,
                attempt: None,
                runtime: None,
                reply: None,
                accounts: Vec::new(),
                detail: None,
                cause: None,
            };
            let changed = tx.execute("INSERT INTO master_turns(session,ordinal,id,input,phase,body) VALUES (?1,?2,?3,?4,'queued',?5)", params![id.as_str(), signed(turn.ordinal)?, turn.id.as_str(), signed(next)?, encode(&turn)?]).map_err(error)?;
            assert_eq!(changed, 1);
            session.current_turn = Some(turn.id);
        }
        MasterOwnerAction::Cancel { turn_id, .. } => {
            if session.current_turn.as_ref() != Some(turn_id) {
                return Ok(reject("master_turn_not_current"));
            }
            let turn = load_turn(tx, &session, turn_id, revision)?
                .ok_or_else(|| error("master_current_turn_record"))?;
            cancel_turn(tx, turn)?;
            session.current_turn = None;
        }
        MasterOwnerAction::Close { .. } => {
            if let Some(turn_id) = &session.current_turn {
                let turn = load_turn(tx, &session, turn_id, revision)?
                    .ok_or_else(|| error("master_current_turn_record"))?;
                cancel_turn(tx, turn)?;
            }
            session.current_turn = None;
            session.phase = MasterSessionPhase::Closed;
        }
        MasterOwnerAction::Open { .. } | MasterOwnerAction::RegisterRcliProfile { .. } => {
            return Err(error("master_action_invalid"));
        }
    }
    session.version = Cursor::new(next);
    save_session(tx, &session)?;
    assert!(!tx.is_autocommit());
    Ok(Outcome::Applied)
}

pub(super) fn apply(
    db: &mut Connection,
    config: &EffectiveConfig,
    sources: Option<&settings::SettingsSources>,
    custody: &mut Result<NativeProfileCustody, EngineError>,
    host: &HostStamp,
    admission: &Admission,
    request: &OwnerRequest,
) -> Result<OwnerApplied, EngineError> {
    request.validate()?;
    let OwnerOperation::Apply {
        mutation: OwnerMutation::ProjectMaster { action },
    } = &request.operation
    else {
        return Err(error("master_action_required"));
    };
    validate_action(action)?;
    let hash_text = request.mutation_hash()?;
    let hash = Sha256::digest(serde_json::to_vec(request).map_err(error)?);
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(error)?;
    check_owner_host(&tx, host)?;
    check_owner_admission(&tx, admission)?;
    let revision = owner_revision(&tx)?;
    if let Some((provenance, result)) = owner_lookup(
        &tx,
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
    let prepared = (|| -> Result<(OwnerResult, OwnerProvenance), EngineError> {
        let mut provenance = OwnerProvenance {
            origin: "native_owner".into(),
            owner_id: admission.owner_id.clone(),
            operation_id: request.request_id.clone(),
            request_hash: hash_text,
            engine_epoch: admission.engine_epoch.clone(),
            admitted_epoch: admission.owner_epoch,
            resulting_epoch: admission.owner_epoch,
            revision_before: Cursor::new(revision),
            revision_after: Cursor::new(revision),
            event_cursor: None,
        };
        let opening = sources.is_some() && matches!(action, MasterOwnerAction::Open { .. });
        if opening {
            tx.execute_batch("SAVEPOINT master_open_adoption")
                .map_err(error)?;
        }
        let outcome = if request.expected_revision.value() != revision {
            reject("stale_revision")
        } else {
            let next = revision
                .checked_add(1)
                .ok_or_else(|| error("revision_exhausted"))?;
            signed(next)?;
            provenance.revision_after = Cursor::new(next);
            provenance.event_cursor = Some(Cursor::new(next));
            let source_hash = settings::verified_source_hash(config, sources)?;
            execute(&tx, config, action, &source_hash, &provenance)?
        };
        let result = match outcome {
            Outcome::Applied => {
                persist_event_ids(
                    &tx,
                    config,
                    &request.request_id,
                    &ClientId::new(format!("owner:{}", admission.owner_id)).map_err(error)?,
                    provenance.revision_after.value(),
                )?;
                OwnerResult::Applied
            }
            Outcome::Rejected { code } => {
                provenance.revision_after = Cursor::new(revision);
                provenance.event_cursor = None;
                OwnerResult::Rejected { code }
            }
            _ => return Err(error("master_mutation_result")),
        };
        owner_record(
            &tx,
            &provenance,
            &hash,
            &serde_json::to_vec(&result).map_err(error)?,
        )?;
        if result == OwnerResult::Applied
            && let MasterOwnerAction::Open { project_id, .. } = action
            && let Some(sources) = sources
        {
            let session = latest(&tx, project_id, owner_revision(&tx)?)?
                .ok_or_else(|| error("master_session_missing"))?;
            match adopt_account_profile(&tx, config, sources, custody, host, &session) {
                Ok(()) => {}
                Err(failure) if failure.0 == "rcli_profile_identity_changed" => {
                    tx.execute_batch(
                        "ROLLBACK TO master_open_adoption; RELEASE master_open_adoption",
                    )
                    .map_err(error)?;
                    assert_eq!(owner_revision(&tx)?, revision);
                    provenance.revision_after = Cursor::new(revision);
                    provenance.event_cursor = None;
                    let result = OwnerResult::Rejected { code: failure.0 };
                    owner_record(
                        &tx,
                        &provenance,
                        &hash,
                        &serde_json::to_vec(&result).map_err(error)?,
                    )?;
                    return Ok((result, provenance));
                }
                Err(failure) => return Err(failure),
            }
        }
        if opening {
            tx.execute_batch("RELEASE master_open_adoption")
                .map_err(error)?;
        }
        Ok((result, provenance))
    })();
    let (result, provenance) = match prepared {
        Ok(value) => value,
        Err(_) => {
            tx.rollback()
                .map_err(|_| error("master_rollback_outcome_unknown"))?;
            return Err(error("master_transaction_rolled_back"));
        }
    };
    let current_revision = provenance.revision_after.clone();
    tx.commit().map_err(error)?;
    assert_eq!(
        current_revision.value() > revision,
        result == OwnerResult::Applied
    );
    Ok(OwnerApplied {
        result,
        provenance,
        historical_replay: false,
        current_revision,
    })
}

fn cursor(
    admission: &Admission,
    config: &ConfigId,
    source: &str,
    project: &ProjectId,
    session: &SessionId,
    revision: u64,
    after: u64,
) -> MasterCursor {
    MasterCursor {
        db_instance: admission.db_instance.clone(),
        owner_id: admission.owner_id.clone(),
        owner_epoch: admission.owner_epoch,
        engine_epoch: admission.engine_epoch.clone(),
        project_id: project.clone(),
        session_id: session.clone(),
        config_id: config.clone(),
        source_hash: source.into(),
        version: Cursor::new(revision),
        after_ordinal: after,
    }
}

fn page(
    tx: &Transaction<'_>,
    session: &MasterSessionView,
    basis: MasterCursor,
    limit: u8,
    revision: u64,
) -> Result<MasterTurnPage, EngineError> {
    let mut page = MasterTurnPage {
        project_id: session.project_id.clone(),
        session_id: session.session_id.clone(),
        turns: Vec::new(),
        next: None,
    };
    let sql = format!(
        "SELECT {TURN_COLUMNS} FROM master_turns WHERE session=?1 AND ordinal>?2 ORDER BY ordinal LIMIT ?3"
    );
    let mut statement = tx.prepare(&sql).map_err(error)?;
    let mut rows = statement
        .query(params![
            session.session_id.as_str(),
            signed(basis.after_ordinal)?,
            i64::from(limit) + 1
        ])
        .map_err(error)?;
    while let Some(row) = rows.next().map_err(error)? {
        let turn = decode_turn(tx, session, row, revision)?;
        if page.turns.len() == usize::from(limit) {
            return Ok(page);
        }
        if turn.ordinal
            != page
                .turns
                .last()
                .map_or(basis.after_ordinal, |previous| previous.ordinal)
                .checked_add(1)
                .ok_or_else(|| error("master_turn_order"))?
        {
            return Err(error("master_turn_order"));
        }
        let mut next = basis.clone();
        next.after_ordinal = turn.ordinal;
        page.turns.push(turn);
        let previous = page.next.replace(next);
        if serde_json::to_vec(&page).map_err(error)?.len() > PAGE_BYTES_MAX {
            page.turns.pop();
            page.next = previous;
            if page.turns.is_empty() {
                return Err(error("master_page_entry_bounds"));
            }
            return Ok(page);
        }
    }
    page.next = None;
    assert!(page.turns.len() <= usize::from(limit));
    assert!(serde_json::to_vec(&page).map_err(error)?.len() <= PAGE_BYTES_MAX);
    Ok(page)
}

pub(super) fn read(
    db: &mut Connection,
    config: &EffectiveConfig,
    sources: Option<&settings::SettingsSources>,
    admission: &Admission,
    query: &OwnerRead,
) -> Result<OwnerReadReply, EngineError> {
    let tx = db.transaction().map_err(error)?;
    check_owner_admission(&tx, admission)?;
    settings::ensure_clear(&tx)?;
    let revision = owner_revision(&tx)?;
    let source_hash = settings::verified_source_hash(config, sources)?;
    let data = match query {
        OwnerRead::ProjectMaster { project_id } => OwnerData::ProjectMaster {
            session: latest(&tx, project_id, revision)?.map(Box::new),
        },
        OwnerRead::ProjectMasterTurns {
            project_id,
            session_id,
            cursor: supplied,
            limit,
        } => {
            validate_page(supplied.as_ref(), *limit)?;
            let expected = cursor(
                admission,
                &config.id,
                &source_hash,
                project_id,
                session_id,
                revision,
                supplied.as_ref().map_or(0, |cursor| cursor.after_ordinal),
            );
            if supplied
                .as_ref()
                .is_some_and(|supplied| supplied != &expected)
            {
                OwnerData::ResyncRequired
            } else {
                let session = load_session(&tx, session_id, revision)?
                    .filter(|session| &session.project_id == project_id)
                    .ok_or_else(|| error("master_session_missing"))?;
                let exists: bool = if supplied.is_some() {
                    tx.query_row(
                        "SELECT EXISTS(SELECT 1 FROM master_turns WHERE session=?1 AND ordinal=?2)",
                        params![session_id.as_str(), signed(expected.after_ordinal)?],
                        |row| row.get(0),
                    )
                    .map_err(error)?
                } else {
                    true
                };
                if !exists {
                    OwnerData::ResyncRequired
                } else {
                    OwnerData::ProjectMasterTurns {
                        page: page(&tx, &session, expected, *limit, revision)?,
                    }
                }
            }
        }
        _ => return Err(error("master_read_required")),
    };
    Ok(OwnerReadReply {
        revision: Cursor::new(revision),
        config_id: config.id.clone(),
        data,
    })
}

#[cfg(all(test, unix))]
mod brief_tests;

#[cfg(all(test, unix))]
mod capacity_tests;

#[cfg(all(test, unix))]
mod rcli_configured_tests {
    use super::*;

    mod stop_settlement_tests {
        include!("master_session/stop_settlement_tests.rs");
    }

    mod brief_boundary_tests {
        include!("master_session/brief_boundary_tests.rs");
    }

    mod readiness_tests {
        include!("master_session/readiness_tests.rs");
    }
    use crate::engine::owner::{ModelAccessCollection, Started};
    use crate::master_session::{MasterBootstrapAuth, MasterSelection, RcliConfiguredProfileProof};
    use crate::protocol::{CredentialRef, ProjectSpec};
    use crate::routing::{ResourceBudget, RouteRequest};
    use crate::storage::{Job, Storage};
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;
    use tokio::sync::oneshot;

    const CONFIG: &str = r#"version = 1
timeout_ms = 2000
allowed_destinations = ["local-profile"]
[routing]
approved_accounts = ["local-account"]
global_capacity = { slots=1, cpu_millis=1000, memory_mib=512, processes=4, output_bytes=1048576, wall_time_ms=60000, spend_microunits=0 }
project_capacity = { slots=1, cpu_millis=1000, memory_mib=512, processes=4, output_bytes=1048576, wall_time_ms=60000, spend_microunits=0 }
task_capacity = { slots=1, cpu_millis=1000, memory_mib=512, processes=4, output_bytes=1048576, wall_time_ms=60000, spend_microunits=0 }
[model_access]
accounts = ["local-account"]
[[model_access.connections]]
id = "local-connection"
provider = "local-provider"
harness = "rcli"
protocol = "openai-chat"
account_ref = "local-account"
endpoint = { kind = "http", url = "http://127.0.0.1:11434/v1" }
model = { id = "local-model", author = "local-author" }
auth = { method = "none" }
[[model_access.pools]]
id = "local-pool"
members = ["local-connection"]
projects = ["rcli-project"]
roles = ["project_leader"]
fallback_allowed = false
"#;

    const PROMPT: &str = "Describe this project in one sentence without using tools.";

    struct Fixture {
        storage: Storage,
        config: EffectiveConfig,
        started: Started,
        session: MasterSessionView,
        turn: MasterTurnView,
        path: PathBuf,
        global: PathBuf,
        home: PathBuf,
        repository: PathBuf,
        root: tempfile::TempDir,
        _repos: Option<tempfile::TempDir>,
    }

    #[derive(Debug, PartialEq, Eq)]
    struct Snapshot {
        revision: u64,
        turn: String,
        counts: [i64; 6],
    }

    fn reference(value: &str) -> CredentialRef {
        CredentialRef::new(value).unwrap()
    }

    fn private_file(path: &Path, bytes: &[u8]) {
        std::fs::write(path, bytes).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }

    fn git(repository: &Path, home: &Path, arguments: &[&str]) {
        let output = crate::spawn_gate::output(
            Command::new("/usr/bin/git")
                .env_clear()
                .env("PATH", "/usr/bin:/bin")
                .env("HOME", home)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_TERMINAL_PROMPT", "0")
                .args([
                    "-c",
                    "core.hooksPath=/dev/null",
                    "-c",
                    "commit.gpgsign=false",
                ])
                .args([
                    "-c",
                    "user.name=Master storage fixture",
                    "-c",
                    "user.email=master-storage@example.invalid",
                ])
                .arg("-C")
                .arg(repository)
                .args(arguments)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped()),
        )
        .unwrap();
        assert!(
            output.status.success(),
            "actual Git fixture failed: {output:?}"
        );
    }

    async fn owner_read(storage: &Storage, started: &Started, query: OwnerRead) -> OwnerReadReply {
        let (send, receive) = oneshot::channel();
        storage
            .sender
            .try_send(Job::OwnerRead(
                started.owners[0].admission.clone(),
                query,
                send,
            ))
            .unwrap_or_else(|_| panic!("actual owner read queue unavailable"));
        receive.await.unwrap().unwrap()
    }

    async fn owner_apply(storage: &Storage, started: &Started, id: &str, mutation: OwnerMutation) {
        assert_eq!(
            owner_result(storage, started, id, mutation).await,
            OwnerResult::Applied
        );
    }

    async fn owner_result(
        storage: &Storage,
        started: &Started,
        id: &str,
        mutation: OwnerMutation,
    ) -> OwnerResult {
        let basis = owner_read(storage, started, OwnerRead::Capabilities).await;
        let (send, receive) = oneshot::channel();
        storage
            .sender
            .try_send(Job::OwnerApply(
                started.owners[0].admission.clone(),
                OwnerRequest {
                    protocol_version: PROTOCOL_VERSION,
                    request_id: RequestId::new(id).unwrap(),
                    expected_revision: basis.revision,
                    operation: OwnerOperation::Apply { mutation },
                },
                send,
            ))
            .unwrap_or_else(|_| panic!("actual owner mutation queue unavailable"));
        receive.await.unwrap().unwrap().result
    }

    impl Fixture {
        async fn new(with_repository: bool) -> Self {
            Self::new_with_message(with_repository, PROMPT).await
        }

        async fn new_with_message(with_repository: bool, message: &str) -> Self {
            Self::build(with_repository, false, message).await
        }

        /// A master on a project the owner added as an existing folder: the
        /// committed repository outside the runtime is onboarded, not declared.
        async fn onboarded() -> Self {
            Self::build(true, true, PROMPT).await
        }

        async fn build(with_repository: bool, onboarded: bool, message: &str) -> Self {
            let root = tempfile::Builder::new()
                .prefix("ca-mrc-")
                .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
                .unwrap();
            std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
            let global = root.path().join("global.toml");
            private_file(&global, CONFIG.as_bytes());
            let home = root.path().join("configured-home");
            std::fs::create_dir(&home).unwrap();
            std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).unwrap();
            let sdk = home.join(".backboard");
            std::fs::create_dir(&sdk).unwrap();
            std::fs::set_permissions(&sdk, std::fs::Permissions::from_mode(0o700)).unwrap();
            private_file(
                &sdk.join("config.json"),
                &serde_json::to_vec(&serde_json::json!({
                    "model": {"provider": "local-provider", "model": "local-model"},
                    "providers": [{"id": "local-provider", "name": "Local provider",
                        "protocol": "openai-chat", "baseUrl": "http://127.0.0.1:11434/v1",
                        "auth": {"type": "none"}}]
                }))
                .unwrap(),
            );
            let repos = onboarded.then(|| {
                tempfile::Builder::new()
                    .prefix("ca-mrc-repos-")
                    .tempdir_in("/private/tmp")
                    .unwrap()
            });
            let repository = match &repos {
                Some(repos) => repos.path().canonicalize().unwrap().join("rcli-project"),
                None => root.path().join("repository"),
            };
            if with_repository {
                std::fs::create_dir(&repository).unwrap();
                git(
                    &repository,
                    root.path(),
                    &["init", "--initial-branch=trunk"],
                );
                private_file(
                    &repository.join("README"),
                    b"Actual committed master workspace fixture.\n",
                );
                git(&repository, root.path(), &["add", "README"]);
                git(
                    &repository,
                    root.path(),
                    &["commit", "-m", "Initialize master workspace fixture"],
                );
            }
            let path = root.path().join("engine.db");
            let (storage, config) = Storage::open_with_settings_sources(
                &path,
                &global,
                &root.path().join("project.toml"),
            )
            .await
            .unwrap();
            let (send, receive) = oneshot::channel();
            storage
                .sender
                .try_send(Job::OwnerStart(send))
                .unwrap_or_else(|_| panic!("actual owner start queue unavailable"));
            let started = receive.await.unwrap().unwrap();
            let project = ProjectId::new("rcli-project").unwrap();
            let create = if onboarded {
                OwnerMutation::StartOnboarding {
                    path: repository.to_str().unwrap().into(),
                    pool: reference("local-pool"),
                }
            } else {
                OwnerMutation::CreateProject {
                    project_id: project.clone(),
                    spec: ProjectSpec {
                        name: "Configured local master".into(),
                        repositories: if with_repository {
                            vec![repository.to_str().unwrap().into()]
                        } else {
                            vec![]
                        },
                    },
                }
            };
            owner_apply(&storage, &started, "create-rcli-project", create).await;
            let basis = owner_read(
                &storage,
                &started,
                OwnerRead::ModelAccessPage {
                    collection: ModelAccessCollection::Pools,
                    cursor: None,
                    limit: 1,
                },
            )
            .await;
            let OwnerData::ModelAccessPage { source_hash, .. } = basis.data else {
                panic!("actual model source basis missing");
            };
            owner_apply(
                &storage,
                &started,
                "open-rcli-master",
                OwnerMutation::ProjectMaster {
                    action: MasterOwnerAction::Open {
                        project_id: project.clone(),
                        expected_config_id: basis.config_id,
                        expected_source_hash: source_hash,
                        selection: Box::new(MasterSelection {
                            backend: MasterBackend::Rcli,
                            pool_ref: reference("local-pool"),
                            target_profile_ref: reference("local-profile"),
                            route: RouteRequest {
                                preferred_target: Some(reference("local-profile")),
                                required_target: Some(reference("local-profile")),
                                required_capabilities: vec![],
                                account_ref: reference("local-account"),
                                budget_ref: reference("conversation-budget"),
                                resources: ResourceBudget::unit(),
                            },
                        }),
                    },
                },
            )
            .await;
            let state = owner_read(
                &storage,
                &started,
                OwnerRead::ProjectMaster {
                    project_id: project.clone(),
                },
            )
            .await;
            let OwnerData::ProjectMaster {
                session: Some(session),
            } = state.data
            else {
                panic!("actual committed master session missing");
            };
            let session = *session;
            owner_apply(
                &storage,
                &started,
                "submit-rcli-turn",
                OwnerMutation::ProjectMaster {
                    action: MasterOwnerAction::Submit {
                        project_id: project.clone(),
                        session_id: session.session_id.clone(),
                        text: message.into(),
                    },
                },
            )
            .await;
            let state = owner_read(
                &storage,
                &started,
                OwnerRead::ProjectMaster {
                    project_id: project.clone(),
                },
            )
            .await;
            let OwnerData::ProjectMaster {
                session: Some(session),
            } = state.data
            else {
                panic!("actual submitted master session missing");
            };
            let session = *session;
            let state = owner_read(
                &storage,
                &started,
                OwnerRead::ProjectMasterTurns {
                    project_id: project,
                    session_id: session.session_id.clone(),
                    cursor: None,
                    limit: 1,
                },
            )
            .await;
            let OwnerData::ProjectMasterTurns { page } = state.data else {
                panic!("actual queued master turn missing");
            };
            assert_eq!(page.turns.len(), 1);
            let turn = page.turns.into_iter().next().unwrap();
            assert_eq!(turn.phase, MasterTurnPhase::Queued);
            assert_eq!(turn.attempt, None);
            assert_eq!(turn.runtime, None);
            assert_eq!(turn.reply, None);
            Self {
                storage,
                config,
                started,
                session,
                turn,
                path,
                global,
                home,
                repository,
                root,
                _repos: repos,
            }
        }

        async fn runtime(
            &self,
            command: MasterRuntimeCommand,
        ) -> Result<MasterRuntimeResult, EngineError> {
            let (send, receive) = oneshot::channel();
            self.storage
                .sender
                .try_send(Job::MasterRuntime(Box::new(command), send))
                .unwrap_or_else(|_| panic!("actual master runtime queue unavailable"));
            receive.await.unwrap()
        }

        async fn register(&self) -> RcliConfiguredProfileProof {
            let result = self
                .runtime(MasterRuntimeCommand::RegisterRcliProfile {
                    host: self.started.host.clone(),
                    project_id: self.session.project_id.clone(),
                    session_id: self.session.session_id.clone(),
                    configured_home: self.home.clone(),
                })
                .await
                .unwrap();
            let MasterRuntimeResult::ConfiguredProfile(proof) = result else {
                panic!("actual configured profile receipt missing");
            };
            *proof
        }

        async fn view(&self) -> (MasterSessionView, Vec<MasterTurnView>) {
            let project_id = self.session.project_id.clone();
            let state = owner_read(
                &self.storage,
                &self.started,
                OwnerRead::ProjectMaster {
                    project_id: project_id.clone(),
                },
            )
            .await;
            let OwnerData::ProjectMaster {
                session: Some(session),
            } = state.data
            else {
                panic!("actual master session missing");
            };
            let state = owner_read(
                &self.storage,
                &self.started,
                OwnerRead::ProjectMasterTurns {
                    project_id,
                    session_id: self.session.session_id.clone(),
                    cursor: None,
                    limit: 8,
                },
            )
            .await;
            let OwnerData::ProjectMasterTurns { page } = state.data else {
                panic!("actual master turns missing");
            };
            (*session, page.turns)
        }

        fn action(&self, id: &str, action: MasterOwnerAction) -> (String, OwnerMutation) {
            (id.into(), OwnerMutation::ProjectMaster { action })
        }

        fn submit(&self, id: &str, text: &str) -> (String, OwnerMutation) {
            self.action(
                id,
                MasterOwnerAction::Submit {
                    project_id: self.session.project_id.clone(),
                    session_id: self.session.session_id.clone(),
                    text: text.into(),
                },
            )
        }

        async fn result(&self, (id, mutation): (String, OwnerMutation)) -> OwnerResult {
            owner_result(&self.storage, &self.started, &id, mutation).await
        }

        fn prepare(&self) -> MasterRuntimeCommand {
            MasterRuntimeCommand::Prepare {
                host: self.started.host.clone(),
                project_id: self.session.project_id.clone(),
                session_id: self.session.session_id.clone(),
                turn_id: self.turn.id.clone(),
            }
        }

        fn db(&self) -> Connection {
            let db = Connection::open_with_flags(
                &self.path,
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
                    | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW,
            )
            .unwrap();
            db.busy_timeout(std::time::Duration::from_secs(1)).unwrap();
            db
        }

        fn snapshot(&self) -> Snapshot {
            let mut db = self.db();
            let tx = db.transaction().unwrap();
            let tables = [
                "master_preparations",
                "model_gateway_attempts",
                "model_gateway_observations",
                "native_auth_operations",
                "worktree_leases",
                "bootstrap_workspaces",
            ];
            let counts = tables.map(|table| {
                tx.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap()
            });
            let snapshot = Snapshot {
                revision: owner_revision(&tx).unwrap(),
                turn: tx
                    .query_row(
                        "SELECT body FROM master_turns WHERE session=?1 AND id=?2",
                        params![self.session.session_id.as_str(), self.turn.id.as_str()],
                        |row| row.get(0),
                    )
                    .unwrap(),
                counts,
            };
            drop(tx);
            snapshot
        }

        async fn close(self) -> tempfile::TempDir {
            let (send, receive) = oneshot::channel();
            self.storage
                .sender
                .try_send(Job::OwnerStop(self.started.host, send))
                .unwrap_or_else(|_| panic!("actual owner stop queue unavailable"));
            receive.await.unwrap().unwrap();
            let (send, receive) = oneshot::channel();
            self.storage
                .sender
                .try_send(Job::Close(send))
                .unwrap_or_else(|_| panic!("actual actor close queue unavailable"));
            receive.await.unwrap().unwrap();
            assert!(self.root.path().is_dir());
            self.root
        }
    }

    #[tokio::test]
    async fn registered_local_rcli_prepares_real_lease_and_claims_one_producer_without_ready() {
        let case = Fixture::new(true).await;
        let original_config = std::fs::read(case.home.join(".backboard/config.json")).unwrap();
        let proof = case.register().await;
        assert_eq!(case.register().await, proof);
        assert_eq!(proof.database_instance, case.started.host.db_instance);
        assert_eq!(proof.engine_epoch, case.started.host.engine_epoch);
        assert_eq!(proof.config_id, case.config.id);
        assert_eq!(proof.source_hash, case.session.source_hash);
        assert_eq!(proof.profile_ref, case.session.selection.target_profile_ref);
        assert_eq!(proof.connection.harness, reference("rcli"));
        assert_eq!(proof.connection.auth, ModelAuthentication::None {});
        assert_eq!(
            proof.profile_config_sha256,
            format!("{:x}", Sha256::digest(&original_config))
        );
        assert!(proof.generation > 0);
        assert!(!proof.custody_id.is_empty());
        let result = case.runtime(case.prepare()).await.unwrap();
        let MasterRuntimeResult::Prepared(prepared) = result else {
            panic!("registered local RCLI must prepare the actual first conversation");
        };
        assert_eq!(prepared.text, PROMPT);
        assert_eq!(prepared.session.session_id, case.session.session_id);
        assert_eq!(
            prepared.session.bootstrap_task_id,
            case.session.bootstrap_task_id
        );
        assert_eq!(prepared.attempt.request.project_id, case.session.project_id);
        assert_eq!(
            prepared.attempt.request.task_id,
            turn_task_id(&case.session.session_id, &case.turn.id).unwrap()
        );
        assert_eq!(prepared.attempt.request.role, Role::ProjectLeader);
        assert!(
            prepared
                .attempt
                .request
                .route
                .required_capabilities
                .is_empty()
        );
        assert!(prepared.attempt.request.previous_attempt.is_none());
        assert_eq!(prepared.attempt.connection, proof.connection);
        assert!(prepared.attempt.completion.is_none());
        assert_eq!(prepared.lease.state, LeaseState::Ready);
        assert_eq!(prepared.lease.project_id, case.session.project_id);
        assert_eq!(prepared.lease.task_id, case.session.bootstrap_task_id);
        assert_eq!(prepared.lease.owner_session, case.session.session_id);
        assert_eq!(prepared.cwd, PathBuf::from(&prepared.lease.path));
        assert_eq!(prepared.repository, case.repository);
        assert!(prepared.cwd.is_dir());
        assert!(prepared.cwd.join(".git").is_file());
        {
            let mut db = case.db();
            let tx = db.transaction().unwrap();
            let auth = MasterBootstrapAuth::RcliConfigured(proof.clone());
            validate_bootstrap_auth(&tx, &prepared.session, &auth).unwrap();
            let (allocation, lease) =
                super::super::master_bootstrap::ready(&tx, &prepared.session, &auth).unwrap();
            assert_eq!(allocation, prepared.worktree_lease);
            assert_eq!(lease, prepared.lease);
            assert_eq!(
                model_gateway::read_attempt(&tx, &prepared.attempt.request.id).unwrap(),
                Some(prepared.attempt.clone())
            );
            let body: String = tx
                .query_row(
                    "SELECT body FROM master_preparations WHERE session=?1 AND turn=?2",
                    params![case.session.session_id.as_str(), case.turn.id.as_str()],
                    |row| row.get(0),
                )
                .unwrap();
            let record: serde_json::Value = serde_json::from_str(&body).unwrap();
            assert_eq!(record["auth"], serde_json::to_value(&auth).unwrap());
            let state =
                model_gateway::connection_state(&tx, &proof.connection, now_ms().unwrap()).unwrap();
            assert_eq!(state.health, model_gateway::Health::Unknown);
            assert_eq!(state.quota, model_gateway::Quota::Unknown);
            assert_eq!(state.observed_at_ms, None);
        }
        let acquire = || MasterRuntimeCommand::AcquireProfile {
            host: case.started.host.clone(),
            project_id: case.session.project_id.clone(),
            session_id: case.session.session_id.clone(),
            turn_id: case.turn.id.clone(),
            attempt_id: prepared.attempt.request.id.clone(),
        };
        let MasterRuntimeResult::Profile(guard) = case.runtime(acquire()).await.unwrap() else {
            panic!("actual configured RCLI Execution guard missing");
        };
        let MasterRuntimeResult::Turn(duplicate) = case.runtime(acquire()).await.unwrap() else {
            panic!("duplicate acquisition must return the current turn");
        };
        assert_eq!(duplicate.phase, MasterTurnPhase::Delivering);
        assert_eq!(duplicate.attempt.as_ref(), Some(&prepared.attempt));
        assert_eq!(duplicate.runtime, None);
        assert_eq!(duplicate.reply, None);
        assert_eq!(duplicate.reason, None);
        drop(guard);
        let snapshot = case.snapshot();
        assert_eq!(snapshot.counts, [1, 1, 0, 0, 1, 1]);
        assert_eq!(
            std::fs::read(case.home.join(".backboard/config.json")).unwrap(),
            original_config
        );
        assert!(!case.home.join(".backboard/keys.json").exists());
        case.close().await;
    }

    #[tokio::test]
    async fn an_added_folder_project_prepares_its_master_turn_on_its_own_repository() {
        let case = Fixture::onboarded().await;
        case.register().await;
        let prepared = match case.runtime(case.prepare()).await {
            Ok(MasterRuntimeResult::Prepared(prepared)) => prepared,
            Ok(_) => panic!("an added folder must prepare its master turn"),
            Err(failure) => panic!("an added folder must reach its master turn: {failure:?}"),
        };
        assert_eq!(prepared.text, PROMPT);
        assert_eq!(prepared.repository, case.repository);
        assert_eq!(prepared.lease.state, LeaseState::Ready);
        assert!(prepared.cwd.join(".git").is_file());
        case.close().await;
    }

    #[tokio::test]
    async fn unregistered_local_rcli_is_configuration_required_without_admission_or_allocation() {
        let case = Fixture::new(false).await;
        let before = case.snapshot();
        let result = case.runtime(case.prepare()).await;
        assert_eq!(
            result.err().unwrap().0,
            "rcli_profile_configuration_required"
        );
        assert_eq!(case.snapshot(), before);
        assert_eq!(before.counts, [0; 6]);
        assert!(!case.home.join(".backboard/keys.json").exists());
        case.close().await;
    }

    #[tokio::test]
    async fn engine_generated_local_rcli_home_registers_on_first_prepare_without_owner_path() {
        let case = Fixture::new(true).await;
        let connection = case
            .config
            .settings
            .model_access
            .as_ref()
            .unwrap()
            .connections[0]
            .clone();
        let profile_ref =
            super::super::account_profile::configured_profile_ref(&connection).unwrap();
        let homes = case.root.path().join("configured-sdk-homes");
        std::fs::create_dir(&homes).unwrap();
        std::fs::set_permissions(&homes, std::fs::Permissions::from_mode(0o700)).unwrap();
        let generated = homes.join(format!(
            "{:x}",
            Sha256::digest(profile_ref.as_str().as_bytes())
        ));
        std::fs::rename(&case.home, &generated).unwrap();
        let result = case.runtime(case.prepare()).await.unwrap();
        let MasterRuntimeResult::Prepared(prepared) = result else {
            panic!("an engine-generated SDK home must register and prepare the first conversation");
        };
        assert_eq!(prepared.text, PROMPT);
        assert_eq!(prepared.attempt.connection, connection);
        let mut db = case.db();
        let tx = db.transaction().unwrap();
        let record = registration(&tx, &case.session.session_id)
            .unwrap()
            .unwrap();
        assert_eq!(
            record.proof.profile_ref,
            case.session.selection.target_profile_ref
        );
        drop(tx);
        case.close().await;
    }

    #[tokio::test]
    async fn configured_receipt_cannot_be_replaced_by_a_constructed_proof() {
        let case = Fixture::new(false).await;
        let proof = case.register().await;
        let before = case.snapshot();
        let mut db = case.db();
        let tx = db.transaction().unwrap();
        validate_bootstrap_auth(
            &tx,
            &case.session,
            &MasterBootstrapAuth::RcliConfigured(proof.clone()),
        )
        .unwrap();
        for field in 0..11 {
            let mut changed = proof.clone();
            match field {
                0 => changed.registration_id = RequestId::new("unregistered-proof").unwrap(),
                1 => changed.generation = changed.generation.checked_add(1).unwrap(),
                2 => changed.engine_epoch = "0".repeat(64),
                3 => changed.config_id = ConfigId::new("0".repeat(64)).unwrap(),
                4 => changed.source_hash = "0".repeat(64),
                5 => changed.profile_ref = reference("unregistered-profile"),
                6 => changed.connection.account_ref = reference("foreign-account"),
                7 => changed.profile_config_sha256 = "0".repeat(64),
                8 => changed.database_instance = "0".repeat(64),
                9 => changed.custody_id = "unregistered-custody".into(),
                10 => changed.at_revision = Cursor::new(u64::MAX),
                _ => unreachable!(),
            }
            assert!(
                validate_bootstrap_auth(
                    &tx,
                    &case.session,
                    &MasterBootstrapAuth::RcliConfigured(changed)
                )
                .is_err(),
                "constructed proof field {field} was trusted"
            );
        }
        drop(tx);
        drop(db);
        assert_eq!(case.snapshot(), before);
        assert_eq!(before.counts, [0; 6]);
        case.close().await;
    }

    #[tokio::test]
    async fn configured_profiles_of_closed_sessions_leave_room_for_a_new_registration() {
        let case = Fixture::new(true).await;
        let project = case.session.project_id.clone();
        let mut session = case.session.session_id.clone();
        for cycle in 0..=crate::master_session::SESSIONS_MAX {
            let registered = case
                .runtime(MasterRuntimeCommand::RegisterRcliProfile {
                    host: case.started.host.clone(),
                    project_id: project.clone(),
                    session_id: session.clone(),
                    configured_home: case.home.clone(),
                })
                .await;
            match registered {
                Ok(MasterRuntimeResult::ConfiguredProfile(_)) => {}
                Ok(_) => panic!("register {cycle}: another runtime result"),
                Err(failure) => panic!("register {cycle}: {}", failure.0),
            }
            owner_apply(
                &case.storage,
                &case.started,
                &format!("close-configured-{cycle}"),
                OwnerMutation::ProjectMaster {
                    action: MasterOwnerAction::Close {
                        project_id: project.clone(),
                        session_id: session.clone(),
                    },
                },
            )
            .await;
            let basis = owner_read(
                &case.storage,
                &case.started,
                OwnerRead::ModelAccessPage {
                    collection: ModelAccessCollection::Pools,
                    cursor: None,
                    limit: 1,
                },
            )
            .await;
            let OwnerData::ModelAccessPage { source_hash, .. } = basis.data else {
                panic!("actual model source basis missing");
            };
            owner_apply(
                &case.storage,
                &case.started,
                &format!("open-configured-{cycle}"),
                OwnerMutation::ProjectMaster {
                    action: MasterOwnerAction::Open {
                        project_id: project.clone(),
                        expected_config_id: basis.config_id,
                        expected_source_hash: source_hash,
                        selection: Box::new(case.session.selection.clone()),
                    },
                },
            )
            .await;
            let state = owner_read(
                &case.storage,
                &case.started,
                OwnerRead::ProjectMaster {
                    project_id: project.clone(),
                },
            )
            .await;
            let OwnerData::ProjectMaster {
                session: Some(opened),
            } = state.data
            else {
                panic!("actual reopened master session missing");
            };
            assert_ne!(opened.session_id, session);
            session = opened.session_id;
        }
        let mut db = case.db();
        let tx = db.transaction().unwrap();
        let registrations: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM configured_master_profiles",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(registrations >= 1);
        assert!(registrations <= crate::master_session::SESSIONS_MAX as i64);
        drop(tx);
        case.close().await;
    }

    #[tokio::test]
    async fn configured_registration_does_not_bypass_current_settings_or_host_epoch() {
        let case = Fixture::new(false).await;
        case.register().await;
        let before = case.snapshot();
        let mut foreign = case.started.host.clone();
        foreign.engine_epoch = "0".repeat(64);
        let result = case
            .runtime(MasterRuntimeCommand::Prepare {
                host: foreign,
                project_id: case.session.project_id.clone(),
                session_id: case.session.session_id.clone(),
                turn_id: case.turn.id.clone(),
            })
            .await;
        assert_eq!(result.err().unwrap().0, "owner_runtime_stale");
        assert_eq!(case.snapshot(), before);
        private_file(
            &case.global,
            CONFIG
                .replace("timeout_ms = 2000", "timeout_ms = 2001")
                .as_bytes(),
        );
        let result = case.runtime(case.prepare()).await;
        assert_eq!(result.err().unwrap().0, "settings_source_conflict");
        assert_eq!(case.snapshot(), before);
        case.close().await;
    }

    #[tokio::test]
    async fn stopping_observation_waits_for_a_pending_settings_publication() {
        let case = Fixture::new(true).await;
        case.register().await;
        let MasterRuntimeResult::Prepared(prepared) = case.runtime(case.prepare()).await.unwrap()
        else {
            panic!("registered local RCLI must prepare the actual turn");
        };
        let basis = owner_read(
            &case.storage,
            &case.started,
            OwnerRead::ModelAccessPage {
                collection: ModelAccessCollection::Accounts,
                cursor: None,
                limit: 1,
            },
        )
        .await;
        let OwnerData::ModelAccessPage { source_hash, .. } = basis.data else {
            panic!("actual model source basis missing");
        };
        let db = Connection::open(&case.path).unwrap();
        db.execute_batch("CREATE TRIGGER publication_abort BEFORE UPDATE ON settings_publication BEGIN SELECT RAISE(ABORT, 'actual_journal_abort'); END;").unwrap();
        let (send, receive) = oneshot::channel();
        case.storage
            .sender
            .try_send(Job::OwnerApply(
                case.started.owners[0].admission.clone(),
                OwnerRequest {
                    protocol_version: PROTOCOL_VERSION,
                    request_id: RequestId::new("pending-publication").unwrap(),
                    expected_revision: basis.revision,
                    operation: OwnerOperation::Apply {
                        mutation: OwnerMutation::ModelAccess {
                            expected_config_id: basis.config_id,
                            expected_source_hash: source_hash,
                            edit: crate::config::ModelAccessEdit::AddAccount {
                                account_ref: reference("pending-account"),
                            },
                        },
                    },
                },
                send,
            ))
            .unwrap_or_else(|_| panic!("actual owner mutation queue unavailable"));
        assert!(receive.await.unwrap().is_err());
        db.execute_batch("DROP TRIGGER publication_abort").unwrap();
        drop(db);
        let before = case.snapshot();
        let observed = case
            .runtime(MasterRuntimeCommand::Observe {
                host: case.started.host.clone(),
                project_id: case.session.project_id.clone(),
                session_id: case.session.session_id.clone(),
                turn_id: case.turn.id.clone(),
                attempt_id: prepared.attempt.request.id.clone(),
                observation: MasterRuntimeObservation::Disconnected,
            })
            .await;
        assert_eq!(observed.err().unwrap().0, "settings_publication_pending");
        assert_eq!(case.snapshot(), before);
        let path = case.path.clone();
        let global = case.global.clone();
        let root = case.close().await;
        let (storage, config) =
            Storage::open_with_settings_sources(&path, &global, &root.path().join("project.toml"))
                .await
                .unwrap();
        assert!(
            config
                .settings
                .model_access
                .as_ref()
                .unwrap()
                .accounts
                .contains(&reference("pending-account"))
        );
        let (send, receive) = oneshot::channel();
        storage
            .sender
            .try_send(Job::Close(send))
            .unwrap_or_else(|_| panic!("actual actor close queue unavailable"));
        receive.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn configured_profile_capacity_counts_live_sessions_not_closed_history() {
        let case = Fixture::new(false).await;
        {
            let mut db = Connection::open(&case.path).unwrap();
            db.busy_timeout(std::time::Duration::from_secs(10)).unwrap();
            let tx = db.transaction().unwrap();
            for n in 0..SESSIONS_MAX {
                tx.execute(
                    "INSERT INTO configured_master_profiles(id,session,body) VALUES (?1,?1,'{}')",
                    [format!("closed-{n:02}")],
                )
                .unwrap();
            }
            tx.commit().unwrap();
        }
        let proof = case.register().await;
        let db = case.db();
        let count = |sql: &str| -> i64 { db.query_row(sql, [], |row| row.get(0)).unwrap() };
        assert_eq!(
            count("SELECT COUNT(*) FROM configured_master_profiles"),
            SESSIONS_MAX as i64
        );
        assert_eq!(
            count("SELECT COUNT(*) FROM configured_master_profiles WHERE id='closed-00'"),
            0
        );
        let stored: String = db
            .query_row(
                "SELECT id FROM configured_master_profiles WHERE session=?1",
                [case.session.session_id.as_str()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(stored, proof.registration_id.as_str());
        drop(db);
        case.close().await;
    }

    #[tokio::test]
    async fn a_queued_turn_holds_the_session_until_its_own_cancel_frees_it() {
        let case = Fixture::new(false).await;
        let rejected = |code: &str| OwnerResult::Rejected { code: code.into() };
        let before = case.snapshot();
        assert_eq!(
            case.result(case.submit("second-turn", "Still outstanding."))
                .await,
            rejected("master_turn_outstanding")
        );
        let cancel = |id: &str, turn: &str| {
            case.action(
                id,
                MasterOwnerAction::Cancel {
                    project_id: case.session.project_id.clone(),
                    session_id: case.session.session_id.clone(),
                    turn_id: RequestId::new(turn).unwrap(),
                },
            )
        };
        assert_eq!(
            case.result(cancel("cancel-other", "another-turn")).await,
            rejected("master_turn_not_current")
        );
        let held = case.snapshot();
        assert_eq!((held.turn, held.counts), (before.turn, before.counts));
        assert_eq!(
            case.result(cancel("cancel-turn", case.turn.id.as_str()))
                .await,
            OwnerResult::Applied
        );
        let (session, turns) = case.view().await;
        assert_eq!(session.current_turn, None);
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].id, case.turn.id);
        assert_eq!(turns[0].phase, MasterTurnPhase::Cancelled);
        assert_eq!(turns[0].attempt, None);
        assert_eq!(turns[0].runtime, None);
        assert_eq!(turns[0].reply, None);
        assert_eq!(
            case.result(case.submit("next-turn", "Use the revised requirement."))
                .await,
            OwnerResult::Applied
        );
        let (session, turns) = case.view().await;
        assert_eq!(turns.len(), 2);
        assert_eq!(turns[1].ordinal, 2);
        assert_eq!(turns[1].phase, MasterTurnPhase::Queued);
        assert_eq!(session.current_turn, Some(turns[1].id.clone()));
        case.close().await;
    }

    #[tokio::test]
    async fn closing_a_session_cancels_its_queued_turn_and_refuses_late_input() {
        let case = Fixture::new(false).await;
        let close = case.action(
            "close-session",
            MasterOwnerAction::Close {
                project_id: case.session.project_id.clone(),
                session_id: case.session.session_id.clone(),
            },
        );
        assert_eq!(case.result(close).await, OwnerResult::Applied);
        let (session, turns) = case.view().await;
        assert_eq!(session.phase, MasterSessionPhase::Closed);
        assert_eq!(session.current_turn, None);
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].phase, MasterTurnPhase::Cancelled);
        assert_eq!(turns[0].attempt, None);
        assert_eq!(turns[0].runtime, None);
        assert_eq!(turns[0].reply, None);
        assert_eq!(
            case.result(case.submit("late-turn", "Must not reopen."))
                .await,
            OwnerResult::Rejected {
                code: "master_session_not_active".into()
            }
        );
        assert_eq!(case.view().await.1, turns);
        case.close().await;
    }
}

#[cfg(test)]
mod sign_in_gap_tests {
    use super::sign_in_gap;
    use crate::engine::owner::native_auth::NativeAuthPhase;

    #[test]
    fn a_finished_sign_in_without_an_account_reads_as_not_signed_in() {
        for phase in [
            NativeAuthPhase::UnauthenticatedObserved,
            NativeAuthPhase::Failed,
            NativeAuthPhase::Cancelled,
            NativeAuthPhase::Unavailable,
        ] {
            assert_eq!(
                sign_in_gap(phase),
                Some("master_authentication_unknown"),
                "{phase:?}"
            );
        }
        for phase in [
            NativeAuthPhase::Starting,
            NativeAuthPhase::AwaitingOwner,
            NativeAuthPhase::CheckingAccount,
            NativeAuthPhase::Cancelling,
            NativeAuthPhase::Uncertain,
        ] {
            assert_eq!(
                sign_in_gap(phase),
                Some("master_authentication_stale"),
                "{phase:?}"
            );
        }
        assert_eq!(sign_in_gap(NativeAuthPhase::AuthenticatedObserved), None);
    }
}
