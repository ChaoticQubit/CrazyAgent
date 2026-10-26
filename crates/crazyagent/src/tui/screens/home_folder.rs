use super::intake::{ask, copy, event};
use super::new_project::{complete, display_path, expand};
use super::{Flow, Place, Screen, open};
use crate::engine::owner::OwnerResult;
use crate::projects_home::PATH_BYTES_MAX;
use crate::tui::client::intake::{IntakeQuery, IntakeView};
use crate::tui::ctx::Ctx;
use crate::tui::frame::{Hint, Message, draw_lines, hint};
use crate::tui::keys::Key;
use crate::tui::seam::Arrival;
use crate::tui::text::{styled, wrap};
use crate::tui::theme::Role;
use crate::tui::widgets::{TextField, buttons};
use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::text::Line;
use std::path::Path;

const FIELD_WIDTH: usize = 72;
const SUGGESTED: &str = "~/Projects";

/// The projects folder page: where CrazyAgent keeps and watches every project.
/// First run shows it after the profile and continues to Setup; Home opens it
/// when no folder is picked yet and goes back after saving. Nothing is moved.
pub struct HomeFolder {
    first_run: bool,
    field: TextField,
    loaded: bool,
    sent: Option<String>,
    problem: Option<String>,
}

impl HomeFolder {
    /// A projects folder page with the suggested folder typed in.
    /// Takes whether this is first run and the context for the home folder.
    /// Returns the screen; the saved folder replaces the suggestion when it arrives.
    pub fn new(first_run: bool, ctx: &Ctx) -> Self {
        let mut field = TextField::new("Projects folder", PATH_BYTES_MAX);
        if ctx.home.is_some() {
            field.set(SUGGESTED);
        }
        Self {
            first_run,
            field,
            loaded: false,
            sent: None,
            problem: None,
        }
    }

    fn home(ctx: &Ctx) -> Option<&Path> {
        ctx.home.as_deref().map(Path::new)
    }

    fn submit(&mut self, ctx: &mut Ctx) {
        let typed = self.field.value().trim();
        let Some(path) = expand(typed, Self::home(ctx)) else {
            self.problem = Some("Type a full folder path, such as ~/Projects.".into());
            return;
        };
        let mut path = path.to_string_lossy().into_owned();
        while path.len() > 1 && path.ends_with('/') {
            path.pop();
        }
        if path.len() > PATH_BYTES_MAX {
            self.problem = Some("This path is too long. Pick a shorter one.".into());
            return;
        }
        self.problem = None;
        self.sent = Some(path.clone());
        let generation = ctx.next_generation();
        ask(ctx, IntakeQuery::SetHome { path, generation });
    }
}

impl Screen for HomeFolder {
    fn crumbs(&self) -> Vec<String> {
        vec!["Projects folder".into()]
    }

    fn hints(&self, _ctx: &Ctx) -> Vec<Hint> {
        let mut hints = vec![hint("Enter", "Continue"), hint("Tab", "Complete folder")];
        if self.first_run {
            hints.push(hint("Ctrl-Q", "Quit"));
        } else {
            hints.push(hint("Esc", "Back"));
        }
        hints
    }

    fn typing(&self) -> bool {
        true
    }

    fn key(&mut self, key: &Key, ctx: &mut Ctx) -> Flow {
        if self.sent.is_some() {
            return Flow::Handled;
        }
        match key {
            Key::Enter => {
                self.submit(ctx);
                Flow::Handled
            }
            Key::Tab => {
                if let Some(completed) = complete(self.field.value(), Self::home(ctx), true) {
                    self.field.set(&completed);
                }
                Flow::Handled
            }
            Key::Esc if self.first_run => Flow::Handled,
            Key::Esc => Flow::Ignored,
            other => {
                if self.field.key(other) {
                    self.problem = None;
                }
                Flow::Handled
            }
        }
    }

    fn arrive(&mut self, arrival: &Arrival, ctx: &mut Ctx) -> Flow {
        let Some(reply) = event(arrival) else {
            return Flow::Handled;
        };
        match (&reply.query, &reply.result) {
            (IntakeQuery::Home, Ok(IntakeView::Home(home))) if !self.loaded => {
                self.loaded = true;
                if let Some(path) = &home.path
                    && self.sent.is_none()
                {
                    self.field.set(&display_path(path, Self::home(ctx)));
                }
                Flow::Handled
            }
            (IntakeQuery::SetHome { path, .. }, result) if self.sent.as_ref() == Some(path) => {
                self.sent = None;
                match result {
                    Ok(IntakeView::Changed(OwnerResult::Applied)) => {
                        ctx.say(Message::success(
                            "Saved.",
                            "CrazyAgent now watches your projects folder.",
                        ));
                        if self.first_run {
                            Flow::Replace(open(&Place::Setup { first_run: true }, ctx))
                        } else {
                            Flow::Back
                        }
                    }
                    Ok(_) => {
                        self.problem = Some(copy("intake_change_outcome_unknown"));
                        Flow::Handled
                    }
                    Err(code) => {
                        self.problem = Some(copy(code));
                        Flow::Handled
                    }
                }
            }
            _ => Flow::Handled,
        }
    }

    fn shown(&mut self, ctx: &mut Ctx) {
        if !self.loaded {
            ask(ctx, IntakeQuery::Home);
        }
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect, ctx: &Ctx) -> Option<Position> {
        let theme = &ctx.theme;
        let limit = (area.width as usize)
            .saturating_sub(4)
            .clamp(6, FIELD_WIDTH);
        let mut lines = vec![
            styled("Your projects folder", Role::Heading, theme),
            Line::default(),
        ];
        lines.extend(
            wrap(
                "CrazyAgent keeps every project in one folder. It watches that folder and lists what it finds on Home. Nothing is moved.",
                limit,
            )
            .into_iter()
            .map(Line::raw),
        );
        lines.push(Line::default());
        let top = lines.len() as u16;
        let (field, cursor) = self
            .field
            .lines(limit, self.sent.is_none(), theme, ctx.glyphs);
        lines.extend(field);
        lines.push(styled("Tab completes a folder name.", Role::Muted, theme));
        lines.push(Line::default());
        match (&self.sent, &self.problem) {
            (Some(_), _) => lines.push(ctx.loading("Saving")),
            (None, Some(problem)) => {
                lines.extend(
                    wrap(problem, limit)
                        .into_iter()
                        .map(|text| styled(text, Role::Error, theme)),
                );
                lines.push(Line::default());
                lines.push(buttons(&["Continue"], Some(0), theme));
            }
            (None, None) => lines.push(buttons(&["Continue"], Some(0), theme)),
        }
        let lines: Vec<Line<'static>> = lines
            .into_iter()
            .map(|line| crate::tui::text::join([Line::raw("  "), line]))
            .collect();
        draw_lines(frame.buffer_mut(), area, &lines);
        cursor
            .filter(|_| self.sent.is_none())
            .map(|at| Position::new(area.x + 2 + at.x, area.y + top + at.y))
    }

    fn busy(&self, _ctx: &Ctx) -> bool {
        self.sent.is_some()
    }

    fn place(&self) -> Option<Place> {
        Some(Place::HomeFolder {
            first_run: self.first_run,
        })
    }
}
