use super::intake::{NOT_READY, ask, copy, event, not_ready};
use super::project_publish::rule_label;
use super::{Flow, Place, Screen};
use crate::github::GitHubRule;
use crate::tui::client::intake::{IntakeQuery, IntakeView};
use crate::tui::ctx::{Ctx, Load};
use crate::tui::frame::{Hint, draw_lines, hint};
use crate::tui::keys::Key;
use crate::tui::seam::Arrival;
use crate::tui::text::{join, list_row, row, styled, wrap};
use crate::tui::theme::Role;
use crate::tui::widgets::checkbox;
use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::text::Line;
use std::collections::BTreeSet;

pub(in crate::tui) const PAGE_WIDTH: usize = 76;
const PAGE_STEP: usize = 10;
pub(in crate::tui) const ROWS_MIN: usize = 3;
const UNREADABLE: [&str; 3] = [
    "github_state_invalid",
    "github_config_unsafe",
    "github_config_invalid",
];

pub(in crate::tui) const DEFAULTS_TITLE: &str = "New repositories";
pub(in crate::tui) const DEFAULTS_ABOUT: &str = "The rules each project CrazyAgent publishes on GitHub starts with. A publish already started keeps its own copy.";
pub(in crate::tui) const STANDARD_LABEL: &str = "Use the standard setup";
pub(in crate::tui) const DEFAULTS_UNREADABLE: &str = "The saved defaults for new repositories can't be read safely. Use the standard setup to replace them.";
pub(in crate::tui) const DEFAULTS_UNSAVED: &str =
    "Couldn't save the defaults for new repositories. Try again.";

/// Where a list cursor lands after a move key, for a list of `count` rows.
/// Inputs: the selected row, the row count and the key. Output: the new row, or none for
/// a key that does not move.
pub(in crate::tui) fn moved(selected: usize, count: usize, key: &Key) -> Option<usize> {
    assert!(count > 0);
    let last = count - 1;
    Some(match key {
        Key::Up => selected.saturating_sub(1),
        Key::Down => (selected + 1).min(last),
        Key::Home => 0,
        Key::End => last,
        Key::PageUp => selected.saturating_sub(PAGE_STEP),
        Key::PageDown => (selected + PAGE_STEP).min(last),
        _ => return None,
    })
}

/// The first row shown so the selected one stays in a window of `room` rows.
/// Inputs: the selected row, the current first row, the room and the row count.
/// Output: the first row to show.
pub(in crate::tui) fn followed(selected: usize, offset: usize, room: usize, count: usize) -> usize {
    assert!(room > 0 && selected < count);
    let offset = if selected < offset {
        selected
    } else if selected >= offset + room {
        selected + 1 - room
    } else {
        offset
    };
    let shown = offset.min(count.saturating_sub(room));
    assert!(shown <= selected && selected < shown + room);
    shown
}

/// How many list rows fit between the lines above and below the list.
/// Inputs: the body height and the line count around the list. Output: the room, at
/// least `ROWS_MIN`.
pub(in crate::tui) fn list_room(height: u16, around: usize) -> usize {
    (height as usize).saturating_sub(around).max(ROWS_MIN)
}

fn read_problem(code: &str) -> String {
    if UNREADABLE.contains(&code) {
        DEFAULTS_UNREADABLE.to_owned()
    } else {
        copy(code)
    }
}

fn save_problem(code: &str) -> String {
    match code {
        "github_state_unavailable" => DEFAULTS_UNSAVED.to_owned(),
        code if not_ready(code) => NOT_READY.to_owned(),
        code => read_problem(code),
    }
}

/// Settings / GitHub: the rules each project CrazyAgent publishes on GitHub starts with,
/// one switch per rule of DEC-182. A switch saves the whole set, one save at a time, and
/// a failed save reads the saved set again. Nothing here reaches GitHub.
pub struct GitHubDefaults {
    read: Load<BTreeSet<GitHubRule>>,
    selected: usize,
    offset: usize,
    pending: Option<IntakeQuery>,
    problem: Option<String>,
}

impl Default for GitHubDefaults {
    fn default() -> Self {
        Self::new()
    }
}

impl GitHubDefaults {
    /// The page before its read. Takes nothing. Returns the screen.
    pub fn new() -> Self {
        Self {
            read: Load::Idle,
            selected: 0,
            offset: 0,
            pending: None,
            problem: None,
        }
    }

    fn reload(&mut self, ctx: &mut Ctx) {
        assert!(self.pending.is_none());
        self.read = Load::Loading;
        self.pending = Some(IntakeQuery::GitHubRuleDefaults);
        ask(ctx, IntakeQuery::GitHubRuleDefaults);
    }

    fn save(&mut self, rules: BTreeSet<GitHubRule>, ctx: &mut Ctx) {
        assert!(self.pending.is_none());
        self.problem = None;
        let query = IntakeQuery::GitHubRuleDefaultsSet { rules };
        self.pending = Some(query.clone());
        ask(ctx, query);
    }

    fn toggle(&mut self, ctx: &mut Ctx) {
        let Load::Ready(on) = &self.read else {
            return;
        };
        let rule = GitHubRule::ALL[self.selected];
        let mut rules = on.clone();
        if !rules.remove(&rule) {
            rules.insert(rule);
        }
        self.save(rules, ctx);
    }

    fn rows(
        &self,
        on: &BTreeSet<GitHubRule>,
        limit: usize,
        room: usize,
        ctx: &Ctx,
    ) -> Vec<Line<'static>> {
        let theme = &ctx.theme;
        let standard = GitHubRule::defaults();
        let mut lines: Vec<Line<'static>> = GitHubRule::ALL
            .iter()
            .enumerate()
            .skip(self.offset)
            .take(room)
            .map(|(index, rule)| {
                let now = on.contains(rule);
                let changed = if now == standard.contains(rule) {
                    ""
                } else {
                    "Changed  "
                };
                let state = if now { "On" } else { "Off" };
                list_row(
                    row(
                        limit.saturating_sub(2),
                        Line::raw(checkbox(now, rule_label(*rule))),
                        styled(format!("{changed}{state}"), Role::Muted, theme),
                        ctx.glyphs,
                    ),
                    limit,
                    index == self.selected,
                    true,
                    theme,
                    ctx.glyphs,
                )
            })
            .collect();
        lines.push(Line::default());
        lines.push(styled(
            format!("{} of {}", self.selected + 1, GitHubRule::ALL.len()),
            Role::Muted,
            theme,
        ));
        lines
    }
}

impl Screen for GitHubDefaults {
    fn crumbs(&self) -> Vec<String> {
        vec!["Settings".into(), "GitHub".into()]
    }

    fn hints(&self, ctx: &Ctx) -> Vec<Hint> {
        let arrows = format!("{}{}", ctx.glyphs.up, ctx.glyphs.down);
        let mut hints = Vec::new();
        match (&self.read, &self.pending) {
            (Load::Ready(_), None) => {
                hints.push(hint("Space", "On or off"));
                hints.push(hint(&arrows, "Move"));
                hints.push(hint("s", STANDARD_LABEL));
            }
            (Load::Ready(_), Some(_)) => hints.push(hint(&arrows, "Move")),
            (Load::Failed(_), None) => {
                hints.push(hint("r", "Try again"));
                hints.push(hint("s", STANDARD_LABEL));
            }
            _ => {}
        }
        hints.push(hint("Esc", "Back"));
        hints
    }

    fn wraps_hints(&self) -> bool {
        true
    }

    fn key(&mut self, key: &Key, ctx: &mut Ctx) -> Flow {
        if matches!(self.read, Load::Ready(_))
            && let Some(selected) = moved(self.selected, GitHubRule::ALL.len(), key)
        {
            self.selected = selected;
            return Flow::Handled;
        }
        if self.pending.is_some() {
            return match key {
                Key::Esc => Flow::Ignored,
                _ => Flow::Handled,
            };
        }
        match (key, &self.read) {
            (Key::Char(' ') | Key::Enter, Load::Ready(_)) => self.toggle(ctx),
            (Key::Char('s'), Load::Ready(_) | Load::Failed(_)) => {
                self.save(GitHubRule::defaults(), ctx);
            }
            (Key::Char('r'), Load::Failed(_)) => self.reload(ctx),
            _ => return Flow::Ignored,
        }
        Flow::Handled
    }

    fn arrive(&mut self, arrival: &Arrival, ctx: &mut Ctx) -> Flow {
        let Some(reply) = event(arrival) else {
            return Flow::Handled;
        };
        if self.pending.as_ref() != Some(&reply.query) {
            return Flow::Handled;
        }
        self.pending = None;
        match (&reply.query, &reply.result) {
            (
                IntakeQuery::GitHubRuleDefaults | IntakeQuery::GitHubRuleDefaultsSet { .. },
                Ok(IntakeView::RuleDefaults(rules)),
            ) => self.read = Load::Ready(rules.clone()),
            (IntakeQuery::GitHubRuleDefaults, Err(code)) if not_ready(code) => {
                self.read = Load::NotReady;
            }
            (IntakeQuery::GitHubRuleDefaults, Err(code)) => {
                self.read = Load::Failed(code.clone());
            }
            (IntakeQuery::GitHubRuleDefaultsSet { .. }, Err(code)) => {
                self.problem = Some(save_problem(code));
                self.reload(ctx);
            }
            (IntakeQuery::GitHubRuleDefaultsSet { .. }, Ok(_)) => {
                self.problem = Some(copy("github_reply_invalid"));
                self.reload(ctx);
            }
            _ => self.read = Load::Failed("github_reply_invalid".into()),
        }
        Flow::Handled
    }

    fn shown(&mut self, ctx: &mut Ctx) {
        if matches!(self.read, Load::Idle) && self.pending.is_none() {
            self.reload(ctx);
        }
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect, ctx: &Ctx) -> Option<Position> {
        let theme = &ctx.theme;
        let limit = (area.width as usize).saturating_sub(4).clamp(6, PAGE_WIDTH);
        let mut lines = vec![
            styled(DEFAULTS_TITLE, Role::Heading, theme),
            Line::default(),
        ];
        lines.extend(
            wrap(DEFAULTS_ABOUT, limit)
                .into_iter()
                .map(|text| styled(text, Role::Muted, theme)),
        );
        lines.push(Line::default());
        let mut below = Vec::new();
        if matches!(
            self.pending,
            Some(IntakeQuery::GitHubRuleDefaultsSet { .. })
        ) {
            below.push(Line::default());
            below.push(styled("Saving...", Role::Muted, theme));
        }
        if let Some(problem) = &self.problem {
            below.push(Line::default());
            below.extend(
                wrap(problem, limit)
                    .into_iter()
                    .map(|text| styled(text, Role::Error, theme)),
            );
        }
        match &self.read {
            Load::Idle | Load::Loading => lines.push(ctx.loading("Loading")),
            Load::NotReady => lines.push(styled(NOT_READY, Role::Muted, theme)),
            Load::Failed(code) => lines.extend(
                wrap(&read_problem(code), limit)
                    .into_iter()
                    .map(|text| styled(text, Role::Error, theme)),
            ),
            Load::Ready(on) => {
                let count = GitHubRule::ALL.len();
                let room = list_room(area.height, lines.len() + below.len() + 2).min(count);
                self.offset = followed(self.selected, self.offset, room, count);
                lines.extend(self.rows(on, limit, room, ctx));
            }
        }
        lines.extend(below);
        let lines: Vec<Line<'static>> = lines
            .into_iter()
            .map(|line| join([Line::raw("  "), line]))
            .collect();
        draw_lines(frame.buffer_mut(), area, &lines);
        None
    }

    fn busy(&self, _ctx: &Ctx) -> bool {
        self.pending.is_some()
    }

    fn place(&self) -> Option<Place> {
        Some(Place::GitHubDefaults)
    }
}
