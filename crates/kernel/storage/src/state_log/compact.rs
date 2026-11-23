use super::replay::securely;
use super::{
    AggregateName, Appender, Chunk, SNAPSHOT_CHUNK, SNAPSHOT_DONE, SnapshotDone, TOMBSTONE, error,
    verify_snapshot,
};
use crate::StorageError;
use rusqlite::{OptionalExtension, Transaction, params};

#[cfg(test)]
mod tests;

/// Events per aggregate between two compaction snapshot runs (v2.5 plan section 3.2).
pub const COMPACT_EVERY: u64 = 10_000;

/// Whether compaction runs, from the caller's setting: a dev-only switch, Off by default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Compaction {
    #[default]
    Off,
    On,
}

/// After an append, when `compaction` is On and the aggregate has [`COMPACT_EVERY`] events
/// since its newest `SnapshotDone`, writes a snapshot run from `rows` at `at_ms`, verifies it
/// and the second-newest run, then tombstones that aggregate's bodies behind the
/// second-newest run, zeroed, so seq and the replay gap check stay whole. Inputs: the
/// appender, the switch, the aggregate, the time and its rows. Output: the tombstoned row
/// count, 0 when Off, not due or with one run only.
pub fn compact(
    appender: &mut Appender<'_, '_>,
    compaction: Compaction,
    aggregate: AggregateName,
    at_ms: u64,
    rows: impl FnOnce() -> Result<Vec<Chunk>, StorageError>,
) -> Result<u64, StorageError> {
    if compaction == Compaction::Off {
        return Ok(0);
    }
    let tx = appender.tx();
    let newest = newest_done(tx, aggregate, 0)?;
    if events_after(tx, aggregate, newest.unwrap_or(0))? < COMPACT_EVERY {
        return Ok(0);
    }
    let done = appender.snapshot(aggregate, at_ms, rows()?)?;
    assert!(newest.is_none_or(|newest| done > newest));
    verify_snapshot(tx, done)?;
    let Some(second) = newest_done(tx, aggregate, 1)? else {
        return Ok(0);
    };
    verify_snapshot(tx, second)?;
    let behind = first_chunk(tx, second)?;
    let pruned = securely(tx, || {
        tx.execute(
            "UPDATE state_log SET body=?1 WHERE aggregate=?2 AND seq<?3 AND body<>?1",
            params![TOMBSTONE, aggregate.name(), seq_value(behind)?],
        )
        .map_err(error)
    })?;
    let kept: Vec<u8> = tx
        .query_row(
            "SELECT body FROM state_log WHERE seq=?1",
            [seq_value(second)?],
            |row| row.get(0),
        )
        .map_err(error)?;
    assert_ne!(kept.as_slice(), TOMBSTONE, "the second-newest run is kept");
    u64::try_from(pruned).map_err(error)
}

/// The seq of one of an aggregate's `SnapshotDone` events, counted from the newest. Inputs:
/// the transaction, the aggregate and how many newer ones to pass. Output: the seq, or none.
fn newest_done(
    tx: &Transaction<'_>,
    aggregate: AggregateName,
    skip: i64,
) -> Result<Option<u64>, StorageError> {
    let seq: Option<i64> = tx
        .query_row(
            "SELECT seq FROM state_log WHERE aggregate=?1 AND kind=?2
            ORDER BY seq DESC LIMIT 1 OFFSET ?3",
            params![aggregate.name(), SNAPSHOT_DONE, skip],
            |row| row.get(0),
        )
        .optional()
        .map_err(error)?;
    seq.map(|seq| u64::try_from(seq).map_err(error)).transpose()
}

/// How many of an aggregate's events, snapshot runs left out, follow a seq. Inputs: the
/// transaction, the aggregate and the seq. Output: the count.
fn events_after(
    tx: &Transaction<'_>,
    aggregate: AggregateName,
    after: u64,
) -> Result<u64, StorageError> {
    let count: i64 = tx
        .query_row(
            "SELECT COUNT(*) FROM state_log WHERE aggregate=?1 AND seq>?2
            AND kind<>?3 AND kind<>?4",
            params![
                aggregate.name(),
                seq_value(after)?,
                SNAPSHOT_CHUNK,
                SNAPSHOT_DONE
            ],
            |row| row.get(0),
        )
        .map_err(error)?;
    u64::try_from(count).map_err(error)
}

/// The seq of the first chunk of the run a verified `SnapshotDone` closes: its chunks are
/// the events right before it, as many as its body counts. Inputs: the transaction and the
/// closing seq. Output: the first chunk's seq.
fn first_chunk(tx: &Transaction<'_>, done: u64) -> Result<u64, StorageError> {
    let body: Vec<u8> = tx
        .query_row(
            "SELECT body FROM state_log WHERE seq=?1",
            [seq_value(done)?],
            |row| row.get(0),
        )
        .map_err(error)?;
    let closed: SnapshotDone = serde_json::from_slice(&body).map_err(error)?;
    let first = done
        .checked_sub(u64::from(closed.chunks))
        .ok_or_else(|| error("state_log_snapshot_chunks"))?;
    assert!(first <= done);
    Ok(first)
}

/// A seq as SQLite stores it. Input: the seq. Output: it as i64, or the conversion error.
fn seq_value(seq: u64) -> Result<i64, StorageError> {
    i64::try_from(seq).map_err(error)
}
