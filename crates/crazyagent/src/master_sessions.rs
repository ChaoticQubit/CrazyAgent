use crate::engine::EngineError;
use crate::master_session::MasterSessionPhase;
use crate::protocol::{Cursor, ProjectId, SessionId};
use crate::team::master::SessionConversation;
use crate::team::names;
use crate::team::{AgentRole, SwitchMode};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

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
/// The most bytes of earlier turns a master harness switch hands to the new harness.
pub const HANDOFF_BYTES_MAX: usize = 8192;
/// The line a switch handoff ends its turns with when older turns did not fit.
pub const HANDOFF_OMITTED: &str = "(Older turns were left out to fit.)";
/// The most bytes one turn takes in a switch handoff, so the newest turn always fits.
pub const HANDOFF_TURN_BYTES_MAX: usize = HANDOFF_BYTES_MAX / 4;
/// The line a handed-over turn shows where its earlier replies did not fit.
pub const TURN_OMITTED: &str = "(Earlier replies in this turn were left out.)";
/// The fewest bytes of room an older turn is cut into rather than left out.
const HANDOFF_TURN_MIN_BYTES: usize = 160;
/// The longest title a handoff part has, naming the conversation it comes from.
const HANDOFF_TITLE_BYTES_MAX: usize = 128;

const MARKS: &str = "CREATE TABLE IF NOT EXISTS session_marks (
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

/// How the live master runs a project's live session. `Resume` continues the harness
/// conversation the session kept, by its id, in the home of `agent` where `record` keeps
/// it. `Fresh` starts a new harness conversation whose first input is `seed`, the
/// engine-built summary of the session's earlier turns and its state, and shows `note` in
/// the master view so history is never dropped silently. `New` starts a session that has
/// no earlier turns.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ResumePlan {
    Resume {
        agent: i64,
        conversation: String,
        record: PathBuf,
    },
    Fresh {
        seed: String,
        note: String,
    },
    New,
}

/// The note a resumed session shows when its harness conversation starts fresh.
pub const FRESH_NOTE: &str = "This session continues in a new harness conversation, started with a summary of its earlier turns.";
/// The note a resumed session shows when its earlier harness conversation can no longer be read.
pub const LOST_NOTE: &str = "This session continues in a new harness conversation. Its earlier harness conversation could not be read, so only the session's state was passed on.";

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
        "INSERT INTO session_marks(session,project,started_ms,active_ms) VALUES (?1,?2,?3,?3)
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
             FROM master_sessions s LEFT JOIN session_marks m ON m.session=s.id
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
/// Takes the connection, project and session. Returns the summary within
/// `SUMMARY_BYTES_MAX`, or None when the session has no turns with text.
pub fn transcript_summary(
    connection: &Connection,
    project: &ProjectId,
    session: &SessionId,
) -> Result<Option<String>, EngineError> {
    let entries = turn_entries(connection, project, session)?;
    Ok(newest(
        "Summary of this session's conversation so far, oldest first:",
        "Summary of the newest turns of this session's conversation, oldest first:",
        &entries,
    ))
}

/// Reads a session's engine turns as one entry each, oldest first: the owner's message or
/// the engine's line, then the reply or a no-reply line, each flattened and clipped.
/// Takes the connection, project and session. Returns the entries of turns with text.
fn turn_entries(
    connection: &Connection,
    project: &ProjectId,
    session: &SessionId,
) -> Result<Vec<String>, EngineError> {
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
        .query_map(params![session.as_str(), project.as_str()], |row| {
            Ok((
                row.get::<_, Option<String>>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, Option<String>>(2)?,
            ))
        })
        .map_err(error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(error)?;
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
    Ok(entries)
}

/// Keeps the newest entries of a summary that fit `SUMMARY_BYTES_MAX`, oldest first under
/// `header`, or under `cut` when older entries had to go. Takes the two headers and the
/// entries oldest first. Returns the summary, or None without entries.
fn newest(header: &str, cut: &str, entries: &[String]) -> Option<String> {
    if entries.is_empty() {
        return None;
    }
    let mut kept = Vec::new();
    let mut used = header.len().max(cut.len());
    for entry in entries.iter().rev() {
        if used + entry.len() + 2 > SUMMARY_BYTES_MAX {
            break;
        }
        used += entry.len() + 2;
        kept.push(entry.as_str());
    }
    kept.reverse();
    let mut text = String::from(if kept.len() < entries.len() {
        cut
    } else {
        header
    });
    for entry in kept {
        text.push_str("\n\n");
        text.push_str(entry);
    }
    let text = fit(text, SUMMARY_BYTES_MAX);
    assert!(text.len() <= SUMMARY_BYTES_MAX);
    Some(text)
}

/// Summarizes the harness conversation a session kept, read from its record: what was
/// typed into the master and its replies, oldest first, newest kept when it must be cut.
/// Takes the kept conversation. Returns the summary within `SUMMARY_BYTES_MAX`, or None
/// when the record cannot be read or holds no messages.
fn record_summary(kept: &SessionConversation) -> Option<String> {
    let messages =
        crate::harnesses::transcript::conversation_messages(&kept.harness, &kept.resume.record);
    let entries: Vec<String> = messages
        .iter()
        .filter_map(|(speaker, text)| {
            let said = flat(text, 600)?;
            Some(match speaker {
                crate::harnesses::transcript::Speaker::Input => format!("Typed to you: {said}"),
                crate::harnesses::transcript::Speaker::Agent => format!("You: {said}"),
            })
        })
        .collect();
    newest(
        "Your earlier harness conversation in this session, oldest first:",
        "The newest part of your earlier harness conversation in this session, oldest first:",
        &entries,
    )
}

/// The engine hook the live master calls before it starts a project's live session. It
/// continues the harness conversation the session kept when the master about to start
/// can: the same harness and account, the session's master agent stopped with its own
/// worktree, an id the harness resumes by and the record still a plain file. Otherwise
/// the session starts as `fresh_harness` says. Takes the connection, project, session,
/// the master's launch harness and the account connection. Returns the plan.
pub fn resume_harness(
    connection: &Connection,
    project: &ProjectId,
    session: &SessionId,
    harness: &str,
    account: &str,
) -> Result<ResumePlan, EngineError> {
    let kept = crate::team::master::session_conversation(connection, project, session.as_str())?;
    if let Some(kept) = &kept
        && crate::harnesses::same(&kept.harness, harness)
        && kept.connection == account
        && crate::harnesses::interactive::resumable(&kept.resume.id)
        && std::fs::symlink_metadata(&kept.resume.record).is_ok_and(|meta| meta.is_file())
        && stopped_master(connection, project, kept.agent)?
    {
        return Ok(ResumePlan::Resume {
            agent: kept.agent,
            conversation: kept.resume.id.clone(),
            record: kept.resume.record.clone(),
        });
    }
    fresh_harness(connection, project, session, kept.as_ref())
}

/// How a session's master starts when its kept harness conversation cannot continue: a
/// session with earlier turns starts a fresh harness conversation seeded with the summary
/// of its engine turns, its kept harness conversation and its state, with a note saying
/// so; a session without starts new. A record the reader refused at a resume is not read.
/// Takes the connection, project, session and the kept conversation. Returns `Fresh` or
/// `New`.
pub fn fresh_harness(
    connection: &Connection,
    project: &ProjectId,
    session: &SessionId,
    kept: Option<&SessionConversation>,
) -> Result<ResumePlan, EngineError> {
    let mut parts = Vec::new();
    if let Some(summary) = transcript_summary(connection, project, session)? {
        parts.push(summary);
    }
    let read = kept
        .filter(|kept| kept.resume.id != crate::team::master::REFUSED_CONVERSATION)
        .and_then(record_summary);
    let lost = kept.is_some() && read.is_none();
    parts.extend(read);
    if parts.is_empty() && !lost {
        return Ok(ResumePlan::New);
    }
    let note = if parts.is_empty() {
        LOST_NOTE
    } else {
        FRESH_NOTE
    };
    if let Some(state) = session_state(connection, project, session)? {
        parts.push(state);
    }
    if parts.is_empty() {
        parts.push(String::from(
            "This session's earlier conversation could not be read.",
        ));
    }
    let seed = parts.join("\n\n");
    assert!(seed.len() <= 2 * SUMMARY_BYTES_MAX + STATE_BYTES_MAX + 4);
    Ok(ResumePlan::Fresh {
        seed,
        note: note.into(),
    })
}

/// Tells which harness a session's conversation last ran on when the master about to start
/// runs another: the target of its last applied switch (seen once, even before the new
/// harness reports a conversation), else the kept conversation's, else, with engine turns,
/// the last master's; aliases count as one. Takes the connection, project, session and the
/// harness about to start. Returns the earlier harness, or None when unchanged or unknown.
pub fn switched_from(
    connection: &Connection,
    project: &ProjectId,
    session: &SessionId,
    harness: &str,
) -> Result<Option<String>, EngineError> {
    let switched: Option<String> = connection
        .query_row(
            "SELECT CASE WHEN length(CAST(harness AS BLOB))<=64 THEN harness END
             FROM team_session_switches WHERE session=?1 AND project=?2",
            params![session.as_str(), project.as_str()],
            |row| row.get(0),
        )
        .optional()
        .map_err(error)?
        .flatten();
    let kept = crate::team::master::session_conversation(connection, project, session.as_str())?;
    let earlier = match (switched, kept) {
        (Some(switched), _) => Some(switched),
        (None, Some(kept)) => Some(kept.harness),
        (None, None) if turn_count(connection, session)? > 0 => connection
            .query_row(
                "SELECT CASE WHEN length(harness)<=64 THEN harness END FROM team_agents
                 WHERE project=?1 AND role='master' ORDER BY id DESC LIMIT 1",
                [project.as_str()],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()
            .map_err(error)?
            .flatten(),
        (None, None) => None,
    };
    Ok(earlier.filter(|earlier| !crate::harnesses::same(earlier, harness)))
}

/// Tells whether a session's master starts new because its last switch, to this harness
/// under Start fresh, is not yet followed by a conversation the new harness reported: the
/// kept conversation, if any, is still another harness's, which the owner chose not to
/// carry over (HSL-1). Takes the connection, project, session and the harness about to
/// start. Returns whether to start new.
pub fn fresh_after_switch(
    connection: &Connection,
    project: &ProjectId,
    session: &SessionId,
    harness: &str,
) -> Result<bool, EngineError> {
    let switch: Option<(Option<String>, String)> = connection
        .query_row(
            "SELECT CASE WHEN length(CAST(harness AS BLOB))<=64 THEN harness END, mode
             FROM team_session_switches WHERE session=?1 AND project=?2",
            params![session.as_str(), project.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(error)?;
    let Some((Some(to), mode)) = switch else {
        return Ok(false);
    };
    if !crate::harnesses::same(&to, harness) || SwitchMode::named(&mode) != Some(SwitchMode::Fresh)
    {
        return Ok(false);
    }
    let kept = crate::team::master::session_conversation(connection, project, session.as_str())?;
    Ok(kept.is_none_or(|kept| !crate::harnesses::same(&kept.harness, harness)))
}

/// The display name of a harness: its catalog name, or the stored name the catalog does
/// not know. Takes the stored name. Returns the name.
pub fn harness_label(harness: &str) -> &str {
    crate::harnesses::spec(harness).map_or(harness, |spec| spec.name)
}

/// The note a session's conversation shows where its master harness switched, read as a
/// divider in the conversation: what the new harness was given under the owner's setting.
/// Takes the new harness's display name and the mode. Returns the note.
pub fn switched_note(to: &str, mode: SwitchMode) -> String {
    match mode {
        SwitchMode::Carry => format!(
            "Switched to {to}. The conversation continues: {to} got its earlier turns, newest first, and the open tasks."
        ),
        SwitchMode::Fresh => format!(
            "Switched to {to}. Started fresh: {to} did not get the earlier conversation, as the On harness switch setting asks."
        ),
    }
}

/// Builds the Carry over handoff the new master harness reads first: a line naming the
/// switch, then one part per earlier conversation, the kept harness conversation first and
/// the engine's master conversation second, each newest first within its share of
/// `HANDOFF_BYTES_MAX`, then the open tasks. Takes the connection, project, session, the
/// kept conversation and the earlier harness's display name. Returns the handoff.
pub fn handoff(
    connection: &Connection,
    project: &ProjectId,
    session: &SessionId,
    kept: Option<&SessionConversation>,
    from: &str,
) -> Result<String, EngineError> {
    let mut parts: Vec<(String, Vec<String>)> = Vec::new();
    if let Some(kept) =
        kept.filter(|kept| kept.resume.id != crate::team::master::REFUSED_CONVERSATION)
    {
        let entries = record_turns(kept);
        if !entries.is_empty() {
            let label = harness_label(&kept.harness);
            parts.push((
                fit(
                    format!("From the {label} conversation, newest first:"),
                    HANDOFF_TITLE_BYTES_MAX,
                ),
                entries,
            ));
        }
    }
    let engine = turn_entries(connection, project, session)?;
    if !engine.is_empty() {
        parts.push((
            "From CrazyAgent's master conversation, newest first:".to_owned(),
            engine,
        ));
    }
    let header = if parts.is_empty() {
        format!(
            "The owner switched this conversation's master from {from} to you. Its earlier turns could not be read."
        )
    } else {
        format!(
            "The owner switched this conversation's master from {from} to you. Its earlier turns, newest first:"
        )
    };
    let mut text = fit(header, HANDOFF_BYTES_MAX / 4);
    let mut left = HANDOFF_BYTES_MAX - text.len() - 2 * parts.len();
    for (index, (title, entries)) in parts.iter().enumerate() {
        let share = match parts.get(index + 1) {
            Some((next_title, next)) => {
                (left / 2).max(left.saturating_sub(part_bytes(next_title, next)))
            }
            None => left,
        };
        let part = handoff_part(title, entries, share);
        left -= part.len();
        text.push_str("\n\n");
        text.push_str(&part);
    }
    assert!(text.len() <= HANDOFF_BYTES_MAX);
    if let Some(state) = session_state(connection, project, session)? {
        text.push_str("\n\n");
        text.push_str(&state);
    }
    assert!(text.len() <= HANDOFF_BYTES_MAX + STATE_BYTES_MAX + 2);
    Ok(text)
}

/// The room a handoff part needs to hand every entry over whole, its omitted line included.
/// Takes the part's title and entries. Returns the size.
fn part_bytes(title: &str, entries: &[String]) -> usize {
    let room = title.len() + 2 + HANDOFF_OMITTED.len();
    entries.iter().fold(room, |bytes, entry| {
        bytes + 2 + entry.len().min(HANDOFF_TURN_BYTES_MAX)
    })
}

/// One part of a switch handoff within `room` bytes: its title, then its entries newest
/// first, each cut to `HANDOFF_TURN_BYTES_MAX` so the newest always fits, the one crossing
/// the room cut to what is left, every cut ending in "...", and `HANDOFF_OMITTED` when any
/// were cut or left out. Takes the title, entries oldest first and the room. Returns it.
fn handoff_part(title: &str, entries: &[String], room: usize) -> String {
    let budget = room - HANDOFF_OMITTED.len() - 2;
    let mut text = title.to_owned();
    let mut given = 0;
    let mut cut = false;
    for entry in entries.iter().rev() {
        let entry = marked(entry, HANDOFF_TURN_BYTES_MAX);
        let left = budget.saturating_sub(text.len() + 2);
        let whole = entry.len() <= left;
        if !whole && left < HANDOFF_TURN_MIN_BYTES {
            break;
        }
        text.push_str("\n\n");
        text.push_str(&marked(&entry, left));
        given += 1;
        if !whole {
            cut = true;
            break;
        }
    }
    assert!(given > 0, "the newest turn of a part is always handed over");
    if cut || given < entries.len() {
        text.push_str("\n\n");
        text.push_str(HANDOFF_OMITTED);
    }
    assert!(text.len() <= room);
    text
}

/// Reads the conversation a session kept as one entry per turn, oldest first: what was typed
/// to the master with the replies that followed it. Takes the kept conversation. Returns the
/// entries, none when its record cannot be read.
fn record_turns(kept: &SessionConversation) -> Vec<String> {
    let messages =
        crate::harnesses::transcript::conversation_messages(&kept.harness, &kept.resume.record);
    let mut turns: Vec<(Option<String>, Vec<String>)> = Vec::new();
    for (speaker, text) in &messages {
        let Some(said) = flat(text, 600) else {
            continue;
        };
        match (speaker, turns.last_mut()) {
            (crate::harnesses::transcript::Speaker::Agent, Some((_, replies))) => {
                replies.push(said);
            }
            (crate::harnesses::transcript::Speaker::Agent, None) => turns.push((None, vec![said])),
            (crate::harnesses::transcript::Speaker::Input, _) => turns.push((Some(said), vec![])),
        }
    }
    turns
        .iter()
        .map(|(input, replies)| bounded_turn(input.as_deref(), replies))
        .collect()
}

/// One handed-over turn within `HANDOFF_TURN_BYTES_MAX`: what was typed, then its last
/// replies that fit, with `TURN_OMITTED` where earlier replies were left out and the last
/// reply cut with "..." when even it does not fit. Takes the typed input, if any, and the
/// replies oldest first. Returns the turn's lines.
fn bounded_turn(input: Option<&str>, replies: &[String]) -> String {
    let head = input.map(|input| {
        marked(
            &format!("Typed to you: {input}"),
            HANDOFF_TURN_BYTES_MAX / 2,
        )
    });
    let room = HANDOFF_TURN_BYTES_MAX
        - head.as_ref().map_or(0, |head| head.len() + 1)
        - TURN_OMITTED.len()
        - 1;
    let mut kept: Vec<String> = Vec::new();
    let mut used = 0;
    for reply in replies.iter().rev() {
        let line = format!("You: {reply}");
        if used + line.len() + 1 > room {
            if kept.is_empty() {
                kept.push(marked(&line, room - 1));
            }
            break;
        }
        used += line.len() + 1;
        kept.push(line);
    }
    let mut lines: Vec<String> = head.into_iter().collect();
    if kept.len() < replies.len() {
        lines.push(TURN_OMITTED.to_owned());
    }
    lines.extend(kept.into_iter().rev());
    let text = lines.join("\n");
    assert!(text.len() <= HANDOFF_TURN_BYTES_MAX);
    text
}

/// Tells whether a team agent is a stopped master of the project with its worktree still
/// recorded, so it can start again in the home that keeps its harness conversations.
/// Takes the connection, project and agent. Returns the answer.
fn stopped_master(
    connection: &Connection,
    project: &ProjectId,
    agent: i64,
) -> Result<bool, EngineError> {
    connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM team_agents a JOIN team_worktrees w ON w.agent=a.id AND w.project=a.project
             WHERE a.id=?1 AND a.project=?2 AND a.role='master' AND a.status IN ('stopped','failed'))",
            params![agent, project.as_str()],
            |row| row.get(0),
        )
        .map_err(error)
}

/// The first line of a text with visible words, made one line by `flat`.
/// Takes the text and bound. Returns None when no line has visible words.
pub fn clip(text: &str, max: usize) -> Option<String> {
    text.lines().find_map(|line| flat(line, max))
}

/// A whole text on one line, every control character and run of white space made one
/// space, so no terminal escape reaches a screen, cut to at most `max` characters with
/// "..." marking a cut. Takes the text and bound. Returns None when nothing visible is left.
pub fn flat(text: &str, max: usize) -> Option<String> {
    assert!(max > 3);
    let visible: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let joined = visible.split_whitespace().collect::<Vec<_>>().join(" ");
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
    assert!(!kept.chars().any(char::is_control));
    Some(kept)
}

/// Cuts a text to at most `max` bytes on a character boundary, ending a cut with "..." as
/// `flat` marks one, so a handed-over turn never loses its end silently. Takes the text and
/// the bound, over 3 when the text must be cut. Returns the text, whole when it fits.
fn marked(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_owned();
    }
    assert!(max > 3);
    let mut kept = fit(text.to_owned(), max - 3);
    kept.push_str("...");
    assert!(kept.len() <= max);
    kept
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

#[cfg(test)]
mod tests {
    use super::{
        HANDOFF_BYTES_MAX, HANDOFF_OMITTED, HANDOFF_TURN_BYTES_MAX, LOST_NOTE, ResumePlan,
        STATE_BYTES_MAX, TURN_OMITTED, clip, flat, handoff, resume_harness, switched_from,
    };
    use crate::harnesses::terminal_session::Resume;
    use crate::protocol::{ProjectId, SessionId};
    use crate::team::master::{SessionConversation, keep_conversation};
    use std::time::Duration;

    /// A switch handoff gives the new harness the session's earlier turns newest first, as
    /// many as fit its byte bound with a line saying older ones were left out, then the
    /// session's open tasks; and the switch is seen only for another harness, never for an
    /// alias of the one the conversation ran on.
    #[test]
    fn a_switch_handoff_is_newest_first_bounded_and_lists_open_tasks() {
        let folder = tempfile::tempdir().unwrap();
        let connection = rusqlite::Connection::open(folder.path().join("engine.db")).unwrap();
        crate::storage::initialize_authority_schemas(&connection).unwrap();
        let owner = connection.unchecked_transaction().unwrap();
        crate::conversation::initialize(&owner).unwrap();
        owner.commit().unwrap();
        crate::team::initialize(&connection).unwrap();
        let project = ProjectId::new("switch-project").unwrap();
        let session = SessionId::new("s-switch").unwrap();
        let id = "0f8c2c1e-5a7b-4c3d-9e2f-1a2b3c4d5e6f";
        let record = folder.path().join(format!("{id}.jsonl"));
        let mut lines = String::new();
        for turn in 1..=30 {
            let words = format!("turn {turn:02} ").repeat(60);
            lines.push_str(
                &serde_json::json!({"type": "user", "message": {"content": words}}).to_string(),
            );
            lines.push('\n');
            lines.push_str(
                &serde_json::json!({"type": "assistant", "message": {"content": [{"type": "text", "text": format!("answer {turn:02}")}], "stop_reason": "end_turn"}})
                    .to_string(),
            );
            lines.push('\n');
        }
        std::fs::write(&record, lines).unwrap();
        let kept = SessionConversation {
            agent: 1,
            harness: "claude-code".into(),
            connection: "local-a".into(),
            resume: Resume {
                id: id.into(),
                record,
            },
        };
        keep_conversation(&connection, &project, session.as_str(), &kept).unwrap();
        connection
            .execute(
                "INSERT INTO team_tasks(project,source,harness,tools,prompt,status,created_ms)
                 VALUES (?1,'owner','codex',0,'Write the release notes','queued',1)",
                [project.as_str()],
            )
            .unwrap();

        assert_eq!(
            switched_from(&connection, &project, &session, "claude").unwrap(),
            None,
            "an alias of the conversation's harness is no switch"
        );
        assert_eq!(
            switched_from(&connection, &project, &session, "codex").unwrap(),
            Some("claude-code".to_owned())
        );

        let text = handoff(&connection, &project, &session, Some(&kept), "Claude Code").unwrap();
        assert!(
            text.starts_with(
                "The owner switched this conversation's master from Claude Code to you. Its earlier turns, newest first:"
            ),
            "{text}"
        );
        let newest = text
            .find("answer 30")
            .expect("the newest turn is handed over");
        let older = text
            .find("answer 29")
            .expect("the next newest turn follows");
        assert!(newest < older, "newest first: {text}");
        assert!(!text.contains("answer 01"), "the oldest turn did not fit");
        let turns_end = text.find(HANDOFF_OMITTED).expect("the cut is said");
        assert!(turns_end <= HANDOFF_BYTES_MAX);
        assert!(
            text.contains("Open tasks:\n- Task 1 (codex, queued): Write the release notes"),
            "{text}"
        );
        assert!(text.len() <= HANDOFF_BYTES_MAX + STATE_BYTES_MAX + 2);
    }

    /// A tool-heavy newest turn, one typed input followed by many replies that together
    /// exceed the whole handoff, is cut to fit, never dropped: the new harness gets that
    /// turn's input and its last replies, a line saying earlier replies were left out, and
    /// the older turns after it.
    #[test]
    fn a_long_newest_turn_is_cut_to_fit_the_handoff_not_dropped() {
        let folder = tempfile::tempdir().unwrap();
        let connection = rusqlite::Connection::open(folder.path().join("engine.db")).unwrap();
        crate::storage::initialize_authority_schemas(&connection).unwrap();
        let owner = connection.unchecked_transaction().unwrap();
        crate::conversation::initialize(&owner).unwrap();
        owner.commit().unwrap();
        crate::team::initialize(&connection).unwrap();
        let project = ProjectId::new("switch-project").unwrap();
        let session = SessionId::new("s-switch").unwrap();
        let id = "1f8c2c1e-5a7b-4c3d-9e2f-1a2b3c4d5e6f";
        let record = folder.path().join(format!("{id}.jsonl"));
        let user = |text: &str| {
            serde_json::json!({"type": "user", "message": {"content": text}}).to_string() + "\n"
        };
        let assistant = |text: &str| {
            serde_json::json!({"type": "assistant", "message": {"content": [{"type": "text", "text": text}], "stop_reason": "end_turn"}})
                .to_string()
                + "\n"
        };
        let mut lines = String::new();
        for turn in 1..=3 {
            lines.push_str(&user(&format!("older {turn:02}")));
            lines.push_str(&assistant(&format!("answer {turn:02}")));
        }
        lines.push_str(&user("implement the export"));
        for reply in 1..=25 {
            lines.push_str(&assistant(&format!("reply {reply:02} ").repeat(44)));
        }
        std::fs::write(&record, lines).unwrap();
        let kept = SessionConversation {
            agent: 1,
            harness: "claude-code".into(),
            connection: "local-a".into(),
            resume: Resume {
                id: id.into(),
                record,
            },
        };

        let text = handoff(&connection, &project, &session, Some(&kept), "Claude Code").unwrap();
        let newest = text
            .find("Typed to you: implement the export")
            .expect("the newest turn's input is handed over");
        let cut = text.find(TURN_OMITTED).expect("the cut replies are said");
        let last = text
            .find("You: reply 25 reply 25")
            .expect("the newest turn's last reply is handed over");
        let older = text
            .find("Typed to you: older 03\nYou: answer 03")
            .expect("the older turns follow");
        assert!(newest < cut && cut < last && last < older, "{text}");
        assert!(
            !text.contains("reply 01"),
            "the earliest replies did not fit"
        );
        assert!(
            text.contains("Typed to you: older 01\nYou: answer 01"),
            "every older turn fits: {text}"
        );
        assert!(!text.contains(HANDOFF_OMITTED), "{text}");
        let turn = &text[newest..older];
        assert!(turn.len() <= HANDOFF_TURN_BYTES_MAX + 2, "{}", turn.len());
        assert!(text.len() <= HANDOFF_BYTES_MAX + STATE_BYTES_MAX + 2);
    }

    /// A session with turns in both the kept harness conversation and the engine's master
    /// conversation hands over each in its own part, newest first, so the newest engine
    /// turns are never crowded out by a long harness conversation.
    #[test]
    fn a_switch_handoff_keeps_a_part_for_each_conversation_newest_first() {
        let folder = tempfile::tempdir().unwrap();
        let connection = rusqlite::Connection::open(folder.path().join("engine.db")).unwrap();
        crate::storage::initialize_authority_schemas(&connection).unwrap();
        let owner = connection.unchecked_transaction().unwrap();
        crate::conversation::initialize(&owner).unwrap();
        owner.commit().unwrap();
        crate::team::initialize(&connection).unwrap();
        let project = ProjectId::new("switch-project").unwrap();
        let session = SessionId::new("s-parts").unwrap();
        for turn in 1..=3_i64 {
            let body = serde_json::json!({
                "engine_request": {"line": format!("Check the build {turn:02}")},
                "reply": {"text": format!("Build reply {turn:02}")},
            });
            connection
                .execute(
                    "INSERT INTO master_turns(session,ordinal,id,input,phase,body) VALUES (?1,?2,?3,?4,'succeeded',?5)",
                    rusqlite::params![
                        session.as_str(),
                        turn,
                        format!("turn-{turn}"),
                        1000 + turn,
                        body.to_string()
                    ],
                )
                .unwrap();
        }
        let id = "2f8c2c1e-5a7b-4c3d-9e2f-1a2b3c4d5e6f";
        let record = folder.path().join(format!("{id}.jsonl"));
        let mut lines = String::new();
        for turn in 1..=20 {
            let words = format!("turn {turn:02} ").repeat(60);
            lines.push_str(
                &serde_json::json!({"type": "user", "message": {"content": words}}).to_string(),
            );
            lines.push('\n');
            lines.push_str(
                &serde_json::json!({"type": "assistant", "message": {"content": [{"type": "text", "text": format!("answer {turn:02}")}], "stop_reason": "end_turn"}})
                    .to_string(),
            );
            lines.push('\n');
        }
        std::fs::write(&record, lines).unwrap();
        let kept = SessionConversation {
            agent: 1,
            harness: "claude-code".into(),
            connection: "local-a".into(),
            resume: Resume {
                id: id.into(),
                record,
            },
        };

        let text = handoff(&connection, &project, &session, Some(&kept), "Claude Code").unwrap();
        let terminal = text
            .find("From the Claude Code conversation, newest first:")
            .expect("the harness conversation has its part");
        let newest_record = text
            .find("answer 20")
            .expect("its newest turn is handed over");
        let older_record = text.find("answer 19").expect("then the one before");
        let engine = text
            .find("From CrazyAgent's master conversation, newest first:")
            .expect("the engine conversation has its part");
        let newest_engine = text
            .find("Engine: Check the build 03\nYou: Build reply 03")
            .expect("the newest engine turn is handed over");
        let oldest_engine = text
            .find("Engine: Check the build 01\nYou: Build reply 01")
            .expect("every engine turn fits");
        assert!(
            terminal < newest_record
                && newest_record < older_record
                && older_record < engine
                && engine < newest_engine
                && newest_engine < oldest_engine,
            "{text}"
        );
        assert!(
            text.contains(HANDOFF_OMITTED),
            "the harness part was cut: {text}"
        );
        assert!(!text.contains("answer 01"), "{text}");
        assert!(text.len() <= HANDOFF_BYTES_MAX + STATE_BYTES_MAX + 2);
    }

    /// A switch is seen once: after the session's master switched to Codex, a later start on
    /// Codex is no switch even before Codex reported a conversation of its own, and a start
    /// on Claude Code again is a switch back from Codex.
    #[test]
    fn a_switch_is_seen_once_even_before_the_new_harness_reports_its_conversation() {
        let folder = tempfile::tempdir().unwrap();
        let connection = rusqlite::Connection::open(folder.path().join("engine.db")).unwrap();
        crate::storage::initialize_authority_schemas(&connection).unwrap();
        let owner = connection.unchecked_transaction().unwrap();
        crate::conversation::initialize(&owner).unwrap();
        owner.commit().unwrap();
        crate::team::initialize(&connection).unwrap();
        let project = ProjectId::new("switch-project").unwrap();
        let session = SessionId::new("s-once").unwrap();
        let id = "3f8c2c1e-5a7b-4c3d-9e2f-1a2b3c4d5e6f";
        let record = folder.path().join(format!("{id}.jsonl"));
        std::fs::write(&record, "").unwrap();
        let kept = SessionConversation {
            agent: 1,
            harness: "claude-code".into(),
            connection: "local-a".into(),
            resume: Resume {
                id: id.into(),
                record,
            },
        };
        keep_conversation(&connection, &project, session.as_str(), &kept).unwrap();
        assert_eq!(
            switched_from(&connection, &project, &session, "codex").unwrap(),
            Some("claude-code".to_owned())
        );
        crate::team::record_switch(
            &connection,
            &project,
            session.as_str(),
            "codex",
            crate::team::SwitchMode::Carry,
        )
        .unwrap();
        assert_eq!(
            switched_from(&connection, &project, &session, "codex").unwrap(),
            None,
            "the switch to Codex was applied already"
        );
        assert_eq!(
            switched_from(&connection, &project, &session, "claude-code").unwrap(),
            Some("codex".to_owned())
        );
    }

    /// An engine turn of multi-byte text longer than one handed-over turn is cut with a
    /// visible "..." where its reply was cut, never silently.
    #[test]
    fn a_multi_byte_engine_turn_cut_to_fit_shows_where_it_was_cut() {
        let folder = tempfile::tempdir().unwrap();
        let connection = rusqlite::Connection::open(folder.path().join("engine.db")).unwrap();
        crate::storage::initialize_authority_schemas(&connection).unwrap();
        let owner = connection.unchecked_transaction().unwrap();
        crate::conversation::initialize(&owner).unwrap();
        owner.commit().unwrap();
        crate::team::initialize(&connection).unwrap();
        let project = ProjectId::new("switch-project").unwrap();
        let session = SessionId::new("s-wide").unwrap();
        let body = serde_json::json!({
            "engine_request": {"line": "検".repeat(300)},
            "reply": {"text": "答".repeat(600)},
        });
        connection
            .execute(
                "INSERT INTO master_turns(session,ordinal,id,input,phase,body) VALUES (?1,1,'turn-1',1001,'succeeded',?2)",
                rusqlite::params![session.as_str(), body.to_string()],
            )
            .unwrap();
        let text = handoff(&connection, &project, &session, None, "Codex").unwrap();
        let turn = text
            .find(&format!("Engine: {}", "検".repeat(300)))
            .expect("the engine turn is handed over");
        let reply = text[turn..].find("\nYou: 答").expect("its reply starts") + turn;
        assert!(text[reply..].contains("答..."), "the cut is shown: {text}");
        assert!(!text.contains(&"答".repeat(600)), "{text}");
        assert!(text.len() <= HANDOFF_BYTES_MAX + STATE_BYTES_MAX + 2);
    }

    #[test]
    fn listed_lines_hold_no_control_characters_and_stay_bounded() {
        assert_eq!(
            flat("Ship \u{1b}[31mnow\u{7}\u{0} please", 80).as_deref(),
            Some("Ship [31mnow please")
        );
        assert_eq!(
            clip("\n  \u{1b}\u{8}\n First line \r\nSecond", 80).as_deref(),
            Some("First line")
        );
        assert_eq!(flat("\u{1b}\u{7} \t", 80), None);
        let long = flat(&"word ".repeat(40), 12).unwrap();
        assert_eq!(long, "word word...");
        assert!(long.chars().count() <= 12);
    }

    /// A FIFO left at a kept harness record, as a tools-on master could make in its own
    /// home, is never opened for a blocking read: the session's start returns at once with a
    /// fresh conversation and the note that the earlier one could not be read.
    #[cfg(unix)]
    #[test]
    fn a_fifo_at_a_kept_record_falls_back_at_once_with_the_lost_note() {
        let folder = tempfile::tempdir().unwrap();
        let database = folder.path().join("engine.db");
        let connection = rusqlite::Connection::open(&database).unwrap();
        crate::storage::initialize_authority_schemas(&connection).unwrap();
        let owner = connection.unchecked_transaction().unwrap();
        crate::conversation::initialize(&owner).unwrap();
        owner.commit().unwrap();
        let id = "0f8c2c1e-5a7b-4c3d-9e2f-1a2b3c4d5e6f";
        let record = folder.path().join(format!("{id}.jsonl"));
        let made = crate::spawn_gate::status(
            std::process::Command::new("/usr/bin/mkfifo")
                .arg(&record)
                .env_clear()
                .stdin(std::process::Stdio::null()),
        )
        .unwrap();
        assert!(made.success());
        let project = ProjectId::new("fifo-project").unwrap();
        let kept = SessionConversation {
            agent: 1,
            harness: "claude-code".into(),
            connection: "local-a".into(),
            resume: Resume {
                id: id.into(),
                record,
            },
        };
        keep_conversation(&connection, &project, "s-fifo", &kept).unwrap();
        drop(connection);
        let (sent, got) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let connection = rusqlite::Connection::open(&database).unwrap();
            let plan = resume_harness(
                &connection,
                &project,
                &SessionId::new("s-fifo").unwrap(),
                "claude-code",
                "local-a",
            );
            let _ = sent.send(plan.map_err(|problem| problem.0));
        });
        let plan = got
            .recv_timeout(Duration::from_secs(10))
            .expect("a FIFO at the kept record must not block the session's start")
            .unwrap();
        let ResumePlan::Fresh { note, .. } = &plan else {
            panic!("a FIFO is no record to resume: {plan:?}");
        };
        assert_eq!(note, LOST_NOTE);
    }
}
