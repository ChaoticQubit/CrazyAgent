use kernel_sandbox::policy::AgentPolicy;
use rusqlite::{Connection, OptionalExtension, params};

const TABLE: &str = "CREATE TABLE IF NOT EXISTS project_agent_policy (
    project TEXT PRIMARY KEY,
    body TEXT NOT NULL
)";

const BODY_BYTES_MAX: usize = 16 * 1024;

/// Whether a project's agent policy was ever saved. Inputs: the open connection or
/// transaction and the project id. Output: the answer, or the database error.
pub fn saved(connection: &Connection, project: &str) -> rusqlite::Result<bool> {
    connection.execute(TABLE, [])?;
    connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM project_agent_policy WHERE project=?1)",
        [project],
        |row| row.get(0),
    )
}

/// Replaces a project's agent policy. Inputs: the open connection or transaction, the
/// project id and a validated policy. Output: done, or the database error.
pub fn save(connection: &Connection, project: &str, policy: &AgentPolicy) -> rusqlite::Result<()> {
    assert!(policy.validate().is_ok());
    let body = serde_json::to_string(policy)
        .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
    assert!(body.len() <= BODY_BYTES_MAX);
    connection.execute(TABLE, [])?;
    connection.execute(
        "INSERT INTO project_agent_policy (project, body) VALUES (?1, ?2)
         ON CONFLICT(project) DO UPDATE SET body=excluded.body",
        params![project, body],
    )?;
    Ok(())
}

/// Reads a project's agent policy; the default (Sandboxed, no toolchain choice) when none
/// was ever saved. Inputs: the open connection or transaction and the project id. Output:
/// the policy, or the database error for an unreadable row.
pub fn load(connection: &Connection, project: &str) -> rusqlite::Result<AgentPolicy> {
    let present: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name='project_agent_policy')",
        [],
        |row| row.get(0),
    )?;
    if !present {
        return Ok(AgentPolicy::default());
    }
    let body: Option<String> = connection
        .query_row(
            "SELECT body FROM project_agent_policy WHERE project=?1",
            [project],
            |row| row.get(0),
        )
        .optional()?;
    let Some(body) = body else {
        return Ok(AgentPolicy::default());
    };
    let unreadable = |error: Box<dyn std::error::Error + Send + Sync>| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, error)
    };
    if body.len() > BODY_BYTES_MAX {
        return Err(unreadable("agent policy too long".into()));
    }
    let policy: AgentPolicy =
        serde_json::from_str(&body).map_err(|error| unreadable(Box::new(error)))?;
    policy.validate().map_err(|code| unreadable(code.into()))?;
    Ok(policy)
}

/// Deletes a project's agent policy when the project is removed. Inputs: the open
/// connection or transaction and the project id. Output: done, or the database error.
pub fn remove(connection: &Connection, project: &str) -> rusqlite::Result<()> {
    connection.execute(TABLE, [])?;
    connection.execute(
        "DELETE FROM project_agent_policy WHERE project=?1",
        [project],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests;
