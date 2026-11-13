use super::{Flow, Place, Screen};
use crate::learning::MarkStep;
use crate::tui::client::OwnerEvent;
use crate::tui::client::learning::LearningQuery;
use crate::tui::ctx::Ctx;
use crate::tui::frame::{Hint, Message, draw_lines, hint};
use crate::tui::keys::Key;
use crate::tui::seam::{Arrival, Request};
use crate::tui::text::{join, styled, wrap};
use crate::tui::theme::Role;
use crate::tui::widgets::{buttons, checkbox};
use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::text::Line;

const TEXT_WIDTH: usize = 72;
const TITLE: &str = "Add an account";
const INTRO: &str = "Agents need a model account to work. Sign in with ChatGPT or Claude, or add an API key or a local model.";
const ADD: &str = "Add account";
const SKIP: &str = "Skip for now";
const LEARN_TITLE: &str = "Learn CrazyAgent";
const LEARN: &str = "A short guide on Home walks you through accounts, projects, the master and its agents, questions and change sets. Its first step is an optional demo project.";
const LEARN_TICK: &str = "Show the guide on Home";
const SKIPPING: &str = "Skipping the guide";
const SKIP_FAILED: &str = "Couldn't skip the guide, so it stays on Home.";

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

/// The first-run Accounts step after Setup, in the desktop words: Add account (Home under
/// Add account) or Skip for now (Home), and the Learn CrazyAgent offer with its ticked box.
/// Unticked, either button first writes the skipped mark and leaves on the engine's answer,
/// saying the guide stays on Home when it refused. Esc does nothing here.
/// Input: keys and the mark reply. Output: the step, the focused button marked.
pub struct FirstRunAccounts {
    choice: Choice,
    guide: bool,
    sent: Option<(Choice, LearningQuery)>,
}

impl Default for FirstRunAccounts {
    fn default() -> Self {
        Self {
            choice: Choice::Add,
            guide: true,
            sent: None,
        }
    }
}

impl FirstRunAccounts {
    /// A new Accounts step with Add account focused and the guide ticked.
    /// Input: none. Output: the screen.
    pub fn new() -> Self {
        Self::default()
    }

    fn leave(choice: Choice) -> Flow {
        match choice {
            Choice::Add => Flow::Go(Place::AddAccount),
            Choice::Skip => Flow::Go(Place::Home),
        }
    }

    fn pick(&mut self, ctx: &mut Ctx) -> Flow {
        if self.guide {
            return Self::leave(self.choice);
        }
        let query = LearningQuery::Mark {
            step: MarkStep::Skipped,
            on: true,
            generation: ctx.next_generation(),
        };
        ctx.request(Request::Learning(Box::new(query.clone())));
        self.sent = Some((self.choice, query));
        Flow::Handled
    }
}

impl Screen for FirstRunAccounts {
    fn crumbs(&self) -> Vec<String> {
        vec!["Accounts".into()]
    }

    fn hints(&self, ctx: &Ctx) -> Vec<Hint> {
        if self.sent.is_some() {
            return vec![hint("Ctrl-Q", "Quit")];
        }
        let arrows = format!("{}{}", ctx.glyphs.left, ctx.glyphs.right);
        vec![
            hint("Enter", self.choice.label()),
            hint(&arrows, "Choose"),
            hint("Space", if self.guide { "Untick" } else { "Tick" }),
            hint("Ctrl-Q", "Quit"),
        ]
    }

    fn key(&mut self, key: &Key, ctx: &mut Ctx) -> Flow {
        if self.sent.is_some() {
            return Flow::Handled;
        }
        match key {
            Key::Left | Key::Right | Key::Tab | Key::BackTab => {
                self.choice = self.choice.other();
                Flow::Handled
            }
            Key::Enter => self.pick(ctx),
            Key::Char(' ') => {
                self.guide = !self.guide;
                Flow::Handled
            }
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
        if self.sent.is_some() {
            lines.push(ctx.loading(SKIPPING));
        } else {
            lines.push(buttons(&[ADD, SKIP], Some(focused), theme));
        }
        lines.push(Line::default());
        lines.push(styled(LEARN_TITLE, Role::Heading, theme));
        lines.extend(wrap(LEARN, limit).into_iter().map(Line::raw));
        lines.push(Line::raw(checkbox(self.guide, LEARN_TICK)));
        let lines: Vec<Line<'static>> = lines
            .into_iter()
            .map(|line| join([Line::raw("  "), line]))
            .collect();
        draw_lines(frame.buffer_mut(), area, &lines);
        None
    }

    fn arrive(&mut self, arrival: &Arrival, ctx: &mut Ctx) -> Flow {
        let Arrival::Owner(event) = arrival else {
            return Flow::Handled;
        };
        let OwnerEvent::Learning(event) = &**event else {
            return Flow::Handled;
        };
        match self.sent.take() {
            Some((choice, query)) if query == event.query => {
                if event.result.is_err() {
                    ctx.say(Message::warning(SKIP_FAILED, ""));
                }
                Self::leave(choice)
            }
            other => {
                self.sent = other;
                Flow::Handled
            }
        }
    }

    fn busy(&self, _ctx: &Ctx) -> bool {
        self.sent.is_some()
    }

    fn place(&self) -> Option<Place> {
        Some(Place::FirstRunAccounts)
    }
}

#[cfg(test)]
mod tests;
