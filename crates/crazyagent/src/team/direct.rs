use super::{AgentRole, DIRECT_INPUT_BYTES_MAX, DirectInput, bound_text, error};
use crate::engine::EngineError;
use crate::protocol::ProjectId;
use rusqlite::{Connection, OptionalExtension, params};

pub const PENDING_PER_PROJECT_MAX: i64 = 200;
pub const NOTES_PER_TURN_MAX: i64 = 20;
pub const NOTE_TEXT_BYTES_MAX: usize = 300;

/// Erases what older engines kept of the keys the owner typed into sub-agent terminals,
/// overwriting the freed pages and checkpointing the write-ahead log so the text does not
/// stay in the database files. Takes the engine connection. Returns any SQLite failure.
pub fn erase_typed_input(connection: &Connection) -> Result<(), EngineError> {
    let kept: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE name='team_direct_inputs'",
            [],
            |row| row.get(0),
        )
        .map_err(error)?;
    if kept == 0 {
        return Ok(());
    }
    let secure: i64 = connection
        .query_row("PRAGMA secure_delete", [], |row| row.get(0))
        .map_err(error)?;
    connection
        .execute_batch("PRAGMA secure_delete=ON; DROP TABLE team_direct_inputs;")
        .map_err(error)?;
    connection
        .execute_batch(&format!("PRAGMA secure_delete={secure};"))
        .map_err(error)?;
    connection
        .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))
        .map_err(error)
}

/// Records one prompt the owner sent to a team sub-agent with an explicit send, on the
/// sub-agent's current task, for the master's next turn. Prompts to a master need no note.
/// Inputs: the engine connection, the team agent id, the prompt and the time. Output: the
/// recorded prompt, or none for a master, an unknown agent, a blank prompt or a full queue.
pub fn record(
    connection: &Connection,
    agent: i64,
    text: &str,
    at_ms: i64,
) -> Result<Option<DirectInput>, EngineError> {
    let text = text.trim();
    if text.is_empty() || text.len() > DIRECT_INPUT_BYTES_MAX {
        return Ok(None);
    }
    let row: Option<(String, String, String, Option<i64>)> = connection
        .query_row(
            "SELECT project,role,harness,task FROM team_agents WHERE id=?1",
            [agent],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()
        .map_err(error)?;
    let Some((project, role, harness, task)) = row else {
        return Ok(None);
    };
    if AgentRole::parse(&role) != Some(AgentRole::Sub) {
        return Ok(None);
    }
    let Ok(project) = ProjectId::new(project) else {
        return Ok(None);
    };
    let pending: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM team_owner_prompts WHERE project=?1",
            [project.as_str()],
            |row| row.get(0),
        )
        .map_err(error)?;
    if pending >= PENDING_PER_PROJECT_MAX {
        return Ok(None);
    }
    connection
        .execute(
            "INSERT INTO team_owner_prompts(project,agent,task,harness,text,created_ms) VALUES (?1,?2,?3,?4,?5,?6)",
            params![project.as_str(), agent, task, harness, text, at_ms],
        )
        .map_err(error)?;
    Ok(Some(DirectInput {
        project,
        agent,
        text: text.to_owned(),
        at_ms,
    }))
}

/// Builds the note the master reads before its next message: one line per prompt the owner
/// sent to a sub-agent since the master's last turn, oldest first, at most
/// `NOTES_PER_TURN_MAX`. Inputs: the connection and the project. Output: the note, empty
/// when nothing was sent, and the id of the newest prompt it covers for `delivered`.
pub fn notes(
    connection: &Connection,
    project: &ProjectId,
) -> Result<(String, Option<i64>), EngineError> {
    let mut statement = connection
        .prepare(
            "SELECT id,agent,task,harness,text FROM team_owner_prompts WHERE project=?1 ORDER BY id LIMIT ?2",
        )
        .map_err(error)?;
    let rows: Vec<(i64, i64, Option<i64>, String, String)> = statement
        .query_map(params![project.as_str(), NOTES_PER_TURN_MAX], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        })
        .map_err(error)?
        .collect::<Result<_, _>>()
        .map_err(error)?;
    let Some(newest) = rows.last().map(|row| row.0) else {
        return Ok((String::new(), None));
    };
    let mut note =
        String::from("The owner sent these prompts straight to sub-agents since your last turn:\n");
    for (_, agent, task, harness, text) in &rows {
        let task = task.map_or_else(String::new, |task| format!(" on task {task}"));
        note.push_str(&format!(
            "- Owner told {harness}-{agent}{task}: {}\n",
            bound_text(text, NOTE_TEXT_BYTES_MAX)
        ));
    }
    note.push('\n');
    assert!(rows.len() as i64 <= NOTES_PER_TURN_MAX);
    Ok((note, Some(newest)))
}

/// Deletes every prompt up to `newest` once the master's turn that heard them was sent.
/// Inputs: the connection, the project and the newest prompt id `notes` covered.
pub fn delivered(
    connection: &Connection,
    project: &ProjectId,
    newest: i64,
) -> Result<(), EngineError> {
    connection
        .execute(
            "DELETE FROM team_owner_prompts WHERE project=?1 AND id<=?2",
            params![project.as_str(), newest],
        )
        .map_err(error)?;
    Ok(())
}

#[cfg(test)]
mod tests;
