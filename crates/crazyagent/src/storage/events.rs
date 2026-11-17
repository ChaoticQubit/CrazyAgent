use super::{error, owner_revision, settings};
use crate::config::EffectiveConfig;
use crate::engine::EngineError;
use crate::protocol::{ClientId, Cursor, Event, Outcome, RequestId, Topic, WireRequest};
use rusqlite::{Connection, Transaction, params};
use std::collections::BTreeSet;

/// The most events [`logged`] reads at once.
const CATCH_UP_MAX: usize = 128;

#[cfg(test)]
mod tests;

/// What a watch reads after its cursor (v2.5 S3a). `Logged`: the cursor to send next and the
/// sorted topics of the events after the old one; `Refresh`: the old cursor is older than the
/// retained events or ahead of the store, with the store's revision.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Changes {
    Logged { cursor: Cursor, topics: Vec<Topic> },
    Refresh { cursor: Cursor },
}

/// Reads, in one read transaction, the store's revision and the events after `after`, at most
/// the 128 the catch-up reader returns; at that many the cursor stops at the last one read,
/// so the next read goes on from there. Inputs: a connection on the engine store and the
/// cursor. Output: the changes, or the store's error.
pub(crate) fn changes(db: &mut Connection, after: &Cursor) -> Result<Changes, EngineError> {
    let tx = db.transaction().map_err(error)?;
    let revision = owner_revision(&tx)?;
    let Some(events) = logged(&tx, after.value(), revision)? else {
        return Ok(Changes::Refresh {
            cursor: Cursor::new(revision),
        });
    };
    let cursor = match events.last() {
        Some(last) if events.len() == CATCH_UP_MAX => last.cursor.clone(),
        _ => Cursor::new(revision),
    };
    let topics: BTreeSet<Topic> = events.iter().map(|event| event.topic).collect();
    Ok(Changes::Logged {
        cursor,
        topics: topics.into_iter().collect(),
    })
}

/// The `Events` command's reply: the events after `after`, or `ResyncRequired` for a cursor
/// older than the retained events or ahead of the store. Inputs: the transaction, the cursor
/// and the store's revision. Output: the outcome, or the store's error.
pub(super) fn events(
    tx: &Transaction<'_>,
    after: u64,
    revision: u64,
) -> Result<Outcome, EngineError> {
    Ok(match logged(tx, after, revision)? {
        Some(events) => Outcome::Events { events },
        None => Outcome::ResyncRequired,
    })
}

/// The events after `after`, at most [`CATCH_UP_MAX`] in cursor order, or none for a cursor
/// older than the retained events or ahead of the store. Inputs: the transaction, the cursor
/// and the store's revision. Output: the events, or the store's error.
fn logged(
    tx: &Transaction<'_>,
    after: u64,
    revision: u64,
) -> Result<Option<Vec<Event>>, EngineError> {
    let first: Option<i64> = tx
        .query_row("SELECT MIN(cursor) FROM events", [], |row| row.get(0))
        .map_err(error)?;
    if after > revision || first.is_some_and(|first| after.saturating_add(1) < first as u64) {
        return Ok(None);
    }
    let mut statement = tx
        .prepare("SELECT body FROM events WHERE cursor>?1 ORDER BY cursor LIMIT ?2")
        .map_err(error)?;
    let rows = statement
        .query_map(params![after as i64, CATCH_UP_MAX as i64], |row| {
            row.get::<_, String>(0)
        })
        .map_err(error)?;
    let events = rows
        .map(|row| serde_json::from_str(&row.map_err(error)?).map_err(error))
        .collect::<Result<Vec<Event>, _>>()?;
    assert!(events.len() <= CATCH_UP_MAX);
    assert!(
        events
            .windows(2)
            .all(|w| w[0].cursor.value() < w[1].cursor.value())
    );
    Ok(Some(events))
}

/// Logs an applied request's event under the topic its command names. Inputs: the request's
/// transaction, the config, the request and the revision it committed. Output: Ok, or the
/// store's error.
pub(super) fn persist_event(
    tx: &Transaction<'_>,
    config: &EffectiveConfig,
    request: &WireRequest,
    revision: u64,
) -> Result<(), EngineError> {
    persist_topic(
        tx,
        config,
        &request.request_id,
        &request.client_id,
        revision,
        request.command.topic(),
    )
}

/// Logs the event of a change made with no command to name its subject, as `Unknown`.
/// Inputs: the transaction, the config, the request and client ids and the revision. Output:
/// as [`persist_event`].
pub(super) fn persist_event_ids(
    tx: &Transaction<'_>,
    config: &EffectiveConfig,
    request_id: &RequestId,
    client_id: &ClientId,
    revision: u64,
) -> Result<(), EngineError> {
    persist_topic(tx, config, request_id, client_id, revision, Topic::Unknown)
}

/// Moves the store's revision to `revision`, logs its event with the topic and prunes the
/// events older than the retained window. Inputs: the transaction, the config, the request
/// and client ids, the revision and the topic. Output: as [`persist_event`].
fn persist_topic(
    tx: &Transaction<'_>,
    config: &EffectiveConfig,
    request_id: &RequestId,
    client_id: &ClientId,
    revision: u64,
    topic: Topic,
) -> Result<(), EngineError> {
    settings::ensure_clear(tx)?;
    let event = Event {
        cursor: Cursor::new(revision),
        request_id: request_id.clone(),
        client_id: client_id.clone(),
        topic,
    };
    tx.execute("UPDATE state SET revision=?1 WHERE id=1", [revision as i64])
        .map_err(error)?;
    tx.execute(
        "INSERT INTO events VALUES (?1,?2)",
        params![
            revision as i64,
            serde_json::to_string(&event).map_err(error)?
        ],
    )
    .map_err(error)?;
    tx.execute(
        "DELETE FROM events WHERE cursor<=?1",
        [revision.saturating_sub(config.settings.events_retained as u64) as i64],
    )
    .map_err(error)?;
    assert!(revision > 0);
    assert_eq!(event.cursor.value(), revision);
    Ok(())
}
