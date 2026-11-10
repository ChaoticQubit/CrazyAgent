use super::store::{Change, MESSAGE_COLUMNS, agent, failure, message_from_row, party_of, state};
use super::{
    AgentId, AgentState, Message, Party, Presence, Receipt, Snapshot, TOOL_BYTES_MAX, Waiting,
};
use crate::engine::EngineError;
use crate::protocol::ProjectId;
use rusqlite::{Connection, OptionalExtension, params};
use std::collections::BTreeMap;

/// Concord's presence thresholds, in ms since the last activity.
const LIVE_MS: i64 = 5 * 60_000;
const IDLE_MS: i64 = 30 * 60_000;
const AWAY_MS: i64 = 60 * 60_000;
/// The most agents, owner questions and unread messages one snapshot reads.
const AGENTS_SHOWN_MAX: usize = 256;
const QUESTIONS_SHOWN_MAX: usize = 100;
const UNREAD_SCAN_MAX: usize = 4096;
const HOUR_MS: i64 = 60 * 60 * 1000;
/// The most different tools one agent keeps rows for in one hour.
pub(crate) const TOOLS_PER_HOUR_MAX: i64 = 64;
/// The most rows the tools chart keeps.
pub(crate) const TOOL_HOURS_MAX: i64 = 1 << 20;

const _: () =
    assert!(LIVE_MS < IDLE_MS && IDLE_MS < AWAY_MS && super::adoption::UNGROUNDED_MS < IDLE_MS);

/// Records activity of an agent now (Concord presence): its last activity becomes `now`
/// unless a later one is stored. Takes the database, the agent and the time in ms.
/// Returns Ok or `coord_agent_unknown`.
pub fn touch(db: &Connection, agent_id: AgentId, now: i64) -> Result<(), EngineError> {
    if agent(db, agent_id)?.is_none() {
        return Err(failure("coord_agent_unknown"));
    }
    seen(db, agent_id, now)
}

/// Marks an agent as waiting on its user in its own terminal, or clears it (Q14), and
/// counts as activity. The time a wait started stays while the same wait goes on.
/// Takes the database, the agent, the wait or None and the time in ms.
pub fn set_waiting(
    db: &Connection,
    agent_id: AgentId,
    waiting: Option<Waiting>,
    now: i64,
) -> Result<(), EngineError> {
    touch(db, agent_id, now)?;
    let text = waiting.map(Waiting::as_str);
    db.execute(
        "UPDATE coord_activity SET
         waiting_since=CASE WHEN waiting IS ?2 THEN waiting_since ELSE ?3 END,
         waiting=?2 WHERE agent=?1",
        params![agent_id, text, waiting.map(|_| now)],
    )
    .map_err(state)?;
    Ok(())
}

/// Counts one tool call of an agent for the adoption counts (Q18) and as activity, the
/// count and its chart hour in one change, nested in the caller's transaction if it holds
/// one. Takes the database, the agent, the tool name as its harness reported it (1 to
/// `TOOL_BYTES_MAX` bytes, no space or control character) and the time in ms.
/// Returns Ok, `coord_tool_invalid` or `coord_agent_unknown`.
pub fn record_tool(
    db: &Connection,
    agent_id: AgentId,
    tool: &str,
    now: i64,
) -> Result<(), EngineError> {
    if tool.is_empty()
        || tool.len() > TOOL_BYTES_MAX
        || tool.chars().any(|c| c.is_control() || c.is_whitespace())
    {
        return Err(failure("coord_tool_invalid"));
    }
    let change = Change::begin(db)?;
    touch(change.db(), agent_id, now)?;
    change
        .db()
        .execute(
            "INSERT INTO coord_tool_use(agent,tool,count,last) VALUES (?1,?2,1,?3)
             ON CONFLICT(agent,tool) DO UPDATE SET count=MIN(count+1, 4294967295), last=?3",
            params![agent_id, tool, now],
        )
        .map_err(state)?;
    add_tool_hour(change.db(), now - now.rem_euclid(HOUR_MS), agent_id, tool).map_err(state)?;
    change.commit()
}

/// Counts one tool call in its hour. A new row needs room under both caps; past them the call
/// is left out of the chart only. Inputs: the store, the hour, the agent and the tool. Output:
/// the rows written.
pub(crate) fn add_tool_hour(
    db: &Connection,
    hour_ms: i64,
    agent: AgentId,
    tool: &str,
) -> rusqlite::Result<usize> {
    db.execute(
        "INSERT INTO coord_tool_hours(hour_ms,agent,tool,count)
         SELECT ?1,?2,?3,1 WHERE EXISTS (SELECT 1 FROM coord_tool_hours WHERE hour_ms=?1 AND agent=?2 AND tool=?3)
            OR (COALESCE((SELECT rows FROM coord_tool_hours_rows),0)<?4
                AND (SELECT COUNT(*) FROM coord_tool_hours WHERE hour_ms=?1 AND agent=?2)<?5)
         ON CONFLICT(hour_ms,agent,tool) DO UPDATE SET count=MIN(count+1, 4294967295)",
        params![hour_ms, agent, tool, TOOL_HOURS_MAX, TOOLS_PER_HOUR_MAX],
    )
}

/// Stores activity without checking the agent, for callers that already did.
pub(super) fn seen(db: &Connection, agent_id: AgentId, now: i64) -> Result<(), EngineError> {
    db.execute(
        "INSERT INTO coord_activity(agent,last_seen) VALUES (?1,?2)
         ON CONFLICT(agent) DO UPDATE SET last_seen=MAX(last_seen, ?2)",
        params![agent_id, now],
    )
    .map_err(state)?;
    Ok(())
}

/// An agent's presence now: Stopped for a stopped, failed or unknown agent, else from its
/// last activity, or its start when it has none yet. Takes the database, the agent and
/// the time in ms.
pub(super) fn current(
    db: &Connection,
    agent_id: AgentId,
    now: i64,
) -> Result<Presence, EngineError> {
    let Some(row) = agent(db, agent_id)? else {
        return Ok(Presence::Stopped);
    };
    if !row.live() {
        return Ok(Presence::Stopped);
    }
    let last: Option<i64> = db
        .query_row(
            "SELECT last_seen FROM coord_activity WHERE agent=?1",
            [agent_id],
            |found| found.get(0),
        )
        .optional()
        .map_err(state)?;
    Ok(presence(last.unwrap_or(row.created), now))
}

/// The last activity at or before which an agent counts as Away or Stopped now, as
/// [`presence`] reads it, for a query that decides on presence in SQL. Input: the time in
/// ms. Output: that time.
pub(super) fn away_at_or_before(now: i64) -> i64 {
    now.saturating_sub(IDLE_MS)
}

fn presence(last: i64, now: i64) -> Presence {
    match now.saturating_sub(last) {
        quiet if quiet < LIVE_MS => Presence::Live,
        quiet if quiet < IDLE_MS => Presence::Idle,
        quiet if quiet < AWAY_MS => Presence::Away,
        _ => Presence::Stopped,
    }
}

/// The coordination state of one project for the graph, the Process page and the
/// owner's inbox: each agent's presence, wait, the file it waits for under Wait, worktree,
/// unread count, counted tools and adoption flags (Q18), every claim overlap, the open
/// questions for the owner by severity, and the unread messages per sender and recipient
/// with the latest receipt.
/// Takes the database, the project and the time in ms. Returns the snapshot.
pub fn snapshot(db: &Connection, project: &ProjectId, now: i64) -> Result<Snapshot, EngineError> {
    let settings = super::settings::read(db, project)?;
    let ids: Vec<AgentId> = {
        let mut statement = db
            .prepare("SELECT id FROM team_agents WHERE project=?1 ORDER BY id DESC LIMIT ?2")
            .map_err(state)?;
        let limit = i64::try_from(AGENTS_SHOWN_MAX).map_err(state)?;
        let rows = statement
            .query_map(params![project.as_str(), limit], |row| row.get(0))
            .map_err(state)?;
        rows.collect::<Result<_, _>>().map_err(state)?
    };
    let mut agents = Vec::with_capacity(ids.len());
    for id in ids.into_iter().rev() {
        agents.push(agent_state(db, id, &settings.counted_tools, now)?);
    }
    Ok(Snapshot {
        agents,
        overlaps: super::claims::all_overlaps(db, project, now)?,
        questions_for_owner: owner_questions(db, project)?,
        unread_edges: unread_edges(db, project)?,
    })
}

fn agent_state(
    db: &Connection,
    id: AgentId,
    counted: &[String],
    now: i64,
) -> Result<AgentState, EngineError> {
    let row = agent(db, id)?.ok_or_else(|| failure("coord_agent_unknown"))?;
    let presence = current(db, id, now)?;
    let waiting: Option<String> = db
        .query_row(
            "SELECT waiting FROM coord_activity WHERE agent=?1",
            [id],
            |found| found.get(0),
        )
        .optional()
        .map_err(state)?
        .flatten();
    let waiting = match waiting {
        Some(text) => {
            Some(Waiting::parse(&text).ok_or_else(|| failure("coord_state_unavailable"))?)
        }
        None => None,
    };
    let has_worktree: bool = db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM team_worktrees WHERE agent=?1)
             OR EXISTS(SELECT 1 FROM team_agents WHERE id=?1 AND role='teammate'
                       AND worktree IS NOT NULL)",
            [id],
            |found| found.get(0),
        )
        .map_err(state)?;
    let unread: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM coord_messages WHERE to_kind='agent' AND to_agent=?1
             AND receipt IN ('sent','delivered','typed')",
            [id],
            |found| found.get(0),
        )
        .map_err(state)?;
    let used = super::adoption::used(db, id)?;
    let count = |name: &(String, i64)| u32::try_from(name.1.max(0)).unwrap_or(u32::MAX);
    let tool_counts: Vec<(String, u32)> = used
        .iter()
        .filter(|tool| counted.iter().any(|pattern| counts(pattern, &tool.0)))
        .map(|tool| (tool.0.clone(), count(tool)))
        .collect();
    let flags = super::adoption::flags(db, id, &row, &used, now)?
        .into_iter()
        .map(|flag| flag.name().to_owned())
        .collect();
    Ok(AgentState {
        agent: id,
        presence,
        waiting,
        waits_for: super::claims::latest_wait(db, id)?.map(|(_, _, file)| file),
        has_worktree,
        unread: u32::try_from(unread.max(0)).unwrap_or(u32::MAX),
        tool_counts,
        flags,
    })
}

/// The tool name after an MCP server prefix (`mcp__<server>__<tool>`), else the name.
pub(super) fn short(tool: &str) -> &str {
    match tool.strip_prefix("mcp__") {
        Some(rest) => rest.rsplit("__").next().unwrap_or(rest),
        None => tool,
    }
}

/// Says whether a counted-tools entry covers a tool: the entry equals the name or its
/// short form, or ends in `*` and starts one of them.
pub(super) fn counts(pattern: &str, tool: &str) -> bool {
    let matches = |name: &str| match pattern.strip_suffix('*') {
        Some(stem) => name.starts_with(stem),
        None => name == pattern,
    };
    matches(tool) || matches(short(tool))
}

/// The open questions for the owner (Q14b), highest severity first, then oldest, at most
/// `QUESTIONS_SHOWN_MAX`, which the inbox and the leads and master in the chain read.
/// Takes the database and the project. Returns the questions.
pub fn owner_questions(db: &Connection, project: &ProjectId) -> Result<Vec<Message>, EngineError> {
    let mut statement = db
        .prepare(&format!(
            "SELECT {MESSAGE_COLUMNS} FROM coord_messages
             WHERE project=?1 AND kind='question' AND to_kind='owner'
             AND receipt NOT IN ('replied','failed','expired')
             ORDER BY CASE q_severity WHEN 'high' THEN 0 WHEN 'normal' THEN 1 ELSE 2 END, id
             LIMIT ?2"
        ))
        .map_err(state)?;
    let limit = i64::try_from(QUESTIONS_SHOWN_MAX).map_err(state)?;
    let rows = statement
        .query_map(params![project.as_str(), limit], message_from_row)
        .map_err(state)?;
    rows.collect::<Result<_, _>>().map_err(state)
}

/// The unread messages per sender and recipient, with how many and the latest one's
/// receipt.
fn unread_edges(
    db: &Connection,
    project: &ProjectId,
) -> Result<Vec<(Party, Party, u32, Receipt)>, EngineError> {
    let mut statement = db
        .prepare(
            "SELECT from_kind,from_agent,to_kind,to_agent,receipt FROM coord_messages
             WHERE project=?1 AND receipt IN ('sent','delivered','typed')
             ORDER BY id DESC LIMIT ?2",
        )
        .map_err(state)?;
    let limit = i64::try_from(UNREAD_SCAN_MAX).map_err(state)?;
    let rows = statement
        .query_map(params![project.as_str(), limit], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<i64>>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<i64>>(3)?,
                row.get::<_, String>(4)?,
            ))
        })
        .map_err(state)?;
    let mut edges: BTreeMap<(Party, Party), (u32, Receipt)> = BTreeMap::new();
    for row in rows {
        let (from_kind, from_agent, to_kind, to_agent, receipt) = row.map_err(state)?;
        let (Some(from), Some(to), Some(receipt)) = (
            party_of(&from_kind, from_agent),
            party_of(&to_kind, to_agent),
            Receipt::parse(&receipt),
        ) else {
            return Err(failure("coord_state_unavailable"));
        };
        let edge = edges.entry((from, to)).or_insert((0, receipt));
        edge.0 = edge.0.saturating_add(1);
    }
    Ok(edges
        .into_iter()
        .map(|((from, to), (count, receipt))| (from, to, count, receipt))
        .collect())
}
