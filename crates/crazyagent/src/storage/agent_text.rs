use crate::agent_home::TEXT_BYTES_MAX;
use crate::engine::EngineError;
use crate::protocol::ProjectId;
use crate::storage::versioned::{self, Refusal, Writer};
use rusqlite::{OptionalExtension, Transaction, params};

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS agent_texts (
    agent INTEGER PRIMARY KEY CHECK (agent > 0),
    project TEXT NOT NULL,
    text TEXT NOT NULL CHECK (length(CAST(text AS BLOB)) <= 65536)
);
CREATE INDEX IF NOT EXISTS agent_texts_project ON agent_texts(project, agent);
";
const _: () = assert!(TEXT_BYTES_MAX == 65536);

/// Reads the owner's own instructions for one team agent and their version: empty at
/// version 0 when none were saved. Inputs: a transaction on `engine.db`, the agent's
/// project and its id, above 0. Output: the text and version, or `versioned_unavailable`.
pub fn read(
    tx: &Transaction<'_>,
    project: &ProjectId,
    agent: i64,
) -> Result<(String, u64), EngineError> {
    let item = versioned::agent_instructions_item(project, agent);
    let version = versioned::current(tx, &item)?.version;
    let text = match exists(tx)? {
        true => tx
            .query_row(
                "SELECT text FROM agent_texts WHERE agent = ?1 AND project = ?2",
                params![agent, project.as_str()],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|_| unavailable())?
            .unwrap_or_default(),
        false => String::new(),
    };
    assert!(version > 0 || text.is_empty());
    assert!(text.len() <= TEXT_BYTES_MAX);
    Ok((text, version))
}

/// Saves the owner's own instructions for one team agent as their next version when the
/// write names the current one; a stale write changes nothing. The caller checks that the
/// agent belongs to the project and writes in an immediate transaction. Inputs: the
/// transaction, the project, the agent id, the text (at most `TEXT_BYTES_MAX` bytes, no
/// NUL), the version read and the writer. Output: the new version, or the refusal.
pub fn save(
    tx: &Transaction<'_>,
    project: &ProjectId,
    agent: i64,
    text: &str,
    expected: u64,
    writer: &Writer,
) -> Result<u64, Refusal> {
    crate::agent_home::validate(text).map_err(|problem| Refusal::Failed(EngineError(problem.0)))?;
    let item = versioned::agent_instructions_item(project, agent);
    let version = versioned::write_if(tx, &item, expected, writer)?;
    ensure(tx).map_err(Refusal::Failed)?;
    tx.execute(
        "INSERT INTO agent_texts(agent, project, text) VALUES (?1, ?2, ?3)
         ON CONFLICT(agent) DO UPDATE SET text = excluded.text
         WHERE agent_texts.project = excluded.project",
        params![agent, project.as_str(), text],
    )
    .map_err(|_| Refusal::Failed(unavailable()))?;
    assert_eq!(
        read(tx, project, agent).map_err(Refusal::Failed)?,
        (text.to_owned(), version)
    );
    Ok(version)
}

/// Drops the owner's own instructions of every agent of one project, with their versions,
/// when the project is removed. Inputs: a transaction on `engine.db` and the project.
/// Output: unit, or `versioned_unavailable`.
pub fn forget_project(tx: &Transaction<'_>, project: &ProjectId) -> Result<(), EngineError> {
    if !exists(tx)? {
        return Ok(());
    }
    let agents = {
        let mut statement = tx
            .prepare("SELECT agent FROM agent_texts WHERE project = ?1")
            .map_err(|_| unavailable())?;
        let rows = statement
            .query_map([project.as_str()], |row| row.get::<_, i64>(0))
            .map_err(|_| unavailable())?;
        rows.collect::<Result<Vec<i64>, _>>()
            .map_err(|_| unavailable())?
    };
    for agent in &agents {
        versioned::forget(tx, &versioned::agent_instructions_item(project, *agent))?;
    }
    let removed = tx
        .execute(
            "DELETE FROM agent_texts WHERE project = ?1",
            [project.as_str()],
        )
        .map_err(|_| unavailable())?;
    assert_eq!(removed, agents.len());
    Ok(())
}

fn ensure(tx: &Transaction<'_>) -> Result<(), EngineError> {
    if exists(tx)? {
        return Ok(());
    }
    tx.execute_batch(SCHEMA).map_err(|_| unavailable())
}

fn exists(tx: &Transaction<'_>) -> Result<bool, EngineError> {
    tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'agent_texts')",
        [],
        |row| row.get::<_, bool>(0),
    )
    .map_err(|_| unavailable())
}

fn unavailable() -> EngineError {
    EngineError("versioned_unavailable".into())
}
