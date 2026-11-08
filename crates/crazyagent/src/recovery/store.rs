use super::{
    ASKED_BYTES_MAX, HeldItem, Interrupted, LastTurn, Listed, REASON_BYTES_MAX,
    RUNS_PER_PROJECT_MAX, RunState, unavailable,
};
use crate::engine::EngineError;
use crate::harnesses::interactive::Transcript;
use crate::harnesses::terminal_session::{PaneRecord, RecordSource};
use crate::harnesses::transcript::TranscriptReader;
use crate::protocol::ProjectId;
use crate::team::{AgentRole, bound_text, now_ms};
use rusqlite::{Connection, OptionalExtension, Row, Transaction, TransactionBehavior, params};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

const CAPTURED_MAX: i64 = 4096;

const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS recovery_runs (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    agent INTEGER NOT NULL,
    project TEXT NOT NULL,
    kind TEXT NOT NULL CHECK(kind IN ('master','sub')),
    harness TEXT NOT NULL,
    at_ms INTEGER NOT NULL,
    turn INTEGER NOT NULL CHECK(turn >= 1),
    task INTEGER,
    message INTEGER,
    asked TEXT,
    record TEXT,
    cwd TEXT,
    record_file TEXT,
    paused INTEGER NOT NULL DEFAULT 0 CHECK(paused IN (0,1)),
    state TEXT NOT NULL CHECK(state IN ('stopped','interrupted','resuming','resumed','failed')),
    recorded_ms INTEGER,
    reason TEXT,
    retried INTEGER NOT NULL DEFAULT 0 CHECK(retried IN (0,1))
);
CREATE UNIQUE INDEX IF NOT EXISTS recovery_runs_once ON recovery_runs(agent, at_ms);
CREATE INDEX IF NOT EXISTS recovery_runs_state ON recovery_runs(state, project, id);
CREATE INDEX IF NOT EXISTS recovery_runs_agent ON recovery_runs(agent, state, id);
CREATE TABLE IF NOT EXISTS recovery_sessions (
    agent INTEGER PRIMARY KEY,
    record TEXT NOT NULL
);";

const COLUMNS: &str = "id,agent,project,kind,harness,at_ms,turn,task,message,asked,record,cwd,record_file,paused,state,reason";

const WAITING: &str = "(state IN ('interrupted','resuming')
    OR (state='failed' AND (retried=0 OR task IS NOT NULL OR message IS NOT NULL)))";

/// One team agent in a turn as the engine stops: its row, the prompt of the task it ran,
/// the owner message a master answered and its stored pane record.
struct Noted {
    agent: i64,
    project: String,
    kind: String,
    harness: String,
    heartbeat_ms: i64,
    turns: i64,
    task: Option<i64>,
    worktree: Option<String>,
    prompt: Option<String>,
    message: Option<i64>,
    text: Option<String>,
    record: Option<String>,
}

/// One interrupted run as the store keeps it.
struct Stored {
    id: i64,
    agent: i64,
    project: String,
    kind: String,
    harness: String,
    at_ms: i64,
    turn: i64,
    task: Option<i64>,
    message: Option<i64>,
    asked: Option<String>,
    record: Option<String>,
    cwd: Option<String>,
    record_file: Option<String>,
    paused: bool,
    state: String,
    reason: Option<String>,
}

fn storage(_: rusqlite::Error) -> EngineError {
    unavailable()
}

fn invalid() -> EngineError {
    EngineError("recovery_record_invalid".into())
}

/// Adds the recovery table to the engine database when missing, and the `retried` column
/// to a table made before it. Input: the connection. Output: done, or `recovery_unavailable`.
pub(super) fn initialize(db: &Connection) -> Result<(), EngineError> {
    db.execute_batch(SCHEMA).map_err(storage)?;
    let has = || {
        db.query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_info('recovery_runs') WHERE name='retried')",
            [],
            |row| row.get::<_, bool>(0),
        )
        .map_err(storage)
    };
    if !has()?
        && db
            .execute_batch(
                "ALTER TABLE recovery_runs ADD COLUMN retried INTEGER NOT NULL DEFAULT 0 CHECK(retried IN (0,1))",
            )
            .is_err()
        && !has()?
    {
        return Err(unavailable());
    }
    Ok(())
}

/// Keeps the first non-empty line of what a turn was asked, trimmed and bounded.
/// Input: the task prompt or message text. Output: the line, or none for blank text.
fn first_line(text: &str) -> Option<String> {
    let line = text.lines().map(str::trim).find(|line| !line.is_empty())?;
    let asked = bound_text(line, ASKED_BYTES_MAX);
    assert!(!asked.is_empty() && asked.len() <= ASKED_BYTES_MAX);
    Some(asked)
}

/// Finds the exact session record a resume of the pane's harness conversation continues:
/// the record a resumed pane named, else the Claude Code transcript or the Codex rollout
/// that started in the pane's folder after the pane did, as a transcript reader finds it.
/// Input: the pane record. Output: the record file, or none when it cannot be found.
fn record_file(pane: &PaneRecord) -> Option<PathBuf> {
    if let Some((file, _)) = &pane.resumed {
        return Some(file.clone());
    }
    let source = match &pane.source {
        RecordSource::CodexRollouts { sessions } => Transcript::CodexRollouts {
            sessions: sessions.clone(),
        },
        RecordSource::ClaudeSession { file } => Transcript::ClaudeSession { file: file.clone() },
    };
    let since = UNIX_EPOCH.checked_add(Duration::from_millis(pane.since_ms))?;
    let mut reader = TranscriptReader::adopted(source, &pane.cwd, since);
    reader.read().ok()?;
    let (_, file) = reader.conversation()?;
    assert!(file.is_absolute());
    Some(file)
}

fn noted(row: &Row<'_>) -> rusqlite::Result<Noted> {
    Ok(Noted {
        agent: row.get(0)?,
        project: row.get(1)?,
        kind: row.get(2)?,
        harness: row.get(3)?,
        heartbeat_ms: row.get(4)?,
        turns: row.get(5)?,
        task: row.get(6)?,
        worktree: row.get(7)?,
        prompt: row.get(8)?,
        message: row.get(9)?,
        text: row.get(10)?,
        record: row.get(11)?,
    })
}

/// Stores one `stopped` run for every team agent marked in a turn, or shown idle with its
/// task still open while its turn is held for its background commands, keyed by the agent and
/// the time it was last seen running, so noting the same stop twice keeps one run. The
/// run's session is its pane record, else the record of the turn's own harness session;
/// its folder is that session's working folder, else the agent's worktree.
/// Inputs: the team runtime's connection and the stop time, none after a crash.
/// Output: done, or `recovery_unavailable`.
pub(super) fn capture(db: &Connection, stopped_ms: Option<i64>) -> Result<(), EngineError> {
    initialize(db)?;
    let rows: Vec<Noted> = {
        let mut statement = db
            .prepare(
                "SELECT a.id,a.project,a.role,a.harness,a.heartbeat_ms,a.turns,a.task,a.worktree,
                        t.prompt,m.id,m.text,COALESCE(p.record,s.record)
                 FROM team_agents a
                 LEFT JOIN team_tasks t ON t.id=a.task
                 LEFT JOIN team_messages m ON a.role='master' AND m.id=(
                     SELECT MAX(id) FROM team_messages WHERE project=a.project AND state='sent')
                 LEFT JOIN team_panes p ON p.agent=a.id
                 LEFT JOIN recovery_sessions s ON s.agent=a.id
                 WHERE (a.status='working' OR (a.status='idle' AND a.task IS NOT NULL))
                 AND a.role IN ('master','sub')
                 ORDER BY a.id LIMIT ?1",
            )
            .map_err(storage)?;
        statement
            .query_map([CAPTURED_MAX], noted)
            .map_err(storage)?
            .collect::<Result<_, _>>()
            .map_err(storage)?
    };
    for row in rows {
        let asked = row
            .prompt
            .as_deref()
            .or(row.text.as_deref())
            .and_then(first_line);
        let pane = row.record.as_deref().and_then(PaneRecord::decode);
        let cwd = match &pane {
            Some(pane) => pane.cwd.to_str().map(str::to_owned),
            None => row
                .worktree
                .filter(|folder| Path::new(folder).is_absolute()),
        };
        let record = pane.and_then(|pane| pane.encode());
        db.execute(
            "INSERT OR IGNORE INTO recovery_runs(agent,project,kind,harness,at_ms,turn,task,message,asked,record,cwd,state)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,'stopped')",
            params![
                row.agent,
                row.project,
                row.kind,
                row.harness,
                stopped_ms.unwrap_or(row.heartbeat_ms),
                row.turns.max(0).saturating_add(1),
                row.task,
                row.message,
                asked,
                record,
                cwd
            ],
        )
        .map_err(storage)?;
    }
    Ok(())
}

/// Settles every `stopped` run: dropped when its agent runs again, as an agent the team
/// runtime took over in its live pane does, unless it is a sub-agent's run holding a task,
/// which its agent resumes in that pane; every kept run is recorded `interrupted` with the
/// session record file its pane names and whether its project's team is paused now. Keeps the
/// newest `RUNS_PER_PROJECT_MAX` resumed runs of each project; a run still open or failed
/// is kept until it resumes, since it holds its task or message back.
/// Input: the engine database with the team tables. Output: done.
pub(super) fn settle(db: &Connection) -> Result<(), EngineError> {
    let waiting: Vec<(i64, Option<String>)> = {
        let mut statement = db
            .prepare(
                "SELECT id,record FROM recovery_runs WHERE state='stopped' ORDER BY id LIMIT ?1",
            )
            .map_err(storage)?;
        statement
            .query_map([CAPTURED_MAX], |row| Ok((row.get(0)?, row.get(1)?)))
            .map_err(storage)?
            .collect::<Result<_, _>>()
            .map_err(storage)?
    };
    let files: Vec<(i64, Option<String>)> = waiting
        .into_iter()
        .map(|(id, record)| {
            let file = record
                .as_deref()
                .and_then(PaneRecord::decode)
                .and_then(|pane| record_file(&pane))
                .and_then(|file| file.to_str().map(str::to_owned));
            (id, file)
        })
        .collect();
    let tx = Transaction::new_unchecked(db, TransactionBehavior::Immediate).map_err(storage)?;
    tx.execute(
        "DELETE FROM recovery_runs WHERE state='stopped'
             AND (kind='master' OR task IS NULL) AND agent IN (
             SELECT id FROM team_agents WHERE status IN ('starting','idle','working'))",
        [],
    )
    .map_err(storage)?;
    let now = now_ms();
    for (id, file) in files {
        tx.execute(
            "UPDATE recovery_runs SET state='interrupted', recorded_ms=?2, record_file=?3,
                 paused=EXISTS(SELECT 1 FROM team_paused_projects p WHERE p.project=recovery_runs.project)
             WHERE id=?1 AND state='stopped'",
            params![id, now, file],
        )
        .map_err(storage)?;
    }
    tx.execute(
        "DELETE FROM recovery_runs WHERE id IN (
             SELECT id FROM (
                 SELECT id, ROW_NUMBER() OVER (PARTITION BY project ORDER BY id DESC) AS newer
                 FROM recovery_runs WHERE state='resumed')
             WHERE newer > ?1)",
        [i64::try_from(RUNS_PER_PROJECT_MAX).map_err(|_| unavailable())?],
    )
    .map_err(storage)?;
    tx.commit().map_err(storage)
}

fn stored(row: &Row<'_>) -> rusqlite::Result<Stored> {
    Ok(Stored {
        id: row.get(0)?,
        agent: row.get(1)?,
        project: row.get(2)?,
        kind: row.get(3)?,
        harness: row.get(4)?,
        at_ms: row.get(5)?,
        turn: row.get(6)?,
        task: row.get(7)?,
        message: row.get(8)?,
        asked: row.get(9)?,
        record: row.get(10)?,
        cwd: row.get(11)?,
        record_file: row.get(12)?,
        paused: row.get(13)?,
        state: row.get(14)?,
        reason: row.get(15)?,
    })
}

/// Reads a stored absolute path. Input: the text or none. Output: the path, none for
/// none, or `recovery_record_invalid` for a relative one the store never writes.
fn absolute(text: Option<String>) -> Result<Option<PathBuf>, EngineError> {
    match text.map(PathBuf::from) {
        Some(path) if !path.is_absolute() => Err(invalid()),
        path => Ok(path),
    }
}

/// Reads one stored interrupted run. Input: its columns. Output: the run, or
/// `recovery_record_invalid` for a project, kind, state, path or pane record the store
/// never writes.
fn decode(stored: Stored) -> Result<Interrupted, EngineError> {
    let kind = match stored.kind.as_str() {
        "master" => AgentRole::Master,
        "sub" => AgentRole::Sub,
        _ => return Err(invalid()),
    };
    let record = match stored.record {
        Some(text) => Some(PaneRecord::decode(&text).ok_or_else(invalid)?),
        None => None,
    };
    Ok(Interrupted {
        id: stored.id,
        agent: stored.agent,
        project: ProjectId::new(stored.project).map_err(|_| invalid())?,
        kind,
        harness: stored.harness,
        at: stored.at_ms,
        last_turn: LastTurn {
            number: stored.turn,
            task: stored.task,
            message: stored.message,
            asked: stored.asked,
        },
        record,
        cwd: absolute(stored.cwd)?,
        record_file: absolute(stored.record_file)?,
        paused: stored.paused,
        state: RunState::parse(&stored.state).ok_or_else(invalid)?,
        reason: stored.reason,
    })
}

fn read(
    db: &Connection,
    sql: &str,
    values: impl rusqlite::Params,
) -> Result<Vec<Interrupted>, EngineError> {
    let rows: Vec<Stored> = {
        let mut statement = db.prepare(sql).map_err(storage)?;
        statement
            .query_map(values, stored)
            .map_err(storage)?
            .collect::<Result<_, _>>()
            .map_err(storage)?
    };
    rows.into_iter().map(decode).collect()
}

/// Reads the runs in the given resume states, oldest first, of one project or of all.
/// Inputs: the store, the project or none, the states and the most runs to read.
/// Output: the runs, or a plain code.
pub(super) fn runs(
    db: &Connection,
    project: Option<&ProjectId>,
    states: &[RunState],
    max: usize,
) -> Result<Vec<Interrupted>, EngineError> {
    assert!(!states.is_empty() && states.len() <= 4);
    let limit = i64::try_from(max).map_err(|_| unavailable())?;
    let names: Vec<&str> = states.iter().map(|state| state.name()).collect();
    read(
        db,
        &format!(
            "SELECT {COLUMNS} FROM recovery_runs
             WHERE state IN ('{}') AND (?1 IS NULL OR project=?1)
             ORDER BY id LIMIT ?2",
            names.join("','")
        ),
        params![project.map(ProjectId::as_str), limit],
    )
}

/// Reads a project's runs in one snapshot: the oldest runs still waiting, open, or failed
/// and holding their task or message or not yet retried, with how many more wait, and the
/// newest settled ones, resumed or failed after a retry that let their item go, each oldest
/// first. Inputs: the store, the project and the most runs of each kind. Output: the runs.
pub(super) fn listed(
    db: &Connection,
    project: &ProjectId,
    max: usize,
) -> Result<Listed, EngineError> {
    let limit = i64::try_from(max).map_err(|_| unavailable())?;
    let tx = Transaction::new_unchecked(db, TransactionBehavior::Deferred).map_err(storage)?;
    let waiting = read(
        &tx,
        &format!(
            "SELECT {COLUMNS} FROM recovery_runs WHERE project=?1 AND {WAITING}
             ORDER BY id LIMIT ?2"
        ),
        params![project.as_str(), limit],
    )?;
    let count: i64 = tx
        .query_row(
            &format!("SELECT COUNT(*) FROM recovery_runs WHERE project=?1 AND {WAITING}"),
            [project.as_str()],
            |row| row.get(0),
        )
        .map_err(storage)?;
    let settled = read(
        &tx,
        &format!(
            "SELECT {COLUMNS} FROM (
                 SELECT {COLUMNS} FROM recovery_runs
                 WHERE project=?1 AND state IN ('resumed','failed') AND NOT {WAITING}
                 ORDER BY id DESC LIMIT ?2)
             ORDER BY id"
        ),
        params![project.as_str(), limit],
    )?;
    tx.commit().map_err(storage)?;
    let count = usize::try_from(count).map_err(|_| unavailable())?;
    assert!(waiting.len() <= max && settled.len() <= max && waiting.len() <= count);
    assert!(waiting.iter().all(|run| run.state != RunState::Resumed));
    Ok(Listed {
        more: count - waiting.len(),
        waiting,
        settled,
    })
}

/// Reads the newest run of one agent whose resume started and has not settled.
/// Inputs: the store and the agent. Output: the run, or none.
pub(super) fn resuming(db: &Connection, agent: i64) -> Result<Option<Interrupted>, EngineError> {
    let mut found = read(
        db,
        &format!(
            "SELECT {COLUMNS} FROM recovery_runs WHERE agent=?1 AND state='resuming'
             ORDER BY id DESC LIMIT 1"
        ),
        [agent],
    )?;
    Ok(found.pop())
}

/// Moves a run between resume states, keeping a failure's reason bounded and clearing it
/// otherwise. Inputs: the store, the run, its expected state, the new state and the reason.
/// Output: whether the run was in the expected state.
pub(super) fn mark(
    db: &Connection,
    run: i64,
    from: RunState,
    to: RunState,
    reason: Option<&str>,
) -> Result<bool, EngineError> {
    assert_ne!(from, to);
    assert_eq!(reason.is_some(), to == RunState::Failed);
    let reason = reason.map(|reason| bound_text(reason, REASON_BYTES_MAX));
    let changed = db
        .execute(
            "UPDATE recovery_runs SET state=?3, reason=?4 WHERE id=?1 AND state=?2",
            params![run, from.name(), to.name(), reason],
        )
        .map_err(storage)?;
    Ok(changed == 1)
}

/// Sets the agent's newest failed run waiting to resume again, without its reason, marked
/// retried. Inputs: the store and the agent. Output: whether a failed run was found.
pub(super) fn retry(db: &Connection, agent: i64) -> Result<bool, EngineError> {
    let changed = db
        .execute(
            "UPDATE recovery_runs SET state='interrupted', reason=NULL, retried=1
             WHERE id=(SELECT MAX(id) FROM recovery_runs WHERE agent=?1 AND state='failed')",
            [agent],
        )
        .map_err(storage)?;
    Ok(changed == 1)
}

/// Tells whether the owner retried the run. Inputs: the store and the run.
/// Output: the answer, false for a run the store no longer has.
pub(super) fn retried(db: &Connection, run: i64) -> Result<bool, EngineError> {
    db.query_row(
        "SELECT retried FROM recovery_runs WHERE id=?1",
        [run],
        |row| row.get(0),
    )
    .optional()
    .map(|retried| retried.unwrap_or(false))
    .map_err(storage)
}

fn column(item: HeldItem) -> (&'static str, i64) {
    match item {
        HeldItem::Task(task) => ("task", task),
        HeldItem::Message(message) => ("message", message),
    }
}

/// Tells whether a run holds the item: one in an open or failed resume state, or a
/// `stopped` one `settle` keeps: a sub-agent's, or one whose agent does not run again.
/// Inputs: the store and the item. Output: the answer, or `recovery_unavailable`.
pub(super) fn holds(db: &Connection, item: HeldItem) -> Result<bool, EngineError> {
    initialize(db)?;
    let (column, id) = column(item);
    db.query_row(
        &format!(
            "SELECT EXISTS(SELECT 1 FROM recovery_runs r WHERE r.{column}=?1 AND (
                 r.state IN ('interrupted','resuming','failed')
                 OR (r.state='stopped' AND (r.kind='sub' OR NOT EXISTS(
                     SELECT 1 FROM team_agents a WHERE a.id=r.agent
                     AND a.status IN ('starting','idle','working'))))))"
        ),
        [id],
        |row| row.get(0),
    )
    .map_err(storage)
}

/// Drops every `stopped` run holding the item and clears the item from every other run, in
/// the caller's transaction when it has one. Inputs: the store and the item. Output: done.
pub(super) fn release(db: &Connection, item: HeldItem) -> Result<(), EngineError> {
    initialize(db)?;
    let (column, id) = column(item);
    db.execute(
        &format!("DELETE FROM recovery_runs WHERE {column}=?1 AND state='stopped'"),
        [id],
    )
    .map_err(storage)?;
    db.execute(
        &format!("UPDATE recovery_runs SET {column}=NULL WHERE {column}=?1"),
        [id],
    )
    .map_err(storage)?;
    assert!(!holds(db, item)?);
    Ok(())
}

/// Reads the kept per-turn session records, of one agent or of all, at most `SCANNED_MAX`;
/// a record that does not decode is left out. Inputs: the store and the agent or none.
/// Output: the records by agent.
pub(super) fn sessions(
    db: &Connection,
    agent: Option<i64>,
) -> Result<BTreeMap<i64, PaneRecord>, EngineError> {
    initialize(db)?;
    let limit = i64::try_from(super::SCANNED_MAX).map_err(|_| unavailable())?;
    let mut statement = db
        .prepare(
            "SELECT agent,record FROM recovery_sessions WHERE ?1 IS NULL OR agent=?1
             ORDER BY agent LIMIT ?2",
        )
        .map_err(storage)?;
    let rows = statement
        .query_map(params![agent, limit], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(storage)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(storage)?;
    Ok(rows
        .into_iter()
        .filter_map(|(id, text)| PaneRecord::decode(&text).map(|record| (id, record)))
        .collect())
}

/// Keeps the session record of the agent's running per-turn harness session.
/// Inputs: the store, the agent and the record. Output: done.
pub(super) fn remember(
    db: &Connection,
    agent: i64,
    record: &PaneRecord,
) -> Result<(), EngineError> {
    let Some(text) = record.encode() else {
        return Err(invalid());
    };
    db.execute(
        "INSERT OR REPLACE INTO recovery_sessions(agent,record) VALUES (?1,?2)",
        params![agent, text],
    )
    .map_err(storage)?;
    Ok(())
}

/// Drops the agent's kept per-turn session record. Inputs: the store and the agent.
/// Output: done.
pub(super) fn forget(db: &Connection, agent: i64) -> Result<(), EngineError> {
    db.execute("DELETE FROM recovery_sessions WHERE agent=?1", [agent])
        .map_err(storage)?;
    Ok(())
}
