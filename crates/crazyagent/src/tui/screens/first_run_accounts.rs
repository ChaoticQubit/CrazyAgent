use super::{Flow, Place, Screen};
use crate::tui::ctx::Ctx;
use crate::tui::frame::Hint;
use crate::tui::keys::Key;
use ratatui::Frame;
use ratatui::layout::{Position, Rect};

/// The first-run Accounts step, after Setup: Add account or Skip for now, the
/// same words as the desktop first run.
/// Input: keys. Output: the step; both choices end first run.
pub struct FirstRunAccounts;

impl FirstRunAccounts {
    /// A new Accounts step.
    /// Input: none. Output: the screen.
    pub fn new() -> Self {
        Self
    }
}

impl Screen for FirstRunAccounts {
    fn crumbs(&self) -> Vec<String> {
        vec!["Accounts".into()]
    }

    fn hints(&self, _ctx: &Ctx) -> Vec<Hint> {
        Vec::new()
    }

    fn key(&mut self, _key: &Key, _ctx: &mut Ctx) -> Flow {
        Flow::Handled
    }

    fn draw(&mut self, _frame: &mut Frame, _area: Rect, _ctx: &Ctx) -> Option<Position> {
        None
    }

    fn place(&self) -> Option<Place> {
        Some(Place::FirstRunAccounts)
    }
}

#[cfg(test)]
mod tests;
