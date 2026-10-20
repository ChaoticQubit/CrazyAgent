use super::{
    check_owner_admission, owner_lookup, owner_record, owner_revision, persist_event_ids,
    random_id, settings,
};
use crate::config::EffectiveConfig;
use crate::conversation::ConversationMutation;
use crate::engine::EngineError;
use crate::engine::owner::{
    Admission, OwnerApplied, OwnerData, OwnerMutation, OwnerOperation, OwnerProvenance, OwnerRead,
    OwnerReadReply, OwnerRequest, OwnerResult, is_hex_id,
};
use crate::master_session::{
    METADATA_BYTES_MAX, MasterCursor, MasterOwnerAction, MasterReason, MasterSessionPhase,
    MasterSessionView, MasterTurnPage, MasterTurnPhase, MasterTurnView, PAGE_BYTES_MAX,
    SESSIONS_MAX, TURNS_MAX, error, validate_action, validate_page, validate_selection,
};
use crate::protocol::{
    ClientId, ConfigId, Cursor, Outcome, PROTOCOL_VERSION, ProjectId, RequestId, SessionId, TaskId,
};
use rusqlite::{Connection, OptionalExtension, Row, Transaction, TransactionBehavior, params};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

const SESSIONS: &str = "CREATE TABLE master_sessions (
    id TEXT PRIMARY KEY CHECK(length(id) BETWEEN 1 AND 128),
    project TEXT NOT NULL CHECK(length(project) BETWEEN 1 AND 128),
    created INTEGER NOT NULL UNIQUE CHECK(created > 0),
    version INTEGER NOT NULL CHECK(version >= created),
    phase TEXT NOT NULL CHECK(phase IN ('configured','closed')),
    body TEXT NOT NULL CHECK(length(CAST(body AS BLOB)) BETWEEN 1 AND 16384)
)";
const CURRENT: &str = "CREATE UNIQUE INDEX master_current_project ON master_sessions(project) WHERE phase='configured'";
const LATEST: &str = "CREATE INDEX master_latest_project ON master_sessions(project,created DESC)";
const TURNS: &str = "CREATE TABLE master_turns (
    session TEXT NOT NULL CHECK(length(session) BETWEEN 1 AND 128),
    ordinal INTEGER NOT NULL CHECK(ordinal BETWEEN 1 AND 1024),
    id TEXT NOT NULL CHECK(length(id) BETWEEN 1 AND 128),
    input INTEGER NOT NULL UNIQUE CHECK(input > 0),
    phase TEXT NOT NULL CHECK(phase IN ('queued','cancelled')),
    body TEXT NOT NULL CHECK(length(CAST(body AS BLOB)) BETWEEN 1 AND 16384),
    PRIMARY KEY(session,ordinal),
    UNIQUE(session,id)
)";
const OUTSTANDING: &str =
    "CREATE UNIQUE INDEX master_outstanding ON master_turns(session) WHERE phase='queued'";
const SESSION_COLUMNS: &str = "CASE WHEN length(CAST(id AS BLOB))<=128 THEN id END,CASE WHEN length(CAST(project AS BLOB))<=128 THEN project END,created,version,CASE WHEN length(phase)<=10 THEN phase END,CASE WHEN length(CAST(body AS BLOB))<=16384 THEN body END";
const TURN_COLUMNS: &str = "CASE WHEN length(CAST(session AS BLOB))<=128 THEN session END,ordinal,CASE WHEN length(CAST(id AS BLOB))<=128 THEN id END,input,CASE WHEN length(phase)<=10 THEN phase END,CASE WHEN length(CAST(body AS BLOB))<=16384 THEN body END";

pub(super) fn initialize(db: &Connection) -> Result<(), EngineError> {
    let tx = db.unchecked_transaction().map_err(error)?;
    let expected: BTreeMap<_, _> = [
        ("master_sessions", SESSIONS),
        ("master_current_project", CURRENT),
        ("master_latest_project", LATEST),
        ("master_turns", TURNS),
        ("master_outstanding", OUTSTANDING),
    ]
    .into_iter()
    .map(|(name, sql)| (name.to_owned(), sql.to_owned()))
    .collect();
    {
        let mut statement = tx.prepare("SELECT CASE WHEN length(name)<=128 THEN name END,CASE WHEN length(CAST(sql AS BLOB))<=4096 THEN sql END FROM sqlite_schema WHERE sql IS NOT NULL AND (name GLOB 'master_*' OR tbl_name IN ('master_sessions','master_turns')) ORDER BY name LIMIT 6").map_err(error)?;
        let actual = statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(error)?
            .collect::<Result<BTreeMap<_, _>, _>>()
            .map_err(error)?;
        if actual.is_empty() {
            for sql in [SESSIONS, CURRENT, LATEST, TURNS, OUTSTANDING] {
                tx.execute_batch(sql).map_err(error)?;
            }
        } else if actual != expected {
            return Err(error("master_schema_conflict"));
        }
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
                    selection: session.selection.clone(),
                },
            },
        },
    };
    if original.mutation_hash()? != session.owner.request_hash {
        return Err(error("master_open_provenance"));
    }
    let mut statement = tx
        .prepare("SELECT CASE WHEN length(CAST(id AS BLOB))<=128 THEN id END FROM master_turns WHERE session=?1 AND phase='queued' LIMIT 2")
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
        if turn.phase != MasterTurnPhase::Queued {
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
    let expected_phase = match turn.phase {
        MasterTurnPhase::Queued if turn.reason.is_none() => "queued",
        MasterTurnPhase::Cancelled if turn.reason == Some(MasterReason::Cancelled) => "cancelled",
        _ => return Err(error("master_runtime_state_unqualified")),
    };
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
        || expected_phase != stored_phase
        || turn.attempt.is_some()
        || turn.runtime.is_some()
        || turn.reply.is_some()
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

fn cancel_turn(tx: &Transaction<'_>, mut turn: MasterTurnView) -> Result<(), EngineError> {
    if turn.phase != MasterTurnPhase::Queued {
        return Err(error("master_turn_not_queued"));
    }
    turn.phase = MasterTurnPhase::Cancelled;
    turn.reason = Some(MasterReason::Cancelled);
    let changed = tx.execute("UPDATE master_turns SET phase='cancelled',body=?1 WHERE session=?2 AND id=?3 AND phase='queued'", params![encode(&turn)?, turn.session_id.as_str(), turn.id.as_str()]).map_err(error)?;
    assert_eq!(changed, 1);
    assert!(turn.attempt.is_none() && turn.runtime.is_none() && turn.reply.is_none());
    Ok(())
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
                "SELECT COUNT(*) FROM (SELECT 1 FROM master_sessions LIMIT 65)",
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
            selection: selection.clone(),
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
        MasterOwnerAction::Open { .. } => return Err(error("master_action_invalid")),
    };
    let Some(mut session) = load_session(tx, id, revision)? else {
        return Ok(reject("master_session_missing"));
    };
    if &session.project_id != project || session.phase != MasterSessionPhase::Configured {
        return Ok(reject("master_session_not_active"));
    }
    match action {
        MasterOwnerAction::Submit { text, .. } => {
            if session.config_id != config.id || session.source_hash != source_hash {
                return Ok(reject("master_source_conflict"));
            }
            if let Err(reason) = validate_selection(config, project, &session.selection) {
                return Ok(reject(&reason.0));
            }
            if session.current_turn.is_some() {
                return Ok(reject("master_turn_outstanding"));
            }
            let (count, maximum): (i64, i64) = tx.query_row("SELECT COUNT(*),COALESCE(MAX(ordinal),0) FROM (SELECT ordinal FROM master_turns WHERE session=?1 ORDER BY ordinal LIMIT 1025)", [id.as_str()], |row| Ok((row.get(0)?, row.get(1)?))).map_err(error)?;
            if count != maximum || count < 0 {
                return Err(error("master_turn_order"));
            }
            if count >= i64::try_from(TURNS_MAX).map_err(error)? {
                return Ok(reject("master_turn_capacity"));
            }
            let input = ConversationMutation::AppendMessage {
                project_id: project.clone(),
                text: text.clone(),
            };
            let result = crate::conversation::apply(tx, &input, revision)?;
            if result != Outcome::Applied {
                return Ok(result);
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
        MasterOwnerAction::Open { .. } => return Err(error("master_action_invalid")),
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
            session: latest(&tx, project_id, revision)?,
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
