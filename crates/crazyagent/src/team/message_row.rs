use super::*;

pub(super) fn message_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<TeamMessage> {
    let author: String = row.get(1)?;
    let state: String = row.get(3)?;
    Ok(TeamMessage {
        id: row.get(0)?,
        author: MessageAuthor::parse(&author).ok_or_else(|| invalid_row("author"))?,
        text: row.get(2)?,
        state: MessageState::parse(&state).ok_or_else(|| invalid_row("state"))?,
        created_ms: row.get(4)?,
    })
}
