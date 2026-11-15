use crate::connectors::mcp_client::VendorTool;
use crate::engine::EngineError;
use rusqlite::{Connection, params};

const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS connector_tool_pins (
    connector TEXT NOT NULL CHECK(length(connector) BETWEEN 1 AND 40),
    endpoint TEXT NOT NULL CHECK(endpoint IN ('normal','read_only')),
    sha256 TEXT NOT NULL CHECK(length(sha256) = 64),
    pinned_ms INTEGER NOT NULL,
    PRIMARY KEY(connector, endpoint)
);
CREATE TABLE IF NOT EXISTS connector_tool_changes (
    connector TEXT NOT NULL CHECK(length(connector) BETWEEN 1 AND 40),
    endpoint TEXT NOT NULL CHECK(endpoint IN ('normal','read_only')),
    sha256 TEXT NOT NULL CHECK(length(sha256) = 64),
    title TEXT NOT NULL CHECK(length(title) BETWEEN 1 AND 120),
    seen_ms INTEGER NOT NULL,
    PRIMARY KEY(connector, endpoint)
);";

/// The longest connector title a change row keeps for the owner's notice.
const TITLE_BYTES_MAX: usize = 120;

/// Which of a connector's two tool lists a pin is for: the one at its normal address, or
/// the one its vendor read-only address or header gives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Endpoint {
    Normal,
    ReadOnly,
}

impl Endpoint {
    fn name(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::ReadOnly => "read_only",
        }
    }
}

/// What a list is against its pin: the list the connector was pinned to (made now when
/// there was none), or a different one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Pin {
    Same,
    Changed,
}

fn sqlite(_: rusqlite::Error) -> EngineError {
    EngineError("connectors_unavailable".into())
}

/// Writes a JSON value with every object's keys sorted, so the same value always gives
/// the same bytes whatever order a vendor sent its keys in. Inputs: the value and the
/// text to append to.
fn canonical(value: &serde_json::Value, out: &mut String) {
    use serde_json::Value;
    match value {
        Value::Object(fields) => {
            let mut keys: Vec<&String> = fields.keys().collect();
            keys.sort_unstable();
            out.push('{');
            for (at, key) in keys.into_iter().enumerate() {
                if at > 0 {
                    out.push(',');
                }
                out.push_str(&Value::String(key.clone()).to_string());
                out.push(':');
                canonical(&fields[key.as_str()], out);
            }
            out.push('}');
        }
        Value::Array(items) => {
            out.push('[');
            for (at, item) in items.iter().enumerate() {
                if at > 0 {
                    out.push(',');
                }
                canonical(item, out);
            }
            out.push(']');
        }
        other => out.push_str(&other.to_string()),
    }
}

/// The SHA-256 of a tool list in a form that does not depend on the order of its tools or
/// of any schema's keys: each tool's name, description, schema and both hints, sorted by
/// name. Input: the cleaned list. Output: the hex digest.
pub(crate) fn digest(tools: &[VendorTool]) -> String {
    let mut sorted: Vec<&VendorTool> = tools.iter().collect();
    sorted.sort_unstable_by(|left, right| left.name.cmp(&right.name));
    let entries: Vec<serde_json::Value> = sorted
        .into_iter()
        .map(|tool| {
            serde_json::json!({
                "name": tool.name,
                "description": tool.description,
                "inputSchema": tool.input_schema,
                "readOnlyHint": tool.read_only_hint,
                "destructiveHint": tool.destructive_hint,
            })
        })
        .collect();
    let mut text = String::new();
    canonical(&serde_json::Value::Array(entries), &mut text);
    let sha = crate::semantic::sha256_hex(text.as_bytes());
    assert_eq!(sha.len(), 64);
    sha
}

fn short_title(title: &str) -> String {
    let mut title = crate::memory::without_hidden(title).trim().to_owned();
    if title.len() > TITLE_BYTES_MAX {
        let mut end = TITLE_BYTES_MAX;
        while !title.is_char_boundary(end) {
            end -= 1;
        }
        title.truncate(end);
    }
    if title.is_empty() {
        title.push_str("A connector");
    }
    assert!(!title.is_empty() && title.len() <= TITLE_BYTES_MAX);
    title
}

/// Checks a tool list against the connector's pin for one endpoint. The first list read
/// for an endpoint becomes its pin; a different list later is kept for the owner to
/// review and stops the connector, and the pinned list again drops what was kept for that
/// endpoint. Inputs: the engine database, the connector's name and title, the endpoint
/// and the cleaned list. Output: how the list stands, or `connectors_unavailable`.
pub(crate) fn check(
    db: &Connection,
    connector: &str,
    title: &str,
    endpoint: Endpoint,
    tools: &[VendorTool],
) -> Result<Pin, EngineError> {
    db.execute_batch(SCHEMA).map_err(sqlite)?;
    let sha = digest(tools);
    let now = crate::semantic::now_ms();
    let tx = db.unchecked_transaction().map_err(sqlite)?;
    tx.execute(
        "INSERT INTO connector_tool_pins(connector, endpoint, sha256, pinned_ms)
         VALUES (?1, ?2, ?3, ?4) ON CONFLICT DO NOTHING",
        params![connector, endpoint.name(), sha, now],
    )
    .map_err(sqlite)?;
    let pinned: String = tx
        .query_row(
            "SELECT sha256 FROM connector_tool_pins WHERE connector=?1 AND endpoint=?2",
            params![connector, endpoint.name()],
            |row| row.get(0),
        )
        .map_err(sqlite)?;
    let pin = if pinned == sha {
        tx.execute(
            "DELETE FROM connector_tool_changes WHERE connector=?1 AND endpoint=?2",
            params![connector, endpoint.name()],
        )
        .map_err(sqlite)?;
        Pin::Same
    } else {
        tx.execute(
            "INSERT INTO connector_tool_changes(connector, endpoint, sha256, title, seen_ms)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(connector, endpoint) DO UPDATE SET
                 sha256=excluded.sha256, title=excluded.title, seen_ms=excluded.seen_ms",
            params![connector, endpoint.name(), sha, short_title(title), now],
        )
        .map_err(sqlite)?;
        Pin::Changed
    };
    tx.commit().map_err(sqlite)?;
    Ok(pin)
}

/// Whether a connector is stopped because one of its tool lists changed and the owner has
/// not accepted it. Inputs: the engine database and the connector. Output: the answer, or
/// `connectors_unavailable`.
pub fn stopped(db: &Connection, connector: &str) -> Result<bool, EngineError> {
    db.execute_batch(SCHEMA).map_err(sqlite)?;
    db.query_row(
        "SELECT EXISTS(SELECT 1 FROM connector_tool_changes WHERE connector=?1)",
        [connector],
        |row| row.get(0),
    )
    .map_err(sqlite)
}

/// Accepts a connector's changed tool lists as its new pins, which starts it again.
/// Inputs: the engine database and the connector. Output: whether anything was accepted,
/// or `connectors_unavailable`.
pub fn accept_tools(db: &Connection, connector: &str) -> Result<bool, EngineError> {
    db.execute_batch(SCHEMA).map_err(sqlite)?;
    let tx = db.unchecked_transaction().map_err(sqlite)?;
    let changed: Vec<(String, String)> = tx
        .prepare("SELECT endpoint, sha256 FROM connector_tool_changes WHERE connector=?1")
        .map_err(sqlite)?
        .query_map([connector], |row| Ok((row.get(0)?, row.get(1)?)))
        .map_err(sqlite)?
        .collect::<Result<_, _>>()
        .map_err(sqlite)?;
    let now = crate::semantic::now_ms();
    for (endpoint, sha) in &changed {
        tx.execute(
            "INSERT INTO connector_tool_pins(connector, endpoint, sha256, pinned_ms)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(connector, endpoint) DO UPDATE SET
                 sha256=excluded.sha256, pinned_ms=excluded.pinned_ms",
            params![connector, endpoint, sha, now],
        )
        .map_err(sqlite)?;
    }
    tx.execute(
        "DELETE FROM connector_tool_changes WHERE connector=?1",
        [connector],
    )
    .map_err(sqlite)?;
    tx.commit().map_err(sqlite)?;
    Ok(!changed.is_empty())
}

/// Deletes a connector's pins and kept changes: when it is removed or trusted again.
/// Inputs: the engine database and the connector. Output: Ok, or `connectors_unavailable`.
pub fn forget_connector(db: &Connection, connector: &str) -> Result<(), EngineError> {
    db.execute_batch(SCHEMA).map_err(sqlite)?;
    for table in ["connector_tool_pins", "connector_tool_changes"] {
        db.execute(
            &format!("DELETE FROM {table} WHERE connector=?1"),
            [connector],
        )
        .map_err(sqlite)?;
    }
    Ok(())
}

/// The connectors stopped by a changed tool list, each with the title kept for its
/// notice, by name. Inputs: the engine database and the most to read. Output: the rows,
/// none when the tables do not exist yet, or `connectors_unavailable`.
pub(crate) fn changed(
    db: &Connection,
    max: usize,
) -> Result<Option<Vec<(String, String)>>, EngineError> {
    let exists: bool = db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='connector_tool_changes')",
            [],
            |row| row.get(0),
        )
        .map_err(sqlite)?;
    if !exists {
        return Ok(None);
    }
    let rows: Vec<(String, String)> = db
        .prepare(
            "SELECT connector, MAX(title) FROM connector_tool_changes
             GROUP BY connector ORDER BY connector LIMIT ?1",
        )
        .map_err(sqlite)?
        .query_map([max as i64], |row| Ok((row.get(0)?, row.get(1)?)))
        .map_err(sqlite)?
        .collect::<Result<_, _>>()
        .map_err(sqlite)?;
    assert!(rows.len() <= max);
    Ok(Some(rows))
}
