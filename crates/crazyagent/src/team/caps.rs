use super::{AgentRole, CAP_CHOICES, error};
use crate::coord::AgentId;
use crate::engine::EngineError;
use crate::protocol::ProjectId;
use rusqlite::{Connection, OptionalExtension, params};

/// A project's limits on the agents it runs at once: harnesses, the sub-agents the master
/// starts, and sub-agents each harness leads, app-spawned and native teammates alike.
/// None is No limit.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Caps {
    pub harnesses: Option<u8>,
    pub teammates: Option<u8>,
}

/// Reads the project's limits on running agents, No limit for each the owner left unset.
/// Takes a connection and the project. Returns the limits, or a SQLite failure.
pub fn read(connection: &Connection, project: &ProjectId) -> Result<Caps, EngineError> {
    let row: Option<(Option<i64>, Option<i64>)> = connection
        .query_row(
            "SELECT harnesses,teammates FROM team_caps WHERE project=?1",
            [project.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(error)?;
    let Some((harnesses, teammates)) = row else {
        return Ok(Caps::default());
    };
    let cap = |value: Option<i64>| -> Result<Option<u8>, EngineError> {
        value
            .map(|value| {
                u8::try_from(value)
                    .ok()
                    .filter(|value| (1..=CAP_CHOICES[8]).contains(value))
                    .ok_or_else(|| error("team_cap_invalid"))
            })
            .transpose()
    };
    Ok(Caps {
        harnesses: cap(harnesses)?,
        teammates: cap(teammates)?,
    })
}

/// Counts the sub-agents a lead runs now in both modes (DEC-104): its app-spawned sub-agents,
/// plus its native teammates, counted as their own agent rows or as the teammate panes the
/// engine saw in its terminal, whichever is more, so one teammate never counts twice.
/// Takes a connection, the project and the lead. Returns the count, or a SQLite failure.
pub fn running_sub_agents(
    connection: &Connection,
    project: &ProjectId,
    lead: AgentId,
) -> Result<u32, EngineError> {
    let (spawned, native, panes): (i64, i64, i64) = connection
        .query_row(
            "SELECT
                 (SELECT COUNT(*) FROM team_agents WHERE project=?1 AND parent=?2 AND id<>?2
                  AND role=?3 AND status IN ('starting','idle','working')),
                 (SELECT COUNT(*) FROM team_agents WHERE project=?1 AND parent=?2 AND id<>?2
                  AND role<>?3 AND status IN ('starting','idle','working')),
                 (SELECT COUNT(*) FROM team_teammates WHERE project=?1 AND agent=?2
                  AND ended_ms IS NULL AND stopped_ms IS NULL)",
            params![project.as_str(), lead, AgentRole::Sub.name()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .map_err(error)?;
    assert!(spawned >= 0 && native >= 0 && panes >= 0);
    Ok(u32::try_from(spawned + native.max(panes)).unwrap_or(u32::MAX))
}

/// Counts the project's running harnesses: sub-agents the master started, not ones a lead
/// started. Takes a connection, the project and the master's id, if any. Returns the count,
/// or a SQLite failure.
fn running_harnesses(
    connection: &Connection,
    project: &ProjectId,
    master: Option<AgentId>,
) -> Result<u32, EngineError> {
    let running: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM team_agents WHERE project=?1 AND role=?2
             AND (parent IS NULL OR parent=?3) AND status IN ('starting','idle','working')",
            params![project.as_str(), AgentRole::Sub.name(), master],
            |row| row.get(0),
        )
        .map_err(error)?;
    assert!(running >= 0);
    Ok(u32::try_from(running).unwrap_or(u32::MAX))
}

/// Admits one more app-spawned sub-agent under `lead` (DEC-104 K8): refused while the owner
/// stopped the whole team, or when the lead already runs the owner's Max teammates per
/// harness in both modes. A master lead starts harnesses, so Max harnesses per project
/// counts for it instead. Takes a connection, the project and the lead. Returns Ok, or the
/// plain refusal sentence for the agent that asked.
pub fn admit_sub_agent(db: &Connection, project: &ProjectId, lead: AgentId) -> Result<(), String> {
    let unreadable = |_: EngineError| "Could not read this project's agent limits.".to_owned();
    let role: Option<String> = db
        .query_row(
            "SELECT role FROM team_agents WHERE id=?1 AND project=?2",
            params![lead, project.as_str()],
            |row| row.get(0),
        )
        .optional()
        .map_err(|_| "Could not read this project's agent limits.".to_owned())?;
    let Some(role) = role.as_deref().and_then(AgentRole::parse) else {
        return Err("That lead is not an agent of this project.".into());
    };
    if super::paused(db, project).map_err(unreadable)? {
        return Err("The owner stopped every agent of this project, so no sub-agent starts until they start the team again.".into());
    }
    let caps = read(db, project).map_err(unreadable)?;
    match role {
        AgentRole::Master => {
            let Some(cap) = caps.harnesses else {
                return Ok(());
            };
            let running = running_harnesses(db, project, Some(lead)).map_err(unreadable)?;
            if running >= u32::from(cap) {
                return Err(format!(
                    "The owner allows at most {cap} harnesses in this project, and {running} already run. Wait for one to finish, or ask the owner to raise Max harnesses per project."
                ));
            }
        }
        AgentRole::Sub => {
            let Some(cap) = caps.teammates else {
                return Ok(());
            };
            let running = running_sub_agents(db, project, lead).map_err(unreadable)?;
            if running >= u32::from(cap) {
                return Err(format!(
                    "The owner allows at most {cap} sub-agents per harness, and this harness already runs {running}. Wait for one to finish, or ask the owner to raise Max teammates per harness."
                ));
            }
        }
        AgentRole::Teammate => {
            return Err("A teammate does not start sub-agents. Its lead starts them.".into());
        }
    }
    Ok(())
}

#[cfg(all(test, unix))]
#[path = "caps/tests.rs"]
mod tests;
