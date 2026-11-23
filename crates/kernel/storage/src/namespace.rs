use crate::StorageError;
use crate::state_log::replay::Logged;
use crate::state_log::{AggregateName, Appender, BODY_BYTES_MAX, Draft, error};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};

#[cfg(test)]
mod tests;

/// The most bytes of a plugin id.
pub const PLUGIN_BYTES_MAX: usize = 64;
/// The most bytes of a key.
pub const KEY_BYTES_MAX: usize = 256;
/// The most bytes of a value.
pub const VALUE_BYTES_MAX: usize = 32_768;
/// The most records one plugin holds.
pub const RECORDS_PER_PLUGIN_MAX: u64 = 4_096;
/// The most key and value bytes one plugin holds, summed over its records.
pub const BYTES_PER_PLUGIN_MAX: u64 = 16 * 1024 * 1024;
const _: () = assert!(
    PLUGIN_BYTES_MAX + KEY_BYTES_MAX + VALUE_BYTES_MAX.div_ceil(3) * 4 + 512 <= BODY_BYTES_MAX
);

/// The aggregate every namespace event is logged under.
pub const PLUGINS: AggregateName = AggregateName::new("plugins");
/// The kind of the event that sets one record's value.
pub const RECORD_PUT: &str = "RecordPut";
/// The kind of the event that removes one record.
pub const RECORD_DELETED: &str = "RecordDeleted";

/// A plugin's own namespace, named by the plugin id of the bound host that makes the call.
/// Storage never checks grants: the bus gateway does, for every SDK call. Input: the plugin
/// id, 1 to 64 bytes of `[a-z0-9_-]`. Output: the namespace, or `namespace_plugin_name`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Namespace {
    plugin: String,
}

impl Namespace {
    /// Names the namespace of one plugin id. Input: the id. Output: the namespace, or
    /// `namespace_plugin_name` for an id out of shape.
    pub fn for_plugin(plugin: &str) -> Result<Self, StorageError> {
        let shaped = (1..=PLUGIN_BYTES_MAX).contains(&plugin.len())
            && plugin
                .bytes()
                .all(|byte| matches!(byte, b'a'..=b'z' | b'0'..=b'9' | b'_' | b'-'));
        if !shaped {
            return Err(error("namespace_plugin_name"));
        }
        Ok(Self {
            plugin: plugin.to_string(),
        })
    }
}

/// The body of a [`RECORD_PUT`] event: the plugin, the key and the value in base64.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RecordPut {
    plugin: String,
    key: String,
    value: String,
}

/// The body of a [`RECORD_DELETED`] event: the plugin and the key.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RecordDeleted {
    plugin: String,
    key: String,
}

/// Creates `plugin_records` when the store has none. Input: the connection or open
/// transaction. Output: Ok, or the store's refusal.
pub fn initialize(db: &Connection) -> Result<(), StorageError> {
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS plugin_records (
            plugin TEXT NOT NULL CHECK(length(plugin) BETWEEN 1 AND 64),
            key TEXT NOT NULL CHECK(length(key) BETWEEN 1 AND 256),
            value BLOB NOT NULL CHECK(length(value) <= 32768),
            put_seq INTEGER NOT NULL CHECK(put_seq >= 1),
            PRIMARY KEY (plugin, key)
        ) STRICT;",
    )
    .map_err(error)
}

/// Reads one record of a namespace. Inputs: the transaction, the namespace and the key.
/// Output: the value, none when the key holds none, or `namespace_key_bounds`.
pub fn get(
    tx: &Transaction<'_>,
    ns: &Namespace,
    key: &str,
) -> Result<Option<Vec<u8>>, StorageError> {
    check_key(key)?;
    tx.query_row(
        "SELECT value FROM plugin_records WHERE plugin=?1 AND key=?2",
        params![ns.plugin, key],
        |row| row.get(0),
    )
    .optional()
    .map_err(error)
}

/// Sets one record's value: appends a [`RECORD_PUT`], tombstones the event of the value it
/// replaces and writes the row through the appender's projection, all in its transaction.
/// Inputs: the appender, the namespace, the key, the value and the time. Output: Ok, or
/// `namespace_key_bounds`, `namespace_value_bounds`, `namespace_full` or the log's refusal.
pub fn put(
    appender: &mut Appender<'_, '_>,
    ns: &Namespace,
    key: &str,
    value: &[u8],
    at_ms: u64,
) -> Result<(), StorageError> {
    check_key(key)?;
    if value.len() > VALUE_BYTES_MAX {
        return Err(error("namespace_value_bounds"));
    }
    let tx = appender.tx();
    let replaced = current(tx, ns, key)?;
    let (records, bytes) = held(tx, ns)?;
    let (records, bytes) = match replaced {
        Some((_, held_bytes)) => (records - 1, bytes - held_bytes),
        None => (records, bytes),
    };
    let added = (key.len() + value.len()) as u64;
    if records >= RECORDS_PER_PLUGIN_MAX || bytes + added > BYTES_PER_PLUGIN_MAX {
        return Err(error("namespace_full"));
    }
    let body = serde_json::to_vec(&RecordPut {
        plugin: ns.plugin.clone(),
        key: key.to_string(),
        value: BASE64.encode(value),
    })
    .map_err(error)?;
    assert!(
        body.len() <= BODY_BYTES_MAX,
        "a bounded record fits one body"
    );
    let seq = appender.append(&draft(RECORD_PUT, at_ms, &body))?;
    if let Some((old, _)) = replaced {
        assert!(old < seq, "the replaced put is older");
        appender.redact(old)?;
    }
    appender.project(|tx| write_record(tx, &ns.plugin, key, value, seq))
}

/// Removes one record: appends a [`RECORD_DELETED`], tombstones the event of its value and
/// deletes the row through the appender's projection. Inputs: the appender, the namespace,
/// the key and the time. Output: whether a record was removed, `namespace_key_bounds` or the
/// log's refusal.
pub fn delete(
    appender: &mut Appender<'_, '_>,
    ns: &Namespace,
    key: &str,
    at_ms: u64,
) -> Result<bool, StorageError> {
    check_key(key)?;
    let Some((old, _)) = current(appender.tx(), ns, key)? else {
        return Ok(false);
    };
    let body = serde_json::to_vec(&RecordDeleted {
        plugin: ns.plugin.clone(),
        key: key.to_string(),
    })
    .map_err(error)?;
    assert!(body.len() <= BODY_BYTES_MAX, "a bounded key fits one body");
    let seq = appender.append(&draft(RECORD_DELETED, at_ms, &body))?;
    assert!(old < seq, "the deleted put is older");
    appender.redact(old)?;
    appender.project(|tx| remove_record(tx, &ns.plugin, key))?;
    Ok(true)
}

/// Lists a namespace's keys in order, after a key when one is given. Inputs: the
/// transaction, the namespace, the key to start after and the most keys, at most
/// [`RECORDS_PER_PLUGIN_MAX`]. Output: the keys, or `namespace_key_bounds`.
pub fn list(
    tx: &Transaction<'_>,
    ns: &Namespace,
    after: Option<&str>,
    limit: usize,
) -> Result<Vec<String>, StorageError> {
    if let Some(after) = after {
        check_key(after)?;
    }
    let limit = limit.min(RECORDS_PER_PLUGIN_MAX as usize) as i64;
    let mut statement = tx
        .prepare("SELECT key FROM plugin_records WHERE plugin=?1 AND key>?2 ORDER BY key LIMIT ?3")
        .map_err(error)?;
    let keys = statement
        .query_map(params![ns.plugin, after.unwrap_or(""), limit], |row| {
            row.get(0)
        })
        .map_err(error)?
        .collect::<rusqlite::Result<Vec<String>>>()
        .map_err(error)?;
    assert!(keys.len() as i64 <= limit);
    Ok(keys)
}

/// Applies one Plugins event to `plugin_records` during replay: a put writes its row at the
/// event's own seq, a delete removes it, and the aggregate's other kinds are left to their
/// own projections. Inputs: the transaction and the event. Output: Ok, or
/// `namespace_replay_body` for a body that does not read.
pub fn apply(tx: &Transaction<'_>, event: &Logged) -> Result<(), StorageError> {
    assert!(
        event.aggregate == PLUGINS,
        "replay passes Plugins events only"
    );
    let unread = |_| error("namespace_replay_body");
    match event.kind.as_str() {
        RECORD_PUT => {
            let put: RecordPut = serde_json::from_slice(&event.body).map_err(unread)?;
            let value = BASE64
                .decode(&put.value)
                .map_err(|_| error("namespace_replay_body"))?;
            write_record(tx, &put.plugin, &put.key, &value, event.seq)
        }
        RECORD_DELETED => {
            let deleted: RecordDeleted = serde_json::from_slice(&event.body).map_err(unread)?;
            remove_record(tx, &deleted.plugin, &deleted.key)
        }
        _ => Ok(()),
    }
}

/// Refuses a key out of bounds: 1 to [`KEY_BYTES_MAX`] bytes with no control character.
/// Input: the key. Output: Ok, or `namespace_key_bounds`.
fn check_key(key: &str) -> Result<(), StorageError> {
    if !(1..=KEY_BYTES_MAX).contains(&key.len()) || key.chars().any(char::is_control) {
        return Err(error("namespace_key_bounds"));
    }
    Ok(())
}

/// The put seq and key plus value bytes of one record. Inputs: the transaction, the
/// namespace and the key. Output: them, or none when the key holds no record.
fn current(
    tx: &Transaction<'_>,
    ns: &Namespace,
    key: &str,
) -> Result<Option<(u64, u64)>, StorageError> {
    let found: Option<(i64, i64)> = tx
        .query_row(
            "SELECT put_seq, length(CAST(key AS BLOB)) + length(value) FROM plugin_records
            WHERE plugin=?1 AND key=?2",
            params![ns.plugin, key],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(error)?;
    found
        .map(|(seq, bytes)| {
            Ok((
                u64::try_from(seq).map_err(error)?,
                u64::try_from(bytes).map_err(error)?,
            ))
        })
        .transpose()
}

/// How many records a namespace holds and their key plus value bytes. Inputs: the
/// transaction and the namespace. Output: the count and the bytes.
fn held(tx: &Transaction<'_>, ns: &Namespace) -> Result<(u64, u64), StorageError> {
    let (records, bytes): (i64, i64) = tx
        .query_row(
            "SELECT COUNT(*), COALESCE(SUM(length(CAST(key AS BLOB)) + length(value)), 0)
            FROM plugin_records WHERE plugin=?1",
            [&ns.plugin],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(error)?;
    Ok((
        u64::try_from(records).map_err(error)?,
        u64::try_from(bytes).map_err(error)?,
    ))
}

/// One namespace event of the Plugins aggregate. Inputs: the kind, the time and the body.
/// Output: the draft.
fn draft<'b>(kind: &'b str, at_ms: u64, body: &'b [u8]) -> Draft<'b> {
    Draft {
        aggregate: PLUGINS,
        kind,
        schema: 1,
        at_ms,
        body,
    }
}

/// Writes one record's row with the seq of the put holding its value. Inputs: the
/// transaction, the plugin, the key, the value and the seq. Output: Ok, or the store's
/// refusal.
fn write_record(
    tx: &Transaction<'_>,
    plugin: &str,
    key: &str,
    value: &[u8],
    seq: u64,
) -> Result<(), StorageError> {
    let seq = i64::try_from(seq).map_err(error)?;
    tx.execute(
        "INSERT INTO plugin_records VALUES (?1,?2,?3,?4)
        ON CONFLICT(plugin, key) DO UPDATE SET value=excluded.value, put_seq=excluded.put_seq",
        params![plugin, key, value, seq],
    )
    .map_err(error)?;
    Ok(())
}

/// Removes one record's row. Inputs: the transaction, the plugin and the key. Output: Ok, or
/// the store's refusal.
fn remove_record(tx: &Transaction<'_>, plugin: &str, key: &str) -> Result<(), StorageError> {
    tx.execute(
        "DELETE FROM plugin_records WHERE plugin=?1 AND key=?2",
        params![plugin, key],
    )
    .map_err(error)?;
    Ok(())
}
