use super::{Flow, Place, Screen, open};
use crate::tui::ctx::Ctx;
use crate::tui::frame::{Hint, draw_lines, hint};
use crate::tui::keys::Key;
use crate::tui::text::{indent, styled};
use crate::tui::theme::Role;
use crate::tui::widgets::menu_item;
use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::text::Line;

const MENU_WIDTH: usize = 72;

const CHOICES: [(&str, &str, char); 3] = [
    (
        "New project",
        "Start from nothing. CrazyAgent makes the folder and plans the work with you.",
        'n',
    ),
    (
        "Clone from GitHub",
        "Sign in with GitHub and pick a repository. It lands in your projects folder.",
        'c',
    ),
    (
        "Existing folder",
        "Move or copy a folder from this computer into your projects folder.",
        'e',
    ),
];

/// The Add project page: a new project, a GitHub clone or an existing folder.
/// Opened from Home with `a`. The chosen page replaces it, so going back from
/// there returns to Home.
pub struct AddProject {
    selected: usize,
}

impl Default for AddProject {
    fn default() -> Self {
        Self::new()
    }
}

impl AddProject {
    /// An Add project page with New project selected.
    /// Takes nothing.
    /// Returns the screen.
    pub fn new() -> Self {
        Self { selected: 0 }
    }

    fn target(index: usize) -> Place {
        match index {
            0 => Place::NewProject,
            1 => Place::CloneRepo,
            _ => Place::AddFolder,
        }
    }
}

impl Screen for AddProject {
    fn crumbs(&self) -> Vec<String> {
        vec!["Add project".into()]
    }

    fn hints(&self, ctx: &Ctx) -> Vec<Hint> {
        let arrows = format!("{}{}", ctx.glyphs.up, ctx.glyphs.down);
        vec![
            hint("Enter", CHOICES[self.selected].0),
            hint(&arrows, "Move"),
            hint("Esc", "Back"),
        ]
    }

    fn key(&mut self, key: &Key, ctx: &mut Ctx) -> Flow {
        match key {
            Key::Up => {
                self.selected = self.selected.saturating_sub(1);
                Flow::Handled
            }
            Key::Down => {
                self.selected = (self.selected + 1).min(CHOICES.len() - 1);
                Flow::Handled
            }
            Key::Enter => Flow::Replace(open(&Self::target(self.selected), ctx)),
            Key::Char(letter) => match CHOICES.iter().position(|choice| choice.2 == *letter) {
                Some(index) => Flow::Replace(open(&Self::target(index), ctx)),
                None => Flow::Ignored,
            },
            _ => Flow::Ignored,
        }
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect, ctx: &Ctx) -> Option<Position> {
        let theme = &ctx.theme;
        let limit = (area.width as usize).saturating_sub(4).min(MENU_WIDTH);
        let mut lines = vec![styled("Add project", Role::Heading, theme), Line::default()];
        for (index, (title, description, letter)) in CHOICES.iter().enumerate() {
            lines.extend(menu_item(
                &format!("{title}  ({letter})"),
                description,
                limit,
                index == self.selected,
                theme,
                ctx.glyphs,
            ));
        }
        draw_lines(frame.buffer_mut(), area, &indent(lines, 2));
        None
    }

    fn place(&self) -> Option<Place> {
        Some(Place::AddProject)
    }
}
