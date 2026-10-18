use crate::config::EffectiveConfig;
use crate::engine::EngineError;
use crate::protocol::*;
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KnowledgePolicy {
    pub processing_destinations: Vec<CredentialRef>,
    pub text_bytes_max: usize,
    pub excerpt_bytes_max: usize,
    pub records_max: usize,
    pub queue_capacity: usize,
    pub batch_size: usize,
    pub asset_bytes_max: usize,
    pub asset_sources_max: usize,
}

impl Default for KnowledgePolicy {
    fn default() -> Self {
        Self {
            processing_destinations: vec![],
            text_bytes_max: 4096,
            excerpt_bytes_max: 8192,
            records_max: 512,
            queue_capacity: 32,
            batch_size: 8,
            asset_bytes_max: 16384,
            asset_sources_max: 64,
        }
    }
}

impl KnowledgePolicy {
    pub fn is_default(&self) -> bool {
        self == &Self::default()
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.processing_destinations.len() > 64
            || self
                .processing_destinations
                .iter()
                .collect::<BTreeSet<_>>()
                .len()
                != self.processing_destinations.len()
            || !(1..=4096).contains(&self.text_bytes_max)
            || !(1..=16384).contains(&self.excerpt_bytes_max)
            || !(1..=4096).contains(&self.records_max)
            || !(1..=128).contains(&self.queue_capacity)
            || !(1..=8).contains(&self.batch_size)
            || !(1..=32768).contains(&self.asset_bytes_max)
            || !(1..=128).contains(&self.asset_sources_max)
        {
            return Err("knowledge_setting_bounds".into());
        }
        assert!(self.batch_size > 0);
        assert!(self.records_max > 0);
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum KnowledgeRole {
    Reader,
    Contributor,
    Curator,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Principal {
    pub id: RequestId,
    pub project_id: ProjectId,
    pub task_id: TaskId,
    pub session_id: SessionId,
    pub role: KnowledgeRole,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CaptureCategory {
    UserMessage,
    AssistantMessage,
    ToolInput,
    ToolOutput,
    Lifecycle,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActivityInput {
    pub id: RequestId,
    pub source_id: RequestId,
    pub category: CaptureCategory,
    pub text: String,
    pub gaps: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActivityRecord {
    pub input: ActivityInput,
    pub ordinal: u64,
    pub project_id: ProjectId,
    pub session_id: SessionId,
    pub task_id: TaskId,
    pub config_id: ConfigId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NominationInput {
    pub id: RequestId,
    pub activity_ids: Vec<RequestId>,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Nomination {
    pub input: NominationInput,
    pub principal: Principal,
    pub source_ids: Vec<RequestId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum KnowledgeStatus {
    Provisional,
    Confirmed,
    Rejected,
    Removed,
    Retracted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KnowledgeEntry {
    pub id: RequestId,
    pub project_id: ProjectId,
    pub version: u64,
    pub content_hash: String,
    pub text: String,
    pub status: KnowledgeStatus,
    pub nomination_id: RequestId,
    pub activity_ids: Vec<RequestId>,
    pub source_ids: Vec<RequestId>,
    pub curator_session: SessionId,
    pub config_id: ConfigId,
    pub revision: Cursor,
    pub human_reviewed: bool,
}

impl KnowledgeEntry {
    fn assert_consistent(&self) {
        assert!(self.version > 0);
        assert_eq!(self.content_hash, hash(self.text.as_bytes()));
        assert!(!self.activity_ids.is_empty());
        assert!(!self.source_ids.is_empty());
        assert!(self.revision.value() > 0);
        assert!(self.text.len() <= 4096);
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtractionBatch {
    pub id: RequestId,
    pub project_id: ProjectId,
    pub destination: CredentialRef,
    pub config_id: ConfigId,
    pub activities: Vec<ActivityRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExtractionJob {
    id: RequestId,
    principal_id: RequestId,
    project_id: ProjectId,
    destination: CredentialRef,
    config_id: ConfigId,
    activity_ids: Vec<RequestId>,
    through: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GlobalCandidate {
    pub id: RequestId,
    pub entry_id: RequestId,
    pub project_id: ProjectId,
    pub version: u64,
    pub content_hash: String,
    pub nominator: Principal,
    pub approved_revision: Option<Cursor>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "operation", deny_unknown_fields)]
pub enum KnowledgeAction {
    Nominations {
        principal_id: RequestId,
        after: Option<RequestId>,
    },
    Admit {
        principal: Principal,
    },
    RevokePrincipal {
        principal_id: RequestId,
    },
    Capture {
        principal_id: RequestId,
        activity: ActivityInput,
    },
    Nominate {
        principal_id: RequestId,
        nomination: NominationInput,
    },
    Publish {
        principal_id: RequestId,
        nomination_id: RequestId,
        entry_id: RequestId,
        text: String,
    },
    NominateGlobal {
        principal_id: RequestId,
        candidate_id: RequestId,
        entry_id: RequestId,
        version: u64,
        content_hash: String,
    },
    QueueExtraction {
        principal_id: RequestId,
        job_id: RequestId,
        destination: CredentialRef,
    },
    Extraction {
        principal_id: RequestId,
        job_id: RequestId,
    },
    CompleteExtraction {
        principal_id: RequestId,
        job_id: RequestId,
    },
    Read {
        principal_id: RequestId,
        global: bool,
        after: Option<RequestId>,
    },
    Owner {
        action: HumanReview,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "operation", deny_unknown_fields)]
pub enum HumanReview {
    GlobalInbox {
        project_id: ProjectId,
        after: Option<RequestId>,
    },
    Review {
        entry_id: RequestId,
        version: u64,
        decision: ReviewDecision,
    },
    ApproveGlobal {
        candidate_id: RequestId,
        version: u64,
        content_hash: String,
    },
    RevokeSource {
        project_id: ProjectId,
        source_id: RequestId,
    },
    DeleteActivity {
        project_id: ProjectId,
        activity_id: RequestId,
    },
    Inbox {
        project_id: ProjectId,
        after: Option<RequestId>,
    },
    History {
        entry_id: RequestId,
        after_version: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "decision", deny_unknown_fields)]
pub enum ReviewDecision {
    Keep,
    Edit { text: String },
    Reject,
    Remove,
}

pub(crate) fn error(value: impl std::fmt::Display) -> EngineError {
    EngineError(value.to_string())
}

pub(crate) fn reject(code: &str) -> Result<Outcome, EngineError> {
    Ok(Outcome::Rejected { code: code.into() })
}

pub(crate) fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub(crate) fn initialize(connection: &Connection) -> Result<(), EngineError> {
    connection.execute_batch("CREATE TABLE IF NOT EXISTS knowledge_principals
        (id TEXT PRIMARY KEY, body TEXT NOT NULL, revoked INTEGER NOT NULL DEFAULT 0);
        CREATE TABLE IF NOT EXISTS knowledge_sources
        (project TEXT NOT NULL, id TEXT NOT NULL, revoked INTEGER NOT NULL DEFAULT 0,
        PRIMARY KEY(project,id));
        CREATE TABLE IF NOT EXISTS knowledge_activity
        (ordinal INTEGER PRIMARY KEY AUTOINCREMENT, id TEXT NOT NULL UNIQUE,
        project TEXT NOT NULL, source TEXT NOT NULL, body TEXT NOT NULL);
        CREATE INDEX IF NOT EXISTS knowledge_activity_project ON knowledge_activity(project,ordinal);
        CREATE TABLE IF NOT EXISTS knowledge_nominations
        (id TEXT PRIMARY KEY, project TEXT NOT NULL, body TEXT NOT NULL);
        CREATE INDEX IF NOT EXISTS knowledge_nominations_project ON knowledge_nominations(project,id);
        CREATE TABLE IF NOT EXISTS knowledge_entries
        (id TEXT NOT NULL, version INTEGER NOT NULL, project TEXT NOT NULL, body TEXT NOT NULL,
        PRIMARY KEY(id,version));
        CREATE TABLE IF NOT EXISTS knowledge_heads
        (id TEXT PRIMARY KEY, version INTEGER NOT NULL, project TEXT NOT NULL);
        CREATE INDEX IF NOT EXISTS knowledge_heads_project ON knowledge_heads(project,id);
        CREATE TABLE IF NOT EXISTS knowledge_globals
        (id TEXT PRIMARY KEY, entry TEXT NOT NULL, version INTEGER NOT NULL, hash TEXT NOT NULL,
        approved INTEGER NOT NULL DEFAULT 0, body TEXT NOT NULL);
        CREATE INDEX IF NOT EXISTS knowledge_global_approval ON knowledge_globals(entry,version,hash,approved);
        CREATE TABLE IF NOT EXISTS knowledge_jobs
        (id TEXT PRIMARY KEY, project TEXT NOT NULL, destination TEXT NOT NULL, body TEXT NOT NULL,
        UNIQUE(project,destination));
        CREATE TABLE IF NOT EXISTS knowledge_watermarks
        (project TEXT NOT NULL, destination TEXT NOT NULL, ordinal INTEGER NOT NULL,
        PRIMARY KEY(project,destination));").map_err(error)
}

pub(crate) fn capacity(
    tx: &Transaction<'_>,
    table: &str,
    maximum: usize,
) -> Result<(), EngineError> {
    let count: i64 = tx
        .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
        .map_err(error)?;
    if count >= maximum as i64 {
        return Err(error("knowledge_record_capacity"));
    }
    assert!(count < maximum as i64);
    assert!(!tx.is_autocommit());
    Ok(())
}

pub(crate) fn principal(tx: &Transaction<'_>, id: &RequestId) -> Result<Principal, EngineError> {
    let body: Option<String> = tx
        .query_row(
            "SELECT body FROM knowledge_principals WHERE id=?1 AND revoked=0",
            [id.as_str()],
            |r| r.get(0),
        )
        .optional()
        .map_err(error)?;
    let principal: Principal =
        serde_json::from_str(&body.ok_or_else(|| error("knowledge_principal_denied"))?)
            .map_err(error)?;
    let context = crate::storage::task_context(tx, &principal.task_id)?
        .ok_or_else(|| error("task_missing"))?;
    if context.project_id != principal.project_id {
        return Err(error("knowledge_scope_denied"));
    }
    assert_eq!(&principal.id, id);
    assert_eq!(context.task_id, principal.task_id);
    Ok(principal)
}

pub(crate) fn policy(
    tx: &Transaction<'_>,
    principal: &Principal,
    current: &EffectiveConfig,
) -> Result<EffectiveConfig, EngineError> {
    let context = crate::storage::task_context(tx, &principal.task_id)?
        .ok_or_else(|| error("task_missing"))?;
    let body: String = tx
        .query_row(
            "SELECT body FROM configs WHERE id=?1",
            [context.config_id.as_str()],
            |r| r.get(0),
        )
        .map_err(error)?;
    let mut pinned: EffectiveConfig = serde_json::from_str(&body).map_err(error)?;
    pinned.validate().map_err(error)?;
    pinned
        .settings
        .revoked_destinations
        .extend(current.settings.revoked_destinations.iter().cloned());
    pinned
        .settings
        .allowed_destinations
        .retain(|d| context.allowed_destinations.contains(d));
    pinned
        .settings
        .knowledge
        .processing_destinations
        .retain(|d| {
            current
                .settings
                .knowledge
                .processing_destinations
                .contains(d)
        });
    assert_eq!(pinned.id, context.config_id);
    assert_eq!(principal.project_id, context.project_id);
    Ok(pinned)
}

pub(crate) fn is_mutation(action: &KnowledgeAction) -> bool {
    !matches!(
        action,
        KnowledgeAction::Read { .. }
            | KnowledgeAction::Nominations { .. }
            | KnowledgeAction::Extraction { .. }
            | KnowledgeAction::Owner {
                action: HumanReview::Inbox { .. }
                    | HumanReview::History { .. }
                    | HumanReview::GlobalInbox { .. }
            }
    )
}

pub(crate) fn execute(
    tx: &Transaction<'_>,
    current: &EffectiveConfig,
    action: &KnowledgeAction,
    revision: u64,
) -> Result<Outcome, EngineError> {
    match action {
        KnowledgeAction::Admit { principal } => admit(tx, current, principal),
        KnowledgeAction::RevokePrincipal { principal_id } => {
            if tx
                .execute(
                    "UPDATE knowledge_principals SET revoked=1 WHERE id=?1 AND revoked=0",
                    [principal_id.as_str()],
                )
                .map_err(error)?
                == 0
            {
                return reject("knowledge_principal_denied");
            }
            Ok(Outcome::Applied)
        }
        KnowledgeAction::Owner { action } => owner(tx, current, action, revision),
        _ => execute_scoped(tx, current, action, revision),
    }
}

fn admit(
    tx: &Transaction<'_>,
    current: &EffectiveConfig,
    p: &Principal,
) -> Result<Outcome, EngineError> {
    let Some(context) = crate::storage::task_context(tx, &p.task_id)? else {
        return reject("task_missing");
    };
    if context.project_id != p.project_id {
        return reject("knowledge_scope_denied");
    }
    capacity(
        tx,
        "knowledge_principals",
        current.settings.knowledge.records_max,
    )?;
    tx.execute(
        "INSERT INTO knowledge_principals(id,body) VALUES (?1,?2)",
        params![p.id.as_str(), serde_json::to_string(p).map_err(error)?],
    )
    .map_err(error)?;
    assert_eq!(context.project_id, p.project_id);
    assert!(!tx.is_autocommit());
    Ok(Outcome::Applied)
}

fn action_principal(action: &KnowledgeAction) -> &RequestId {
    match action {
        KnowledgeAction::Nominations { principal_id, .. } => principal_id,
        KnowledgeAction::Capture { principal_id, .. }
        | KnowledgeAction::Nominate { principal_id, .. }
        | KnowledgeAction::Publish { principal_id, .. }
        | KnowledgeAction::NominateGlobal { principal_id, .. }
        | KnowledgeAction::QueueExtraction { principal_id, .. }
        | KnowledgeAction::Extraction { principal_id, .. }
        | KnowledgeAction::CompleteExtraction { principal_id, .. }
        | KnowledgeAction::Read { principal_id, .. } => principal_id,
        _ => unreachable!("owner actions are dispatched before scoped actions"),
    }
}

fn execute_scoped(
    tx: &Transaction<'_>,
    current: &EffectiveConfig,
    action: &KnowledgeAction,
    revision: u64,
) -> Result<Outcome, EngineError> {
    let p = match principal(tx, action_principal(action)) {
        Ok(p) => p,
        Err(e) => return reject(&e.0),
    };
    let config = policy(tx, &p, current)?;
    if let KnowledgeAction::Read { global, after, .. } = action {
        return read(tx, &p, &config.settings.knowledge, *global, after.as_ref());
    }
    let curator = matches!(
        action,
        KnowledgeAction::Publish { .. }
            | KnowledgeAction::Nominations { .. }
            | KnowledgeAction::NominateGlobal { .. }
            | KnowledgeAction::QueueExtraction { .. }
            | KnowledgeAction::Extraction { .. }
            | KnowledgeAction::CompleteExtraction { .. }
    );
    if p.role == KnowledgeRole::Reader || (curator && p.role != KnowledgeRole::Curator) {
        return reject("knowledge_role_denied");
    }
    match action {
        KnowledgeAction::Nominations { after, .. } => nominations(tx, &config, &p, after.as_ref()),
        KnowledgeAction::Capture { activity, .. } => capture(tx, &config, &p, activity),
        KnowledgeAction::Nominate { nomination, .. } => nominate(tx, &config, &p, nomination),
        KnowledgeAction::Publish {
            nomination_id,
            entry_id,
            text,
            ..
        } => publish(tx, &config, &p, nomination_id, entry_id, text, revision),
        KnowledgeAction::NominateGlobal {
            candidate_id,
            entry_id,
            version,
            content_hash,
            ..
        } => nominate_global(
            tx,
            &config,
            &p,
            candidate_id,
            entry_id,
            *version,
            content_hash,
        ),
        KnowledgeAction::QueueExtraction {
            job_id,
            destination,
            ..
        } => queue(tx, &config, &p, job_id, destination),
        KnowledgeAction::Extraction { job_id, .. } => extraction(tx, &config, &p, job_id),
        KnowledgeAction::CompleteExtraction { job_id, .. } => complete(tx, &config, &p, job_id),
        _ => unreachable!("read and owner actions were dispatched"),
    }
}

fn text_valid(text: &str, maximum: usize) -> bool {
    !text.trim().is_empty() && text.len() <= maximum && !text.contains('\0')
}

fn source_live(
    tx: &Transaction<'_>,
    project: &ProjectId,
    source: &RequestId,
) -> Result<bool, EngineError> {
    tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM knowledge_sources WHERE project=?1 AND id=?2 AND revoked=0)",
        params![project.as_str(), source.as_str()],
        |r| r.get(0),
    )
    .map_err(error)
}

fn capture(
    tx: &Transaction<'_>,
    config: &EffectiveConfig,
    p: &Principal,
    input: &ActivityInput,
) -> Result<Outcome, EngineError> {
    let bounds = &config.settings.knowledge;
    let mut record = match captured_record(config, p, input) {
        Ok(record) => record,
        Err(e) => return reject(&e.0),
    };
    capacity(tx, "knowledge_activity", bounds.records_max)?;
    let exists: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM knowledge_sources WHERE project=?1 AND id=?2)",
            params![p.project_id.as_str(), input.source_id.as_str()],
            |r| r.get(0),
        )
        .map_err(error)?;
    if exists && !source_live(tx, &p.project_id, &input.source_id)? {
        return reject("knowledge_source_revoked");
    }
    if !exists {
        capacity(tx, "knowledge_sources", bounds.records_max)?;
        tx.execute(
            "INSERT INTO knowledge_sources(project,id) VALUES (?1,?2)",
            params![p.project_id.as_str(), input.source_id.as_str()],
        )
        .map_err(error)?;
    }
    tx.execute(
        "INSERT INTO knowledge_activity(id,project,source,body) VALUES (?1,?2,?3,'')",
        params![
            input.id.as_str(),
            p.project_id.as_str(),
            input.source_id.as_str()
        ],
    )
    .map_err(error)?;
    let ordinal = u64::try_from(tx.last_insert_rowid()).map_err(error)?;
    record.ordinal = ordinal;
    tx.execute(
        "UPDATE knowledge_activity SET body=?1 WHERE id=?2",
        params![
            serde_json::to_string(&record).map_err(error)?,
            input.id.as_str()
        ],
    )
    .map_err(error)?;
    assert!(ordinal > 0);
    assert!(!tx.is_autocommit());
    Ok(Outcome::Applied)
}

fn captured_record(
    config: &EffectiveConfig,
    p: &Principal,
    input: &ActivityInput,
) -> Result<ActivityRecord, EngineError> {
    if !text_valid(&input.text, config.settings.knowledge.excerpt_bytes_max)
        || input.gaps.len() > 16
        || input.gaps.iter().any(|g| !text_valid(g, 256))
    {
        return Err(error("invalid_activity"));
    }
    let record = ActivityRecord {
        input: input.clone(),
        ordinal: i64::MAX as u64,
        project_id: p.project_id.clone(),
        session_id: p.session_id.clone(),
        task_id: p.task_id.clone(),
        config_id: config.id.clone(),
    };
    if serde_json::to_vec(&record).map_err(error)?.len() > FRAME_BYTES_MAX - 4096 {
        return Err(error("activity_reply_capacity"));
    }
    Ok(record)
}

fn activity(
    tx: &Transaction<'_>,
    id: &RequestId,
    project: &ProjectId,
) -> Result<ActivityRecord, EngineError> {
    let body: Option<String> = tx
        .query_row(
            "SELECT a.body FROM knowledge_activity a JOIN knowledge_sources s
        ON s.id=a.source AND s.project=a.project WHERE a.id=?1 AND a.project=?2 AND s.revoked=0",
            params![id.as_str(), project.as_str()],
            |r| r.get(0),
        )
        .optional()
        .map_err(error)?;
    let record: ActivityRecord =
        serde_json::from_str(&body.ok_or_else(|| error("knowledge_source_denied"))?)
            .map_err(error)?;
    assert_eq!(&record.project_id, project);
    assert_eq!(&record.input.id, id);
    Ok(record)
}

fn nominate(
    tx: &Transaction<'_>,
    config: &EffectiveConfig,
    p: &Principal,
    input: &NominationInput,
) -> Result<Outcome, EngineError> {
    if !text_valid(&input.text, config.settings.knowledge.text_bytes_max)
        || input.activity_ids.is_empty()
        || input.activity_ids.len() > 16
        || input.activity_ids.iter().collect::<BTreeSet<_>>().len() != input.activity_ids.len()
    {
        return reject("invalid_nomination");
    }
    let mut sources = BTreeSet::new();
    for id in &input.activity_ids {
        let record = match activity(tx, id, &p.project_id) {
            Ok(r) => r,
            Err(e) => return reject(&e.0),
        };
        sources.insert(record.input.source_id);
    }
    capacity(
        tx,
        "knowledge_nominations",
        config.settings.knowledge.records_max,
    )?;
    let nomination = Nomination {
        input: input.clone(),
        principal: p.clone(),
        source_ids: sources.into_iter().collect(),
    };
    tx.execute(
        "INSERT INTO knowledge_nominations VALUES (?1,?2,?3)",
        params![
            input.id.as_str(),
            p.project_id.as_str(),
            serde_json::to_string(&nomination).map_err(error)?
        ],
    )
    .map_err(error)?;
    assert!(!nomination.source_ids.is_empty());
    assert!(!tx.is_autocommit());
    Ok(Outcome::Applied)
}

fn live_entry(tx: &Transaction<'_>, entry: &KnowledgeEntry) -> Result<bool, EngineError> {
    if !matches!(
        entry.status,
        KnowledgeStatus::Provisional | KnowledgeStatus::Confirmed
    ) {
        return Ok(false);
    }
    for source in &entry.source_ids {
        if !source_live(tx, &entry.project_id, source)? {
            return Ok(false);
        }
    }
    Ok(true)
}

fn load_entry(tx: &Transaction<'_>, id: &RequestId) -> Result<Option<KnowledgeEntry>, EngineError> {
    let body: Option<String> = tx
        .query_row(
            "SELECT e.body FROM knowledge_entries e JOIN knowledge_heads h
        ON e.id=h.id AND e.version=h.version WHERE h.id=?1",
            [id.as_str()],
            |r| r.get(0),
        )
        .optional()
        .map_err(error)?;
    body.map(|b| {
        let e: KnowledgeEntry = serde_json::from_str(&b).map_err(error)?;
        e.assert_consistent();
        assert_eq!(&e.id, id);
        Ok(e)
    })
    .transpose()
}

fn save_entry(
    tx: &Transaction<'_>,
    bounds: &KnowledgePolicy,
    entry: &KnowledgeEntry,
) -> Result<Outcome, EngineError> {
    entry.assert_consistent();
    capacity(tx, "knowledge_entries", bounds.records_max)?;
    tx.execute(
        "INSERT INTO knowledge_entries VALUES (?1,?2,?3,?4)",
        params![
            entry.id.as_str(),
            i64::try_from(entry.version).map_err(error)?,
            entry.project_id.as_str(),
            serde_json::to_string(entry).map_err(error)?
        ],
    )
    .map_err(error)?;
    tx.execute("INSERT INTO knowledge_heads VALUES (?1,?2,?3) ON CONFLICT(id) DO UPDATE SET version=excluded.version",
        params![entry.id.as_str(), i64::try_from(entry.version).map_err(error)?, entry.project_id.as_str()]).map_err(error)?;
    assert_eq!(load_entry(tx, &entry.id)?.as_ref(), Some(entry));
    assert!(!tx.is_autocommit());
    Ok(Outcome::Applied)
}

fn publish(
    tx: &Transaction<'_>,
    config: &EffectiveConfig,
    p: &Principal,
    nomination_id: &RequestId,
    entry_id: &RequestId,
    text: &str,
    revision: u64,
) -> Result<Outcome, EngineError> {
    if !text_valid(text, config.settings.knowledge.text_bytes_max) {
        return reject("invalid_knowledge_text");
    }
    let body: Option<String> = tx
        .query_row(
            "SELECT body FROM knowledge_nominations WHERE id=?1",
            [nomination_id.as_str()],
            |r| r.get(0),
        )
        .optional()
        .map_err(error)?;
    let Some(body) = body else {
        return reject("nomination_missing");
    };
    let n: Nomination = serde_json::from_str(&body).map_err(error)?;
    if n.principal.project_id != p.project_id {
        return reject("knowledge_scope_denied");
    }
    for activity_id in &n.input.activity_ids {
        if let Err(e) = activity(tx, activity_id, &p.project_id) {
            return reject(&e.0);
        }
    }
    if load_entry(tx, entry_id)?.is_some() {
        return reject("knowledge_entry_exists");
    }
    let entry = KnowledgeEntry {
        id: entry_id.clone(),
        project_id: p.project_id.clone(),
        version: 1,
        content_hash: hash(text.as_bytes()),
        text: text.into(),
        status: KnowledgeStatus::Provisional,
        nomination_id: nomination_id.clone(),
        activity_ids: n.input.activity_ids,
        source_ids: n.source_ids,
        curator_session: p.session_id.clone(),
        config_id: config.id.clone(),
        revision: Cursor::new(revision + 1),
        human_reviewed: false,
    };
    save_entry(tx, &config.settings.knowledge, &entry)
}

fn nominate_global(
    tx: &Transaction<'_>,
    config: &EffectiveConfig,
    p: &Principal,
    candidate_id: &RequestId,
    entry_id: &RequestId,
    version: u64,
    content_hash: &str,
) -> Result<Outcome, EngineError> {
    let Some(entry) = load_entry(tx, entry_id)? else {
        return reject("knowledge_entry_missing");
    };
    if entry.project_id != p.project_id {
        return reject("knowledge_scope_denied");
    }
    if entry.version != version || entry.content_hash != content_hash {
        return reject("knowledge_version_conflict");
    }
    if !live_entry(tx, &entry)? {
        return reject("knowledge_source_revoked");
    }
    capacity(
        tx,
        "knowledge_globals",
        config.settings.knowledge.records_max,
    )?;
    let candidate = GlobalCandidate {
        id: candidate_id.clone(),
        entry_id: entry_id.clone(),
        project_id: p.project_id.clone(),
        version,
        content_hash: content_hash.into(),
        nominator: p.clone(),
        approved_revision: None,
    };
    tx.execute(
        "INSERT INTO knowledge_globals(id,entry,version,hash,body) VALUES (?1,?2,?3,?4,?5)",
        params![
            candidate_id.as_str(),
            entry_id.as_str(),
            i64::try_from(version).map_err(error)?,
            content_hash,
            serde_json::to_string(&candidate).map_err(error)?
        ],
    )
    .map_err(error)?;
    assert_eq!(candidate.version, entry.version);
    assert!(candidate.approved_revision.is_none());
    Ok(Outcome::Applied)
}

fn read(
    tx: &Transaction<'_>,
    p: &Principal,
    bounds: &KnowledgePolicy,
    global: bool,
    after: Option<&RequestId>,
) -> Result<Outcome, EngineError> {
    let mut statement = tx.prepare("SELECT e.body FROM knowledge_heads h JOIN knowledge_entries e ON h.id=e.id AND h.version=e.version
        WHERE h.id>?1 AND (?2 OR h.project=?3) ORDER BY h.id LIMIT ?4").map_err(error)?;
    let rows = statement
        .query_map(
            params![
                after.map_or("", RequestId::as_str),
                global,
                p.project_id.as_str(),
                4096_i64
            ],
            |r| r.get::<_, String>(0),
        )
        .map_err(error)?;
    let mut entries = Vec::new();
    for body in rows {
        let entry: KnowledgeEntry = serde_json::from_str(&body.map_err(error)?).map_err(error)?;
        entry.assert_consistent();
        if live_entry(tx, &entry)? && (!global || globally_approved(tx, &entry)?) {
            entries.push(entry);
        }
        if entries.len() >= bounds.batch_size {
            break;
        }
    }
    assert!(entries.len() <= bounds.batch_size);
    assert!(
        entries
            .iter()
            .all(|e| global || e.project_id == p.project_id)
    );
    Ok(Outcome::Knowledge {
        entries: bounded_entries(entries)?,
    })
}

fn bounded_entries(entries: Vec<KnowledgeEntry>) -> Result<Vec<KnowledgeEntry>, EngineError> {
    let mut result = Vec::new();
    let mut bytes = 0;
    for entry in entries {
        let size = serde_json::to_vec(&entry).map_err(error)?.len() + 1;
        if bytes + size > FRAME_BYTES_MAX - 2048 {
            break;
        }
        bytes += size;
        result.push(entry);
    }
    assert!(bytes <= FRAME_BYTES_MAX - 2048);
    assert!(result.len() <= 8);
    Ok(result)
}

fn globally_approved(tx: &Transaction<'_>, entry: &KnowledgeEntry) -> Result<bool, EngineError> {
    tx.query_row("SELECT EXISTS(SELECT 1 FROM knowledge_globals WHERE entry=?1 AND version=?2 AND hash=?3 AND approved=1)",
        params![entry.id.as_str(), i64::try_from(entry.version).map_err(error)?, entry.content_hash], |r| r.get(0)).map_err(error)
}

fn destination_allowed(config: &EffectiveConfig, destination: &CredentialRef) -> bool {
    config.settings.allowed_destinations.contains(destination)
        && config
            .settings
            .knowledge
            .processing_destinations
            .contains(destination)
        && !config.settings.revoked_destinations.contains(destination)
}

fn queue(
    tx: &Transaction<'_>,
    config: &EffectiveConfig,
    p: &Principal,
    job_id: &RequestId,
    destination: &CredentialRef,
) -> Result<Outcome, EngineError> {
    if !destination_allowed(config, destination) {
        return reject("knowledge_destination_denied");
    }
    let count: i64 = tx
        .query_row("SELECT COUNT(*) FROM knowledge_jobs", [], |r| r.get(0))
        .map_err(error)?;
    let pending: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM knowledge_jobs WHERE project=?1 AND destination=?2)",
            params![p.project_id.as_str(), destination.as_str()],
            |r| r.get(0),
        )
        .map_err(error)?;
    if count >= config.settings.knowledge.queue_capacity as i64 || pending {
        return reject("knowledge_queue_capacity");
    }
    let after: i64 = tx
        .query_row(
            "SELECT ordinal FROM knowledge_watermarks WHERE project=?1 AND destination=?2",
            params![p.project_id.as_str(), destination.as_str()],
            |r| r.get(0),
        )
        .optional()
        .map_err(error)?
        .unwrap_or(0);
    let records = next_activities(tx, config, p, after)?;
    let Some(last) = records.last() else {
        return reject("knowledge_no_activity");
    };
    let through = last.ordinal;
    let job = ExtractionJob {
        id: job_id.clone(),
        principal_id: p.id.clone(),
        project_id: p.project_id.clone(),
        destination: destination.clone(),
        config_id: config.id.clone(),
        activity_ids: records.into_iter().map(|r| r.input.id).collect(),
        through,
    };
    tx.execute(
        "INSERT INTO knowledge_jobs VALUES (?1,?2,?3,?4)",
        params![
            job.id.as_str(),
            p.project_id.as_str(),
            destination.as_str(),
            serde_json::to_string(&job).map_err(error)?
        ],
    )
    .map_err(error)?;
    assert!(through > after as u64);
    assert!(job.activity_ids.len() <= config.settings.knowledge.batch_size);
    Ok(Outcome::Applied)
}

fn next_activities(
    tx: &Transaction<'_>,
    config: &EffectiveConfig,
    p: &Principal,
    after: i64,
) -> Result<Vec<ActivityRecord>, EngineError> {
    let mut statement = tx.prepare("SELECT a.body FROM knowledge_activity a JOIN knowledge_sources s ON a.source=s.id AND a.project=s.project
        WHERE a.project=?1 AND a.ordinal>?2 AND s.revoked=0 ORDER BY a.ordinal LIMIT ?3").map_err(error)?;
    let rows = statement
        .query_map(
            params![
                p.project_id.as_str(),
                after,
                config.settings.knowledge.batch_size as i64
            ],
            |r| r.get::<_, String>(0),
        )
        .map_err(error)?;
    let mut records = Vec::new();
    let mut bytes = 0;
    for body in rows {
        let record: ActivityRecord = serde_json::from_str(&body.map_err(error)?).map_err(error)?;
        let size = serde_json::to_vec(&record).map_err(error)?.len();
        if bytes + size > FRAME_BYTES_MAX - 4096 {
            break;
        }
        bytes += size;
        records.push(record);
    }
    assert!(records.len() <= config.settings.knowledge.batch_size);
    assert!(records.iter().all(|r| r.ordinal > after as u64));
    Ok(records)
}

fn job(
    tx: &Transaction<'_>,
    config: &EffectiveConfig,
    p: &Principal,
    job_id: &RequestId,
) -> Result<ExtractionBatch, EngineError> {
    let body: Option<String> = tx
        .query_row(
            "SELECT body FROM knowledge_jobs WHERE id=?1",
            [job_id.as_str()],
            |r| r.get(0),
        )
        .optional()
        .map_err(error)?;
    let j: ExtractionJob =
        serde_json::from_str(&body.ok_or_else(|| error("knowledge_job_missing"))?)
            .map_err(error)?;
    if j.project_id != p.project_id || j.principal_id != p.id || j.config_id != config.id {
        return Err(error("knowledge_scope_denied"));
    }
    if !destination_allowed(config, &j.destination) {
        return Err(error("knowledge_destination_denied"));
    }
    let activities = j
        .activity_ids
        .iter()
        .map(|id| activity(tx, id, &p.project_id))
        .collect::<Result<Vec<_>, _>>()?;
    assert!(!activities.is_empty());
    assert_eq!(activities.last().unwrap().ordinal, j.through);
    Ok(ExtractionBatch {
        id: j.id,
        project_id: j.project_id,
        destination: j.destination,
        config_id: j.config_id,
        activities,
    })
}

fn extraction(
    tx: &Transaction<'_>,
    config: &EffectiveConfig,
    p: &Principal,
    job_id: &RequestId,
) -> Result<Outcome, EngineError> {
    match job(tx, config, p, job_id) {
        Ok(batch) => Ok(Outcome::Extraction { batch }),
        Err(e) => reject(&e.0),
    }
}

fn complete(
    tx: &Transaction<'_>,
    config: &EffectiveConfig,
    p: &Principal,
    job_id: &RequestId,
) -> Result<Outcome, EngineError> {
    let batch = match job(tx, config, p, job_id) {
        Ok(batch) => batch,
        Err(e) => return reject(&e.0),
    };
    let through = batch.activities.last().unwrap().ordinal;
    let exists: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM knowledge_watermarks WHERE project=?1 AND destination=?2)",
            params![p.project_id.as_str(), batch.destination.as_str()],
            |r| r.get(0),
        )
        .map_err(error)?;
    if !exists {
        capacity(
            tx,
            "knowledge_watermarks",
            config.settings.knowledge.records_max,
        )?;
    }
    tx.execute("INSERT INTO knowledge_watermarks VALUES (?1,?2,?3) ON CONFLICT(project,destination) DO UPDATE SET ordinal=excluded.ordinal",
        params![p.project_id.as_str(), batch.destination.as_str(), i64::try_from(through).map_err(error)?]).map_err(error)?;
    let count = tx
        .execute("DELETE FROM knowledge_jobs WHERE id=?1", [job_id.as_str()])
        .map_err(error)?;
    assert_eq!(count, 1);
    assert!(through > 0);
    Ok(Outcome::Applied)
}

fn owner(
    tx: &Transaction<'_>,
    config: &EffectiveConfig,
    action: &HumanReview,
    revision: u64,
) -> Result<Outcome, EngineError> {
    match action {
        HumanReview::GlobalInbox { project_id, after } => {
            global_inbox(tx, config, project_id, after.as_ref())
        }
        HumanReview::Review {
            entry_id,
            version,
            decision,
        } => review(tx, config, entry_id, *version, decision, revision),
        HumanReview::ApproveGlobal {
            candidate_id,
            version,
            content_hash,
        } => approve(tx, candidate_id, *version, content_hash, revision),
        HumanReview::RevokeSource {
            project_id,
            source_id,
        } => revoke_source(tx, config, project_id, source_id, revision),
        HumanReview::DeleteActivity {
            project_id,
            activity_id,
        } => delete_activity(tx, config, project_id, activity_id, revision),
        HumanReview::Inbox { project_id, after } => inbox(tx, config, project_id, after.as_ref()),
        HumanReview::History {
            entry_id,
            after_version,
        } => history(tx, config, entry_id, *after_version),
    }
}

fn nominations(
    tx: &Transaction<'_>,
    config: &EffectiveConfig,
    p: &Principal,
    after: Option<&RequestId>,
) -> Result<Outcome, EngineError> {
    let mut statement = tx.prepare("SELECT body FROM knowledge_nominations WHERE project=?1 AND id>?2 ORDER BY id LIMIT 4096").map_err(error)?;
    let rows = statement
        .query_map(
            params![p.project_id.as_str(), after.map_or("", RequestId::as_str)],
            |r| r.get::<_, String>(0),
        )
        .map_err(error)?;
    let mut nominations = Vec::new();
    let mut bytes = 0;
    for body in rows {
        let body = body.map_err(error)?;
        let n: Nomination = serde_json::from_str(&body).map_err(error)?;
        let mut live = true;
        for source in &n.source_ids {
            live &= source_live(tx, &p.project_id, source)?;
        }
        if !live {
            continue;
        }
        if bytes + body.len() > FRAME_BYTES_MAX - 2048 {
            break;
        }
        bytes += body.len();
        assert_eq!(n.principal.project_id, p.project_id);
        nominations.push(n);
        if nominations.len() >= config.settings.knowledge.batch_size {
            break;
        }
    }
    assert!(nominations.len() <= config.settings.knowledge.batch_size);
    assert!(bytes <= FRAME_BYTES_MAX - 2048);
    Ok(Outcome::Nominations { nominations })
}

fn global_inbox(
    tx: &Transaction<'_>,
    config: &EffectiveConfig,
    project: &ProjectId,
    after: Option<&RequestId>,
) -> Result<Outcome, EngineError> {
    let mut statement = tx
        .prepare(
            "SELECT g.body FROM knowledge_globals g JOIN knowledge_heads h ON g.entry=h.id
        WHERE h.project=?1 AND g.id>?2 ORDER BY g.id LIMIT ?3",
        )
        .map_err(error)?;
    let rows = statement
        .query_map(
            params![
                project.as_str(),
                after.map_or("", RequestId::as_str),
                config.settings.knowledge.batch_size as i64
            ],
            |r| r.get::<_, String>(0),
        )
        .map_err(error)?;
    let candidates = rows
        .map(|body| serde_json::from_str(&body.map_err(error)?).map_err(error))
        .collect::<Result<Vec<GlobalCandidate>, _>>()?;
    for candidate in &candidates {
        assert_eq!(&candidate.project_id, project);
        assert!(candidate.version > 0);
    }
    Ok(Outcome::GlobalCandidates { candidates })
}

fn delete_activity(
    tx: &Transaction<'_>,
    config: &EffectiveConfig,
    project: &ProjectId,
    activity_id: &RequestId,
    revision: u64,
) -> Result<Outcome, EngineError> {
    let source: Option<String> = tx
        .query_row(
            "SELECT source FROM knowledge_activity WHERE id=?1 AND project=?2",
            params![activity_id.as_str(), project.as_str()],
            |r| r.get(0),
        )
        .optional()
        .map_err(error)?;
    let Some(source) = source else {
        return reject("knowledge_source_denied");
    };
    let source = RequestId::new(source).map_err(error)?;
    if source_live(tx, project, &source)? {
        revoke_source(tx, config, project, &source, revision)?;
    }
    let count = tx
        .execute(
            "DELETE FROM knowledge_activity WHERE project=?1 AND source=?2",
            params![project.as_str(), source.as_str()],
        )
        .map_err(error)?;
    assert!(count > 0);
    assert!(!source_live(tx, project, &source)?);
    Ok(Outcome::Applied)
}

fn review(
    tx: &Transaction<'_>,
    config: &EffectiveConfig,
    id: &RequestId,
    version: u64,
    decision: &ReviewDecision,
    revision: u64,
) -> Result<Outcome, EngineError> {
    let Some(mut entry) = load_entry(tx, id)? else {
        return reject("knowledge_entry_missing");
    };
    if entry.version != version {
        return reject("knowledge_version_conflict");
    }
    if !live_entry(tx, &entry)? {
        return reject("knowledge_source_revoked");
    }
    entry.status = match decision {
        ReviewDecision::Keep => KnowledgeStatus::Confirmed,
        ReviewDecision::Edit { text } => {
            if !text_valid(text, config.settings.knowledge.text_bytes_max) {
                return reject("invalid_knowledge_text");
            }
            entry.text = text.clone();
            entry.content_hash = hash(text.as_bytes());
            KnowledgeStatus::Confirmed
        }
        ReviewDecision::Reject => KnowledgeStatus::Rejected,
        ReviewDecision::Remove => KnowledgeStatus::Removed,
    };
    entry.version += 1;
    entry.human_reviewed = true;
    entry.revision = Cursor::new(revision + 1);
    save_entry(tx, &config.settings.knowledge, &entry)
}

fn approve(
    tx: &Transaction<'_>,
    id: &RequestId,
    version: u64,
    content_hash: &str,
    revision: u64,
) -> Result<Outcome, EngineError> {
    let body: Option<String> = tx
        .query_row(
            "SELECT body FROM knowledge_globals WHERE id=?1",
            [id.as_str()],
            |r| r.get(0),
        )
        .optional()
        .map_err(error)?;
    let Some(body) = body else {
        return reject("knowledge_candidate_missing");
    };
    let mut candidate: GlobalCandidate = serde_json::from_str(&body).map_err(error)?;
    let Some(entry) = load_entry(tx, &candidate.entry_id)? else {
        return reject("knowledge_entry_missing");
    };
    if candidate.version != version
        || candidate.content_hash != content_hash
        || entry.version != version
        || entry.content_hash != content_hash
    {
        return reject("knowledge_version_conflict");
    }
    if !live_entry(tx, &entry)? {
        return reject("knowledge_source_revoked");
    }
    candidate.approved_revision = Some(Cursor::new(revision + 1));
    tx.execute(
        "UPDATE knowledge_globals SET body=?1,approved=1 WHERE id=?2",
        params![
            serde_json::to_string(&candidate).map_err(error)?,
            id.as_str()
        ],
    )
    .map_err(error)?;
    assert_eq!(candidate.version, entry.version);
    assert!(candidate.approved_revision.is_some());
    Ok(Outcome::Applied)
}

fn revoke_source(
    tx: &Transaction<'_>,
    config: &EffectiveConfig,
    project: &ProjectId,
    source: &RequestId,
    revision: u64,
) -> Result<Outcome, EngineError> {
    if !source_live(tx, project, source)? {
        return reject("knowledge_source_denied");
    }
    let mut statement = tx.prepare("SELECT e.body FROM knowledge_entries e JOIN knowledge_heads h ON e.id=h.id AND e.version=h.version
        WHERE h.project=?1 ORDER BY h.id LIMIT 4096").map_err(error)?;
    let rows = statement
        .query_map([project.as_str()], |r| r.get::<_, String>(0))
        .map_err(error)?;
    for body in rows {
        let mut entry: KnowledgeEntry =
            serde_json::from_str(&body.map_err(error)?).map_err(error)?;
        if entry.source_ids.contains(source)
            && matches!(
                entry.status,
                KnowledgeStatus::Provisional | KnowledgeStatus::Confirmed
            )
        {
            entry.version += 1;
            entry.status = KnowledgeStatus::Retracted;
            entry.revision = Cursor::new(revision + 1);
            let mut reserved = config.settings.knowledge.clone();
            reserved.records_max = 8192;
            save_entry(tx, &reserved, &entry)?;
        }
    }
    tx.execute(
        "UPDATE knowledge_sources SET revoked=1 WHERE project=?1 AND id=?2",
        params![project.as_str(), source.as_str()],
    )
    .map_err(error)?;
    tx.execute(
        "DELETE FROM knowledge_jobs WHERE project=?1",
        [project.as_str()],
    )
    .map_err(error)?;
    assert!(!source_live(tx, project, source)?);
    assert!(!tx.is_autocommit());
    Ok(Outcome::Applied)
}

fn inbox(
    tx: &Transaction<'_>,
    config: &EffectiveConfig,
    project: &ProjectId,
    after: Option<&RequestId>,
) -> Result<Outcome, EngineError> {
    let mut statement = tx.prepare("SELECT e.body FROM knowledge_entries e JOIN knowledge_heads h ON e.id=h.id AND e.version=h.version
        WHERE h.project=?1 AND h.id>?2 ORDER BY h.id LIMIT ?3").map_err(error)?;
    let rows = statement
        .query_map(
            params![
                project.as_str(),
                after.map_or("", RequestId::as_str),
                config.settings.knowledge.batch_size as i64
            ],
            |r| r.get::<_, String>(0),
        )
        .map_err(error)?;
    let entries = rows
        .map(|body| serde_json::from_str(&body.map_err(error)?).map_err(error))
        .collect::<Result<Vec<KnowledgeEntry>, _>>()?;
    for entry in &entries {
        entry.assert_consistent();
        assert_eq!(&entry.project_id, project);
    }
    Ok(Outcome::Knowledge {
        entries: bounded_entries(entries)?,
    })
}

fn history(
    tx: &Transaction<'_>,
    config: &EffectiveConfig,
    id: &RequestId,
    after: u64,
) -> Result<Outcome, EngineError> {
    let mut statement = tx.prepare("SELECT body FROM knowledge_entries WHERE id=?1 AND version>?2 ORDER BY version LIMIT ?3").map_err(error)?;
    let rows = statement
        .query_map(
            params![
                id.as_str(),
                after.min(i64::MAX as u64) as i64,
                config.settings.knowledge.batch_size as i64
            ],
            |r| r.get::<_, String>(0),
        )
        .map_err(error)?;
    let entries = rows
        .map(|body| serde_json::from_str(&body.map_err(error)?).map_err(error))
        .collect::<Result<Vec<KnowledgeEntry>, _>>()?;
    for entry in &entries {
        entry.assert_consistent();
        assert_eq!(&entry.id, id);
    }
    Ok(Outcome::Knowledge {
        entries: bounded_entries(entries)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persisted_knowledge_invariants_are_runtime_assertions() {
        let entry = KnowledgeEntry {
            id: RequestId::new("entry").unwrap(),
            project_id: ProjectId::new("project").unwrap(),
            version: 1,
            text: "fact".into(),
            content_hash: hash(b"fact"),
            status: KnowledgeStatus::Provisional,
            nomination_id: RequestId::new("nomination").unwrap(),
            activity_ids: vec![RequestId::new("activity").unwrap()],
            source_ids: vec![RequestId::new("source").unwrap()],
            curator_session: SessionId::new("curator").unwrap(),
            config_id: ConfigId::new("config").unwrap(),
            revision: Cursor::new(1),
            human_reviewed: false,
        };
        entry.assert_consistent();
        for corruption in 0..6 {
            let mut corrupted = entry.clone();
            match corruption {
                0 => corrupted.version = 0,
                1 => corrupted.content_hash = hash(b"substituted"),
                2 => corrupted.activity_ids.clear(),
                3 => corrupted.source_ids.clear(),
                4 => corrupted.revision = Cursor::new(0),
                _ => {
                    corrupted.text = "x".repeat(4097);
                    corrupted.content_hash = hash(corrupted.text.as_bytes());
                }
            }
            assert!(std::panic::catch_unwind(|| corrupted.assert_consistent()).is_err());
        }
    }
}
