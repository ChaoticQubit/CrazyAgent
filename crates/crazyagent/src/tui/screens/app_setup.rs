use super::{Flow, Place, Screen, open};
use crate::tui::ctx::Ctx;
use crate::tui::frame::{Hint, draw_lines, hint};
use crate::tui::keys::Key;
use crate::tui::text::{indent, styled, wrap};
use crate::tui::theme::Role;
use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::text::Line;

#[cfg(test)]
mod tests;

const NOT_READY: &str =
    "Not ready yet. Setup comes with the setup and default skills and plugins update.";

/// The setup page: the tools CrazyAgent needs on this machine and the default
/// skills and plugins pack. First run shows it after the projects home folder
/// and continues to Home; Settings shows it with the projects home folder and
/// the GitHub account.
pub struct AppSetup {
    first_run: bool,
}

impl AppSetup {
    /// A setup page.
    /// Input: true during first run. Output: the screen.
    pub fn new(first_run: bool) -> Self {
        Self { first_run }
    }
}

impl Screen for AppSetup {
    fn crumbs(&self) -> Vec<String> {
        if self.first_run {
            vec!["Setup".into()]
        } else {
            vec!["Settings".into(), "Setup".into()]
        }
    }

    fn hints(&self, _ctx: &Ctx) -> Vec<Hint> {
        if self.first_run {
            vec![hint("Enter", "Continue"), hint("Ctrl-Q", "Quit")]
        } else {
            vec![hint("Esc", "Back")]
        }
    }

    fn key(&mut self, key: &Key, ctx: &mut Ctx) -> Flow {
        match key {
            Key::Enter if self.first_run => Flow::Replace(open(&Place::Home, ctx)),
            _ => Flow::Ignored,
        }
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect, ctx: &Ctx) -> Option<Position> {
        let limit = (area.width as usize).saturating_sub(4).max(1);
        let mut lines = vec![styled("Setup", Role::Heading, &ctx.theme), Line::default()];
        lines.extend(
            wrap(NOT_READY, limit)
                .into_iter()
                .map(|text| styled(text, Role::Muted, &ctx.theme)),
        );
        draw_lines(frame.buffer_mut(), area, &indent(lines, 2));
        None
    }

    fn place(&self) -> Option<Place> {
        Some(Place::Setup {
            first_run: self.first_run,
        })
    }
}
