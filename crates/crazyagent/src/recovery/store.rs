use super::{ASKED_BYTES_MAX, Interrupted, LastTurn, RUNS_PER_PROJECT_MAX, unavailable};
use crate::engine::EngineError;
use crate::harnesses::interactive::Transcript;
use crate::harnesses::terminal_session::{PaneRecord, RecordSource};
use crate::harnesses::transcript::TranscriptReader;
use crate::protocol::ProjectId;
use crate::team::{AgentRole, bound_text, now_ms};
use rusqlite::{Connection, Row, Transaction, TransactionBehavior, params};
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
    state TEXT NOT NULL CHECK(state IN ('stopped','interrupted')),
    recorded_ms INTEGER
);
CREATE UNIQUE INDEX IF NOT EXISTS recovery_runs_once ON recovery_runs(agent, at_ms);
CREATE INDEX IF NOT EXISTS recovery_runs_state ON recovery_runs(state, project, id);";

const COLUMNS: &str =
    "id,agent,project,kind,harness,at_ms,turn,task,message,asked,record,cwd,record_file,paused";

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
}

fn storage(_: rusqlite::Error) -> EngineError {
    unavailable()
}

fn invalid() -> EngineError {
    EngineError("recovery_record_invalid".into())
}

/// Adds the recovery table to the engine database when missing.
/// Input: the connection. Output: done, or `recovery_unavailable`.
pub(super) fn initialize(db: &Connection) -> Result<(), EngineError> {
    db.execute_batch(SCHEMA).map_err(storage)
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

/// Stores one `stopped` run for every team agent marked in a turn, keyed by the agent and
/// the time it was last seen running, so noting the same stop twice keeps one run. The
/// run's folder is its pane's working folder, else the agent's worktree.
/// Inputs: the team runtime's connection and the stop time, none after a crash.
/// Output: done, or `recovery_unavailable`.
pub(super) fn capture(db: &Connection, stopped_ms: Option<i64>) -> Result<(), EngineError> {
    initialize(db)?;
    let rows: Vec<Noted> = {
        let mut statement = db
            .prepare(
                "SELECT a.id,a.project,a.role,a.harness,a.heartbeat_ms,a.turns,a.task,a.worktree,
                        t.prompt,m.id,m.text,p.record
                 FROM team_agents a
                 LEFT JOIN team_tasks t ON t.id=a.task
                 LEFT JOIN team_messages m ON a.role='master' AND m.id=(
                     SELECT MAX(id) FROM team_messages WHERE project=a.project AND state='sent')
                 LEFT JOIN team_panes p ON p.agent=a.id
                 WHERE a.status='working' AND a.role IN ('master','sub')
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
/// runtime took over in its live pane does, else recorded `interrupted` with the session
/// record file its pane names and whether its project's team is paused now. Keeps the
/// newest `RUNS_PER_PROJECT_MAX` interrupted runs of each project.
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
        "DELETE FROM recovery_runs WHERE state='stopped' AND agent IN (
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
                 FROM recovery_runs WHERE state='interrupted')
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
/// `recovery_record_invalid` for a project, kind, path or pane record the store never writes.
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
    })
}

/// Reads the interrupted runs not yet handled, oldest first, of one project or of all.
/// Inputs: the store, the project or none, and the most runs to read.
/// Output: the runs, or a plain code.
pub(super) fn interrupted(
    db: &Connection,
    project: Option<&ProjectId>,
    max: usize,
) -> Result<Vec<Interrupted>, EngineError> {
    let limit = i64::try_from(max).map_err(|_| unavailable())?;
    let rows: Vec<Stored> = {
        let mut statement = db
            .prepare(&format!(
                "SELECT {COLUMNS} FROM recovery_runs
                 WHERE state='interrupted' AND (?1 IS NULL OR project=?1)
                 ORDER BY id LIMIT ?2"
            ))
            .map_err(storage)?;
        statement
            .query_map(params![project.map(ProjectId::as_str), limit], stored)
            .map_err(storage)?
            .collect::<Result<_, _>>()
            .map_err(storage)?
    };
    rows.into_iter().map(decode).collect()
}
