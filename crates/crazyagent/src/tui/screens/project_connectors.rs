use crate::protocol::ProjectId;
use crate::tui::ctx::Ctx;
use crate::tui::frame::Hint;
use crate::tui::keys::Key;
use crate::tui::screens::{Flow, Place, Screen};
use ratatui::Frame;
use ratatui::layout::{Position, Rect};

#[cfg(all(test, unix))]
mod tests;

/// A project's Connectors page (DEC-168 addenda 18 and 20): the connectors its master and
/// agents may use, each with its On switch and, while On, who it reaches.
pub(in crate::tui) struct ProjectConnectorsPage {
    project: ProjectId,
    name: String,
}

impl ProjectConnectorsPage {
    /// Opens the page for one project with nothing read yet. Inputs: the project and its
    /// name. Output: the page.
    pub fn new(project: ProjectId, name: String) -> Self {
        Self { project, name }
    }
}

impl Screen for ProjectConnectorsPage {
    fn crumbs(&self) -> Vec<String> {
        vec![self.name.clone(), "Connectors".into()]
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
        Some(Place::ProjectConnectors {
            id: self.project.clone(),
            name: self.name.clone(),
        })
    }
}
