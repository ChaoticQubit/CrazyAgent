use crate::tui::ctx::Ctx;
use crate::tui::frame::Hint;
use crate::tui::keys::Key;
use crate::tui::screens::{Flow, Place, Screen};
use ratatui::Frame;
use ratatui::layout::{Position, Rect};

/// The page's title, the words the desktop's Settings list and page use too.
pub(in crate::tui) const TITLE: &str = "Connector activity";

/// A call's sizes as both apps show them, such as "120 B in, 2.0 KiB out". Inputs: the
/// bytes sent and received. Output: the words.
pub(in crate::tui) fn size_words(bytes_in: i64, bytes_out: i64) -> String {
    let _ = (bytes_in, bytes_out);
    String::new()
}

/// Settings, Connector activity (DEC-168 addendum 8): the newest connector calls with
/// their filters and the keep setting beside them. Opened by Go to.
pub(in crate::tui) struct ConnectorActivityPage;

impl ConnectorActivityPage {
    /// Opens the page with nothing read yet. Input: none. Output: the page.
    pub fn new() -> Self {
        Self
    }
}

impl Screen for ConnectorActivityPage {
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
        Some(Place::ConnectorActivity)
    }
}

#[cfg(all(test, unix))]
mod tests;
