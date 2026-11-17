use super::*;
use crate::master_session::{SESSION_DELETED, SessionDeleted};
use crate::state_log::replay::Logged;

/// The harness conversation a master session continues: the team agent whose home keeps
/// the record, its harness and account, and the id and record the harness resumes by.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionConversation {
    pub agent: i64,
    pub harness: String,
    pub connection: String,
    pub resume: Resume,
}

/// Keeps the harness conversation a project's master session continues, replacing the one
/// kept before, so resuming the session continues it. Takes the engine database, the
/// project, the session and the conversation. Returns any SQLite failure; a record path
/// that is not plain text or too long is not kept.
pub(crate) fn keep_conversation(
    connection: &Connection,
    project: &ProjectId,
    session: &str,
    kept: &SessionConversation,
) -> Result<(), EngineError> {
    let Some(record) = kept.resume.record.to_str() else {
        return Ok(());
    };
    if record.len() > RECORD_PATH_BYTES_MAX || session.is_empty() || session.len() > 128 {
        return Ok(());
    }
    assert!(!kept.resume.id.is_empty() && kept.resume.id.len() <= 128);
    connection
        .execute(
            "INSERT OR REPLACE INTO team_master_conversations(session,project,agent,harness,connection,conversation,record)
             VALUES (?1,?2,?3,?4,?5,?6,?7)",
            params![
                session,
                project.as_str(),
                kept.agent,
                kept.harness,
                kept.connection,
                kept.resume.id,
                record
            ],
        )
        .map_err(state)?;
    Ok(())
}

/// The conversation id kept for a session whose record the harness reader refused at a
/// resume: never resumed, and its record is never read for a summary again.
pub(crate) const REFUSED_CONVERSATION: &str = "refused";

/// Marks the harness conversation a project's master session kept as refused, after the
/// reader would not follow its record at a resume, so the next start begins a fresh
/// conversation instead of trying the same resume again. Takes the engine database, the
/// project and the session. Returns any SQLite failure; nothing changes without a kept row.
pub(crate) fn refuse_conversation(
    connection: &Connection,
    project: &ProjectId,
    session: &str,
) -> Result<(), EngineError> {
    connection
        .execute(
            "UPDATE team_master_conversations SET conversation=?3 WHERE session=?1 AND project=?2",
            params![session, project.as_str(), REFUSED_CONVERSATION],
        )
        .map_err(state)?;
    Ok(())
}

/// Reads the harness conversation a project's master session continues. Takes the engine
/// database, the project and the session. Returns it, or none when none was kept.
pub fn session_conversation(
    connection: &Connection,
    project: &ProjectId,
    session: &str,
) -> Result<Option<SessionConversation>, EngineError> {
    connection
        .query_row(
            "SELECT agent,harness,connection,conversation,record FROM team_master_conversations
             WHERE session=?1 AND project=?2",
            params![session, project.as_str()],
            |row| {
                Ok(SessionConversation {
                    agent: row.get(0)?,
                    harness: row.get(1)?,
                    connection: row.get(2)?,
                    resume: Resume {
                        id: row.get(3)?,
                        record: std::path::PathBuf::from(row.get::<_, String>(4)?),
                    },
                })
            },
        )
        .optional()
        .map_err(state)
}

/// The team projection of a [`SESSION_DELETED`] event: the deleted session's kept
/// conversation row goes, applied in the delete's own transaction and again by a replay of
/// the log; another kind changes nothing. Inputs: the store and the event. Output: any
/// SQLite failure, or a body that is not a session delete.
pub(crate) fn project_session_deleted(
    connection: &Connection,
    event: &Logged,
) -> Result<(), EngineError> {
    if event.kind != SESSION_DELETED {
        return Ok(());
    }
    let deleted: SessionDeleted = serde_json::from_slice(&event.body).map_err(state)?;
    connection
        .execute(
            "DELETE FROM team_master_conversations WHERE session=?1",
            [deleted.session.as_str()],
        )
        .map_err(state)?;
    Ok(())
}
