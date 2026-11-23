use std::path::Path;

use kernel_plugin_host::Refused;
use kernel_plugin_host::store::KeyFolder;
use kernel_plugin_host::store_log::{self, Captured, Event, KINDS, REDACTED_ON_REMOVE, SCHEMA};
use kernel_storage::state_log::Draft;
use rusqlite::{Connection, Transaction, TransactionBehavior};

use crate::engine::EngineError;
use crate::state_log::Aggregate;

fn unavailable(_: rusqlite::Error) -> EngineError {
    EngineError("app_plugins_unavailable".into())
}

fn refusal(refused: Refused) -> EngineError {
    EngineError(refused.code.into())
}

/// Runs plugin store work in one immediate transaction on the engine connection through
/// [`logged_in`], then deletes the keys whose bindings went once the commit is done, and
/// checkpoints the WAL after a redaction. Inputs: the connection, outside any transaction,
/// the engine root and the work. Output: the work's result, its refusal with nothing
/// written, a fence refusal, or `app_plugins_unavailable` on a connection already inside
/// a transaction.
pub(crate) fn logged<T>(
    db: &Connection,
    root: &Path,
    work: impl FnOnce(&Connection) -> Result<T, EngineError>,
) -> Result<T, EngineError> {
    if !db.is_autocommit() {
        return Err(EngineError("app_plugins_unavailable".into()));
    }
    let tx = Transaction::new_unchecked(db, TransactionBehavior::Immediate).map_err(unavailable)?;
    let (value, captured, redacted) = logged_in(&tx, work)?;
    tx.commit().map_err(unavailable)?;
    assert!(db.is_autocommit());
    for key in &captured.forgotten_keys {
        let _ = (super::KEYS.remove)(root, KeyFolder::Plugin, key);
    }
    if redacted {
        let _ = kernel_storage::state_log::replay::checkpoint(db);
    }
    Ok(value)
}

/// Runs plugin store work inside the caller's transaction: every change it records is
/// written to the store tables and appended to the log under the store fence in that
/// transaction, and a removal tombstones the plugin's earlier events, so a rollback drops
/// both and a commit keeps both. Work that records nothing appends nothing and moves no
/// revision. A key whose binding went stays for the start sweep unless the caller deletes
/// it after its commit. Inputs: the transaction and the work. Output: the work's result,
/// what it recorded and whether anything was redacted, or the work's or the fence's
/// refusal.
pub(crate) fn logged_in<T>(
    tx: &Transaction<'_>,
    work: impl FnOnce(&Connection) -> Result<T, EngineError>,
) -> Result<(T, Captured, bool), EngineError> {
    let (result, captured) = store_log::capture(|| work(tx));
    let value = result?;
    let redacted = if captured.events.is_empty() {
        false
    } else {
        append(tx, &captured.events)?
    };
    Ok((value, captured, redacted))
}

/// Appends the recorded events in their order under the `Plugins` aggregate behind the
/// store fence, tombstoning for each removal the plugin's earlier events but the removal
/// and any code plugin switch. Inputs: the transaction and the events, at least one.
/// Output: whether anything was redacted, or the fence's or log's refusal.
fn append(tx: &Transaction<'_>, events: &[Event]) -> Result<bool, EngineError> {
    assert!(!events.is_empty());
    let fence = crate::worktrees::log::store_fence(tx)?;
    let mut appender = crate::state_log::own(tx, fence.clone())?;
    let at_ms = u64::try_from(crate::team::now_ms()).unwrap_or(0);
    let mut redacted = false;
    for event in events {
        let body = event.body().map_err(refusal)?;
        let seq = appender.append(&Draft {
            aggregate: Aggregate::Plugins.into(),
            kind: event.kind(),
            schema: SCHEMA,
            at_ms,
            body: &body,
        })?;
        if let Event::Forgotten { plugin } = event {
            crate::state_log::redact_matching(
                tx,
                &[Aggregate::Plugins],
                Some(&REDACTED_ON_REMOVE),
                "plugin",
                plugin,
                seq,
                &fence,
            )?;
            redacted = true;
        }
    }
    Ok(redacted)
}

/// Clears the three plugin store tables and writes them again from the plugin store
/// events of the log, the way they were written live; other kinds of the aggregate are
/// left to their own projections. Input: the transaction. Output: the last seq replayed,
/// or the log's refusal.
pub fn rebuild(tx: &Transaction<'_>) -> Result<u64, EngineError> {
    tx.execute_batch(
        "DELETE FROM app_plugin_secrets;
         DELETE FROM app_plugin_state;
         DELETE FROM app_plugin_trust;",
    )
    .map_err(unavailable)?;
    crate::state_log::replay(tx, Aggregate::Plugins, 0, |logged| {
        if !KINDS.contains(&logged.kind.as_str()) {
            return Ok(());
        }
        let event = Event::decode(&logged.kind, logged.schema, &logged.body).map_err(refusal)?;
        store_log::apply(tx, &event).map_err(refusal)
    })
}
