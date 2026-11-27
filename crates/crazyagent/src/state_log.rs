use crate::engine::EngineError;
use fence::Fence;
use kernel_storage::StorageError;
use kernel_storage::state_log::replay::Logged;
use kernel_storage::state_log::{AggregateName, Appender, Chunk, FenceCheck};
use rusqlite::{Connection, Transaction};
use serde::{Deserialize, Serialize};

/// The write fence on append, one per writer class, and the engine hosts this process runs
/// (v2.5 L3).
pub mod fence;
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

/// A logged name equals an engine aggregate when it is that aggregate's stored name, so a
/// decoder checks a replayed event's aggregate as it did before the move.
impl PartialEq<Aggregate> for AggregateName {
    fn eq(&self, other: &Aggregate) -> bool {
        self.name() == other.name()
    }
}

impl From<Aggregate> for AggregateName {
    fn from(aggregate: Aggregate) -> Self {
        AggregateName::new(aggregate.name())
    }
}

/// After an aggregate's append, runs the kernel compaction under the owner's switch: a build
/// that is not a dev build and the Plugins aggregate pass Off before any read; a stored
/// choice that does not read is Off, with the cause on stderr. Inputs: the appender, the
/// aggregate, the writer's clock and its chunk builder. Output: the tombstoned row count.
pub(crate) fn compact_after(
    appender: &mut Appender<'_, '_>,
    aggregate: Aggregate,
    at_ms: u64,
    chunks: impl FnOnce(&Transaction<'_>) -> Result<Vec<Chunk>, EngineError>,
) -> Result<u64, EngineError> {
    let dev = crate::log_compaction::this_build_is_dev();
    if !dev || aggregate == Aggregate::Plugins {
        return Ok(0);
    }
    let stored = appender.project(|tx| {
        Ok::<_, EngineError>(crate::log_compaction::load(tx).unwrap_or_else(|problem| {
            eprintln!("log_compaction_unread cause={}", problem.0);
            crate::log_compaction::LogCompaction::Off
        }))
    })?;
    let compaction = crate::log_compaction::compaction_for(aggregate, dev, stored);
    kernel_storage::state_log::compact::compact(
        appender,
        compaction,
        aggregate.into(),
        at_ms,
        chunks,
    )
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
    Ok(Appender::owner(tx, revision, hook(fence), known)?)
}

/// Starts appending in an owner request whose signed next revision is not yet in `state`:
/// the kernel checks the fence, moves `state` from the revision before it to it and opens
/// there. Inputs: the transaction, the next revision and the fence. Output: the appender, a
/// fence refusal, `state_log_revision_stale` or `state_log_revision_not_newer`.
pub(crate) fn owner_moving<'a, 'c>(
    tx: &'a Transaction<'c>,
    next: u64,
    fence: Fence,
) -> Result<Appender<'a, 'c>, EngineError> {
    Ok(Appender::owner_moving(tx, next, hook(fence), known)?)
}

/// Starts appending in a transaction of a writer on its own connection: the kernel moves
/// the `state` revision once after the fence and writes that revision's `events` row with
/// the engine's body, so clients see the change. Inputs: the transaction and the fence.
/// Output: the appender, a fence refusal, `state_log_revision_exhausted` or
/// `state_log_body_bounds`.
pub fn own<'a, 'c>(tx: &'a Transaction<'c>, fence: Fence) -> Result<Appender<'a, 'c>, EngineError> {
    Ok(Appender::own(tx, hook(fence), known, engine_event)?)
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

/// The engine's aggregate names for the kernel writer and replay: only the closed
/// [`Aggregate`] set, read through its serde name, which [`Aggregate::name`] matches.
/// Input: the stored name. Output: its kernel name, none for another.
fn known(name: &str) -> Option<AggregateName> {
    parse(name).map(AggregateName::from)
}

/// The aggregate a stored name names, read through its serde name. Input: the name.
/// Output: the aggregate, none for another.
fn parse(name: &str) -> Option<Aggregate> {
    serde_json::from_value(serde_json::Value::String(name.to_owned())).ok()
}

/// Replays the log after `after` into one aggregate's projection through the kernel replay,
/// reading names through the engine's set. Inputs: the connection, the aggregate, the last
/// seq already applied and the projection. Output: the last seq read, or the first refusal.
pub fn replay(
    db: &Connection,
    aggregate: Aggregate,
    after: u64,
    apply: impl FnMut(&Logged) -> Result<(), EngineError>,
) -> Result<u64, EngineError> {
    kernel_storage::state_log::replay::replay(db, aggregate.into(), after, known, apply)
}

/// Redacts one logged event for an owner delete through the kernel, behind the delete's
/// fence. Inputs: the transaction, the seq and the fence. Output: as the kernel's redact.
pub fn redact(tx: &Transaction<'_>, seq: u64, fence: &Fence) -> Result<(), EngineError> {
    Ok(kernel_storage::state_log::replay::redact(
        tx,
        seq,
        &hook(fence.clone()),
    )?)
}

/// Redacts every matching event of the given aggregates for an owner delete through the
/// kernel, behind the delete's fence. Inputs: the transaction, the aggregates, the kinds
/// (none for all), the field, the value, the kept seq and the fence. Output: how many were
/// redacted, or the kernel's refusal.
pub fn redact_matching(
    tx: &Transaction<'_>,
    aggregates: &[Aggregate],
    kinds: Option<&[&str]>,
    field: &str,
    value: &str,
    keep: u64,
    fence: &Fence,
) -> Result<usize, EngineError> {
    let names: Vec<AggregateName> = aggregates.iter().map(|each| (*each).into()).collect();
    assert_eq!(names.len(), aggregates.len());
    Ok(kernel_storage::state_log::replay::redact_matching(
        tx,
        &names,
        kinds,
        field,
        value,
        keep,
        &hook(fence.clone()),
    )?)
}

/// How many compiled statements an engine connection keeps. One append runs about a dozen
/// fixed statements, and the engine's other writers run their own, so the cache holds them
/// all and no save compiles its SQL again.
const STATEMENTS_CACHED: usize = 64;

/// Readies an engine connection for log writes: a statement cache of [`STATEMENTS_CACHED`]
/// and the namespace tables plugins append to. The engine open and every store built as the
/// engine builds one run it. Input: the connection. Output: Ok, or the store's refusal.
pub(crate) fn initialize(db: &Connection) -> Result<(), EngineError> {
    db.set_prepared_statement_cache_capacity(STATEMENTS_CACHED);
    Ok(kernel_storage::namespace::initialize(db)?)
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
