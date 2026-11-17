use crate::engine::EngineError;
use crate::engine::owner::Admission;
use crate::master_session::MasterSessionView;
use crate::owner_texts::TextDigest;
use crate::protocol::{ProjectId, SessionId};
use crate::state_log::Appender;
use crate::state_log::fence::Fence;
use crate::state_log::replay::Logged;
#[cfg(any(unix, test))]
use crate::storage::opening::Opening;
use rusqlite::{Connection, Transaction};
use serde::{Deserialize, Serialize};

#[cfg(test)]
mod tests;

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
/// Every kind this module writes; L12's delete event is the delete's own.
pub const KINDS: [&str; 5] = [
    KIND_SESSION,
    KIND_ENTRY,
    KIND_ENTRIES_DROPPED,
    KIND_TEXT,
    KIND_TEXTS_DROPPED,
];
/// The three tables on the log, in snapshot order.
const TABLES: [&str; 3] = ["master_sessions", "conversation_entries", "agent_texts"];
/// The most keys one dropped event lists.
const DROPPED_MAX: usize = 2_048;

/// One master session row: its two ids, its `created` revision and the stored session.
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

/// One conversation entry with the digest its text is kept under.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EntryAppended {
    pub(crate) project: ProjectId,
    pub(crate) cursor: u64,
    pub(crate) kind: EntryKind,
    pub(crate) version: u64,
    pub(crate) text: TextDigest,
}

/// Conversation entries of one project gone from their table, by cursor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EntriesDropped {
    pub(crate) project: ProjectId,
    pub(crate) cursors: Vec<u64>,
}

/// One agent's own instructions with the digest their text is kept under.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TextSaved {
    pub(crate) project: ProjectId,
    pub(crate) agent: u64,
    pub(crate) text: TextDigest,
}

/// Agents' own instructions of one project gone from their table, by agent id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TextsDropped {
    pub(crate) project: ProjectId,
    pub(crate) agents: Vec<u64>,
}

/// One sessions event, one kind per change a close finds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Session(SessionSaved),
    Entry(EntryAppended),
    EntriesDropped(EntriesDropped),
    Text(TextSaved),
    TextsDropped(TextsDropped),
}

/// One table a write marks a key of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Table {
    Sessions,
    Entries,
    Texts,
}

/// Marks a master session row for the transaction's close (not written yet). Inputs: the
/// transaction, the project and the session. Output: Ok.
pub(crate) fn mark_session(_: &Connection, _: &str, _: &str) -> Result<(), EngineError> {
    Ok(())
}

/// Marks a conversation entry for the transaction's close (not written yet). Inputs: the
/// transaction, the project and the cursor. Output: Ok.
pub(crate) fn mark_entry(_: &Connection, _: &str, _: u64) -> Result<(), EngineError> {
    Ok(())
}

/// Marks one key for the transaction's close (not written yet). Inputs: the transaction,
/// the table, the key and the project. Output: Ok.
fn mark(_: &Connection, _: Table, _: &str, _: &str) -> Result<(), EngineError> {
    Ok(())
}

/// Closes the marks of an owner request (not written yet). Output: the refusal.
pub(crate) fn close_owner(
    _: &Transaction<'_>,
    _: u64,
    _: &Admission,
) -> Result<usize, EngineError> {
    Err(unwritten())
}

/// Closes the marks of an engine writer (not written yet). Output: the refusal.
pub(crate) fn close_engine(_: &Transaction<'_>, _: u64) -> Result<usize, EngineError> {
    Err(unwritten())
}

/// Closes the marks of a writer on its own transaction (not written yet). Output: the
/// refusal.
fn close_own(_: &Transaction<'_>) -> Result<usize, EngineError> {
    Err(unwritten())
}

/// Logs every marked key as it stands (not written yet). Output: the refusal.
pub(crate) fn close(_: &mut Appender<'_, '_>) -> Result<usize, EngineError> {
    Err(unwritten())
}

/// Reads one logged sessions event (not written yet). Output: the refusal.
pub fn decode(_: &Logged) -> Result<Event, EngineError> {
    Err(unwritten())
}

/// Rebuilds the three tables from the log (not written yet). Output: the refusal.
pub fn rebuild(_: &Transaction<'_>) -> Result<u64, EngineError> {
    Err(unwritten())
}

/// Appends a snapshot run of the three tables (not written yet). Output: the refusal.
pub fn snapshot(_: &mut Appender<'_, '_>, _: u64) -> Result<u64, EngineError> {
    Err(unwritten())
}

/// The first snapshot run for step 15 (not written yet). Output: the refusal.
#[cfg(any(unix, test))]
pub(crate) fn first_run(_: &Transaction<'_>, _: Opening) -> Result<bool, EngineError> {
    Err(unwritten())
}

/// Tombstones a cleared project's sessions events (not written yet). Output: the refusal.
pub(crate) fn forget_project(
    _: &Transaction<'_>,
    _: &str,
    _: &Fence,
) -> Result<usize, EngineError> {
    Err(unwritten())
}

/// The refusal of every function the red rows call before L13 is written. Output: the
/// error.
fn unwritten() -> EngineError {
    assert!(TABLES.len() == 3 && DROPPED_MAX > 0);
    EngineError("sessions_log_unwritten".into())
}
