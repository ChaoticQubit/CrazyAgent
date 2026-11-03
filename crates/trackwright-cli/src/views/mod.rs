mod board;
pub mod cases;
pub mod coverage;
mod detail;
mod roadmap;
pub mod runs;
mod table;
pub mod text;
mod theme;
mod wire;

pub use theme::Theme;

use board::Board;
use detail::Detail;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use roadmap::Roadmap;
use serde_json::Value;
use table::Table;
use text::{cut, safe, width};
use trackwright_core::model::{
    CheckKind, CriterionState, CycleStatus, EvidenceResult, OpKind, ProjectKey, TicketKind,
    TicketStatus, VersionStatus,
};
use wire::Refused;

/// Smallest area a view draws into; a smaller one shows one line asking for more room.
pub const MIN_WIDTH: u16 = 60;
/// Smallest area height a view draws into.
pub const MIN_HEIGHT: u16 = 8;

/// The tracker as the views reach it: one operation in, one reply envelope out. The
/// standalone TUI implements it over the core's `dispatch`; a host implements it over its
/// own protocol.
pub trait Client {
    /// Runs one operation. Inputs: the operation and its input object, exactly as the
    /// wire carries them. Output: the reply envelope (`api`, `ok`, `data` or `code` and
    /// `message`, `next`, `cursor`).
    fn call(&mut self, op: OpKind, input: Value) -> Value;
}

/// The pages the views offer; a ticket's detail opens over any of them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Page {
    List,
    Ready,
    Board,
    Roadmap,
}

impl Page {
    /// Every page in tab order.
    pub const ALL: [Page; 4] = [Page::List, Page::Ready, Page::Board, Page::Roadmap];

    /// The page's name for tabs and title rows. Output: the name.
    pub fn label(self) -> &'static str {
        match self {
            Page::List => "Tickets",
            Page::Ready => "Ready",
            Page::Board => "Board",
            Page::Roadmap => "Roadmap",
        }
    }
}

/// A key the views act on. A host maps its own key events to these.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Up,
    Down,
    Left,
    Right,
    PageUp,
    PageDown,
    Enter,
    Back,
    Char(char),
}

/// One key and what it does, for the host's help line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hint {
    pub key: &'static str,
    pub label: &'static str,
}

/// Whether a notice reports an ordinary outcome or a refusal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Info,
    Error,
}

/// The one-line result the views show on their last row until the next key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notice {
    pub tone: Tone,
    pub text: String,
}

impl Notice {
    fn info(text: impl Into<String>) -> Self {
        Self {
            tone: Tone::Info,
            text: text.into(),
        }
    }

    fn refused(refused: &Refused) -> Self {
        Self {
            tone: Tone::Error,
            text: format!("{}: {}", refused.code, refused.message),
        }
    }
}

/// The tracker's human views for one project: list, ready, board and roadmap pages and
/// a ticket's detail over them. Every read and write goes through the given `Client`.
pub struct Views {
    page: Page,
    list: Table,
    ready: Table,
    board: Board,
    roadmap: Roadmap,
    detail: Option<Detail>,
    notice: Option<Notice>,
}

impl Views {
    /// Starts the views on the list page with nothing loaded. Input: the project key.
    /// Output: the views; call `open` to load a page.
    pub fn new(project: ProjectKey) -> Self {
        Self {
            list: Table::new(OpKind::List, &project),
            ready: Table::new(OpKind::Ready, &project),
            board: Board::new(&project),
            roadmap: Roadmap::new(&project),
            page: Page::List,
            detail: None,
            notice: None,
        }
    }

    /// Shows a page and loads it. Inputs: the page and the client. A refused load shows
    /// its code and message on the notice row.
    pub fn open(&mut self, page: Page, client: &mut dyn Client) {
        self.page = page;
        self.detail = None;
        self.notice = None;
        self.reload(client);
    }

    /// The page under the detail, or the page shown. Output: the page.
    pub fn page(&self) -> Page {
        self.page
    }

    /// The notice on the last row, if any. Output: the notice.
    pub fn notice(&self) -> Option<&Notice> {
        self.notice.as_ref()
    }

    /// Acts on one key. Inputs: the key and the client. Output: true when the views used
    /// the key; false leaves it to the host (for example Back on a page, to leave).
    pub fn key(&mut self, key: Key, client: &mut dyn Client) -> bool {
        self.notice = None;
        if key == Key::Char('r') {
            self.reload(client);
            return true;
        }
        if let Some(detail) = self.detail.as_mut() {
            if key == Key::Back {
                self.detail = None;
                return true;
            }
            return detail.key(key);
        }
        let outcome = match self.page {
            Page::List => self.list.key(key, client),
            Page::Ready => self.ready.key(key, client),
            Page::Board => self.board.key(key, client),
            Page::Roadmap => Outcome::from(self.roadmap.key(key)),
        };
        match outcome {
            Outcome::Unused => false,
            Outcome::Used => true,
            Outcome::Said(notice) => {
                self.notice = Some(notice);
                true
            }
            Outcome::Open(id) => {
                self.show(&id, client);
                true
            }
        }
    }

    /// The keys the shown view takes, for the host's help line. Output: the hints.
    pub fn hints(&self) -> Vec<Hint> {
        let hint = |key, label| Hint { key, label };
        if self.detail.is_some() {
            return vec![
                hint("↑↓", "Scroll"),
                hint("r", "Reload"),
                hint("Esc", "Back"),
            ];
        }
        match self.page {
            Page::List | Page::Ready => vec![
                hint("↑↓", "Select"),
                hint("Enter", "Open"),
                hint("r", "Reload"),
            ],
            Page::Board => vec![
                hint("←→", "Column"),
                hint("↑↓", "Select"),
                hint("< >", "Move"),
                hint("Enter", "Open"),
                hint("r", "Reload"),
            ],
            Page::Roadmap => vec![hint("↑↓", "Scroll"), hint("r", "Reload")],
        }
    }

    /// Draws the shown view and the notice row. Inputs: the area, the buffer and the
    /// theme. An area under `MIN_WIDTH` x `MIN_HEIGHT` gets one line asking for room.
    pub fn render(&self, area: Rect, buf: &mut Buffer, theme: &Theme) {
        if area.width < MIN_WIDTH || area.height < MIN_HEIGHT {
            let ask = format!(" Make the window at least {MIN_WIDTH} x {MIN_HEIGHT}.");
            buf.set_line(
                area.x,
                area.y,
                &Line::styled(cut(&ask, usize::from(area.width)), theme.warning),
                area.width,
            );
            return;
        }
        let body = Rect {
            height: area.height - 1,
            ..area
        };
        match (&self.detail, self.page) {
            (Some(detail), _) => detail.render(body, buf, theme),
            (None, Page::List) => self.list.render(body, buf, theme),
            (None, Page::Ready) => self.ready.render(body, buf, theme),
            (None, Page::Board) => self.board.render(body, buf, theme),
            (None, Page::Roadmap) => self.roadmap.render(body, buf, theme),
        }
        if let Some(notice) = &self.notice {
            let style = match notice.tone {
                Tone::Info => theme.text,
                Tone::Error => theme.error,
            };
            let line = format!(" {}", safe(&notice.text));
            buf.set_line(
                area.x,
                area.y + area.height - 1,
                &Line::styled(cut(&line, usize::from(area.width)), style),
                area.width,
            );
        }
    }

    fn reload(&mut self, client: &mut dyn Client) {
        if let Some(id) = self.detail.as_ref().map(|detail| detail.id().to_owned()) {
            self.show(&id, client);
            return;
        }
        let loaded = match self.page {
            Page::List => self.list.load(client),
            Page::Ready => self.ready.load(client),
            Page::Board => self.board.load(client),
            Page::Roadmap => self.roadmap.load(client),
        };
        if let Err(refused) = loaded {
            self.notice = Some(Notice::refused(&refused));
        }
    }

    fn show(&mut self, id: &str, client: &mut dyn Client) {
        match Detail::load(id, client) {
            Ok(detail) => self.detail = Some(detail),
            Err(refused) => self.notice = Some(Notice::refused(&refused)),
        }
    }
}

/// What a page did with a key: nothing, something silent, something to say, or a ticket
/// to open in the detail view.
enum Outcome {
    Unused,
    Used,
    Said(Notice),
    Open(String),
}

impl From<bool> for Outcome {
    fn from(used: bool) -> Self {
        if used { Outcome::Used } else { Outcome::Unused }
    }
}

/// Draws a view's title row: the project key and the page name on the left, a count on
/// the right. Inputs: the area's first row, the buffer, the theme, the key, the name and
/// the count text.
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

/// Draws one row of plain text cut to the area's width. Inputs: the area, the row index
/// inside it, the text and its style. A row past the area's bottom is not drawn.
fn put(area: Rect, buf: &mut Buffer, row: u16, text: &str, style: Style) {
    if row >= area.height {
        return;
    }
    let line = Line::styled(cut(text, usize::from(area.width)), style);
    buf.set_line(area.x, area.y + row, &line, area.width);
}

/// Keeps a selection inside the visible window with the least scrolling. Inputs: the
/// current top, the selected index and the visible row count. Output: the new top.
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

fn status_label(status: TicketStatus) -> &'static str {
    match status {
        TicketStatus::Triage => "Triage",
        TicketStatus::Backlog => "Backlog",
        TicketStatus::Todo => "Todo",
        TicketStatus::InProgress => "In progress",
        TicketStatus::InReview => "In review",
        TicketStatus::Done => "Done",
        TicketStatus::Cancelled => "Cancelled",
    }
}

fn kind_label(kind: TicketKind) -> &'static str {
    match kind {
        TicketKind::Feature => "Feature",
        TicketKind::Bug => "Bug",
        TicketKind::Chore => "Chore",
        TicketKind::Spike => "Spike",
    }
}

fn version_label(status: VersionStatus) -> &'static str {
    match status {
        VersionStatus::Planned => "Planned",
        VersionStatus::Active => "Active",
        VersionStatus::Released => "Released",
    }
}

fn cycle_label(status: CycleStatus) -> &'static str {
    match status {
        CycleStatus::Upcoming => "Upcoming",
        CycleStatus::Active => "Active",
        CycleStatus::Closed => "Closed",
    }
}

fn criterion_mark(state: CriterionState) -> &'static str {
    match state {
        CriterionState::Open => "[ ]",
        CriterionState::Met => "[x]",
        CriterionState::Waived => "[-]",
    }
}

fn criterion_label(state: CriterionState) -> &'static str {
    match state {
        CriterionState::Open => "open",
        CriterionState::Met => "met",
        CriterionState::Waived => "waived",
    }
}

fn check_label(kind: CheckKind) -> &'static str {
    match kind {
        CheckKind::Test => "test",
        CheckKind::Change => "change",
        CheckKind::Manual => "manual",
        CheckKind::Ci => "ci",
    }
}

fn evidence_label(result: Option<EvidenceResult>) -> &'static str {
    match result {
        Some(EvidenceResult::Pass) => "pass",
        Some(EvidenceResult::Fail) => "fail",
        None => "no evidence",
    }
}
