use super::{draw_page, step};
use crate::tui::ctx::{Ctx, Load};
use crate::tui::frame::{Hint, Message, hint, sections_hint};
use crate::tui::keys::Key;
use crate::tui::prefs;
use crate::tui::screens::profile_form::{FormKey, ProfileForm, Slot};
use crate::tui::screens::{Flow, Place, Screen, SettingsSection};
use crate::tui::seam::{Arrival, Request};
use crate::tui::text::styled;
use crate::tui::theme::Role;
use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::text::{Line, Span};

const FORM_WIDTH: usize = 72;

/// Settings, Profile: the welcome form prefilled, plus the Colors choice.
/// Input: keys and the profile arrivals. Output: the form; Save changes saves
/// the profile in the engine and the Colors choice on this Mac.
pub struct ProfilePage {
    form: Option<ProfileForm>,
}

impl ProfilePage {
    /// The page, prefilled when the profile is already loaded.
    /// Input: the context. Output: the screen.
    pub fn new(ctx: &mut Ctx) -> Self {
        let form = match &ctx.store.profile {
            Load::Ready(profile) => Some(Self::form(profile.as_ref(), ctx)),
            _ => None,
        };
        Self { form }
    }

    fn form(profile: Option<&crate::owner_profile::OwnerProfile>, ctx: &Ctx) -> ProfileForm {
        let mut form = ProfileForm::new(profile, Some(ctx.colors));
        form.set_focus(None);
        form
    }
}

impl Screen for ProfilePage {
    fn crumbs(&self) -> Vec<String> {
        vec!["Settings".into(), "Profile".into()]
    }

    fn hints(&self, ctx: &Ctx) -> Vec<Hint> {
        if self.form.as_ref().and_then(ProfileForm::focus) == Some(Slot::Save) {
            return vec![
                hint("Enter", "Save changes"),
                hint("Shift-Tab", "Previous field"),
                sections_hint(ctx.glyphs),
                hint("Esc", "Home"),
            ];
        }
        vec![
            hint("Tab", "Next field"),
            hint("Enter", "Next"),
            sections_hint(ctx.glyphs),
            hint("Esc", "Home"),
        ]
    }

    fn typing(&self) -> bool {
        self.form.as_ref().is_some_and(ProfileForm::typing)
    }

    fn key(&mut self, key: &Key, ctx: &mut Ctx) -> Flow {
        let Some(form) = &mut self.form else {
            return Flow::Ignored;
        };
        match form.key(key) {
            FormKey::Submit => {
                match form.profile() {
                    Ok(profile) => {
                        form.saving = true;
                        ctx.request(Request::SaveProfile { profile });
                    }
                    Err(slot) => form.set_focus(Some(slot)),
                }
                Flow::Handled
            }
            FormKey::Handled => Flow::Handled,
            FormKey::Ignored => Flow::Ignored,
        }
    }

    fn step(&mut self, forward: bool, ctx: &mut Ctx) -> Flow {
        step(SettingsSection::Profile, forward, ctx)
    }

    fn arrive(&mut self, arrival: &Arrival, ctx: &mut Ctx) -> Flow {
        match arrival {
            Arrival::Profile(Ok(profile)) if self.form.is_none() => {
                self.form = Some(Self::form(profile.as_ref(), ctx));
            }
            Arrival::ProfileSaved(result) => {
                let Some(form) = &mut self.form else {
                    return Flow::Handled;
                };
                if !form.saving {
                    return Flow::Handled;
                }
                form.saving = false;
                match result {
                    Ok(()) => {
                        if let Ok(profile) = form.profile() {
                            ctx.store.profile = Load::Ready(Some(profile));
                        }
                        form.settle();
                        let colors = form.colors().unwrap_or_default();
                        let saved_colors = match &ctx.prefs {
                            Some(path) => prefs::save_colors(path, colors),
                            None => Ok(()),
                        };
                        ctx.restyle(colors, ctx.appearance);
                        match saved_colors {
                            Ok(()) => ctx.say(Message::success("Saved.", "")),
                            Err(_) => ctx.say(Message::warning(
                                "Profile saved, but the color choice could not be kept on this Mac.",
                                "",
                            )),
                        }
                    }
                    Err(code) => {
                        let slot = match code.as_str() {
                            "owner_profile_name" => Some(Slot::Name),
                            "owner_profile_role" => Some(Slot::Role),
                            "owner_profile_bio" => Some(Slot::Bio),
                            "owner_profile_stack" => Some(Slot::Stack),
                            _ => None,
                        };
                        match slot {
                            Some(slot) => form.show_problem(slot, code),
                            None => ctx.say(Message::error(
                                "Could not save your profile.",
                                "Press Enter to try again.",
                            )),
                        }
                    }
                }
            }
            _ => {}
        }
        Flow::Handled
    }

    fn shown(&mut self, ctx: &mut Ctx) {
        if self.form.is_none() {
            ctx.request(Request::Profile);
        }
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect, ctx: &Ctx) -> Option<Position> {
        let wide = ctx.wide();
        let (content, cursor) = match &self.form {
            Some(form) => {
                let limit = if wide {
                    FORM_WIDTH
                } else {
                    area.width as usize - 4
                };
                let (lines, cursor) = form.lines(limit, wide, "Save changes", ctx);
                if wide {
                    let mut content = vec![
                        styled("Profile", Role::Accent, &ctx.theme),
                        styled(
                            "Agents read this before they start.",
                            Role::Muted,
                            &ctx.theme,
                        ),
                        Line::default(),
                    ];
                    content.extend(lines);
                    (content, cursor.map(|at| Position::new(at.x, at.y + 3)))
                } else {
                    (lines, cursor)
                }
            }
            None => (
                vec![
                    styled("Profile", Role::Accent, &ctx.theme),
                    Line::default(),
                    Line::from(vec![
                        Span::styled(ctx.spinner(), ctx.theme.running),
                        Span::raw(" Loading your profile"),
                    ]),
                ],
                None,
            ),
        };
        draw_page(frame, area, SettingsSection::Profile, content, cursor, ctx)
    }

    fn busy(&self, _ctx: &Ctx) -> bool {
        self.form.as_ref().is_none_or(|form| form.saving)
    }

    fn draft(&self) -> bool {
        self.form.as_ref().is_some_and(ProfileForm::edited)
    }

    fn loss(&self) -> &'static str {
        "Your profile changes will be lost."
    }

    fn place(&self) -> Option<Place> {
        Some(Place::Settings(SettingsSection::Profile))
    }
}
