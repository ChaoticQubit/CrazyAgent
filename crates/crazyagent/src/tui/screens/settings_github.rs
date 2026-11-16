use super::{Flow, Screen};
use crate::tui::ctx::Ctx;
use crate::tui::frame::Hint;
use crate::tui::keys::Key;
use ratatui::Frame;
use ratatui::layout::{Position, Rect};

pub(in crate::tui) const DEFAULTS_TITLE: &str = "";
pub(in crate::tui) const DEFAULTS_ABOUT: &str = "";
pub(in crate::tui) const STANDARD_LABEL: &str = "";
pub(in crate::tui) const DEFAULTS_UNREADABLE: &str = "";
pub(in crate::tui) const DEFAULTS_UNSAVED: &str = "";

/// Settings / GitHub: the rules each project CrazyAgent publishes on GitHub starts with.
pub struct GitHubDefaults;

impl GitHubDefaults {
    /// The page before its read. Takes nothing. Returns the screen.
    pub fn new() -> Self {
        Self
    }
}

impl Screen for GitHubDefaults {
    fn crumbs(&self) -> Vec<String> {
        Vec::new()
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
}
