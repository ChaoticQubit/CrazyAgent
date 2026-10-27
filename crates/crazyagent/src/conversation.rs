use crate::engine::EngineError;
use crate::engine::owner::{Admission, OwnerData, OwnerProvenance, OwnerResult, is_hex_id};
use crate::protocol::{ConfigId, Cursor, FRAME_BYTES_MAX, Outcome, ProjectId, RequestId};
use rusqlite::{Row, Transaction, params};
use serde::{Deserialize, Serialize};

pub const TEXT_BYTES_MAX: usize = 8192;
pub const ENTRIES_MAX: usize = 1024;
pub const PAGE_ITEMS_MAX: u8 = 32;
const PAGE_BYTES_MAX: usize = FRAME_BYTES_MAX - 4096;

#[cfg(unix)]
const META: &str = "CREATE TABLE conversation_meta (
    version INTEGER PRIMARY KEY CHECK(version = 1)
)";
#[cfg(unix)]
const ENTRIES: &str = "CREATE TABLE conversation_entries (
    project TEXT NOT NULL,
    event_cursor INTEGER NOT NULL UNIQUE CHECK(event_cursor > 0),
    kind TEXT NOT NULL CHECK(kind IN ('message','brief')),
    brief_version INTEGER NOT NULL CHECK(brief_version >= 0),
    text TEXT NOT NULL CHECK(length(CAST(text AS BLOB)) BETWEEN 1 AND 8192),
    PRIMARY KEY(project,event_cursor),
    CHECK((kind = 'message' AND brief_version = 0) OR (kind = 'brief' AND brief_version > 0))
)";
#[cfg(unix)]
const BRIEFS: &str = "CREATE UNIQUE INDEX conversation_brief_version
    ON conversation_entries(project,brief_version) WHERE kind = 'brief'";
const RECORD_COLUMNS: &str = "c.event_cursor,c.kind,c.brief_version,
    CASE WHEN length(CAST(c.text AS BLOB))<=8192 THEN c.text END,
    CASE WHEN length(o.owner_id)=64 THEN o.owner_id END,
    CASE WHEN length(o.operation_id)<=128 THEN o.operation_id END";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", deny_unknown_fields)]
pub enum ConversationMutation {
    AppendMessage {
        project_id: ProjectId,
        text: String,
    },
    ReplaceBrief {
        project_id: ProjectId,
        expected_version: u64,
        text: String,
    },
}

impl ConversationMutation {
    pub(crate) fn validate(&self) -> Result<(), EngineError> {
        let text = match self {
            Self::AppendMessage { text, .. } => text,
            Self::ReplaceBrief {
                expected_version,
                text,
                ..
            } => {
                if *expected_version >= i64::MAX as u64 {
                    return Err(error("conversation_version_bounds"));
                }
                text
            }
        };
        validate_text(text)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", deny_unknown_fields)]
pub enum ConversationContent {
    Message,
    Brief { version: u64 },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConversationEntry {
    pub event_cursor: Cursor,
    pub content: ConversationContent,
    pub text: String,
    pub provenance: OwnerProvenance,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConversationCursor {
    pub project_id: ProjectId,
    pub db_instance: String,
    pub owner_id: String,
    pub engine_epoch: String,
    pub owner_epoch: i64,
    pub config_id: ConfigId,
    pub at_revision: Cursor,
    pub after: Cursor,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConversationPage {
    pub project_id: ProjectId,
    pub entries: Vec<ConversationEntry>,
    pub next: Option<ConversationCursor>,
}

fn error(value: impl std::fmt::Display) -> EngineError {
    EngineError(value.to_string())
}

fn validate_text(text: &str) -> Result<(), EngineError> {
    if text.trim().is_empty() || text.len() > TEXT_BYTES_MAX || text.contains('\0') {
        return Err(error("conversation_text_bounds"));
    }
    Ok(())
}

pub(crate) fn validate_history(
    project: &ProjectId,
    cursor: Option<&ConversationCursor>,
    limit: u8,
) -> Result<(), EngineError> {
    if !(1..=PAGE_ITEMS_MAX).contains(&limit) {
        return Err(error("conversation_page_limit"));
    }
    if let Some(c) = cursor {
        if c.project_id != *project {
            return Err(error("conversation_cursor_project"));
        }
        if !is_hex_id(&c.db_instance)
            || !is_hex_id(&c.owner_id)
            || !is_hex_id(&c.engine_epoch)
            || c.owner_epoch < 1
            || c.at_revision.value() > i64::MAX as u64
            || c.after.value() == 0
            || c.after.value() > c.at_revision.value()
        {
            return Err(error("conversation_cursor"));
        }
    }
    Ok(())
}

#[cfg(unix)]
pub(crate) fn initialize(tx: &Transaction<'_>) -> Result<(), EngineError> {
    let expected = [
        ("conversation_meta", META),
        ("conversation_entries", ENTRIES),
        ("conversation_brief_version", BRIEFS),
    ];
    let mut stmt = tx.prepare("SELECT CASE WHEN length(name)<=128 THEN name END,CASE WHEN length(CAST(sql AS BLOB))<=4096 THEN sql END FROM sqlite_schema WHERE sql IS NOT NULL AND (name GLOB 'conversation_*' OR tbl_name IN ('conversation_meta','conversation_entries')) ORDER BY name LIMIT 4").map_err(error)?;
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, Option<String>>(0)?,
                r.get::<_, Option<String>>(1)?,
            ))
        })
        .map_err(error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(error)?;
    if rows.is_empty() {
        for sql in [META, ENTRIES, BRIEFS] {
            tx.execute_batch(sql).map_err(error)?;
        }
        tx.execute("INSERT INTO conversation_meta VALUES (1)", [])
            .map_err(error)?;
    } else {
        if rows.len() != expected.len() {
            return Err(error("conversation_schema_unknown"));
        }
        for (name, sql) in rows {
            let name = name.ok_or_else(|| error("conversation_schema_unknown"))?;
            let sql = sql.ok_or_else(|| error("conversation_schema_unknown"))?;
            if !expected
                .iter()
                .any(|(n, definition)| *n == name && *definition == sql.trim_end_matches(';'))
            {
                return Err(error("conversation_schema_unknown"));
            }
        }
        let version: i64 = tx
            .query_row("SELECT version FROM conversation_meta", [], |r| r.get(0))
            .map_err(error)?;
        if version != 1 {
            return Err(error("conversation_schema_unknown"));
        }
    }
    assert!(!tx.is_autocommit());
    Ok(())
}

fn project_exists(tx: &Transaction<'_>, project: &ProjectId) -> Result<bool, EngineError> {
    tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM projects WHERE id=?1)",
        [project.as_str()],
        |r| r.get(0),
    )
    .map_err(error)
}

pub(crate) fn apply(
    tx: &Transaction<'_>,
    mutation: &ConversationMutation,
    revision: u64,
) -> Result<Outcome, EngineError> {
    mutation.validate()?;
    let (project, text, kind, version) = match mutation {
        ConversationMutation::AppendMessage { project_id, text } => {
            (project_id, text, "message", 0)
        }
        ConversationMutation::ReplaceBrief {
            project_id,
            expected_version,
            text,
        } => (project_id, text, "brief", *expected_version + 1),
    };
    if !project_exists(tx, project)? {
        return Ok(Outcome::Rejected {
            code: "project_missing".into(),
        });
    }
    if let ConversationMutation::ReplaceBrief {
        expected_version, ..
    } = mutation
    {
        let current: i64 = tx.query_row("SELECT COALESCE(MAX(brief_version),0) FROM conversation_entries WHERE project=?1 AND kind='brief'", [project.as_str()], |r| r.get(0)).map_err(error)?;
        if current < 0 || current as u64 != *expected_version {
            return Ok(Outcome::Rejected {
                code: "stale_brief_version".into(),
            });
        }
    }
    let count: i64 = tx
        .query_row(
            "SELECT COUNT(*) FROM (SELECT 1 FROM conversation_entries WHERE project=?1 LIMIT 1024)",
            [project.as_str()],
            |r| r.get(0),
        )
        .map_err(error)?;
    if count >= ENTRIES_MAX as i64 {
        tx.execute(
            "DELETE FROM conversation_entries WHERE project=?1 AND event_cursor=(SELECT event_cursor FROM conversation_entries WHERE project=?1 AND NOT (kind='brief' AND brief_version=(SELECT MAX(brief_version) FROM conversation_entries WHERE project=?1 AND kind='brief')) ORDER BY event_cursor LIMIT 1)",
            [project.as_str()],
        )
        .map_err(error)?;
    }
    let next = revision
        .checked_add(1)
        .filter(|n| *n <= i64::MAX as u64)
        .ok_or_else(|| error("revision_exhausted"))?;
    let changed = tx
        .execute(
            "INSERT INTO conversation_entries VALUES (?1,?2,?3,?4,?5)",
            params![project.as_str(), next as i64, kind, version as i64, text],
        )
        .map_err(error)?;
    assert_eq!(changed, 1);
    assert!(!tx.is_autocommit());
    Ok(Outcome::Applied)
}

fn decode_entry(tx: &Transaction<'_>, row: &Row<'_>) -> Result<ConversationEntry, EngineError> {
    let event: i64 = row.get(0).map_err(error)?;
    let kind: String = row.get(1).map_err(error)?;
    let version: i64 = row.get(2).map_err(error)?;
    let text: Option<String> = row.get(3).map_err(error)?;
    let owner: Option<String> = row.get(4).map_err(error)?;
    let operation: Option<String> = row.get(5).map_err(error)?;
    let text = text.ok_or_else(|| error("conversation_text_bounds"))?;
    validate_text(&text)?;
    let content = match (kind.as_str(), version) {
        ("message", 0) => ConversationContent::Message,
        ("brief", v) if v > 0 => ConversationContent::Brief { version: v as u64 },
        _ => return Err(error("conversation_record_invalid")),
    };
    if event < 1 {
        return Err(error("conversation_record_invalid"));
    }
    let owner = owner
        .filter(|s| is_hex_id(s))
        .ok_or_else(|| error("conversation_provenance_invalid"))?;
    let id = RequestId::new(operation.ok_or_else(|| error("conversation_provenance_invalid"))?)
        .map_err(error)?;
    let (provenance, result) = crate::storage::owner_lookup(tx, "native_owner", &owner, &id, None)?
        .ok_or_else(|| error("conversation_provenance_invalid"))?;
    let result: OwnerResult = serde_json::from_slice(&result).map_err(error)?;
    if result != OwnerResult::Applied
        || provenance.event_cursor.as_ref().map(Cursor::value) != Some(event as u64)
        || provenance.revision_after.value() != event as u64
    {
        return Err(error("conversation_provenance_invalid"));
    }
    Ok(ConversationEntry {
        event_cursor: Cursor::new(event as u64),
        content,
        text,
        provenance,
    })
}

fn next_cursor(
    a: &Admission,
    config: &ConfigId,
    project: &ProjectId,
    revision: u64,
    after: Cursor,
) -> ConversationCursor {
    ConversationCursor {
        project_id: project.clone(),
        db_instance: a.db_instance.clone(),
        owner_id: a.owner_id.clone(),
        engine_epoch: a.engine_epoch.clone(),
        owner_epoch: a.owner_epoch,
        config_id: config.clone(),
        at_revision: Cursor::new(revision),
        after,
    }
}

pub(crate) fn history(
    tx: &Transaction<'_>,
    config: &ConfigId,
    a: &Admission,
    project: &ProjectId,
    cursor: Option<&ConversationCursor>,
    limit: u8,
    revision: u64,
) -> Result<OwnerData, EngineError> {
    validate_history(project, cursor, limit)?;
    if !project_exists(tx, project)? {
        return Err(error("project_missing"));
    }
    if let Some(c) = cursor {
        if c.db_instance != a.db_instance
            || c.owner_id != a.owner_id
            || c.engine_epoch != a.engine_epoch
            || c.owner_epoch != a.owner_epoch
            || c.config_id != *config
            || c.at_revision.value() != revision
        {
            return Ok(OwnerData::ResyncRequired);
        }
        let exists: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM conversation_entries WHERE project=?1 AND event_cursor=?2)", params![project.as_str(),c.after.value() as i64], |r| r.get(0)).map_err(error)?;
        if !exists {
            return Err(error("conversation_cursor"));
        }
    }
    let after = cursor.map_or(0, |c| c.after.value());
    let sql = format!(
        "SELECT {RECORD_COLUMNS} FROM conversation_entries c LEFT JOIN owner_operations o ON o.event_cursor=c.event_cursor AND o.origin='native_owner' WHERE c.project=?1 AND c.event_cursor>?2 ORDER BY c.event_cursor LIMIT ?3"
    );
    let mut stmt = tx.prepare(&sql).map_err(error)?;
    let mut rows = stmt
        .query(params![
            project.as_str(),
            after as i64,
            i64::from(limit) + 1
        ])
        .map_err(error)?;
    let mut page = ConversationPage {
        project_id: project.clone(),
        entries: vec![],
        next: None,
    };
    while let Some(row) = rows.next().map_err(error)? {
        if page.entries.len() == usize::from(limit) {
            break;
        }
        let entry = decode_entry(tx, row)?;
        page.next = Some(next_cursor(
            a,
            config,
            project,
            revision,
            entry.event_cursor.clone(),
        ));
        page.entries.push(entry);
        if serde_json::to_vec(&page).map_err(error)?.len() > PAGE_BYTES_MAX {
            page.entries.pop();
            if page.entries.is_empty() {
                return Err(error("conversation_response_bounds"));
            }
            page.next = page
                .entries
                .last()
                .map(|e| next_cursor(a, config, project, revision, e.event_cursor.clone()));
            break;
        }
        page.next = None;
    }
    if let Some(last) = page.entries.last() {
        let more: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM conversation_entries WHERE project=?1 AND event_cursor>?2)", params![project.as_str(),last.event_cursor.value() as i64], |r| r.get(0)).map_err(error)?;
        if more {
            page.next = Some(next_cursor(
                a,
                config,
                project,
                revision,
                last.event_cursor.clone(),
            ));
        }
    }
    assert!(page.entries.len() <= usize::from(limit));
    assert!(
        page.entries
            .windows(2)
            .all(|w| w[0].event_cursor.value() < w[1].event_cursor.value())
    );
    assert!(serde_json::to_vec(&page).map_err(error)?.len() <= PAGE_BYTES_MAX);
    Ok(OwnerData::ConversationHistory { page })
}

/// The owner's messages one master session's turns took as input, so a client can show a
/// resumed or parked session's whole history. Inputs: the transaction, project, session and
/// at most `PAGE_ITEMS_MAX` input cursors from that session's turns. Output: the entries
/// found, oldest first, within the page byte bound; a cursor that is not one of this
/// session's inputs is skipped, and too many cursors is a plain error.
pub(crate) fn master_inputs(
    tx: &Transaction<'_>,
    project: &ProjectId,
    session: &crate::protocol::SessionId,
    cursors: &[Cursor],
) -> Result<Vec<ConversationEntry>, EngineError> {
    if cursors.len() > usize::from(PAGE_ITEMS_MAX) {
        return Err(error("master_inputs_bounds"));
    }
    let mut wanted: Vec<i64> = cursors
        .iter()
        .map(|cursor| i64::try_from(cursor.value()).map_err(error))
        .collect::<Result<_, _>>()?;
    wanted.sort_unstable();
    wanted.dedup();
    let sql = format!(
        "SELECT {RECORD_COLUMNS} FROM conversation_entries c LEFT JOIN owner_operations o ON o.event_cursor=c.event_cursor AND o.origin='native_owner'
         WHERE c.project=?1 AND c.kind='message' AND c.event_cursor=?2
         AND EXISTS (SELECT 1 FROM master_turns t WHERE t.session=?3 AND t.input=c.event_cursor)"
    );
    let mut stmt = tx.prepare(&sql).map_err(error)?;
    let mut entries = Vec::with_capacity(wanted.len());
    let mut bytes = 2usize;
    for cursor in wanted {
        let mut rows = stmt
            .query(params![project.as_str(), cursor, session.as_str()])
            .map_err(error)?;
        let Some(row) = rows.next().map_err(error)? else {
            continue;
        };
        let entry = decode_entry(tx, row)?;
        let size = serde_json::to_vec(&entry).map_err(error)?.len() + 1;
        if bytes + size > PAGE_BYTES_MAX {
            break;
        }
        bytes += size;
        entries.push(entry);
    }
    assert!(entries.len() <= usize::from(PAGE_ITEMS_MAX));
    assert!(
        entries
            .windows(2)
            .all(|w| w[0].event_cursor.value() < w[1].event_cursor.value())
    );
    assert!(serde_json::to_vec(&entries).map_err(error)?.len() <= PAGE_BYTES_MAX);
    Ok(entries)
}

pub(crate) fn current_brief(
    tx: &Transaction<'_>,
    project: &ProjectId,
) -> Result<OwnerData, EngineError> {
    if !project_exists(tx, project)? {
        return Err(error("project_missing"));
    }
    let sql = format!(
        "SELECT {RECORD_COLUMNS} FROM conversation_entries c LEFT JOIN owner_operations o ON o.event_cursor=c.event_cursor AND o.origin='native_owner' WHERE c.project=?1 AND c.kind='brief' ORDER BY c.brief_version DESC LIMIT 1"
    );
    let mut stmt = tx.prepare(&sql).map_err(error)?;
    let mut rows = stmt.query([project.as_str()]).map_err(error)?;
    let brief = rows
        .next()
        .map_err(error)?
        .map(|row| decode_entry(tx, row))
        .transpose()?;
    Ok(OwnerData::CurrentBrief {
        project_id: project.clone(),
        brief,
    })
}
