use super::new_project_view::{Look, Row, columns, list_row, spread};
use super::project_team::{Help, window};
use super::project_time::{clock, duration};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

const DAY_MS: i64 = 86_400_000;
const STATE_WIDTH: usize = 6;
const STARTED_WIDTH: usize = 12;
const WAITING_WIDTH: usize = 9;
const AGENTS_NARROW: usize = 14;
const AGENTS_WIDE: usize = 24;
const WIDE: usize = 96;
const TITLE_ROWS: usize = 2;

/// One master session as the Sessions list shows it: its id, whether it is the live one,
/// when it started, its line from the engine, the agents it used and how many of their
/// results wait for it. Filled from the engine's session list; the text is untrusted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::tui) struct SessionRow {
    pub id: String,
    pub live: bool,
    pub started_ms: Option<i64>,
    pub line: String,
    pub agents: Vec<String>,
    pub waiting: u32,
}

/// What the owner asked for: a fresh session, or making a parked one live. `cancel_running`
/// is true only after the owner confirmed stopping the turn that was running.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::tui) enum SessionsAction {
    New {
        cancel_running: bool,
    },
    Resume {
        session: String,
        cancel_running: bool,
    },
}

/// What the sessions part asks its host to do after a key: nothing (the host's own keys
/// run), nothing more (the key was used), read the list again, or send an owner action.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::tui) enum SessionsReply {
    Ignored,
    Handled,
    Read,
    Act(SessionsAction),
}

/// The master view's sessions part: the engine's list, whether the list is open, the
/// cursor, the action waiting for the owner's confirmation and the last read problem.
/// Takes engine reads through `set` and `failed` and keys through `key`; never invents rows.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(in crate::tui) struct Sessions {
    rows: Option<Vec<SessionRow>>,
    open: bool,
    cursor: usize,
    confirm: Option<SessionsAction>,
    problem: Option<String>,
}

impl Sessions {
    /// Takes a fresh engine list, newest activity first, keeping the cursor on the same
    /// session when it is still listed. Input: the rows. Output: none.
    pub fn set(&mut self, rows: Vec<SessionRow>) {
        assert!(rows.iter().filter(|row| row.live).count() <= 1);
        let kept = self.picked().map(|row| row.id.clone());
        self.cursor = kept
            .and_then(|id| rows.iter().position(|row| row.id == id))
            .unwrap_or(0);
        self.rows = Some(rows);
        self.problem = None;
    }

    /// Keeps the plain reason the list could not be read. Input: the text. Output: none.
    pub fn failed(&mut self, problem: String) {
        self.problem = Some(problem);
    }

    /// Says whether the sessions part draws in place of the conversation: the list is open
    /// or a confirmation waits. Output: true when `view` gives the body.
    pub fn showing(&self) -> bool {
        self.open || self.confirm.is_some()
    }

    fn picked(&self) -> Option<&SessionRow> {
        self.rows.as_ref()?.get(self.cursor)
    }

    fn ask(&mut self, action: SessionsAction, running: bool) -> SessionsReply {
        if running {
            self.confirm = Some(action);
            return SessionsReply::Handled;
        }
        self.open = false;
        SessionsReply::Act(action)
    }

    /// Handles a key on the Conversation tab while the owner is not typing.
    /// Inputs: the key, whether the master can take actions now and whether a turn runs.
    /// Output: the reply; `n` starts a new session, `l` opens the list, Enter in the list
    /// resumes the chosen session, and either asks first while a turn runs.
    pub fn key(&mut self, key: KeyEvent, ready: bool, running: bool) -> SessionsReply {
        if key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        {
            return SessionsReply::Ignored;
        }
        if let Some(action) = self.confirm.clone() {
            return match key.code {
                KeyCode::Char('y') => {
                    self.confirm = None;
                    self.open = false;
                    SessionsReply::Act(match action {
                        SessionsAction::New { .. } => SessionsAction::New {
                            cancel_running: true,
                        },
                        SessionsAction::Resume { session, .. } => SessionsAction::Resume {
                            session,
                            cancel_running: true,
                        },
                    })
                }
                KeyCode::Char('n') | KeyCode::Esc => {
                    self.confirm = None;
                    SessionsReply::Handled
                }
                _ => SessionsReply::Handled,
            };
        }
        if !ready {
            return SessionsReply::Ignored;
        }
        let count = self.rows.as_ref().map_or(0, Vec::len);
        match key.code {
            KeyCode::Char('n') => self.ask(
                SessionsAction::New {
                    cancel_running: false,
                },
                running,
            ),
            KeyCode::Char('l') if !self.open => {
                self.open = true;
                SessionsReply::Read
            }
            KeyCode::Esc if self.open => {
                self.open = false;
                SessionsReply::Handled
            }
            KeyCode::Up if self.open => {
                self.cursor = self.cursor.saturating_sub(1);
                SessionsReply::Handled
            }
            KeyCode::Down if self.open => {
                self.cursor = (self.cursor + 1).min(count.saturating_sub(1));
                SessionsReply::Handled
            }
            KeyCode::Char('r') if self.open && self.problem.is_some() => SessionsReply::Read,
            KeyCode::Enter if self.open => match self.picked() {
                Some(row) if row.live => {
                    self.open = false;
                    SessionsReply::Handled
                }
                Some(row) => {
                    let session = row.id.clone();
                    self.ask(
                        SessionsAction::Resume {
                            session,
                            cancel_running: false,
                        },
                        running,
                    )
                }
                None => SessionsReply::Handled,
            },
            _ => SessionsReply::Ignored,
        }
    }

    /// The header line under the master's own: when the live session started and how many
    /// results wait in other sessions. Inputs: the width, now, the UTC offset and the look.
    /// Output: the row, or none before the engine has listed a live session.
    pub fn header(&self, width: usize, now_ms: i64, offset: i64, look: &Look) -> Option<Row> {
        let rows = self.rows.as_ref()?;
        let live = rows.iter().find(|row| row.live)?;
        let others = rows.len() - 1;
        let waiting: u32 = rows
            .iter()
            .filter(|row| !row.live)
            .map(|row| row.waiting)
            .sum();
        let mut left = Row::of("Session", look.heading).push(
            format!("  started {}", started(live.started_ms, now_ms, offset)),
            look.fg,
        );
        if others > 0 {
            left = left.push(format!("   {others} earlier"), look.muted);
        }
        let right = if waiting > 0 {
            Row::of(format!("{waiting} results waiting elsewhere"), look.warning)
        } else {
            Row::default()
        };
        Some(spread(width, left, right))
    }

    /// The help keys the Conversation tab adds while the list is closed.
    /// Output: New session and Sessions.
    pub fn conversation_help() -> Help {
        vec![("n", "New session"), ("l", "Sessions")]
    }

    /// The body and help drawn in place of the conversation while `showing` is true.
    /// Inputs: the width, the body rows, now, the UTC offset and the look.
    /// Output: at most `room` rows and the help keys, or none while nothing shows.
    pub fn view(
        &self,
        width: usize,
        room: usize,
        now_ms: i64,
        offset: i64,
        look: &Look,
    ) -> Option<(Vec<Row>, Help)> {
        if !self.showing() {
            return None;
        }
        let mut body = Vec::new();
        let question = self.confirm.as_ref().map(|action| match action {
            SessionsAction::New { .. } => {
                "A turn is running. Stop it and start a new session?".to_owned()
            }
            SessionsAction::Resume { session, .. } => {
                let when = self
                    .rows
                    .as_ref()
                    .and_then(|rows| rows.iter().find(|row| &row.id == session))
                    .map_or_else(String::new, |row| {
                        format!(" from {}", started(row.started_ms, now_ms, offset))
                    });
                format!("A turn is running. Stop it and resume the session{when}?")
            }
        });
        let list_room = room.saturating_sub(usize::from(question.is_some()) * 2);
        if self.open {
            body.extend(self.list(width, list_room, now_ms, offset, look));
        }
        let help = if let Some(question) = question {
            body.truncate(list_room);
            if !body.is_empty() {
                body.push(Row::default());
            }
            body.push(Row::of(question, look.warning).pad(width));
            vec![("y", "Stop the turn and switch"), ("n", "Keep it running")]
        } else {
            vec![
                ("\u{2191}\u{2193}", "Move"),
                ("Enter", "Resume"),
                ("n", "New session"),
                ("Esc", "Back"),
            ]
        };
        body.truncate(room);
        assert!(body.len() <= room);
        Some((body, help))
    }

    fn list(&self, width: usize, room: usize, now_ms: i64, offset: i64, look: &Look) -> Vec<Row> {
        let Some(rows) = &self.rows else {
            return vec![match &self.problem {
                Some(problem) => Row::of(problem.clone(), look.error),
                None => Row::of("Reading sessions...", look.muted),
            }];
        };
        let mut lines = vec![
            Row::of("Sessions", look.heading).push(format!("  {}", rows.len()), look.muted),
            Row::default(),
        ];
        if let Some(problem) = &self.problem {
            lines[1] = Row::of(format!("{problem} Press r to try again."), look.error);
        }
        if rows.is_empty() {
            lines.push(Row::of("No sessions yet.", look.muted));
            return lines;
        }
        let inner = width.saturating_sub(2);
        let agents = if width >= WIDE {
            AGENTS_WIDE
        } else {
            AGENTS_NARROW
        };
        let fixed = STATE_WIDTH + STARTED_WIDTH + agents + WAITING_WIDTH + 4 * 2;
        let widths = [
            STATE_WIDTH,
            STARTED_WIDTH,
            inner.saturating_sub(fixed).max(1),
            agents,
            WAITING_WIDTH,
        ];
        let shown = room.saturating_sub(TITLE_ROWS).max(1);
        let first = window(self.cursor, rows.len(), shown);
        for (index, row) in rows.iter().enumerate().skip(first).take(shown) {
            let cells = vec![
                if row.live {
                    Row::of("Live", look.success)
                } else {
                    Row::of("Parked", look.muted)
                },
                Row::of(started(row.started_ms, now_ms, offset), look.fg),
                Row::of(first_line(&row.line), look.fg),
                Row::of(agent_names(&row.agents), look.muted),
                if row.waiting > 0 {
                    Row::of(format!("{} waiting", row.waiting), look.warning)
                } else {
                    Row::default()
                },
            ];
            lines.push(list_row(
                columns(&widths, cells),
                width,
                index == self.cursor,
                true,
                look,
            ));
        }
        lines
    }
}

fn started(at_ms: Option<i64>, now_ms: i64, offset: i64) -> String {
    match at_ms {
        None => "earlier".into(),
        Some(at)
            if (now_ms + offset * 1000).div_euclid(DAY_MS)
                == (at + offset * 1000).div_euclid(DAY_MS) =>
        {
            clock(at, offset)
        }
        Some(at) => format!("{} ago", duration(now_ms - at)),
    }
}

fn first_line(text: &str) -> String {
    let line = text.lines().map(str::trim).find(|line| !line.is_empty());
    line.unwrap_or("No messages yet").to_owned()
}

fn agent_names(agents: &[String]) -> String {
    if agents.is_empty() {
        return "No agents".into();
    }
    agents.join(", ")
}

#[cfg(test)]
mod tests;
