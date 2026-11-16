//! Write approvals: a tool call that may change something at a vendor waits here for the
//! owner's Allow or Deny, as one Action notice, by the write setting and the wait setting.
//! A project's standing allow, under Ask once per project and connector, is kept here too.

use crate::app_plugins::store;
use crate::connectors::settings::{Wait, WritePolicy};
use crate::credentials::SecretString;
use crate::engine::EngineError;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// The most bytes an approval's preview of a call holds.
pub const PREVIEW_BYTES_MAX: usize = 480;
/// The most writes of one launch that may wait for the owner at once.
pub const WAITING_PER_LAUNCH_MAX: usize = 4;
/// The most writes that may wait for the owner at once, across every launch.
pub const WAITING_MAX: usize = 64;
/// The refusal for an approval id that names no approval.
pub const UNKNOWN: &str = "connector_approval_unknown";
/// The refusal for an answer to an approval already answered or ended.
pub const CLOSED: &str = "connector_approval_closed";
/// The refusal for a write while the most writes already wait.
pub const WAITING_FULL: &str = "connector_writes_waiting_full";
/// The most bytes a waiting write's full arguments may take as the owner reads them.
pub const ARGUMENTS_BYTES_MAX: usize = 2 * crate::connectors::shim::ASK_BYTES_MAX as usize;

const KEPT_MAX: i64 = 1024;
const VALUE_CHARS_MAX: usize = 60;
const MINUTE_MS: i64 = 60 * 1000;
const WITHHELD: &str = "[withheld]";

const _: () = assert!(WAITING_PER_LAUNCH_MAX <= WAITING_MAX);
const _: () = assert!((WAITING_MAX as i64) < KEPT_MAX);

const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS connector_approvals (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    launch TEXT NOT NULL,
    project TEXT NOT NULL,
    agent TEXT NOT NULL,
    connector TEXT NOT NULL,
    title TEXT NOT NULL,
    tool TEXT NOT NULL,
    preview TEXT NOT NULL,
    arguments TEXT NOT NULL,
    hidden INTEGER NOT NULL CHECK (hidden IN (0, 1)),
    state TEXT NOT NULL CHECK (state IN ('waiting', 'allowed', 'denied', 'expired')),
    created_ms INTEGER NOT NULL,
    expires_ms INTEGER NOT NULL,
    answered_ms INTEGER
);
CREATE INDEX IF NOT EXISTS connector_approvals_state ON connector_approvals (state, launch);
CREATE TABLE IF NOT EXISTS connector_write_allowed (
    project TEXT NOT NULL,
    connector TEXT NOT NULL,
    allowed_ms INTEGER NOT NULL,
    PRIMARY KEY (project, connector)
) WITHOUT ROWID;";

/// Where an approval stands: waiting for the owner, or answered by an Allow, a Deny or the
/// end of its wait.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Waiting,
    Allowed,
    Denied,
    Expired,
}

impl State {
    const ALL: [Self; 4] = [Self::Waiting, Self::Allowed, Self::Denied, Self::Expired];

    fn name(self) -> &'static str {
        match self {
            Self::Waiting => "waiting",
            Self::Allowed => "allowed",
            Self::Denied => "denied",
            Self::Expired => "expired",
        }
    }

    fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|state| state.name() == name)
    }
}

/// The owner's answer to one waiting write.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Answer {
    Allow,
    Deny,
}

/// One write call that asks the owner: the launch, project and agent it came from, the
/// connector by name and title, the vendor tool, the call's preview and full arguments, and
/// whether those arguments hold hidden characters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingWrite {
    pub launch: String,
    pub project: String,
    pub agent: String,
    pub connector: String,
    pub title: String,
    pub tool: String,
    pub preview: String,
    pub arguments: String,
    pub hidden: bool,
}

/// One approval as the owner's approval page shows it: what asks, where, the preview, the
/// full arguments with hidden characters escaped and `hidden` marking that it holds some,
/// where it stands, and its times in milliseconds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovalView {
    pub id: i64,
    pub project: String,
    pub project_name: String,
    pub agent: String,
    pub connector: String,
    pub title: String,
    pub tool: String,
    pub preview: String,
    pub arguments: String,
    pub hidden: bool,
    pub state: State,
    pub created_ms: i64,
    pub expires_ms: i64,
    pub answered_ms: Option<i64>,
}

/// The connectors a project's owner allowed to write without asking again, by name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AllowedWrites {
    pub project: String,
    pub connectors: Vec<String>,
}

/// One waiting approval as its notice names it: its id, the project's id and shown name,
/// the connector's title and the vendor tool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Waiting {
    pub id: i64,
    pub project: String,
    pub project_name: String,
    pub title: String,
    pub tool: String,
}

fn refusal(code: &str) -> EngineError {
    EngineError(code.into())
}

fn sqlite(_: rusqlite::Error) -> EngineError {
    refusal("connectors_unavailable")
}

fn asked_before(db: &Connection) -> Result<bool, EngineError> {
    db.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master
         WHERE type='table' AND name='connector_approvals')",
        [],
        |row| row.get(0),
    )
    .map_err(sqlite)
}

/// The name a project shows: its own name, or its id when it has none.
fn shown_name(name: Option<String>, project: &str) -> String {
    name.filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| project.to_owned())
}

fn leaks(text: &str, secrets: &[SecretString]) -> bool {
    secrets
        .iter()
        .any(|secret| !secret.expose().is_empty() && text.contains(secret.expose()))
}

/// One text as a preview shows it: without hidden or control characters, or `[withheld]`
/// when it holds one of the connector's secret forms before or after that cleaning.
fn plain(text: &str, secrets: &[SecretString]) -> String {
    let clean: String = text
        .chars()
        .filter(|found| !found.is_control() && !crate::memory::hidden_char(*found))
        .collect();
    if leaks(text, secrets) || leaks(&clean, secrets) {
        return WITHHELD.to_owned();
    }
    clean
}

/// One name or text as the full arguments show it: each hidden or control character other
/// than a line break or tab written as a visible `\u{XXXX}` escape, never left out, or
/// `[withheld]` when it holds a secret form as sent, as escaped or as [`plain`] cleans it.
fn whole(text: &str, secrets: &[SecretString]) -> String {
    let mut shown = String::with_capacity(text.len());
    for found in text.chars() {
        if crate::memory::hidden_char(found) {
            shown.push_str(&format!("\\u{{{:04X}}}", u32::from(found)));
        } else {
            shown.push(found);
        }
    }
    if leaks(&shown, secrets) || plain(text, secrets) == WITHHELD {
        return WITHHELD.to_owned();
    }
    assert!(!holds(&shown), "an escaped text holds no hidden character");
    shown
}

/// Whether one name or text holds a character that [`whole`] escapes.
fn holds(text: &str) -> bool {
    text.chars().any(crate::memory::hidden_char)
}

/// Whether any name or text in one argument value holds a character that [`whole`] escapes.
fn hides(value: &Value) -> bool {
    match value {
        Value::String(text) => holds(text),
        Value::Array(items) => items.iter().any(hides),
        Value::Object(fields) => fields.iter().any(|(key, item)| holds(key) || hides(item)),
        Value::Null | Value::Bool(_) | Value::Number(_) => false,
    }
}

/// One argument value whole, every name and text as [`whole`] shows it.
fn scrubbed(value: &Value, secrets: &[SecretString]) -> Value {
    match value {
        Value::String(text) => Value::String(whole(text, secrets)),
        Value::Array(items) => {
            Value::Array(items.iter().map(|item| scrubbed(item, secrets)).collect())
        }
        Value::Object(fields) => Value::Object(
            fields
                .iter()
                .map(|(key, item)| (whole(key, secrets), scrubbed(item, secrets)))
                .collect(),
        ),
        Value::Null | Value::Bool(_) | Value::Number(_) => value.clone(),
    }
}

/// One argument value as a preview shows it: a text quoted and cut to `VALUE_CHARS_MAX`
/// characters, a list or an object by its size, anything else as written.
fn short(value: &Value, secrets: &[SecretString]) -> String {
    match value {
        Value::String(text) => {
            let text = plain(text, secrets);
            let mut kept: String = text.chars().take(VALUE_CHARS_MAX).collect();
            if kept.len() < text.len() {
                kept.push('…');
            }
            format!("\"{kept}\"")
        }
        Value::Array(items) => format!("[{} items]", items.len()),
        Value::Object(fields) => format!("{{{} fields}}", fields.len()),
        Value::Null | Value::Bool(_) | Value::Number(_) => value.to_string(),
    }
}

fn cut(mut text: String, max: usize) -> String {
    if text.len() > max {
        let mut end = max;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
    }
    assert!(text.len() <= max);
    text
}

/// Makes the approval tables. Input: the engine database. Output: Ok, or
/// `connectors_unavailable`.
pub fn initialize(db: &Connection) -> Result<(), EngineError> {
    db.execute_batch(SCHEMA).map_err(sqlite)
}

/// The preview of one write call. Inputs: the vendor tool, the call's arguments and the
/// connector's secret forms. Output: the preview, at most `PREVIEW_BYTES_MAX` bytes.
pub fn preview(tool: &str, arguments: &Map<String, Value>, secrets: &[SecretString]) -> String {
    let parts: Vec<String> = arguments
        .iter()
        .map(|(key, value)| format!("{}={}", plain(key, secrets), short(value, secrets)))
        .collect();
    let named = plain(tool, secrets);
    let shown = if parts.is_empty() {
        named
    } else {
        format!("{named}: {}", parts.join(", "))
    };
    cut(shown, PREVIEW_BYTES_MAX)
}

/// Every argument of one write call as JSON text, each name and text value shown whole with
/// its hidden characters escaped, so the owner reads every character the vendor would get.
/// Inputs: the arguments and the connector's secret forms. Output: the text, or none past
/// `ARGUMENTS_BYTES_MAX`.
pub fn arguments(arguments: &Map<String, Value>, secrets: &[SecretString]) -> Option<String> {
    let shown = scrubbed(&Value::Object(arguments.clone()), secrets);
    let text = serde_json::to_string(&shown).ok()?;
    if text.len() > ARGUMENTS_BYTES_MAX || leaks(&text, secrets) {
        return None;
    }
    Some(text)
}

/// Whether any name or text of one write call's arguments holds a hidden or control
/// character, which the full arguments show escaped. Input: the arguments. Output: whether
/// the owner's view is marked.
pub fn hidden(arguments: &Map<String, Value>) -> bool {
    arguments
        .iter()
        .any(|(key, value)| holds(key) || hides(value))
}

/// Records one write that waits for the owner, unless `WAITING_PER_LAUNCH_MAX` of its launch
/// or `WAITING_MAX` in all already wait, and keeps at most `KEPT_MAX` answered approvals.
/// Inputs: the database, the write, the wait and the time now. Output: the approval's id,
/// `connector_writes_waiting_full`, or `connectors_unavailable`.
pub fn ask(
    db: &Connection,
    write: &PendingWrite,
    wait: Wait,
    now_ms: i64,
) -> Result<i64, EngineError> {
    assert!(write.preview.len() <= PREVIEW_BYTES_MAX);
    assert!(write.arguments.len() <= ARGUMENTS_BYTES_MAX);
    initialize(db)?;
    let wait_ms = i64::from(wait.minutes()) * MINUTE_MS;
    let expires_ms = now_ms.saturating_add(wait_ms);
    let added = db
        .execute(
            "INSERT INTO connector_approvals
                 (launch, project, agent, connector, title, tool, preview, arguments, hidden,
                  state, created_ms, expires_ms)
             SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?12, ?13, 'waiting', ?8, ?9
             WHERE (SELECT COUNT(*) FROM connector_approvals
                    WHERE state='waiting' AND launch=?1) < ?10
               AND (SELECT COUNT(*) FROM connector_approvals WHERE state='waiting') < ?11",
            params![
                write.launch,
                write.project,
                write.agent,
                write.connector,
                write.title,
                write.tool,
                write.preview,
                now_ms,
                expires_ms,
                WAITING_PER_LAUNCH_MAX as i64,
                WAITING_MAX as i64,
                write.arguments,
                write.hidden,
            ],
        )
        .map_err(sqlite)?;
    if added == 0 {
        return Err(refusal(WAITING_FULL));
    }
    assert_eq!(added, 1);
    let id = db.last_insert_rowid();
    db.execute(
        "DELETE FROM connector_approvals WHERE state<>'waiting' AND id NOT IN
             (SELECT id FROM connector_approvals WHERE state<>'waiting'
              ORDER BY id DESC LIMIT ?1)",
        [KEPT_MAX],
    )
    .map_err(sqlite)?;
    Ok(id)
}

/// Answers one waiting write once; an Allow under Ask once per project and connector also
/// records that project's standing allow. A wait that ran out by `now_ms` ends as expired
/// first. Inputs: the database, the id, the answer, the write setting and the time now.
/// Output: Ok, `connector_approval_unknown`, `connector_approval_closed`, or
/// `connectors_unavailable`.
pub fn answer(
    db: &Connection,
    id: i64,
    answer: Answer,
    write: WritePolicy,
    now_ms: i64,
) -> Result<(), EngineError> {
    if !asked_before(db)? {
        return Err(refusal(UNKNOWN));
    }
    let tx = db.unchecked_transaction().map_err(sqlite)?;
    let row: Option<(String, String, String, i64)> = tx
        .query_row(
            "SELECT state, project, connector, expires_ms FROM connector_approvals WHERE id=?1",
            [id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()
        .map_err(sqlite)?;
    let Some((state, project, connector, expires_ms)) = row else {
        return Err(refusal(UNKNOWN));
    };
    if State::parse(&state) != Some(State::Waiting) {
        return Err(refusal(CLOSED));
    }
    if now_ms >= expires_ms {
        tx.execute(
            "UPDATE connector_approvals SET state='expired', arguments='', answered_ms=expires_ms
             WHERE id=?1 AND state='waiting'",
            [id],
        )
        .map_err(sqlite)?;
        tx.commit().map_err(sqlite)?;
        return Err(refusal(CLOSED));
    }
    let answered = match answer {
        Answer::Allow => State::Allowed,
        Answer::Deny => State::Denied,
    };
    let changed = tx
        .execute(
            "UPDATE connector_approvals SET state=?2, arguments='', answered_ms=?3
             WHERE id=?1 AND state='waiting'",
            params![id, answered.name(), now_ms],
        )
        .map_err(sqlite)?;
    if changed != 1 {
        return Err(refusal(CLOSED));
    }
    if answer == Answer::Allow && write == WritePolicy::AskOncePerProject {
        tx.execute(
            "INSERT OR REPLACE INTO connector_write_allowed (project, connector, allowed_ms)
             VALUES (?1, ?2, ?3)",
            params![project, connector, now_ms],
        )
        .map_err(sqlite)?;
    }
    tx.commit().map_err(sqlite)
}

/// Where one approval stands. Inputs: the database and the id. Output: its state, or none.
pub fn state(db: &Connection, id: i64) -> Result<Option<State>, EngineError> {
    if !asked_before(db)? {
        return Ok(None);
    }
    let name: Option<String> = db
        .query_row(
            "SELECT state FROM connector_approvals WHERE id=?1",
            [id],
            |row| row.get(0),
        )
        .optional()
        .map_err(sqlite)?;
    Ok(name.as_deref().and_then(State::parse))
}

/// One approval as the owner's page shows it. Inputs: the database and the id. Output:
/// the approval, or none.
pub fn get(db: &Connection, id: i64) -> Result<Option<ApprovalView>, EngineError> {
    if !asked_before(db)? {
        return Ok(None);
    }
    db.query_row(
        "SELECT a.id, a.project, json_extract(p.body, '$.spec.name'), a.agent, a.connector,
                a.title, a.tool, a.preview, a.state, a.created_ms, a.expires_ms, a.answered_ms,
                a.arguments, a.hidden
         FROM connector_approvals a LEFT JOIN projects p ON p.id = a.project
         WHERE a.id=?1",
        [id],
        |row| {
            let project: String = row.get(1)?;
            let state: String = row.get(8)?;
            Ok(ApprovalView {
                id: row.get(0)?,
                project_name: shown_name(row.get(2)?, &project),
                project,
                agent: row.get(3)?,
                connector: row.get(4)?,
                title: row.get(5)?,
                tool: row.get(6)?,
                preview: row.get(7)?,
                arguments: row.get(12)?,
                hidden: row.get(13)?,
                state: State::parse(&state).ok_or(rusqlite::Error::InvalidQuery)?,
                created_ms: row.get(9)?,
                expires_ms: row.get(10)?,
                answered_ms: row.get(11)?,
            })
        },
    )
    .optional()
    .map_err(sqlite)
}

/// Ends the waits that ran out, each at its own end. Inputs: the database and the time now.
/// Output: how many.
pub fn expire(db: &Connection, now_ms: i64) -> Result<usize, EngineError> {
    if !asked_before(db)? {
        return Ok(0);
    }
    db.execute(
        "UPDATE connector_approvals SET state='expired', arguments='', answered_ms=expires_ms
         WHERE state='waiting' AND expires_ms<=?1",
        [now_ms],
    )
    .map_err(sqlite)
}

/// Ends the given waits. Inputs: the database, the ids and the time now. Output: how many.
pub fn expire_ids(db: &Connection, ids: &[i64], now_ms: i64) -> Result<usize, EngineError> {
    if ids.is_empty() || !asked_before(db)? {
        return Ok(0);
    }
    let mut statement = db
        .prepare(
            "UPDATE connector_approvals SET state='expired', arguments='', answered_ms=?2
             WHERE id=?1 AND state='waiting'",
        )
        .map_err(sqlite)?;
    let mut ended = 0;
    for id in ids {
        ended += statement.execute(params![id, now_ms]).map_err(sqlite)?;
    }
    assert!(ended <= ids.len());
    Ok(ended)
}

/// Ends every wait. Inputs: the database and the time now. Output: how many.
pub fn expire_waiting(db: &Connection, now_ms: i64) -> Result<usize, EngineError> {
    if !asked_before(db)? {
        return Ok(0);
    }
    db.execute(
        "UPDATE connector_approvals SET state='expired', arguments='', answered_ms=?1
         WHERE state='waiting'",
        [now_ms],
    )
    .map_err(sqlite)
}

/// Whether a project's owner allowed a connector to write without asking again. Inputs:
/// the database, the project and the connector. Output: the answer.
pub fn writes_allowed(
    db: &Connection,
    project: &str,
    connector: &str,
) -> Result<bool, EngineError> {
    if !asked_before(db)? {
        return Ok(false);
    }
    db.query_row(
        "SELECT EXISTS(SELECT 1 FROM connector_write_allowed WHERE project=?1 AND connector=?2)",
        [project, connector],
        |row| row.get(0),
    )
    .map_err(sqlite)
}

/// The connectors a project may write with without asking again. Inputs: the database and
/// the project. Output: them, by name.
pub fn allowed_writes(db: &Connection, project: &str) -> Result<AllowedWrites, EngineError> {
    let mut connectors = Vec::new();
    if asked_before(db)? {
        connectors = db
            .prepare(
                "SELECT connector FROM connector_write_allowed WHERE project=?1
                 ORDER BY connector LIMIT ?2",
            )
            .map_err(sqlite)?
            .query_map(params![project, store::PLUGINS_MAX as i64], |row| row.get(0))
            .map_err(sqlite)?
            .collect::<Result<_, _>>()
            .map_err(sqlite)?;
    }
    assert!(connectors.len() <= store::PLUGINS_MAX);
    Ok(AllowedWrites {
        project: project.to_owned(),
        connectors,
    })
}

/// Takes back a project's standing allow for one connector. Inputs: the database, the
/// project and the connector. Output: whether there was one.
pub fn revoke_writes(
    db: &Connection,
    project: &str,
    connector: &str,
) -> Result<bool, EngineError> {
    if !asked_before(db)? {
        return Ok(false);
    }
    let removed = db
        .execute(
            "DELETE FROM connector_write_allowed WHERE project=?1 AND connector=?2",
            [project, connector],
        )
        .map_err(sqlite)?;
    assert!(removed <= 1);
    Ok(removed == 1)
}

/// The waiting approvals for their notices. Inputs: the database and the most to read.
/// Output: them in id order, or none before any approval was asked.
pub fn waiting(db: &Connection, max: usize) -> Result<Option<Vec<Waiting>>, EngineError> {
    if !asked_before(db)? {
        return Ok(None);
    }
    let rows: Vec<Waiting> = db
        .prepare(
            "SELECT a.id, a.project, json_extract(p.body, '$.spec.name'), a.title, a.tool
             FROM connector_approvals a LEFT JOIN projects p ON p.id = a.project
             WHERE a.state='waiting' ORDER BY a.id LIMIT ?1",
        )
        .map_err(sqlite)?
        .query_map([i64::try_from(max).unwrap_or(i64::MAX)], |row| {
            let project: String = row.get(1)?;
            Ok(Waiting {
                id: row.get(0)?,
                project_name: shown_name(row.get(2)?, &project),
                project,
                title: row.get(3)?,
                tool: row.get(4)?,
            })
        })
        .map_err(sqlite)?
        .collect::<Result<_, _>>()
        .map_err(sqlite)?;
    assert!(rows.len() <= max);
    Ok(Some(rows))
}

#[cfg(all(test, unix))]
#[path = "approvals_tests.rs"]
mod tests;
