use super::{Flow, Place, Screen};
use crate::tui::ctx::Ctx;
use crate::tui::frame::Hint;
use crate::tui::keys::Key;
use ratatui::Frame;
use ratatui::layout::{Position, Rect};

/// The notification center list (DEC-136a decisions 9 and 10), opened with n.
pub(in crate::tui) struct Notifications;

impl Notifications {
    /// Opens the list with nothing read yet. Input: none. Output: the screen.
    pub fn new() -> Self {
        Self
    }
}

impl Screen for Notifications {
    fn crumbs(&self) -> Vec<String> {
        vec!["Notifications".into()]
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
        Some(Place::Notifications)
    }
}

#[cfg(all(test, unix))]
mod tests;
