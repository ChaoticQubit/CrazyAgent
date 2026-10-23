use crate::owner_profile::{
    AnswerDetail, BIO_BYTES_MAX, Initiative, NAME_BYTES_MAX, OwnerProfile, ROLE_BYTES_MAX,
    WorkingStyle, plain_problem,
};
use crate::tui::ctx::Ctx;
use crate::tui::keys::Key;
use crate::tui::text::{join, pad};
use crate::tui::theme::Colors;
use crate::tui::widgets::{TextField, buttons, radio};
use ratatui::layout::Position;
use ratatui::text::{Line, Span};

const STACK_CHARS_MAX: usize = 520;
const CHOICE_WIDTH: usize = 16;
const CHOICE_LABEL_WIDTH: usize = 22;

/// One focusable part of the profile form, in Tab order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Slot {
    Name,
    Role,
    Bio,
    Stack,
    Detail,
    Initiative,
    Colors,
    Save,
}

/// What a key did to the form.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FormKey {
    Handled,
    Ignored,
    Submit,
}

/// The raw values of the form, compared to tell edited from saved.
type Snapshot = ([String; 4], AnswerDetail, Initiative, Option<Colors>);

/// The profile form shared by the first-run Welcome page and Settings, Profile.
/// Input: an optional saved profile and, in Settings, the Colors choice.
/// Output: drawn lines with the cursor, and a validated profile on submit.
#[derive(Debug)]
pub struct ProfileForm {
    baseline: Snapshot,
    name: TextField,
    role: TextField,
    bio: TextField,
    stack: TextField,
    detail: AnswerDetail,
    initiative: Initiative,
    colors: Option<Colors>,
    focus: Option<Slot>,
    pub saving: bool,
}

impl ProfileForm {
    /// Builds the form, prefilled from a saved profile when there is one.
    /// Inputs: the profile and the Colors choice for Settings. Output: the form
    /// with Name focused.
    pub fn new(profile: Option<&OwnerProfile>, colors: Option<Colors>) -> Self {
        let mut form = Self {
            baseline: Default::default(),
            name: TextField::new("Name", NAME_BYTES_MAX).right("Required"),
            role: TextField::new("Role", ROLE_BYTES_MAX)
                .placeholder("For example: Founder and full-stack developer"),
            bio: TextField::new("Short bio", BIO_BYTES_MAX)
                .multiline()
                .placeholder("A sentence or two agents should know about you"),
            stack: TextField::new("Preferred stack", STACK_CHARS_MAX)
                .placeholder("For example: TypeScript, React, Postgres"),
            detail: AnswerDetail::Terse,
            initiative: Initiative::AskFirst,
            colors,
            focus: Some(Slot::Name),
            saving: false,
        };
        if let Some(profile) = profile {
            form.name.set(&profile.name);
            form.role.set(&profile.role);
            form.bio.set(&profile.bio);
            form.stack.set(&profile.preferred_stack.join(", "));
            form.detail = profile.working_style.detail;
            form.initiative = profile.working_style.initiative;
        }
        form.baseline = form.snapshot();
        form
    }

    fn snapshot(&self) -> Snapshot {
        (
            [&self.name, &self.role, &self.bio, &self.stack].map(|field| field.value().to_owned()),
            self.detail,
            self.initiative,
            self.colors,
        )
    }

    /// True when any value differs from the one the form was built or last
    /// saved with.
    /// Input: none. Output: whether leaving would lose edits.
    pub fn edited(&self) -> bool {
        self.snapshot() != self.baseline
    }

    /// Takes the current values as saved, after the engine kept them.
    /// Input: none. Output: none; `edited` is false until the next change.
    pub fn settle(&mut self) {
        self.baseline = self.snapshot();
        assert!(!self.edited());
    }

    /// The Tab order of this form.
    /// Input: none. Output: slots; Colors only in Settings.
    pub fn slots(&self) -> Vec<Slot> {
        let mut slots = vec![
            Slot::Name,
            Slot::Role,
            Slot::Bio,
            Slot::Stack,
            Slot::Detail,
            Slot::Initiative,
        ];
        if self.colors.is_some() {
            slots.push(Slot::Colors);
        }
        slots.push(Slot::Save);
        slots
    }

    /// The focused slot.
    /// Input: none. Output: the slot, or None after Esc left a field.
    pub fn focus(&self) -> Option<Slot> {
        self.focus
    }

    /// Moves focus to one slot.
    /// Input: the slot. Output: none.
    pub fn set_focus(&mut self, slot: Option<Slot>) {
        self.focus = slot;
    }

    /// The chosen Colors value in Settings.
    /// Input: none. Output: the choice, or None on Welcome.
    pub fn colors(&self) -> Option<Colors> {
        self.colors
    }

    /// True while a text field has focus.
    /// Input: none. Output: whether letters type.
    pub fn typing(&self) -> bool {
        matches!(
            self.focus,
            Some(Slot::Name | Slot::Role | Slot::Bio | Slot::Stack)
        )
    }

    fn field_mut(&mut self, slot: Slot) -> Option<&mut TextField> {
        match slot {
            Slot::Name => Some(&mut self.name),
            Slot::Role => Some(&mut self.role),
            Slot::Bio => Some(&mut self.bio),
            Slot::Stack => Some(&mut self.stack),
            _ => None,
        }
    }

    fn move_focus(&mut self, forward: bool) {
        let slots = self.slots();
        if let Some(current) = self.focus {
            self.check(current);
        }
        let next = match self
            .focus
            .and_then(|slot| slots.iter().position(|s| *s == slot))
        {
            Some(index) if forward => slots[(index + 1) % slots.len()],
            Some(index) => slots[(index + slots.len() - 1) % slots.len()],
            None => slots[0],
        };
        self.focus = Some(next);
    }

    fn check(&mut self, slot: Slot) {
        if slot == Slot::Name {
            self.name.error = self
                .name
                .value()
                .trim()
                .is_empty()
                .then(|| "Enter your name to continue.".to_owned());
        }
    }

    fn stack_items(&self) -> Vec<String> {
        let mut items: Vec<String> = Vec::new();
        for item in self.stack.value().split(',') {
            let item = item.trim();
            if !item.is_empty() && !items.iter().any(|known| known == item) {
                items.push(item.to_owned());
            }
        }
        items
    }

    /// Validates the form and builds the profile.
    /// Input: none. Output: the profile, or the slot to focus with its error
    /// shown under the field.
    pub fn profile(&mut self) -> Result<OwnerProfile, Slot> {
        self.check(Slot::Name);
        if self.name.error.is_some() {
            return Err(Slot::Name);
        }
        let profile = OwnerProfile {
            name: self.name.value().trim().to_owned(),
            role: self.role.value().trim().to_owned(),
            bio: self.bio.value().trim().to_owned(),
            preferred_stack: self.stack_items(),
            working_style: WorkingStyle {
                detail: self.detail,
                initiative: self.initiative,
            },
        };
        if let Err(problem) = profile.validate() {
            let slot = match problem.0.as_str() {
                "owner_profile_name" => Slot::Name,
                "owner_profile_role" => Slot::Role,
                "owner_profile_bio" => Slot::Bio,
                _ => Slot::Stack,
            };
            self.show_problem(slot, &problem.0);
            return Err(slot);
        }
        Ok(profile)
    }

    /// Shows an engine or local refusal under the field it names.
    /// Inputs: the slot and the refusal code. Output: none.
    pub fn show_problem(&mut self, slot: Slot, code: &str) {
        let sentence = plain_problem(code).unwrap_or("Check this field and try again.");
        if let Some(field) = self.field_mut(slot) {
            field.error = Some(sentence.to_owned());
        }
        self.focus = Some(slot);
    }

    /// Handles one key.
    /// Input: the key. Output: handled, ignored (for the screen to use) or
    /// submit when Enter is pressed on the button.
    pub fn key(&mut self, key: &Key) -> FormKey {
        if self.saving {
            return FormKey::Handled;
        }
        match key {
            Key::Tab => {
                self.move_focus(true);
                return FormKey::Handled;
            }
            Key::BackTab => {
                self.move_focus(false);
                return FormKey::Handled;
            }
            _ => {}
        }
        let Some(slot) = self.focus else {
            return FormKey::Ignored;
        };
        if let Some(field) = self.field_mut(slot) {
            if matches!(key, Key::Esc) {
                self.check(slot);
                self.focus = None;
                return FormKey::Handled;
            }
            if matches!(key, Key::Enter) {
                self.move_focus(true);
                return FormKey::Handled;
            }
            if field.key(key) {
                if slot == Slot::Name && !self.name.value().trim().is_empty() {
                    self.name.error = None;
                }
                return FormKey::Handled;
            }
            return FormKey::Ignored;
        }
        match (slot, key) {
            (Slot::Save, Key::Enter) => FormKey::Submit,
            (_, Key::Enter) => {
                self.move_focus(true);
                FormKey::Handled
            }
            (Slot::Detail, Key::Left | Key::Right) => {
                self.detail = match self.detail {
                    AnswerDetail::Terse => AnswerDetail::Detailed,
                    AnswerDetail::Detailed => AnswerDetail::Terse,
                };
                FormKey::Handled
            }
            (Slot::Initiative, Key::Left | Key::Right) => {
                self.initiative = match self.initiative {
                    Initiative::AskFirst => Initiative::ActFirst,
                    Initiative::ActFirst => Initiative::AskFirst,
                };
                FormKey::Handled
            }
            (Slot::Colors, Key::Left | Key::Right) => {
                let current = self.colors.unwrap_or_default();
                let index = Colors::ALL
                    .iter()
                    .position(|(choice, _)| *choice == current)
                    .expect("every choice is listed");
                let count = Colors::ALL.len();
                let next = if matches!(key, Key::Right) {
                    (index + 1) % count
                } else {
                    (index + count - 1) % count
                };
                self.colors = Some(Colors::ALL[next].0);
                FormKey::Handled
            }
            (_, Key::Esc) => {
                self.focus = None;
                FormKey::Handled
            }
            _ => FormKey::Ignored,
        }
    }

    fn choice_row(
        &self,
        label: &str,
        options: [(bool, &str); 2],
        focused: bool,
        ctx: &Ctx,
    ) -> Line<'static> {
        let label_style = if focused {
            ctx.theme.accent
        } else {
            ctx.theme.text
        };
        join([
            pad(
                Line::from(Span::styled(label.to_owned(), label_style)),
                CHOICE_LABEL_WIDTH,
                ctx.glyphs,
            ),
            pad(
                Line::raw(radio(options[0].0, options[0].1)),
                CHOICE_WIDTH,
                ctx.glyphs,
            ),
            Line::raw(radio(options[1].0, options[1].1)),
        ])
    }

    /// Draws the form.
    /// Inputs: field width, whether to leave a blank line between fields, the
    /// button label and the context. Output: lines and the cursor position
    /// relative to the first line.
    pub fn lines(
        &self,
        limit: usize,
        spaced: bool,
        button: &str,
        ctx: &Ctx,
    ) -> (Vec<Line<'static>>, Option<Position>) {
        let mut out: Vec<Line<'static>> = Vec::new();
        let mut cursor = None;
        for (slot, field) in [
            (Slot::Name, &self.name),
            (Slot::Role, &self.role),
            (Slot::Bio, &self.bio),
            (Slot::Stack, &self.stack),
        ] {
            let focused = self.focus == Some(slot) && !self.saving;
            let (lines, at) = field.lines(limit, focused, &ctx.theme, ctx.glyphs);
            if let Some(at) = at {
                cursor = Some(Position::new(at.x, at.y + out.len() as u16));
            }
            out.extend(lines);
            if spaced {
                out.push(Line::default());
            }
        }
        out.push(Line::from(Span::styled("Working style", ctx.theme.heading)));
        out.push(self.choice_row(
            "Replies",
            [
                (self.detail == AnswerDetail::Terse, "Short"),
                (self.detail == AnswerDetail::Detailed, "Detailed"),
            ],
            self.focus == Some(Slot::Detail),
            ctx,
        ));
        out.push(self.choice_row(
            "Before big changes",
            [
                (self.initiative == Initiative::AskFirst, "Ask first"),
                (self.initiative == Initiative::ActFirst, "Act first"),
            ],
            self.focus == Some(Slot::Initiative),
            ctx,
        ));
        out.push(Line::default());
        if let Some(colors) = self.colors {
            let focused = self.focus == Some(Slot::Colors);
            out.push(Line::from(Span::styled(
                "Colors",
                if focused {
                    ctx.theme.accent
                } else {
                    ctx.theme.heading
                },
            )));
            out.push(Line::raw(
                Colors::ALL
                    .iter()
                    .map(|(choice, label)| radio(*choice == colors, label))
                    .collect::<Vec<_>>()
                    .join("   "),
            ));
            out.push(Line::default());
        }
        if self.saving {
            out.push(ctx.loading("Saving your profile"));
        } else {
            out.push(buttons(
                &[button],
                (self.focus == Some(Slot::Save)).then_some(0),
                &ctx.theme,
            ));
        }
        (out, cursor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn typed(form: &mut ProfileForm, text: &str) {
        for character in text.chars() {
            assert_eq!(form.key(&Key::Char(character)), FormKey::Handled);
        }
    }

    #[test]
    fn name_is_required_on_leave_and_on_submit() {
        let mut form = ProfileForm::new(None, None);
        assert!(form.typing());
        assert_eq!(form.key(&Key::Tab), FormKey::Handled);
        assert_eq!(form.focus(), Some(Slot::Role));
        assert_eq!(
            form.name.error.as_deref(),
            Some("Enter your name to continue.")
        );
        for _ in 0..5 {
            form.key(&Key::Tab);
        }
        assert_eq!(form.focus(), Some(Slot::Save));
        assert_eq!(form.key(&Key::Enter), FormKey::Submit);
        assert_eq!(form.profile(), Err(Slot::Name));
        form.set_focus(Some(Slot::Name));
        typed(&mut form, "  Mira Okafor ");
        assert_eq!(form.name.error, None, "typing a name clears the error");
        let profile = form.profile().unwrap();
        assert_eq!(profile.name, "Mira Okafor");
        assert_eq!(profile.working_style.detail, AnswerDetail::Terse);
        assert_eq!(profile.working_style.initiative, Initiative::AskFirst);
    }

    #[test]
    fn enter_moves_on_and_choices_toggle_with_arrows() {
        let mut form = ProfileForm::new(None, Some(Colors::FollowMacos));
        typed(&mut form, "Mira");
        form.key(&Key::Enter);
        typed(&mut form, "Founder");
        form.key(&Key::Enter);
        typed(&mut form, "Ten years of TypeScript.");
        assert_eq!(form.key(&Key::Ctrl('j')), FormKey::Handled);
        typed(&mut form, "Learning Rust.");
        form.key(&Key::Enter);
        typed(&mut form, "TypeScript, React,, Rust, React");
        form.key(&Key::Enter);
        assert!(!form.typing());
        assert_eq!(form.key(&Key::Right), FormKey::Handled);
        form.key(&Key::Enter);
        assert_eq!(form.key(&Key::Left), FormKey::Handled);
        form.key(&Key::Enter);
        assert_eq!(form.focus(), Some(Slot::Colors));
        form.key(&Key::Left);
        assert_eq!(form.colors(), Some(Colors::Terminal));
        form.key(&Key::Right);
        form.key(&Key::Right);
        assert_eq!(form.colors(), Some(Colors::Dark));
        let profile = form.profile().unwrap();
        assert_eq!(profile.bio, "Ten years of TypeScript.\nLearning Rust.");
        assert_eq!(profile.preferred_stack, ["TypeScript", "React", "Rust"]);
        assert_eq!(profile.working_style.detail, AnswerDetail::Detailed);
        assert_eq!(profile.working_style.initiative, Initiative::ActFirst);
        assert_eq!(form.key(&Key::Char('x')), FormKey::Ignored);
    }

    #[test]
    fn esc_leaves_the_field_and_letters_stop_typing() {
        let mut form = ProfileForm::new(None, None);
        typed(&mut form, "Mira");
        assert_eq!(form.key(&Key::Esc), FormKey::Handled);
        assert!(!form.typing());
        assert_eq!(form.key(&Key::Char('n')), FormKey::Ignored);
        assert_eq!(form.key(&Key::Esc), FormKey::Ignored);
        assert_eq!(form.key(&Key::Tab), FormKey::Handled);
        assert_eq!(form.focus(), Some(Slot::Name));
    }

    #[test]
    fn engine_limits_show_under_the_field() {
        let mut form = ProfileForm::new(None, None);
        typed(&mut form, "Mira");
        form.stack.set(
            &(0..13)
                .map(|index| format!("tech{index}"))
                .collect::<Vec<_>>()
                .join(","),
        );
        assert_eq!(form.profile(), Err(Slot::Stack));
        assert_eq!(
            form.stack.error.as_deref(),
            Some("List up to 12 different technologies, each up to 40 characters.")
        );
        assert_eq!(form.focus(), Some(Slot::Stack));
    }

    #[test]
    fn saved_profiles_prefill_the_form() {
        let saved = OwnerProfile {
            name: "Mira Okafor".into(),
            role: "Founder".into(),
            bio: String::new(),
            preferred_stack: vec!["Rust".into(), "Postgres".into()],
            working_style: WorkingStyle {
                detail: AnswerDetail::Detailed,
                initiative: Initiative::AskFirst,
            },
        };
        let mut form = ProfileForm::new(Some(&saved), Some(Colors::Light));
        assert_eq!(form.profile().unwrap(), saved);
        assert_eq!(form.colors(), Some(Colors::Light));
    }
}
