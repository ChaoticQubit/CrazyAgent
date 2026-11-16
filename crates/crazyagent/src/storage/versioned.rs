use crate::engine::EngineError;
use crate::protocol::ProjectId;
use crate::terminal::AgentId;
use rusqlite::{Connection, OpenFlags, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::time::Duration;

#[cfg(test)]
mod tests;

/// The longest item name a versioned write accepts, in bytes.
pub const ITEM_BYTES_MAX: usize = 256;
/// How many owner force records one item keeps; older ones are dropped first.
pub const FORCES_PER_ITEM_MAX: usize = 100;
const VERSION_MAX: u64 = i64::MAX as u64;
const BUSY_WAIT: Duration = Duration::from_secs(5);
const OWNER: &str = "owner";
const AGENT_PREFIX: &str = "agent:";
const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS versioned_items (
    item TEXT PRIMARY KEY NOT NULL,
    version INTEGER NOT NULL CHECK (version >= 1),
    writer TEXT NOT NULL,
    written_ms INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS versioned_forces (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    item TEXT NOT NULL,
    replaced_version INTEGER NOT NULL,
    replaced_writer TEXT NOT NULL,
    version INTEGER NOT NULL,
    at_ms INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS versioned_forces_item ON versioned_forces(item, id);
";

/// Names one shared item whose writes are versioned, such as `process:app` or
/// `memory:<project>:<entry>`: 1 to `ITEM_BYTES_MAX` printable ASCII bytes, no spaces.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Item(String);

impl Item {
    /// Checks an item name. Input: the name. Output: the item, or `versioned_item_invalid`
    /// for an empty, long, spaced or non-ASCII name.
    pub fn new(name: &str) -> Result<Self, EngineError> {
        let valid = !name.is_empty()
            && name.len() <= ITEM_BYTES_MAX
            && name.bytes().all(|byte| byte.is_ascii_graphic());
        if !valid {
            return Err(failure("versioned_item_invalid"));
        }
        Ok(Self(name.to_owned()))
    }

    /// Gives the item name as text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Names the versioned item of the owner's app-wide instructions, or of one project's.
/// Input: the project, none for the app. Output: the item.
pub fn process_text_item(project: Option<&ProjectId>) -> Item {
    named(match project {
        None => "process:app".to_owned(),
        Some(project) => format!("process:project:{}", project.as_str()),
    })
}

/// Names the versioned item of the owner's own instructions for one team agent.
/// Inputs: the agent's project and its id, above 0. Output: the item.
pub fn agent_instructions_item(project: &ProjectId, agent: i64) -> Item {
    assert!(agent > 0);
    named(format!(
        "agent-instructions:{}:team-{agent}",
        project.as_str()
    ))
}

/// Names the versioned item of one project's extra read-only folders. Input: the project.
/// Output: the item.
pub fn read_paths_item(project: &ProjectId) -> Item {
    named(format!("read-paths:{}", project.as_str()))
}

/// Names the versioned item of the folders one project's agents are kept out of. Input: the
/// project. Output: the item.
pub fn hidden_paths_item(project: &ProjectId) -> Item {
    named(format!("hidden-paths:{}", project.as_str()))
}

/// Names the versioned item of one project's agent instruction settings: which instruction
/// files its agents get and whether headless Claude runs keep their transcript. Input: the
/// project. Output: the item.
pub fn instruction_settings_item(project: &ProjectId) -> Item {
    named(format!("instruction-settings:{}", project.as_str()))
}

/// Names the versioned item of one project's agent policy: its access level and toolchain
/// switches. Input: the project. Output: the item.
pub fn agent_policy_item(project: &ProjectId) -> Item {
    named(format!("agent-policy:{}", project.as_str()))
}

/// Names the versioned item of one project's team settings: keep warm, the task and session
/// time limits with the idle stop, the On harness switch, the live master switch and where
/// finished agents show. Input: the project. Output: the item.
pub fn team_settings_item(project: &ProjectId) -> Item {
    named(format!("team-settings:{}", project.as_str()))
}

/// Names the versioned item of one wake-rule level. Inputs: the project, none for the app
/// level, and the sub-agent id, 0 for the app or project level; team agent ids are never
/// reused, so an agent's level needs no project. Output: the item.
pub fn team_rule_item(project: Option<&ProjectId>, agent: i64) -> Item {
    assert!(agent >= 0);
    assert!(agent == 0 || project.is_some());
    named(match (project, agent) {
        (None, _) => "team-rule:app".to_owned(),
        (Some(project), 0) => format!("team-rule:project:{}", project.as_str()),
        (Some(_), agent) => format!("team-rule:agent:team-{agent}"),
    })
}

/// Names the versioned item of the role the master gave one team task; task ids are never
/// reused. Input: the task id, above 0. Output: the item.
pub fn task_role_item(task: i64) -> Item {
    assert!(task > 0);
    named(format!("task-role:task-{task}"))
}

/// Names the versioned item of one skill or MCP server: its On switch, its project switches
/// and, for a server, its command. Input: the plugin id. Output: the item, or
/// `versioned_item_invalid` for an id that cannot name one.
pub fn plugin_item(id: &str) -> Result<Item, EngineError> {
    Item::new(&format!("plugin:{id}"))
}

/// Names the versioned item of the default pack's On switches and installed entries.
pub fn default_pack_item() -> Item {
    named("default-pack".to_owned())
}

/// Names the versioned item of the owner's account scheduling choices.
pub fn account_schedule_item() -> Item {
    named("account-schedule".to_owned())
}

fn named(name: String) -> Item {
    assert!(Item::new(&name).is_ok());
    Item(name)
}

/// Who asks to write a shared item. The owner may keep their own text after being shown a
/// conflict (`force_after_conflict`), which is recorded; an agent has no way to force.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Writer {
    Owner { force_after_conflict: bool },
    Agent(AgentId),
}

/// Who wrote an item's current version: nobody yet, the owner, or one agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LastWriter {
    Nobody,
    Owner,
    Agent(AgentId),
}

/// An item's current version and who wrote it; version 0 means nobody wrote it yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Current {
    pub version: u64,
    pub writer: LastWriter,
}

/// The reply to a write that named a version other than the item's current one: the version
/// the write named, the current version to re-read, and who wrote it. The item's value is read
/// by the caller from the item's own store in the same transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stale {
    pub version: u64,
    pub current: u64,
    pub writer: LastWriter,
}

/// Why a versioned write did not land: it was stale, or the store could not be used
/// (`versioned_unavailable`, also when another connection holds the write lock).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    Stale(Stale),
    Failed(EngineError),
}

/// One recorded owner "Keep mine": the version it replaced, who wrote that version, the
/// version the owner's write became and when.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Force {
    pub replaced_version: u64,
    pub replaced_writer: LastWriter,
    pub version: u64,
    pub at_ms: i64,
}

/// The plain refusal of a stale owner write to a structured item. Every client already
/// shows it as "this changed since you opened it".
pub const STALE_CODE: &str = "stale_revision";

/// Writes the next version of an item only the owner edits, such as a toggle or a schedule,
/// in the caller's transaction before the caller writes the value. Inputs: an immediate
/// transaction on `engine.db`, the item and the version the owner's screen showed.
/// Output: the new version, the stale refusal, or `versioned_unavailable`.
pub fn owner_write(
    tx: &Transaction<'_>,
    item: &Item,
    expected: u64,
) -> Result<Result<u64, Stale>, EngineError> {
    let owner = Writer::Owner {
        force_after_conflict: false,
    };
    match write_if(tx, item, expected, &owner) {
        Ok(version) => {
            assert_eq!(version, expected + 1);
            Ok(Ok(version))
        }
        Err(Refusal::Stale(stale)) => Ok(Err(stale)),
        Err(Refusal::Failed(failed)) => Err(failed),
    }
}

/// Writes one shared item's next version when the writer read the current one, so no write
/// silently replaces a version its writer never saw. A stale write is refused with the current
/// version and its writer. An owner's write with `force_after_conflict` is recorded as the
/// owner keeping their text over the version they were shown; it is refused like any other
/// write when a newer version landed since. Inputs: a transaction on `engine.db` (immediate,
/// so racing writers queue), the item, the version the writer read (0 for an item never
/// written) and the writer. Output: the new version, or the refusal.
pub fn write_if(
    tx: &Transaction<'_>,
    item: &Item,
    expected: u64,
    writer: &Writer,
) -> Result<u64, Refusal> {
    ensure(tx).map_err(Refusal::Failed)?;
    let now = current(tx, item).map_err(Refusal::Failed)?;
    if now.version != expected {
        return Err(Refusal::Stale(Stale {
            version: expected,
            current: now.version,
            writer: now.writer,
        }));
    }
    if expected >= VERSION_MAX {
        return Err(Refusal::Failed(failure("versioned_unavailable")));
    }
    let version = expected + 1;
    let at_ms = crate::team::now_ms();
    let stored = writer_text(writer);
    let changed = match expected {
        0 => tx.execute(
            "INSERT INTO versioned_items(item, version, writer, written_ms) VALUES (?1, 1, ?2, ?3)
             ON CONFLICT(item) DO NOTHING",
            params![item.as_str(), stored, at_ms],
        ),
        _ => tx.execute(
            "UPDATE versioned_items SET version = ?3, writer = ?4, written_ms = ?5
             WHERE item = ?1 AND version = ?2",
            params![
                item.as_str(),
                to_column(expected),
                to_column(version),
                stored,
                at_ms
            ],
        ),
    }
    .map_err(|_| Refusal::Failed(failure("versioned_unavailable")))?;
    if changed != 1 {
        let now = current(tx, item).map_err(Refusal::Failed)?;
        assert_ne!(now.version, expected);
        return Err(Refusal::Stale(Stale {
            version: expected,
            current: now.version,
            writer: now.writer,
        }));
    }
    if matches!(
        writer,
        Writer::Owner {
            force_after_conflict: true
        }
    ) {
        record_force(tx, item, &now, version, at_ms).map_err(Refusal::Failed)?;
    }
    assert_eq!(current(tx, item).map_err(Refusal::Failed)?.version, version);
    Ok(version)
}

/// Reads one shared item's current version and its writer. Input: a connection or
/// transaction on `engine.db` and the item. Output: the version (0 for an item never
/// written), or `versioned_unavailable`.
pub fn current(tx: &Connection, item: &Item) -> Result<Current, EngineError> {
    if !exists(tx)? {
        return Ok(Current {
            version: 0,
            writer: LastWriter::Nobody,
        });
    }
    let row = tx
        .query_row(
            "SELECT version, writer FROM versioned_items WHERE item = ?1",
            [item.as_str()],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()
        .map_err(|_| failure("versioned_unavailable"))?;
    let Some((version, writer)) = row else {
        return Ok(Current {
            version: 0,
            writer: LastWriter::Nobody,
        });
    };
    let version = from_column(version)?;
    assert!(version >= 1);
    Ok(Current {
        version,
        writer: parse_writer(&writer)?,
    })
}

/// Reads the recorded owner "Keep mine" writes of one item, oldest first, at most
/// `FORCES_PER_ITEM_MAX`. Input: a transaction on `engine.db` and the item. Output: the
/// records, or `versioned_unavailable`.
pub fn forces(tx: &Transaction<'_>, item: &Item) -> Result<Vec<Force>, EngineError> {
    if !exists(tx)? {
        return Ok(Vec::new());
    }
    let mut statement = tx
        .prepare(
            "SELECT replaced_version, replaced_writer, version, at_ms FROM versioned_forces
             WHERE item = ?1 ORDER BY id",
        )
        .map_err(|_| failure("versioned_unavailable"))?;
    let rows = statement
        .query_map([item.as_str()], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
            ))
        })
        .map_err(|_| failure("versioned_unavailable"))?;
    let mut records = Vec::new();
    for row in rows {
        let (replaced_version, replaced_writer, version, at_ms) =
            row.map_err(|_| failure("versioned_unavailable"))?;
        records.push(Force {
            replaced_version: from_column(replaced_version)?,
            replaced_writer: parse_writer(&replaced_writer)?,
            version: from_column(version)?,
            at_ms,
        });
    }
    assert!(records.len() <= FORCES_PER_ITEM_MAX);
    Ok(records)
}

/// Drops one shared item's version and its owner "Keep mine" records, for an item deleted
/// together with its value in the same transaction; a later write starts again at version 0.
/// Inputs: a transaction on `engine.db` and the item. Output: whether the item had a version.
pub fn forget(tx: &Transaction<'_>, item: &Item) -> Result<bool, EngineError> {
    if !exists(tx)? {
        return Ok(false);
    }
    let removed = tx
        .execute(
            "DELETE FROM versioned_items WHERE item = ?1",
            [item.as_str()],
        )
        .map_err(|_| failure("versioned_unavailable"))?;
    tx.execute(
        "DELETE FROM versioned_forces WHERE item = ?1",
        [item.as_str()],
    )
    .map_err(|_| failure("versioned_unavailable"))?;
    assert!(removed <= 1);
    assert_eq!(current(tx, item)?.version, 0);
    Ok(removed == 1)
}

/// Moves one owner item's version past every version a screen could have read, for an item
/// whose value goes away while its name can come back, such as a removed project's settings
/// when a project is added again under the same id. A screen read before is then refused as
/// stale instead of saving onto the new value. Inputs: an immediate transaction on
/// `engine.db` and the item. Output: the new version, or `versioned_unavailable`.
pub fn retire(tx: &Transaction<'_>, item: &Item) -> Result<u64, EngineError> {
    let read = current(tx, item)?.version;
    let moved = owner_write(tx, item, read)?;
    assert!(moved.is_ok(), "no other write lands inside one transaction");
    assert_eq!(current(tx, item)?.version, read + 1);
    Ok(read + 1)
}

/// Opens the engine database for versioned writes without ever creating it, waiting up to
/// five seconds behind another writer. Callers then write in an immediate transaction so a
/// racing writer is told to re-read. Input: the absolute engine root holding `engine.db`.
/// Output: the connection, or `versioned_unavailable`.
pub fn open(root: &Path) -> Result<Connection, EngineError> {
    if !root.is_absolute() {
        return Err(failure("versioned_unavailable"));
    }
    let connection = Connection::open_with_flags(
        root.join("engine.db"),
        OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )
    .map_err(|_| failure("versioned_unavailable"))?;
    connection
        .busy_timeout(BUSY_WAIT)
        .map_err(|_| failure("versioned_unavailable"))?;
    Ok(connection)
}

fn record_force(
    tx: &Transaction<'_>,
    item: &Item,
    replaced: &Current,
    version: u64,
    at_ms: i64,
) -> Result<(), EngineError> {
    assert_eq!(replaced.version + 1, version);
    tx.execute(
        "INSERT INTO versioned_forces(item, replaced_version, replaced_writer, version, at_ms)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            item.as_str(),
            to_column(replaced.version),
            last_writer_text(&replaced.writer),
            to_column(version),
            at_ms
        ],
    )
    .map_err(|_| failure("versioned_unavailable"))?;
    tx.execute(
        "DELETE FROM versioned_forces WHERE item = ?1 AND id NOT IN
         (SELECT id FROM versioned_forces WHERE item = ?1 ORDER BY id DESC LIMIT ?2)",
        params![item.as_str(), to_column(FORCES_PER_ITEM_MAX as u64)],
    )
    .map_err(|_| failure("versioned_unavailable"))?;
    Ok(())
}

fn ensure(tx: &Transaction<'_>) -> Result<(), EngineError> {
    if exists(tx)? {
        return Ok(());
    }
    tx.execute_batch(SCHEMA)
        .map_err(|_| failure("versioned_unavailable"))
}

fn exists(tx: &Connection) -> Result<bool, EngineError> {
    tx.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table'
         AND name IN ('versioned_items', 'versioned_forces')",
        [],
        |row| row.get::<_, i64>(0),
    )
    .map(|tables| tables == 2)
    .map_err(|_| failure("versioned_unavailable"))
}

fn writer_text(writer: &Writer) -> String {
    match writer {
        Writer::Owner { .. } => OWNER.to_owned(),
        Writer::Agent(agent) => format!("{AGENT_PREFIX}{}", agent.as_str()),
    }
}

fn last_writer_text(writer: &LastWriter) -> String {
    match writer {
        LastWriter::Nobody => String::new(),
        LastWriter::Owner => OWNER.to_owned(),
        LastWriter::Agent(agent) => format!("{AGENT_PREFIX}{}", agent.as_str()),
    }
}

fn parse_writer(text: &str) -> Result<LastWriter, EngineError> {
    if text.is_empty() {
        return Ok(LastWriter::Nobody);
    }
    if text == OWNER {
        return Ok(LastWriter::Owner);
    }
    text.strip_prefix(AGENT_PREFIX)
        .and_then(|agent| AgentId::new(agent).ok())
        .map(LastWriter::Agent)
        .ok_or_else(|| failure("versioned_unavailable"))
}

fn to_column(value: u64) -> i64 {
    assert!(value <= VERSION_MAX);
    i64::try_from(value).unwrap_or(i64::MAX)
}

fn from_column(value: i64) -> Result<u64, EngineError> {
    u64::try_from(value).map_err(|_| failure("versioned_unavailable"))
}

fn failure(code: &str) -> EngineError {
    EngineError(code.to_owned())
}
