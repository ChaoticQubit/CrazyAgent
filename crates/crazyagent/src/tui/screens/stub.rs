use super::{Flow, Place, ProjectTab, Screen};
use crate::protocol::ProjectId;
use crate::tui::ctx::Ctx;
use crate::tui::frame::{Hint, Tab, draw_lines, hint, sections_hint, tab_strip};
use crate::tui::glyphs::Section;
use crate::tui::keys::Key;
use crate::tui::text::{indent, styled};
use crate::tui::theme::Role;
use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::text::Line;

/// A place whose screen is not built in this version: it says so plainly and
/// shows no data.
/// Input: built by the registry for New project and Project. Output: a title,
/// "Not ready yet." and Esc to go back.
pub struct NotReady {
    crumbs: Vec<String>,
    title: String,
    place: Place,
}

impl NotReady {
    /// The New project placeholder.
    /// Input: none. Output: the screen.
    pub fn new_project() -> Self {
        Self {
            crumbs: vec!["Home".into(), "New project".into()],
            title: "New project".into(),
            place: Place::NewProject,
        }
    }

    /// The Project placeholder.
    /// Inputs: project id, name and tab. Output: the screen.
    pub fn project(id: ProjectId, name: String, tab: ProjectTab) -> Self {
        Self {
            crumbs: vec!["Home".into(), name.clone()],
            title: name.clone(),
            place: Place::Project { id, name, tab },
        }
    }
}

fn tab_section(tab: ProjectTab) -> Section {
    match tab {
        ProjectTab::Overview => Section::Overview,
        ProjectTab::Conversation => Section::Conversation,
        ProjectTab::Agents => Section::Agents,
        ProjectTab::Context => Section::Context,
        ProjectTab::Settings => Section::Settings,
    }
}

impl Screen for NotReady {
    fn crumbs(&self) -> Vec<String> {
        self.crumbs.clone()
    }

    fn hints(&self, ctx: &Ctx) -> Vec<Hint> {
        let mut hints = vec![hint("Esc", "Back"), hint("Ctrl-K", "Go to"), hint("Ctrl-Q", "Quit")];
        if matches!(self.place, Place::Project { .. }) {
            hints.insert(1, sections_hint(ctx.glyphs));
            hints[1].label = "Tabs".into();
        }
        hints
    }

    fn key(&mut self, _key: &Key, _ctx: &mut Ctx) -> Flow {
        Flow::Ignored
    }

    fn step(&mut self, forward: bool, _ctx: &mut Ctx) -> Flow {
        let Place::Project { id, name, tab } = &self.place else {
            return Flow::Ignored;
        };
        let index = ProjectTab::ALL
            .iter()
            .position(|each| each == tab)
            .expect("every tab is listed");
        let count = ProjectTab::ALL.len();
        let next = ProjectTab::ALL[if forward { (index + 1) % count } else { (index + count - 1) % count }];
        Flow::Replace(Box::new(Self::project(id.clone(), name.clone(), next)))
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect, ctx: &Ctx) -> Option<Position> {
        let mut lines = Vec::new();
        if let Place::Project { tab, .. } = &self.place {
            let tabs: Vec<Tab> = ProjectTab::ALL
                .iter()
                .map(|each| Tab {
                    label: each.label().into(),
                    section: Some(tab_section(*each)),
                    count: String::new(),
                })
                .collect();
            let active = ProjectTab::ALL
                .iter()
                .position(|each| each == tab)
                .expect("every tab is listed");
            lines.extend(tab_strip(area.width as usize, &tabs, active, &ctx.theme, ctx.glyphs));
        }
        lines.extend(indent(
            vec![
                styled(self.title.clone(), Role::Accent, &ctx.theme),
                Line::default(),
                Line::raw("Not ready yet."),
                styled(
                    "This part of CrazyAgent is still being built. Press Esc to go back.",
                    Role::Muted,
                    &ctx.theme,
                ),
            ],
            2,
        ));
        draw_lines(frame.buffer_mut(), area, &lines);
        None
    }

    fn place(&self) -> Option<Place> {
        Some(self.place.clone())
    }
}
