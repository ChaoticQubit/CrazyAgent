use super::{AgentRole, error, failure, now_ms};
use crate::engine::EngineError;
use crate::protocol::ProjectId;
use crate::storage::versioned;
use crate::terminal::{AgentId, PANES_PER_PROJECT_MAX};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
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
const FINISHED: &str = "a.status IN ('stopped','failed')";
/// The most handles the registry keeps per project, the newest ones (F16).
pub const HANDLES_KEPT_MAX: usize = 512;
/// The most retired handles one listing returns.
pub const RETIRED_LISTED_MAX: usize = 32;
const WAITING_TASKS_READ_MAX: i64 = 64;
const WAKING: &str = "EXISTS(SELECT 1 FROM team_task_pins p JOIN team_tasks t ON t.id=p.task
     WHERE p.agent=a.id AND t.status IN ('queued','needs_pool'))";
const SUFFIXES_MAX: usize = PANES_PER_PROJECT_MAX + 2;

const _: () = assert!(PERSONAS.len() > PANES_PER_PROJECT_MAX);
const _: () = assert!(HANDLE_CHARS_MAX >= 8 && HANDLE_CHARS_MAX < NAME_CHARS_MAX);
const _: () = assert!(HANDLES_KEPT_MAX >= PANES_PER_PROJECT_MAX && RETIRED_LISTED_MAX <= HANDLES_KEPT_MAX);

/// A team sub-agent's stable persona and the role handle of its current job, without the `@`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentName {
    pub persona: String,
    pub handle: Option<String>,
}

/// Creates the tables that keep sub-agent names and the roles the master gave tasks, and
/// clears every kept handle in a numbered form ([`numbered`]), stored before such handles
/// were refused, so no label shows a name that now reaches another agent.
/// Takes the engine connection.
/// Returns any SQLite failure.
pub fn initialize(connection: &Connection) -> Result<(), EngineError> {
    create(connection)?;
    let stored = connection
        .prepare("SELECT agent,handle FROM team_names WHERE handle GLOB '*[0-9]'")
        .map_err(error)?
        .query_map([], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(error)?;
    for (agent, _) in stored.iter().filter(|(_, handle)| numbered(handle)) {
        connection
            .execute("UPDATE team_names SET handle=NULL WHERE agent=?1", [agent])
            .map_err(error)?;
    }
    Ok(())
}

fn create(connection: &Connection) -> Result<(), EngineError> {
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
            );
            CREATE TABLE IF NOT EXISTS team_handles (
                project TEXT NOT NULL,
                handle TEXT NOT NULL,
                agent INTEGER NOT NULL,
                held_ms INTEGER NOT NULL,
                PRIMARY KEY(project, handle)
            );",
        )
        .map_err(error)
}

/// Turns free text into a role handle: lower-case ASCII letters and digits joined by single
/// hyphens, at most `HANDLE_CHARS_MAX` characters. Takes the text, a leading `@` allowed.
/// Returns the handle, or None when no letter or digit is left or the handle reads as an
/// agent number ([`numbered`]).
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
    (!kept.is_empty() && !numbered(kept)).then(|| kept.to_owned())
}

/// Says whether a name reads as an agent number in a form a master names agents by
/// (RV-DS-62): bare digits, `team-<n>`, `sub-agent-<n>` or `<harness>-<n>`, which
/// [`super::events::resolve`] reads as that agent, so no handle may take it. Takes the name
/// without its `@`. Returns true for such a form, in any case.
pub(crate) fn numbered(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    let (prefix, digits) = name.rsplit_once('-').unwrap_or(("", name.as_str()));
    !digits.is_empty()
        && digits.bytes().all(|byte| byte.is_ascii_digit())
        && (prefix.is_empty()
            || prefix == "team"
            || prefix == "sub-agent"
            || crate::harnesses::spec(prefix).is_some())
}

/// Names an agent the way every client shows it: "Ada (@onboarding-fix)", "Ada" before its
/// first job, "Master" for the master and "Sub-agent 7" for a sub-agent with no name kept.
/// Takes the role, team agent id and kept name. Returns the label.
pub fn label(role: AgentRole, id: i64, name: Option<&AgentName>) -> String {
    match (role, name) {
        (
            AgentRole::Sub | AgentRole::Teammate,
            Some(AgentName {
                persona,
                handle: Some(handle),
            }),
        ) => {
            format!("{persona} (@{handle})")
        }
        (
            AgentRole::Sub | AgentRole::Teammate,
            Some(AgentName {
                persona,
                handle: None,
            }),
        ) => persona.clone(),
        (AgentRole::Master, _) | (AgentRole::Sub | AgentRole::Teammate, None) => {
            role.agent_label(id)
        }
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
            "SELECT 1 FROM team_agents WHERE id=?1 AND project=?2 AND role IN ('sub','teammate')",
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
             WHERE n.project=?1 AND a.project=?1 AND n.agent<>?2 AND n.{column} IS NOT NULL
             AND ({LIVE} OR {WAKING})"
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

/// Gives a sub-agent its persona: the one it already has while no other live or waking agent
/// of the project holds it, or else a free persona no agent of the project held before, or the
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

/// Reads a name in one of a sub-agent's id forms, `team-7`, `<harness>-7` or a bare `7`,
/// the id a positive number written without leading zeros. Takes the name with no `@`.
/// Returns the prefix, None for bare digits, and the id, or None for any other name.
pub fn id_form(name: &str) -> Option<(Option<&str>, i64)> {
    let (prefix, digits) = match name.rsplit_once('-') {
        Some((prefix, digits)) => (Some(prefix), digits),
        None => (None, name),
    };
    let id = digits
        .parse::<i64>()
        .ok()
        .filter(|id| *id > 0 && id.to_string() == digits)?;
    Some((prefix, id))
}

/// The prefixes that make `<prefix>-<n>` an id form for some agent of a project: `team`,
/// every catalog harness and every harness the project's agents run, all lower case.
/// Takes the connection and project. Returns the prefixes.
fn id_prefixes(
    connection: &Connection,
    project: &ProjectId,
) -> Result<BTreeSet<String>, EngineError> {
    let mut statement = connection
        .prepare("SELECT DISTINCT lower(harness) FROM team_agents WHERE project=?1")
        .map_err(error)?;
    let mut prefixes = statement
        .query_map([project.as_str()], |row| row.get::<_, String>(0))
        .map_err(error)?
        .collect::<Result<BTreeSet<_>, _>>()
        .map_err(error)?;
    prefixes.insert("team".to_owned());
    prefixes.extend(
        crate::harnesses::specs()
            .iter()
            .map(|spec| spec.id.to_ascii_lowercase()),
    );
    Ok(prefixes)
}

/// Whether a handle reads as an agent's id form, so `events::resolve` would take it for
/// another agent's id. Takes the id prefixes and the handle. Returns true for such a handle.
fn id_shaped(prefixes: &BTreeSet<String>, handle: &str) -> bool {
    id_form(handle).is_some_and(|(prefix, _)| {
        prefix.is_none_or(|prefix| prefixes.contains(&prefix.to_ascii_lowercase()))
    })
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
    let prefixes = id_prefixes(connection, project)?;
    if id_shaped(&prefixes, wanted) {
        return Ok(Err("team_handle_invalid"));
    }
    let taken = held(connection, project, agent, "handle")?;
    let free = |candidate: &str| {
        candidate != MASTER_HANDLE
            && !numbered(candidate)
            && !taken.contains(candidate)
            && !id_shaped(&prefixes, candidate)
    };
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
    remember(connection, project, agent, &chosen)?;
    Ok(Ok(chosen))
}

/// Records in the handle registry that an agent holds a handle now (F16), so a later name
/// for that handle still finds the agent after it moves to another job's handle or the
/// owner renames it, and a new holder of the handle takes it over. Keeps the newest
/// `HANDLES_KEPT_MAX` handles of the project and none of an agent that no longer exists.
/// Takes the connection, project, team agent id and handle. Returns any SQLite failure.
fn remember(
    connection: &Connection,
    project: &ProjectId,
    agent: i64,
    handle: &str,
) -> Result<(), EngineError> {
    assert!(!handle.is_empty() && handle.len() <= HANDLE_CHARS_MAX);
    connection
        .execute(
            "INSERT INTO team_handles(project,handle,agent,held_ms) VALUES (?1,?2,?3,?4)
             ON CONFLICT(project,handle) DO UPDATE SET agent=excluded.agent, held_ms=excluded.held_ms",
            params![project.as_str(), handle, agent, now_ms()],
        )
        .map_err(error)?;
    connection
        .execute(
            "DELETE FROM team_handles WHERE project=?1 AND (agent NOT IN (SELECT id FROM team_agents)
               OR rowid NOT IN (SELECT rowid FROM team_handles WHERE project=?1
                                ORDER BY held_ms DESC, rowid DESC LIMIT ?2))",
            params![project.as_str(), HANDLES_KEPT_MAX as i64],
        )
        .map_err(error)?;
    Ok(())
}

/// Follows the handle registry for a handle no agent in the wanted state carries now
/// (F16): the agent that last held it, while it is a sub-agent of the project in that
/// state, so whatever was sent to a renamed agent or one reused for a later job reaches
/// it. The registry maps a handle straight to its agent, so there is no chain to follow.
/// Takes the connection, project, the handle without its `@` and `LIVE` or `FINISHED`.
/// Returns the agent, or None.
fn redirected(
    connection: &Connection,
    project: &ProjectId,
    handle: &str,
    state: &str,
) -> Result<Option<i64>, EngineError> {
    assert!(state == LIVE || state == FINISHED);
    connection
        .query_row(
            &format!(
                "SELECT a.id FROM team_handles h JOIN team_agents a ON a.id=h.agent
                 WHERE h.project=?1 AND a.project=?1 AND a.role='sub' AND {state}
                   AND h.handle=?2 COLLATE NOCASE
                 ORDER BY h.held_ms DESC LIMIT 1"
            ),
            params![project.as_str(), handle],
            |row| row.get(0),
        )
        .optional()
        .map_err(error)
}

/// Finds the waiting task whose role reserves a handle no agent carries yet (F16, DM
/// "@classifier-storage not recognized"): a task the master named that has not started,
/// the newest when several do. Takes the connection, project and the name, `@` allowed.
/// Returns the task id, or None.
pub fn waiting_task(
    connection: &Connection,
    project: &ProjectId,
    text: &str,
) -> Result<Option<i64>, EngineError> {
    let text = text.trim();
    if text.is_empty() || text.len() > NAME_CHARS_MAX {
        return Ok(None);
    }
    let Some(wanted) = handle(text) else {
        return Ok(None);
    };
    let mut statement = connection
        .prepare(
            "SELECT t.id,r.role FROM team_task_roles r JOIN team_tasks t ON t.id=r.task
             WHERE r.project=?1 AND t.project=?1 AND t.status IN ('queued','needs_pool')
             ORDER BY t.id DESC LIMIT ?2",
        )
        .map_err(error)?;
    let waiting = statement
        .query_map(params![project.as_str(), WAITING_TASKS_READ_MAX], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(error)?;
    assert!(waiting.len() <= WAITING_TASKS_READ_MAX as usize);
    Ok(waiting
        .into_iter()
        .find(|(_, role)| handle(role).as_deref() == Some(wanted.as_str()))
        .map(|(task, _)| task))
}

/// Lists the project's retired handles (F10, F16): those of sub-agents that finished and
/// whose worktree is gone, so they cannot wake there, and that no live agent holds now.
/// Newest first, at most `RETIRED_LISTED_MAX`. Takes the connection and project. Returns
/// each handle with its agent id.
pub fn retired(
    connection: &Connection,
    project: &ProjectId,
) -> Result<Vec<(String, i64)>, EngineError> {
    let mut statement = connection
        .prepare(&format!(
            "SELECT n.handle,a.id FROM team_names n JOIN team_agents a ON a.id=n.agent
             WHERE n.project=?1 AND a.project=?1 AND a.role='sub' AND {FINISHED}
               AND n.handle IS NOT NULL
               AND NOT EXISTS(SELECT 1 FROM team_worktrees w WHERE w.agent=a.id)
               AND NOT EXISTS(SELECT 1 FROM team_names m JOIN team_agents b ON b.id=m.agent
                              WHERE m.project=?1 AND b.project=?1 AND m.handle=n.handle
                                AND b.status IN ('starting','idle','working'))
             ORDER BY COALESCE(a.stopped_ms,a.heartbeat_ms) DESC, a.id DESC LIMIT ?2"
        ))
        .map_err(error)?;
    let found = statement
        .query_map(params![project.as_str(), RETIRED_LISTED_MAX as i64], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })
        .map_err(error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(error)?;
    assert!(found.len() <= RETIRED_LISTED_MAX);
    Ok(found)
}

/// Gives a sub-agent that starts again its names back (DEC-102): its persona while no other
/// live agent holds it, and its role handle, with the first free `-2`, `-3` suffix when a
/// live agent took that handle while it was stopped, or no handle when every suffix is
/// taken, so no two live agents ever share a name and a live agent's name never changes
/// under its master. A woken agent's names are settled when its wake is admitted and stay
/// held while its pinned task waits (WKC-2), so it starts under the name the master was
/// told. Takes the connection, project and team agent id. Returns the name, or an error
/// when the agent is not a sub-agent of the project.
pub fn reclaim(
    connection: &Connection,
    project: &ProjectId,
    agent: i64,
) -> Result<AgentName, EngineError> {
    let persona = assign_persona(connection, project, agent)?;
    let handle = match name(connection, agent)?.and_then(|name| name.handle) {
        Some(handle) => match store_handle(connection, project, agent, &handle, true)? {
            Ok(kept) => Some(kept),
            Err(_) => {
                connection
                    .execute("UPDATE team_names SET handle=NULL WHERE agent=?1", [agent])
                    .map_err(error)?;
                None
            }
        },
        None => None,
    };
    if let Some(handle) = &handle {
        assert!(!held(connection, project, agent, "handle")?.contains(handle));
    }
    Ok(AgentName { persona, handle })
}

/// Sets the role handle of a sub-agent's current job, as the master asked for it; a handle
/// another live agent of the project holds, or `@master`, gets the first free `-2`, `-3`
/// suffix that is no agent's id form. Takes the connection, project, team agent id and the
/// role in free text. Returns the handle, or a plain refusal code for an unusable role, one
/// shaped like an agent's id form such as `team-7`, a taken one or an unknown agent.
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
/// takes the task, when the role's version is the one the master read: 0 for a task it has
/// just created. Roles of tasks that no longer exist are dropped with their versions. The
/// writer is the project's newest team master, or the owner whose message started the turn
/// when no master agent is recorded. Takes the transaction, project, task id, role text and
/// that version. Returns the role's new version, the stale refusal, or any SQLite failure.
pub fn set_task_role(
    tx: &Transaction<'_>,
    project: &ProjectId,
    task: i64,
    role: &str,
    expected: u64,
) -> Result<Result<u64, versioned::Stale>, EngineError> {
    assert!(task > 0);
    let gone: Vec<i64> = tx
        .prepare(
            "SELECT task FROM team_task_roles WHERE project=?1 AND task NOT IN (SELECT id FROM team_tasks)",
        )
        .map_err(error)?
        .query_map([project.as_str()], |row| row.get(0))
        .map_err(error)?
        .collect::<Result<_, _>>()
        .map_err(error)?;
    for old in &gone {
        versioned::forget(tx, &versioned::task_role_item(*old))?;
    }
    tx.execute(
        "DELETE FROM team_task_roles WHERE project=?1 AND task NOT IN (SELECT id FROM team_tasks)",
        [project.as_str()],
    )
    .map_err(error)?;
    let writer = master_writer(tx, project)?;
    let version = match versioned::write_if(tx, &versioned::task_role_item(task), expected, &writer)
    {
        Ok(version) => version,
        Err(versioned::Refusal::Stale(stale)) => return Ok(Err(stale)),
        Err(versioned::Refusal::Failed(failed)) => return Err(failed),
    };
    tx.execute(
        "INSERT INTO team_task_roles(task,project,role) VALUES (?1,?2,?3)
         ON CONFLICT(task) DO UPDATE SET role=excluded.role",
        params![task, project.as_str(), role],
    )
    .map_err(error)?;
    assert!(version > expected);
    Ok(Ok(version))
}

/// Names who writes a task role for a project: its newest team master agent, or the owner
/// when no master agent is recorded. Takes the connection and project.
fn master_writer(
    connection: &Connection,
    project: &ProjectId,
) -> Result<versioned::Writer, EngineError> {
    let master: Option<i64> = connection
        .query_row(
            "SELECT id FROM team_agents WHERE project=?1 AND role='master' ORDER BY id DESC LIMIT 1",
            [project.as_str()],
            |row| row.get(0),
        )
        .optional()
        .map_err(error)?;
    Ok(match master {
        Some(id) => {
            versioned::Writer::Agent(AgentId::team(id).map_err(|_| failure("team_agent_invalid"))?)
        }
        None => versioned::Writer::Owner {
            force_after_conflict: false,
        },
    })
}

/// Gives a sub-agent the handle for the task it takes: the task's role, or for a task pinned
/// to it the handle it already has, or else `task-<id>`, so a reused agent never keeps the
/// handle of an earlier job and no handle reads as an agent's id form. Takes the
/// connection, project, team agent and task ids. Returns the handle, or None when the agent
/// is unknown or every candidate is taken.
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
    let prefixes = id_prefixes(connection, project)?;
    let usable = |wanted: &String| !id_shaped(&prefixes, wanted);
    let wanted = role
        .as_deref()
        .and_then(handle)
        .filter(|wanted| usable(wanted))
        .or(current.filter(|current| pinned && usable(current)))
        .unwrap_or_else(|| format!("task-{task}"));
    Ok(store_handle(connection, project, agent, &wanted, true)?.ok())
}

/// The owner's rename of a live sub-agent's role handle. Takes the connection, project,
/// team agent id and the new handle, already kebab case, a leading `@` allowed. Returns the
/// handle, or `team_handle_invalid` (also for an id form such as `team-7`, `codex-7` or
/// `7`), `team_handle_taken` or `team_agent_unknown`.
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
/// in any case, or a bare handle, else the live agent the handle registry says last held
/// that handle (F16). A name in a numbered form ([`numbered`]) is an agent number, never a
/// name, even for a handle stored before such handles were refused.
/// Takes the connection, project and the name text.
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
    if numbered(&bare) {
        return Ok(None);
    }
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
    match by("handle")? {
        Some(found) => Ok(Some(found)),
        None => redirected(connection, project, &bare, LIVE),
    }
}

/// Finds the finished sub-agent of a project that a name addresses, for a wake (DEC-102):
/// a persona in any case first, then an `@handle` or bare handle, among the project's
/// sub-agents that stopped or failed, the most recently stopped one when several carried it,
/// else the finished agent the handle registry says last held that handle (F16).
/// A name in a numbered form is left to the number lookup, as in [`resolve`].
/// Takes the connection, project and the name text. Returns the team agent id, or None.
pub fn resolve_finished(
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
    if numbered(&bare) {
        return Ok(None);
    }
    let by = |column: &str| {
        connection
            .query_row(
                &format!(
                    "SELECT a.id FROM team_names n JOIN team_agents a ON a.id=n.agent
                     WHERE n.project=?1 AND a.project=?1 AND a.role='sub'
                       AND a.status IN ('stopped','failed') AND n.{column}=?2 COLLATE NOCASE
                     ORDER BY COALESCE(a.stopped_ms,a.heartbeat_ms) DESC, a.id DESC LIMIT 1"
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
    match by("handle")? {
        Some(found) => Ok(Some(found)),
        None => redirected(connection, project, &bare, FINISHED),
    }
}

#[cfg(all(test, unix))]
#[path = "names/tests.rs"]
mod tests;
