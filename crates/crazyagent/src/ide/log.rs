use crate::engine::EngineError;
use crate::engine::owner::{HostStamp, failure};
use crate::state_log::fence::Fence;
use crate::state_log::replay::{self, Logged};
use crate::state_log::{Aggregate, Appender, BODY_BYTES_MAX, Chunk, Draft, SNAPSHOT_CHUNK};
use rusqlite::types::Value;
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[cfg(all(test, unix))]
mod tests;

/// The schema version every ide event body has (v2.5 L7).
pub const SCHEMA: u16 = 1;
/// The kind of an IDE ask the owner confirms in the desktop app.
pub const IDE_ASKED: &str = "IdeAsked";
/// The kind of a project's Run setting saved.
pub const RUN_MODE_SET: &str = "RunModeSet";
/// The kind of a file the owner unlocked in a running agent's worktree.
pub const FILE_UNLOCKED: &str = "FileUnlocked";
/// The kind of a project's workspace followed with its show-all switch.
pub const WORKSPACE_WRITTEN: &str = "WorkspaceWritten";
/// The kind of a workspace no longer followed, its project gone.
pub const WORKSPACE_FORGOTTEN: &str = "WorkspaceForgotten";
/// Every ide event kind, the list a project removal redacts that project's events by
/// (rv-sec2 #309 N1); a snapshot chunk is not one, since redact refuses a snapshot.
pub const KINDS: [&str; 5] = [
    IDE_ASKED,
    RUN_MODE_SET,
    FILE_UNLOCKED,
    WORKSPACE_WRITTEN,
    WORKSPACE_FORGOTTEN,
];
/// The most rows one ide table may hand a snapshot run.
pub const SNAPSHOT_ROWS_MAX: usize = 1 << 20;
/// The bytes a snapshot chunk keeps for its envelope (aggregate, table, seq) beside its rows.
const CHUNK_ENVELOPE_BYTES: usize = 1024;

const _: () = assert!(CHUNK_ENVELOPE_BYTES < BODY_BYTES_MAX);

/// The body of [`IDE_ASKED`]: the memory revision the IDE asked the owner to approve or
/// reject, and when, as `ide_asks` stores it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IdeAsked {
    pub project: String,
    pub entry: String,
    pub version: i64,
    pub decision: String,
    pub asked_ms: i64,
}

/// The body of [`RUN_MODE_SET`]: the project and its Run setting's stored name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunModeSet {
    pub project: String,
    pub mode: String,
}

/// The body of [`FILE_UNLOCKED`]: the project, the team agent, the path inside its worktree
/// and the agent's turn the unlock lasts for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileUnlocked {
    pub project: String,
    pub agent: i64,
    pub path: String,
    pub turns: i64,
}

/// The body of [`WORKSPACE_WRITTEN`]: the project and its show-all switch. The rendered
/// workspace stays out of the log: `ide_workspaces.body` is a cache of the file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceWritten {
    pub project: String,
    pub show_all: bool,
}

/// The body of [`WORKSPACE_FORGOTTEN`]: the project whose workspace is no longer followed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceForgotten {
    pub project: String,
}

/// One ide change as the log carries it, one variant per kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    Asked(IdeAsked),
    RunModeSet(RunModeSet),
    FileUnlocked(FileUnlocked),
    WorkspaceWritten(WorkspaceWritten),
    WorkspaceForgotten(WorkspaceForgotten),
}

impl Change {
    /// The event kind of the change.
    /// Input: none.
    /// Output: the kind name.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Asked(_) => IDE_ASKED,
            Self::RunModeSet(_) => RUN_MODE_SET,
            Self::FileUnlocked(_) => FILE_UNLOCKED,
            Self::WorkspaceWritten(_) => WORKSPACE_WRITTEN,
            Self::WorkspaceForgotten(_) => WORKSPACE_FORGOTTEN,
        }
    }

    /// The project the change is about, the top-level `project` of every ide body, so a
    /// redaction finds the events of one project by that key.
    /// Input: none. Output: the project id.
    pub fn project(&self) -> &str {
        match self {
            Self::Asked(asked) => &asked.project,
            Self::RunModeSet(set) => &set.project,
            Self::FileUnlocked(unlocked) => &unlocked.project,
            Self::WorkspaceWritten(written) => &written.project,
            Self::WorkspaceForgotten(forgotten) => &forgotten.project,
        }
    }

    /// The event body: the change's fields as one JSON object.
    /// Input: none.
    /// Output: the bytes, or `ide_log_body`.
    pub fn body(&self) -> Result<Vec<u8>, EngineError> {
        let body = match self {
            Self::Asked(asked) => serde_json::to_vec(asked),
            Self::RunModeSet(set) => serde_json::to_vec(set),
            Self::FileUnlocked(unlocked) => serde_json::to_vec(unlocked),
            Self::WorkspaceWritten(written) => serde_json::to_vec(written),
            Self::WorkspaceForgotten(forgotten) => serde_json::to_vec(forgotten),
        };
        body.map_err(|_| failure("ide_log_body"))
    }

    /// Reads a logged ide event back as its change: the aggregate must be ide, the schema
    /// [`SCHEMA`] and the body exactly the kind's fields.
    /// Input: the event. Output: the change, or `ide_log_event`.
    pub fn parse(event: &Logged) -> Result<Self, EngineError> {
        let refused = || failure("ide_log_event");
        if event.aggregate != Aggregate::Ide || event.schema != SCHEMA {
            return Err(refused());
        }
        let body = event.body.as_slice();
        let change = match event.kind.as_str() {
            IDE_ASKED => serde_json::from_slice(body).map(Self::Asked),
            RUN_MODE_SET => serde_json::from_slice(body).map(Self::RunModeSet),
            FILE_UNLOCKED => serde_json::from_slice(body).map(Self::FileUnlocked),
            WORKSPACE_WRITTEN => serde_json::from_slice(body).map(Self::WorkspaceWritten),
            WORKSPACE_FORGOTTEN => serde_json::from_slice(body).map(Self::WorkspaceForgotten),
            _ => return Err(refused()),
        };
        change.map_err(|_| refused())
    }
}

/// Opens the transaction one logged ide write runs in, taking the write lock at once, so
/// the revision it moves never meets another writer's.
/// Input: the connection. Output: the transaction, or `ide_log_unavailable`.
pub fn begin(db: &Connection) -> Result<Transaction<'_>, EngineError> {
    Transaction::new_unchecked(db, TransactionBehavior::Immediate).map_err(unavailable)
}

/// Logs one ide change in the caller's transaction: appends it at a revision of its own
/// behind the engine fence of `host`, then applies it through the projection, which checks
/// the fence again. The caller commits. Inputs: the transaction, the engine host, the time
/// and the change. Output: Ok, or the fence's, the log's or the projection's refusal.
pub fn record(
    tx: &Transaction<'_>,
    host: &HostStamp,
    at_ms: u64,
    change: &Change,
) -> Result<(), EngineError> {
    assert!(
        !change.project().is_empty(),
        "every ide event names its project"
    );
    let body = change.body()?;
    let mut appender = Appender::own(tx, Fence::Engine(host.clone()))?;
    let seq = appender.append(&Draft {
        aggregate: Aggregate::Ide,
        kind: change.kind(),
        schema: SCHEMA,
        at_ms,
        body: &body,
    })?;
    assert!(seq > 0);
    appender.project(|tx| apply(tx, change))
}

/// Applies one ide change to its table with the same statements live and in replay.
/// Inputs: the connection and the change.
/// Output: Ok, or the table's refusal.
pub fn apply(db: &Connection, change: &Change) -> Result<(), EngineError> {
    match change {
        Change::Asked(asked) => super::bridge::memory::apply_asked(db, asked),
        Change::RunModeSet(set) => super::bridge::run_command::apply_run_mode(db, set),
        Change::FileUnlocked(unlocked) => super::readonly::apply_unlocked(db, unlocked),
        Change::WorkspaceWritten(written) => super::workspace::apply_written(db, written),
        Change::WorkspaceForgotten(forgotten) => super::workspace::apply_forgotten(db, forgotten),
    }
}

/// The ide projection a replay runs: a change applies as it does live, and a snapshot run
/// replaces each table's rows with the rows its chunks carry. Fields: the seq of the last
/// chunk applied, and the tables the current run already emptied.
#[derive(Debug, Default)]
pub struct Projection {
    chunk_last: Option<u64>,
    emptied: BTreeSet<&'static str>,
}

impl Projection {
    /// Applies one replayed ide event. A chunk right after another chunk is the same run; any
    /// other chunk starts a run, which empties each table before its first chunk.
    /// Inputs: the connection and the event. Output: Ok, or the refusal.
    pub fn apply(&mut self, db: &Connection, event: &Logged) -> Result<(), EngineError> {
        if event.kind != SNAPSHOT_CHUNK {
            self.chunk_last = None;
            return apply(db, &Change::parse(event)?);
        }
        if self.chunk_last.and_then(|last| last.checked_add(1)) != Some(event.seq) {
            self.emptied.clear();
        }
        self.chunk_last = Some(event.seq);
        let chunk = replay::chunk(event)?;
        let table = Table::named(&chunk.table)?;
        if self.emptied.insert(table.name) {
            table.empty(db)?;
        }
        assert!(self.emptied.contains(table.name));
        table.insert(db, &chunk.rows)
    }
}

/// Rebuilds the four ide tables from the log: empties them, then replays every ide event
/// into a fresh [`Projection`]. `ide_workspaces.body` comes back empty, to be rebuilt by
/// the next write. Input: the connection. Output: the last seq read, or the refusal.
pub fn rebuild(db: &Connection) -> Result<u64, EngineError> {
    for table in &TABLES {
        table.empty(db)?;
    }
    let mut projection = Projection::default();
    replay::replay(db, Aggregate::Ide, 0, |event| projection.apply(db, event))
}

/// The chunks of an ide snapshot run: every row of the four tables in column order, at
/// least one chunk per table so a later run empties a table with no rows, each under the
/// body cap. `ide_workspaces` rows carry project and show_all only. A missing table gives
/// an empty chunk. Input: the connection. Output: the chunks, or `ide_snapshot_*`.
pub fn snapshot(db: &Connection) -> Result<Vec<Chunk>, EngineError> {
    let room = BODY_BYTES_MAX - CHUNK_ENVELOPE_BYTES;
    let mut chunks = Vec::with_capacity(TABLES.len());
    for table in &TABLES {
        let (mut rows, mut bytes) = (Vec::new(), 0);
        for row in table.rows(db)? {
            let size = serde_json::to_vec(&row).map_err(unavailable)?.len() + 1;
            if size > room {
                return Err(failure("ide_snapshot_row_bounds"));
            }
            if bytes + size > room {
                let full = std::mem::take(&mut rows);
                chunks.push(Chunk {
                    table: table.name.to_owned(),
                    rows: full,
                });
                bytes = 0;
            }
            bytes += size;
            rows.push(row);
        }
        assert!(bytes <= room);
        chunks.push(Chunk {
            table: table.name.to_owned(),
            rows,
        });
    }
    assert!(chunks.len() >= TABLES.len());
    Ok(chunks)
}

/// One ide table as the projection and the snapshot see it: its name, its schema, the
/// columns a snapshot row holds and the statement that puts one back.
struct Table {
    name: &'static str,
    schema: &'static str,
    columns: &'static [&'static str],
    insert: &'static str,
}

static TABLES: [Table; 4] = [
    Table {
        name: "ide_asks",
        schema: super::bridge::memory::ASKS_SCHEMA,
        columns: &["project", "entry", "version", "decision", "asked_ms"],
        insert: "INSERT INTO ide_asks(project,entry,version,decision,asked_ms)
            VALUES (?1,?2,?3,?4,?5)",
    },
    Table {
        name: "ide_run_settings",
        schema: super::bridge::run_command::SCHEMA,
        columns: &["project", "mode"],
        insert: "INSERT INTO ide_run_settings(project,mode) VALUES (?1,?2)",
    },
    Table {
        name: "ide_unlocked",
        schema: super::readonly::SCHEMA,
        columns: &["agent", "path", "turns"],
        insert: "INSERT INTO ide_unlocked(agent,path,turns) VALUES (?1,?2,?3)",
    },
    Table {
        name: "ide_workspaces",
        schema: super::workspace::SCHEMA,
        columns: &["project", "show_all"],
        insert: "INSERT INTO ide_workspaces(project,show_all,body) VALUES (?1,?2,'')",
    },
];

impl Table {
    /// The ide table of a snapshot chunk.
    /// Input: the chunk's table name.
    /// Output: the table, or `ide_log_table` for a name that is not one of the four.
    fn named(name: &str) -> Result<&'static Self, EngineError> {
        TABLES
            .iter()
            .find(|table| table.name == name)
            .ok_or_else(|| failure("ide_log_table"))
    }

    /// Creates the table when it is absent, then removes every row.
    /// Input: the connection.
    /// Output: Ok, or `ide_log_unavailable`.
    fn empty(&self, db: &Connection) -> Result<(), EngineError> {
        db.execute_batch(self.schema).map_err(unavailable)?;
        db.execute(&format!("DELETE FROM {}", self.name), [])
            .map_err(unavailable)?;
        Ok(())
    }

    /// Puts snapshot rows back, each an object holding exactly the table's columns, each a
    /// whole number or a text. Inputs: the connection and the rows.
    /// Output: Ok, or `ide_snapshot_row` for a row of another shape.
    fn insert(&self, db: &Connection, rows: &[serde_json::Value]) -> Result<(), EngineError> {
        db.execute_batch(self.schema).map_err(unavailable)?;
        let mut statement = db.prepare(self.insert).map_err(unavailable)?;
        let refused = || failure("ide_snapshot_row");
        for row in rows {
            let object = row
                .as_object()
                .filter(|object| object.len() == self.columns.len())
                .ok_or_else(refused)?;
            let values = self
                .columns
                .iter()
                .map(|column| object.get(*column).and_then(sql).ok_or_else(refused))
                .collect::<Result<Vec<Value>, _>>()?;
            assert_eq!(values.len(), self.columns.len());
            statement
                .execute(rusqlite::params_from_iter(values))
                .map_err(unavailable)?;
        }
        Ok(())
    }

    /// Every row of the table as objects of its snapshot columns, in column order, none for
    /// a table not created yet. Input: the connection.
    /// Output: the rows, or `ide_snapshot_rows` past [`SNAPSHOT_ROWS_MAX`].
    fn rows(&self, db: &Connection) -> Result<Vec<serde_json::Value>, EngineError> {
        let exists: bool = db
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
                [self.name],
                |row| row.get(0),
            )
            .map_err(unavailable)?;
        if !exists {
            return Ok(Vec::new());
        }
        let columns = self.columns.join(",");
        let mut statement = db
            .prepare(&format!(
                "SELECT {columns} FROM {} ORDER BY {columns} LIMIT ?1",
                self.name
            ))
            .map_err(unavailable)?;
        let read = statement
            .query_map([SNAPSHOT_ROWS_MAX as i64 + 1], |row| {
                (0..self.columns.len())
                    .map(|index| row.get::<_, Value>(index))
                    .collect::<rusqlite::Result<Vec<Value>>>()
            })
            .map_err(unavailable)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(unavailable)?;
        if read.len() > SNAPSHOT_ROWS_MAX {
            return Err(failure("ide_snapshot_rows"));
        }
        read.into_iter()
            .map(|values| {
                let fields =
                    self.columns.iter().zip(values).map(|(column, value)| {
                        json(value).map(|value| ((*column).to_owned(), value))
                    });
                fields
                    .collect::<Result<serde_json::Map<_, _>, _>>()
                    .map(serde_json::Value::Object)
            })
            .collect()
    }
}

/// A snapshot value as SQLite takes it back: a whole number or a text.
/// Input: the JSON value.
/// Output: the SQLite value, none for any other JSON.
fn sql(value: &serde_json::Value) -> Option<Value> {
    match value {
        serde_json::Value::Number(number) => number.as_i64().map(Value::Integer),
        serde_json::Value::String(text) => Some(Value::Text(text.clone())),
        _ => None,
    }
}

/// A stored value as a snapshot carries it: a whole number or a text; the ide tables hold
/// no other. Input: the SQLite value.
/// Output: the JSON value, or `ide_snapshot_value`.
fn json(value: Value) -> Result<serde_json::Value, EngineError> {
    match value {
        Value::Integer(number) => Ok(serde_json::Value::from(number)),
        Value::Text(text) => Ok(serde_json::Value::String(text)),
        _ => Err(failure("ide_snapshot_value")),
    }
}

/// The engine host `owner_meta` names as running, for a writer that holds no stamp of its
/// own (the team runtime's workspace follow). Input: the connection.
/// Output: the host, none when no host runs, or `ide_log_unavailable`.
pub fn active_host(db: &Connection) -> Result<Option<HostStamp>, EngineError> {
    db.query_row(
        "SELECT db_instance,engine_epoch FROM owner_meta
         WHERE singleton=1 AND schema_version=1 AND runtime_state='active'",
        [],
        |row| {
            Ok(HostStamp {
                db_instance: row.get(0)?,
                engine_epoch: row.get(1)?,
            })
        },
    )
    .optional()
    .map_err(unavailable)
}

fn unavailable<E>(_: E) -> EngineError {
    failure("ide_log_unavailable")
}

/// Seeds a test store for logged ide writes: the `state` row, the `events` table and, for a
/// store with no owner tables yet, the test host active in them and started in this
/// process. A store whose engine already started keeps its own host. Input: the store.
/// Output: the running host to write with.
#[cfg(test)]
pub(crate) fn seed(db: &Connection) -> HostStamp {
    use crate::state_log::fence;
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS state
            (id INTEGER PRIMARY KEY CHECK(id=1), revision INTEGER NOT NULL);
         INSERT OR IGNORE INTO state VALUES (1,0);
         CREATE TABLE IF NOT EXISTS events (cursor INTEGER PRIMARY KEY, body TEXT NOT NULL);",
    )
    .unwrap();
    let owned: bool = db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='owner_meta')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    if !owned {
        fence::seed(db);
    }
    let host = active_host(db)
        .unwrap()
        .expect("the seeded store has a running host");
    assert!(
        fence::hosted(&host),
        "the store's host runs in this process"
    );
    host
}
