use super::{
    AggregateName, BODY_BYTES_MAX, Chunk, Draft, SNAPSHOT_CHUNK, SNAPSHOT_DONE,
    TRANSACTION_EVENTS_MAX, check, ensure, error, snapshot_bodies,
};
use crate::StorageError;
use rusqlite::{Connection, OptionalExtension, Transaction, params};

#[cfg(test)]
mod tests;

/// The writer's fence, checked inside the appending transaction: the engine's check of the
/// writer's proof against the store, so a host that stopped is refused in the same
/// transaction as the write. It owns what it checks, so dropping an appender never holds the
/// transaction's borrow. Input: the transaction. Output: Ok, or the engine's refusal.
pub type FenceCheck = Box<dyn Fn(&Transaction<'_>) -> Result<(), StorageError>>;

/// The engine's set of aggregate names, which the writer and replay share: the writer refuses a name outside it and replay reads each stored name through it,
/// so the log never holds a row replay refuses. Input: the name. Output: the engine's own
/// name for it, or none for a name outside the set.
pub type KnownName = fn(&str) -> Option<AggregateName>;

/// The most `events` rows an own write keeps: the top of the engine's `events_retained`
/// setting (1 to 256), so it never trims a row a client's setting keeps; the next owner
/// request trims to the setting.
const EVENTS_KEPT_MAX: i64 = 256;

/// Appends the events of one write transaction to `state_log` at that transaction's
/// `state` revision, each with the next `seq`, behind the writer's fence.
pub struct Appender<'a, 'c> {
    tx: &'a Transaction<'c>,
    revision: u64,
    appended: usize,
    fence: FenceCheck,
    known: KnownName,
}

impl<'a, 'c> Appender<'a, 'c> {
    /// Starts appending in an owner request's transaction, whose revision the request
    /// already moved: it must be the `state` revision now and newer than the last logged
    /// one, so each appending transaction brings its own and none goes back. The fence is
    /// checked first, once for the transaction. Inputs: the transaction, that revision, the
    /// fence and the engine's name check. Output: the appender, the fence's refusal,
    /// `state_log_revision_stale` or `state_log_revision_not_newer`.
    pub fn owner(
        tx: &'a Transaction<'c>,
        revision: u64,
        fence: FenceCheck,
        known: KnownName,
    ) -> Result<Self, StorageError> {
        fence(tx)?;
        Self::begin(tx, revision, fence, known)
    }

    /// Starts appending in an owner request's transaction whose signed next revision is not
    /// yet in `state`: after the fence, it moves `state` from the revision before it to it in
    /// one statement, refusing a store at any other revision, then opens at it, so the
    /// revision the move just set is not read back. Inputs: the transaction, the next
    /// revision, the fence and the name check. Output: the appender, the fence's refusal,
    /// `state_log_revision_stale` or `state_log_revision_not_newer`.
    pub fn owner_moving(
        tx: &'a Transaction<'c>,
        next: u64,
        fence: FenceCheck,
        known: KnownName,
    ) -> Result<Self, StorageError> {
        fence(tx)?;
        let stale = || error("state_log_revision_stale");
        let before = next.checked_sub(1).ok_or_else(stale)?;
        let next_value = i64::try_from(next).map_err(error)?;
        let before_value = i64::try_from(before).map_err(error)?;
        let moved = tx
            .prepare_cached("UPDATE state SET revision=?1 WHERE id=1 AND revision=?2")
            .and_then(|mut statement| statement.execute(params![next_value, before_value]))
            .map_err(error)?;
        if moved != 1 {
            return Err(stale());
        }
        Self::open(tx, next, fence, known)
    }

    /// Starts appending in a transaction of a writer on its own connection, moving the
    /// `state` revision once for it after its fence is checked, and writing that revision's
    /// `events` row so clients see the change. The engine builds the row's body for the new
    /// revision; it is refused over [`BODY_BYTES_MAX`] before any write. Inputs: the
    /// transaction, the fence, the engine's name check and the body builder. Output: the
    /// appender, the fence's refusal, `state_log_revision_exhausted` or
    /// `state_log_body_bounds`.
    pub fn own(
        tx: &'a Transaction<'c>,
        fence: FenceCheck,
        known: KnownName,
        event: impl FnOnce(u64) -> Result<String, StorageError>,
    ) -> Result<Self, StorageError> {
        fence(tx)?;
        let current: Option<i64> = tx
            .prepare_cached("SELECT revision FROM state WHERE id=1")
            .and_then(|mut statement| statement.query_row([], |row| row.get(0)))
            .optional()
            .map_err(error)?;
        let Some(next) = current
            .filter(|revision| *revision < i64::MAX)
            .map(|r| r + 1)
        else {
            return Err(error("state_log_revision_exhausted"));
        };
        let next = u64::try_from(next).map_err(error)?;
        let body = event(next)?;
        if body.len() > BODY_BYTES_MAX {
            return Err(error("state_log_body_bounds"));
        }
        let moved = tx
            .prepare_cached("UPDATE state SET revision=revision+1 WHERE id=1 AND revision<?1")
            .and_then(|mut statement| statement.execute([i64::MAX]))
            .map_err(error)?;
        if moved != 1 {
            return Err(error("state_log_revision_exhausted"));
        }
        let appender = Self::begin(tx, state_revision(tx)?, fence, known)?;
        assert_eq!(appender.revision(), next, "the revision moved once");
        publish(tx, appender.revision(), &body)?;
        Ok(appender)
    }

    /// Runs one projection apply in the appender's transaction after checking the fence
    /// again, the pair of the check at the start. Input: the apply. Output: its result, or
    /// the fence's refusal without running it.
    pub fn project<T, E: From<StorageError>>(
        &self,
        apply: impl FnOnce(&Transaction<'c>) -> Result<T, E>,
    ) -> Result<T, E> {
        (self.fence)(self.tx)?;
        apply(self.tx)
    }

    /// Tombstones one earlier event of this transaction's writer through
    /// [`super::replay::redact`], behind the appender's own fence. Input: the seq. Output: Ok,
    /// or the fence's or redact's refusal.
    pub(crate) fn redact(&self, seq: u64) -> Result<(), StorageError> {
        super::replay::redact(self.tx, seq, &self.fence)
    }

    /// Opens the appender at a fenced revision: it must be the `state` revision now and
    /// newer than the last logged one. Inputs: the transaction, the revision, the fence and
    /// the name check. Output: the appender, or `state_log_revision_stale` or
    /// `state_log_revision_not_newer`.
    fn begin(
        tx: &'a Transaction<'c>,
        revision: u64,
        fence: FenceCheck,
        known: KnownName,
    ) -> Result<Self, StorageError> {
        if revision != state_revision(tx)? {
            return Err(error("state_log_revision_stale"));
        }
        Self::open(tx, revision, fence, known)
    }

    /// Opens the appender at a revision already known to be the `state` revision: it must be
    /// newer than the last logged one. Inputs: the transaction, the revision, the fence and
    /// the name check. Output: the appender, or `state_log_revision_not_newer`.
    fn open(
        tx: &'a Transaction<'c>,
        revision: u64,
        fence: FenceCheck,
        known: KnownName,
    ) -> Result<Self, StorageError> {
        let last = last_logged(tx)?;
        if revision == 0 || last.is_some_and(|last| revision <= last as u64) {
            return Err(error("state_log_revision_not_newer"));
        }
        Ok(Self {
            tx,
            revision,
            appended: 0,
            fence,
            known,
        })
    }

    /// The transaction the appender writes in, for a kernel step that reads or rewrites the
    /// log in the same transaction. Output: the transaction.
    pub(crate) fn tx(&self) -> &'a Transaction<'c> {
        self.tx
    }

    /// The revision this transaction's events carry. Output: the revision.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Appends one event; the snapshot kinds are written only by [`Self::snapshot`]. Input:
    /// the draft. Output: its `seq`, or `state_log_kind_reserved` or a bound's refusal.
    pub fn append(&mut self, draft: &Draft<'_>) -> Result<u64, StorageError> {
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
    ) -> Result<u64, StorageError> {
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

    /// Appends one checked event at the next `seq`, refusing a name the engine's replay does
    /// not read (`state_log_aggregate_unknown`), one past the transaction's bound, past the
    /// last possible `seq`, or after the `state` revision moved off this appender's
    /// (`state_log_revision_stale`). Input: the draft. Output: its `seq`.
    fn push(&mut self, draft: &Draft<'_>) -> Result<u64, StorageError> {
        check(draft)?;
        if (self.known)(draft.aggregate.name()).is_none() {
            return Err(error("state_log_aggregate_unknown"));
        }
        if self.appended >= TRANSACTION_EVENTS_MAX {
            return Err(error("state_log_transaction_full"));
        }
        let (previous, revision): (i64, i64) = self
            .tx
            .prepare_cached(
                "SELECT COALESCE(MAX(seq), 0), (SELECT revision FROM state WHERE id=1) FROM state_log",
            )
            .and_then(|mut statement| {
                statement.query_row([], |row| Ok((row.get(0)?, row.get(1)?)))
            })
            .map_err(error)?;
        if u64::try_from(revision).ok() != Some(self.revision) {
            return Err(error("state_log_revision_stale"));
        }
        let seq = previous
            .checked_add(1)
            .ok_or_else(|| error("state_log_seq_exhausted"))?;
        self.tx
            .prepare_cached("INSERT INTO state_log VALUES (?1,?2,?3,?4,?5,?6,?7)")
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

/// Reads the revision of the last logged event, creating the log table first only when the
/// store has none, so a store that has one runs a single statement. Input: the connection.
/// Output: the revision, none for an empty log, or the store's refusal.
fn last_logged(db: &Connection) -> Result<Option<i64>, StorageError> {
    let tail = || {
        db.prepare_cached("SELECT revision FROM state_log ORDER BY seq DESC LIMIT 1")
            .and_then(|mut statement| statement.query_row([], |row| row.get(0)))
            .optional()
    };
    match tail() {
        Err(failed) if log_missing(&failed) => {
            ensure(db)?;
            tail().map_err(error)
        }
        read => read.map_err(error),
    }
}

/// Whether a refusal is SQLite's answer for a store with no `state_log` table. Input: the
/// refusal. Output: true for that answer only.
fn log_missing(failed: &rusqlite::Error) -> bool {
    matches!(
        failed,
        rusqlite::Error::SqliteFailure(code, Some(message))
            if code.extended_code == rusqlite::ffi::SQLITE_ERROR
                && message == "no such table: state_log"
    )
}

/// Reads the store's `state` revision. Input: the connection. Output: the revision.
fn state_revision(db: &Connection) -> Result<u64, StorageError> {
    let revision: i64 = db
        .prepare_cached("SELECT revision FROM state WHERE id=1")
        .and_then(|mut statement| statement.query_row([], |row| row.get(0)))
        .map_err(error)?;
    u64::try_from(revision).map_err(error)
}

/// Writes the `events` row of a revision an own write moved, in its transaction, bound as
/// text, and trims rows older than [`EVENTS_KEPT_MAX`] revisions. Inputs: the transaction,
/// the revision and the engine's body, already bounded. Output: Ok or the store's refusal.
fn publish(tx: &Transaction<'_>, revision: u64, body: &str) -> Result<(), StorageError> {
    assert!(revision > 0, "an own write moved the revision");
    assert!(
        body.len() <= BODY_BYTES_MAX,
        "the body is bounded before the write"
    );
    let cursor = i64::try_from(revision).map_err(error)?;
    tx.prepare_cached("INSERT INTO events VALUES (?1,?2)")
        .and_then(|mut statement| statement.execute(params![cursor, body]))
        .map_err(error)?;
    tx.prepare_cached("DELETE FROM events WHERE cursor<=?1")
        .and_then(|mut statement| statement.execute([cursor.saturating_sub(EVENTS_KEPT_MAX)]))
        .map_err(error)?;
    Ok(())
}
