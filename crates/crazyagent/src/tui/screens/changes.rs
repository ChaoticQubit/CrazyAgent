use super::intake::NOT_READY;
use super::new_project_input::TextInput;
use super::new_project_view::{Look, Row, columns, list_row, printable, spread, wrap_words};
use super::project::plain_problem;
use super::project_team::{Help, Notice, TABS, window};
use super::project_time::duration;
use crate::ide::bridge::reveal::RevealTarget;
use crate::landing::{
    AcceptOutcome, AgentKind, ChangeDiff, ChangeSetId, ChangeSetView, ChangeState, ChangedFile,
    FileKind, FileMark, FileStatus, MESSAGE_BYTES_MAX, TargetBranches, TurnOutcome,
};
use crate::protocol::ProjectId;
use crate::tui::client::changes::{ChangesEvent, ChangesQuery, ChangesView};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::Style;
use std::cell::Cell;

const LIST_POLL_MS: i64 = 5_000;
const DIFF_HEADER: &str = "diff --git ";
const QUOTED_HEADER: &str = "diff --git \"";
const DIFF_LINES_MAX: usize = 20_000;
const DIFF_AHEAD: usize = 40;
const DIFF_PAGE_ROWS: usize = 20;
const LIST_ROWS_MAX: usize = 8;
const FILES_SHOWN: usize = 10;
const FILES_MAX: usize = 10_000;
const CONFLICTS_SHOWN: usize = 5;
const SHORT_COMMIT: usize = 7;
const TAB_SPACES: &str = "    ";
const AGENT_WIDTH: usize = 24;
const STATE_WIDTH: usize = 30;
const NOT_READY_LINE: &str = "Landing changes isn't ready in this engine yet.";
const MESSAGE_ROWS: usize = 6;
const UPDATED_NOTICE: &str =
    "Your branch is merged into the agent's copy. The list shows when the agent picks it up.";
const MOVED_NOTE: &str = "These changes moved on since you opened them. They are read again from the top: Accept and Discard come back at the last page.";
const WAITING_NOTE: &str = "Your branch is merged into the agent's copy, but the agent wasn't handed the work to resolve it. Ask it to update again, or discard.";
const NO_FILE_SHOWN: &str = "Scroll the diff to a changed text file to open it in your editor.";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ask {
    Accept,
    Discard,
}

struct Diff {
    id: ChangeSetId,
    head: String,
    files: Vec<ChangedFile>,
    files_done: bool,
    files_cut: bool,
    lines: Vec<String>,
    next: Option<u16>,
    truncated: bool,
    top: usize,
    moved: bool,
    seen_all: bool,
    skip: usize,
    width: Cell<usize>,
}

impl Diff {
    /// Starts reading a change set's diff from its first page at one head, with nothing
    /// read yet. Inputs: the change set, the head and whether the set moved on since the
    /// owner opened it. Output: the diff, waiting for its first page.
    fn at(id: ChangeSetId, head: String, moved: bool) -> Self {
        Self {
            id,
            head,
            files: Vec::new(),
            files_done: false,
            files_cut: false,
            lines: Vec::new(),
            next: Some(0),
            truncated: false,
            top: 0,
            moved,
            seen_all: false,
            skip: 0,
            width: Cell::new(0),
        }
    }

    /// Tells whether Accept and Discard may name this diff's head: always for the head
    /// the owner opened, and after a move only once the new head's last page was read
    /// (RV-DS-79). Output: the answer.
    fn reviewed(&self) -> bool {
        !self.moved || self.seen_all
    }

    /// Whether the next page is wanted now: while the file list is not complete, so every
    /// engine risk mark is shown even past the text limit, else while the owner scrolls
    /// toward the end of the text read so far.
    fn wants_more(&self) -> bool {
        self.next.is_some()
            && (!self.files_done
                || (self.lines.len() < DIFF_LINES_MAX && self.top + DIFF_AHEAD >= self.lines.len()))
    }

    /// Takes one page: its files up to `FILES_MAX` and its text up to `DIFF_LINES_MAX` lines.
    /// The file list is complete at the first page without files or at the last page; text
    /// past the limit is not read once the file list is complete. Input: the page.
    fn add(&mut self, page: &ChangeDiff) {
        let room = FILES_MAX - self.files.len();
        self.files_cut |= page.files.len() > room;
        self.files.extend(page.files.iter().take(room).cloned());
        self.files_done |= self.files_cut || page.files.is_empty() || page.next.is_none();
        if self.lines.len() < DIFF_LINES_MAX {
            self.lines.extend(page.text.lines().map(diff_line));
        } else {
            self.truncated |= !page.text.is_empty();
        }
        if self.lines.len() > DIFF_LINES_MAX {
            self.lines.truncate(DIFF_LINES_MAX);
            self.truncated = true;
        }
        self.truncated |= page.truncated;
        self.next = page.next;
        if self.files_done && self.lines.len() >= DIFF_LINES_MAX {
            self.next = None;
        }
        assert!(self.lines.len() <= DIFF_LINES_MAX);
        assert!(self.files.len() <= FILES_MAX);
        assert!(self.files_done || self.next.is_some());
    }

    /// Whether a diff header line starts a file that carries the instruction file mark; a
    /// quoted header, which git writes for unusual names, counts when any file is marked.
    /// Input: the header line. Output: true for a marked file.
    fn marked_header(&self, header: &str) -> bool {
        let instructions = instruction_headers(&self.files);
        instructions.iter().any(|known| known == header)
            || (header.starts_with(QUOTED_HEADER) && !instructions.is_empty())
    }

    /// How many view rows one diff line takes: its wrapped rows at the width last drawn when
    /// it belongs to a marked instruction file, which is shown in full, else one, as other
    /// lines are cut. Input: the line index. Output: the rows, at least one.
    fn rows_of(&self, index: usize) -> usize {
        let width = self.width.get();
        let Some(line) = self.lines.get(index) else {
            return 1;
        };
        let marked = self.lines[..=index]
            .iter()
            .rev()
            .find(|line| line.starts_with(DIFF_HEADER))
            .is_some_and(|header| self.marked_header(header));
        if width == 0 || !marked {
            return 1;
        }
        crate::tui::text::wrap(&printable(line.clone()), width)
            .len()
            .max(1)
    }

    /// Moves the view down by rows: through the rest of a marked line's wrapped rows before
    /// the next line, so a marked line taller than the view can be read to its end.
    /// Input: the rows.
    fn down(&mut self, rows: usize) {
        let last = self.lines.len().saturating_sub(1);
        self.skip = self.skip.min(self.rows_of(self.top) - 1);
        for _ in 0..rows {
            if self.skip + 1 < self.rows_of(self.top) {
                self.skip += 1;
            } else if self.top < last {
                self.top += 1;
                self.skip = 0;
            } else {
                break;
            }
        }
        assert!(self.top <= last && self.skip < self.rows_of(self.top));
    }

    /// Moves the view up by rows: through a marked line's wrapped rows, entering a line
    /// above at its last row. Input: the rows.
    fn up(&mut self, rows: usize) {
        self.skip = self.skip.min(self.rows_of(self.top) - 1);
        for _ in 0..rows {
            if self.skip > 0 {
                self.skip -= 1;
            } else if self.top > 0 {
                self.top -= 1;
                self.skip = self.rows_of(self.top) - 1;
            } else {
                break;
            }
        }
        assert!(self.skip < self.rows_of(self.top));
    }
}

/// One project's change sets: what each agent changed, waiting for the owner to accept,
/// discard, restore or send back to the agent to update.
/// Takes engine replies through `apply` and keys through `key`; never invents data.
/// Sends at most one request at a time; an action waits for a read in flight.
pub(in crate::tui) struct ProjectChanges {
    pub project: ProjectId,
    sets: Vec<ChangeSetView>,
    loaded: bool,
    not_ready: bool,
    cursor: usize,
    selected: Option<ChangeSetId>,
    dropped: bool,
    diff: Option<Diff>,
    asking: Option<Ask>,
    target: Option<(ChangeSetId, String)>,
    branches: Option<(ChangeSetId, TargetBranches)>,
    branch: Option<usize>,
    wanted_branches: Option<ChangeSetId>,
    message: TextInput,
    notice: Option<Notice>,
    notice_set: Option<ChangeSetId>,
    queued: Option<ChangesQuery>,
    sent: Option<ChangesQuery>,
    stale: bool,
    read_ms: i64,
    edit: Option<String>,
    reveal: Option<RevealTarget>,
}

impl ProjectChanges {
    /// Opens one project's Changes view with nothing read yet.
    /// Takes the project.
    /// Returns the view; reads start with `next_query`.
    pub fn new(project: ProjectId) -> Self {
        Self {
            project,
            sets: Vec::new(),
            loaded: false,
            not_ready: false,
            cursor: 0,
            selected: None,
            dropped: false,
            diff: None,
            asking: None,
            target: None,
            branches: None,
            branch: None,
            wanted_branches: None,
            message: TextInput::multi(MESSAGE_BYTES_MAX),
            notice: None,
            notice_set: None,
            queued: None,
            sent: None,
            stale: true,
            read_ms: 0,
            edit: None,
            reveal: None,
        }
    }

    /// Asks for a fresh list on the next `next_query`, as when the owner opens the tab.
    /// Takes the view. Returns nothing.
    pub fn refresh(&mut self) {
        self.stale = true;
    }

    /// Gives the next request: a queued action first, then a diff page the owner is
    /// scrolling toward, then the list when it is stale or older than the poll interval.
    /// Takes the time now in milliseconds. Returns the query, or none while one is in flight.
    pub fn next_query(&mut self, now_ms: i64) -> Option<ChangesQuery> {
        if self.sent.is_some() {
            return None;
        }
        let query = if let Some(action) = self.queued.take() {
            action
        } else if let Some(id) = self.wanted_branches.take() {
            ChangesQuery::Branches { id }
        } else if let Some(diff) = self.diff.as_ref().filter(|diff| diff.wants_more()) {
            ChangesQuery::Diff {
                id: diff.id.clone(),
                head: diff.head.clone(),
                page: diff.next?,
            }
        } else if self.stale || (!self.not_ready && now_ms - self.read_ms >= LIST_POLL_MS) {
            self.stale = false;
            self.read_ms = now_ms;
            ChangesQuery::List {
                project: self.project.clone(),
            }
        } else {
            return None;
        };
        self.sent = Some(query.clone());
        Some(query)
    }

    /// Forgets the request in flight after the engine connection dropped.
    /// Takes the view; what is on screen stays. A lost action is not resent: the list
    /// is read again so the owner sees whether it happened.
    /// Returns nothing.
    pub fn forget_reads(&mut self) {
        let Some(lost) = self.sent.take() else {
            return;
        };
        self.stale = true;
        if !matches!(
            lost,
            ChangesQuery::List { .. } | ChangesQuery::Diff { .. } | ChangesQuery::Branches { .. }
        ) {
            self.say(
                Notice::problem(
                    "The engine connection dropped before it answered. Check the list before trying again.",
                ),
                None,
            );
        }
    }

    /// Says whether a request is in flight or an action is waiting to be sent.
    /// Takes the view. Returns true until its reply arrives.
    pub fn busy(&self) -> bool {
        self.sent.is_some() || self.queued.is_some()
    }

    /// Applies one Changes reply.
    /// Takes the event; a reply to a request this view did not send is left alone.
    /// Returns nothing.
    pub fn apply(&mut self, event: &ChangesEvent) {
        if self.sent.as_ref() != Some(&event.query) {
            return;
        }
        self.sent = None;
        if matches!(event.query, ChangesQuery::List { .. })
            && matches!(&event.result, Err(code) if code == "changes_unavailable")
        {
            self.not_ready = true;
            self.loaded = true;
            self.diff = None;
            self.asking = None;
            self.notice = None;
            self.notice_set = None;
            return;
        }
        let about = about(&event.query).cloned();
        match (&event.query, &event.result) {
            (ChangesQuery::List { .. }, Ok(ChangesView::List(sets))) => {
                self.sets = sets.clone();
                self.loaded = true;
                self.not_ready = false;
                if let Some(diff) = self.diff.as_mut() {
                    match self.sets.iter().find(|set| set.id == diff.id) {
                        None => self.diff = None,
                        Some(set)
                            if set.head != diff.head
                                || (diff.moved && !diff.seen_all && diff.next.is_none()) =>
                        {
                            *diff = Diff::at(set.id.clone(), set.head.clone(), true);
                            self.notice = Some(Notice::problem(MOVED_NOTE));
                        }
                        Some(_) => {}
                    }
                }
            }
            (ChangesQuery::List { .. }, Err(code)) => {
                self.say(
                    Notice::problem(format!("Couldn't load changes. {}", refusal(code))),
                    None,
                );
            }
            (ChangesQuery::Diff { id, head, page }, Ok(ChangesView::Diff(page_diff))) => {
                if let Some(diff) = self
                    .diff
                    .as_mut()
                    .filter(|diff| &diff.id == id && &diff.head == head)
                    && diff.next == Some(*page)
                {
                    diff.add(page_diff);
                    diff.seen_all = diff.next.is_none();
                }
            }
            (ChangesQuery::Diff { id, .. }, Err(code)) if code == "changes_stale" => {
                if let Some(diff) = self.diff.as_mut().filter(|diff| &diff.id == id) {
                    *diff = Diff {
                        next: None,
                        ..Diff::at(diff.id.clone(), diff.head.clone(), true)
                    };
                    self.stale = true;
                    self.notice = Some(Notice::problem(MOVED_NOTE));
                }
            }
            (ChangesQuery::Diff { .. }, Err(code)) => {
                if let Some(diff) = self.diff.as_mut() {
                    diff.next = None;
                }
                self.say(
                    Notice::problem(format!("Couldn't load the changes. {}", refusal(code))),
                    about,
                );
            }
            (ChangesQuery::Branches { id }, Ok(ChangesView::Branches(listed))) => {
                if self.target.as_ref().is_some_and(|(asked, _)| asked == id) {
                    self.branch = match listed.land_on_problem {
                        Some(_) => None,
                        None => Some(
                            landing_default(listed)
                                .and_then(|default| {
                                    listed.branches.iter().position(|name| name == default)
                                })
                                .unwrap_or(0),
                        ),
                    };
                    self.branches = Some((id.clone(), listed.clone()));
                }
            }
            (ChangesQuery::Branches { .. }, Err(code)) => {
                self.say(
                    Notice::problem(format!(
                        "Couldn't list your branches, so these changes land on your project's Land on branch. {}",
                        refusal(code)
                    )),
                    about,
                );
            }
            (_, Ok(view)) => {
                self.stale = true;
                self.say(done(view), about);
                if !matches!(view, ChangesView::Accepted(AcceptOutcome::Conflict { .. })) {
                    self.diff = None;
                }
            }
            (_, Err(code)) => {
                self.stale = true;
                self.say(Notice::problem(refusal(code)), about);
            }
        }
        self.clamp();
    }

    /// Keeps the highlight on the change set the owner chose, not on its place in the list,
    /// since each read can reorder the sets: on the set a question was asked about while it
    /// is open, else on the selected set. A chosen set that left the list takes the selection
    /// with it, so no key acts on the set now in its place until the owner picks again; the
    /// first set is picked only before the owner picked any. Then drops a question whose set
    /// is gone. Input: the view. Output: none.
    fn clamp(&mut self) {
        let anchor = match (&self.asking, &self.target) {
            (Some(_), Some((id, _))) => Some(id),
            _ => self.selected.as_ref(),
        };
        match anchor.map(|id| self.sets.iter().position(|set| &set.id == id)) {
            Some(Some(index)) => self.cursor = index,
            Some(None) => {
                self.selected = None;
                self.dropped = true;
            }
            None => {}
        }
        self.cursor = self.cursor.min(self.sets.len().saturating_sub(1));
        if self.selected.is_none() && !self.dropped {
            self.select(self.cursor);
        }
        let target_listed = self
            .target
            .as_ref()
            .is_some_and(|(id, _)| self.sets.iter().any(|set| &set.id == id));
        if self.chosen().is_none() || !target_listed {
            self.asking = None;
        }
        assert!(self.sets.is_empty() || self.cursor < self.sets.len());
        assert!(
            self.selected
                .as_ref()
                .is_none_or(|id| self.sets.get(self.cursor).is_some_and(|set| &set.id == id))
        );
    }

    /// Shows one notice: page-wide when it is about no change set, else only while that set
    /// is chosen, so the outcome of an action on one set never reads as another set's
    /// details. Inputs: the notice and the set it is about. Output: none.
    fn say(&mut self, notice: Notice, about: Option<ChangeSetId>) {
        self.notice = Some(notice);
        self.notice_set = about;
    }

    /// The notice to draw now: a page-wide one, or one about the chosen change set.
    /// Input: the view. Output: the notice, or None.
    fn shown_notice(&self) -> Option<&Notice> {
        assert!(self.notice_set.is_none() || self.notice.is_some());
        let chosen = self.chosen().map(|set| &set.id);
        self.notice
            .as_ref()
            .filter(|_| self.notice_set.as_ref().is_none_or(|id| chosen == Some(id)))
    }

    /// Moves the highlight to a place in the list and remembers the set there by its id.
    /// Input: the place, bounded to the list. Output: none.
    fn select(&mut self, index: usize) {
        self.cursor = index.min(self.sets.len().saturating_sub(1));
        self.selected = self.sets.get(self.cursor).map(|set| set.id.clone());
        self.dropped = false;
    }

    fn chosen(&self) -> Option<&ChangeSetView> {
        let id = match &self.diff {
            Some(diff) => &diff.id,
            None => self.selected.as_ref()?,
        };
        self.sets.iter().find(|set| &set.id == id)
    }

    /// The actions the chosen change set allows now, with Accept and Discard held back
    /// while an open diff that moved on is not read to its last page (RV-DS-79).
    /// Input: the time now. Output: the actions, or none when no set is chosen.
    fn actions(&self, now_ms: i64) -> Option<Actions> {
        let reviewed = self.diff.as_ref().is_none_or(Diff::reviewed);
        self.chosen().map(|set| {
            let mut actions = Actions::of(set, now_ms);
            actions.accept &= reviewed;
            actions.discard &= reviewed;
            actions
        })
    }

    fn reviewed_head(&self) -> Option<String> {
        let set = self.chosen()?;
        Some(match &self.diff {
            Some(diff) => diff.head.clone(),
            None => set.head.clone(),
        })
    }

    /// Handles one key: move through the change sets, open a diff and scroll it,
    /// accept after editing the commit message or discard after a yes, ask the agent to
    /// update, restore, or name the file shown in the diff for the owner's editor.
    /// Takes the key event. Returns true when the view used the key; other keys,
    /// including Esc on the list, are left to the host.
    pub fn key(&mut self, key: KeyEvent, now_ms: i64) -> bool {
        if self.asking == Some(Ask::Accept) {
            match key.code {
                KeyCode::Enter if key.modifiers.is_empty() => {
                    if !self.message.value().trim().is_empty() && self.unpicked_problem().is_none()
                    {
                        self.asking = None;
                        self.act(Ask::Accept);
                    }
                }
                KeyCode::Esc => {
                    self.asking = None;
                    self.branches = None;
                    self.wanted_branches = None;
                }
                KeyCode::Tab => self.turn_branch(true),
                KeyCode::BackTab => self.turn_branch(false),
                _ => {
                    self.message.handle(key);
                }
            }
            return true;
        }
        if key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SHIFT)
        {
            return false;
        }
        if let Some(ask) = self.asking {
            match key.code {
                KeyCode::Char('y') => {
                    self.asking = None;
                    self.act(ask);
                }
                KeyCode::Char('n') | KeyCode::Esc => self.asking = None,
                _ => {}
            }
            return true;
        }
        if self.not_ready || !self.loaded {
            return false;
        }
        let actions = self.actions(now_ms);
        let used = match (key.code, actions) {
            (KeyCode::Char('a'), Some(actions)) if actions.accept => {
                let proposed = self.chosen().map(|set| set.message.clone());
                self.message.set(&proposed.unwrap_or_default());
                self.notice = None;
                self.notice_set = None;
                self.ask(Ask::Accept);
                self.branches = None;
                self.branch = None;
                self.wanted_branches = self.target.as_ref().map(|(id, _)| id.clone());
                true
            }
            (KeyCode::Char('d'), Some(actions)) if actions.discard => {
                self.ask(Ask::Discard);
                true
            }
            (KeyCode::Char('u'), Some(actions)) if actions.update => {
                self.update();
                true
            }
            (KeyCode::Char('r'), Some(actions)) if actions.restore => {
                self.restore();
                true
            }
            (KeyCode::Char('v'), _) if self.diff.is_some() => {
                self.edit = self.shown_file();
                if self.edit.is_none() {
                    self.notice = Some(Notice::problem(NO_FILE_SHOWN));
                }
                true
            }
            (KeyCode::Char('O'), _) => {
                let target = self.chosen().map(|set| RevealTarget::ChangeSet {
                    id: set.id.clone(),
                    path: self.diff.as_ref().and_then(|_| self.shown_file()),
                });
                self.reveal = target;
                true
            }
            _ if self.diff.is_some() => self.diff_key(key.code),
            _ => self.list_key(key.code),
        };
        self.clamp();
        used
    }

    /// Takes the changed file the owner asked to open in their editor with `v`.
    /// Takes the view. Returns the path the change set lists, once per ask.
    pub fn take_edit(&mut self) -> Option<String> {
        self.edit.take()
    }

    /// Takes what the owner asked to open in the desktop IDE with `O`: the chosen change
    /// set's worktree, or the file whose diff is shown. Takes the view. Returns it once.
    pub fn take_reveal(&mut self) -> Option<RevealTarget> {
        self.reveal.take()
    }

    /// Names the changed file whose diff section is at the top of the diff view, from that
    /// section's `+++ b/` line, or its `--- a/` line for a deleted file, read only before
    /// its first hunk. Takes the view. Returns the path, only when the change set lists it
    /// as a text file, so a quoted, binary or unlisted path names none.
    fn shown_file(&self) -> Option<String> {
        let diff = self.diff.as_ref()?;
        let last = diff.lines.len().checked_sub(1)?;
        let start = diff.lines[..=diff.top.min(last)]
            .iter()
            .rposition(|line| line.starts_with(DIFF_HEADER))?;
        let (mut new, mut old) = (None, None);
        for line in diff.lines[start + 1..]
            .iter()
            .take_while(|line| !line.starts_with(DIFF_HEADER) && !line.starts_with("@@"))
        {
            if let Some(path) = line.strip_prefix("+++ b/") {
                new = Some(path);
            } else if let Some(path) = line.strip_prefix("--- a/") {
                old = Some(path);
            }
        }
        let named = new.or(old)?;
        let file = diff.files.iter().find(|file| {
            file.kind == FileKind::Text
                && (file.path == named
                    || named.strip_suffix(TAB_SPACES) == Some(file.path.as_str()))
        })?;
        Some(file.path.clone())
    }

    /// Tells whether the owner is editing the Accept commit message, so the host leaves
    /// typed characters to it. Output: true while the message editor is open.
    pub fn editing(&self) -> bool {
        self.asking == Some(Ask::Accept)
    }

    /// Inserts pasted text into the Accept commit message while it is being edited.
    /// Input: untrusted pasted text. Output: true when the editor took it.
    pub fn paste(&mut self, text: &str) -> bool {
        if self.editing() {
            self.message.insert_str(text);
        }
        self.editing()
    }

    fn list_key(&mut self, code: KeyCode) -> bool {
        match code {
            KeyCode::Up | KeyCode::Down if self.dropped => self.select(self.cursor),
            KeyCode::Up => self.select(self.cursor.saturating_sub(1)),
            KeyCode::Down => self.select(self.cursor.saturating_add(1)),
            KeyCode::Enter => {
                let Some(set) = self.chosen() else {
                    return true;
                };
                self.diff = Some(Diff::at(set.id.clone(), set.head.clone(), false));
            }
            _ => return false,
        }
        true
    }

    fn diff_key(&mut self, code: KeyCode) -> bool {
        let Some(diff) = self.diff.as_mut() else {
            return false;
        };
        match code {
            KeyCode::Up => diff.up(1),
            KeyCode::Down => diff.down(1),
            KeyCode::PageUp => diff.up(DIFF_PAGE_ROWS),
            KeyCode::PageDown | KeyCode::Char(' ') => diff.down(DIFF_PAGE_ROWS),
            KeyCode::Esc => self.diff = None,
            _ => return false,
        }
        true
    }

    fn queue(&mut self, query: ChangesQuery) {
        if self.queued.is_some()
            || self.sent.as_ref().is_some_and(|sent| {
                !matches!(
                    sent,
                    ChangesQuery::List { .. }
                        | ChangesQuery::Diff { .. }
                        | ChangesQuery::Branches { .. }
                )
            })
        {
            self.say(
                Notice::problem("Still working on the last change. Try again in a moment."),
                None,
            );
            return;
        }
        self.notice = None;
        self.notice_set = None;
        self.queued = Some(query);
    }

    fn ask(&mut self, ask: Ask) {
        let id = self.chosen().map(|set| set.id.clone());
        self.target = id.zip(self.reviewed_head());
        self.asking = self.target.is_some().then_some(ask);
    }

    fn act(&mut self, ask: Ask) {
        let Some((id, expected_head)) = self.target.take() else {
            return;
        };
        let message = self.message.value().trim().to_owned();
        let target = self.chosen_branch();
        self.branches = None;
        self.wanted_branches = None;
        self.queue(match (ask, target) {
            (Ask::Accept, Some(target)) => ChangesQuery::AcceptInto {
                id,
                expected_head,
                message,
                target,
            },
            (Ask::Accept, None) => ChangesQuery::Accept {
                id,
                expected_head,
                message,
            },
            (Ask::Discard, _) => ChangesQuery::Discard { id, expected_head },
        });
    }

    /// Moves the Accept target to the next or previous listed branch, wrapping around, or
    /// picks the first or last one while none is picked. Input: true for the next one.
    /// Output: none; nothing moves before the list arrives.
    fn turn_branch(&mut self, forward: bool) {
        let Some((_, listed)) = &self.branches else {
            return;
        };
        let count = listed.branches.len();
        if count == 0 {
            return;
        }
        self.branch = Some(match self.branch {
            Some(at) if forward => (at + 1) % count,
            Some(at) => (at + count - 1) % count,
            None if forward => 0,
            None => count - 1,
        });
        assert!(self.branch.is_some_and(|at| at < count));
    }

    /// The sentence naming the missing Land on branch while the owner has picked no branch
    /// for Accept, which waits for a pick then (DEC-111 Q1). Output: the sentence, or None.
    fn unpicked_problem(&self) -> Option<&str> {
        match (&self.branches, self.branch) {
            (Some((_, listed)), None) => listed.land_on_problem.as_deref(),
            _ => None,
        }
    }

    /// The branch the owner picked for Accept, when it is not the one the engine lands on
    /// with no branch named, as [`landing_default`] says. Output: the branch, or None for
    /// the default.
    fn chosen_branch(&self) -> Option<String> {
        let (_, listed) = self.branches.as_ref()?;
        let name = listed.branches.get(self.branch?)?;
        (landing_default(listed) != Some(name)).then(|| name.clone())
    }

    /// The branch Accept lands on, once the list arrived, and whether it is checked out.
    /// Output: the branch and that answer, or None before the list arrives.
    fn landing_branch(&self) -> Option<(&str, bool)> {
        let (_, listed) = self.branches.as_ref()?;
        let name = listed.branches.get(self.branch?)?;
        Some((name.as_str(), listed.current.as_ref() == Some(name)))
    }

    fn update(&mut self) {
        if let Some(set) = self.chosen() {
            let id = set.id.clone();
            self.queue(ChangesQuery::Update { id });
        }
    }

    fn restore(&mut self) {
        if let Some(set) = self.chosen() {
            let id = set.id.clone();
            self.queue(ChangesQuery::Restore { id });
        }
    }

    /// Draws the Changes tab: the change sets and the chosen one's details, or its diff.
    /// Takes the content width, the rows available, the time now and the look.
    /// Returns the rows, the commit message cursor as a column and row of those rows
    /// while it is edited, and the help keys.
    pub fn rows(
        &self,
        content: usize,
        room: usize,
        now_ms: i64,
        look: &Look,
    ) -> (Vec<Row>, Option<(usize, usize)>, Help) {
        let mut lines = Vec::new();
        if self.not_ready {
            lines.push(Row::of("Changes", look.heading));
            lines.push(Row::of(NOT_READY, look.fg));
            lines.push(Row::of(NOT_READY_LINE, look.muted));
            return (lines, None, vec![TABS, ("Esc", "Home")]);
        }
        if !self.loaded {
            lines.push(Row::of("Changes", look.heading));
            lines.push(Row::of("Loading changes...", look.muted));
            lines.extend(self.shown_notice().map(|notice| notice.row(look)));
            return (lines, None, vec![TABS, ("Esc", "Home")]);
        }
        let (footer, at) = self.footer(content, look);
        if let Some(diff) = &self.diff {
            let body = room.saturating_sub(footer.len());
            lines.extend(self.diff_rows(diff, content, body, now_ms, look));
        } else {
            lines.extend(self.list_rows(content, room.saturating_sub(footer.len()), now_ms, look));
        }
        let cursor = at
            .map(|(column, row)| (column, lines.len() + row))
            .filter(|(_, row)| *row < room);
        lines.extend(footer);
        (lines, cursor, self.help(now_ms))
    }

    fn list_rows(&self, content: usize, room: usize, now_ms: i64, look: &Look) -> Vec<Row> {
        let mut lines = vec![
            Row::of("Changes", look.heading).push(format!("  {}", self.sets.len()), look.muted),
        ];
        if self.sets.is_empty() {
            lines.extend(
                wrap_words(
                    "No changes yet. When the master or a sub-agent edits files, its changes wait here for you to review and accept.",
                    content,
                )
                .into_iter()
                .map(|line| Row::of(line, look.muted)),
            );
            return lines;
        }
        let shown = LIST_ROWS_MAX.min(room.saturating_sub(8).max(1));
        let first = window(self.cursor, self.sets.len(), shown);
        let widths = [
            AGENT_WIDTH,
            STATE_WIDTH,
            content.saturating_sub(2 + AGENT_WIDTH + STATE_WIDTH + 4),
        ];
        for (index, set) in self.sets.iter().enumerate().skip(first).take(shown) {
            let cells = vec![
                Row::of(agent_name(set), look.fg),
                state_row(set, look),
                Row::of(size_line(set), look.muted),
            ];
            lines.push(list_row(
                columns(&widths, cells),
                content,
                index == self.cursor && self.selected.as_ref() == Some(&set.id),
                true,
                look,
            ));
        }
        lines.push(Row::default());
        if let Some(set) = self.chosen() {
            lines.extend(details(set, content, now_ms, look));
        } else if self.dropped {
            lines.push(Row::of(
                "The change set you picked left the list. Pick one with \u{2191}\u{2193}.",
                look.muted,
            ));
        }
        lines.truncate(room.max(1));
        lines
    }

    fn diff_rows(
        &self,
        diff: &Diff,
        content: usize,
        room: usize,
        now_ms: i64,
        look: &Look,
    ) -> Vec<Row> {
        let mut lines = Vec::new();
        if let Some(set) = self.chosen() {
            lines.push(spread(
                content,
                Row::of(agent_name(set), look.heading),
                state_row(set, look),
            ));
            lines.extend(details(set, content, now_ms, look).into_iter().skip(1));
        }
        let listed = listed_files(&diff.files);
        for file in listed.shown {
            lines.push(Row::of(format!("  {}", file_line(file)), look.fg));
            for mark in file_marks(file) {
                lines.push(Row::of(format!("    ! {mark}"), look.warning));
            }
        }
        if listed.marked_hidden > 0 {
            lines.push(Row::of(
                match listed.marked_hidden {
                    1 => "  1 more file needs checking before you accept.".to_owned(),
                    hidden => format!("  {hidden} more files need checking before you accept."),
                },
                look.warning,
            ));
        }
        if listed.plain_hidden > 0 {
            lines.push(Row::of(
                format!("  and {} more files", listed.plain_hidden),
                look.muted,
            ));
        }
        if diff.files_cut {
            lines.push(Row::of(
                format!(
                    "Only the first {FILES_MAX} changed files are listed. Files after them, and their marks, are not shown."
                ),
                look.warning,
            ));
        }
        if diff.truncated {
            lines.push(Row::of(
                "Part of this diff is not shown: it is longer than CrazyAgent shows at once.",
                look.warning,
            ));
        }
        lines.push(Row::default());
        if diff.lines.is_empty() {
            let waiting = if diff.next.is_some() || !diff.reviewed() {
                "Loading changes..."
            } else {
                "No text changes to show."
            };
            lines.push(Row::of(waiting, look.muted));
            return lines;
        }
        let text_rows = room.saturating_sub(lines.len()).max(1);
        diff.width.set(content.max(1));
        let mut inside = diff
            .lines
            .iter()
            .take(diff.top)
            .rev()
            .find(|line| line.starts_with(DIFF_HEADER))
            .is_some_and(|header| diff.marked_header(header));
        let mut shown = 0;
        for (index, line) in diff.lines.iter().enumerate().skip(diff.top) {
            if shown >= text_rows {
                break;
            }
            if line.starts_with(DIFF_HEADER) {
                inside = diff.marked_header(line);
            }
            let style = diff_style(line, look);
            if inside {
                let pieces = crate::tui::text::wrap(&printable(line.clone()), content.max(1));
                let skip = match index == diff.top {
                    true => diff.skip.min(pieces.len().saturating_sub(1)),
                    false => 0,
                };
                for piece in pieces.into_iter().skip(skip).take(text_rows - shown) {
                    lines.push(Row::of(piece, style).pad(content));
                    shown += 1;
                }
                if line.is_empty() {
                    lines.push(Row::of(String::new(), style).pad(content));
                    shown += 1;
                }
            } else {
                lines.push(Row::of(line.clone(), style).pad(content));
                shown += 1;
            }
        }
        lines.truncate(room.max(1));
        lines
    }

    fn footer(&self, content: usize, look: &Look) -> (Vec<Row>, Option<(usize, usize)>) {
        let Some(ask) = self.asking else {
            let rows = self
                .shown_notice()
                .into_iter()
                .map(|notice| notice.row(look))
                .collect();
            return (rows, None);
        };
        if ask == Ask::Accept {
            return self.message_rows(content, look);
        }
        let (question, size) = self.question(ask);
        let mut rows = vec![
            Row::of(question, look.warning)
                .push("   y", look.heading)
                .push(" Discard   ", look.fg)
                .push("n", look.heading)
                .push(" Keep reviewing", look.fg),
        ];
        rows.extend(size.map(|size| Row::of(size, look.fg)));
        let explain = "The agent's working copy goes back to your branch. You can restore these changes for 7 days.";
        rows.extend(
            wrap_words(explain, content)
                .into_iter()
                .map(|line| Row::of(line, look.muted)),
        );
        (rows, None)
    }

    /// The question an open Accept or Discard asks, naming the change set it is about, with
    /// that set's size and the short head it was reviewed at. Input: the question.
    /// Output: the question line and the size line, or a plain question if the set is gone.
    fn question(&self, ask: Ask) -> (String, Option<String>) {
        let verb = match ask {
            Ask::Accept => "Accept",
            Ask::Discard => "Discard",
        };
        let asked = self.target.as_ref().and_then(|(id, head)| {
            let set = self.sets.iter().find(|set| &set.id == id)?;
            let short = head.get(..SHORT_COMMIT).unwrap_or(head);
            Some((agent_name(set), format!("{}, at {short}.", size_line(set))))
        });
        match asked {
            Some((name, size)) => (format!("{verb} the changes of {name}?"), Some(size)),
            None => (format!("{verb} these changes?"), None),
        }
    }

    fn message_rows(&self, content: usize, look: &Look) -> (Vec<Row>, Option<(usize, usize)>) {
        let (question, size) = self.question(Ask::Accept);
        let mut rows = vec![Row::of(question, look.warning)];
        rows.extend(size.map(|size| Row::of(size, look.fg)));
        let landing = match (self.unpicked_problem(), self.landing_branch()) {
            (Some(problem), _) => problem.to_owned(),
            (None, None) => {
                "They land as one commit on your project's Land on branch, under your own Git name."
                    .to_owned()
            }
            (None, Some((name, true))) => format!(
                "They land as one commit on {name}, the branch checked out in your project folder, under your own Git name."
            ),
            (None, Some((name, false))) => format!(
                "They land as one commit on {name}, under your own Git name. That branch isn't checked out, so the files in your project folder stay as they are."
            ),
        };
        rows.extend(
            wrap_words(
                &format!(
                    "{landing} CrazyAgent did not check them. Edit the commit message if you like."
                ),
                content,
            )
            .into_iter()
            .map(|line| Row::of(line, look.muted)),
        );
        rows.extend(self.shown_notice().map(|notice| notice.row(look)));
        if let Some((name, _)) = self.landing_branch() {
            rows.push(Row::of("Branch", look.heading).push(format!("  {name}"), look.fg));
        }
        rows.push(Row::of("Commit message", look.heading));
        let (lines, (line, column)) = self.message.wrap(content.saturating_sub(2).max(1));
        let first = window(line, lines.len(), MESSAGE_ROWS);
        let cursor = (2 + column, rows.len() + line - first);
        for text in lines.into_iter().skip(first).take(MESSAGE_ROWS) {
            rows.push(Row::of(format!("  {text}"), look.fg).pad(content));
        }
        if self.message.value().trim().is_empty() {
            rows.push(Row::of("Write a commit message to accept.", look.warning));
        }
        if self.unpicked_problem().is_some() {
            rows.push(Row::of(
                "Press Tab to pick the branch they land on.",
                look.warning,
            ));
        }
        (rows, Some(cursor))
    }

    fn help(&self, now_ms: i64) -> Help {
        match self.asking {
            Some(Ask::Accept) => {
                let mut help = vec![
                    ("Enter", "Accept"),
                    ("Ctrl-J", "New line"),
                    ("Esc", "Keep reviewing"),
                ];
                if self.branches.as_ref().is_some_and(|(_, listed)| {
                    listed.branches.len() > 1
                        || (listed.land_on_problem.is_some() && !listed.branches.is_empty())
                }) {
                    help.insert(1, ("Tab", "Branch"));
                }
                return help;
            }
            Some(Ask::Discard) => return vec![("y", "Discard"), ("n", "Keep reviewing")],
            None => {}
        }
        let mut help = if self.diff.is_some() {
            vec![("\u{2191}\u{2193}", "Scroll")]
        } else {
            vec![("\u{2191}\u{2193}", "Move"), ("Enter", "Show changes")]
        };
        if let Some(actions) = self.actions(now_ms) {
            if actions.accept {
                help.push(("a", "Accept"));
            }
            if actions.discard {
                help.push(("d", "Discard"));
            }
            if actions.update {
                help.push(("u", "Ask agent to update"));
            }
            if actions.restore {
                help.push(("r", "Restore"));
            }
        }
        if !self.sets.is_empty() {
            help.push(("O", "Open in IDE"));
        }
        if self.diff.is_some() {
            help.push(("v", "Edit your copy"));
            help.push(("Esc", "Back to the list"));
        } else {
            help.extend([TABS, ("Esc", "Home")]);
        }
        help
    }
}

#[derive(Clone, Copy)]
struct Actions {
    accept: bool,
    discard: bool,
    update: bool,
    restore: bool,
}

impl Actions {
    fn of(set: &ChangeSetView, now_ms: i64) -> Self {
        let open = matches!(set.state, ChangeState::Pending | ChangeState::Conflict);
        Self {
            accept: open,
            discard: open || set.waiting,
            update: set.state == ChangeState::Conflict || set.waiting,
            restore: set.state == ChangeState::Discarded && restorable(set, now_ms),
        }
    }
}

fn restorable(set: &ChangeSetView, now_ms: i64) -> bool {
    set.restorable_until_ms
        .is_some_and(|until| i64::try_from(until).is_ok_and(|until| until > now_ms))
}

fn details(set: &ChangeSetView, content: usize, now_ms: i64, look: &Look) -> Vec<Row> {
    let mut notes = Vec::new();
    if !set.checked {
        notes.push((
            "Not checked by CrazyAgent. Any test results in it are reported by the agent."
                .to_owned(),
            look.muted,
        ));
    }
    match set.last_outcome {
        TurnOutcome::Failed => notes.push((
            "The agent's last turn failed. These are the changes it made before it stopped."
                .to_owned(),
            look.warning,
        )),
        TurnOutcome::Cancelled => notes.push((
            "The agent's last turn was stopped. These are the changes it made before it stopped."
                .to_owned(),
            look.warning,
        )),
        TurnOutcome::Succeeded => {}
    }
    match set.state {
        ChangeState::Conflict if !set.conflicts.is_empty() => {
            notes.push((conflict_line(&set.conflicts), look.warning))
        }
        ChangeState::Updating if set.waiting => notes.push((WAITING_NOTE.to_owned(), look.warning)),
        ChangeState::Updating => notes.push((
            "The agent is resolving the conflicts with your branch. Review again when it finishes."
                .to_owned(),
            look.fg,
        )),
        ChangeState::Accepted if set.outside => notes.push((
            "Your branch already has these changes. They were landed outside the app.".to_owned(),
            look.fg,
        )),
        ChangeState::Discarded if set.outside => notes.push((
            "The agent's copy of these changes was removed outside the app, so they cannot be restored."
                .to_owned(),
            look.fg,
        )),
        ChangeState::Discarded => notes.push((restore_note(set, now_ms), look.fg)),
        _ => {}
    }
    let mut rows = vec![Row::of(agent_name(set), look.heading)];
    for (note, style) in notes {
        rows.extend(
            wrap_words(&note, content)
                .into_iter()
                .map(|line| Row::of(line, style)),
        );
    }
    rows
}

fn restore_note(set: &ChangeSetView, now_ms: i64) -> String {
    match set
        .restorable_until_ms
        .and_then(|until| i64::try_from(until).ok())
    {
        Some(until) if until > now_ms => format!(
            "You can restore these changes for the next {}.",
            duration(until - now_ms)
        ),
        _ => "These changes can no longer be restored.".to_owned(),
    }
}

fn agent_name(set: &ChangeSetView) -> String {
    match set.agent.kind {
        AgentKind::Master => "Master".to_owned(),
        AgentKind::Sub => set.agent.label.clone(),
    }
}

/// The branch an Accept with no branch named lands on (DEC-111 Q1): the team's Land on
/// branch, else the checked-out one. Input: the listed branches. Output: the name, None
/// when neither is known or the Land on branch is missing.
fn landing_default(listed: &TargetBranches) -> Option<&String> {
    if listed.land_on_problem.is_some() {
        return None;
    }
    listed.land_on.as_ref().or(listed.current.as_ref())
}

/// The change set a Changes request is about. Input: the request. Output: its id, or None
/// for the list read, which is about the whole page.
fn about(query: &ChangesQuery) -> Option<&ChangeSetId> {
    match query {
        ChangesQuery::List { .. } | ChangesQuery::Waiting { .. } => None,
        ChangesQuery::Diff { id, .. }
        | ChangesQuery::Accept { id, .. }
        | ChangesQuery::AcceptInto { id, .. }
        | ChangesQuery::Branches { id }
        | ChangesQuery::Discard { id, .. }
        | ChangesQuery::Restore { id }
        | ChangesQuery::Update { id } => Some(id),
    }
}

fn state_word(set: &ChangeSetView) -> &'static str {
    match set.state {
        ChangeState::Pending => "Ready to review",
        ChangeState::Conflict => "Conflicts with your branch",
        ChangeState::Updating if set.waiting => "Updated, waiting for the agent",
        ChangeState::Updating if !set.conflicts.is_empty() => "Agent is fixing conflicts with main",
        ChangeState::Updating => "Agent is updating",
        ChangeState::Accepted if set.outside => "Landed outside the app",
        ChangeState::Accepted => "Accepted",
        ChangeState::Discarded if set.outside => "Removed outside the app",
        ChangeState::Discarded => "Discarded",
    }
}

fn state_row(set: &ChangeSetView, look: &Look) -> Row {
    let style = match set.state {
        ChangeState::Conflict => look.warning,
        ChangeState::Updating if set.waiting => look.warning,
        ChangeState::Accepted => look.success,
        ChangeState::Discarded => look.muted,
        ChangeState::Pending | ChangeState::Updating => look.fg,
    };
    Row::of(state_word(set), style)
}

fn plural(count: u32, one: &str, many: &str) -> String {
    format!("{count} {}", if count == 1 { one } else { many })
}

fn size_line(set: &ChangeSetView) -> String {
    format!(
        "{}, {} added, {} removed",
        plural(set.files, "file", "files"),
        plural(set.added, "line", "lines"),
        set.removed
    )
}

fn file_line(file: &ChangedFile) -> String {
    let word = match file.status {
        FileStatus::Added => "Added",
        FileStatus::Modified => "Modified",
        FileStatus::Deleted => "Deleted",
        FileStatus::Renamed => "Renamed",
    };
    match (&file.old_path, file.status) {
        (Some(old), FileStatus::Renamed) => format!("Renamed {old} to {}", file.path),
        _ => format!("{word} {}", file.path),
    }
}

/// The changed files the diff view names, with counts of those it leaves out.
struct ListedFiles<'a> {
    shown: Vec<&'a ChangedFile>,
    marked_hidden: usize,
    plain_hidden: usize,
}

/// Picks the files the diff view names: every file the engine marked comes first, so a risk
/// mark never hides behind plain files, then plain files, at most `FILES_SHOWN` in all.
/// Input: the diff's files. Output: the files to name and how many marked and plain files
/// are left out.
fn listed_files(files: &[ChangedFile]) -> ListedFiles<'_> {
    let (marked, plain): (Vec<&ChangedFile>, Vec<&ChangedFile>) =
        files.iter().partition(|file| !file_marks(file).is_empty());
    let marked_shown = marked.len().min(FILES_SHOWN);
    let plain_shown = plain.len().min(FILES_SHOWN - marked_shown);
    let listed = ListedFiles {
        shown: marked[..marked_shown]
            .iter()
            .chain(&plain[..plain_shown])
            .copied()
            .collect(),
        marked_hidden: marked.len() - marked_shown,
        plain_hidden: plain.len() - plain_shown,
    };
    assert!(listed.shown.len() <= FILES_SHOWN);
    assert_eq!(
        listed.shown.len() + listed.marked_hidden + listed.plain_hidden,
        files.len()
    );
    listed
}

fn file_marks(file: &ChangedFile) -> Vec<&'static str> {
    let kind = match file.kind {
        FileKind::Binary => Some("Binary file, no text diff"),
        FileKind::Symlink => Some("Symbolic link: points to another path"),
        FileKind::Gitlink => Some("Submodule commit"),
        FileKind::Text => None,
    };
    let mut marks: Vec<&'static str> = kind.into_iter().collect();
    for mark in &file.marks {
        let word = match mark {
            FileMark::Executable => "Can run as a program",
            FileMark::Symlink => "Symbolic link: points to another path",
            FileMark::Attributes => ".gitattributes: changes how Git treats files",
            FileMark::Modules => ".gitmodules: changes the project's submodules",
            FileMark::Mode => "File mode changed",
            FileMark::OwnerEdited => "Edited by you",
            FileMark::IdeConfig => "IDE settings: changes how the IDE behaves in this project",
            FileMark::Instructions => {
                "Instruction file: changes what every agent of this project is told"
            }
        };
        if !marks.contains(&word) {
            marks.push(word);
        }
    }
    marks
}

fn conflict_line(files: &[String]) -> String {
    let shown = files
        .iter()
        .take(CONFLICTS_SHOWN)
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(", ");
    let more = files.len().saturating_sub(CONFLICTS_SHOWN);
    let more = if more > 0 {
        format!(" and {more} more")
    } else {
        String::new()
    };
    format!(
        "These files also changed on your branch: {shown}{more}. Ask the agent to update, or discard."
    )
}

/// The diff header lines of the changed files that carry the instruction file mark, as the
/// engine's patch writes them, so the diff view shows those files' lines in full rather than
/// cut. A quoted header, which git writes for unusual names, counts when any file is marked.
/// Input: the files. Output: the headers.
fn instruction_headers(files: &[ChangedFile]) -> Vec<String> {
    files
        .iter()
        .filter(|file| file.marks.contains(&FileMark::Instructions))
        .map(|file| {
            let old = file.old_path.as_deref().unwrap_or(&file.path);
            format!("{DIFF_HEADER}a/{old} b/{}", file.path)
        })
        .collect()
}

fn diff_line(line: &str) -> String {
    line.replace('\t', TAB_SPACES)
}

fn diff_style(line: &str, look: &Look) -> Style {
    if line.starts_with("+++") || line.starts_with("---") {
        look.muted
    } else if line.starts_with('+') {
        look.success
    } else if line.starts_with('-') {
        look.error
    } else if line.starts_with("@@") {
        look.accent
    } else {
        look.fg
    }
}

fn done(view: &ChangesView) -> Notice {
    match view {
        ChangesView::Accepted(AcceptOutcome::Landed { commit, branch }) => Notice::plain(format!(
            "Accepted as {} on {branch}.",
            commit.get(..SHORT_COMMIT).unwrap_or(commit)
        )),
        ChangesView::Accepted(AcceptOutcome::Conflict { files }) => {
            Notice::problem(conflict_line(files))
        }
        ChangesView::Discarded { .. } => {
            Notice::plain("Discarded. You can restore these changes for 7 days.")
        }
        ChangesView::Restored => Notice::plain("Restored. The changes are ready to review again."),
        ChangesView::Updating => Notice::plain(UPDATED_NOTICE),
        ChangesView::List(_)
        | ChangesView::Diff(_)
        | ChangesView::Waiting(_)
        | ChangesView::Branches(_) => Notice::plain(""),
    }
}

/// One plain sentence for a refused Changes request.
/// Takes the bounded refusal code.
/// Returns the sentence, or the shared plain sentence for any other code.
pub(in crate::tui) fn refusal(code: &str) -> String {
    match code {
        "changes_owner_dirty" => {
            "Your project folder has uncommitted changes to tracked files. Commit or stash them, then accept again."
        }
        "changes_detached_head" => {
            "Your project folder isn't on a branch. Check out the branch these changes should land on, then accept again."
        }
        "changes_git_busy" => {
            "Git is in the middle of a merge, rebase, cherry-pick or bisect in your project folder. Finish or stop it, then accept again."
        }
        "changes_conflict" => {
            "These changes conflict with your branch. Ask the agent to update, or discard."
        }
        "changes_stale" => {
            "These changes moved on since you opened them. Review them again, then accept."
        }
        "changes_branch_moved" => {
            "Your branch moved while these changes were landing, so nothing changed. Review and accept again."
        }
        "changes_branch_not_above_base" => {
            "The agent's branch no longer holds the commit these changes start from. Ask the agent to update, or discard."
        }
        "changes_git_unsupported" => {
            "The Git on this computer can't land changes. Update Git, then try again."
        }
        "changes_restore_expired" => {
            "These changes can no longer be restored. Discarded changes are kept for 7 days."
        }
        "changes_agent_has_pending" => {
            "This agent has newer changes waiting. Accept or discard those first."
        }
        "changes_agent_busy" => "The agent is working right now. Try again when it finishes.",
        "changes_not_found" => "These changes are gone. The list is up to date now.",
        "changes_message_invalid" => {
            "The commit message is empty, too long or has characters Git can't keep. Edit it, then accept again."
        }
        "changes_owner_identity_missing" => {
            "Git doesn't know your name and email yet. Set user.name and user.email, then accept again."
        }
        "changes_embedded_repository" => {
            "These changes add a folder Git would treat as a repository of its own inside the project, such as a submodule. Ask the agent to remove it."
        }
        "changes_agent_branch_invalid" => {
            "The agent's branch couldn't be read from its own copy of the project. Ask the agent to check its branch, or discard."
        }
        "changes_agent_branch_exists" => {
            "A branch with this agent's name is already in your project. Rename or delete it, then start the agent again."
        }
        "changes_target_invalid" => {
            "That branch isn't a local branch of your project any more. Pick another branch, then accept again."
        }
        "changes_target_checked_out" => {
            "That branch is checked out in another working copy CrazyAgent doesn't manage. Pick another branch, or check it out in your project folder."
        }
        "changes_master_tools_off" => {
            "This master has tools off, so it can't edit files to update these changes. Turn tools on for the master, or discard."
        }
        "changes_master_closed" => {
            "The master conversation these changes came from is closed, so it can't update them. Discard them, or start a new conversation."
        }
        "changes_master_not_started" => {
            "Your branch is merged into the master's copy, but the master couldn't start resolving it yet. It starts after the master's next turn ends."
        }
        "team_agent_not_found" => {
            "The agent that made these changes is no longer on the team, so it can't update them. Discard them instead."
        }
        "team_conversation_full" => {
            "Too many of your messages are waiting for the master. Send more once it has answered."
        }
        "team_task_invalid" => "The task is empty or too long.",
        "changes_unavailable" => return format!("{NOT_READY} {NOT_READY_LINE}"),
        other => return plain_problem(other),
    }
    .to_owned()
}

#[cfg(all(test, unix))]
mod tests;
