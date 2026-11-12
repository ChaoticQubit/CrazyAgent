use super::project::REFRESH_MS;
use super::project_markdown::sanitize;
use super::project_time::clock;
use crate::project_setup::{SetupQuery, SetupView};
use crate::protocol::ProjectId;
use crate::team::agent_messages::{
    AgentMessage, AgentMessages, Cursor, EntryKind, MessageSettings, ResultsShown, Source, Speaker,
};
use crate::team::events::EventState;
use crate::tui::keys::Key;
use crate::tui::text::{cut, width, wrap};
use crate::tui::theme::Role;
#[cfg(test)]
use std::cell::Cell;
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};

/// The most entries one open list holds; entries dropped past it stay in the engine and are
/// read again when the owner pages back to them.
pub(in crate::tui) const KEPT_MAX: usize = 1000;

/// What `m` on the master says: its Messages list is a sub-agent's input, and the master's own
/// conversation has its own place.
pub(in crate::tui) const MASTER_ELSEWHERE: &str =
    "The master's own conversation is in the Conversation tab.";

const PAGE_STEP: usize = 10;
const TEXT_MIN: usize = 12;
/// What a narrow box draws before each line of an entry's text and result, which start on their
/// own lines there. A row starts with its time, so no line of text can pass as a row.
const TEXT_GUTTER: &str = "  | ";

/// The lines each entry drew as at one width and one results setting, keyed by the entry's
/// cursor and whether it was the picked one, so a redraw wraps only entries it has not drawn.
#[derive(Default)]
struct Drawn {
    limit: usize,
    full: bool,
    lines: BTreeMap<(Cursor, bool), Vec<Shown>>,
}

/// One drawn line of a Messages list: plain text fitted to the width and the color role it
/// draws in, none for the plain style. Each host turns it into its own line type.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::tui) struct Shown {
    pub text: String,
    pub role: Option<Role>,
}

/// What a key did to an open Messages list: handled inside it, or Esc closed it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::tui) enum ListKey {
    Handled,
    Closed,
}

/// One sub-agent's Messages timeline as the owner reads it in the Team view and the team
/// tree: tasks, what the master and the owner said to it and its events, oldest at the top.
/// Pages come from the engine through `next_query` and `apply`; the newest page is read
/// again once per refresh period and PgUp at the top reads the page before the oldest entry.
/// Entries merge by their cursor and at most `KEPT_MAX` stay, dropped from the end away from
/// the pick. Once the newest are dropped the list stops following new entries until End.
/// A draw wraps only the entries in view, each once per width. Never invents an entry.
pub(in crate::tui) struct Messages {
    pub project: ProjectId,
    pub agent: i64,
    label: String,
    offset: i64,
    entries: Vec<AgentMessage>,
    older: bool,
    newer: bool,
    settings: Option<MessageSettings>,
    loaded: bool,
    asked: Option<SetupQuery>,
    want_older: bool,
    read_ms: Option<i64>,
    now_ms: i64,
    problem: Option<String>,
    cursor: usize,
    opened: BTreeSet<(Source, i64)>,
    drawn: RefCell<Drawn>,
    #[cfg(test)]
    wraps: Cell<usize>,
}

impl Messages {
    /// Opens the list of one sub-agent with nothing read yet.
    /// Takes the project, the agent id, its name as the engine gave it and the UTC offset in
    /// seconds for the times. Returns the list; the first read comes from `next_query`.
    pub fn new(project: ProjectId, agent: i64, label: &str, offset: i64) -> Self {
        Self {
            project,
            agent,
            label: one_line(label),
            offset,
            entries: Vec::new(),
            older: false,
            newer: false,
            settings: None,
            loaded: false,
            asked: None,
            want_older: false,
            read_ms: None,
            now_ms: 0,
            problem: None,
            cursor: 0,
            opened: BTreeSet::new(),
            drawn: RefCell::default(),
            #[cfg(test)]
            wraps: Cell::new(0),
        }
    }

    /// The box or heading title. Returns "Messages: " and the agent's name.
    pub fn title(&self) -> String {
        format!("Messages: {}", self.label)
    }

    /// Gives the next page read, one at a time: the older page the owner asked for first,
    /// else the newest page once per refresh period.
    /// Takes the current time in milliseconds.
    /// Returns `SetupQuery::TeamMessages`, or none while a read is out or nothing is due.
    pub fn next_query(&mut self, now_ms: i64) -> Option<SetupQuery> {
        self.now_ms = now_ms;
        if self.asked.is_some() {
            return None;
        }
        let before = match (self.want_older, self.entries.first()) {
            (true, Some(oldest)) => Some(oldest.cursor()),
            _ if self.newer => return None,
            _ if self.read_ms.is_none_or(|last| now_ms - last >= REFRESH_MS) => None,
            _ => return None,
        };
        self.want_older = false;
        let query = SetupQuery::TeamMessages {
            project: self.project.clone(),
            agent: self.agent,
            before,
        };
        self.asked = Some(query.clone());
        Some(query)
    }

    /// Forgets the read in flight after the engine connection dropped, so the next
    /// `next_query` asks again. The entries already read stay on screen.
    pub fn forget_reads(&mut self) {
        self.asked = None;
    }

    /// Applies the engine's answer to the read this list sent.
    /// Takes the query and the engine's view or plain problem code.
    /// Returns true when the answer was this list's, false for anything else.
    pub fn apply(&mut self, query: &SetupQuery, result: &Result<SetupView, String>) -> bool {
        let SetupQuery::TeamMessages { before, .. } = query else {
            return false;
        };
        if self.asked.as_ref() != Some(query) {
            return false;
        }
        self.asked = None;
        if before.is_none() {
            self.read_ms = Some(self.now_ms);
        }
        match result {
            Ok(SetupView::TeamMessages(page)) => {
                self.take(page, *before);
                self.problem = None;
                self.loaded = true;
            }
            Ok(_) => {}
            Err(code) => self.problem = Some(words(code).to_owned()),
        }
        true
    }

    fn take(&mut self, page: &AgentMessages, before: Option<Cursor>) {
        assert!(
            page.project_id == self.project && page.agent == self.agent,
            "a Messages page answers the agent it was read for"
        );
        let picked = self.entries.get(self.cursor).map(AgentMessage::cursor);
        let at_newest = self.cursor + 1 >= self.entries.len();
        let gap = before.is_none()
            && page.older
            && match (
                page.entries.iter().map(AgentMessage::cursor).min(),
                self.entries.last().map(AgentMessage::cursor),
            ) {
                (Some(oldest), Some(newest)) => oldest > newest,
                _ => false,
            };
        if gap {
            self.entries.clear();
            self.opened.clear();
        }
        if before.is_some() || self.entries.is_empty() {
            self.older = page.older;
        }
        let mut merged: BTreeMap<Cursor, AgentMessage> = self
            .entries
            .drain(..)
            .map(|entry| (entry.cursor(), entry))
            .collect();
        let mut changed = BTreeSet::new();
        for entry in &page.entries {
            if merged
                .insert(entry.cursor(), entry.clone())
                .is_some_and(|kept| kept != *entry)
            {
                changed.insert(entry.cursor());
            }
        }
        self.entries = merged.into_values().collect();
        self.settings = Some(page.settings);
        let last = self.entries.len().saturating_sub(1);
        let cursor = match picked {
            Some(picked) if !at_newest || before.is_some() => self
                .entries
                .iter()
                .position(|entry| entry.cursor() == picked)
                .unwrap_or(last),
            _ => last,
        };
        let over = self.entries.len().saturating_sub(KEPT_MAX);
        self.cursor = if over == 0 {
            cursor
        } else if cursor * 2 < self.entries.len() {
            self.entries.truncate(KEPT_MAX);
            self.newer = true;
            cursor.min(KEPT_MAX - 1)
        } else {
            self.entries.drain(..over);
            self.older = true;
            cursor.saturating_sub(over)
        };
        let entries = &self.entries;
        self.drawn.get_mut().lines.retain(|(at, _), _| {
            !changed.contains(at)
                && entries
                    .binary_search_by_key(at, AgentMessage::cursor)
                    .is_ok()
        });
        let last = self.entries.len().saturating_sub(1);
        assert!(self.entries.len() <= KEPT_MAX && self.cursor <= last);
    }

    fn ask_older(&mut self) {
        if self.older && self.asked.is_none() {
            self.want_older = true;
        }
    }

    /// Handles one key while the list is open: the arrows move, PgUp and PgDn move a page,
    /// PgUp on the oldest entry or Home asks for the older page, End goes to the newest and
    /// reads it again when newer entries were dropped,
    /// Enter opens or closes a task result and Esc closes the list.
    /// Takes the key. Returns whether the list stays open.
    pub fn key(&mut self, key: &Key) -> ListKey {
        let last = self.entries.len().saturating_sub(1);
        match key {
            Key::Esc => return ListKey::Closed,
            Key::Up => self.cursor = self.cursor.saturating_sub(1),
            Key::Down => self.cursor = (self.cursor + 1).min(last),
            Key::PageDown => self.cursor = (self.cursor + PAGE_STEP).min(last),
            Key::End => {
                self.cursor = last;
                if self.newer {
                    self.newer = false;
                    self.read_ms = None;
                }
            }
            Key::PageUp if self.cursor > 0 => self.cursor = self.cursor.saturating_sub(PAGE_STEP),
            Key::PageUp => self.ask_older(),
            Key::Home => {
                self.cursor = 0;
                self.ask_older();
            }
            Key::Enter => self.open_result(),
            _ => {}
        }
        ListKey::Handled
    }

    fn open_result(&mut self) {
        let Some(entry) = self.entries.get(self.cursor) else {
            return;
        };
        if entry.result.is_none() {
            return;
        }
        let (key, cursor) = ((entry.source, entry.id), entry.cursor());
        if !self.opened.remove(&key) {
            self.opened.insert(key);
        }
        let drawn = self.drawn.get_mut();
        for picked in [false, true] {
            drawn.lines.remove(&(cursor, picked));
        }
    }

    /// The keys the open list offers, as key and label pairs.
    pub fn help(&self) -> Vec<(&'static str, &'static str)> {
        let mut help = vec![("\u{2191}\u{2193}", "Move")];
        if self.older {
            help.push(("PgUp", "Older"));
        }
        if self
            .entries
            .get(self.cursor)
            .is_some_and(|entry| entry.result.is_some())
        {
            help.push(("Enter", "Result"));
        }
        if self.newer {
            help.push(("End", "Newest"));
        }
        help.push(("Esc", "Back"));
        help
    }

    /// Draws the list: one line about older entries, a failed read's reason, then every entry
    /// oldest first, scrolled so the picked one shows.
    /// Takes the line width and the rows available.
    /// Returns at most `room` lines (one when `room` is zero), each at most `limit` cells.
    pub fn lines(&self, limit: usize, room: usize) -> Vec<Shown> {
        let limit = limit.max(1);
        let room = room.max(1);
        let mut head = Vec::new();
        if !self.loaded && self.problem.is_none() {
            head.push(muted("Reading messages..."));
        }
        let top = match &self.asked {
            Some(SetupQuery::TeamMessages {
                before: Some(_), ..
            }) => Some("Reading older messages...".to_owned()),
            _ if !self.loaded => None,
            _ if self.older => Some("PgUp at the top reads older messages.".to_owned()),
            _ => Some("Start of the messages.".to_owned()),
        };
        head.extend(top.as_deref().map(muted));
        if self.newer {
            head.push(muted(
                "Newer messages are not shown. End goes back to the newest.",
            ));
        }
        if let Some(problem) = &self.problem {
            head.push(Shown {
                text: problem.clone(),
                role: Some(Role::Warning),
            });
        }
        if self.loaded && self.entries.is_empty() {
            head.push(muted("No messages yet."));
        }
        let mut head: Vec<Shown> = head
            .into_iter()
            .flat_map(|line| {
                wrap(&line.text, limit).into_iter().map(move |text| Shown {
                    text,
                    role: line.role,
                })
            })
            .collect();
        head.truncate(room);
        let body = room - head.len();
        head.extend(self.window(limit, body));
        assert!(head.len() <= room);
        head
    }

    /// The entry lines that fill `body` rows with the picked entry in view: the entries above
    /// it up to the top, then the ones below it when the top comes first. Wraps only the
    /// entries it shows, each once per width and results setting through the drawn cache.
    /// Takes the line width and the rows for entries. Returns at most `body` lines.
    fn window(&self, limit: usize, body: usize) -> Vec<Shown> {
        if body == 0 || self.entries.is_empty() {
            return Vec::new();
        }
        let mut drawn = self.drawn.borrow_mut();
        let full = self
            .settings
            .is_some_and(|settings| settings.results == ResultsShown::Full);
        if (drawn.limit, drawn.full) != (limit, full) {
            *drawn = Drawn {
                limit,
                full,
                lines: BTreeMap::new(),
            };
        }
        let mut shown = self.drawn_lines(&mut drawn, self.cursor, limit);
        shown.truncate(body);
        let mut index = self.cursor;
        while shown.len() < body && index > 0 {
            index -= 1;
            let lines = self.drawn_lines(&mut drawn, index, limit);
            let keep = lines.len().min(body - shown.len());
            shown.splice(0..0, lines[lines.len() - keep..].iter().cloned());
        }
        let mut index = self.cursor + 1;
        while shown.len() < body && index < self.entries.len() {
            let lines = self.drawn_lines(&mut drawn, index, limit);
            let keep = lines.len().min(body - shown.len());
            shown.extend(lines.into_iter().take(keep));
            index += 1;
        }
        assert!(shown.len() <= body);
        shown
    }

    fn drawn_lines(&self, drawn: &mut Drawn, index: usize, limit: usize) -> Vec<Shown> {
        let entry = &self.entries[index];
        let picked = index == self.cursor;
        drawn
            .lines
            .entry((entry.cursor(), picked))
            .or_insert_with(|| self.entry_lines(entry, picked, limit))
            .clone()
    }

    fn entry_lines(&self, entry: &AgentMessage, picked: bool, limit: usize) -> Vec<Shown> {
        #[cfg(test)]
        self.wraps.set(self.wraps.get() + 1);
        let mark = if picked { "> " } else { "  " };
        let lead = format!(
            "{}  {}:",
            clock(entry.at_ms, self.offset),
            said(entry.from, entry.kind)
        );
        let mut text = entry
            .text
            .as_deref()
            .map_or_else(|| "Message sent, text not kept".to_owned(), sanitize);
        if entry.state == Some(EventState::Dropped) {
            text.push_str(" (not delivered)");
        }
        let inline = width(mark) + width(&lead) + 1;
        let beside = limit >= inline + TEXT_MIN;
        let (mut lines, indent) = if beside {
            (vec![format!("{mark}{lead} ")], inline)
        } else {
            let mut heads = wrap(&lead, limit.saturating_sub(width(mark)).max(1)).into_iter();
            let first = format!("{mark}{}", heads.next().unwrap_or_default());
            let lines: Vec<String> = std::iter::once(first)
                .chain(heads.map(|line| format!("  {line}")))
                .collect();
            (lines, width(TEXT_GUTTER).min(limit.saturating_sub(1)))
        };
        let room = limit.saturating_sub(indent).max(1);
        let pad: String = if beside {
            " ".repeat(indent)
        } else {
            TEXT_GUTTER.chars().take(indent).collect()
        };
        let mut words = paragraphs(&text, room).into_iter();
        if beside {
            lines[0].push_str(&words.next().unwrap_or_default());
            let kept = lines[0].trim_end().len();
            lines[0].truncate(kept);
        }
        lines.extend(words.map(|line| format!("{pad}{line}")));
        let role = picked.then_some(Role::Accent);
        let mut shown: Vec<Shown> = lines
            .into_iter()
            .enumerate()
            .map(|(at, line)| Shown {
                text: cut(&line, limit, ""),
                role: if at == 0 { role } else { None },
            })
            .collect();
        if let Some(result) = &entry.result {
            let full = self
                .settings
                .is_some_and(|settings| settings.results == ResultsShown::Full)
                || self.opened.contains(&(entry.source, entry.id));
            let mut result = paragraphs(&format!("Result: {}", sanitize(result)), room);
            if !full {
                result.truncate(1);
            }
            if full && entry.result_more {
                result.extend(paragraphs("The whole result is in the task.", room));
            }
            shown.extend(
                result
                    .into_iter()
                    .map(|line| muted(&cut(&format!("{pad}{line}"), limit, ""))),
            );
        }
        shown
    }
}

/// Who said an entry and what it is, the way a row names it: "Master > task", "You > prompt",
/// "Agent < question". The speaker comes only from the engine's `from` field, never from the
/// text. Takes the speaker and the kind. Returns the words.
fn said(from: Speaker, kind: EntryKind) -> String {
    let (who, way) = match from {
        Speaker::Master => ("Master", ">"),
        Speaker::Owner => ("You", ">"),
        Speaker::Agent => ("Agent", "<"),
        Speaker::Engine => ("Engine", "<"),
    };
    let what = match kind {
        EntryKind::Task => "task",
        EntryKind::Wake => "wake",
        EntryKind::Message => "message",
        EntryKind::Answer => "answer",
        EntryKind::Prompt => "prompt",
        EntryKind::Question => "question",
        EntryKind::Progress => "progress",
        EntryKind::ChangeSetReady => "change set ready",
        EntryKind::Finished => "finished",
        EntryKind::Failed => "failed",
    };
    format!("{who} {way} {what}")
}

/// Wraps cleaned text to a width, one paragraph per line break, without blank lines.
/// Takes printable text and the width. Returns the lines.
fn paragraphs(text: &str, limit: usize) -> Vec<String> {
    wrap(text, limit.max(1))
        .into_iter()
        .filter(|line| !line.trim().is_empty())
        .collect()
}

/// A name or label on one line: cleaned of control and hidden characters, breaks as spaces.
fn one_line(text: &str) -> String {
    sanitize(text).replace('\n', " ")
}

fn muted(text: &str) -> Shown {
    Shown {
        text: text.to_owned(),
        role: Some(Role::Muted),
    }
}

/// Turns a failed Messages read into the sentence shown in the list.
/// Takes the engine's plain code. Returns the sentence.
fn words(code: &str) -> &'static str {
    match code {
        "team_agent_not_found" => "That agent is no longer part of this team.",
        "team_messages_limit_invalid" => "The engine refused the page size.",
        "trusted_connection_unavailable" | "controller_disconnected" => {
            "The engine connection is not ready yet. The list reads again when it is back."
        }
        other => crate::config::plain_problem(other).unwrap_or("The messages could not be read."),
    }
}

#[cfg(all(test, unix))]
mod tests;
