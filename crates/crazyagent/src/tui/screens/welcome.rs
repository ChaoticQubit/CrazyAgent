use super::profile_form::{FormKey, ProfileForm, Slot};
use super::{Flow, Place, Screen, open};
use crate::tui::ctx::{Ctx, Load};
use crate::tui::frame::{Hint, Message, draw_lines, hint};
use crate::tui::keys::Key;
use crate::tui::seam::{Arrival, Request};
use crate::tui::text::{join, pad, styled, wrap};
use crate::tui::theme::Role;
use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::text::Line;

const FORM_WIDTH: usize = 72;
const INTRO_WIDTH: usize = 38;
const INTRO_GAP: usize = 4;

/// The first-run page: asks for the owner's profile once, then opens Home.
/// Input: keys for the form, the save reply. Output: the welcome text and the
/// profile form; at 120 columns the form sits in a 72-column column.
pub struct Welcome {
    form: ProfileForm,
}

impl Welcome {
    /// A new, empty welcome form.
    /// Input: the context. Output: the screen.
    pub fn new(_ctx: &Ctx) -> Self {
        Self {
            form: ProfileForm::new(None, None),
        }
    }

    fn submit(&mut self, ctx: &mut Ctx) {
        match self.form.profile() {
            Ok(profile) => {
                self.form.saving = true;
                ctx.request(Request::SaveProfile { profile });
            }
            Err(slot) => self.form.set_focus(Some(slot)),
        }
    }
}

impl Screen for Welcome {
    fn crumbs(&self) -> Vec<String> {
        vec!["Welcome".into()]
    }

    fn hints(&self, ctx: &Ctx) -> Vec<Hint> {
        let arrows = format!("{}{}", ctx.glyphs.left, ctx.glyphs.right);
        if self.form.focus() == Some(Slot::Save) && !self.form.saving {
            return vec![
                hint("Enter", "Save and continue"),
                hint("Shift-Tab", "Previous field"),
                hint("Ctrl-Q", "Quit"),
            ];
        }
        vec![
            hint("Tab", "Next field"),
            hint("Shift-Tab", "Previous"),
            hint(&arrows, "Choose"),
            hint("Enter", "Next"),
            hint("Ctrl-Q", "Quit"),
        ]
    }

    fn typing(&self) -> bool {
        self.form.typing()
    }

    fn key(&mut self, key: &Key, ctx: &mut Ctx) -> Flow {
        match self.form.key(key) {
            FormKey::Submit => {
                self.submit(ctx);
                Flow::Handled
            }
            FormKey::Handled => Flow::Handled,
            FormKey::Ignored if matches!(key, Key::Esc) => Flow::Handled,
            FormKey::Ignored => Flow::Ignored,
        }
    }

    fn arrive(&mut self, arrival: &Arrival, ctx: &mut Ctx) -> Flow {
        match arrival {
            Arrival::ProfileSaved(Ok(())) if self.form.saving => {
                self.form.saving = false;
                if let Ok(profile) = self.form.profile() {
                    ctx.store.profile = Load::Ready(Some(profile));
                }
                ctx.say(Message::success("Saved.", "Welcome to CrazyAgent."));
                Flow::Replace(open(&Place::Home, ctx))
            }
            Arrival::ProfileSaved(Err(code)) if self.form.saving => {
                self.form.saving = false;
                let slot = match code.as_str() {
                    "owner_profile_name" => Some(Slot::Name),
                    "owner_profile_role" => Some(Slot::Role),
                    "owner_profile_bio" => Some(Slot::Bio),
                    "owner_profile_stack" => Some(Slot::Stack),
                    _ => None,
                };
                match slot {
                    Some(slot) => self.form.show_problem(slot, code),
                    None => {
                        self.form.set_focus(Some(Slot::Save));
                        ctx.say(Message::error(
                            "Could not save your profile.",
                            "Press Enter to try again.",
                        ));
                    }
                }
                Flow::Handled
            }
            _ => Flow::Handled,
        }
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect, ctx: &Ctx) -> Option<Position> {
        let theme = &ctx.theme;
        let wide = ctx.wide();
        let (lines, cursor) = if wide {
            let (form, cursor) = self.form.lines(FORM_WIDTH, true, "Save and continue", ctx);
            let form: Vec<Line<'static>> = std::iter::once(Line::default()).chain(form).collect();
            let mut intro = vec![
                Line::default(),
                styled("Welcome to CrazyAgent", Role::Heading, theme),
                Line::default(),
            ];
            intro.extend(
                wrap(
                    "Tell us a little about you. Every agent reads this before it starts, so its work fits how you work.",
                    INTRO_WIDTH - 2,
                )
                .into_iter()
                .map(Line::raw),
            );
            intro.push(Line::default());
            intro.extend(
                wrap(
                    "Only your name is required. You can change all of it later in Settings, Profile.",
                    INTRO_WIDTH - 2,
                )
                .into_iter()
                .map(Line::raw),
            );
            let rows = intro.len().max(form.len());
            let lines = (0..rows)
                .map(|index| {
                    join([
                        Line::raw("  "),
                        pad(intro.get(index).cloned().unwrap_or_default(), INTRO_WIDTH, ctx.glyphs),
                        Line::raw(" ".repeat(INTRO_GAP)),
                        form.get(index).cloned().unwrap_or_default(),
                    ])
                })
                .collect::<Vec<_>>();
            let offset = (2 + INTRO_WIDTH + INTRO_GAP) as u16;
            (lines, cursor.map(|at| Position::new(at.x + offset, at.y + 1)))
        } else {
            let limit = area.width as usize - 4;
            let (form, cursor) = self.form.lines(limit, false, "Save and continue", ctx);
            let mut lines = vec![
                styled("Welcome to CrazyAgent", Role::Heading, theme),
                Line::raw("Tell us about you. Agents read this before they start."),
                Line::default(),
            ];
            let top = lines.len() as u16;
            lines.extend(form);
            let lines = lines
                .into_iter()
                .map(|line| join([Line::raw("  "), line]))
                .collect::<Vec<_>>();
            (lines, cursor.map(|at| Position::new(at.x + 2, at.y + top)))
        };
        draw_lines(frame.buffer_mut(), area, &lines);
        cursor.map(|at| Position::new(area.x + at.x, area.y + at.y))
    }

    fn busy(&self, _ctx: &Ctx) -> bool {
        self.form.saving
    }

    fn draft(&self) -> bool {
        false
    }

    fn place(&self) -> Option<Place> {
        Some(Place::Welcome)
    }
}
