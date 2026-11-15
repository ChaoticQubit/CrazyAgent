//! The connector activity log: one row per tool list and tool call an agent made through
//! the gateway, with who, what, how long, how large, whether it reached the vendor and how
//! it ended. It never holds a call's arguments or a result.

use crate::connectors::settings::{ConnectorSettings, KeepLog};
use crate::engine::EngineError;
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};

/// The most rows the log keeps of calls that reached a vendor, whatever the keep setting
/// says. Rows are counted, never id distance, so ids that deleted rows used up never count.
pub const ROWS_MAX: i64 = 100_000;
/// The most rows one read returns.
pub const READ_MAX: usize = 500;
/// The most rows one launch keeps of lists and calls that sent nothing of an agent's to a
/// vendor; its oldest go first, so a flood of them never pushes a sent call out.
pub const QUIET_ROWS_PER_LAUNCH_MAX: usize = 1_000;
/// The most rows the log keeps of lists and calls that sent nothing to a vendor, across
/// every launch; they are capped apart from the calls that reached a vendor, so no number
/// of them pushes one of those out.
pub const QUIET_ROWS_MAX: usize = 10_000;

/// The most calls one activity view holds.
pub const VIEW_CALLS_MAX: usize = 100;
/// The most connectors, and the most projects, one activity view offers as filters.
pub const VIEW_CHOICES_MAX: usize = 32;
/// The longest project id a filter takes and a view offers, the protocol's identifier bound.
pub const PROJECT_ID_BYTES_MAX: usize = 128;
const VIEW_CALL_BYTES_MAX: usize = 40 * 1024;
const VIEW_CHOICE_BYTES_MAX: usize = 12 * 1024;

const _: () = assert!(QUIET_ROWS_PER_LAUNCH_MAX < ROWS_MAX as usize / 10);
const _: () = assert!(QUIET_ROWS_PER_LAUNCH_MAX < QUIET_ROWS_MAX);
const _: () = assert!(QUIET_ROWS_MAX <= ROWS_MAX as usize / 10);
const _: () = assert!(VIEW_CALLS_MAX <= READ_MAX);
const _: () = assert!(
    VIEW_CALL_BYTES_MAX + VIEW_CHOICE_BYTES_MAX + 8 * 1024 <= crate::protocol::FRAME_BYTES_MAX
);

const DAY_MS: i64 = 24 * 60 * 60 * 1000;

const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS connector_calls (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    launch TEXT NOT NULL CHECK(length(launch) BETWEEN 1 AND 64),
    connector TEXT NOT NULL CHECK(length(connector) <= 40),
    tool TEXT NOT NULL CHECK(length(tool) <= 128),
    kind TEXT NOT NULL CHECK(kind IN ('read','write','list','unknown')),
    sent INTEGER NOT NULL CHECK(sent IN (0,1)),
    project TEXT NOT NULL CHECK(length(project) <= 256),
    agent TEXT NOT NULL CHECK(length(agent) BETWEEN 1 AND 128),
    started_ms INTEGER NOT NULL,
    ms INTEGER NOT NULL CHECK(ms >= 0),
    bytes_in INTEGER NOT NULL CHECK(bytes_in >= 0),
    bytes_out INTEGER NOT NULL CHECK(bytes_out >= 0),
    outcome TEXT NOT NULL CHECK(length(outcome) BETWEEN 1 AND 64)
);
CREATE INDEX IF NOT EXISTS connector_calls_started ON connector_calls(started_ms);
CREATE INDEX IF NOT EXISTS connector_calls_quiet ON connector_calls(launch, sent, id);
CREATE INDEX IF NOT EXISTS connector_calls_sent ON connector_calls(sent, id);";

/// What a row is about: a tool the gateway counts as read or write, a tool list, or a
/// call that named no tool the gateway knows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CallKind {
    Read,
    Write,
    List,
    Unknown,
}

impl CallKind {
    const ALL: [Self; 4] = [Self::Read, Self::Write, Self::List, Self::Unknown];

    /// The kind's stored name. Input: the kind. Output: the fixed name.
    pub fn name(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Write => "write",
            Self::List => "list",
            Self::Unknown => "unknown",
        }
    }
}

/// One row of the log, as it is written and read: the launch it came from, sizes in bytes,
/// times in milliseconds, whether the call's arguments went to the vendor, and the
/// outcome as `ok` or a refusal code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallRow {
    pub launch: String,
    pub connector: String,
    pub tool: String,
    pub kind: CallKind,
    pub sent: bool,
    pub project: String,
    pub agent: String,
    pub started_ms: i64,
    pub ms: i64,
    pub bytes_in: i64,
    pub bytes_out: i64,
    pub outcome: String,
}

/// One logged call as both apps show it: the text cleaned of hidden and control
/// characters, the project's name beside its id, and the outcome as its code and the
/// engine's plain sentence. It never holds a call's arguments or a result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CallView {
    pub connector: String,
    pub tool: String,
    pub kind: CallKind,
    pub project: String,
    pub project_name: String,
    pub agent: String,
    pub started_ms: i64,
    pub ms: i64,
    pub bytes_in: i64,
    pub bytes_out: i64,
    pub outcome: String,
    pub outcome_text: String,
}

/// One project a view offers as a filter: its id and the name shown for it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectChoice {
    pub id: String,
    pub name: String,
}

/// The owner's activity page: the newest calls inside the keep time that match the
/// filters, whether more matched than it holds, the filters it answers, the connectors and
/// projects the log holds to filter by, and the connector settings with their version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActivityView {
    pub calls: Vec<CallView>,
    pub more: bool,
    pub connector: Option<String>,
    pub project: Option<String>,
    pub connectors: Vec<String>,
    pub projects: Vec<ProjectChoice>,
    pub settings: ConnectorSettings,
    pub version: u64,
}

fn sqlite(_: rusqlite::Error) -> EngineError {
    EngineError("connectors_unavailable".into())
}

/// Writes one row and drops what the keep setting, the row cap and, for a row that sent
/// nothing, its launch's quiet cap no longer hold. Inputs: the engine database, the row
/// and the keep setting. Output: Ok, or `connectors_unavailable` when the row does not fit
/// the log or the database fails.
pub fn record(db: &Connection, row: &CallRow, keep: KeepLog) -> Result<(), EngineError> {
    db.execute_batch(SCHEMA).map_err(sqlite)?;
    let added = db
        .execute(
            "INSERT INTO connector_calls
                 (launch, connector, tool, kind, sent, project, agent, started_ms, ms,
                  bytes_in, bytes_out, outcome)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                row.launch,
                row.connector,
                row.tool,
                row.kind.name(),
                row.sent,
                row.project,
                row.agent,
                row.started_ms,
                row.ms,
                row.bytes_in,
                row.bytes_out,
                row.outcome
            ],
        )
        .map_err(sqlite)?;
    if added != 1 {
        return Err(sqlite(rusqlite::Error::QueryReturnedNoRows));
    }
    if !row.sent {
        db.execute(
            "DELETE FROM connector_calls WHERE launch=?1 AND sent=0 AND id <= (
                 SELECT id FROM connector_calls WHERE launch=?1 AND sent=0
                 ORDER BY id DESC LIMIT 1 OFFSET ?2)",
            params![row.launch, QUIET_ROWS_PER_LAUNCH_MAX as i64],
        )
        .map_err(sqlite)?;
    }
    prune(db, keep)
}

fn cutoff(keep: KeepLog) -> i64 {
    crate::semantic::now_ms() - i64::from(keep.days()) * DAY_MS
}

/// Reads the newest rows. Inputs: the engine database and how many, at most `READ_MAX`.
/// Output: the rows, newest first, or `connectors_unavailable`.
pub fn recent(db: &Connection, limit: usize) -> Result<Vec<CallRow>, EngineError> {
    db.execute_batch(SCHEMA).map_err(sqlite)?;
    let limit = limit.min(READ_MAX);
    let mut statement = db
        .prepare(
            "SELECT connector, tool, kind, project, agent, started_ms, ms, bytes_in, bytes_out,
                    outcome, launch, sent
             FROM connector_calls ORDER BY id DESC LIMIT ?1",
        )
        .map_err(sqlite)?;
    let rows = statement
        .query_map([limit as i64], |row| {
            let kind: String = row.get(2)?;
            Ok((
                kind,
                CallRow {
                    launch: row.get(10)?,
                    connector: row.get(0)?,
                    tool: row.get(1)?,
                    kind: CallKind::Unknown,
                    sent: row.get(11)?,
                    project: row.get(3)?,
                    agent: row.get(4)?,
                    started_ms: row.get(5)?,
                    ms: row.get(6)?,
                    bytes_in: row.get(7)?,
                    bytes_out: row.get(8)?,
                    outcome: row.get(9)?,
                },
            ))
        })
        .map_err(sqlite)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(sqlite)?;
    let mut found = Vec::with_capacity(rows.len());
    for (kind, mut row) in rows {
        row.kind = CallKind::ALL
            .into_iter()
            .find(|known| known.name() == kind)
            .ok_or_else(|| EngineError("connectors_unavailable".into()))?;
        found.push(row);
    }
    assert!(found.len() <= READ_MAX);
    Ok(found)
}

/// Reads the activity page. Inputs: the engine database and the connector and project to
/// filter by, none for all. Output: the view, or `connectors_unavailable`.
pub fn view(
    db: &Connection,
    connector: Option<&str>,
    project: Option<&str>,
) -> Result<ActivityView, EngineError> {
    db.execute_batch(SCHEMA).map_err(sqlite)?;
    let (settings, version) = crate::connectors::settings::load(db)?;
    let after = cutoff(settings.keep_log);
    let mut statement = db
        .prepare(
            "SELECT c.connector, c.tool, c.kind, c.project, c.agent, c.started_ms, c.ms,
                    c.bytes_in, c.bytes_out, c.outcome, json_extract(p.body, '$.spec.name')
             FROM connector_calls c LEFT JOIN projects p ON p.id = c.project
             WHERE c.started_ms >= ?1
               AND (?2 IS NULL OR c.connector = ?2)
               AND (?3 IS NULL OR c.project = ?3)
             ORDER BY c.started_ms DESC, c.id DESC LIMIT ?4",
        )
        .map_err(sqlite)?;
    let found = statement
        .query_map(
            params![after, connector, project, VIEW_CALLS_MAX as i64 + 1],
            |row| {
                let kind: String = row.get(2)?;
                let project: String = row.get(3)?;
                let name: Option<String> = row.get(10).ok().flatten();
                let outcome: String = row.get(9)?;
                Ok((
                    kind,
                    CallView {
                        connector: shown(&row.get::<_, String>(0)?),
                        tool: shown(&row.get::<_, String>(1)?),
                        kind: CallKind::Unknown,
                        project_name: project_name(&project, name.as_deref()),
                        project,
                        agent: shown(&row.get::<_, String>(4)?),
                        started_ms: row.get(5)?,
                        ms: row.get(6)?,
                        bytes_in: row.get(7)?,
                        bytes_out: row.get(8)?,
                        outcome_text: outcome_text(&outcome),
                        outcome: shown(&outcome),
                    },
                ))
            },
        )
        .map_err(sqlite)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(sqlite)?;
    let mut more = found.len() > VIEW_CALLS_MAX;
    let mut calls = Vec::with_capacity(found.len().min(VIEW_CALLS_MAX));
    let mut bytes = 0;
    for (kind, mut call) in found.into_iter().take(VIEW_CALLS_MAX) {
        call.kind = CallKind::ALL
            .into_iter()
            .find(|known| known.name() == kind)
            .ok_or_else(|| EngineError("connectors_unavailable".into()))?;
        let json =
            serde_json::to_vec(&call).map_err(|_| EngineError("connectors_unavailable".into()))?;
        bytes += json.len() + 1;
        if bytes > VIEW_CALL_BYTES_MAX {
            more = true;
            break;
        }
        calls.push(call);
    }
    let (connectors, projects) = choices(db, after)?;
    assert!(calls.len() <= VIEW_CALLS_MAX);
    assert!(connectors.len() <= VIEW_CHOICES_MAX && projects.len() <= VIEW_CHOICES_MAX);
    Ok(ActivityView {
        calls,
        more,
        connector: connector.map(str::to_owned),
        project: project.map(str::to_owned),
        connectors,
        projects,
        settings,
        version,
    })
}

/// The connectors and projects the log holds inside the keep time, the most recently used
/// first up to `VIEW_CHOICES_MAX` each and inside one shared byte budget, then sorted by
/// what is shown. Inputs: the database and the oldest start kept. Output: both lists.
fn choices(db: &Connection, after: i64) -> Result<(Vec<String>, Vec<ProjectChoice>), EngineError> {
    let mut connectors: Vec<String> = db
        .prepare(
            "SELECT connector FROM connector_calls WHERE started_ms >= ?1
             GROUP BY connector ORDER BY MAX(id) DESC LIMIT ?2",
        )
        .map_err(sqlite)?
        .query_map(params![after, VIEW_CHOICES_MAX as i64], |row| row.get(0))
        .map_err(sqlite)?
        .collect::<Result<_, _>>()
        .map_err(sqlite)?;
    let mut projects: Vec<ProjectChoice> = db
        .prepare(
            "SELECT c.project, json_extract(p.body, '$.spec.name')
             FROM connector_calls c LEFT JOIN projects p ON p.id = c.project
             WHERE c.started_ms >= ?1 AND length(CAST(c.project AS BLOB)) BETWEEN 1 AND ?3
             GROUP BY c.project ORDER BY MAX(c.id) DESC LIMIT ?2",
        )
        .map_err(sqlite)?
        .query_map(
            params![after, VIEW_CHOICES_MAX as i64, PROJECT_ID_BYTES_MAX as i64],
            |row| {
                let id: String = row.get(0)?;
                let name: Option<String> = row.get(1).ok().flatten();
                Ok(ProjectChoice {
                    name: project_name(&id, name.as_deref()),
                    id,
                })
            },
        )
        .map_err(sqlite)?
        .collect::<Result<_, _>>()
        .map_err(sqlite)?;
    let mut bytes: usize = 0;
    connectors.retain(|name| {
        bytes = bytes.saturating_add(name.len() * 2 + 4);
        bytes <= VIEW_CHOICE_BYTES_MAX
    });
    projects.retain(|choice| {
        let size = serde_json::to_vec(choice).map_or(usize::MAX, |json| json.len() + 1);
        bytes = bytes.saturating_add(size);
        bytes <= VIEW_CHOICE_BYTES_MAX
    });
    connectors.sort_unstable();
    projects.sort_by_cached_key(|choice| (choice.name.to_lowercase(), choice.id.clone()));
    assert!(
        projects
            .iter()
            .all(|choice| choice.id.len() <= PROJECT_ID_BYTES_MAX)
    );
    Ok((connectors, projects))
}

/// Text the log holds as both apps may show it: hidden and control characters taken out.
/// Input: the stored text. Output: the shown text.
fn shown(text: &str) -> String {
    crate::memory::without_hidden(text)
        .chars()
        .filter(|character| !character.is_control())
        .collect()
}

/// A project's shown name: its name in the projects table, else its id for a project that
/// is gone or unnamed. Inputs: the id and the stored name. Output: the name.
fn project_name(id: &str, name: Option<&str>) -> String {
    let named = name.map(shown).unwrap_or_default();
    if named.trim().is_empty() {
        shown(id)
    } else {
        named
    }
}

/// The words for an outcome: OK, the engine's plain sentence for a known code with its
/// first letter raised, or the code itself. Input: the stored outcome. Output: the words.
fn outcome_text(outcome: &str) -> String {
    if outcome == "ok" {
        return "OK".into();
    }
    match crate::connectors::plain_problem(outcome) {
        Some(sentence) => {
            let mut letters = sentence.chars();
            letters.next().map_or_else(String::new, |first| {
                first.to_uppercase().chain(letters).collect()
            })
        }
        None => shown(outcome),
    }
}

/// Drops the rows the keep setting no longer holds, then the oldest past each cap: rows of
/// calls that reached a vendor past `ROWS_MAX`, and the rest past `QUIET_ROWS_MAX`, each
/// counted on its own so the second kind never pushes out the first. Inputs: the engine
/// database and the keep setting. Output: Ok, or `connectors_unavailable`.
pub fn prune(db: &Connection, keep: KeepLog) -> Result<(), EngineError> {
    db.execute_batch(SCHEMA).map_err(sqlite)?;
    db.execute(
        "DELETE FROM connector_calls WHERE started_ms < ?1",
        [cutoff(keep)],
    )
    .map_err(sqlite)?;
    for (sent, cap) in [(true, ROWS_MAX), (false, QUIET_ROWS_MAX as i64)] {
        db.execute(
            "DELETE FROM connector_calls WHERE sent=?1 AND id <= (
                 SELECT id FROM connector_calls WHERE sent=?1
                 ORDER BY id DESC LIMIT 1 OFFSET ?2)",
            params![sent, cap],
        )
        .map_err(sqlite)?;
    }
    Ok(())
}

#[cfg(all(test, unix))]
#[path = "audit_tests.rs"]
mod tests;
