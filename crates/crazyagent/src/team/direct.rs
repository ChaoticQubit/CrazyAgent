use super::{AgentRole, DIRECT_INPUT_BYTES_MAX, DirectInput, bound_text, error};
use crate::engine::EngineError;
use crate::protocol::ProjectId;
use rusqlite::{Connection, OptionalExtension, params};

pub const PENDING_PER_PROJECT_MAX: i64 = 200;
pub const NOTES_PER_TURN_MAX: i64 = 20;
pub const NOTE_TEXT_BYTES_MAX: usize = 300;
const ESCAPE: u8 = 0x1b;
const PASTE_START: &[u8] = b"200~";
const PASTE_END: &[u8] = b"201~";
const SEQUENCE_BYTES_MAX: usize = 32;

/// Records one line the owner typed into a team sub-agent's terminal on the sub-agent's
/// current task, for the master's next turn. Lines typed into a master need no note.
/// Inputs: the engine connection, the team agent id, the line and the time. Output: the
/// recorded input, or none for a master, an unknown agent, a blank line or a full queue.
pub fn record(
    connection: &Connection,
    agent: i64,
    text: &str,
    at_ms: i64,
) -> Result<Option<DirectInput>, EngineError> {
    let text = text.trim();
    if text.is_empty() || text.len() > DIRECT_INPUT_BYTES_MAX {
        return Ok(None);
    }
    let row: Option<(String, String, String, Option<i64>)> = connection
        .query_row(
            "SELECT project,role,harness,task FROM team_agents WHERE id=?1",
            [agent],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()
        .map_err(error)?;
    let Some((project, role, harness, task)) = row else {
        return Ok(None);
    };
    if AgentRole::parse(&role) != Some(AgentRole::Sub) {
        return Ok(None);
    }
    let Ok(project) = ProjectId::new(project) else {
        return Ok(None);
    };
    let pending: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM team_direct_inputs WHERE project=?1 AND delivered=0",
            [project.as_str()],
            |row| row.get(0),
        )
        .map_err(error)?;
    if pending >= PENDING_PER_PROJECT_MAX {
        return Ok(None);
    }
    connection
        .execute(
            "INSERT INTO team_direct_inputs(project,agent,task,harness,text,created_ms,delivered) VALUES (?1,?2,?3,?4,?5,?6,0)",
            params![project.as_str(), agent, task, harness, text, at_ms],
        )
        .map_err(error)?;
    Ok(Some(DirectInput {
        project,
        agent,
        text: text.to_owned(),
        at_ms,
    }))
}

/// Builds the note the master reads before its next message: one line per input the owner
/// typed into a sub-agent since the master's last turn, oldest first, at most
/// `NOTES_PER_TURN_MAX`. Inputs: the connection and the project. Output: the note, empty
/// when nothing was typed, and the id of the newest input it covers for `delivered`.
pub fn notes(
    connection: &Connection,
    project: &ProjectId,
) -> Result<(String, Option<i64>), EngineError> {
    let mut statement = connection
        .prepare(
            "SELECT id,agent,task,harness,text FROM team_direct_inputs WHERE project=?1 AND delivered=0 ORDER BY id LIMIT ?2",
        )
        .map_err(error)?;
    let rows: Vec<(i64, i64, Option<i64>, String, String)> = statement
        .query_map(params![project.as_str(), NOTES_PER_TURN_MAX], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        })
        .map_err(error)?
        .collect::<Result<_, _>>()
        .map_err(error)?;
    let Some(newest) = rows.last().map(|row| row.0) else {
        return Ok((String::new(), None));
    };
    let mut note = String::from("The owner typed straight into sub-agents since your last turn:\n");
    for (_, agent, task, harness, text) in &rows {
        let task = task.map_or_else(String::new, |task| format!(" on task {task}"));
        note.push_str(&format!(
            "- Owner told {harness}-{agent}{task}: {}\n",
            bound_text(text, NOTE_TEXT_BYTES_MAX)
        ));
    }
    note.push('\n');
    assert!(rows.len() as i64 <= NOTES_PER_TURN_MAX);
    Ok((note, Some(newest)))
}

/// Marks every input up to `newest` as heard by the master, once its turn was sent.
/// Inputs: the connection, the project and the newest input id `notes` covered.
pub fn delivered(
    connection: &Connection,
    project: &ProjectId,
    newest: i64,
) -> Result<(), EngineError> {
    connection
        .execute(
            "UPDATE team_direct_inputs SET delivered=1 WHERE project=?1 AND id<=?2 AND delivered=0",
            params![project.as_str(), newest],
        )
        .map_err(error)?;
    Ok(())
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum State {
    #[default]
    Text,
    Escape,
    Control,
    Single,
    Command,
    CommandEscape,
}

/// Rebuilds the lines the owner types into a terminal from the raw key bytes a viewer
/// sends: printable text and pastes are kept, Backspace removes a character, Ctrl-C and
/// Ctrl-U clear the line, other keys and escape sequences are dropped and Enter ends a line.
#[derive(Debug, Default)]
pub struct InputLines {
    line: Vec<u8>,
    state: State,
    sequence: Vec<u8>,
    pasting: bool,
}

impl InputLines {
    /// Takes the next key bytes, in any split. Output: the lines Enter completed, each
    /// trimmed, non-empty and at most `DIRECT_INPUT_BYTES_MAX` bytes.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<String> {
        let mut lines = Vec::new();
        for &byte in bytes {
            match self.state {
                State::Text => self.text(byte, &mut lines),
                State::Escape => {
                    self.state = match byte {
                        b'[' => State::Control,
                        b'O' => State::Single,
                        b']' | b'P' | b'_' | b'^' => State::Command,
                        _ => State::Text,
                    };
                    self.sequence.clear();
                }
                State::Control => {
                    if (0x40..=0x7e).contains(&byte) {
                        self.sequence.push(byte);
                        if self.sequence == PASTE_START {
                            self.pasting = true;
                        } else if self.sequence == PASTE_END {
                            self.pasting = false;
                        }
                        self.state = State::Text;
                    } else if self.sequence.len() < SEQUENCE_BYTES_MAX {
                        self.sequence.push(byte);
                    }
                }
                State::Single => self.state = State::Text,
                State::Command => match byte {
                    0x07 => self.state = State::Text,
                    ESCAPE => self.state = State::CommandEscape,
                    _ => {}
                },
                State::CommandEscape => {
                    self.state = if byte == b'\\' {
                        State::Text
                    } else {
                        State::Command
                    };
                }
            }
        }
        assert!(self.line.len() <= DIRECT_INPUT_BYTES_MAX);
        lines
    }

    fn text(&mut self, byte: u8, lines: &mut Vec<String>) {
        match byte {
            ESCAPE => self.state = State::Escape,
            b'\r' | b'\n' if self.pasting => self.push(b'\n'),
            b'\r' | b'\n' => {
                let line: String = String::from_utf8_lossy(&self.line)
                    .chars()
                    .filter(|character| {
                        *character == '\n' || *character == '\t' || !character.is_control()
                    })
                    .collect();
                let line = line.trim().to_owned();
                self.line.clear();
                if !line.is_empty() {
                    lines.push(bound_text(&line, DIRECT_INPUT_BYTES_MAX));
                }
            }
            0x7f | 0x08 => {
                while let Some(last) = self.line.pop() {
                    if last & 0xc0 != 0x80 {
                        break;
                    }
                }
            }
            0x03 | 0x15 => self.line.clear(),
            b'\t' => self.push(byte),
            byte if byte < 0x20 => {}
            byte => self.push(byte),
        }
    }

    fn push(&mut self, byte: u8) {
        if self.line.len() < DIRECT_INPUT_BYTES_MAX {
            self.line.push(byte);
        }
    }
}

#[cfg(test)]
mod tests;
