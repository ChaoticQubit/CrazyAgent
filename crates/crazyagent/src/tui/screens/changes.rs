use super::intake::NOT_READY;
use super::new_project_view::{Look, Row, columns, list_row, spread, wrap_words};
use super::project::plain_problem;
use super::project_team::{Help, Notice, TABS, window};
use super::project_time::duration;
use crate::landing::{
    AcceptOutcome, AgentKind, ChangeSetId, ChangeSetView, ChangeState, ChangedFile, FileKind,
    FileMark, FileStatus, TurnOutcome,
};
use crate::protocol::ProjectId;
use crate::tui::client::changes::{ChangesEvent, ChangesQuery, ChangesView};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::Style;

const LIST_POLL_MS: i64 = 5_000;
const DIFF_LINES_MAX: usize = 20_000;
const DIFF_AHEAD: usize = 40;
const DIFF_PAGE_ROWS: usize = 20;
const LIST_ROWS_MAX: usize = 8;
const FILES_SHOWN: usize = 10;
const CONFLICTS_SHOWN: usize = 5;
const SHORT_COMMIT: usize = 7;
const TAB_SPACES: &str = "    ";
const AGENT_WIDTH: usize = 24;
const STATE_WIDTH: usize = 26;
const NOT_READY_LINE: &str = "Landing changes isn't ready in this engine yet.";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ask {
    Accept,
    Discard,
}

struct Diff {
    id: ChangeSetId,
    head: String,
    files: Vec<ChangedFile>,
    lines: Vec<String>,
    next: Option<u16>,
    truncated: bool,
    top: usize,
}

impl Diff {
    fn wants_more(&self) -> bool {
        self.next.is_some() && self.top + DIFF_AHEAD >= self.lines.len()
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
    diff: Option<Diff>,
    asking: Option<Ask>,
    notice: Option<Notice>,
    queued: Option<ChangesQuery>,
    sent: Option<ChangesQuery>,
    stale: bool,
    read_ms: i64,
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
            diff: None,
            asking: None,
            notice: None,
            queued: None,
            sent: None,
            stale: true,
            read_ms: 0,
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
        } else if let Some(diff) = self.diff.as_ref().filter(|diff| diff.wants_more()) {
            ChangesQuery::Diff {
                id: diff.id.clone(),
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
        if !matches!(lost, ChangesQuery::List { .. } | ChangesQuery::Diff { .. }) {
            self.notice = Some(Notice::problem(
                "The engine connection dropped before it answered. Check the list before trying again.",
            ));
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
            return;
        }
        match (&event.query, &event.result) {
            (ChangesQuery::List { .. }, Ok(ChangesView::List(sets))) => {
                self.sets = sets.clone();
                self.loaded = true;
                self.not_ready = false;
                if let Some(diff) = &self.diff
                    && !self.sets.iter().any(|set| set.id == diff.id)
                {
                    self.diff = None;
                }
            }
            (ChangesQuery::List { .. }, Err(code)) => {
                self.notice = Some(Notice::problem(format!(
                    "Couldn't load changes. {}",
                    refusal(code)
                )));
            }
            (ChangesQuery::Diff { id, page }, Ok(ChangesView::Diff(page_diff))) => {
                if let Some(diff) = self.diff.as_mut().filter(|diff| &diff.id == id)
                    && diff.next == Some(*page)
                {
                    diff.files.extend(page_diff.files.iter().cloned());
                    diff.lines.extend(page_diff.text.lines().map(diff_line));
                    diff.truncated |= page_diff.truncated;
                    diff.next = page_diff.next;
                    if diff.lines.len() > DIFF_LINES_MAX {
                        diff.lines.truncate(DIFF_LINES_MAX);
                        diff.truncated = true;
                        diff.next = None;
                    }
                    assert!(diff.lines.len() <= DIFF_LINES_MAX);
                }
            }
            (ChangesQuery::Diff { .. }, Err(code)) => {
                if let Some(diff) = self.diff.as_mut() {
                    diff.next = None;
                }
                self.notice = Some(Notice::problem(format!(
                    "Couldn't load the changes. {}",
                    refusal(code)
                )));
            }
            (_, Ok(view)) => {
                self.stale = true;
                self.notice = Some(done(view));
                if !matches!(view, ChangesView::Accepted(AcceptOutcome::Conflict { .. })) {
                    self.diff = None;
                }
            }
            (_, Err(code)) => {
                self.stale = true;
                self.notice = Some(Notice::problem(refusal(code)));
            }
        }
        self.clamp();
    }

    fn clamp(&mut self) {
        self.cursor = self.cursor.min(self.sets.len().saturating_sub(1));
        if self.chosen().is_none() {
            self.asking = None;
        }
        assert!(self.sets.is_empty() || self.cursor < self.sets.len());
    }

    fn chosen(&self) -> Option<&ChangeSetView> {
        match &self.diff {
            Some(diff) => self.sets.iter().find(|set| set.id == diff.id),
            None => self.sets.get(self.cursor),
        }
    }

    fn reviewed_head(&self) -> Option<String> {
        let set = self.chosen()?;
        Some(match &self.diff {
            Some(diff) => diff.head.clone(),
            None => set.head.clone(),
        })
    }

    /// Handles one key: move through the change sets, open a diff and scroll it,
    /// accept or discard after a yes, ask the agent to update, or restore.
    /// Takes the key event. Returns true when the view used the key; other keys,
    /// including Esc on the list, are left to the host.
    pub fn key(&mut self, key: KeyEvent, now_ms: i64) -> bool {
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
        let actions = self.chosen().map(|set| Actions::of(set, now_ms));
        let used = match (key.code, actions) {
            (KeyCode::Char('a'), Some(actions)) if actions.accept => {
                self.asking = Some(Ask::Accept);
                true
            }
            (KeyCode::Char('d'), Some(actions)) if actions.discard => {
                self.asking = Some(Ask::Discard);
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
            _ if self.diff.is_some() => self.diff_key(key.code),
            _ => self.list_key(key.code),
        };
        self.clamp();
        used
    }

    fn list_key(&mut self, code: KeyCode) -> bool {
        match code {
            KeyCode::Up => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Down => self.cursor = self.cursor.saturating_add(1),
            KeyCode::Enter => {
                let Some(set) = self.sets.get(self.cursor) else {
                    return true;
                };
                self.diff = Some(Diff {
                    id: set.id.clone(),
                    head: set.head.clone(),
                    files: Vec::new(),
                    lines: Vec::new(),
                    next: Some(0),
                    truncated: false,
                    top: 0,
                });
            }
            _ => return false,
        }
        true
    }

    fn diff_key(&mut self, code: KeyCode) -> bool {
        let Some(diff) = self.diff.as_mut() else {
            return false;
        };
        let last = diff.lines.len().saturating_sub(1);
        match code {
            KeyCode::Up => diff.top = diff.top.saturating_sub(1),
            KeyCode::Down => diff.top = (diff.top + 1).min(last),
            KeyCode::PageUp => diff.top = diff.top.saturating_sub(DIFF_PAGE_ROWS),
            KeyCode::PageDown | KeyCode::Char(' ') => {
                diff.top = (diff.top + DIFF_PAGE_ROWS).min(last)
            }
            KeyCode::Esc => self.diff = None,
            _ => return false,
        }
        true
    }

    fn queue(&mut self, query: ChangesQuery) {
        if self.queued.is_some()
            || self.sent.as_ref().is_some_and(|sent| {
                !matches!(sent, ChangesQuery::List { .. } | ChangesQuery::Diff { .. })
            })
        {
            self.notice = Some(Notice::problem(
                "Still working on the last change. Try again in a moment.",
            ));
            return;
        }
        self.notice = None;
        self.queued = Some(query);
    }

    fn act(&mut self, ask: Ask) {
        let (Some(set), Some(expected_head)) = (self.chosen(), self.reviewed_head()) else {
            return;
        };
        let id = set.id.clone();
        self.queue(match ask {
            Ask::Accept => ChangesQuery::Accept { id, expected_head },
            Ask::Discard => ChangesQuery::Discard { id, expected_head },
        });
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
    /// Returns the rows and the help keys.
    pub fn rows(&self, content: usize, room: usize, now_ms: i64, look: &Look) -> (Vec<Row>, Help) {
        let mut lines = Vec::new();
        if self.not_ready {
            lines.push(Row::of("Changes", look.heading));
            lines.push(Row::of(NOT_READY, look.fg));
            lines.push(Row::of(NOT_READY_LINE, look.muted));
            return (lines, vec![TABS, ("Esc", "Home")]);
        }
        if !self.loaded {
            lines.push(Row::of("Changes", look.heading));
            lines.push(Row::of("Loading changes...", look.muted));
            lines.extend(self.notice.iter().map(|notice| notice.row(look)));
            return (lines, vec![TABS, ("Esc", "Home")]);
        }
        let footer = self.footer(content, look);
        if let Some(diff) = &self.diff {
            let body = room.saturating_sub(footer.len());
            lines.extend(self.diff_rows(diff, content, body, now_ms, look));
        } else {
            lines.extend(self.list_rows(content, room.saturating_sub(footer.len()), now_ms, look));
        }
        lines.extend(footer);
        (lines, self.help(now_ms))
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
                index == self.cursor,
                true,
                look,
            ));
        }
        lines.push(Row::default());
        if let Some(set) = self.sets.get(self.cursor) {
            lines.extend(details(set, content, now_ms, look));
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
        for file in diff.files.iter().take(FILES_SHOWN) {
            lines.push(Row::of(format!("  {}", file_line(file)), look.fg));
            for mark in file_marks(file) {
                lines.push(Row::of(format!("    ! {mark}"), look.warning));
            }
        }
        if diff.files.len() > FILES_SHOWN {
            lines.push(Row::of(
                format!("  and {} more files", diff.files.len() - FILES_SHOWN),
                look.muted,
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
            let waiting = if diff.next.is_some() {
                "Loading changes..."
            } else {
                "No text changes to show."
            };
            lines.push(Row::of(waiting, look.muted));
            return lines;
        }
        let text_rows = room.saturating_sub(lines.len()).max(1);
        for line in diff.lines.iter().skip(diff.top).take(text_rows) {
            lines.push(Row::of(line.clone(), diff_style(line, look)).pad(content));
        }
        lines.truncate(room.max(1));
        lines
    }

    fn footer(&self, content: usize, look: &Look) -> Vec<Row> {
        let Some(ask) = self.asking else {
            return self.notice.iter().map(|notice| notice.row(look)).collect();
        };
        let (question, yes, explain) = match ask {
            Ask::Accept => (
                "Accept these changes?",
                " Accept   ",
                "They land as one commit on the branch checked out in your project folder, under your own Git name. CrazyAgent did not check them.",
            ),
            Ask::Discard => (
                "Discard these changes?",
                " Discard   ",
                "The agent's working copy goes back to your branch. You can restore these changes for 7 days.",
            ),
        };
        let mut rows = vec![
            Row::of(question, look.warning)
                .push("   y", look.heading)
                .push(yes, look.fg)
                .push("n", look.heading)
                .push(" Keep reviewing", look.fg),
        ];
        rows.extend(
            wrap_words(explain, content)
                .into_iter()
                .map(|line| Row::of(line, look.muted)),
        );
        rows
    }

    fn help(&self, now_ms: i64) -> Help {
        match self.asking {
            Some(Ask::Accept) => return vec![("y", "Accept"), ("n", "Keep reviewing")],
            Some(Ask::Discard) => return vec![("y", "Discard"), ("n", "Keep reviewing")],
            None => {}
        }
        let mut help = if self.diff.is_some() {
            vec![("\u{2191}\u{2193}", "Scroll")]
        } else {
            vec![("\u{2191}\u{2193}", "Move"), ("Enter", "Show changes")]
        };
        if let Some(actions) = self.chosen().map(|set| Actions::of(set, now_ms)) {
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
        if self.diff.is_some() {
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
            discard: open,
            update: set.state == ChangeState::Conflict,
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
        ChangeState::Updating => notes.push((
            "The agent is resolving the conflicts with your branch. Review again when it finishes."
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
        AgentKind::Sub => format!("Sub-agent {}", set.agent.label),
    }
}

fn state_word(state: ChangeState) -> &'static str {
    match state {
        ChangeState::Pending => "Ready to review",
        ChangeState::Conflict => "Conflicts with your branch",
        ChangeState::Updating => "Agent is updating",
        ChangeState::Accepted => "Accepted",
        ChangeState::Discarded => "Discarded",
    }
}

fn state_row(set: &ChangeSetView, look: &Look) -> Row {
    let style = match set.state {
        ChangeState::Conflict => look.warning,
        ChangeState::Accepted => look.success,
        ChangeState::Discarded => look.muted,
        ChangeState::Pending | ChangeState::Updating => look.fg,
    };
    Row::of(state_word(set.state), style)
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
        ChangesView::Updating => {
            Notice::plain("The agent is updating its changes onto your branch.")
        }
        ChangesView::List(_) | ChangesView::Diff(_) => Notice::plain(""),
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
        "changes_owner_identity_missing" => {
            "Git doesn't know your name and email yet. Set user.name and user.email, then accept again."
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
        "changes_unavailable" => return format!("{NOT_READY} {NOT_READY_LINE}"),
        other => return plain_problem(other),
    }
    .to_owned()
}

#[cfg(all(test, unix))]
mod tests;
