use super::new_project_input::{TextInput, cells};
use super::new_project_view::{Look, Row, wrap_words};
use super::project_team::{Help, Notice, TABS};
use crate::checks::settings::GateMode;
use crate::engine::owner::CheckGateView;
use crate::landing::ChangeSetId;
use crate::protocol::ProjectId;
use crate::tui::client::checks::{ChangeChecks, ChecksEvent, ChecksJob, STEPS_PER_JOB_MAX};
use crate::tui::client::settings::SettingsEvent;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use serde::Deserialize;
use serde_json::{Value, json};
use std::cell::Cell;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use trackwright_cli::views::cases::CaseList;
use trackwright_cli::views::coverage::Coverage;
use trackwright_cli::views::runs::RunDetail;
use trackwright_cli::views::text::{cut, pad, safe, width};
use trackwright_cli::views::{Client, Key as ViewKey, Theme};
use trackwright_core::coverage::CoverageOf;
use trackwright_core::model::{OpKind, ProjectKey, ShortId};
use trackwright_core::runs::{RunRow, RunState, Verdict};

#[cfg(all(test, unix))]
mod pages;

const STEPS_QUEUED_MAX: usize = 32;
const SCOPE_BYTES_MAX: usize = 64;
const RUNS_SHOWN_MAX: usize = 500;
const HEAD_ROWS: u16 = 3;
const MARKER_CELLS: usize = 3;
const ID_CELLS: usize = 12;
const VERDICT_CELLS: usize = 14;
const PASSED_CELLS: usize = 12;
const FIELD_LEFT: usize = 22;
const FIELD_CELLS: usize = 24;
const CHECK_CELLS: usize = 18;
const COMMIT_SHOWN: usize = 7;
const CHECKS_POLL_MS: i64 = 5_000;
const GAPS_SHOWN: usize = 4;
const STALE: &str = "checks_stale";

/// One page of the Tests tab, in tab order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::tui) enum TestPage {
    Cases,
    Runs,
    Coverage,
}

impl TestPage {
    /// Every page in tab order.
    pub(in crate::tui) const ALL: [TestPage; 3] = [Self::Cases, Self::Runs, Self::Coverage];

    /// The page's name for the page row. Output: the name.
    pub(in crate::tui) fn label(self) -> &'static str {
        match self {
            Self::Cases => "Cases",
            Self::Runs => "Runs",
            Self::Coverage => "Coverage",
        }
    }
}

/// One thing the test views do on the owner worker: show a page, act on a key, show the
/// next verdict filter of the run list, or read coverage of a ticket or release.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::tui) enum Step {
    Open(TestPage),
    Key(ViewKey),
    Filter,
    Cover(String),
}

#[derive(Debug, Deserialize)]
struct RunsPage {
    #[serde(default)]
    runs: Vec<RunRow>,
}

/// The project's runs as `run_list` gives them, newest first, under an optional verdict
/// filter; Down past the last row reads the next page.
struct RunRows {
    rows: Vec<RunRow>,
    cursor: Option<String>,
    filter: Option<Verdict>,
    selected: usize,
    loaded: bool,
    top: Cell<usize>,
    visible: Cell<usize>,
}

/// The test pages of one project over trackwright's views: the case list, the run list,
/// coverage of one ticket or release, and a run's detail over any of them. Every read goes
/// through the given `Client`; a refusal is kept as one sentence for the last row.
pub(in crate::tui) struct TestViews {
    key: ProjectKey,
    page: TestPage,
    cases: CaseList,
    runs: RunRows,
    coverage: Option<Coverage>,
    detail: Option<RunDetail>,
    problem: Option<String>,
}

impl TestViews {
    /// Starts on the case list with nothing read. Input: the project's tracker key.
    /// Output: the views; an `Open` step reads a page.
    pub(in crate::tui) fn new(key: ProjectKey) -> Self {
        Self {
            cases: CaseList::new(&key),
            runs: RunRows {
                rows: Vec::new(),
                cursor: None,
                filter: None,
                selected: 0,
                loaded: false,
                top: Cell::new(0),
                visible: Cell::new(1),
            },
            key,
            page: TestPage::Cases,
            coverage: None,
            detail: None,
            problem: None,
        }
    }

    /// The page shown under any detail. Output: the page.
    pub(in crate::tui) fn page(&self) -> TestPage {
        self.page
    }

    /// Whether a run's detail is open. Output: the answer.
    pub(in crate::tui) fn detail(&self) -> bool {
        self.detail.is_some()
    }

    /// The run list's verdict filter. Output: the verdict, or None for every run.
    #[cfg(test)]
    pub(in crate::tui) fn filter(&self) -> Option<Verdict> {
        self.runs.filter
    }

    /// Runs one step through the client; a refusal replaces the last problem. Inputs: the
    /// step and the client.
    pub(in crate::tui) fn step(&mut self, step: &Step, client: &mut dyn Client) {
        self.problem = None;
        let done = match step {
            Step::Open(page) => {
                self.page = *page;
                self.detail = None;
                self.load(client)
            }
            Step::Key(key) => self.key(*key, client),
            Step::Filter => {
                self.page = TestPage::Runs;
                self.detail = None;
                self.runs.filter = next_filter(self.runs.filter);
                self.runs.load(&self.key, client)
            }
            Step::Cover(text) => {
                self.page = TestPage::Coverage;
                self.detail = None;
                Coverage::load(&self.key, &scope_of(text), client)
                    .map(|coverage| self.coverage = Some(coverage))
                    .map_err(|refused| said(&refused.code, &refused.message))
            }
        };
        if let Err(problem) = done {
            self.problem = Some(problem);
        }
    }

    /// Closes the run detail without a call. Output: true when one was open.
    pub(in crate::tui) fn back(&mut self) -> bool {
        self.problem = None;
        self.detail.take().is_some()
    }

    fn load(&mut self, client: &mut dyn Client) -> Result<(), String> {
        match self.page {
            TestPage::Cases => self
                .cases
                .load(client)
                .map_err(|refused| said(&refused.code, &refused.message)),
            TestPage::Runs => self.runs.load(&self.key, client),
            TestPage::Coverage => Ok(()),
        }
    }

    fn key(&mut self, key: ViewKey, client: &mut dyn Client) -> Result<(), String> {
        if key == ViewKey::Back {
            self.detail = None;
            return Ok(());
        }
        if let Some(detail) = self.detail.as_mut() {
            detail.key(key);
            return Ok(());
        }
        match self.page {
            TestPage::Cases => self
                .cases
                .key(key, client)
                .map(|_| ())
                .map_err(|refused| said(&refused.code, &refused.message)),
            TestPage::Runs if key == ViewKey::Enter => {
                let Some(id) = self.runs.rows.get(self.runs.selected) else {
                    return Ok(());
                };
                let id = id.id.to_string();
                RunDetail::load(&id, client)
                    .map(|detail| self.detail = Some(detail))
                    .map_err(|refused| said(&refused.code, &refused.message))
            }
            TestPage::Runs => self.runs.key(key, &self.key, client),
            TestPage::Coverage => {
                if let Some(coverage) = self.coverage.as_mut() {
                    coverage.key(key);
                }
                Ok(())
            }
        }
    }

    /// Draws the detail or the page, and the last problem on the last row. Inputs: the
    /// area, the buffer and the theme.
    pub(in crate::tui) fn render(&self, area: Rect, buf: &mut Buffer, theme: &Theme) {
        let body = Rect {
            height: area
                .height
                .saturating_sub(u16::from(self.problem.is_some())),
            ..area
        };
        match (&self.detail, self.page) {
            (Some(detail), _) => detail.render(body, buf, theme),
            (None, TestPage::Cases) => self.cases.render(body, buf, theme),
            (None, TestPage::Runs) => self.runs.render(&self.key, body, buf, theme),
            (None, TestPage::Coverage) => match &self.coverage {
                Some(coverage) => coverage.render(body, buf, theme),
                None => put(
                    body,
                    buf,
                    0,
                    " Press c to show the coverage of a ticket or a release.",
                    theme.muted,
                ),
            },
        }
        if let Some(problem) = &self.problem {
            put(
                area,
                buf,
                area.height.saturating_sub(1),
                &format!(" {problem}"),
                theme.error,
            );
        }
    }
}

impl RunRows {
    fn load(&mut self, key: &ProjectKey, client: &mut dyn Client) -> Result<(), String> {
        let (page, cursor) = runs_page(client, key, self.filter, None)?;
        let kept = self.rows.get(self.selected).map(|row| row.id.clone());
        self.rows = page.runs;
        self.rows.truncate(RUNS_SHOWN_MAX);
        self.cursor = cursor;
        self.loaded = true;
        self.selected = kept
            .and_then(|id| self.rows.iter().position(|row| row.id == id))
            .unwrap_or(0);
        assert!(self.rows.is_empty() || self.selected < self.rows.len());
        Ok(())
    }

    fn more(&mut self, key: &ProjectKey, client: &mut dyn Client) -> Result<bool, String> {
        let Some(at) = self.cursor.clone() else {
            return Ok(false);
        };
        if self.rows.len() >= RUNS_SHOWN_MAX {
            return Ok(false);
        }
        let (page, next) = runs_page(client, key, self.filter, Some(&at))?;
        if page.runs.is_empty() || next.as_deref() == Some(at.as_str()) {
            return Err(said(
                "reply_unreadable",
                "the run_list reply is not one the views can read",
            ));
        }
        let before = self.rows.len();
        self.rows.extend(page.runs);
        self.rows.truncate(RUNS_SHOWN_MAX);
        self.cursor = next;
        assert!(self.rows.len() > before);
        Ok(true)
    }

    fn key(
        &mut self,
        key: ViewKey,
        project: &ProjectKey,
        client: &mut dyn Client,
    ) -> Result<(), String> {
        let last = self.rows.len().saturating_sub(1);
        let screen = self.visible.get().max(1);
        match key {
            ViewKey::Up => self.selected = self.selected.saturating_sub(1),
            ViewKey::Down if self.selected < last => self.selected += 1,
            ViewKey::Down => {
                if self.more(project, client)? {
                    self.selected += 1;
                }
            }
            ViewKey::PageUp => self.selected = self.selected.saturating_sub(screen),
            ViewKey::PageDown => self.selected = (self.selected + screen).min(last),
            _ => {}
        }
        assert!(self.rows.is_empty() || self.selected < self.rows.len());
        Ok(())
    }

    fn render(&self, key: &ProjectKey, area: Rect, buf: &mut Buffer, theme: &Theme) {
        let more = if self.cursor.is_some() { "+" } else { "" };
        let count = self.rows.len();
        let plural = if count == 1 && more.is_empty() {
            ""
        } else {
            "s"
        };
        let shown = self.filter.map_or("every verdict", Verdict::as_str);
        let total = format!("{count}{more} run{plural}  {shown}");
        title_row(area, buf, theme, key, "Runs", &total);
        let fixed = MARKER_CELLS + ID_CELLS + VERDICT_CELLS + PASSED_CELLS;
        let title_cells = usize::from(area.width).saturating_sub(fixed);
        let header = format!(
            "{}{}{}{}{}",
            " ".repeat(MARKER_CELLS),
            pad("ID", ID_CELLS),
            pad("Verdict", VERDICT_CELLS),
            pad("Passed", PASSED_CELLS),
            pad("Title", title_cells),
        );
        put(area, buf, 2, &header, theme.muted);
        if self.rows.is_empty() {
            let empty = if self.loaded {
                "   No runs yet."
            } else {
                "   Reading the runs..."
            };
            put(area, buf, HEAD_ROWS, empty, theme.muted);
            return;
        }
        let visible = usize::from(area.height.saturating_sub(HEAD_ROWS)).max(1);
        let top = fit(self.top.get(), self.selected, visible);
        self.top.set(top);
        self.visible.set(visible);
        for (offset, (index, row)) in self
            .rows
            .iter()
            .enumerate()
            .skip(top)
            .take(visible)
            .enumerate()
        {
            let chosen = index == self.selected;
            let verdict = row.verdict.map_or_else(
                || state_word(row).to_owned(),
                |verdict| verdict.as_str().to_owned(),
            );
            let elided = if row.elided { "\u{2026}" } else { "" };
            let line = format!(
                "{}{}{}{}{}",
                if chosen { " > " } else { "   " },
                pad(&row.id.to_string(), ID_CELLS),
                pad(&verdict, VERDICT_CELLS),
                pad(
                    &format!("{} of {}", row.totals.passed, row.totals.total),
                    PASSED_CELLS
                ),
                pad(&format!("{}{elided}", safe(&row.title)), title_cells),
            );
            let style = match (chosen, row.verdict) {
                (true, _) => theme.selected,
                (false, Some(Verdict::Passed)) => theme.success,
                (false, Some(_)) => theme.error,
                (false, None) => theme.text,
            };
            let at = HEAD_ROWS + u16::try_from(offset).expect("visible rows fit a u16");
            put(area, buf, at, &line, style);
        }
    }
}

fn state_word(row: &RunRow) -> &'static str {
    run_state(row.state)
}

/// Reads one page of the project's runs with `run_list`. Inputs: the client, the project,
/// the verdict filter and the cursor of the page before. Output: the page and its cursor,
/// or the refusal as one sentence.
fn runs_page(
    client: &mut dyn Client,
    key: &ProjectKey,
    filter: Option<Verdict>,
    cursor: Option<&str>,
) -> Result<(RunsPage, Option<String>), String> {
    let mut input = json!({ "project": key.as_str() });
    if let Some(verdict) = filter {
        input["verdict"] = json!(verdict.as_str());
    }
    if let Some(cursor) = cursor {
        input["cursor"] = json!(cursor);
    }
    let reply = client.call(OpKind::RunList, input);
    let unreadable = || {
        said(
            "reply_unreadable",
            "the run_list reply is not one the views can read",
        )
    };
    if reply["ok"].as_bool() != Some(true) {
        let code = reply["code"].as_str().ok_or_else(unreadable)?;
        return Err(said(code, reply["message"].as_str().unwrap_or("")));
    }
    let page: RunsPage = serde_json::from_value(reply["data"].clone()).map_err(|_| unreadable())?;
    let next = match &reply["cursor"] {
        Value::Null => None,
        Value::String(next) => Some(next.clone()),
        _ => return Err(unreadable()),
    };
    Ok((page, next))
}

/// The verdict filter after one: every verdict, then each verdict in order, then every
/// verdict again. Input: the filter. Output: the next filter.
pub(in crate::tui) fn next_filter(filter: Option<Verdict>) -> Option<Verdict> {
    match filter {
        None => Verdict::ALL.first().copied(),
        Some(verdict) => {
            let at = Verdict::ALL
                .iter()
                .position(|candidate| *candidate == verdict)
                .expect("every verdict is listed");
            Verdict::ALL.get(at + 1).copied()
        }
    }
}

/// What a typed coverage scope names: a ticket when it reads as a ticket id, else a release
/// version by name. Input: the text. Output: the scope.
pub(in crate::tui) fn scope_of(text: &str) -> CoverageOf {
    let text = text.trim();
    match ShortId::parse(text) {
        Ok(ticket) => CoverageOf::Ticket(ticket),
        Err(_) => CoverageOf::Version(text.to_owned()),
    }
}

fn title_row(
    area: Rect,
    buf: &mut Buffer,
    theme: &Theme,
    key: &ProjectKey,
    name: &str,
    count: &str,
) {
    let limit = usize::from(area.width);
    let right = format!("{count}  ");
    let left = cut(
        &format!(" {}  {name}", key.as_str()),
        limit.saturating_sub(width(&right)),
    );
    let gap = limit.saturating_sub(width(&left) + width(&right));
    let line = Line::from(vec![
        Span::styled(left, theme.heading),
        Span::raw(" ".repeat(gap)),
        Span::styled(right, theme.muted),
    ]);
    buf.set_line(area.x, area.y, &line, area.width);
}

fn put(area: Rect, buf: &mut Buffer, row: u16, text: &str, style: Style) {
    if row >= area.height {
        return;
    }
    let line = Line::styled(cut(text, usize::from(area.width)), style);
    buf.set_line(area.x, area.y + row, &line, area.width);
}

fn fit(top: usize, selected: usize, visible: usize) -> usize {
    assert!(visible > 0);
    if selected < top {
        selected
    } else if selected >= top + visible {
        selected + 1 - visible
    } else {
        top
    }
}

/// What the Tests tab did with a key: nothing, something, or the owner asked for the
/// project's check settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::tui) enum TestsReply {
    Ignored,
    Handled,
    Settings,
}

enum State {
    Unread,
    Failed(String),
    Off,
    On(Arc<Mutex<TestViews>>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Sent {
    Tracked,
    Views,
}

/// One project's Tests tab: its cases, runs and coverage over trackwright's views, reached
/// through the owner protocol as the owner, so holdout results show in full. The views go
/// to the owner worker with the keys that need the tracker and come back drawn; while they
/// are away the tab shows the rows it drew last. A project without tickets has no tests
/// yet and says so. Sends at most one request at a time.
pub(in crate::tui) struct ProjectTests {
    pub project: ProjectId,
    name: String,
    state: State,
    page: TestPage,
    asked: bool,
    sent: Option<Sent>,
    steps: VecDeque<Step>,
    scoping: Option<TextInput>,
    shot: Vec<Row>,
    notice: Option<Notice>,
    detail: bool,
}

impl ProjectTests {
    /// Opens one project's Tests tab with nothing read yet. Inputs: the project and its
    /// name.
    pub(in crate::tui) fn new(project: ProjectId, name: String) -> Self {
        Self {
            project,
            name,
            state: State::Unread,
            page: TestPage::Cases,
            asked: false,
            sent: None,
            steps: VecDeque::new(),
            scoping: None,
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

    /// The next request to send: the project's tracker key first, then the views with up
    /// to `STEPS_PER_JOB_MAX` queued steps. Output: the job, or none while one is in flight
    /// or nothing waits.
    pub(in crate::tui) fn next_job(&mut self) -> Option<ChecksJob> {
        if self.sent.is_some() {
            return None;
        }
        if matches!(self.state, State::Unread) && !self.asked {
            self.asked = true;
            self.sent = Some(Sent::Tracked);
            return Some(ChecksJob::Tracked {
                project: self.project.clone(),
            });
        }
        let State::On(views) = &self.state else {
            return None;
        };
        if self.steps.is_empty() {
            return None;
        }
        let count = self.steps.len().min(STEPS_PER_JOB_MAX);
        let steps: Vec<Step> = self.steps.drain(..count).collect();
        self.sent = Some(Sent::Views);
        Some(ChecksJob::Views {
            project: self.project.clone(),
            views: Arc::clone(views),
            steps,
        })
    }

    /// Forgets the request in flight after the engine connection dropped: the tracker key
    /// is asked again, and lent views stay with the worker's last state.
    pub(in crate::tui) fn forget_reads(&mut self) {
        if self.sent.take() == Some(Sent::Tracked) {
            self.asked = false;
        }
    }

    /// Whether a request is in flight or steps wait to be sent.
    pub(in crate::tui) fn busy(&self) -> bool {
        self.sent.is_some() || !self.steps.is_empty()
    }

    /// Whether the owner is typing a coverage scope.
    pub(in crate::tui) fn editing(&self) -> bool {
        self.scoping.is_some()
    }

    /// Takes the engine's answer to the request in flight; any other reply is left alone.
    /// Input: the Settings event.
    pub(in crate::tui) fn apply(&mut self, event: &SettingsEvent) {
        let SettingsEvent::Checks(event) = event else {
            return;
        };
        match (event.as_ref(), self.sent) {
            (ChecksEvent::Tracked { project, result }, Some(Sent::Tracked))
                if *project == self.project =>
            {
                self.sent = None;
                match result {
                    Ok(Some(key)) => self.turn_on(key.clone()),
                    Ok(None) => self.state = State::Off,
                    Err(code) => self.state = State::Failed(sentence(code)),
                }
            }
            (ChecksEvent::Views { project, problem }, Some(Sent::Views))
                if *project == self.project =>
            {
                self.sent = None;
                if let Some(code) = problem {
                    self.notice = Some(Notice::problem(sentence(code)));
                }
            }
            _ => {}
        }
    }

    fn turn_on(&mut self, key: ProjectKey) {
        self.state = State::On(Arc::new(Mutex::new(TestViews::new(key))));
        self.steps.clear();
        self.steps.push_back(Step::Open(self.page));
        self.detail = false;
    }

    fn queue(&mut self, step: Step) {
        if self.steps.len() < STEPS_QUEUED_MAX {
            self.steps.push_back(step);
        }
    }

    /// Takes pasted text into the coverage field. Input: the text. Output: true when the
    /// field took it.
    pub(in crate::tui) fn paste(&mut self, text: &str) -> bool {
        let Some(input) = self.scoping.as_mut() else {
            return false;
        };
        let line: String = text.chars().filter(|c| !c.is_control()).collect();
        input.insert_str(&line);
        true
    }

    /// Acts on one key. Input: the key. Output: what the tab did; `Ignored` leaves it to the
    /// project screen (tabs, Esc to Home).
    pub(in crate::tui) fn key(&mut self, key: KeyEvent) -> TestsReply {
        let shifted_arrow = key.modifiers.contains(KeyModifiers::SHIFT)
            && matches!(key.code, KeyCode::Left | KeyCode::Right);
        if shifted_arrow
            || key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        {
            return TestsReply::Ignored;
        }
        if self.scoping.is_some() {
            return self.field_key(key);
        }
        self.notice = None;
        match &self.state {
            State::Unread => TestsReply::Ignored,
            State::Failed(_) | State::Off => match key.code {
                KeyCode::Char('r') => {
                    self.state = State::Unread;
                    self.asked = false;
                    TestsReply::Handled
                }
                KeyCode::Char('s') => TestsReply::Settings,
                _ => TestsReply::Ignored,
            },
            State::On(views) => {
                let views = Arc::clone(views);
                self.views_key(key, &views)
            }
        }
    }

    fn views_key(&mut self, key: KeyEvent, views: &Mutex<TestViews>) -> TestsReply {
        let step = match key.code {
            KeyCode::Char('s') => return TestsReply::Settings,
            KeyCode::Tab | KeyCode::BackTab => {
                self.page = next_page(self.page, key.code == KeyCode::Tab);
                Step::Open(self.page)
            }
            KeyCode::Esc => {
                if self.sent.is_some() || !self.steps.is_empty() {
                    if !self.detail {
                        return TestsReply::Ignored;
                    }
                    self.detail = false;
                    self.queue(Step::Key(ViewKey::Back));
                    return TestsReply::Handled;
                }
                let Ok(mut views) = views.try_lock() else {
                    return TestsReply::Handled;
                };
                return if views.back() {
                    self.detail = false;
                    TestsReply::Handled
                } else {
                    TestsReply::Ignored
                };
            }
            KeyCode::Char('f') if self.page == TestPage::Runs && !self.detail => Step::Filter,
            KeyCode::Char('c') if self.page == TestPage::Coverage && !self.detail => {
                self.scoping = Some(TextInput::single(SCOPE_BYTES_MAX));
                return TestsReply::Handled;
            }
            KeyCode::Char('r') => Step::Open(self.page),
            KeyCode::Up => Step::Key(ViewKey::Up),
            KeyCode::Down => Step::Key(ViewKey::Down),
            KeyCode::PageUp => Step::Key(ViewKey::PageUp),
            KeyCode::PageDown => Step::Key(ViewKey::PageDown),
            KeyCode::Enter if self.page == TestPage::Runs && !self.detail => {
                Step::Key(ViewKey::Enter)
            }
            _ => return TestsReply::Ignored,
        };
        self.queue(step);
        TestsReply::Handled
    }

    fn field_key(&mut self, key: KeyEvent) -> TestsReply {
        let Some(input) = self.scoping.as_mut() else {
            return TestsReply::Ignored;
        };
        match key.code {
            KeyCode::Esc => self.scoping = None,
            KeyCode::Enter => {
                let text = input.value().trim().to_owned();
                if !text.is_empty() {
                    self.scoping = None;
                    self.queue(Step::Cover(text));
                }
            }
            _ => {
                input.handle(key);
            }
        }
        TestsReply::Handled
    }

    /// Draws the tab's rows. Inputs: the content width, the rows it may fill and the look.
    /// Output: the rows, the cursor in the coverage field as (column, row), and the help
    /// keys.
    pub(in crate::tui) fn rows(
        &mut self,
        content: usize,
        room: usize,
        look: &Look,
    ) -> (Vec<Row>, Option<(usize, usize)>, Help) {
        let mut lines = vec![Row::of("Tests", look.heading)];
        match &self.state {
            State::Unread => {
                lines.push(Row::of("Loading tests...", look.muted));
                (lines, None, vec![TABS, ("Esc", "Home")])
            }
            State::Failed(problem) => {
                lines.extend(
                    wrap_words(&format!("Could not read the tests. {problem}"), content)
                        .into_iter()
                        .map(|line| Row::of(line, look.error)),
                );
                lines.push(Row::of("Press r to try again.", look.muted));
                (
                    lines,
                    None,
                    vec![("r", "Try again"), ("s", "Settings"), TABS, ("Esc", "Home")],
                )
            }
            State::Off => {
                lines.extend(
                    wrap_words(
                        &format!(
                            "{} has no tests yet. Tests, runs and coverage live with the project's tickets: turn tickets on in the Tickets tab, and check runs record here once a check profile is approved.",
                            self.name
                        ),
                        content,
                    )
                    .into_iter()
                    .map(|line| Row::of(line, look.muted)),
                );
                (
                    lines,
                    None,
                    vec![
                        ("r", "Read again"),
                        ("s", "Settings"),
                        TABS,
                        ("Esc", "Home"),
                    ],
                )
            }
            State::On(views) => {
                let views = Arc::clone(views);
                self.on_rows(&views, content, room, look)
            }
        }
    }

    fn on_rows(
        &mut self,
        views: &Mutex<TestViews>,
        content: usize,
        room: usize,
        look: &Look,
    ) -> (Vec<Row>, Option<(usize, usize)>, Help) {
        let mut lines = vec![page_row(self.page, look)];
        let height = room.saturating_sub(lines.len());
        let away = self.sent == Some(Sent::Views);
        let mut help: Help = vec![("Tab", "Page")];
        match views.try_lock() {
            Ok(views) if !away => {
                self.shot = drawn(&views, content, height, look);
                self.detail = views.detail();
                help = page_help(views.page(), self.detail);
            }
            _ => help.insert(0, ("", "Working...")),
        }
        help.extend([("s", "Settings"), TABS]);
        if !self.detail {
            help.push(("Esc", "Home"));
        }
        lines.extend(self.shot.iter().cloned());
        let mut cursor = None;
        if let Some(input) = &self.scoping {
            let last = height.min(lines.len().saturating_sub(1)).max(1);
            lines.truncate(last);
            lines.push(
                Row::of("Ticket or release", look.heading)
                    .pad(FIELD_LEFT - 2)
                    .push("[ ", look.accent)
                    .cat(Row::of(input.value(), look.fg).pad(FIELD_CELLS))
                    .push(" ]", look.accent),
            );
            cursor = Some((FIELD_LEFT + cells(input.head()), lines.len() - 1));
            help = vec![("Enter", "Show coverage"), ("Esc", "Cancel")];
        } else if let Some(notice) = &self.notice
            && lines.len() > 1
        {
            let last = lines.len() - 1;
            lines[last] = notice.row(look);
        }
        (lines, cursor, help)
    }
}

/// The Checks block of the change set shown in the Changes tab: the newest run of each
/// check with its verdict, a stale run marked on its line, and Run checks. Reads the shown
/// change set when it opens and after its checks start; sends at most one request at a
/// time.
pub(in crate::tui) struct SetChecks {
    project: ProjectId,
    wanted: Option<ChangeSetId>,
    read: Option<(ChangeSetId, Result<ChangeChecks, String>)>,
    sent: Option<SetSent>,
    start: Option<ChangeSetId>,
    notice: Option<Notice>,
    asked_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum SetSent {
    Runs(ChangeSetId),
    Run,
}

impl SetChecks {
    /// The block of one project's Changes tab with nothing read. Input: the project.
    pub(in crate::tui) fn new(project: ProjectId) -> Self {
        Self {
            project,
            wanted: None,
            read: None,
            sent: None,
            start: None,
            notice: None,
            asked_ms: 0,
        }
    }

    /// Asks for the checks of the change set now shown: once when it opens, then again
    /// every `CHECKS_POLL_MS` while any of its checks waits or runs. Inputs: the change set
    /// and the time now in milliseconds.
    pub(in crate::tui) fn want(&mut self, change_set: &ChangeSetId, now_ms: i64) {
        let read = self.read.as_ref().filter(|(id, _)| id == change_set);
        let waiting =
            read.is_some_and(|(_, checks)| checks.as_ref().is_ok_and(|checks| checks.pending > 0));
        let due = read.is_none() || (waiting && now_ms - self.asked_ms >= CHECKS_POLL_MS);
        if due && self.sent.is_none() && self.wanted.as_ref() != Some(change_set) {
            self.wanted = Some(change_set.clone());
            self.asked_ms = now_ms;
        }
    }

    /// Starts the checks of the change set shown, unless a start is already on its way.
    /// Input: the change set.
    pub(in crate::tui) fn run(&mut self, change_set: &ChangeSetId) {
        if self.sent != Some(SetSent::Run) && self.start.is_none() {
            self.start = Some(change_set.clone());
            self.notice = Some(Notice::plain("Starting the checks..."));
        }
    }

    /// The next request: a start first, then a read. Output: the job, or none while one
    /// is in flight or nothing waits.
    pub(in crate::tui) fn next_job(&mut self) -> Option<ChecksJob> {
        if self.sent.is_some() {
            return None;
        }
        if let Some(change_set) = self.start.take() {
            self.sent = Some(SetSent::Run);
            return Some(ChecksJob::Run {
                project: self.project.clone(),
                change_set,
            });
        }
        let change_set = self.wanted.take()?;
        self.sent = Some(SetSent::Runs(change_set.clone()));
        Some(ChecksJob::Runs {
            project: self.project.clone(),
            change_set,
        })
    }

    /// Forgets the request in flight after the engine connection dropped; a lost start
    /// says so, a lost read of a change set is asked again.
    pub(in crate::tui) fn forget_reads(&mut self) {
        match self.sent.take() {
            Some(SetSent::Run) => {
                self.notice = Some(Notice::problem(
                    "The engine connection dropped before the checks started. Press k to start them again.",
                ));
            }
            Some(SetSent::Runs(change_set)) => self.wanted = Some(change_set),
            None => {}
        }
    }

    /// Whether a request is in flight or one waits to be sent.
    pub(in crate::tui) fn busy(&self) -> bool {
        self.sent.is_some() || self.start.is_some() || self.wanted.is_some()
    }

    /// Takes the engine's answer to the request in flight. Input: the Settings event.
    pub(in crate::tui) fn apply(&mut self, event: &SettingsEvent) {
        let SettingsEvent::Checks(event) = event else {
            return;
        };
        match (event.as_ref(), self.sent.clone()) {
            (ChecksEvent::Runs { change_set, result }, Some(SetSent::Runs(asked)))
                if *change_set == asked =>
            {
                self.sent = None;
                self.read = Some((change_set.clone(), result.clone()));
            }
            (ChecksEvent::Started { change_set, result }, Some(SetSent::Run)) => {
                self.sent = None;
                self.notice = Some(match result {
                    Ok(_) => Notice::plain(
                        "The checks are queued. Their runs show here when they finish.",
                    ),
                    Err(code) => Notice::problem(sentence(code)),
                });
                self.wanted = Some(change_set.clone());
            }
            _ => {}
        }
    }

    /// The block's rows for the change set shown. Inputs: the change set, its head, the
    /// content width and the look. Output: the rows, none before the first read.
    pub(in crate::tui) fn rows(
        &self,
        change_set: &ChangeSetId,
        head: &str,
        content: usize,
        look: &Look,
    ) -> Vec<Row> {
        let mut lines = vec![Row::default(), Row::of("Checks", look.heading)];
        match &self.read {
            Some((id, Ok(checks))) if id == change_set => {
                lines.extend(check_rows(checks, head, content, look));
            }
            Some((id, Err(code))) if id == change_set => lines.extend(
                wrap_words(
                    &format!("Could not read the checks. {}", sentence(code)),
                    content,
                )
                .into_iter()
                .map(|line| Row::of(line, look.error)),
            ),
            _ => lines.push(Row::of("Reading the checks...", look.muted)),
        }
        if let Some(notice) = &self.notice {
            lines.push(notice.row(look));
        }
        lines
    }
}

/// One change set's check lines: one per check with its verdict, run and protected paths
/// touched, a run marked stale when it covered another commit than the head or the gate
/// names it stale, then what the Accept gate meets and the jobs still waiting. Inputs: the
/// checks, the change set's head, the content width and the look. Output: the rows.
fn check_rows(checks: &ChangeChecks, head: &str, content: usize, look: &Look) -> Vec<Row> {
    let gate = checks.gate.as_ref().ok();
    let stale_runs: Vec<&str> = gate
        .into_iter()
        .flat_map(|gate| &gate.gaps)
        .filter(|gap| gap.code == STALE)
        .filter_map(|gap| gap.run.as_deref())
        .collect();
    let mut lines = Vec::new();
    if checks.runs.is_empty() {
        lines.push(Row::of(
            "No check has run on this change set yet. Press k to run them.",
            look.muted,
        ));
    }
    for line in &checks.runs {
        let run = line.run_id.to_string();
        let moved = line.commit != head;
        let stale = moved || stale_runs.contains(&run.as_str());
        let verdict = line.verdict.map_or_else(
            || run_state(line.state).to_owned(),
            |v| v.as_str().to_owned(),
        );
        let style = match (stale, line.verdict) {
            (true, _) => look.warning,
            (false, Some(Verdict::Passed)) => look.success,
            (false, Some(_)) => look.error,
            (false, None) => look.fg,
        };
        let holdout = if line.holdout { "  holdout" } else { "" };
        let protected = match line.protected_touched {
            0 => String::new(),
            touched => format!("  protected {touched}"),
        };
        lines.push(Row::of(
            format!(
                "  {}{}{}{holdout}{protected}",
                pad(&safe(&line.check), CHECK_CELLS),
                pad(&verdict, VERDICT_CELLS),
                run
            ),
            style,
        ));
        if moved {
            lines.push(Row::of(
                format!(
                    "    stale: covered {}, head is {}; it does not count for Accept",
                    short_commit(&line.commit),
                    short_commit(head)
                ),
                look.warning,
            ));
        } else if stale {
            lines.push(Row::of(
                "    stale: another profile or approval; it does not count for Accept",
                look.warning,
            ));
        }
    }
    match &checks.gate {
        Ok(gate) => lines.extend(gate_rows(gate, content, look)),
        Err(code) => lines.extend(
            wrap_words(
                &format!("Could not read the Accept gate. {}", sentence(code)),
                content.saturating_sub(2),
            )
            .into_iter()
            .map(|line| Row::of(format!("  {line}"), look.error)),
        ),
    }
    if checks.pending > 0 {
        lines.push(Row::of(
            match checks.pending {
                1 => "  1 check is waiting or running.".to_owned(),
                many => format!("  {many} checks are waiting or running."),
            },
            look.muted,
        ));
    }
    lines
}

/// What the Accept gate meets at the head: each gap but a stale run (shown on its run's
/// line) with the paths it names, at most `GAPS_SHOWN`, how many more there are, then
/// whether Accept is refused, goes ahead recording gaps, or meets nothing. Inputs: the gate
/// view, the content width and the look. Output: the rows.
fn gate_rows(gate: &CheckGateView, content: usize, look: &Look) -> Vec<Row> {
    let wrapped = |text: &str, style: Style| -> Vec<Row> {
        wrap_words(text, content.saturating_sub(2))
            .into_iter()
            .map(|line| Row::of(format!("  {line}"), style))
            .collect()
    };
    let mut lines = Vec::new();
    let listed: Vec<_> = gate.gaps.iter().filter(|gap| gap.code != STALE).collect();
    for gap in listed.iter().take(GAPS_SHOWN) {
        let mut text = gap
            .check
            .as_deref()
            .map(|check| format!("{}: ", safe(check)))
            .unwrap_or_default();
        text.push_str(&sentence(&gap.code));
        if !gap.paths.is_empty() {
            let paths: Vec<String> = gap.paths.iter().map(|path| safe(path)).collect();
            text.push(' ');
            text.push_str(&paths.join(", "));
            let total = usize::try_from(gap.paths_total).unwrap_or(usize::MAX);
            match total.saturating_sub(gap.paths.len()) {
                0 => text.push('.'),
                more => text.push_str(&format!(" and {more} more.")),
            }
        }
        lines.extend(wrapped(&text, look.warning));
    }
    let unlisted = usize::try_from(gate.gaps_total)
        .unwrap_or(usize::MAX)
        .saturating_sub(gate.gaps.len());
    let more = listed.len().saturating_sub(GAPS_SHOWN) + unlisted;
    if more > 0 {
        lines.push(Row::of(format!("  {more} more gaps."), look.muted));
    }
    match (&gate.refusal, gate.gate) {
        (Some(code), _) => {
            lines.extend(wrapped(
                &format!("Accept is refused: {}", sentence(code)),
                look.error,
            ));
        }
        (None, GateMode::Off) => lines.push(Row::of("  The Accept gate is off.", look.muted)),
        (None, _) if gate.gaps_total > 0 => lines.push(Row::of(
            "  Accept goes ahead; the change set records these gaps.",
            look.warning,
        )),
        (None, _) => lines.push(Row::of(
            "  Nothing in the checks blocks Accept.",
            look.success,
        )),
    }
    lines
}

fn run_state(state: RunState) -> &'static str {
    match state {
        RunState::Running => "running",
        RunState::Finished => "no verdict",
        RunState::Interrupted => "interrupted",
    }
}

fn short_commit(commit: &str) -> String {
    safe(commit.get(..COMMIT_SHOWN).unwrap_or(commit))
}

/// The help keys of one page, or of a run's detail. Inputs: the page and whether a detail
/// is open. Output: the keys.
fn page_help(page: TestPage, detail: bool) -> Help {
    if detail {
        return vec![
            ("\u{2191}\u{2193}", "Scroll"),
            ("Esc", "Back"),
            ("Tab", "Page"),
        ];
    }
    let mut help: Help = vec![("Tab", "Page"), ("\u{2191}\u{2193}", "Move")];
    match page {
        TestPage::Cases => {}
        TestPage::Runs => {
            help.insert(1, ("Enter", "Open run"));
            help.push(("f", "Verdict"));
        }
        TestPage::Coverage => help.push(("c", "Ticket or release")),
    }
    help.push(("r", "Read again"));
    help
}

/// The page after or before one, wrapping. Inputs: the page and the direction. Output: the
/// page.
fn next_page(page: TestPage, forward: bool) -> TestPage {
    let count = TestPage::ALL.len();
    let index = TestPage::ALL
        .iter()
        .position(|candidate| *candidate == page)
        .expect("every page is listed");
    TestPage::ALL[if forward {
        (index + 1) % count
    } else {
        (index + count - 1) % count
    }]
}

/// The row naming the tab's pages, the shown one in brackets. Inputs: the page and the
/// look. Output: the row.
fn page_row(page: TestPage, look: &Look) -> Row {
    let mut row = Row::default();
    for (index, candidate) in TestPage::ALL.iter().enumerate() {
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

/// Draws the test views into an area of the given size and reads it back as rows, one
/// style run per span; the cells a wide character covers are skipped. Inputs: the views,
/// the width, the height and the look. Output: one row per line of the area.
pub(in crate::tui) fn drawn(
    views: &TestViews,
    width: usize,
    height: usize,
    look: &Look,
) -> Vec<Row> {
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

/// A refusal as the views show it: the code's sentence, or the code and the reply's
/// message for a code this version does not know. Inputs: the code and the message.
/// Output: the sentence.
fn said(code: &str, message: &str) -> String {
    let known = sentence(code);
    if known != code {
        return known;
    }
    let message = safe(message);
    if message.is_empty() {
        known
    } else {
        format!("{code}: {message}")
    }
}

/// The plain sentence for a tests or checks problem code. Input: the code. Output: the
/// sentence, or the code itself for one this version does not know.
pub(in crate::tui) fn sentence(code: &str) -> String {
    match code {
        "trusted_connection_unavailable" | "controller_disconnected" => {
            "The engine is not connected. It reconnects on its own; try again in a moment."
        }
        "tracker_unavailable" | "checks_unavailable" => {
            "The engine could not read its records just now. Try again."
        }
        "tracker_views_failed" => "The test views stopped while reading. Press r to read again.",
        "tracker_busy" => "Still working on the last key.",
        "project_not_found" | "tracker_project_missing" => {
            "This project has no tickets or tests in the tracker."
        }
        "ticket_not_found" => "No ticket has that id in this project.",
        "version_not_found" => "No release of this project has that name.",
        "run_not_found" => "That run is no longer kept.",
        "reply_unreadable" | "checks_outcome_unknown" => {
            "The engine's answer could not be read. Press r to read again."
        }
        "checks_owner_only" => "Only you can do that, from this app.",
        "checks_profile_unknown" => "That check profile is no longer stored.",
        "checks_profile_invalid" => "That check profile is not one the engine accepts.",
        "checks_profile_revoked" => "That check profile was revoked.",
        "checks_project_missing" => "The engine does not know this project.",
        "checks_setting_invalid" => "That value is not one of the setting's options.",
        "checks_settings_stale" => "The settings changed somewhere else. Read them again.",
        "changes_not_found" => "That change set is gone.",
        "changes_stale" => "The change set moved on since you opened it. Read it again.",
        "checks_not_run" => "Its required checks have not run on this head.",
        "checks_failed" => "A required check did not pass on this head.",
        "checks_stale" => "A check covered another head or profile, so it no longer counts.",
        "checks_protected_change" => "It changes a protected path.",
        "checks_new_skips" => "It adds a skip marker to a test.",
        "checks_tests_removed" => "A check no longer reports a test its last baseline ran.",
        "checks_no_profile" => {
            "No check profile is approved for this project. Approve one in Settings, Checks."
        }
        "checks_nothing_to_run" => "The current profile has no check you can start by hand.",
        "checks_queue_full" => "Too many check runs are waiting. Try again once some finish.",
        "checks_change_set_accepted" => "That change set is already accepted.",
        "checks_change_set_discarded" => "That change set was discarded.",
        "checks_change_set_moved" | "checks_change_set_unknown" => {
            "The change set moved on since you opened it. Read it again."
        }
        "checks_unavailable_off_macos" | "checks_sandbox_unavailable" => {
            "Check runs need the macOS sandbox, so they cannot run here."
        }
        "checks_engine_stopped" => "The engine is stopping, so no check starts now.",
        other => return other.to_owned(),
    }
    .to_owned()
}
