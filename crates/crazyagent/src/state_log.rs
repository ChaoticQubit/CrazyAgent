use crate::engine::EngineError;
use fence::Fence;
use kernel_storage::StorageError;
use kernel_storage::state_log::{AggregateName, Appender, FenceCheck};
use rusqlite::Transaction;
use serde::{Deserialize, Serialize};

/// The write fence on append, one per writer class, and the engine hosts this process runs
/// (v2.5 L3).
pub mod fence;
/// Replay of the log into projections, the compare of rebuilt tables, redaction in place, the
/// writers' clock and the process exit event (v2.5 L2).
pub mod replay;
#[cfg(test)]
mod tests;

/// The engine state groups the log orders events by (v2.5 plan section 3.1), a closed
/// set: each name is stored in the log's `aggregate` column and in snapshot bodies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Aggregate {
    Worktrees,
    Ide,
    Memory,
    Projects,
    Landing,
    Settings,
    Plugins,
    Sessions,
    Intake,
    Engine,
    Accounts,
    Team,
}

impl Aggregate {
    /// The aggregate's stored name, as its snake_case serde name. Output: the name.
    pub fn name(self) -> &'static str {
        match self {
            Self::Worktrees => "worktrees",
            Self::Ide => "ide",
            Self::Memory => "memory",
            Self::Projects => "projects",
            Self::Landing => "landing",
            Self::Settings => "settings",
            Self::Plugins => "plugins",
            Self::Sessions => "sessions",
            Self::Intake => "intake",
            Self::Engine => "engine",
            Self::Accounts => "accounts",
            Self::Team => "team",
        }
    }
}

impl From<Aggregate> for AggregateName {
    fn from(aggregate: Aggregate) -> Self {
        AggregateName::new(aggregate.name())
    }
}

/// Starts appending in an owner request's transaction behind its fence, which the kernel
/// checks first and again before each projection apply. Inputs: the transaction, the
/// revision the request moved and the fence. Output: the appender, a fence refusal,
/// `state_log_revision_stale` or `state_log_revision_not_newer`.
pub fn owner<'a, 'c>(
    tx: &'a Transaction<'c>,
    revision: u64,
    fence: Fence,
) -> Result<Appender<'a, 'c>, EngineError> {
    Ok(Appender::owner(tx, revision, hook(fence))?)
}

/// Starts appending in a transaction of a writer on its own connection: the kernel moves
/// the `state` revision once after the fence and writes that revision's `events` row with
/// the engine's body, so clients see the change. Inputs: the transaction and the fence.
/// Output: the appender, a fence refusal, `state_log_revision_exhausted` or
/// `state_log_body_bounds`.
pub fn own<'a, 'c>(tx: &'a Transaction<'c>, fence: Fence) -> Result<Appender<'a, 'c>, EngineError> {
    Ok(Appender::own(tx, hook(fence), engine_event)?)
}

/// Starts appending in a transaction of the storage open or one of its schema steps,
/// which write before any engine host of the store starts, so they bring the open's
/// proof in place of a host or owner fence (v2.5 L6). It moves the `state` revision
/// once, as [`own`]. Inputs: the transaction and the proof. Output: the appender,
/// `state_log_opening_host_running` or `state_log_revision_exhausted`.
pub(crate) fn opening<'a, 'c>(
    tx: &'a Transaction<'c>,
    opening: crate::storage::opening::Opening,
) -> Result<Appender<'a, 'c>, EngineError> {
    own(tx, Fence::Opening(opening))
}

/// The kernel hook for a writer's fence: [`fence::check`] on the appending transaction,
/// its refusal text unchanged. Input: the fence. Output: the hook.
fn hook(fence: Fence) -> FenceCheck {
    Box::new(move |tx| fence::check(tx, &fence).map_err(|refused| StorageError(refused.0)))
}

/// The client an engine-own revision's `events` row names. The wire refuses every `owner:`
/// client and an owner's own is `owner:` and 64 hex digits, so no client takes the row for
/// its own request.
const ENGINE_CLIENT: &str = "owner:engine";

/// The body of the `events` row of a revision an engine-own write moved, so a client
/// catching up from `events` moves to it and its next save is not `stale_revision` (v2.5
/// L6, fix11 L7 Q6). The row's topic is `unknown`: any writer may append here (S3a).
/// Input: the revision. Output: the JSON text, or the wire types' refusal.
fn engine_event(revision: u64) -> Result<String, StorageError> {
    use crate::protocol::{ClientId, Cursor, Event, RequestId, Topic};
    assert!(revision > 0, "an own write moves the revision");
    let event = Event {
        cursor: Cursor::new(revision),
        request_id: RequestId::new(format!("engine-{revision}")).map_err(storage_error)?,
        client_id: ClientId::new(ENGINE_CLIENT).map_err(storage_error)?,
        topic: Topic::Unknown,
    };
    assert_eq!(event.cursor.value(), revision);
    serde_json::to_string(&event).map_err(storage_error)
}

/// Builds a kernel error from a wire refusal. Input: the cause. Output: the error.
fn storage_error(value: impl std::fmt::Display) -> StorageError {
    StorageError(value.to_string())
}

fn error(value: impl std::fmt::Display) -> EngineError {
    EngineError(value.to_string())
}
