use super::{Flow, Screen};
use crate::protocol::ProjectId;
use crate::tui::ctx::Ctx;
use crate::tui::frame::Hint;
use crate::tui::keys::Key;
use ratatui::Frame;
use ratatui::layout::{Position, Rect};

pub(in crate::tui) const RULES_NOT_PUBLISHED: &str = "";
pub(in crate::tui) const RULES_NOT_READY: &str = "";
pub(in crate::tui) const RULES_UNAVAILABLE: &str = "";
pub(in crate::tui) const SENDING_TAIL: &str = "";
pub(in crate::tui) const REPO_TAIL: &str = "";

/// Project > Settings > GitHub: the live rules of the repository a project was published to.
pub struct ProjectGitHub;

impl ProjectGitHub {
    /// The page before its read. Takes the project and its name. Returns the screen.
    pub fn new(project: ProjectId, name: String) -> Self {
        let _ = (project, name);
        Self
    }
}

impl Screen for ProjectGitHub {
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
