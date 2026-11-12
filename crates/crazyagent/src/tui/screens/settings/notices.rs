use crate::tui::ctx::Ctx;
use crate::tui::frame::Hint;
use crate::tui::keys::Key;
use crate::tui::screens::{Flow, Place, Screen};
use ratatui::Frame;
use ratatui::layout::{Position, Rect};

/// Settings, Notifications (DEC-136a slice 3): each kind's delivery, the macOS
/// notifications choice and the days Done items stay, in the desktop's words, saved as
/// one change with Enter. Reached from Go to and from s on the notifications list.
pub(in crate::tui) struct NotificationSettingsPage;

impl NotificationSettingsPage {
    /// Opens the page with nothing read yet. Input: none. Output: the page; the read goes
    /// out when it is shown.
    pub fn new() -> Self {
        Self
    }
}

impl Screen for NotificationSettingsPage {
    fn crumbs(&self) -> Vec<String> {
        vec!["Settings".into(), "Notifications".into()]
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
        Some(Place::NotificationSettings)
    }
}

#[cfg(all(test, unix))]
mod tests;
