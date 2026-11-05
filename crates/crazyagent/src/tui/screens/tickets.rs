use super::new_project_input::{TextInput, cells};
use super::new_project_view::{Look, Row, wrap_words};
use super::project::plain_problem;
use super::project_team::{Help, Notice, TABS};
use crate::protocol::ProjectId;
use crate::tui::client::settings::SettingsEvent;
use crate::tui::client::tracker::{STEPS_PER_JOB_MAX, Step, TrackerEvent, TrackerJob};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use trackwright_cli::views::{Key as ViewKey, Page, Theme, Views};
use trackwright_core::model::ProjectKey;

#[cfg(all(test, unix))]
mod tests;

const STEPS_QUEUED_MAX: usize = 32;
const KEY_BYTES_MAX: usize = 8;
const FIELD_LEFT: usize = 6;
const FIELD_CELLS: usize = 10;

/// What the Tickets tab did with a key: nothing, something, or the owner asked for the
/// project's ticket settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::tui) enum TicketsReply {
    Ignored,
    Handled,
    Settings,
}

enum State {
    Unread,
    Failed(String),
    Off,
    On {
        key: ProjectKey,
        views: Arc<Mutex<Views>>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Sent {
    Projects,
    Attach,
    Views,
}

/// One project's Tickets tab: trackwright's own list, ready, board and roadmap pages with a
/// ticket's detail over them, reached through the owner protocol. The views go to the owner
/// worker with the keys that need the tracker and come back drawn; while they are away the
/// tab shows the rows it drew last. A project without tickets offers to turn them on under
/// a key the owner picks. Sends at most one request at a time.
pub(in crate::tui) struct ProjectTickets {
    pub project: ProjectId,
    name: String,
    state: State,
    page: Page,
    asked: bool,
    sent: Option<Sent>,
    steps: VecDeque<Step>,
    turning_on: Option<(TextInput, Option<String>)>,
    attach: Option<ProjectKey>,
    rechecking: Option<(String, String)>,
    shot: Vec<Row>,
    notice: Option<Notice>,
    detail: bool,
}

impl ProjectTickets {
    /// Opens one project's Tickets tab with nothing read yet. Inputs: the project and its
    /// name.
    pub(in crate::tui) fn new(project: ProjectId, name: String) -> Self {
        Self {
            project,
            name,
            state: State::Unread,
            page: Page::List,
            asked: false,
            sent: None,
            steps: VecDeque::new(),
            turning_on: None,
            attach: None,
            rechecking: None,
            shot: Vec::new(),
            notice: None,
            detail: false,
        }
    }

    /// Keeps the project name the tab shows in step with the project screen. Input: the
    /// name.
    pub(in crate::tui) fn rename(&mut self, name: &str) {
        name.clone_into(&mut self.name);
    }

    /// The next request to send: the tracked projects first, then a turn-on, then the
    /// views with up to `STEPS_PER_JOB_MAX` queued steps. Output: the job, or none while
    /// one is in flight or nothing waits.
    pub(in crate::tui) fn next_job(&mut self) -> Option<TrackerJob> {
        if self.sent.is_some() {
            return None;
        }
        if matches!(self.state, State::Unread) && !self.asked {
            self.asked = true;
            self.sent = Some(Sent::Projects);
            return Some(TrackerJob::Projects);
        }
        if let Some(key) = self.attach.take() {
            self.sent = Some(Sent::Attach);
            return Some(TrackerJob::Attach {
                project: self.project.clone(),
                key,
                name: self.name.clone(),
            });
        }
        let State::On { views, .. } = &self.state else {
            return None;
        };
        if self.steps.is_empty() {
            return None;
        }
        let count = self.steps.len().min(STEPS_PER_JOB_MAX);
        let steps: Vec<Step> = self.steps.drain(..count).collect();
        self.sent = Some(Sent::Views);
        Some(TrackerJob::Views {
            project: self.project.clone(),
            views: Arc::clone(views),
            steps,
        })
    }

    /// Forgets the request in flight after the engine connection dropped: the projects
    /// are asked again, a turn-on that may have taken effect is settled by reading them,
    /// and lent views stay with the worker's last state.
    pub(in crate::tui) fn forget_reads(&mut self) {
        match self.sent.take() {
            Some(Sent::Projects) => self.asked = false,
            Some(Sent::Attach) => self.recheck("tracker_outcome_unknown"),
            Some(Sent::Views) | None => {}
        }
    }

    /// Whether a request is in flight or steps or a turn-on wait to be sent.
    pub(in crate::tui) fn busy(&self) -> bool {
        self.sent.is_some() || !self.steps.is_empty() || self.attach.is_some()
    }

    /// Whether the owner is typing a key to turn tickets on.
    pub(in crate::tui) fn editing(&self) -> bool {
        self.turning_on.is_some()
    }

    /// Takes the engine's answer to the request in flight; any other reply is left alone.
    /// Input: the Settings event.
    pub(in crate::tui) fn apply(&mut self, event: &SettingsEvent) {
        let SettingsEvent::Tracker(event) = event else {
            return;
        };
        match (event.as_ref(), self.sent) {
            (TrackerEvent::Projects(result), Some(Sent::Projects)) => {
                self.sent = None;
                match result {
                    Ok(projects) => {
                        let tracked = projects
                            .iter()
                            .find(|tracked| tracked.project == self.project)
                            .map(|tracked| tracked.key.clone());
                        match (tracked, self.rechecking.take()) {
                            (Some(key), Some(_)) => self.turned_on(key),
                            (Some(key), None) => self.turn_on(key),
                            (None, Some((code, typed))) if code == "project_exists" => {
                                self.state = State::Off;
                                self.open_field(&typed, Some(sentence(&code)));
                            }
                            (None, Some(_)) => {
                                self.state = State::Off;
                                self.notice = Some(Notice::problem(
                                    "Tickets did not turn on. Press t to try again.",
                                ));
                            }
                            (None, None) => self.state = State::Off,
                        }
                    }
                    Err(code) => self.state = State::Failed(sentence(code)),
                }
            }
            (TrackerEvent::Attached { project, result }, Some(Sent::Attach))
                if *project == self.project =>
            {
                self.sent = None;
                match result {
                    Ok(tracked) => self.turned_on(tracked.key.clone()),
                    Err(code) if code == "tracker_outcome_unknown" || code == "project_exists" => {
                        self.recheck(code);
                    }
                    Err(code) => match self.turning_on.as_mut() {
                        Some((_, error)) => *error = Some(sentence(code)),
                        None => self.notice = Some(Notice::problem(sentence(code))),
                    },
                }
            }
            (TrackerEvent::Views { project, problem }, Some(Sent::Views))
                if *project == self.project =>
            {
                self.sent = None;
                if let Some(code) = problem {
                    if code == "tracker_views_failed"
                        && let State::On { key, .. } = &self.state
                    {
                        let key = key.clone();
                        self.turn_on(key);
                    }
                    self.notice = Some(Notice::problem(sentence(code)));
                }
            }
            _ => {}
        }
    }

    /// Closes the key field after a turn-on only the tracked projects can settle: one whose
    /// outcome is unknown, or one refused because a project already has tickets, which may
    /// be this one. The projects are asked again. Input: the code the turn-on ended with.
    fn recheck(&mut self, code: &str) {
        assert!(
            matches!(self.state, State::Off),
            "a turn-on starts from Off"
        );
        assert!(self.attach.is_none(), "one turn-on at a time");
        let typed = self
            .turning_on
            .take()
            .map(|(input, _)| input.value().to_owned())
            .unwrap_or_default();
        self.state = State::Unread;
        self.asked = false;
        self.notice = None;
        self.rechecking = Some((code.to_owned(), typed));
    }

    /// Shows the tickets after a turn-on took effect and says under which key. Input: the
    /// key.
    fn turned_on(&mut self, key: ProjectKey) {
        self.turning_on = None;
        self.notice = Some(Notice::plain(format!(
            "Tickets are on. Ticket ids start with {}.",
            key.as_str()
        )));
        self.turn_on(key);
    }

    /// Opens the key field. Inputs: the text it starts with and the problem it shows, if any.
    fn open_field(&mut self, text: &str, error: Option<String>) {
        let mut input = TextInput::single(KEY_BYTES_MAX);
        input.set(text);
        self.turning_on = Some((input, error));
    }

    fn turn_on(&mut self, key: ProjectKey) {
        self.state = State::On {
            views: Arc::new(Mutex::new(Views::new(key.clone()))),
            key,
        };
        self.steps.clear();
        self.steps.push_back(Step::Open(self.page));
        self.detail = false;
    }

    fn queue(&mut self, step: Step) {
        if self.steps.len() < STEPS_QUEUED_MAX {
            self.steps.push_back(step);
        }
    }

    /// Takes pasted text into the key field. Input: the text. Output: true when the field
    /// took it.
    pub(in crate::tui) fn paste(&mut self, text: &str) -> bool {
        let Some((input, error)) = self.turning_on.as_mut() else {
            return false;
        };
        let letters: String = text
            .chars()
            .filter(char::is_ascii_alphabetic)
            .map(|letter| letter.to_ascii_uppercase())
            .collect();
        input.insert_str(&letters);
        *error = None;
        true
    }

    /// Acts on one key. Input: the key. Output: what the tab did; `Ignored` leaves it to the
    /// project screen (tabs, Esc to Home).
    pub(in crate::tui) fn key(&mut self, key: KeyEvent) -> TicketsReply {
        let shifted_arrow = key.modifiers.contains(KeyModifiers::SHIFT)
            && matches!(key.code, KeyCode::Left | KeyCode::Right);
        if shifted_arrow
            || key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        {
            return TicketsReply::Ignored;
        }
        if self.turning_on.is_some() {
            return self.field_key(key);
        }
        self.notice = None;
        match &self.state {
            State::Unread => TicketsReply::Ignored,
            State::Failed(_) => match key.code {
                KeyCode::Char('r') => {
                    self.state = State::Unread;
                    self.asked = false;
                    TicketsReply::Handled
                }
                _ => TicketsReply::Ignored,
            },
            State::Off => match key.code {
                KeyCode::Char('t') => {
                    self.open_field(&suggested_key(&self.name), None);
                    TicketsReply::Handled
                }
                KeyCode::Char('r') => {
                    self.state = State::Unread;
                    self.asked = false;
                    TicketsReply::Handled
                }
                _ => TicketsReply::Ignored,
            },
            State::On { views, .. } => {
                let views = Arc::clone(views);
                self.views_key(key, &views)
            }
        }
    }

    fn views_key(&mut self, key: KeyEvent, views: &Mutex<Views>) -> TicketsReply {
        let step = match key.code {
            KeyCode::Char('s') => return TicketsReply::Settings,
            KeyCode::Tab | KeyCode::BackTab => {
                let forward = key.code == KeyCode::Tab;
                self.page = next_page(self.page, forward);
                Step::Open(self.page)
            }
            KeyCode::Esc => {
                if self.sent.is_some() || !self.steps.is_empty() {
                    if !self.detail {
                        return TicketsReply::Ignored;
                    }
                    self.queue(Step::Key(ViewKey::Back));
                    return TicketsReply::Handled;
                }
                let Ok(mut views) = views.try_lock() else {
                    return TicketsReply::Handled;
                };
                return if views.key(ViewKey::Back, &mut Unreached) {
                    self.detail = false;
                    TicketsReply::Handled
                } else {
                    TicketsReply::Ignored
                };
            }
            KeyCode::Up => Step::Key(ViewKey::Up),
            KeyCode::Down => Step::Key(ViewKey::Down),
            KeyCode::Left => Step::Key(ViewKey::Left),
            KeyCode::Right => Step::Key(ViewKey::Right),
            KeyCode::PageUp => Step::Key(ViewKey::PageUp),
            KeyCode::PageDown => Step::Key(ViewKey::PageDown),
            KeyCode::Enter => Step::Key(ViewKey::Enter),
            KeyCode::Char(character @ ('r' | '<' | '>')) => Step::Key(ViewKey::Char(character)),
            _ => return TicketsReply::Ignored,
        };
        self.queue(step);
        TicketsReply::Handled
    }

    fn field_key(&mut self, key: KeyEvent) -> TicketsReply {
        let Some((input, error)) = self.turning_on.as_mut() else {
            return TicketsReply::Ignored;
        };
        match key.code {
            KeyCode::Esc => self.turning_on = None,
            KeyCode::Enter => match ProjectKey::new(input.value()) {
                Ok(key) if self.sent.is_none() && self.attach.is_none() => self.attach = Some(key),
                Ok(_) => {}
                Err(_) => *error = Some(sentence("project_key_invalid")),
            },
            KeyCode::Char(character) => {
                if character.is_ascii_alphabetic() {
                    input.insert_str(&character.to_ascii_uppercase().to_string());
                    *error = None;
                }
            }
            _ => {
                if input.handle(key) {
                    *error = None;
                }
            }
        }
        TicketsReply::Handled
    }

    /// Draws the tab's rows. Inputs: the content width, the rows it may fill and the look.
    /// Output: the rows, the cursor in the key field as (column, row), and the help keys.
    pub(in crate::tui) fn rows(
        &mut self,
        content: usize,
        room: usize,
        look: &Look,
    ) -> (Vec<Row>, Option<(usize, usize)>, Help) {
        let mut lines = vec![Row::of("Tickets", look.heading)];
        match &self.state {
            State::Unread => {
                lines.push(Row::of("Loading tickets...", look.muted));
                lines.extend(self.notice.iter().map(|notice| notice.row(look)));
                (lines, None, vec![TABS, ("Esc", "Home")])
            }
            State::Failed(problem) => {
                lines.extend(
                    wrap_words(&format!("Could not read the tickets. {problem}"), content)
                        .into_iter()
                        .map(|line| Row::of(line, look.error)),
                );
                lines.push(Row::of("Press r to try again.", look.muted));
                (lines, None, vec![("r", "Try again"), TABS, ("Esc", "Home")])
            }
            State::Off => self.off_rows(lines, content, look),
            State::On { views, .. } => {
                let views = Arc::clone(views);
                self.on_rows(&views, content, room, look)
            }
        }
    }

    fn off_rows(
        &self,
        mut lines: Vec<Row>,
        content: usize,
        look: &Look,
    ) -> (Vec<Row>, Option<(usize, usize)>, Help) {
        lines.extend(
            wrap_words(
                &format!(
                    "{} has no tickets yet. Turn them on to plan the work as tickets, cycles and a roadmap that you and the agents share.",
                    self.name
                ),
                content,
            )
            .into_iter()
            .map(|line| Row::of(line, look.muted)),
        );
        let Some((input, error)) = &self.turning_on else {
            lines.extend(self.notice.iter().map(|notice| notice.row(look)));
            return (
                lines,
                None,
                vec![("t", "Turn on tickets"), TABS, ("Esc", "Home")],
            );
        };
        lines.push(Row::default());
        let row = lines.len();
        lines.push(
            Row::of("Key", look.heading)
                .pad(FIELD_LEFT - 2)
                .push("[ ", look.accent)
                .cat(Row::of(input.value(), look.fg).pad(FIELD_CELLS))
                .push(" ]", look.accent),
        );
        lines.push(Row::of(
            "2 to 8 letters A to Z. Ticket ids start with it, like FM-1.",
            look.muted,
        ));
        if let Some(error) = error {
            lines.push(Row::of(error.clone(), look.error));
        }
        let column = FIELD_LEFT + cells(input.head());
        (
            lines,
            Some((column, row)),
            vec![("Enter", "Turn on"), ("Esc", "Cancel")],
        )
    }

    fn on_rows(
        &mut self,
        views: &Mutex<Views>,
        content: usize,
        room: usize,
        look: &Look,
    ) -> (Vec<Row>, Option<(usize, usize)>, Help) {
        let mut lines = vec![page_row(self.page, look)];
        let mut help: Help = vec![("Tab", "Page"), ("s", "Settings"), TABS, ("Esc", "Home")];
        let height = room.saturating_sub(lines.len());
        let away = self.sent == Some(Sent::Views);
        match views.try_lock() {
            Ok(views) if !away => {
                self.shot = drawn(&views, content, height, look);
                let mut shown: Help = views
                    .hints()
                    .iter()
                    .map(|hint| (hint.key, hint.label))
                    .collect();
                self.detail = shown.iter().any(|(key, _)| *key == "Esc");
                if self.detail {
                    help.retain(|(key, _)| *key != "Esc");
                }
                shown.append(&mut help);
                help = shown;
            }
            _ => help.insert(0, ("", "Working...")),
        }
        lines.extend(self.shot.iter().cloned());
        if let Some(notice) = &self.notice
            && lines.len() > 1
        {
            let last = lines.len() - 1;
            lines[last] = notice.row(look);
        }
        (lines, None, help)
    }
}

/// The views' client for a key that must not reach the tracker: Back closes a detail or
/// leaves the page without a call. Any call it gets is refused.
struct Unreached;

impl trackwright_cli::views::Client for Unreached {
    fn call(
        &mut self,
        _op: trackwright_core::model::OpKind,
        _input: serde_json::Value,
    ) -> serde_json::Value {
        crate::tui::client::tracker::refused("tracker_busy")
    }
}

/// The page after or before one, wrapping. Inputs: the page and the direction. Output: the
/// page.
fn next_page(page: Page, forward: bool) -> Page {
    let count = Page::ALL.len();
    let index = Page::ALL
        .iter()
        .position(|candidate| *candidate == page)
        .expect("every page is listed");
    Page::ALL[if forward {
        (index + 1) % count
    } else {
        (index + count - 1) % count
    }]
}

/// The row naming the tab's pages, the shown one in brackets. Inputs: the page and the
/// look. Output: the row.
fn page_row(page: Page, look: &Look) -> Row {
    let mut row = Row::default();
    for (index, candidate) in Page::ALL.iter().enumerate() {
        if index > 0 {
            row = row.push(" ", Style::new());
        }
        row = if *candidate == page {
            row.push(format!("[{}]", candidate.label()), look.accent)
        } else {
            row.push(format!(" {} ", candidate.label()), look.fg)
        };
    }
    row
}

/// The tracker views' styles in the CrazyAgent look. Input: the look. Output: the theme.
pub(in crate::tui) fn theme(look: &Look) -> Theme {
    Theme {
        text: look.fg,
        muted: look.muted,
        heading: look.heading,
        accent: look.accent,
        selected: look.selection,
        success: look.success,
        warning: look.warning,
        error: look.error,
    }
}

/// Draws the views into an area of the given size and reads it back as rows, one style
/// run per span; the cells a wide character covers are skipped. Inputs: the views, the
/// width, the height and the look. Output: one row per line of the area.
pub(in crate::tui) fn drawn(views: &Views, width: usize, height: usize, look: &Look) -> Vec<Row> {
    let area = Rect::new(
        0,
        0,
        u16::try_from(width).unwrap_or(u16::MAX),
        u16::try_from(height).unwrap_or(u16::MAX),
    );
    let mut buffer = Buffer::empty(area);
    views.render(area, &mut buffer, &theme(look));
    (0..area.height)
        .map(|y| {
            let mut row = Row::default();
            let mut run = String::new();
            let mut style: Option<Style> = None;
            let mut covered = 0;
            for x in 0..area.width {
                if covered > 0 {
                    covered -= 1;
                    continue;
                }
                let cell = &buffer[(x, y)];
                let symbol = cell.symbol();
                covered = cells(symbol).saturating_sub(1);
                let cell_style = cell.style();
                if style != Some(cell_style) {
                    if let Some(previous) = style {
                        row = row.push(std::mem::take(&mut run), previous);
                    }
                    style = Some(cell_style);
                }
                run.push_str(symbol);
            }
            if let Some(previous) = style {
                row = row.push(run, previous);
            }
            row
        })
        .collect()
}

/// A key suggested from a project name: the first letter of each word, A to Z, at most 8;
/// a one-word name gives its first letters. Input: the name. Output: a valid key.
pub(in crate::tui) fn suggested_key(name: &str) -> String {
    let letters: Vec<char> = name
        .split_whitespace()
        .filter_map(|word| word.chars().find(char::is_ascii_alphabetic))
        .map(|letter| letter.to_ascii_uppercase())
        .take(KEY_BYTES_MAX)
        .collect();
    let mut key: String = letters.into_iter().collect();
    if key.len() < 2 {
        key = name
            .chars()
            .filter(char::is_ascii_alphabetic)
            .map(|letter| letter.to_ascii_uppercase())
            .take(3)
            .collect();
    }
    if key.len() < 2 {
        key = "TK".to_owned();
    }
    assert!(ProjectKey::new(&key).is_ok());
    key
}

/// The plain sentence for a tracker problem code. Input: the code. Output: the sentence.
pub(in crate::tui) fn sentence(code: &str) -> String {
    match code {
        "project_exists" => "Another project has that key. Pick another one.".into(),
        "project_key_invalid" => "Use 2 to 8 letters A to Z.".into(),
        "tracker_project_missing" => "This project is gone from the engine.".into(),
        "project_not_attached" => "This project has no tickets yet.".into(),
        "tracker_unavailable" => "Tickets aren't ready in this engine yet.".into(),
        "tracker_outcome_unknown" => {
            "The engine did not say how that ended. Press r to read it again.".into()
        }
        "trusted_connection_unavailable" => "The engine connection isn't ready yet.".into(),
        "tracker_views_failed" => "The tickets view stopped and was opened again.".into(),
        "stale_version" => "This ticket changed. Review it again.".into(),
        other => plain_problem(other),
    }
}
