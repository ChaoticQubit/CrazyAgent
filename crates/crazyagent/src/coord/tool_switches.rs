use super::store::{failure, project_known, state};
use crate::engine::EngineError;
use crate::protocol::ProjectId;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

/// The MCP server name the engine's own tools are listed under, the name harnesses see
/// in their tool names (`mcp__crazyagent-team__<tool>`), so a switch maps straight to
/// `--disallowedTools` and Codex `disabled_tools`.
pub const CRAZYAGENT_SERVER: &str = crate::team::tool::SERVER_NAME;
/// The longest server or tool name a switch is kept for, in bytes.
pub const NAME_BYTES_MAX: usize = 128;
/// The stored scope of the app-wide switches.
const GLOBAL: &str = "";

/// The engine's own tools (Q11, Q12), in the order the Plugins page lists them, each with
/// the one line it shows.
pub const BUILTIN_TOOLS: [(&str, &str); 13] = [
    (
        "start_work",
        "Start a unit of work and claim the files, modules or domains it touches.",
    ),
    (
        "inspect_work",
        "Read work state, your inbox and outbox, threads and the team.",
    ),
    (
        "update_work",
        "Record context, or send a message, question or reply to another agent.",
    ),
    (
        "transfer_work",
        "Assign, accept, decline, release, hand off or reopen a unit of work.",
    ),
    (
        "finish_work",
        "Record evidence and mark work review-ready, complete or closed.",
    ),
    (
        "spawn",
        "Start a lead or a sub-agent with its own worktree and branch.",
    ),
    ("stop", "Stop an agent you lead."),
    (
        "merge",
        "Merge a finished agent's branch into its lead's branch, locally.",
    ),
    ("remember", "Save a memory for the owner's approval."),
    ("recall", "Search the memories agents may read."),
    ("sessions", "Search or list past conversations."),
    ("session", "Read one past conversation a page at a time."),
    (
        "log_activity",
        "Log a new phase of the work or a finished test run to the project activity.",
    ),
];

/// One tool's switch: listed and callable, switched off, or deleted from the list until
/// "Restore defaults" (built-in tools only can be deleted this way).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Switch {
    #[default]
    On,
    Off,
    Hidden,
}

named!(Switch { On => "on", Off => "off", Hidden => "hidden" });

/// Reads one tool's effective switch: the project's own row, else the app-wide row,
/// else On. Takes the database, the project or None for the app-wide value, the server
/// and the tool. Returns the switch, or `coord_tool_invalid` for a bad name.
pub fn get(
    db: &Connection,
    project: Option<&ProjectId>,
    server: &str,
    tool: &str,
) -> Result<Switch, EngineError> {
    check_name(server)?;
    check_name(tool)?;
    if let Some(project) = project
        && let Some(switch) = row(db, project.as_str(), server, tool)?
    {
        return Ok(switch);
    }
    Ok(row(db, GLOBAL, server, tool)?.unwrap_or_default())
}

/// Stores one tool's switch for a project or app-wide. Takes the database, the scope,
/// the server, the tool and the switch. Returns Ok, `coord_tool_invalid` or
/// `coord_project_unknown`.
pub fn set(
    db: &Connection,
    project: Option<&ProjectId>,
    server: &str,
    tool: &str,
    switch: Switch,
) -> Result<(), EngineError> {
    check_name(server)?;
    check_name(tool)?;
    if let Some(project) = project {
        project_known(db, project)?;
    }
    db.execute(
        "INSERT INTO tool_switches(scope_project,server,tool,switch) VALUES (?1,?2,?3,?4)
         ON CONFLICT(scope_project,server,tool) DO UPDATE SET switch=excluded.switch",
        params![scope(project), server, tool, switch.as_str()],
    )
    .map_err(state)?;
    Ok(())
}

/// Clears every stored switch of one server in one scope, so its tools fall back to the
/// app-wide value or On, deleted tools included. Takes the database, the scope and the
/// server. Returns Ok or `coord_tool_invalid`.
pub fn restore_defaults(
    db: &Connection,
    project: Option<&ProjectId>,
    server: &str,
) -> Result<(), EngineError> {
    check_name(server)?;
    db.execute(
        "DELETE FROM tool_switches WHERE scope_project=?1 AND server=?2",
        params![scope(project), server],
    )
    .map_err(state)?;
    Ok(())
}

/// The switches stored in exactly one scope for one server, by tool name, so a page can
/// show the app-wide value and a project's own override apart. Takes the database, the
/// scope and the server. Returns the rows.
pub fn overrides(
    db: &Connection,
    project: Option<&ProjectId>,
    server: &str,
) -> Result<Vec<(String, Switch)>, EngineError> {
    check_name(server)?;
    let mut statement = db
        .prepare(
            "SELECT tool,switch FROM tool_switches WHERE scope_project=?1 AND server=?2
             ORDER BY tool",
        )
        .map_err(state)?;
    let rows = statement
        .query_map(params![scope(project), server], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(state)?;
    let mut found = Vec::new();
    for row in rows {
        let (tool, switch) = row.map_err(state)?;
        let switch = Switch::parse(&switch).ok_or_else(|| failure("coord_state_unavailable"))?;
        found.push((tool, switch));
    }
    Ok(found)
}

/// The tools of one server that are effectively Off or Hidden in a project, sorted, which
/// the engine leaves out of its tool lists and passes to harnesses as disallowed. Takes
/// the database, the project and the server.
pub fn off_tools(
    db: &Connection,
    project: &ProjectId,
    server: &str,
) -> Result<Vec<String>, EngineError> {
    let mine = overrides(db, Some(project), server)?;
    let mut effective: std::collections::BTreeMap<String, Switch> =
        overrides(db, None, server)?.into_iter().collect();
    effective.extend(mine);
    Ok(effective
        .into_iter()
        .filter(|(_, switch)| *switch != Switch::On)
        .map(|(tool, _)| tool)
        .collect())
}

fn scope(project: Option<&ProjectId>) -> &str {
    project.map_or(GLOBAL, ProjectId::as_str)
}

fn row(
    db: &Connection,
    scope: &str,
    server: &str,
    tool: &str,
) -> Result<Option<Switch>, EngineError> {
    let stored: Option<String> = db
        .query_row(
            "SELECT switch FROM tool_switches WHERE scope_project=?1 AND server=?2 AND tool=?3",
            params![scope, server, tool],
            |row| row.get(0),
        )
        .optional()
        .map_err(state)?;
    match stored {
        None => Ok(None),
        Some(text) => Switch::parse(&text)
            .map(Some)
            .ok_or_else(|| failure("coord_state_unavailable")),
    }
}

/// Checks a server or tool name: 1 to `NAME_BYTES_MAX` bytes of letters, digits, `_`,
/// `-` and `.`. Returns Ok or `coord_tool_invalid`.
fn check_name(name: &str) -> Result<(), EngineError> {
    let fine = !name.is_empty()
        && name.len() <= NAME_BYTES_MAX
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'));
    fine.then_some(())
        .ok_or_else(|| failure("coord_tool_invalid"))
}
