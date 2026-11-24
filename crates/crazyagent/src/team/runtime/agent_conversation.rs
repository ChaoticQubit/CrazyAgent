use super::*;

/// Keeps the harness conversation a sub-agent's pane records, replacing the one kept
/// before, so waking the agent continues it (DEC-102). A record path that is not plain text
/// or too long, or an id out of bounds, is not kept. Takes the database, the agent, its
/// project, launch harness and account, and the conversation. Returns any SQLite failure.
pub(super) fn keep_agent_conversation(
    connection: &Connection,
    agent: i64,
    project: &ProjectId,
    harness: &str,
    account: &CredentialRef,
    resume: &Resume,
) -> Result<(), EngineError> {
    let Some(record) = resume
        .record
        .to_str()
        .filter(|record| !record.is_empty() && record.len() <= 4096)
    else {
        return Ok(());
    };
    if resume.id.is_empty() || resume.id.len() > 128 {
        return Ok(());
    }
    connection
        .execute(
            "INSERT OR REPLACE INTO team_agent_conversations(agent,project,harness,connection,conversation,record)
             VALUES (?1,?2,?3,?4,?5,?6)",
            params![
                agent,
                project.as_str(),
                harness,
                account.as_str(),
                resume.id,
                record
            ],
        )
        .map_err(error)?;
    Ok(())
}
