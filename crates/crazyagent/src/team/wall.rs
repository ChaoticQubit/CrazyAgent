use super::error;
use crate::engine::EngineError;
use crate::protocol::ProjectId;
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};

/// How many terminals "Open all terminals" puts in one terminal wall window; more agents
/// open more windows. Six by default. Sent and stored as the count itself: 4, 6 or 9.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(into = "u8", try_from = "u8")]
pub enum TerminalsPerWindow {
    Four,
    #[default]
    Six,
    Nine,
}

impl From<TerminalsPerWindow> for u8 {
    fn from(choice: TerminalsPerWindow) -> Self {
        match choice {
            TerminalsPerWindow::Four => 4,
            TerminalsPerWindow::Six => 6,
            TerminalsPerWindow::Nine => 9,
        }
    }
}

impl TryFrom<u8> for TerminalsPerWindow {
    type Error = String;

    /// Reads a count of terminals per window. Input: the count. Output: the choice for 4,
    /// 6 or 9, or `terminals_per_window_invalid` for any other count.
    fn try_from(count: u8) -> Result<Self, Self::Error> {
        match count {
            4 => Ok(Self::Four),
            6 => Ok(Self::Six),
            9 => Ok(Self::Nine),
            _ => Err("terminals_per_window_invalid".into()),
        }
    }
}

/// Reads how many terminals each of the project's terminal wall windows holds, Six until
/// the owner picks another. Takes a connection and the project. Returns the choice.
pub fn terminals_per_window(
    connection: &Connection,
    project: &ProjectId,
) -> Result<TerminalsPerWindow, EngineError> {
    let count: Option<u8> = connection
        .query_row(
            "SELECT per_window FROM team_wall WHERE project=?1",
            [project.as_str()],
            |row| row.get(0),
        )
        .optional()
        .map_err(error)?;
    match count {
        None => Ok(TerminalsPerWindow::default()),
        Some(count) => TerminalsPerWindow::try_from(count).map_err(|_| error("team_wall_invalid")),
    }
}

/// Saves how many terminals each of the project's terminal wall windows holds. Takes the
/// open transaction, the project and the choice. Returns unit, or a SQLite failure.
pub(super) fn store(
    tx: &Transaction<'_>,
    project: &ProjectId,
    choice: TerminalsPerWindow,
) -> Result<(), EngineError> {
    tx.execute(
        "INSERT INTO team_wall(project,per_window) VALUES (?1,?2)
         ON CONFLICT(project) DO UPDATE SET per_window=excluded.per_window",
        params![project.as_str(), u8::from(choice)],
    )
    .map_err(error)?;
    Ok(())
}
