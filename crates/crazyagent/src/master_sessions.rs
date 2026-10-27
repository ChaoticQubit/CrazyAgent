use crate::engine::EngineError;
use crate::master_session::{MasterSessionPhase, MasterSessionView};
use crate::protocol::{Cursor, ProjectId, SessionId};
use crate::team::AgentRole;
use crate::team::names;
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};

/// The most sessions one list read returns, newest activity first.
pub const SESSIONS_LISTED_MAX: usize = 50;
/// The most characters a session's first or last line keeps in the list.
pub const LINE_CHARS_MAX: usize = 80;
/// The most agent names one listed session carries.
pub const LISTED_AGENTS_MAX: usize = 6;
/// The most bytes the encoded session list may take, under the owner frame bound.
pub const LIST_BYTES_MAX: usize = 49152;
/// The most bytes the engine-built state section of a master's context may take.
pub const STATE_BYTES_MAX: usize = 3072;
/// The most bytes the transcript summary that seeds a fresh harness conversation may take.
pub const SUMMARY_BYTES_MAX: usize = 8192;
/// The most rows of each kind the state section lists.
pub const STATE_ROWS_MAX: usize = 12;

const MARKS: &str = "CREATE TABLE IF NOT EXISTS master_session_marks (
    session TEXT PRIMARY KEY CHECK(length(session) BETWEEN 1 AND 128),
    project TEXT NOT NULL CHECK(length(project) BETWEEN 1 AND 128),
    started_ms INTEGER NOT NULL CHECK(started_ms >= 0),
    active_ms INTEGER NOT NULL CHECK(active_ms >= started_ms)
)";

/// One master session as the Sessions list shows it. `created` and `last_activity` are
/// engine revisions; `started_ms` and `active_ms` are wall-clock times, absent for sessions
/// older than the marks. `agents` names the session's own sub-agents, newest first;
/// `results_waiting` counts finished sub-agent results queued for this session's master.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MasterSessionSummary {
    pub session_id: SessionId,
    pub phase: MasterSessionPhase,
    pub created: Cursor,
    pub last_activity: Cursor,
    pub started_ms: Option<u64>,
    pub active_ms: Option<u64>,
    pub turns: u64,
    pub first_line: Option<String>,
    pub last_line: Option<String>,
    pub agents: Vec<String>,
    pub results_waiting: u64,
}

/// How the live master continues a session it resumes. `Fresh` starts a new harness
/// conversation whose first input is `seed`, the engine-built transcript summary and
/// state of the session, and shows `note` in the master view so history is never
/// dropped silently.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ResumePlan {
    Fresh { seed: String, note: String },
}

/// The note a resumed session shows when its harness conversation starts fresh.
pub const FRESH_NOTE: &str = "This session continues in a new harness conversation, started with a summary of its earlier turns.";

/// Creates the table of session wall-clock marks when the engine database opens.
/// Takes the engine connection. Returns any SQLite failure.
pub(crate) fn initialize(connection: &Connection) -> Result<(), EngineError> {
    connection.execute_batch(MARKS).map_err(error)
}

/// Migration part that gives team tasks and team agents their owning session column.
/// Tables not created yet are left to `team::initialize`, which makes the column.
/// Takes the upgrade transaction. Returns Ok once both tables have the column.
pub(crate) fn add_ownership_columns(tx: &Transaction<'_>) -> Result<(), EngineError> {
    for table in ["team_tasks", "team_agents"] {
        let exists: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name=?1)",
                [table],
                |row| row.get(0),
            )
            .map_err(error)?;
        if !exists {
            continue;
        }
        let present: bool = tx
            .query_row(
                &format!(
                    "SELECT EXISTS(SELECT 1 FROM pragma_table_info('{table}') WHERE name='session')"
                ),
                [],
                |row| row.get(0),
            )
            .map_err(error)?;
        if !present {
            tx.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN session TEXT"))
                .map_err(error)?;
        }
    }
    Ok(())
}

/// The live master session of a project, the one whose phase is `configured`.
/// Takes the connection and project. Returns its id, or None when no session is live or
/// the database has no master sessions table yet.
pub fn live_session(
    connection: &Connection,
    project: &ProjectId,
) -> Result<Option<String>, EngineError> {
    let exists: bool = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name='master_sessions')",
            [],
            |row| row.get(0),
        )
        .map_err(error)?;
    if !exists {
        return Ok(None);
    }
    connection
        .query_row(
            "SELECT CASE WHEN length(CAST(id AS BLOB))<=128 THEN id END FROM master_sessions WHERE project=?1 AND phase='configured'",
            [project.as_str()],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()
        .map(Option::flatten)
        .map_err(error)
}

/// Records that a project's live session was used now: its start time on first use and
/// its last activity time. Takes the transaction, project and time. Returns Ok, also
/// when no session is live.
pub(crate) fn touch(
    tx: &Transaction<'_>,
    project: &ProjectId,
    now: u64,
) -> Result<(), EngineError> {
    let Some(session) = live_session(tx, project)? else {
        return Ok(());
    };
    let now = i64::try_from(now).map_err(error)?;
    tx.execute(
        "INSERT INTO master_session_marks(session,project,started_ms,active_ms) VALUES (?1,?2,?3,?3)
         ON CONFLICT(session) DO UPDATE SET active_ms=MAX(active_ms,excluded.active_ms)",
        params![session, project.as_str(), now],
    )
    .map_err(error)?;
    Ok(())
}

/// Counts the finished sub-agent results queued for one session's master: tasks the
/// session owns that ended and were not yet delivered to a master.
/// Takes the connection and session. Returns the count.
pub fn results_waiting(connection: &Connection, session: &SessionId) -> Result<u64, EngineError> {
    let count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM team_tasks WHERE session=?1 AND source='master' AND delivered=0 AND status IN ('done','failed','cancelled')",
            [session.as_str()],
            |row| row.get(0),
        )
        .map_err(error)?;
    u64::try_from(count).map_err(error)
}

/// Lists a project's master sessions: the live one first, then newest activity first,
/// at most `SESSIONS_LISTED_MAX`, within `LIST_BYTES_MAX` once encoded.
/// Takes the connection and project. Returns the summaries.
pub(crate) fn list(
    connection: &Connection,
    project: &ProjectId,
) -> Result<Vec<MasterSessionSummary>, EngineError> {
    let mut statement = connection
        .prepare(
            "SELECT CASE WHEN length(CAST(s.id AS BLOB))<=128 THEN s.id END,s.created,s.version,
                    CASE WHEN length(s.phase)<=10 THEN s.phase END,
                    MAX(s.version,COALESCE((SELECT MAX(t.input) FROM master_turns t WHERE t.session=s.id),0)) AS active,
                    m.started_ms,m.active_ms
             FROM master_sessions s LEFT JOIN master_session_marks m ON m.session=s.id
             WHERE s.project=?1
             ORDER BY s.phase='configured' DESC, active DESC, s.created DESC LIMIT ?2",
        )
        .map_err(error)?;
    type Row = (
        Option<String>,
        i64,
        i64,
        Option<String>,
        i64,
        Option<i64>,
        Option<i64>,
    );
    let rows = statement
        .query_map(
            params![
                project.as_str(),
                i64::try_from(SESSIONS_LISTED_MAX).map_err(error)?
            ],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                ))
            },
        )
        .map_err(error)?
        .collect::<Result<Vec<Row>, _>>()
        .map_err(error)?;
    let mut sessions = Vec::with_capacity(rows.len());
    let mut bytes = 2usize;
    for (id, created, version, phase, active, started_ms, active_ms) in rows {
        let session_id =
            SessionId::new(id.ok_or_else(|| error("master_session_record"))?).map_err(error)?;
        let phase = match phase.as_deref() {
            Some("configured") => MasterSessionPhase::Configured,
            Some("parked") => MasterSessionPhase::Parked,
            Some("closed") => MasterSessionPhase::Closed,
            _ => return Err(error("master_session_record")),
        };
        let created = u64::try_from(created).map_err(error)?;
        let version = u64::try_from(version).map_err(error)?;
        let active = u64::try_from(active).map_err(error)?;
        if created == 0 || version < created || active < version {
            return Err(error("master_session_record"));
        }
        let summary = MasterSessionSummary {
            phase,
            created: Cursor::new(created),
            last_activity: Cursor::new(active),
            started_ms: started_ms.map(u64::try_from).transpose().map_err(error)?,
            active_ms: active_ms.map(u64::try_from).transpose().map_err(error)?,
            turns: turn_count(connection, &session_id)?,
            first_line: first_line(connection, &session_id)?,
            last_line: last_line(connection, &session_id)?,
            agents: agent_names(connection, project, &session_id)?,
            results_waiting: results_waiting(connection, &session_id)?,
            session_id,
        };
        let size = serde_json::to_vec(&summary).map_err(error)?.len() + 1;
        if bytes + size > LIST_BYTES_MAX {
            break;
        }
        bytes += size;
        sessions.push(summary);
    }
    assert!(sessions.len() <= SESSIONS_LISTED_MAX);
    assert!(serde_json::to_vec(&sessions).map_err(error)?.len() <= LIST_BYTES_MAX);
    Ok(sessions)
}

fn turn_count(connection: &Connection, session: &SessionId) -> Result<u64, EngineError> {
    let count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM master_turns WHERE session=?1",
            [session.as_str()],
            |row| row.get(0),
        )
        .map_err(error)?;
    u64::try_from(count).map_err(error)
}

/// The first line of a session's first owner message, clipped for the list.
/// Takes the connection and session. Returns the line, or None before any message.
fn first_line(connection: &Connection, session: &SessionId) -> Result<Option<String>, EngineError> {
    let text: Option<Option<String>> = connection
        .query_row(
            "SELECT CASE WHEN length(CAST(c.text AS BLOB))<=8192 THEN c.text END FROM master_turns t
             JOIN conversation_entries c ON c.event_cursor=t.input AND c.kind='message'
             WHERE t.session=?1 ORDER BY t.ordinal LIMIT 1",
            [session.as_str()],
            |row| row.get(0),
        )
        .optional()
        .map_err(error)?;
    Ok(text.flatten().and_then(|text| clip(&text, LINE_CHARS_MAX)))
}

/// The newest line of a session: its last turn's reply, else that turn's owner message.
/// Takes the connection and session. Returns the clipped line, or None before any turn.
fn last_line(connection: &Connection, session: &SessionId) -> Result<Option<String>, EngineError> {
    let text: Option<Option<String>> = connection
        .query_row(
            "SELECT COALESCE(CASE WHEN length(CAST(t.body AS BLOB))<=16384 THEN json_extract(t.body,'$.reply.text') END,
                             CASE WHEN length(CAST(c.text AS BLOB))<=8192 THEN c.text END)
             FROM master_turns t LEFT JOIN conversation_entries c ON c.event_cursor=t.input AND c.kind='message'
             WHERE t.session=?1 ORDER BY t.ordinal DESC LIMIT 1",
            [session.as_str()],
            |row| row.get(0),
        )
        .optional()
        .map_err(error)?;
    Ok(text.flatten().and_then(|text| clip(&text, LINE_CHARS_MAX)))
}

/// One agent a session owns, as its master's context and the list name it.
struct OwnAgent {
    label: String,
    harness: String,
    status: String,
    result: Option<String>,
}

/// Reads the sub-agents a session owns, newest first. Takes the connection, project,
/// session and the most to read. Returns each agent's label, harness, status and the
/// result of its newest finished task.
fn own_agents(
    connection: &Connection,
    project: &ProjectId,
    session: &SessionId,
    limit: usize,
) -> Result<Vec<OwnAgent>, EngineError> {
    let mut statement = connection
        .prepare(
            "SELECT a.id,CASE WHEN length(a.harness)<=64 THEN a.harness END,CASE WHEN length(a.status)<=16 THEN a.status END,
                    (SELECT CASE WHEN length(CAST(t.result AS BLOB))<=65536 THEN t.result END FROM team_tasks t
                     WHERE t.agent=a.id AND t.result IS NOT NULL ORDER BY t.finished_ms DESC, t.id DESC LIMIT 1)
             FROM team_agents a WHERE a.project=?1 AND a.session=?2 AND a.role='sub'
             ORDER BY a.id DESC LIMIT ?3",
        )
        .map_err(error)?;
    let rows = statement
        .query_map(
            params![
                project.as_str(),
                session.as_str(),
                i64::try_from(limit).map_err(error)?
            ],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                ))
            },
        )
        .map_err(error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(error)?;
    let mut agents = Vec::with_capacity(rows.len());
    for (id, harness, status, result) in rows {
        let name = names::name(connection, id)?;
        agents.push(OwnAgent {
            label: names::label(AgentRole::Sub, id, name.as_ref()),
            harness: harness.ok_or_else(|| error("team_agent_record"))?,
            status: status.ok_or_else(|| error("team_agent_record"))?,
            result,
        });
    }
    assert!(agents.len() <= limit);
    Ok(agents)
}

fn agent_names(
    connection: &Connection,
    project: &ProjectId,
    session: &SessionId,
) -> Result<Vec<String>, EngineError> {
    Ok(own_agents(connection, project, session, LISTED_AGENTS_MAX)?
        .into_iter()
        .filter_map(|agent| clip(&agent.label, 48))
        .collect())
}

/// The engine-built state section of a session's master context: its own agents with
/// harness, state and last result, the project's open tasks, messages waiting for the
/// master and pending change sets. Project-level context is built elsewhere and is the
/// same for every session. Takes the connection, project and session. Returns the
/// section within `STATE_BYTES_MAX`, or None when there is nothing to list.
pub fn session_state(
    connection: &Connection,
    project: &ProjectId,
    session: &SessionId,
) -> Result<Option<String>, EngineError> {
    let mut sections = Vec::new();
    let agents = own_agents(connection, project, session, STATE_ROWS_MAX)?;
    if !agents.is_empty() {
        let mut text =
            String::from("Your agents in this session (message or delegate to them by name):");
        for agent in &agents {
            text.push_str("\n- ");
            text.push_str(&names::line(&agent.label, &agent.harness, &agent.status));
            if let Some(result) = agent.result.as_deref().and_then(|result| flat(result, 160)) {
                text.push_str(". Last result: ");
                text.push_str(&result);
            }
        }
        sections.push(text);
    }
    let tasks = rows(
        connection,
        "SELECT 'Task '||id||' ('||harness||', '||status||'): '||CASE WHEN length(CAST(prompt AS BLOB))<=65536 THEN prompt END
         FROM team_tasks WHERE project=?1 AND status IN ('queued','needs_pool','running') ORDER BY id LIMIT ?2",
        project,
    )?;
    if !tasks.is_empty() {
        sections.push(listed("Open tasks:", &tasks));
    }
    let messages = rows(
        connection,
        "SELECT author||': '||CASE WHEN length(CAST(text AS BLOB))<=65536 THEN text END
         FROM team_messages WHERE project=?1 AND state IN ('pending','sent') AND author<>'master' ORDER BY id LIMIT ?2",
        project,
    )?;
    if !messages.is_empty() {
        sections.push(listed("Waiting questions and messages:", &messages));
    }
    let landing: bool = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name='landing_change_sets')",
            [],
            |row| row.get(0),
        )
        .map_err(error)?;
    let found = if landing {
        crate::landing::list(connection, project)?
    } else {
        Vec::new()
    };
    let changes: Vec<String> = found
        .into_iter()
        .filter(|change| {
            matches!(
                change.state,
                crate::landing::ChangeState::Pending
                    | crate::landing::ChangeState::Conflict
                    | crate::landing::ChangeState::Updating
            )
        })
        .take(STATE_ROWS_MAX)
        .map(|change| {
            format!(
                "{} ({} files, +{} -{}), waiting for the owner",
                change.agent.label, change.files, change.added, change.removed
            )
        })
        .collect();
    if !changes.is_empty() {
        sections.push(listed("Pending change sets:", &changes));
    }
    if sections.is_empty() {
        return Ok(None);
    }
    let mut text = String::from("Session state from the engine:\n\n");
    text.push_str(&sections.join("\n\n"));
    let text = fit(text, STATE_BYTES_MAX);
    assert!(text.len() <= STATE_BYTES_MAX);
    Ok(Some(text))
}

fn rows(
    connection: &Connection,
    sql: &str,
    project: &ProjectId,
) -> Result<Vec<String>, EngineError> {
    let mut statement = connection.prepare(sql).map_err(error)?;
    let found = statement
        .query_map(
            params![
                project.as_str(),
                i64::try_from(STATE_ROWS_MAX).map_err(error)?
            ],
            |row| row.get::<_, Option<String>>(0),
        )
        .map_err(error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(error)?;
    Ok(found
        .into_iter()
        .flatten()
        .filter_map(|line| clip(&line, 200))
        .collect())
}

fn listed(header: &str, lines: &[String]) -> String {
    let mut text = String::from(header);
    for line in lines {
        text.push_str("\n- ");
        text.push_str(line);
    }
    text
}

/// Summarizes a session's own conversation for a fresh harness conversation: its owner
/// messages and replies, oldest first, newest kept when the summary must be cut.
/// Takes the connection and session. Returns the summary within `SUMMARY_BYTES_MAX`, or
/// None when the session has no turns with text.
pub fn transcript_summary(
    connection: &Connection,
    session: &MasterSessionView,
) -> Result<Option<String>, EngineError> {
    let mut statement = connection
        .prepare(
            "SELECT CASE WHEN length(CAST(c.text AS BLOB))<=8192 THEN c.text END,
                    CASE WHEN length(CAST(t.body AS BLOB))<=16384 THEN json_extract(t.body,'$.engine_request.line') END,
                    CASE WHEN length(CAST(t.body AS BLOB))<=16384 THEN json_extract(t.body,'$.reply.text') END
             FROM master_turns t LEFT JOIN conversation_entries c ON c.event_cursor=t.input AND c.kind='message' AND c.project=?2
             WHERE t.session=?1 ORDER BY t.ordinal LIMIT 1024",
        )
        .map_err(error)?;
    let turns = statement
        .query_map(
            params![session.session_id.as_str(), session.project_id.as_str()],
            |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            },
        )
        .map_err(error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(error)?;
    let header = "Summary of this session's conversation so far, oldest first:";
    let mut entries: Vec<String> = Vec::new();
    for (owner, engine, reply) in turns {
        let asked = match (owner, engine) {
            (Some(text), _) => flat(&text, 600).map(|text| format!("Owner: {text}")),
            (None, Some(line)) => flat(&line, 300).map(|line| format!("Engine: {line}")),
            (None, None) => None,
        };
        let Some(mut entry) = asked else {
            continue;
        };
        match reply.as_deref().and_then(|reply| flat(reply, 600)) {
            Some(reply) => {
                entry.push_str("\nYou: ");
                entry.push_str(&reply);
            }
            None => entry.push_str("\n(No reply was received.)"),
        }
        entries.push(entry);
    }
    if entries.is_empty() {
        return Ok(None);
    }
    let mut kept = Vec::new();
    let mut used = header.len();
    for entry in entries.iter().rev() {
        if used + entry.len() + 2 > SUMMARY_BYTES_MAX {
            break;
        }
        used += entry.len() + 2;
        kept.push(entry.as_str());
    }
    kept.reverse();
    let mut text = String::from(header);
    if kept.len() < entries.len() {
        text = String::from(
            "Summary of the newest turns of this session's conversation, oldest first:",
        );
    }
    for entry in kept {
        text.push_str("\n\n");
        text.push_str(entry);
    }
    let text = fit(text, SUMMARY_BYTES_MAX);
    assert!(text.len() <= SUMMARY_BYTES_MAX);
    Ok(Some(text))
}

/// The engine hook the live master calls before it runs a session it resumed. Today it
/// always starts a fresh harness conversation seeded with the session's transcript
/// summary and state; the live master replaces this with a real harness resume.
/// Takes the connection and the session. Returns the plan.
pub fn resume_harness(
    connection: &Connection,
    session: &MasterSessionView,
) -> Result<ResumePlan, EngineError> {
    let mut parts = Vec::new();
    if let Some(summary) = transcript_summary(connection, session)? {
        parts.push(summary);
    }
    if let Some(state) = session_state(connection, &session.project_id, &session.session_id)? {
        parts.push(state);
    }
    if parts.is_empty() {
        parts.push(String::from("This session has no earlier turns."));
    }
    let seed = parts.join("\n\n");
    assert!(seed.len() <= SUMMARY_BYTES_MAX + STATE_BYTES_MAX + 2);
    Ok(ResumePlan::Fresh {
        seed,
        note: FRESH_NOTE.into(),
    })
}

/// The first line of a text, trimmed and cut to at most `max` characters with "..."
/// marking a cut. Takes the text and bound. Returns None when nothing but space is left.
pub fn clip(text: &str, max: usize) -> Option<String> {
    let line = text.lines().map(str::trim).find(|line| !line.is_empty())?;
    flat(line, max)
}

/// A whole text on one line, every run of white space made one space, cut to at most
/// `max` characters with "..." marking a cut. Takes the text and bound. Returns None
/// when nothing but space is left.
pub fn flat(text: &str, max: usize) -> Option<String> {
    assert!(max > 3);
    let joined = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if joined.is_empty() {
        return None;
    }
    if joined.chars().count() <= max {
        return Some(joined);
    }
    let mut kept: String = joined.chars().take(max - 3).collect();
    kept.truncate(kept.trim_end().len());
    kept.push_str("...");
    assert!(kept.chars().count() <= max);
    Some(kept)
}

/// Cuts a text to at most `max` bytes on a character boundary. Takes the text and bound.
/// Returns the text, whole when it fits.
fn fit(mut text: String, max: usize) -> String {
    if text.len() > max {
        let mut end = max;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
    }
    assert!(text.len() <= max);
    text
}

fn error(value: impl std::fmt::Display) -> EngineError {
    EngineError(value.to_string())
}
