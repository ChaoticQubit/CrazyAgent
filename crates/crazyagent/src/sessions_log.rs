use crate::conversation::ConversationMutation;
use crate::engine::EngineError;
use crate::engine::owner::Admission;
use crate::master_session::{MasterSessionView, SESSION_DELETED, SessionDeleted};
use crate::owner_texts::TextDigest;
use crate::protocol::{Outcome, ProjectId, SessionId};
use crate::state_log::fence::Fence;
use crate::state_log::replay::{self, Clock, Logged};
use crate::state_log::{Aggregate, Appender, BODY_BYTES_MAX, Chunk, Draft, SNAPSHOT_CHUNK};
#[cfg(any(unix, test))]
use crate::storage::opening::Opening;
use crate::storage::versioned::{Refusal, Writer};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;

#[cfg(test)]
mod tests;
mod turns;
#[cfg(test)]
mod turns_tests;

pub use turns::{
    PreparationSaved, ProfileSaved, ProfilesDropped, ResolveSaved, ResolvesDropped, TurnSaved,
};
pub(crate) use turns::{
    mark_preparation, mark_profile, mark_profiles, mark_resolve, mark_session_rest, mark_turn,
    release_session_turns,
};

/// The kind of the event that logs one master session row as it stands (v2.5 L13).
pub const KIND_SESSION: &str = "SessionSaved";
/// The kind of the event that logs one conversation entry, its text as a kept digest.
pub const KIND_ENTRY: &str = "ConversationAppended";
/// The kind of the event that logs conversation entries gone from their table.
pub const KIND_ENTRIES_DROPPED: &str = "ConversationDropped";
/// The kind of the event that logs one agent's own instructions, as a kept digest.
pub const KIND_TEXT: &str = "AgentTextSaved";
/// The kind of the event that logs agents' own instructions gone from their table.
pub const KIND_TEXTS_DROPPED: &str = "AgentTextDropped";
/// The kind of the event that logs one master turn row, its body as a kept digest (L13c).
pub const KIND_TURN: &str = "MasterTurnSaved";
/// The kind of the event that logs one turn's preparation, its body as a kept digest.
pub const KIND_PREPARATION: &str = "MasterPreparationSaved";
/// The kind of the event that logs one session's waiting Resolve, its body as a digest.
pub const KIND_RESOLVE: &str = "WaitingResolveSaved";
/// The kind of the event that logs waiting Resolves gone from their table.
pub const KIND_RESOLVES_DROPPED: &str = "WaitingResolveDropped";
/// The kind of the event that logs one configured profile receipt, its record as a digest.
pub const KIND_PROFILE: &str = "ConfiguredProfileSaved";
/// The kind of the event that logs configured profile receipts gone from their table.
pub const KIND_PROFILES_DROPPED: &str = "ConfiguredProfileDropped";
/// Every kind this module writes, L13c's six from the sixth on; L12's [`SESSION_DELETED`]
/// is the delete's own.
pub const KINDS: [&str; 11] = [
    KIND_SESSION,
    KIND_ENTRY,
    KIND_ENTRIES_DROPPED,
    KIND_TEXT,
    KIND_TEXTS_DROPPED,
    KIND_TURN,
    KIND_PREPARATION,
    KIND_RESOLVE,
    KIND_RESOLVES_DROPPED,
    KIND_PROFILE,
    KIND_PROFILES_DROPPED,
];
/// The schema every sessions event is written and read at.
const SCHEMA: u16 = 1;
/// The seven tables on the log, in snapshot order: L13's three, then L13c's four.
const TABLES: [&str; 7] = [
    "master_sessions",
    "conversation_entries",
    "agent_texts",
    "master_turns",
    "master_preparations",
    "master_waiting_resolves",
    "configured_master_profiles",
];
/// The keys a write marks for the next close, made on first use; the `project` column lets
/// a demo clear of the project delete its marks with its rows.
const TOUCHED: &str = "CREATE TABLE IF NOT EXISTS sessions_touched (
    tbl TEXT NOT NULL CHECK (tbl IN ('master_sessions','conversation_entries','agent_texts','master_turns','master_preparations','master_waiting_resolves','configured_master_profiles')),
    key TEXT NOT NULL CHECK (length(key) BETWEEN 1 AND 128),
    project TEXT NOT NULL CHECK (length(project) BETWEEN 1 AND 128),
    PRIMARY KEY (tbl, key))";
/// The most keys one dropped event lists, each at most 20 digits and a comma.
const DROPPED_MAX: usize = 2_048;
/// The most marks one close reads; the rest stay marked for the next close.
const MARKS_MAX: usize = 16_384;
/// The most rows one table may bring to a snapshot run.
const SNAPSHOT_ROWS_MAX: usize = 262_144;
/// The bytes of a chunk body kept for its envelope (aggregate, table, index) around rows.
const CHUNK_ENVELOPE_BYTES: usize = 1_024;
/// The code `Appender::owner` refuses with when its revision is logged already.
const NOT_NEWER: &str = "state_log_revision_not_newer";
/// The code the store fence refuses with before an owner runtime is active.
const RUNTIME_STALE: &str = "owner_runtime_stale";

const _: () = assert!(DROPPED_MAX * 21 + CHUNK_ENVELOPE_BYTES <= BODY_BYTES_MAX);
const _: () = assert!(MARKS_MAX < crate::state_log::TRANSACTION_EVENTS_MAX);

/// One master session row: its two ids at the top level, so a Delete's redaction and a demo
/// clear's find it, the row's `created` revision and the stored session, a closed view of
/// ids, words, hashes and cursors that refuses any field more.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionSaved {
    pub(crate) project: ProjectId,
    pub(crate) session: SessionId,
    pub(crate) created: u64,
    pub(crate) view: MasterSessionView,
}

/// The kind of a conversation entry, as its `kind` column stores it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    Message,
    Brief,
}

impl EntryKind {
    /// The kind as the `kind` column stores it. Output: `message` or `brief`.
    fn as_str(self) -> &'static str {
        match self {
            Self::Message => "message",
            Self::Brief => "brief",
        }
    }

    /// Reads a stored `kind` column. Input: the word. Output: the kind, or none.
    fn parse(word: &str) -> Option<Self> {
        [Self::Message, Self::Brief]
            .into_iter()
            .find(|kind| kind.as_str() == word)
    }
}

/// One conversation entry: its project and cursor, its kind and brief version, and the
/// digest its text is kept under (`owner_texts`), so no prompt text rides in the log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EntryAppended {
    pub(crate) project: ProjectId,
    pub(crate) cursor: u64,
    pub(crate) kind: EntryKind,
    pub(crate) version: u64,
    pub(crate) text: TextDigest,
}

/// Conversation entries of one project gone from their table, by cursor, at most
/// [`DROPPED_MAX`] in one event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EntriesDropped {
    pub(crate) project: ProjectId,
    pub(crate) cursors: Vec<u64>,
}

/// One agent's own instructions: its project, its id and the digest its text is kept under.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TextSaved {
    pub(crate) project: ProjectId,
    pub(crate) agent: u64,
    pub(crate) text: TextDigest,
}

/// Agents' own instructions of one project gone from their table, by agent id, at most
/// [`DROPPED_MAX`] in one event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TextsDropped {
    pub(crate) project: ProjectId,
    pub(crate) agents: Vec<u64>,
}

/// One sessions event, one kind per change a close finds: a row as it stands or the keys
/// gone from a table. Each carries ids, words, numbers and kept-text digests, never text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Session(SessionSaved),
    Entry(EntryAppended),
    EntriesDropped(EntriesDropped),
    Text(TextSaved),
    TextsDropped(TextsDropped),
    Turn(TurnSaved),
    Preparation(PreparationSaved),
    Resolve(ResolveSaved),
    ResolvesDropped(ResolvesDropped),
    Profile(ProfileSaved),
    ProfilesDropped(ProfilesDropped),
}

impl Event {
    /// The event's kind in the log. Output: the kind.
    fn kind(&self) -> &'static str {
        match self {
            Self::Session(_) => KIND_SESSION,
            Self::Entry(_) => KIND_ENTRY,
            Self::EntriesDropped(_) => KIND_ENTRIES_DROPPED,
            Self::Text(_) => KIND_TEXT,
            Self::TextsDropped(_) => KIND_TEXTS_DROPPED,
            Self::Turn(_) => KIND_TURN,
            Self::Preparation(_) => KIND_PREPARATION,
            Self::Resolve(_) => KIND_RESOLVE,
            Self::ResolvesDropped(_) => KIND_RESOLVES_DROPPED,
            Self::Profile(_) => KIND_PROFILE,
            Self::ProfilesDropped(_) => KIND_PROFILES_DROPPED,
        }
    }

    /// The event's body in the log, its fields as JSON. Output: the bytes.
    fn body(&self) -> Result<Vec<u8>, EngineError> {
        match self {
            Self::Session(event) => serde_json::to_vec(event),
            Self::Entry(event) => serde_json::to_vec(event),
            Self::EntriesDropped(event) => serde_json::to_vec(event),
            Self::Text(event) => serde_json::to_vec(event),
            Self::TextsDropped(event) => serde_json::to_vec(event),
            Self::Turn(event) => serde_json::to_vec(event),
            Self::Preparation(event) => serde_json::to_vec(event),
            Self::Resolve(event) => serde_json::to_vec(event),
            Self::ResolvesDropped(event) => serde_json::to_vec(event),
            Self::Profile(event) => serde_json::to_vec(event),
            Self::ProfilesDropped(event) => serde_json::to_vec(event),
        }
        .map_err(error)
    }
}

/// One table a write marks a key of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Table {
    Sessions,
    Entries,
    Texts,
    Turns,
    Preparations,
    Resolves,
    Profiles,
}

impl Table {
    /// Every table, in [`TABLES`] order.
    const ALL: [Self; 7] = [
        Self::Sessions,
        Self::Entries,
        Self::Texts,
        Self::Turns,
        Self::Preparations,
        Self::Resolves,
        Self::Profiles,
    ];

    /// The table's name, as `sessions_touched` stores it. Output: the name.
    fn name(self) -> &'static str {
        match self {
            Self::Sessions => TABLES[0],
            Self::Entries => TABLES[1],
            Self::Texts => TABLES[2],
            Self::Turns => TABLES[3],
            Self::Preparations => TABLES[4],
            Self::Resolves => TABLES[5],
            Self::Profiles => TABLES[6],
        }
    }
}

/// One marked key: its table, the row's key as text and the row's project.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Mark {
    table: Table,
    key: String,
    project: String,
}

/// Marks a master session row a live writer inserted or saved, for the transaction's close.
/// Inputs: the transaction, the project and the session. Output: Ok, or the store's
/// refusal.
pub(crate) fn mark_session(
    db: &Connection,
    project: &str,
    session: &str,
) -> Result<(), EngineError> {
    mark(db, Table::Sessions, session, project)
}

/// Marks a conversation entry a live writer inserted or is about to delete, for the
/// transaction's close. Inputs: the transaction, the project and the entry's cursor.
/// Output: Ok, or the store's refusal.
pub(crate) fn mark_entry(db: &Connection, project: &str, cursor: u64) -> Result<(), EngineError> {
    mark(db, Table::Entries, &cursor.to_string(), project)
}

/// Marks the owner messages of a session's turns before a Delete removes them, by the same
/// rule as its delete statement. Inputs: the transaction and the session. Output: Ok, or
/// the store's refusal.
pub(crate) fn mark_session_entries(db: &Connection, session: &str) -> Result<(), EngineError> {
    db.execute_batch(TOUCHED).map_err(error)?;
    db.execute(
        "INSERT OR IGNORE INTO sessions_touched(tbl,key,project) SELECT 'conversation_entries', CAST(event_cursor AS TEXT), project FROM conversation_entries WHERE kind='message' AND event_cursor IN (SELECT input FROM master_turns WHERE session=?1)",
        [session],
    )
    .map_err(error)?;
    Ok(())
}

/// Marks every agent text of a project before its removal's projection deletes them, so
/// the removal's close logs them gone and lets their texts go. Inputs: the transaction and
/// the project. Output: Ok, or the store's refusal.
pub(crate) fn mark_project_texts(db: &Connection, project: &str) -> Result<(), EngineError> {
    if !exists(db, TABLES[2])? {
        return Ok(());
    }
    db.execute_batch(TOUCHED).map_err(error)?;
    db.execute(
        "INSERT OR IGNORE INTO sessions_touched(tbl,key,project) SELECT 'agent_texts', CAST(agent AS TEXT), project FROM agent_texts WHERE project=?1",
        [project],
    )
    .map_err(error)?;
    Ok(())
}

/// Marks one key for the transaction's close, once. Inputs: the transaction, the table, the
/// key and the row's project. Output: Ok, or the store's refusal.
fn mark(db: &Connection, table: Table, key: &str, project: &str) -> Result<(), EngineError> {
    assert!(
        !db.is_autocommit(),
        "a key is marked in its write's transaction"
    );
    db.execute_batch(TOUCHED).map_err(error)?;
    db.execute(
        "INSERT OR IGNORE INTO sessions_touched(tbl,key,project) VALUES (?1,?2,?3)",
        params![table.name(), key, project],
    )
    .map_err(error)?;
    Ok(())
}

/// Saves an agent's own instructions for the owner transport and logs them: the store's
/// save, then its key marked and closed behind the store's engine host at a revision `own`
/// moves, refused before an owner runtime starts. Inputs: as `agent_text::save`. Output:
/// the new version, or the refusal, which the caller rolls back.
pub(crate) fn agent_text(
    tx: &Transaction<'_>,
    project: &ProjectId,
    agent: i64,
    text: &str,
    expected: u64,
    writer: &Writer,
) -> Result<u64, Refusal> {
    let version = crate::storage::agent_text::save(tx, project, agent, text, expected, writer)?;
    mark(tx, Table::Texts, &agent.to_string(), project.as_str()).map_err(Refusal::Failed)?;
    close_own(tx).map_err(Refusal::Failed)?;
    assert!(!tx.is_autocommit(), "the caller commits the save");
    Ok(version)
}

/// Applies an owner conversation command and logs it: on Applied it moves `state` to the
/// request's next revision and closes the marks there behind the store's engine host (L10
/// `record_at`), so the request's own `events` row at next covers it. Inputs: the
/// transaction, the mutation and the revision read. Output: the outcome, or the refusal.
pub(crate) fn conversation(
    tx: &Transaction<'_>,
    mutation: &ConversationMutation,
    revision: u64,
) -> Result<Outcome, EngineError> {
    let outcome = crate::conversation::apply(tx, mutation, revision)?;
    if !matches!(outcome, Outcome::Applied) {
        return Ok(outcome);
    }
    let next = revision
        .checked_add(1)
        .ok_or_else(|| error("revision_exhausted"))?;
    let moved = tx
        .execute(
            "UPDATE state SET revision=?1 WHERE id=1 AND revision=?2",
            params![signed(next)?, signed(revision)?],
        )
        .map_err(error)?;
    if moved != 1 {
        return Err(error("state_log_revision_stale"));
    }
    let fence = crate::worktrees::log::store_fence(tx)?;
    close_at(tx, next, fence)?;
    assert!(!tx.is_autocommit(), "the entry is part of the request");
    Ok(outcome)
}

/// Closes the marks of an owner request at its moved revision behind the owner's admission.
/// Inputs: the request's transaction, its revision and the admission. Output: how many
/// events were logged (none when nothing is marked or another aggregate logged at that
/// revision first, which leaves the marks for the next close), or the fence's refusal.
pub(crate) fn close_owner(
    tx: &Transaction<'_>,
    revision: u64,
    admission: &Admission,
) -> Result<usize, EngineError> {
    close_at(tx, revision, Fence::Owner(admission.clone()))
}

/// Closes the marks of an engine writer at the revision it moved behind the store's engine
/// host. A host not active or not running here logs nothing and leaves the marks for a
/// fenced close, as the table write is the writer's own. Inputs: the transaction and the
/// revision. Output: how many events were logged, or the first refusal.
pub(crate) fn close_engine(tx: &Transaction<'_>, revision: u64) -> Result<usize, EngineError> {
    if !touched(tx)? {
        return Ok(0);
    }
    let fence = match crate::worktrees::log::store_fence(tx) {
        Ok(fence) => fence,
        Err(stale) if stale.0 == RUNTIME_STALE => return Ok(0),
        Err(refused) => return Err(refused),
    };
    if let Fence::Engine(host) = &fence
        && !crate::state_log::fence::hosted(host)
    {
        return Ok(0);
    }
    close_at(tx, revision, fence)
}

/// Closes the marks of a writer on its own transaction: `own` moves the `state` revision
/// once behind the store's engine host and writes its `events` row. Input: the transaction.
/// Output: how many events were logged, or `owner_runtime_stale` before an owner runtime.
fn close_own(tx: &Transaction<'_>) -> Result<usize, EngineError> {
    if !touched(tx)? {
        return Ok(0);
    }
    let fence = crate::worktrees::log::store_fence(tx)?;
    let mut log = Appender::own(tx, fence)?;
    close(&mut log)
}

/// Closes the marks at a moved revision when any are left. Inputs: the transaction, the
/// revision and the fence. Output: how many events were logged, none when the revision is
/// logged already, or the first refusal.
fn close_at(tx: &Transaction<'_>, revision: u64, fence: Fence) -> Result<usize, EngineError> {
    if !touched(tx)? {
        return Ok(0);
    }
    let mut log = match Appender::owner(tx, revision, fence) {
        Ok(log) => log,
        Err(refused) if refused.0 == NOT_NEWER => return Ok(0),
        Err(refused) => return Err(refused),
    };
    close(&mut log)
}

/// Logs every marked key as it stands, in the live writer's transaction (L8 I1): a present
/// row whole, its text kept under its holder; absent entries and agent texts as dropped
/// events per project, their holders released. A session row is never deleted but by a demo
/// clear, which deletes its marks too. Input: the transaction's only log writer at its
/// revision. Output: how many events were logged, or the first refusal.
pub(crate) fn close(log: &mut Appender<'_, '_>) -> Result<usize, EngineError> {
    let marks = log.project(|tx| marks(tx))?;
    if marks.is_empty() {
        return Ok(0);
    }
    let events = log.project(|tx| settle(tx, &marks))?;
    let at_ms = Clock::System.now_ms();
    for event in &events {
        append(log, at_ms, event)?;
    }
    log.project(|tx| unmark(tx, &marks))?;
    assert!(marks.len() <= MARKS_MAX);
    Ok(events.len())
}

/// Appends one sessions event; each kind has its own draft naming its kind constant.
/// Inputs: the log writer, the clock and the event. Output: its `seq`, or the refusal.
fn append(log: &mut Appender<'_, '_>, at_ms: u64, event: &Event) -> Result<u64, EngineError> {
    let body = event.body()?;
    let aggregate = Aggregate::Sessions;
    let seq = match event {
        Event::Session(_) => log.append(&Draft {
            aggregate,
            kind: KIND_SESSION,
            schema: SCHEMA,
            at_ms,
            body: &body,
        }),
        Event::Entry(_) => log.append(&Draft {
            aggregate,
            kind: KIND_ENTRY,
            schema: SCHEMA,
            at_ms,
            body: &body,
        }),
        Event::EntriesDropped(_) => log.append(&Draft {
            aggregate,
            kind: KIND_ENTRIES_DROPPED,
            schema: SCHEMA,
            at_ms,
            body: &body,
        }),
        Event::Text(_) => log.append(&Draft {
            aggregate,
            kind: KIND_TEXT,
            schema: SCHEMA,
            at_ms,
            body: &body,
        }),
        Event::TextsDropped(_) => log.append(&Draft {
            aggregate,
            kind: KIND_TEXTS_DROPPED,
            schema: SCHEMA,
            at_ms,
            body: &body,
        }),
        Event::Turn(_) => log.append(&Draft {
            aggregate,
            kind: KIND_TURN,
            schema: SCHEMA,
            at_ms,
            body: &body,
        }),
        Event::Preparation(_) => log.append(&Draft {
            aggregate,
            kind: KIND_PREPARATION,
            schema: SCHEMA,
            at_ms,
            body: &body,
        }),
        Event::Resolve(_) => log.append(&Draft {
            aggregate,
            kind: KIND_RESOLVE,
            schema: SCHEMA,
            at_ms,
            body: &body,
        }),
        Event::ResolvesDropped(_) => log.append(&Draft {
            aggregate,
            kind: KIND_RESOLVES_DROPPED,
            schema: SCHEMA,
            at_ms,
            body: &body,
        }),
        Event::Profile(_) => log.append(&Draft {
            aggregate,
            kind: KIND_PROFILE,
            schema: SCHEMA,
            at_ms,
            body: &body,
        }),
        Event::ProfilesDropped(_) => log.append(&Draft {
            aggregate,
            kind: KIND_PROFILES_DROPPED,
            schema: SCHEMA,
            at_ms,
            body: &body,
        }),
    }?;
    assert!(seq > 0, "a logged event has a seq");
    assert!(
        body.len() <= BODY_BYTES_MAX,
        "the log bounds what it appended"
    );
    Ok(seq)
}

/// Whether any key is marked. Input: the connection. Output: the answer, or the store's
/// refusal.
fn touched(db: &Connection) -> Result<bool, EngineError> {
    if !exists(db, "sessions_touched")? {
        return Ok(false);
    }
    db.query_row("SELECT EXISTS(SELECT 1 FROM sessions_touched)", [], |row| {
        row.get(0)
    })
    .map_err(error)
}

/// The marked keys in [`TABLES`] order, each table's in key order, at most [`MARKS_MAX`].
/// Input: the connection. Output: the marks, or `sessions_log_mark_invalid`.
fn marks(db: &Connection) -> Result<Vec<Mark>, EngineError> {
    if !exists(db, "sessions_touched")? {
        return Ok(Vec::new());
    }
    let mut statement = db
        .prepare(
            "SELECT tbl, key, project FROM sessions_touched ORDER BY CASE tbl WHEN 'master_sessions' THEN 0 WHEN 'conversation_entries' THEN 1 WHEN 'agent_texts' THEN 2 WHEN 'master_turns' THEN 3 WHEN 'master_preparations' THEN 4 WHEN 'master_waiting_resolves' THEN 5 ELSE 6 END, length(key), key LIMIT ?1",
        )
        .map_err(error)?;
    let stored = statement
        .query_map([signed(MARKS_MAX as u64)?], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .map_err(error)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(error)?;
    let marks = stored
        .into_iter()
        .map(|(table, key, project)| {
            let table = Table::ALL
                .into_iter()
                .find(|known| known.name() == table)
                .ok_or_else(|| error("sessions_log_mark_invalid"))?;
            Ok(Mark {
                table,
                key,
                project,
            })
        })
        .collect::<Result<Vec<_>, EngineError>>()?;
    assert!(marks.len() <= MARKS_MAX);
    Ok(marks)
}

/// Reads each marked key back into its event: a present row whole, its text kept again
/// under its holder; an absent entry, agent text, waiting Resolve or profile receipt
/// released and gathered per project into dropped events; an absent turn or preparation
/// only released. Inputs: the transaction and the marks. Output: the events in mark order,
/// the dropped ones after them and the saved receipts last, or the first refusal.
fn settle(db: &Connection, marks: &[Mark]) -> Result<Vec<Event>, EngineError> {
    let mut events = Vec::new();
    let mut entries_gone: BTreeMap<&str, Vec<u64>> = BTreeMap::new();
    let mut texts_gone: BTreeMap<&str, Vec<u64>> = BTreeMap::new();
    let mut rest = turns::Rest::default();
    for mark in marks {
        match mark.table {
            Table::Turns | Table::Preparations | Table::Resolves | Table::Profiles => {
                events.extend(rest.settle(db, mark.table.name(), &mark.key, &mark.project)?);
            }
            Table::Sessions => events.extend(session_row(db, &mark.key)?.map(Event::Session)),
            Table::Entries => {
                let cursor = number(&mark.key)?;
                let project = project_id(&mark.project)?;
                match entry_row(db, &project, cursor)? {
                    Some(entry) => events.push(Event::Entry(entry)),
                    None => {
                        crate::owner_texts::release(db, &entry_holder(&project, cursor))?;
                        entries_gone
                            .entry(mark.project.as_str())
                            .or_default()
                            .push(cursor);
                    }
                }
            }
            Table::Texts => {
                let agent = number(&mark.key)?;
                match text_row(db, agent)? {
                    Some(text) => events.push(Event::Text(text)),
                    None => {
                        crate::owner_texts::release(db, &text_holder(agent))?;
                        texts_gone
                            .entry(mark.project.as_str())
                            .or_default()
                            .push(agent);
                    }
                }
            }
        }
    }
    for (project, cursors) in entries_gone {
        for part in cursors.chunks(DROPPED_MAX) {
            events.push(Event::EntriesDropped(EntriesDropped {
                project: project_id(project)?,
                cursors: part.to_vec(),
            }));
        }
    }
    for (project, agents) in texts_gone {
        for part in agents.chunks(DROPPED_MAX) {
            events.push(Event::TextsDropped(TextsDropped {
                project: project_id(project)?,
                agents: part.to_vec(),
            }));
        }
    }
    events.extend(rest.finish()?);
    assert!(events.len() <= marks.len());
    Ok(events)
}

/// Deletes the marks a close logged. Inputs: the transaction and the marks. Output: Ok, or
/// the store's refusal.
fn unmark(db: &Connection, marks: &[Mark]) -> Result<(), EngineError> {
    for mark in marks {
        let deleted = db
            .execute(
                "DELETE FROM sessions_touched WHERE tbl=?1 AND key=?2",
                params![mark.table.name(), mark.key],
            )
            .map_err(error)?;
        assert_eq!(deleted, 1, "a mark read is deleted once");
    }
    Ok(())
}

/// A master session row as its event carries it. Inputs: the connection and the session id.
/// Output: the event, none for no row, or `sessions_log_row_invalid` for a row whose body
/// is not a stored session.
fn session_row(db: &Connection, session: &str) -> Result<Option<SessionSaved>, EngineError> {
    let invalid = || error("sessions_log_row_invalid");
    let row: Option<(String, i64, String)> = db
        .query_row(
            "SELECT project, created, body FROM master_sessions WHERE id=?1",
            [session],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()
        .map_err(error)?;
    let Some((project, created, body)) = row else {
        return Ok(None);
    };
    let view: MasterSessionView = serde_json::from_str(&body).map_err(|_| invalid())?;
    let saved = SessionSaved {
        project: project_id(&project)?,
        session: SessionId::new(session).map_err(|_| invalid())?,
        created: u64::try_from(created).map_err(|_| invalid())?,
        view,
    };
    if saved.view.session_id != saved.session || saved.view.project_id != saved.project {
        return Err(invalid());
    }
    Ok(Some(saved))
}

/// A conversation entry as its event carries it, its text kept under its holder. Inputs:
/// the transaction, the project and the cursor. Output: the event, none for no row, or
/// `sessions_log_row_invalid`.
fn entry_row(
    db: &Connection,
    project: &ProjectId,
    cursor: u64,
) -> Result<Option<EntryAppended>, EngineError> {
    let invalid = || error("sessions_log_row_invalid");
    let row: Option<(String, i64, String)> = db
        .query_row(
            "SELECT kind, brief_version, text FROM conversation_entries WHERE project=?1 AND event_cursor=?2",
            params![project.as_str(), signed(cursor)?],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()
        .map_err(error)?;
    let Some((word, version, stored)) = row else {
        return Ok(None);
    };
    let kind = EntryKind::parse(&word).ok_or_else(invalid)?;
    let holder = entry_holder(project, cursor);
    let text = crate::owner_texts::keep(db, &holder, Some(project), stored.as_bytes())?;
    Ok(Some(EntryAppended {
        project: project.clone(),
        cursor,
        kind,
        version: u64::try_from(version).map_err(|_| invalid())?,
        text,
    }))
}

/// An agent's own instructions as their event carries them, the text kept under its
/// holder. Inputs: the transaction and the agent. Output: the event, none for no row or no
/// table, or `sessions_log_row_invalid`.
fn text_row(db: &Connection, agent: u64) -> Result<Option<TextSaved>, EngineError> {
    if !exists(db, TABLES[2])? {
        return Ok(None);
    }
    let row: Option<(String, String)> = db
        .query_row(
            "SELECT project, text FROM agent_texts WHERE agent=?1",
            [signed(agent)?],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(error)?;
    let Some((project, stored)) = row else {
        return Ok(None);
    };
    let project = project_id(&project)?;
    let text =
        crate::owner_texts::keep(db, &text_holder(agent), Some(&project), stored.as_bytes())?;
    Ok(Some(TextSaved {
        project,
        agent,
        text,
    }))
}

/// The holder key a conversation entry's text is kept under. Inputs: the project and the
/// cursor. Output: `conversation_entries/<project>/<cursor>/text`.
fn entry_holder(project: &ProjectId, cursor: u64) -> String {
    format!("conversation_entries/{}/{cursor}/text", project.as_str())
}

/// The holder key an agent's own instructions are kept under. Input: the agent. Output:
/// `agent_texts/<agent>/text`.
fn text_holder(agent: u64) -> String {
    format!("agent_texts/{agent}/text")
}

/// Applies one sessions event to its table, for replay: the bare upsert or delete of its
/// rows, no check and no other effect. An entry or agent text whose text is no longer kept
/// belongs to a row a later event dropped, so it is skipped (`owner_texts::replayed` names
/// it). Inputs: the connection and the event. Output: Ok, or the store's refusal.
pub fn apply(db: &Connection, event: &Event) -> Result<(), EngineError> {
    let mut missing = Vec::new();
    match event {
        Event::Session(row) => {
            let body = serde_json::to_string(&row.view).map_err(error)?;
            let phase = match serde_json::to_value(row.view.phase).map_err(error)? {
                Value::String(word) => word,
                _ => return Err(error("sessions_log_row_invalid")),
            };
            db.execute(
                "INSERT INTO master_sessions(id,project,created,version,phase,body) VALUES (?1,?2,?3,?4,?5,?6) ON CONFLICT(id) DO UPDATE SET project=excluded.project, created=excluded.created, version=excluded.version, phase=excluded.phase, body=excluded.body",
                params![
                    row.session.as_str(),
                    row.project.as_str(),
                    signed(row.created)?,
                    signed(row.view.version.value())?,
                    phase,
                    body
                ],
            )
            .map_err(error)?;
        }
        Event::Entry(row) => {
            let text = crate::owner_texts::replayed(db, &row.text, &mut missing)?;
            if missing.is_empty() {
                let text =
                    String::from_utf8(text).map_err(|_| error("sessions_log_row_invalid"))?;
                db.execute(
                    "INSERT OR REPLACE INTO conversation_entries(project,event_cursor,kind,brief_version,text) VALUES (?1,?2,?3,?4,?5)",
                    params![row.project.as_str(), signed(row.cursor)?, row.kind.as_str(), signed(row.version)?, text],
                )
                .map_err(error)?;
            }
        }
        Event::EntriesDropped(rows) => {
            for cursor in &rows.cursors {
                db.execute(
                    "DELETE FROM conversation_entries WHERE project=?1 AND event_cursor=?2",
                    params![rows.project.as_str(), signed(*cursor)?],
                )
                .map_err(error)?;
            }
        }
        Event::Text(row) => {
            let text = crate::owner_texts::replayed(db, &row.text, &mut missing)?;
            if missing.is_empty() {
                let text =
                    String::from_utf8(text).map_err(|_| error("sessions_log_row_invalid"))?;
                db.execute(
                    "INSERT INTO agent_texts(agent,project,text) VALUES (?1,?2,?3) ON CONFLICT(agent) DO UPDATE SET project=excluded.project, text=excluded.text",
                    params![signed(row.agent)?, row.project.as_str(), text],
                )
                .map_err(error)?;
            }
        }
        Event::TextsDropped(rows) => {
            for agent in &rows.agents {
                db.execute(
                    "DELETE FROM agent_texts WHERE agent=?1 AND project=?2",
                    params![signed(*agent)?, rows.project.as_str()],
                )
                .map_err(error)?;
            }
        }
        Event::Turn(row) => turns::replace(
            db,
            TABLES[3],
            json!([
                row.session,
                row.ordinal,
                row.id,
                row.input,
                row.phase,
                row.body
            ]),
        )?,
        Event::Preparation(row) => {
            turns::replace(db, TABLES[4], json!([row.session, row.turn, row.body]))?;
        }
        Event::Resolve(row) => turns::replace(
            db,
            TABLES[5],
            json!([row.session, row.project, row.operation, row.input, row.body]),
        )?,
        Event::ResolvesDropped(rows) => {
            for session in &rows.sessions {
                db.execute(
                    "DELETE FROM master_waiting_resolves WHERE session=?1 AND project=?2",
                    params![session.as_str(), rows.project.as_str()],
                )
                .map_err(error)?;
            }
        }
        Event::Profile(row) => turns::replace(
            db,
            TABLES[6],
            json!([row.id, row.session, row.profile, row.body]),
        )?,
        Event::ProfilesDropped(rows) => {
            for id in &rows.ids {
                db.execute(
                    "DELETE FROM configured_master_profiles WHERE id=?1",
                    [id.as_str()],
                )
                .map_err(error)?;
            }
        }
    }
    assert!(missing.len() <= 1, "one event names one text");
    Ok(())
}

/// Reads one logged sessions event: one of [`KINDS`] at schema 1, its body exactly that
/// kind's fields, an entry's kind matching its version, each id above 0 and each dropped
/// list 1 to [`DROPPED_MAX`] long. Input: the logged event. Output: the event, or
/// `sessions_log_event_invalid`.
pub fn decode(logged: &Logged) -> Result<Event, EngineError> {
    let invalid = || error("sessions_log_event_invalid");
    if logged.aggregate != Aggregate::Sessions || logged.schema != SCHEMA {
        return Err(invalid());
    }
    let body = logged.body.as_slice();
    let event = match logged.kind.as_str() {
        KIND_SESSION => serde_json::from_slice(body).map(Event::Session),
        KIND_ENTRY => serde_json::from_slice(body).map(Event::Entry),
        KIND_ENTRIES_DROPPED => serde_json::from_slice(body).map(Event::EntriesDropped),
        KIND_TEXT => serde_json::from_slice(body).map(Event::Text),
        KIND_TEXTS_DROPPED => serde_json::from_slice(body).map(Event::TextsDropped),
        KIND_TURN => serde_json::from_slice(body).map(Event::Turn),
        KIND_PREPARATION => serde_json::from_slice(body).map(Event::Preparation),
        KIND_RESOLVE => serde_json::from_slice(body).map(Event::Resolve),
        KIND_RESOLVES_DROPPED => serde_json::from_slice(body).map(Event::ResolvesDropped),
        KIND_PROFILE => serde_json::from_slice(body).map(Event::Profile),
        KIND_PROFILES_DROPPED => serde_json::from_slice(body).map(Event::ProfilesDropped),
        _ => return Err(invalid()),
    }
    .map_err(|_| invalid())?;
    if !valid(&event) {
        return Err(invalid());
    }
    assert_eq!(event.kind(), logged.kind);
    Ok(event)
}

/// Whether an event's fields hold what its rows can: an entry's kind matching its version,
/// a turn's ordinal 1 to [`turns::ORDINAL_MAX`], each id and input above 0 and each
/// dropped list 1 to its bound long. Input: the event. Output: the answer.
fn valid(event: &Event) -> bool {
    let listed =
        |keys: &[u64]| (1..=DROPPED_MAX).contains(&keys.len()) && keys.iter().all(|key| *key > 0);
    let named = |count: usize| (1..=turns::NAMES_MAX).contains(&count);
    match event {
        Event::Session(row) => {
            row.view.session_id == row.session && row.view.project_id == row.project
        }
        Event::Entry(row) => {
            row.cursor > 0
                && match row.kind {
                    EntryKind::Message => row.version == 0,
                    EntryKind::Brief => row.version > 0,
                }
        }
        Event::EntriesDropped(rows) => listed(&rows.cursors),
        Event::Text(row) => row.agent > 0,
        Event::TextsDropped(rows) => listed(&rows.agents),
        Event::Turn(row) => (1..=turns::ORDINAL_MAX).contains(&row.ordinal) && row.input > 0,
        Event::Preparation(row) => row.input > 0,
        Event::Resolve(row) => row.input > 0,
        Event::ResolvesDropped(rows) => named(rows.sessions.len()),
        Event::Profile(_) => true,
        Event::ProfilesDropped(rows) => named(rows.ids.len()),
    }
}

/// Rebuilds the seven tables from the log: clears them, then replays every sessions event in
/// order; L12's delete, which the team projection also reads, deletes the session's turns
/// and preparations as the live Delete does. A snapshot run holds every row of all seven,
/// so its first chunk clears them again; a chunk opens a run unless it follows another
/// chunk at the previous `seq`. Input: the transaction, on a store whose open made the
/// tables. Output: the last `seq` read.
pub fn rebuild(tx: &Transaction<'_>) -> Result<u64, EngineError> {
    crate::storage::agent_text::ensure(tx)?;
    clear(tx)?;
    let mut chunk_seq = 0;
    let last = replay::replay(tx, Aggregate::Sessions, 0, |logged| {
        if logged.kind == SESSION_DELETED {
            let deleted: SessionDeleted = serde_json::from_slice(&logged.body)
                .map_err(|_| error("sessions_log_event_invalid"))?;
            return turns::delete_session_turns(tx, deleted.session.as_str());
        }
        if logged.kind != SNAPSHOT_CHUNK {
            return apply(tx, &decode(logged)?);
        }
        if chunk_seq == 0 || logged.seq != chunk_seq + 1 {
            clear(tx)?;
        }
        chunk_seq = logged.seq;
        restore(tx, &replay::chunk(logged)?)
    })?;
    assert!(
        last >= chunk_seq,
        "replay reads past every chunk it applied"
    );
    Ok(last)
}

/// Appends a snapshot run of the seven tables as they stand, at least one chunk each, a
/// missing table as an empty one; texts and bodies go as digests, each kept again under its
/// holder in the run's live transaction. Inputs: the log writer and its clock. Output: the `seq` of
/// the run's closing event, or `sessions_log_snapshot_rows` or the log's refusal.
pub fn snapshot(log: &mut Appender<'_, '_>, at_ms: u64) -> Result<u64, EngineError> {
    let mut chunks = Vec::new();
    for table in TABLES {
        let rows = log.project(|tx| table_rows(tx, table))?;
        chunks.extend(pack(table, rows)?);
    }
    assert!(chunks.len() >= TABLES.len(), "one chunk or more per table");
    assert_eq!(chunks[0].table, TABLES[0]);
    log.snapshot(Aggregate::Sessions, at_ms, chunks)
}

/// The first snapshot run of the seven tables, for the schema step that puts sessions on
/// the log (step 16): rows written before the log reach replay through it, through the
/// open's proof. A store without the `state` table, or with no row in any of the tables
/// that exist, gets no run and moves no revision. Inputs: the step's transaction and the
/// open's proof. Output: whether a run was appended, or the first refusal.
#[cfg(any(unix, test))]
pub(crate) fn first_run(tx: &Transaction<'_>, opening: Opening) -> Result<bool, EngineError> {
    if !exists(tx, "state")? || stored_rows(tx, &TABLES)? == 0 {
        return Ok(false);
    }
    run_once(tx, opening)
}

/// The schema step that puts master turns, preparations, waiting Resolves and profile
/// receipts on the log (v2.5 L13c): gives `sessions_touched` the seven-table check when an
/// older shape exists, then one full sessions snapshot run when the four tables hold rows
/// that no event or chunk of theirs reached. Inputs: the step's transaction and the open's
/// proof. Output: whether a run was appended, or the first refusal.
#[cfg(any(unix, test))]
pub(crate) fn first_run_turns(tx: &Transaction<'_>, opening: Opening) -> Result<bool, EngineError> {
    turns::widen_touched(tx)?;
    if !exists(tx, "state")? || stored_rows(tx, &TABLES[3..])? == 0 || turns::on_the_log(tx)? {
        return Ok(false);
    }
    run_once(tx, opening)
}

/// How many rows the tables that exist of a list hold. Inputs: the transaction and the
/// tables. Output: the count, or the store's refusal.
#[cfg(any(unix, test))]
fn stored_rows(tx: &Transaction<'_>, tables: &[&str]) -> Result<i64, EngineError> {
    let mut rows = 0;
    for table in tables {
        if exists(tx, table)? {
            let count: i64 = tx
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .map_err(error)?;
            rows += count;
        }
    }
    assert!(rows >= 0);
    Ok(rows)
}

/// Appends one snapshot run of the seven tables through the open's proof. Inputs: the
/// step's transaction and the proof. Output: true once the run closed, or the refusal.
#[cfg(any(unix, test))]
fn run_once(tx: &Transaction<'_>, opening: Opening) -> Result<bool, EngineError> {
    let at_ms = opening.at_ms();
    let mut log = Appender::opening(tx, opening)?;
    let done = snapshot(&mut log, at_ms)?;
    assert!(done > 0, "a run closes at a seq");
    Ok(true)
}

/// Tombstones, for a demo clear of an earlier id, every logged sessions event naming the
/// project at its top level, so its ids and digests leave the log's events; snapshot runs
/// keep their rows until compaction (L1b). An owner removal redacts none: it keeps the
/// project's sessions and conversation live. Inputs: the transaction, the project and the
/// fence. Output: how many events were tombstoned, or the redaction's refusal.
pub(crate) fn forget_project(
    tx: &Transaction<'_>,
    project: &str,
    fence: &Fence,
) -> Result<usize, EngineError> {
    let redacted = replay::redact_matching(
        tx,
        &[Aggregate::Sessions],
        None,
        "project",
        project,
        0,
        fence,
    )?;
    assert!(!tx.is_autocommit(), "the redaction is part of the clear");
    Ok(redacted)
}

/// Every row of one of the seven tables in key order as a JSON array of its columns, a
/// text or body column as its kept digest; none for a missing table. Inputs: the transaction and
/// the table. Output: the rows, or `sessions_log_snapshot_rows` past [`SNAPSHOT_ROWS_MAX`].
fn table_rows(db: &Connection, table: &str) -> Result<Vec<Value>, EngineError> {
    if !exists(db, table)? {
        return Ok(Vec::new());
    }
    let limit = signed(SNAPSHOT_ROWS_MAX as u64 + 1)?;
    let rows = match table {
        "master_sessions" => db
            .prepare("SELECT id,project,created,version,phase,body FROM master_sessions ORDER BY id LIMIT ?1")
            .and_then(|mut statement| {
                statement
                    .query_map([limit], |row| {
                        Ok(Value::Array(vec![
                            Value::String(row.get(0)?),
                            Value::String(row.get(1)?),
                            Value::from(row.get::<_, i64>(2)?),
                            Value::from(row.get::<_, i64>(3)?),
                            Value::String(row.get(4)?),
                            Value::String(row.get(5)?),
                        ]))
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()
            })
            .map_err(error)?,
        "conversation_entries" => {
            let stored: Vec<(String, i64, String, i64, String)> = db
                .prepare("SELECT project,event_cursor,kind,brief_version,text FROM conversation_entries ORDER BY project,event_cursor LIMIT ?1")
                .and_then(|mut statement| {
                    statement
                        .query_map([limit], |row| {
                            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?))
                        })?
                        .collect::<rusqlite::Result<Vec<_>>>()
                })
                .map_err(error)?;
            let mut rows = Vec::with_capacity(stored.len());
            for (project, cursor, kind, version, text) in stored {
                let id = project_id(&project)?;
                let holder = entry_holder(&id, number(&cursor.to_string())?);
                let digest = crate::owner_texts::keep(db, &holder, Some(&id), text.as_bytes())?;
                rows.push(Value::Array(vec![
                    Value::String(project),
                    Value::from(cursor),
                    Value::String(kind),
                    Value::from(version),
                    Value::String(digest.as_str().to_owned()),
                ]));
            }
            rows
        }
        table if TABLES[3..].contains(&table) => turns::table_rows(db, table)?,
        _ => {
            let stored: Vec<(i64, String, String)> = db
                .prepare("SELECT agent,project,text FROM agent_texts ORDER BY agent LIMIT ?1")
                .and_then(|mut statement| {
                    statement
                        .query_map([limit], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
                        .collect::<rusqlite::Result<Vec<_>>>()
                })
                .map_err(error)?;
            let mut rows = Vec::with_capacity(stored.len());
            for (agent, project, text) in stored {
                let id = project_id(&project)?;
                let holder = text_holder(number(&agent.to_string())?);
                let digest = crate::owner_texts::keep(db, &holder, Some(&id), text.as_bytes())?;
                rows.push(Value::Array(vec![
                    Value::from(agent),
                    Value::String(project),
                    Value::String(digest.as_str().to_owned()),
                ]));
            }
            rows
        }
    };
    if rows.len() > SNAPSHOT_ROWS_MAX {
        return Err(error("sessions_log_snapshot_rows"));
    }
    assert!(TABLES.contains(&table));
    Ok(rows)
}

/// Splits one table's rows, in order, into chunks whose rows fit one event body with room
/// for the chunk's envelope; a table with no rows is one empty chunk. Inputs: the table and
/// its rows. Output: the chunks, or `sessions_log_row_too_large`.
fn pack(table: &str, rows: Vec<Value>) -> Result<Vec<Chunk>, EngineError> {
    let budget = BODY_BYTES_MAX - CHUNK_ENVELOPE_BYTES;
    let total = rows.len();
    let mut chunks = vec![Chunk {
        table: table.into(),
        rows: Vec::new(),
    }];
    let mut used = 0;
    for row in rows {
        let bytes = serde_json::to_vec(&row).map_err(error)?.len() + 1;
        if bytes > budget {
            return Err(error("sessions_log_row_too_large"));
        }
        if used + bytes > budget {
            chunks.push(Chunk {
                table: table.into(),
                rows: Vec::new(),
            });
            used = 0;
        }
        used += bytes;
        if let Some(chunk) = chunks.last_mut() {
            chunk.rows.push(row);
        }
    }
    assert_eq!(
        chunks.iter().map(|chunk| chunk.rows.len()).sum::<usize>(),
        total
    );
    assert!(chunks.iter().all(|chunk| chunk.table == table));
    Ok(chunks)
}

/// Inserts one snapshot chunk's rows into its table, a text or body from the digest it
/// carries; a text no longer kept is skipped as replay skips it. Inputs: the
/// connection and the chunk. Output: Ok, or `sessions_log_chunk_invalid` for an unknown
/// table, a row that is not that table's columns, or a row the table refuses.
fn restore(db: &Connection, chunk: &Chunk) -> Result<(), EngineError> {
    let invalid = || error("sessions_log_chunk_invalid");
    if !TABLES.contains(&chunk.table.as_str()) || chunk.rows.len() > SNAPSHOT_ROWS_MAX {
        return Err(invalid());
    }
    for row in &chunk.rows {
        let values = row.as_array().map(Vec::as_slice).ok_or_else(invalid)?;
        let mut missing = Vec::new();
        let text = |digest: &str, missing: &mut Vec<TextDigest>| -> Result<String, EngineError> {
            let digest = TextDigest::new(digest.to_owned()).map_err(|_| invalid())?;
            let bytes = crate::owner_texts::replayed(db, &digest, missing)?;
            String::from_utf8(bytes).map_err(|_| invalid())
        };
        let written = match (chunk.table.as_str(), values) {
            (table, _) if TABLES[3..].contains(&table) => {
                turns::write(db, table, values, "INSERT")?;
                continue;
            }
            (
                "master_sessions",
                [
                    Value::String(id),
                    Value::String(project),
                    Value::Number(created),
                    Value::Number(version),
                    Value::String(phase),
                    Value::String(body),
                ],
            ) => db.execute(
                "INSERT INTO master_sessions(id,project,created,version,phase,body) VALUES (?1,?2,?3,?4,?5,?6)",
                params![id, project, created.as_i64().ok_or_else(invalid)?, version.as_i64().ok_or_else(invalid)?, phase, body],
            ),
            (
                "conversation_entries",
                [
                    Value::String(project),
                    Value::Number(cursor),
                    Value::String(kind),
                    Value::Number(version),
                    Value::String(digest),
                ],
            ) => {
                let text = text(digest, &mut missing)?;
                if !missing.is_empty() {
                    continue;
                }
                db.execute(
                    "INSERT INTO conversation_entries(project,event_cursor,kind,brief_version,text) VALUES (?1,?2,?3,?4,?5)",
                    params![project, cursor.as_i64().ok_or_else(invalid)?, kind, version.as_i64().ok_or_else(invalid)?, text],
                )
            }
            ("agent_texts", [Value::Number(agent), Value::String(project), Value::String(digest)]) => {
                let text = text(digest, &mut missing)?;
                if !missing.is_empty() {
                    continue;
                }
                db.execute(
                    "INSERT INTO agent_texts(agent,project,text) VALUES (?1,?2,?3)",
                    params![agent.as_i64().ok_or_else(invalid)?, project, text],
                )
            }
            _ => return Err(invalid()),
        };
        written.map_err(|_| invalid())?;
    }
    Ok(())
}

/// Deletes every row of the seven tables, the last first, so preparations go before the
/// turns they name. Input: the connection. Output: Ok or the store's refusal.
fn clear(db: &Connection) -> Result<(), EngineError> {
    for table in TABLES.into_iter().rev() {
        db.execute(&format!("DELETE FROM {table}"), [])
            .map_err(error)?;
    }
    Ok(())
}

/// Whether the store holds a table. Inputs: the connection and the name. Output: the
/// answer, or the store's refusal.
fn exists(db: &Connection, table: &str) -> Result<bool, EngineError> {
    db.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name=?1)",
        [table],
        |row| row.get(0),
    )
    .map_err(error)
}

/// A marked key as the positive number it stores. Input: the key. Output: the number, or
/// `sessions_log_mark_invalid`.
fn number(key: &str) -> Result<u64, EngineError> {
    key.parse::<u64>()
        .ok()
        .filter(|number| *number > 0 && number.to_string() == key)
        .ok_or_else(|| error("sessions_log_mark_invalid"))
}

/// A stored project id. Input: the text. Output: the id, or `sessions_log_mark_invalid`.
fn project_id(project: &str) -> Result<ProjectId, EngineError> {
    ProjectId::new(project).map_err(|_| error("sessions_log_mark_invalid"))
}

/// A count or id as SQLite stores it. Input: the number. Output: the signed number, or
/// `sessions_log_integer_bounds` past `i64::MAX`.
fn signed(number: u64) -> Result<i64, EngineError> {
    i64::try_from(number).map_err(|_| error("sessions_log_integer_bounds"))
}

/// An engine error from any failure. Input: the failure. Output: the error.
fn error(value: impl std::fmt::Display) -> EngineError {
    EngineError(value.to_string())
}
