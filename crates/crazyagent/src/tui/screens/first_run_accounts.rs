use super::{Flow, Place, Screen, open};
use crate::tui::ctx::Ctx;
use crate::tui::frame::{Hint, draw_lines, hint};
use crate::tui::keys::Key;
use crate::tui::text::{join, styled, wrap};
use crate::tui::theme::Role;
use crate::tui::widgets::buttons;
use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::text::Line;

const TEXT_WIDTH: usize = 72;
const TITLE: &str = "Add an account";
const INTRO: &str = "Agents need a model account to work. Sign in with ChatGPT or Claude, or add an API key or a local model.";
const ADD: &str = "Add account";
const SKIP: &str = "Skip for now";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Choice {
    #[default]
    Add,
    Skip,
}

impl Choice {
    fn label(self) -> &'static str {
        match self {
            Self::Add => ADD,
            Self::Skip => SKIP,
        }
    }

    fn other(self) -> Self {
        match self {
            Self::Add => Self::Skip,
            Self::Skip => Self::Add,
        }
    }
}

/// The first-run Accounts step, after Setup: Add account or Skip for now, the
/// same words as the desktop first run. Add account ends first run with Home
/// under Add account; Skip for now ends it on Home. Esc does nothing here.
/// Input: keys. Output: the step, the focused button marked.
#[derive(Default)]
pub struct FirstRunAccounts {
    choice: Choice,
}

impl FirstRunAccounts {
    /// A new Accounts step with Add account focused.
    /// Input: none. Output: the screen.
    pub fn new() -> Self {
        Self::default()
    }
}

impl Screen for FirstRunAccounts {
    fn crumbs(&self) -> Vec<String> {
        vec!["Accounts".into()]
    }

    fn hints(&self, ctx: &Ctx) -> Vec<Hint> {
        let arrows = format!("{}{}", ctx.glyphs.left, ctx.glyphs.right);
        vec![
            hint("Enter", self.choice.label()),
            hint(&arrows, "Choose"),
            hint("Ctrl-Q", "Quit"),
        ]
    }

    fn key(&mut self, key: &Key, ctx: &mut Ctx) -> Flow {
        match key {
            Key::Left | Key::Right | Key::Tab | Key::BackTab => {
                self.choice = self.choice.other();
                Flow::Handled
            }
            Key::Enter => match self.choice {
                Choice::Add => Flow::Go(Place::AddAccount),
                Choice::Skip => Flow::Replace(open(&Place::Home, ctx)),
            },
            Key::Esc => Flow::Handled,
            _ => Flow::Ignored,
        }
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect, ctx: &Ctx) -> Option<Position> {
        let theme = &ctx.theme;
        let limit = (area.width as usize).saturating_sub(4).clamp(6, TEXT_WIDTH);
        let mut lines = vec![styled(TITLE, Role::Heading, theme), Line::default()];
        lines.extend(wrap(INTRO, limit).into_iter().map(Line::raw));
        lines.push(Line::default());
        let focused = match self.choice {
            Choice::Add => 0,
            Choice::Skip => 1,
        };
        lines.push(buttons(&[ADD, SKIP], Some(focused), theme));
        let lines: Vec<Line<'static>> = lines
            .into_iter()
            .map(|line| join([Line::raw("  "), line]))
            .collect();
        draw_lines(frame.buffer_mut(), area, &lines);
        None
    }

    fn place(&self) -> Option<Place> {
        Some(Place::FirstRunAccounts)
    }
}

#[cfg(test)]
mod tests;
