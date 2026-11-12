use super::new_project_view::{Look, Row, wrap_words};
use crate::trust::{Origin, Trust, TrustView};
use crossterm::event::{KeyCode, KeyEvent};

/// What raising trust unlocks, shown before the owner confirms it, the same in Desktop.
pub(in crate::tui) const RAISE_WARNING: &str = "Reviewed lets this repository's settings, hooks, MCP servers and instruction files reach agents, checks, engine git and the IDE, and lets you choose Own branch or Full access. Raise it only after you have read the code.";

/// What lowering trust does to saved access and running agents, the same in Desktop.
pub(in crate::tui) const LOWERING_LINE: &str = "Untrusted lowers a saved Own branch or Full access to Sandboxed and stops agents running above it.";

const INTRO: &str = "How far you trust this repository's code and files. Untrusted locks the rules below; no level closes the gaps.";

const TEXT_WIDTH: usize = 56;

/// The Repository trust box over the Settings tab: the level row, then the rules Untrusted
/// locks and the gaps no level closes, scrolled with Up and Down. A raise asks for the
/// owner's confirm first; busy while the engine saves; a refusal keeps the choice with the
/// reason.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::tui) struct TrustEdit {
    pub view: TrustView,
    pub trust: Trust,
    pub scroll: usize,
    pub confirm_raise: bool,
    pub busy: bool,
    pub error: Option<String>,
}

/// What one key in the Repository trust box asks the screen to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::tui) enum TrustKey {
    Handled,
    Close,
    Save { trust: Trust, confirm_raise: bool },
}

impl TrustEdit {
    /// Opens the box on the saved level. Input: the engine's view. Output: the box.
    pub fn of(view: &TrustView) -> Self {
        Self {
            trust: view.trust,
            view: view.clone(),
            scroll: 0,
            confirm_raise: false,
            busy: false,
            error: None,
        }
    }

    /// Handles one key: Left, Right and Space change the level, Up and Down scroll the
    /// rules, Enter saves (a raise after the owner confirms it with y, the saved level
    /// closes), Esc closes. Input: the key. Output: what the screen does next.
    pub fn key(&mut self, key: KeyEvent) -> TrustKey {
        if self.busy {
            return match key.code {
                KeyCode::Esc => TrustKey::Close,
                _ => TrustKey::Handled,
            };
        }
        if self.confirm_raise {
            return match key.code {
                KeyCode::Char('y') => {
                    self.confirm_raise = false;
                    TrustKey::Save {
                        trust: self.trust,
                        confirm_raise: true,
                    }
                }
                KeyCode::Char('n') | KeyCode::Esc => {
                    self.confirm_raise = false;
                    TrustKey::Handled
                }
                _ => TrustKey::Handled,
            };
        }
        match key.code {
            KeyCode::Esc => return TrustKey::Close,
            KeyCode::Up => self.scroll = self.scroll.saturating_sub(1),
            KeyCode::Down => {
                let plain = ratatui::style::Style::default();
                self.scroll = (self.scroll + 1).min(self.details(plain, plain, plain).len());
            }
            KeyCode::Left | KeyCode::Right | KeyCode::Char(' ') => {
                self.error = None;
                self.trust = match self.trust {
                    Trust::Reviewed => Trust::Untrusted,
                    Trust::Untrusted => Trust::Reviewed,
                };
            }
            KeyCode::Enter if self.trust == self.view.trust => return TrustKey::Close,
            KeyCode::Enter if self.trust.raises(self.view.trust) => self.confirm_raise = true,
            KeyCode::Enter => {
                return TrustKey::Save {
                    trust: self.trust,
                    confirm_raise: false,
                };
            }
            _ => {}
        }
        TrustKey::Handled
    }

    /// The box's lines in `room` rows: what the setting is, the level row with its meaning
    /// and why the project has it, a scrolled window of the rules and gaps, and the warning,
    /// the refusal or the save line. Inputs: the look and the rows the box may use. Output:
    /// the rows.
    pub fn body(&self, look: &Look, room: usize) -> Vec<Row> {
        let mut head: Vec<Row> = wrap_words(INTRO, TEXT_WIDTH)
            .into_iter()
            .map(|line| Row::of(line, look.muted))
            .collect();
        head.push(Row::default());
        head.push(
            Row::of("> ", look.accent)
                .push("Level       ", look.muted)
                .push(format!("< {} >", self.trust.label()), look.selection),
        );
        head.extend(
            wrap_words(self.trust.description(), TEXT_WIDTH)
                .into_iter()
                .map(|line| Row::of(format!("              {line}"), look.muted)),
        );
        head.push(Row::of(
            format!("              {}", origin_line(&self.view)),
            look.muted,
        ));
        if self.trust == Trust::Untrusted && self.view.trust == Trust::Reviewed {
            head.extend(
                wrap_words(LOWERING_LINE, TEXT_WIDTH)
                    .into_iter()
                    .map(|line| Row::of(line, look.warning)),
            );
        }
        head.push(Row::default());
        let mut foot = Vec::new();
        if self.confirm_raise {
            foot.extend(
                wrap_words(&format!("Raise to Reviewed: {RAISE_WARNING}"), TEXT_WIDTH)
                    .into_iter()
                    .map(|line| Row::of(line, look.warning)),
            );
            foot.push(Row::of("[> y Raise trust ]", look.selection).push("   [ n Keep ]", look.fg));
        } else {
            foot.extend(
                self.error
                    .iter()
                    .flat_map(|error| wrap_words(error, TEXT_WIDTH))
                    .map(|line| Row::of(line, look.error)),
            );
            foot.push(if self.busy {
                Row::of("Saving...", look.running)
            } else {
                Row::of("[> Save ]", look.selection).push("   [ Cancel ]", look.fg)
            });
        }
        let details = self.details(look.fg, look.muted, look.accent);
        let shown = room.saturating_sub(head.len() + foot.len() + 3).max(3);
        let start = self.scroll.min(details.len().saturating_sub(shown));
        let end = (start + shown).min(details.len());
        let mut body = head;
        body.extend(details[start..end].iter().cloned());
        if end < details.len() {
            body.push(Row::of("  Down for more", look.muted));
        }
        body.push(Row::default());
        body.extend(foot);
        assert!(start <= end && end <= details.len());
        body
    }

    fn details(
        &self,
        fg: ratatui::style::Style,
        muted: ratatui::style::Style,
        accent: ratatui::style::Style,
    ) -> Vec<Row> {
        let mut rows = vec![Row::of("Locked while Untrusted", accent)];
        for rule in &self.view.rules {
            rows.extend(bullet(&rule.text, fg));
            rows.extend(
                wrap_words(&format!("Why: {}", rule.reason), TEXT_WIDTH - 4)
                    .into_iter()
                    .map(|line| Row::of(format!("    {line}"), muted)),
            );
        }
        rows.push(Row::default());
        rows.push(Row::of("Not covered by any level", accent));
        for gap in &self.view.gaps {
            rows.extend(bullet(&format!("{} {}", gap.id, gap.text), fg));
            rows.extend(
                wrap_words(&gap.effect, TEXT_WIDTH - 4)
                    .into_iter()
                    .map(|line| Row::of(format!("    {line}"), muted)),
            );
        }
        rows
    }
}

fn bullet(text: &str, style: ratatui::style::Style) -> Vec<Row> {
    wrap_words(text, TEXT_WIDTH - 2)
        .into_iter()
        .enumerate()
        .map(|(index, line)| Row::of(if index == 0 { "- " } else { "  " }, style).push(line, style))
        .collect()
}

/// Why the project has its level, in the owner's words. Input: the view. Output: the line.
pub(in crate::tui) fn origin_line(view: &TrustView) -> &'static str {
    match view.origin {
        Origin::Default => "The default for a project you made or added.",
        Origin::Cloned => "Set when CrazyAgent cloned this repository from a URL.",
        Origin::Owner => "You chose this level.",
    }
}

/// The Settings line for a project's repository trust: the level, or loading. Input: the
/// saved view. Output: the text.
pub(in crate::tui) fn summary(view: Option<&TrustView>) -> String {
    match view {
        None => "Loading...".to_owned(),
        Some(view) => view.trust.label().to_owned(),
    }
}
