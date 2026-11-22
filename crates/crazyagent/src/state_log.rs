use crate::engine::EngineError;
use fence::Fence;
use kernel_storage::state_log::{
    AggregateName, Chunk, Draft, SNAPSHOT_CHUNK, SNAPSHOT_DONE, TRANSACTION_EVENTS_MAX, check,
    ensure, snapshot_bodies,
};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
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





/// Appends the events of one write transaction to `state_log` at that transaction's
/// `state` revision, each with the next `seq`, behind the writer's fence.
pub struct Appender<'a, 'c> {
    tx: &'a Transaction<'c>,
    revision: u64,
    appended: usize,
    fence: Fence,
}

impl<'a, 'c> Appender<'a, 'c> {
    /// Starts appending in an owner request's transaction, whose revision the request
    /// already moved: it must be the `state` revision now and newer than the last logged
    /// one, so each appending transaction brings its own and none goes back. The fence is
    /// checked first, once for the transaction. Inputs: the transaction, that revision and
    /// the fence. Output: the appender, a fence refusal, `state_log_revision_stale` or
    /// `state_log_revision_not_newer`.
    pub fn owner(
        tx: &'a Transaction<'c>,
        revision: u64,
        fence: Fence,
    ) -> Result<Self, EngineError> {
        fence::check(tx, &fence)?;
        Self::begin(tx, revision, fence)
    }

    /// Starts appending in a transaction of a writer on its own connection, moving the
    /// `state` revision once for it after its fence is checked, and writing that revision's
    /// `events` row so clients see the change (`publish`). Inputs: the transaction and
    /// the fence. Output: the appender, a fence refusal, or `state_log_revision_exhausted`
    /// when the revision is at its largest value.
    pub fn own(tx: &'a Transaction<'c>, fence: Fence) -> Result<Self, EngineError> {
        fence::check(tx, &fence)?;
        let moved = tx
            .execute(
                "UPDATE state SET revision=revision+1 WHERE id=1 AND revision<?1",
                [i64::MAX],
            )
            .map_err(error)?;
        if moved != 1 {
            return Err(error("state_log_revision_exhausted"));
        }
        let appender = Self::begin(tx, state_revision(tx)?, fence)?;
        publish(tx, appender.revision())?;
        Ok(appender)
    }

    /// Starts appending in a transaction of the storage open or one of its schema steps,
    /// which write before any engine host of the store starts, so they bring the open's
    /// proof in place of a host or owner fence (v2.5 L6). It moves the `state` revision
    /// once, as [`Self::own`]. Inputs: the transaction and the proof. Output: the appender,
    /// `state_log_opening_host_running` or `state_log_revision_exhausted`.
    pub(crate) fn opening(
        tx: &'a Transaction<'c>,
        opening: crate::storage::opening::Opening,
    ) -> Result<Self, EngineError> {
        Self::own(tx, Fence::Opening(opening))
    }

    /// Runs one projection apply in the appender's transaction after checking the fence
    /// again, the pair of the check at the start. Input: the apply. Output: its result, or
    /// the fence refusal without running it.
    pub fn project<T>(
        &self,
        apply: impl FnOnce(&Transaction<'c>) -> Result<T, EngineError>,
    ) -> Result<T, EngineError> {
        fence::check(self.tx, &self.fence)?;
        apply(self.tx)
    }

    /// Opens the appender at a fenced revision: it must be the `state` revision now and
    /// newer than the last logged one. Inputs: the transaction, the revision and the fence.
    /// Output: the appender, or `state_log_revision_stale` or `state_log_revision_not_newer`.
    fn begin(tx: &'a Transaction<'c>, revision: u64, fence: Fence) -> Result<Self, EngineError> {
        ensure(tx)?;
        if revision != state_revision(tx)? {
            return Err(error("state_log_revision_stale"));
        }
        let last: Option<i64> = tx
            .query_row(
                "SELECT revision FROM state_log ORDER BY seq DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .optional()
            .map_err(error)?;
        if revision == 0 || last.is_some_and(|last| revision <= last as u64) {
            return Err(error("state_log_revision_not_newer"));
        }
        Ok(Self {
            tx,
            revision,
            appended: 0,
            fence,
        })
    }

    /// The revision this transaction's events carry. Output: the revision.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Appends one event; the snapshot kinds are written only by [`Self::snapshot`]. Input:
    /// the draft. Output: its `seq`, or `state_log_kind_reserved` or a bound's refusal.
    pub fn append(&mut self, draft: &Draft<'_>) -> Result<u64, EngineError> {
        if draft.kind == SNAPSHOT_CHUNK || draft.kind == SNAPSHOT_DONE {
            return Err(error("state_log_kind_reserved"));
        }
        self.push(draft)
    }

    /// Appends a snapshot run of an aggregate: one chunk event per chunk, in order from 0,
    /// closed by an event holding the chunk count and the sha256 of the chunk bodies in
    /// order. Every chunk is checked under [`BODY_BYTES_MAX`] before any is appended.
    /// Inputs: the aggregate, the time and the chunks. Output: the `seq` of the closing
    /// event, or `state_log_chunk_bounds` or `state_log_transaction_full`.
    pub fn snapshot(
        &mut self,
        aggregate: impl Into<AggregateName>,
        at_ms: u64,
        chunks: Vec<Chunk>,
    ) -> Result<u64, EngineError> {
        let aggregate = aggregate.into();
        if chunks.len() >= TRANSACTION_EVENTS_MAX - self.appended {
            return Err(error("state_log_transaction_full"));
        }
        let (bodies, done) = snapshot_bodies(aggregate, chunks)?;
        for body in &bodies {
            self.push(&Draft {
                aggregate,
                kind: SNAPSHOT_CHUNK,
                schema: 1,
                at_ms,
                body,
            })?;
        }
        self.push(&Draft {
            aggregate,
            kind: SNAPSHOT_DONE,
            schema: 1,
            at_ms,
            body: &done,
        })
    }

    /// Appends one checked event at the next `seq`, refusing one past the transaction's
    /// bound or past the last possible `seq`. Input: the draft. Output: its `seq`.
    fn push(&mut self, draft: &Draft<'_>) -> Result<u64, EngineError> {
        check(draft)?;
        if self.appended >= TRANSACTION_EVENTS_MAX {
            return Err(error("state_log_transaction_full"));
        }
        let previous: i64 = self
            .tx
            .prepare("SELECT COALESCE(MAX(seq), 0) FROM state_log")
            .and_then(|mut statement| statement.query_row([], |row| row.get(0)))
            .map_err(error)?;
        let seq = previous
            .checked_add(1)
            .ok_or_else(|| error("state_log_seq_exhausted"))?;
        self.tx
            .prepare("INSERT INTO state_log VALUES (?1,?2,?3,?4,?5,?6,?7)")
            .and_then(|mut statement| {
                statement.execute(params![
                    seq,
                    self.revision as i64,
                    draft.aggregate.name(),
                    draft.kind,
                    draft.schema,
                    draft.at_ms as i64,
                    draft.body
                ])
            })
            .map_err(error)?;
        self.appended += 1;
        Ok(seq as u64)
    }
}



/// Reads the store's `state` revision. Input: the connection. Output: the revision.
fn state_revision(db: &Connection) -> Result<u64, EngineError> {
    let revision: i64 = db
        .query_row("SELECT revision FROM state WHERE id=1", [], |row| {
            row.get(0)
        })
        .map_err(error)?;
    u64::try_from(revision).map_err(error)
}

/// The client an engine-own revision's `events` row names. The wire refuses every `owner:`
/// client and an owner's own is `owner:` and 64 hex digits, so no client takes the row for
/// its own request.
const ENGINE_CLIENT: &str = "owner:engine";

/// The most `events` rows an engine-own write keeps: the top of the `events_retained`
/// setting (1 to 256), so it never trims a row a client's setting keeps; the next owner
/// request trims to the setting.
const EVENTS_KEPT_MAX: i64 = 256;

/// Writes the `events` row of a revision an engine-own write moved, in its transaction, so a
/// client catching up from `events` moves to it and its next save is not `stale_revision`
/// (v2.5 L6, fix11 L7 Q6). The row's topic is `unknown`: any writer may append here (S3a).
/// Every store with a `state` table has `events`, from the same schema batch. Inputs: the
/// transaction and the revision. Output: Ok or the store's refusal.
fn publish(tx: &Transaction<'_>, revision: u64) -> Result<(), EngineError> {
    use crate::protocol::{ClientId, Cursor, Event, RequestId, Topic};
    assert!(revision > 0, "an own write moved the revision");
    let event = Event {
        cursor: Cursor::new(revision),
        request_id: RequestId::new(format!("engine-{revision}")).map_err(error)?,
        client_id: ClientId::new(ENGINE_CLIENT).map_err(error)?,
        topic: Topic::Unknown,
    };
    let cursor = i64::try_from(revision).map_err(error)?;
    let body = serde_json::to_string(&event).map_err(error)?;
    tx.execute("INSERT INTO events VALUES (?1,?2)", params![cursor, body])
        .map_err(error)?;
    tx.execute(
        "DELETE FROM events WHERE cursor<=?1",
        [cursor.saturating_sub(EVENTS_KEPT_MAX)],
    )
    .map_err(error)?;
    assert_eq!(event.cursor.value(), revision);
    Ok(())
}


fn error(value: impl std::fmt::Display) -> EngineError {
    EngineError(value.to_string())
}
