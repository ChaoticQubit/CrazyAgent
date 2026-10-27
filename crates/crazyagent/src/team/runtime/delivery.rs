use super::super::{MessageAuthor, MessageState, append_message, direct, events, now_ms};
use super::{Busy, error};
use crate::engine::EngineError;
use crate::harnesses::interactive::Transcript;
use crate::harnesses::terminal_session::{PaneRecord, RecordSource};
use crate::harnesses::transcript::{TranscriptReader, typed_after};
use crate::ledger::{self, Action, ActionKind, Check, Outcome, Resolution};
use crate::protocol::ProjectId;
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{Duration, UNIX_EPOCH};

/// The most bytes of an item's first line kept to find it again in the agent's record.
pub(super) const MARKER_BYTES_MAX: usize = 200;
/// The most delivery intents of one kind kept pending with an unknown outcome until the
/// owner dismisses them. A start closes older ones as unknown, their team notes kept, so
/// undismissed ones never fill the ledger that every delivery and agent start shares.
pub(super) const UNKNOWN_KEPT: usize = 64;

/// The team note for a message the master had already received when the engine stopped.
pub(super) const DELIVERED_NOTE: &str = "A message reached the master just before CrazyAgent closed, so it was not sent again. If no answer to it appears, send it again.";
/// The team note for a message whose arrival the master's record cannot confirm or rule out.
pub(super) const UNKNOWN_NOTE: &str = "CrazyAgent closed while it was sending a message to the master, and the master's record cannot tell whether it arrived, so it was not sent again. Send it again if it is still needed.";
/// The task error for a task its sub-agent had already received when the engine stopped.
pub(super) const DELIVERED_TASK: &str = "The sub-agent already had this task when CrazyAgent closed, so it was not handed out again. Any work it did is in its worktree.";
/// The task error for a task whose arrival the sub-agent's record cannot confirm or rule out.
pub(super) const UNKNOWN_TASK: &str = "CrazyAgent closed while it was handing this task to a sub-agent, and the sub-agent's record cannot tell whether it arrived, so it was not handed out again.";

/// What a delivery intent keeps to check it after a restart: the agent, its pane record,
/// the session record and the harness that reads it, the record's length before the item
/// was typed, the item's first line, the newest owner note typed with it, and whether an
/// engine start already found its outcome unknown, after which no record is read for it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Intent {
    agent: i64,
    pane: Option<PaneRecord>,
    harness: Option<String>,
    record: Option<PathBuf>,
    offset: u64,
    marker: String,
    notes: Option<i64>,
    #[serde(default)]
    unknown: bool,
}

/// Records, before the runtime types a message or task into an agent, that it is about to,
/// with where the agent's session record ends. A pending intent of the same item and agent
/// is kept, so an item that waits across ticks keeps its first offset; one for another
/// agent is closed by what that agent's record shows. Inputs: the connection, the item,
/// the agent, the item's own text and the newest owner note typed with it. Output: true
/// once recorded, false while the ledger is full, when the item waits.
pub(super) fn intend(
    connection: &Connection,
    item: Busy,
    agent: i64,
    text: &str,
    notes: Option<i64>,
) -> Result<bool, EngineError> {
    let (kind, key) = (kind(item), key(item));
    if let Some(open) = ledger::pending_key(connection, kind, &key)? {
        let kept = intent(&open);
        if kept.as_ref().is_some_and(|kept| kept.agent == agent) {
            return Ok(true);
        }
        let outcome = match kept.map_or(Resolution::Unknown, |kept| delivered(&kept, None)) {
            Resolution::Done => Outcome::Done,
            Resolution::NotStarted => Outcome::Failed,
            Resolution::Unknown => Outcome::Unknown,
        };
        ledger::finish(connection, open.id, outcome)?;
    }
    let pane = panes(connection, Some(agent))?.remove(&agent);
    let found = pane.as_ref().and_then(record_of);
    let offset = found
        .as_ref()
        .and_then(|(_, record, _)| std::fs::symlink_metadata(record).ok())
        .map_or(0, |metadata| metadata.len());
    let (harness, record) = found.map_or((None, None), |(harness, record, _)| {
        (Some(harness.to_owned()), Some(record))
    });
    let intent = Intent {
        agent,
        pane,
        harness,
        record,
        offset,
        marker: flat(
            text.lines()
                .find(|line| !line.trim().is_empty())
                .unwrap_or_default(),
        ),
        notes,
        unknown: false,
    };
    let payload = serde_json::to_string(&intent).map_err(error)?;
    match ledger::begin(connection, kind, &key, &payload) {
        Ok(_) => Ok(true),
        Err(problem) if problem.0 == "ledger_full" => Ok(false),
        Err(problem) => Err(problem),
    }
}

/// Closes the intent of an item the runtime has settled itself, after it wrote the item's
/// new state: done once the agent's turn on it ended, failed when it was never handed to
/// the agent. Inputs: the connection, the item, if any, and whether it was handed over.
/// Output: Ok, also when no intent is open.
pub(super) fn ended(
    connection: &Connection,
    item: Option<Busy>,
    handed: bool,
) -> Result<(), EngineError> {
    let Some(item) = item else {
        return Ok(());
    };
    let Some(open) = ledger::pending_key(connection, kind(item), &key(item))? else {
        return Ok(());
    };
    let outcome = if handed {
        Outcome::Done
    } else {
        Outcome::Failed
    };
    ledger::finish(connection, open.id, outcome)
}

/// At engine start, right after sent messages and running tasks were reset to wait again,
/// checks every delivery the earlier engine intended against the agent's own session record
/// and settles each, in one transaction. A message that arrived is answered with its owner
/// notes delivered, and a task that arrived fails with a plain reason, which its master
/// hears once the transaction committed; neither is typed again. One that never arrived
/// waits and is typed once more. One the record cannot tell about is failed with a note,
/// never typed again, and its intent stays pending for the owner, up to `UNKNOWN_KEPT` of
/// each kind, marked so later starts keep it unknown without reading any record again.
/// Input: the engine connection. Output: Ok, or a SQLite or ledger failure.
pub(super) fn settle(connection: &Connection) -> Result<(), EngineError> {
    ledger::initialize(connection)?;
    let tx =
        Transaction::new_unchecked(connection, TransactionBehavior::Immediate).map_err(error)?;
    let check = Delivered(panes(&tx, None)?);
    let now = now_ms();
    let messages = ledger::resolve(&tx, ActionKind::Message, &check)?;
    for action in &messages.done {
        settle_message(&tx, action, true, now)?;
    }
    for action in &messages.unknown {
        settle_message(&tx, action, false, now)?;
    }
    keep_newest(&tx, &messages.unknown)?;
    let tasks = ledger::resolve(&tx, ActionKind::Dispatch, &check)?;
    let mut failed = Vec::new();
    for action in &tasks.done {
        failed.extend(settle_task(&tx, action, DELIVERED_TASK, now)?);
    }
    for action in &tasks.unknown {
        failed.extend(settle_task(&tx, action, UNKNOWN_TASK, now)?);
    }
    keep_newest(&tx, &tasks.unknown)?;
    tx.commit().map_err(error)?;
    for task in failed {
        events::task_ended(connection, task, now)?;
    }
    Ok(())
}

/// Closes as unknown every unknown delivery intent of one kind but the newest
/// `UNKNOWN_KEPT`, which stay pending until the owner dismisses them, each marked found
/// unknown so no later start reads a record for it or settles it on a newer session.
/// Inputs: the transaction and the kind's unknown intents, oldest first, as
/// `ledger::pending` lists them. Output: Ok, or a ledger failure.
fn keep_newest(tx: &Transaction<'_>, unknown: &[Action]) -> Result<(), EngineError> {
    let older = unknown.len().saturating_sub(UNKNOWN_KEPT);
    for action in &unknown[..older] {
        ledger::finish(tx, action.id, Outcome::Unknown)?;
    }
    for action in &unknown[older..] {
        let Some(mut kept) = intent(action).filter(|kept| !kept.unknown) else {
            continue;
        };
        kept.unknown = true;
        let payload = serde_json::to_string(&kept).map_err(error)?;
        if payload.len() <= ledger::PAYLOAD_BYTES_MAX {
            ledger::note(tx, action.id, &payload)?;
        }
    }
    Ok(())
}

struct Delivered(BTreeMap<i64, PaneRecord>);

impl Check for Delivered {
    fn check(&self, action: &Action) -> Resolution {
        intent(action).map_or(Resolution::Unknown, |intent| {
            let pane = self.0.get(&intent.agent);
            delivered(&intent, pane)
        })
    }
}

/// Tells from the agent's session record whether an intended item was typed: the record
/// the intent named, read past its offset, else the record of the intent's pane or of the
/// agent's latest pane, read from where that pane's turns begin. An intent an earlier start
/// found unknown stays unknown, read from no record. Inputs: the intent and the agent's
/// latest pane record. Output: Done, NotStarted, or Unknown with no readable record.
fn delivered(intent: &Intent, latest: Option<&PaneRecord>) -> Resolution {
    if intent.unknown || intent.marker.is_empty() {
        return Resolution::Unknown;
    }
    let found = match (&intent.harness, &intent.record) {
        (Some(harness), Some(record)) => Some((harness.clone(), record.clone(), intent.offset)),
        _ => intent
            .pane
            .as_ref()
            .or(latest)
            .and_then(record_of)
            .map(|(harness, record, start)| (harness.to_owned(), record, start)),
    };
    let Some((harness, record, offset)) = found else {
        return Resolution::Unknown;
    };
    match typed_after(&harness, &record, offset) {
        None => Resolution::Unknown,
        Some(typed) if typed.iter().any(|text| flat(text).contains(&intent.marker)) => {
            Resolution::Done
        }
        Some(_) => Resolution::NotStarted,
    }
}

fn kind(item: Busy) -> ActionKind {
    match item {
        Busy::Message(_) => ActionKind::Message,
        Busy::Task(_) => ActionKind::Dispatch,
    }
}

fn key(item: Busy) -> String {
    match item {
        Busy::Message(id) => format!("team-message/{id}"),
        Busy::Task(id) => format!("team-task/{id}"),
    }
}

fn intent(action: &Action) -> Option<Intent> {
    serde_json::from_str(&action.payload).ok()
}

fn item_id(action: &Action) -> Option<i64> {
    action.key.rsplit('/').next()?.parse().ok()
}

/// Flattens every whitespace run to one space and cuts the text to `MARKER_BYTES_MAX`
/// bytes on a character boundary, so typed and recorded text compare alike.
/// Input: the text. Output: at most `MARKER_BYTES_MAX` bytes.
fn flat(text: &str) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut end = flat.len().min(MARKER_BYTES_MAX);
    while !flat.is_char_boundary(end) {
        end -= 1;
    }
    flat[..end].to_owned()
}

fn panes(
    connection: &Connection,
    agent: Option<i64>,
) -> Result<BTreeMap<i64, PaneRecord>, EngineError> {
    let mut statement = connection
        .prepare("SELECT agent,record FROM team_panes WHERE ?1 IS NULL OR agent=?1")
        .map_err(error)?;
    let rows = statement
        .query_map([agent], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(error)?;
    Ok(rows
        .into_iter()
        .filter_map(|(id, text)| PaneRecord::decode(&text).map(|record| (id, record)))
        .collect())
}

/// Finds the session record a pane's harness writes: the resumed conversation's record, the
/// Claude Code transcript it names, or the Codex rollout that started in its folder after it
/// started. Input: the pane record. Output: the harness name the reader parses it as, the
/// record and the offset where the pane's turns begin, or none before a rollout exists.
fn record_of(pane: &PaneRecord) -> Option<(&'static str, PathBuf, u64)> {
    let (harness, source) = match &pane.source {
        RecordSource::ClaudeSession { file } => (
            "claude-code",
            Transcript::ClaudeSession { file: file.clone() },
        ),
        RecordSource::CodexRollouts { sessions } => (
            "codex",
            Transcript::CodexRollouts {
                sessions: sessions.clone(),
            },
        ),
    };
    if let Some((record, start)) = &pane.resumed {
        return Some((harness, record.clone(), *start));
    }
    if let Transcript::ClaudeSession { file } = source {
        return Some((harness, file, 0));
    }
    let since = UNIX_EPOCH.checked_add(Duration::from_millis(pane.since_ms))?;
    let mut reader = TranscriptReader::adopted(source, &pane.cwd, since);
    let _ = reader.read();
    reader
        .conversation()
        .map(|(_, record)| (harness, record, 0))
}

fn settle_message(
    tx: &Transaction<'_>,
    action: &Action,
    arrived: bool,
    now: i64,
) -> Result<(), EngineError> {
    let Some(message) = item_id(action) else {
        return Ok(());
    };
    let project: Option<String> = tx
        .query_row(
            "SELECT project FROM team_messages WHERE id=?1 AND state='pending'",
            [message],
            |row| row.get(0),
        )
        .optional()
        .map_err(error)?;
    let Some(Ok(project)) = project.map(ProjectId::new) else {
        return Ok(());
    };
    let state = if arrived { "answered" } else { "failed" };
    tx.execute(
        "UPDATE team_messages SET state=?2 WHERE id=?1",
        params![message, state],
    )
    .map_err(error)?;
    if arrived && let Some(newest) = intent(action).and_then(|intent| intent.notes) {
        direct::delivered(tx, &project, newest)?;
    }
    let note = if arrived {
        DELIVERED_NOTE
    } else {
        UNKNOWN_NOTE
    };
    let _ = append_message(
        tx,
        &project,
        MessageAuthor::Engine,
        note,
        MessageState::Note,
        now,
    )?;
    Ok(())
}

/// Fails a task its intent names when it still waits after the reset, under its agent.
/// Inputs: the transaction, the action, the reason and the time. Output: the task when it
/// was failed, none when it no longer waits or the intent is unreadable.
fn settle_task(
    tx: &Transaction<'_>,
    action: &Action,
    reason: &str,
    now: i64,
) -> Result<Option<i64>, EngineError> {
    let (Some(task), Some(intent)) = (item_id(action), intent(action)) else {
        return Ok(None);
    };
    let changed = tx
        .execute(
            "UPDATE team_tasks SET status='failed', agent=?2, error=?3, finished_ms=?4
             WHERE id=?1 AND status='queued'",
            params![task, intent.agent, reason, now],
        )
        .map_err(error)?;
    Ok((changed == 1).then_some(task))
}
