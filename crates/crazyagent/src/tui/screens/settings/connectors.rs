use crate::tui::ctx::Ctx;
use crate::tui::frame::Hint;
use crate::tui::keys::Key;
use crate::tui::screens::{Flow, Place, Screen};
use ratatui::Frame;
use ratatui::layout::{Position, Rect};

#[cfg(all(test, unix))]
mod tests;
pub(in crate::tui) mod words;

/// The page's title, the word the desktop's Settings list and page use too.
pub(in crate::tui) const TITLE: &str = "Connectors";

/// Settings, Connectors (DEC-168 addenda 18 and 23): every connector with its state, its
/// own page with its keys, adding through the trust screen, and the connector settings.
pub(in crate::tui) struct ConnectorsPage;

impl ConnectorsPage {
    /// Opens the page with nothing read yet. Input: none. Output: the page.
    pub fn new() -> Self {
        Self
    }
}

impl Default for ConnectorsPage {
    fn default() -> Self {
        Self::new()
    }
}

impl Screen for ConnectorsPage {
    fn crumbs(&self) -> Vec<String> {
        vec!["Settings".into(), TITLE.into()]
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
        Some(Place::Connectors)
    }
}
