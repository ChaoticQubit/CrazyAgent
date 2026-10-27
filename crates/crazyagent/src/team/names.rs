use super::{AgentRole, error, failure};
use crate::engine::EngineError;
use crate::protocol::ProjectId;
use crate::terminal::PANES_PER_PROJECT_MAX;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub const PERSONAS: [&str; 24] = [
    "Ada",
    "Grace",
    "Alan",
    "Linus",
    "Barbara",
    "Ken",
    "Dennis",
    "Margaret",
    "Edsger",
    "Donald",
    "Frances",
    "Radia",
    "Tim",
    "Katherine",
    "Hedy",
    "Niklaus",
    "Leslie",
    "Guido",
    "Bjarne",
    "Anita",
    "Sophie",
    "John",
    "Annie",
    "Mary",
];
pub const HANDLE_CHARS_MAX: usize = 32;
pub const MASTER_HANDLE: &str = "master";
const NAME_CHARS_MAX: usize = 64;
const LIVE: &str = "a.status IN ('starting','idle','working')";
const SUFFIXES_MAX: usize = PANES_PER_PROJECT_MAX + 2;

const _: () = assert!(PERSONAS.len() > PANES_PER_PROJECT_MAX);
const _: () = assert!(HANDLE_CHARS_MAX >= 8 && HANDLE_CHARS_MAX < NAME_CHARS_MAX);

/// A team sub-agent's stable persona and the role handle of its current job, without the `@`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentName {
    pub persona: String,
    pub handle: Option<String>,
}

/// Creates the tables that keep sub-agent names and the roles the master gave tasks.
/// Takes the engine connection.
/// Returns any SQLite failure.
pub fn initialize(connection: &Connection) -> Result<(), EngineError> {
    connection
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS team_names (
                agent INTEGER PRIMARY KEY,
                project TEXT NOT NULL,
                persona TEXT NOT NULL,
                handle TEXT
            );
            CREATE INDEX IF NOT EXISTS team_names_project ON team_names(project, handle);
            CREATE TABLE IF NOT EXISTS team_task_roles (
                task INTEGER PRIMARY KEY,
                project TEXT NOT NULL,
                role TEXT NOT NULL
            );",
        )
        .map_err(error)
}

/// Turns free text into a role handle: lower-case ASCII letters and digits joined by single
/// hyphens, at most `HANDLE_CHARS_MAX` characters. Takes the text, a leading `@` allowed.
/// Returns the handle, or None when no letter or digit is left.
pub fn handle(text: &str) -> Option<String> {
    let text = text.trim();
    let text = text.strip_prefix('@').unwrap_or(text);
    let mut kept = String::with_capacity(HANDLE_CHARS_MAX);
    for c in text.chars() {
        if kept.len() >= HANDLE_CHARS_MAX {
            break;
        }
        let c = c.to_ascii_lowercase();
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            kept.push(c);
        } else if (c.is_whitespace() || matches!(c, '-' | '_' | '.' | '/'))
            && !kept.is_empty()
            && !kept.ends_with('-')
        {
            kept.push('-');
        }
    }
    let kept = kept.trim_end_matches('-');
    assert!(kept.len() <= HANDLE_CHARS_MAX);
    (!kept.is_empty()).then(|| kept.to_owned())
}

/// Names an agent the way every client shows it: "Ada (@onboarding-fix)", "Ada" before its
/// first job, "Master" for the master and "Sub-agent 7" for a sub-agent with no name kept.
/// Takes the role, team agent id and kept name. Returns the label.
pub fn label(role: AgentRole, id: i64, name: Option<&AgentName>) -> String {
    match (role, name) {
        (
            AgentRole::Sub,
            Some(AgentName {
                persona,
                handle: Some(handle),
            }),
        ) => {
            format!("{persona} (@{handle})")
        }
        (
            AgentRole::Sub,
            Some(AgentName {
                persona,
                handle: None,
            }),
        ) => persona.clone(),
        (AgentRole::Master, _) | (AgentRole::Sub, None) => role.agent_label(id),
    }
}

/// Names a harness the way the owner knows it, such as "Claude Code" for `claude-code`.
/// Takes the harness id. Returns the catalog name, or the id itself for an unknown harness.
pub fn harness_name(harness: &str) -> &str {
    crate::harnesses::specs()
        .iter()
        .find(|spec| spec.id == harness)
        .map_or(harness, |spec| spec.name)
}

/// One line for an agent in lists and headers: "Ada (@onboarding-fix) · Claude Code · working".
/// Takes the agent label, harness id and status label; an unknown harness shows its id.
/// Returns the line.
pub fn line(label: &str, harness: &str, status: &str) -> String {
    format!(
        "{label} \u{b7} {} \u{b7} {}",
        harness_name(harness),
        status.to_lowercase()
    )
}

fn sub_agent(
    connection: &Connection,
    project: &ProjectId,
    agent: i64,
) -> Result<bool, EngineError> {
    connection
        .query_row(
            "SELECT 1 FROM team_agents WHERE id=?1 AND project=?2 AND role='sub'",
            params![agent, project.as_str()],
            |_| Ok(()),
        )
        .optional()
        .map(|found| found.is_some())
        .map_err(error)
}

/// Reads the persona and handle kept for one team agent.
/// Takes the connection and team agent id. Returns the name, or None when it has none.
pub fn name(connection: &Connection, agent: i64) -> Result<Option<AgentName>, EngineError> {
    connection
        .query_row(
            "SELECT persona,handle FROM team_names WHERE agent=?1",
            [agent],
            |row| {
                Ok(AgentName {
                    persona: row.get(0)?,
                    handle: row.get(1)?,
                })
            },
        )
        .optional()
        .map_err(error)
}

fn held(
    connection: &Connection,
    project: &ProjectId,
    agent: i64,
    column: &str,
) -> Result<BTreeSet<String>, EngineError> {
    assert!(column == "persona" || column == "handle");
    let mut statement = connection
        .prepare(&format!(
            "SELECT n.{column} FROM team_names n JOIN team_agents a ON a.id=n.agent
             WHERE n.project=?1 AND a.project=?1 AND n.agent<>?2 AND n.{column} IS NOT NULL AND {LIVE}"
        ))
        .map_err(error)?;
    let found = statement
        .query_map(params![project.as_str(), agent], |row| {
            row.get::<_, String>(0)
        })
        .map_err(error)?
        .collect::<Result<BTreeSet<_>, _>>()
        .map_err(error)?;
    Ok(found)
}

/// Gives a sub-agent its persona: the one it already has while no other live agent of the
/// project holds it, or else a free persona no agent of the project held before, or the
/// least used. Takes the connection, project and team agent id. Returns the persona, or an
/// error when the agent is not a sub-agent of the project.
pub fn assign_persona(
    connection: &Connection,
    project: &ProjectId,
    agent: i64,
) -> Result<String, EngineError> {
    if !sub_agent(connection, project, agent)? {
        return Err(failure("team_agent_unknown"));
    }
    connection
        .execute(
            "DELETE FROM team_names WHERE project=?1 AND agent NOT IN (SELECT id FROM team_agents)",
            [project.as_str()],
        )
        .map_err(error)?;
    let taken = held(connection, project, agent, "persona")?;
    let current = name(connection, agent)?;
    if let Some(current) = current
        .as_ref()
        .filter(|current| !taken.contains(&current.persona))
    {
        return Ok(current.persona.clone());
    }
    let mut uses: BTreeMap<String, i64> = BTreeMap::new();
    {
        let mut statement = connection
            .prepare("SELECT persona,COUNT(*) FROM team_names WHERE project=?1 AND agent<>?2 GROUP BY persona")
            .map_err(error)?;
        let rows = statement
            .query_map(params![project.as_str(), agent], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })
            .map_err(error)?;
        for row in rows {
            let (persona, count) = row.map_err(error)?;
            uses.insert(persona, count);
        }
    }
    let persona = PERSONAS
        .iter()
        .enumerate()
        .filter(|(_, persona)| !taken.contains(**persona))
        .min_by_key(|(order, persona)| (uses.get(**persona).copied().unwrap_or(0), *order))
        .map(|(_, persona)| *persona)
        .ok_or_else(|| failure("team_persona_unavailable"))?;
    connection
        .execute(
            "INSERT INTO team_names(agent,project,persona) VALUES (?1,?2,?3)
             ON CONFLICT(agent) DO UPDATE SET persona=excluded.persona",
            params![agent, project.as_str(), persona],
        )
        .map_err(error)?;
    Ok(persona.to_owned())
}

fn suffixed(base: &str, attempt: usize) -> String {
    assert!(attempt >= 1 && !base.is_empty() && base.len() <= HANDLE_CHARS_MAX);
    if attempt == 1 {
        return base.to_owned();
    }
    let suffix = format!("-{attempt}");
    let room = HANDLE_CHARS_MAX - suffix.len();
    let stem = base[..base.len().min(room)].trim_end_matches('-');
    let candidate = format!("{stem}{suffix}");
    assert!(candidate.len() <= HANDLE_CHARS_MAX);
    candidate
}

fn store_handle(
    connection: &Connection,
    project: &ProjectId,
    agent: i64,
    wanted: &str,
    suffix: bool,
) -> Result<Result<String, &'static str>, EngineError> {
    if !sub_agent(connection, project, agent)? {
        return Ok(Err("team_agent_unknown"));
    }
    if name(connection, agent)?.is_none() {
        assign_persona(connection, project, agent)?;
    }
    let taken = held(connection, project, agent, "handle")?;
    let free = |candidate: &str| candidate != MASTER_HANDLE && !taken.contains(candidate);
    let attempts = if suffix { SUFFIXES_MAX } else { 1 };
    let Some(chosen) = (1..=attempts)
        .map(|attempt| suffixed(wanted, attempt))
        .find(|candidate| free(candidate))
    else {
        return Ok(Err("team_handle_taken"));
    };
    let changed = connection
        .execute(
            "UPDATE team_names SET handle=?2 WHERE agent=?1",
            params![agent, chosen],
        )
        .map_err(error)?;
    assert_eq!(changed, 1);
    Ok(Ok(chosen))
}

/// Sets the role handle of a sub-agent's current job, as the master asked for it; a handle
/// another live agent of the project holds, or `@master`, gets the first free `-2`, `-3`
/// suffix. Takes the connection, project, team agent id and the role in free text.
/// Returns the handle, or a plain refusal code for an unusable role or unknown agent.
pub fn assign_role(
    connection: &Connection,
    project: &ProjectId,
    agent: i64,
    role: &str,
) -> Result<Result<String, &'static str>, EngineError> {
    let Some(wanted) = handle(role) else {
        return Ok(Err("team_handle_invalid"));
    };
    store_handle(connection, project, agent, &wanted, true)
}

/// Keeps the role the master gave a task when it delegated it, for the agent that later
/// takes the task; roles of tasks that no longer exist are dropped. Takes the connection,
/// project, task id and role text. Returns any SQLite failure.
pub fn set_task_role(
    connection: &Connection,
    project: &ProjectId,
    task: i64,
    role: &str,
) -> Result<(), EngineError> {
    assert!(task > 0);
    connection
        .execute(
            "DELETE FROM team_task_roles WHERE project=?1 AND task NOT IN (SELECT id FROM team_tasks)",
            [project.as_str()],
        )
        .map_err(error)?;
    connection
        .execute(
            "INSERT INTO team_task_roles(task,project,role) VALUES (?1,?2,?3)
             ON CONFLICT(task) DO UPDATE SET role=excluded.role",
            params![task, project.as_str(), role],
        )
        .map_err(error)?;
    Ok(())
}

/// Gives a sub-agent the handle for the task it takes: the task's role, or for a task pinned
/// to it the handle it already has, or else `task-<id>`, so a reused agent never keeps the
/// handle of an earlier job. Takes the connection, project, team agent and task ids.
/// Returns the handle, or None when the agent is unknown or every candidate is taken.
pub fn assign_task_role(
    connection: &Connection,
    project: &ProjectId,
    agent: i64,
    task: i64,
) -> Result<Option<String>, EngineError> {
    assert!(task > 0);
    let role: Option<String> = connection
        .query_row(
            "SELECT role FROM team_task_roles WHERE task=?1 AND project=?2",
            params![task, project.as_str()],
            |row| row.get(0),
        )
        .optional()
        .map_err(error)?;
    let pinned = connection
        .query_row(
            "SELECT 1 FROM team_task_pins WHERE task=?1 AND project=?2 AND agent=?3",
            params![task, project.as_str(), agent],
            |_| Ok(()),
        )
        .optional()
        .map_err(error)?
        .is_some();
    let current = name(connection, agent)?.and_then(|name| name.handle);
    let wanted = role
        .as_deref()
        .and_then(handle)
        .or(current.filter(|_| pinned))
        .unwrap_or_else(|| format!("task-{task}"));
    Ok(store_handle(connection, project, agent, &wanted, true)?.ok())
}

/// The owner's rename of a live sub-agent's role handle. Takes the connection, project,
/// team agent id and the new handle, already kebab case, a leading `@` allowed. Returns the
/// handle, or `team_handle_invalid`, `team_handle_taken` or `team_agent_unknown`.
pub fn rename(
    connection: &Connection,
    project: &ProjectId,
    agent: i64,
    text: &str,
) -> Result<Result<String, &'static str>, EngineError> {
    let bare = text.strip_prefix('@').unwrap_or(text).to_ascii_lowercase();
    let Some(wanted) = handle(&bare).filter(|wanted| *wanted == bare) else {
        return Ok(Err("team_handle_invalid"));
    };
    let live = connection
        .query_row(
            &format!("SELECT 1 FROM team_agents a WHERE a.id=?1 AND a.project=?2 AND a.role='sub' AND {LIVE}"),
            params![agent, project.as_str()],
            |_| Ok(()),
        )
        .optional()
        .map_err(error)?
        .is_some();
    if !live {
        return Ok(Err("team_agent_unknown"));
    }
    store_handle(connection, project, agent, &wanted, false)
}

/// Finds the live agent of a project that a name addresses: `@handle`, `@master`, a persona
/// in any case, or a bare handle. Takes the connection, project and the name text.
/// Returns the team agent id, or None when no live agent of the project carries that name.
pub fn resolve(
    connection: &Connection,
    project: &ProjectId,
    text: &str,
) -> Result<Option<i64>, EngineError> {
    let text = text.trim();
    if text.is_empty() || text.len() > NAME_CHARS_MAX {
        return Ok(None);
    }
    let (marked, bare) = match text.strip_prefix('@') {
        Some(bare) => (true, bare.to_ascii_lowercase()),
        None => (false, text.to_ascii_lowercase()),
    };
    if bare == MASTER_HANDLE {
        return connection
            .query_row(
                &format!("SELECT a.id FROM team_agents a WHERE a.project=?1 AND a.role='master' AND {LIVE} ORDER BY a.id DESC LIMIT 1"),
                [project.as_str()],
                |row| row.get(0),
            )
            .optional()
            .map_err(error);
    }
    let by = |column: &str| {
        connection
            .query_row(
                &format!(
                    "SELECT a.id FROM team_names n JOIN team_agents a ON a.id=n.agent
                     WHERE n.project=?1 AND a.project=?1 AND a.role='sub' AND {LIVE} AND n.{column}=?2 COLLATE NOCASE
                     ORDER BY a.id DESC LIMIT 1"
                ),
                params![project.as_str(), bare],
                |row| row.get::<_, i64>(0),
            )
            .optional()
            .map_err(error)
    };
    if !marked && let Some(found) = by("persona")? {
        return Ok(Some(found));
    }
    by("handle")
}

#[cfg(all(test, unix))]
#[path = "names/tests.rs"]
mod tests;
