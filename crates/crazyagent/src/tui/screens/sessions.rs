use super::new_project::harness_name;
use super::new_project_view::{Look, Row, columns, list_row, spread, wrap_words};
use super::project_master::TYPED_IN_TERMINAL;
use super::project_team::{Help, window};
use super::project_time::{clock, duration};
use crate::master_session::{MasterOwnerAction, MasterSessionPhase};
use crate::master_sessions::{MasterMode, MasterSessionSummary, MessageSpeaker, SessionMessage};
use crate::tui::master_session::State as MasterState;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

const DAY_MS: i64 = 86_400_000;
const STATE_WIDTH: usize = 6;
const STARTED_WIDTH: usize = 12;
const WAITING_WIDTH: usize = 9;
const AGENTS_NARROW: usize = 14;
const AGENTS_WIDE: usize = 24;
const WIDE: usize = 96;
const TITLE_ROWS: usize = 2;
const SCROLL_PAGE: usize = 10;

/// One master session as the Sessions list shows it: its id, whether it is the live one,
/// when it started, its line from the engine, the agents it used, how many of their
/// results wait for it, how many messages it holds, the master that ran it last and its
/// first owner line when that is not `line`. Filled from the engine's session list; the
/// text is untrusted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::tui) struct SessionRow {
    pub id: String,
    pub live: bool,
    pub started_ms: Option<i64>,
    pub line: String,
    pub agents: Vec<String>,
    pub waiting: u32,
    pub count: u64,
    pub ran: Option<String>,
    pub first: Option<String>,
}

/// One stored message as the opened session shows it: who said it, absent for a divider
/// or a plain notice, and the words.
#[derive(Clone, Debug, PartialEq, Eq)]
struct StoredLine {
    who: Option<String>,
    text: String,
}

/// The stored messages of the session the owner opened from the list: its id, how many
/// lines the view is scrolled up from the newest, the messages as last taken, whether a
/// read arrived, whether they start at the session's first message, and a mark of the
/// rows they were built from.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Reading {
    session: String,
    scroll: usize,
    lines: Vec<StoredLine>,
    loaded: bool,
    whole: bool,
    mark: (usize, u64, usize),
}

/// What the owner asked for: a fresh session, making a parked one live, or deleting one
/// that is not live. `cancel_running` is true only after the owner confirmed stopping the
/// turn that was running.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::tui) enum SessionsAction {
    New {
        cancel_running: bool,
    },
    Resume {
        session: String,
        cancel_running: bool,
    },
    Delete {
        session: String,
    },
}

/// What the sessions part asks its host to do after a key: nothing (the host's own keys
/// run), nothing more (the key was used), or send an owner action.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::tui) enum SessionsReply {
    Ignored,
    Handled,
    Act(SessionsAction),
}

/// The master view's sessions part: the engine's list, whether the list is open, the
/// cursor, the action waiting for the owner's confirmation and the session whose stored
/// messages the owner opened. Takes engine reads through `set` and `set_messages` and keys
/// through `key`; never invents rows.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(in crate::tui) struct Sessions {
    rows: Option<Vec<SessionRow>>,
    open: bool,
    cursor: usize,
    confirm: Option<SessionsAction>,
    reading: Option<Reading>,
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
        if self
            .reading
            .as_ref()
            .is_some_and(|reading| !rows.iter().any(|row| row.id == reading.session))
        {
            self.reading = None;
        }
        self.rows = Some(rows);
    }

    /// The session whose stored messages the owner opened, so the host reads them.
    /// Output: its id, or none while no session is open for reading.
    pub fn reading(&self) -> Option<&str> {
        self.reading
            .as_ref()
            .map(|reading| reading.session.as_str())
    }

    /// Takes the stored messages the client holds for the opened session: the rows,
    /// oldest first, and whether they start at the session's first message, or none
    /// before its first read. The lines are built again only when the rows changed.
    /// Input: the held messages. Output: none.
    pub fn set_messages(&mut self, held: Option<(&[SessionMessage], bool)>) {
        let Some(reading) = &mut self.reading else {
            return;
        };
        let Some((rows, whole)) = held else {
            reading.loaded = false;
            reading.lines.clear();
            return;
        };
        let mark = (
            rows.len(),
            rows.last().map_or(0, |row| row.seq),
            rows.iter().filter(|row| row.text.is_none()).count(),
        );
        if reading.loaded && reading.mark == mark && reading.whole == whole {
            return;
        }
        reading.lines = rows.iter().filter_map(stored_line).collect();
        reading.loaded = true;
        reading.whole = whole;
        reading.mark = mark;
        assert!(reading.lines.len() <= rows.len());
    }

    /// Asks the owner again when the engine refused a switch because a turn started
    /// running meanwhile. Input: the refused action. Output: true when it was a switch.
    pub fn turn_was_running(&mut self, action: &MasterOwnerAction) -> bool {
        self.confirm = match action {
            MasterOwnerAction::NewSession { .. } => Some(SessionsAction::New {
                cancel_running: false,
            }),
            MasterOwnerAction::Resume { session_id, .. } => Some(SessionsAction::Resume {
                session: session_id.as_str().to_owned(),
                cancel_running: false,
            }),
            _ => return false,
        };
        true
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
    /// resumes the chosen session, and either asks first while a turn runs; `d` in the
    /// list asks before deleting the chosen session when it is not the live one; `m` in
    /// the list opens the chosen session's stored messages, where the arrows and the page
    /// keys scroll and Esc goes back to the list.
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
                        SessionsAction::Delete { session } => SessionsAction::Delete { session },
                    })
                }
                KeyCode::Char('n') | KeyCode::Esc => {
                    self.confirm = None;
                    SessionsReply::Handled
                }
                _ => SessionsReply::Handled,
            };
        }
        if self.reading.is_some() && key.code == KeyCode::Esc {
            self.reading = None;
            return SessionsReply::Handled;
        }
        if let Some(reading) = &mut self.reading {
            reading.scroll = match key.code {
                KeyCode::Up => reading.scroll.saturating_add(1),
                KeyCode::Down => reading.scroll.saturating_sub(1),
                KeyCode::PageUp => reading.scroll.saturating_add(SCROLL_PAGE),
                KeyCode::PageDown => reading.scroll.saturating_sub(SCROLL_PAGE),
                _ => reading.scroll,
            };
            return SessionsReply::Handled;
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
                SessionsReply::Handled
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
            KeyCode::Char('m') if self.open => {
                if let Some(row) = self.picked() {
                    self.reading = Some(Reading {
                        session: row.id.clone(),
                        ..Reading::default()
                    });
                }
                SessionsReply::Handled
            }
            KeyCode::Char('d') if self.open => {
                if let Some(row) = self.picked().filter(|row| !row.live) {
                    self.confirm = Some(SessionsAction::Delete {
                        session: row.id.clone(),
                    });
                }
                SessionsReply::Handled
            }
            _ if self.open => SessionsReply::Handled,
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
        if self.confirm.is_none()
            && let Some(reading) = &self.reading
        {
            return Some(self.messages(reading, width, room, now_ms, offset, look));
        }
        let mut body = Vec::new();
        let when = |session: &str| {
            self.rows
                .as_ref()
                .and_then(|rows| rows.iter().find(|row| row.id == session))
                .map_or_else(String::new, |row| {
                    format!(" from {}", started(row.started_ms, now_ms, offset))
                })
        };
        let question = self.confirm.as_ref().map(|action| match action {
            SessionsAction::New { .. } => (
                "A turn is running. Stop it and start a new session?".to_owned(),
                vec![("y", "Stop the turn and switch"), ("n", "Keep it running")],
            ),
            SessionsAction::Resume { session, .. } => (
                format!(
                    "A turn is running. Stop it and resume the session{}?",
                    when(session.as_str())
                ),
                vec![("y", "Stop the turn and switch"), ("n", "Keep it running")],
            ),
            SessionsAction::Delete { session } => (
                format!(
                    "Delete the session{}? Its messages and its transcript are removed for good.",
                    when(session.as_str())
                ),
                vec![("y", "Delete it"), ("n", "Keep it")],
            ),
        });
        let question = question.map(|(text, keys)| (wrap_words(&text, width), keys));
        let list_room =
            room.saturating_sub(question.as_ref().map_or(0, |(lines, _)| lines.len() + 1));
        if self.open {
            body.extend(self.list(width, list_room, now_ms, offset, look));
        }
        let help = if let Some((question, keys)) = question {
            body.truncate(list_room);
            if !body.is_empty() {
                body.push(Row::default());
            }
            body.extend(
                question
                    .into_iter()
                    .map(|line| Row::of(line, look.warning).pad(width)),
            );
            keys
        } else {
            vec![
                ("\u{2191}\u{2193}", "Move"),
                ("Enter", "Resume"),
                ("m", "Messages"),
                ("d", "Delete"),
                ("n", "New session"),
                ("Esc", "Back"),
            ]
        };
        body.truncate(room);
        assert!(body.len() <= room);
        Some((body, help))
    }

    /// The body and help of an opened session: a title naming the session, then its
    /// stored messages, each under who said it, with the dividers of master switches,
    /// showing the newest unless scrolled up. Inputs: the opened session, the width, the
    /// body rows, now, the UTC offset and the look. Output: at most `room` rows and the
    /// help keys.
    fn messages(
        &self,
        reading: &Reading,
        width: usize,
        room: usize,
        now_ms: i64,
        offset: i64,
        look: &Look,
    ) -> (Vec<Row>, Help) {
        let row = self
            .rows
            .as_ref()
            .and_then(|rows| rows.iter().find(|row| row.id == reading.session));
        let named = row.map_or_else(String::new, |row| {
            format!(
                "  session from {}, {}",
                started(row.started_ms, now_ms, offset),
                detail_lines(row)[0]
            )
        });
        let mut body = vec![
            Row::of("Messages", look.heading).push(named, look.muted),
            Row::default(),
        ];
        let inner = width.saturating_sub(2).max(1);
        let mut lines = Vec::new();
        if !reading.loaded {
            lines.push(Row::of("Reading the messages...", look.muted));
        } else if !reading.whole {
            lines.push(Row::of("Reading earlier messages...", look.muted));
        } else if reading.lines.is_empty() {
            lines.push(Row::of("No stored messages in this session.", look.muted));
        }
        for line in &reading.lines {
            match &line.who {
                Some(who) => {
                    lines.push(Row::of(who.clone(), look.heading));
                    lines.extend(
                        wrap_words(&line.text, inner)
                            .into_iter()
                            .map(|text| Row::of(format!("  {text}"), look.fg)),
                    );
                }
                None => lines.extend(
                    wrap_words(&line.text, width)
                        .into_iter()
                        .map(|text| Row::of(text, look.muted)),
                ),
            }
        }
        let shown = room.saturating_sub(TITLE_ROWS).max(1);
        let up = reading.scroll.min(lines.len().saturating_sub(shown));
        let last = lines.len() - up;
        let first = last.saturating_sub(shown);
        body.extend(lines.into_iter().skip(first).take(last - first));
        body.truncate(room);
        assert!(body.len() <= room);
        (body, vec![("\u{2191}\u{2193}", "Scroll"), ("Esc", "Back")])
    }

    fn list(&self, width: usize, room: usize, now_ms: i64, offset: i64, look: &Look) -> Vec<Row> {
        let Some(rows) = &self.rows else {
            return vec![Row::of("Reading sessions...", look.muted)];
        };
        let mut lines = vec![
            Row::of("Sessions", look.heading).push(format!("  {}", rows.len()), look.muted),
            Row::default(),
        ];
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
        let detail = match (self.picked(), &self.confirm) {
            (Some(row), None) => detail_lines(row),
            _ => Vec::new(),
        };
        let spare = room.saturating_sub(TITLE_ROWS + 1);
        let detail_room = if !detail.is_empty() && spare > detail.len() {
            detail.len() + 1
        } else {
            0
        };
        let shown = room.saturating_sub(TITLE_ROWS + detail_room).max(1);
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
        if detail_room > 0 {
            lines.push(Row::default());
            for text in detail {
                let cut = wrap_words(&text, inner.max(1)).into_iter().next();
                lines.push(Row::of(
                    format!("  {}", cut.unwrap_or_default()),
                    look.muted,
                ));
            }
        }
        lines
    }
}

/// Names the master that ran a message or a session: the harness name for a live terminal
/// master, "One-turn master (RCLI)", "(Codex)" or "(Claude)" for the master the engine
/// runs itself. Inputs: the master mode and the stored harness, if any. Output: the name.
pub(in crate::tui) fn ran_by(mode: MasterMode, harness: Option<&str>) -> String {
    match (mode, harness) {
        (MasterMode::Live, Some(id)) => harness_name(id, None),
        (MasterMode::Live, None) => "Live master".into(),
        (MasterMode::OneTurn, Some("rcli")) => "One-turn master (RCLI)".into(),
        (MasterMode::OneTurn, Some("codex")) => "One-turn master (Codex)".into(),
        (MasterMode::OneTurn, Some("claude")) => "One-turn master (Claude)".into(),
        (MasterMode::OneTurn, _) => "One-turn master".into(),
    }
}

/// The lines under the list for the chosen session: how many messages it holds and the
/// master that ran it last, then its first and its newest line when the two differ.
/// Input: the row. Output: one to three lines, the count line first.
fn detail_lines(row: &SessionRow) -> Vec<String> {
    let mut head = match row.count {
        0 => "No messages yet".to_owned(),
        1 => "1 message".to_owned(),
        count => format!("{count} messages"),
    };
    if let Some(ran) = &row.ran {
        head = format!("{head}, last run by {ran}");
    }
    let mut lines = vec![head];
    if let Some(first) = &row.first {
        lines.push(format!("First: {}", first_line(first)));
        lines.push(format!("Last: {}", first_line(&row.line)));
    }
    assert!(!lines.is_empty() && lines.len() <= 3);
    lines
}

/// One stored message as an opened session shows it: the owner as "You", the engine as
/// "CrazyAgent" and the master by `ran_by`; a master switch and a terminal turn whose
/// typed words were not read have no speaker. Input: the row. Output: the line, or none
/// for any other row with no text.
fn stored_line(row: &SessionMessage) -> Option<StoredLine> {
    let (who, text) = match (row.speaker, &row.text) {
        (MessageSpeaker::Owner, Some(text)) => (Some("You".to_owned()), text.clone()),
        (MessageSpeaker::Owner, None) => (None, TYPED_IN_TERMINAL.to_owned()),
        (MessageSpeaker::Master, Some(text)) => {
            (Some(ran_by(row.mode, row.harness.as_deref())), text.clone())
        }
        (MessageSpeaker::Engine, Some(text)) => (Some("CrazyAgent".to_owned()), text.clone()),
        (MessageSpeaker::Switch, Some(text)) => (None, text.clone()),
        (_, None) => return None,
    };
    Some(StoredLine { who, text })
}

/// The list rows from the engine's session summaries, in the engine's order.
/// Input: the summaries. Output: one row per session; the line is the newest line, else
/// the first, with the message count, the master that ran it last when the engine names
/// one, and the first line when it is not the newest.
pub(in crate::tui) fn rows_of(listed: &[MasterSessionSummary]) -> Vec<SessionRow> {
    listed
        .iter()
        .map(|session| SessionRow {
            id: session.session_id.as_str().to_owned(),
            live: session.phase == MasterSessionPhase::Configured,
            started_ms: session.started_ms.and_then(|at| i64::try_from(at).ok()),
            line: session
                .last_line
                .clone()
                .or_else(|| session.first_line.clone())
                .unwrap_or_default(),
            agents: session.agents.clone(),
            waiting: u32::try_from(session.results_waiting).unwrap_or(u32::MAX),
            count: session.turns,
            ran: session
                .mode
                .map(|mode| ran_by(mode, session.harness.as_deref())),
            first: session
                .first_line
                .clone()
                .filter(|first| session.last_line.as_ref().is_some_and(|last| last != first)),
        })
        .collect()
}

/// Sends the owner's choice through the master client. Inputs: the client and the action.
/// Output: the client's plain refusal when it cannot send it.
pub(in crate::tui) fn act(master: &mut MasterState, action: &SessionsAction) -> Result<(), String> {
    let listed = |master: &MasterState, session: &str| {
        master
            .sessions()
            .and_then(|listed| {
                listed
                    .iter()
                    .find(|item| item.session_id.as_str() == session)
            })
            .map(|item| item.session_id.clone())
            .ok_or("That session is not in the Sessions list")
    };
    match action {
        SessionsAction::New { .. } if master.live_session_unused() => {
            Err("This session has no messages yet, so it already is a new session.".into())
        }
        SessionsAction::New { cancel_running } => master.new_session(*cancel_running),
        SessionsAction::Resume {
            session,
            cancel_running,
        } => {
            let id = listed(&*master, session.as_str())?;
            master.resume(&id, *cancel_running)
        }
        SessionsAction::Delete { session } => {
            let id = listed(&*master, session.as_str())?;
            master.delete_session(&id)
        }
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

#[cfg(all(test, unix))]
mod tests;
