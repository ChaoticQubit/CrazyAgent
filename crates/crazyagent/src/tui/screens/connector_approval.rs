use crate::connectors::approvals::{ApprovalView, State};
use crate::tui::ctx::Ctx;
use crate::tui::frame::Hint;
use crate::tui::keys::Key;
use crate::tui::screens::{Flow, Place, Screen};
use ratatui::Frame;
use ratatui::layout::{Position, Rect};

#[cfg(all(test, unix))]
mod tests;

/// The page's title (api-connectors.ts APPROVAL_PAGE.title).
pub(in crate::tui) const TITLE: &str = "Connector write";

/// The time a waiting write has left (timeLeft). Inputs: now and the end of the wait, in
/// epoch milliseconds. Output: such as "9:59".
pub(in crate::tui) fn time_left(_now: i64, _expires: i64) -> String {
    String::new()
}

/// Where a write stands as the page shows it (shownState). Inputs: the approval and now in
/// epoch milliseconds. Output: the state to show.
pub(in crate::tui) fn shown_state(view: &ApprovalView, _now: i64) -> State {
    view.state
}

/// The page's lines in order (approvalLines). Inputs: the approval and the UTC offset.
/// Output: the lines.
pub(in crate::tui) fn approval_lines(_view: &ApprovalView, _offset: i64) -> Vec<String> {
    Vec::new()
}

/// Where an answered write stands (answeredLine). Inputs: the approval, its shown state
/// and the UTC offset. Output: the line, empty while it waits.
pub(in crate::tui) fn answered_line(_view: &ApprovalView, _state: State, _offset: i64) -> String {
    String::new()
}

/// The approval page the ConnectorWrite notice opens (DEC-168 addendum 32).
pub(in crate::tui) struct ConnectorApprovalPage {
    id: i64,
}

impl ConnectorApprovalPage {
    /// Opens the page for one write with nothing read yet. Input: the approval id. Output:
    /// the page; the read goes out when it is shown.
    pub fn new(id: i64) -> Self {
        Self { id }
    }
}

impl Screen for ConnectorApprovalPage {
    fn crumbs(&self) -> Vec<String> {
        vec![TITLE.into()]
    }

    fn hints(&self, _ctx: &Ctx) -> Vec<Hint> {
        Vec::new()
    }

    fn key(&mut self, _key: &Key, _ctx: &mut Ctx) -> Flow {
        Flow::Ignored
    }

    fn draw(&mut self, _frame: &mut Frame, _area: Rect, _ctx: &Ctx) -> Option<Position> {
        None
    }

    fn place(&self) -> Option<Place> {
        Some(Place::ConnectorApproval { id: self.id })
    }
}
