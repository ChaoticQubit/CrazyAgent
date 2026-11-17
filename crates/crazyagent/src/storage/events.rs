use super::{error, settings};
use crate::config::EffectiveConfig;
use crate::engine::EngineError;
use crate::protocol::{ClientId, Cursor, Event, Outcome, RequestId, WireRequest};
use rusqlite::{Transaction, params};

pub(super) fn events(
    tx: &Transaction<'_>,
    after: u64,
    revision: u64,
) -> Result<Outcome, EngineError> {
    let first: Option<i64> = tx
        .query_row("SELECT MIN(cursor) FROM events", [], |row| row.get(0))
        .map_err(error)?;
    if after > revision || first.is_some_and(|first| after.saturating_add(1) < first as u64) {
        return Ok(Outcome::ResyncRequired);
    }
    let mut statement = tx
        .prepare("SELECT body FROM events WHERE cursor>?1 ORDER BY cursor LIMIT 128")
        .map_err(error)?;
    let rows = statement
        .query_map([after as i64], |row| row.get::<_, String>(0))
        .map_err(error)?;
    let events = rows
        .map(|row| serde_json::from_str(&row.map_err(error)?).map_err(error))
        .collect::<Result<Vec<Event>, _>>()?;
    assert!(events.len() <= 128);
    assert!(
        events
            .windows(2)
            .all(|w| w[0].cursor.value() < w[1].cursor.value())
    );
    Ok(Outcome::Events { events })
}

pub(super) fn persist_event(
    tx: &Transaction<'_>,
    config: &EffectiveConfig,
    request: &WireRequest,
    revision: u64,
) -> Result<(), EngineError> {
    persist_event_ids(
        tx,
        config,
        &request.request_id,
        &request.client_id,
        revision,
    )
}

pub(super) fn persist_event_ids(
    tx: &Transaction<'_>,
    config: &EffectiveConfig,
    request_id: &RequestId,
    client_id: &ClientId,
    revision: u64,
) -> Result<(), EngineError> {
    settings::ensure_clear(tx)?;
    let event = Event {
        cursor: Cursor::new(revision),
        request_id: request_id.clone(),
        client_id: client_id.clone(),
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
