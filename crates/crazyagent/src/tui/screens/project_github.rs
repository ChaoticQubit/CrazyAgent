use super::intake::{ask, copy, event, not_ready};
use super::project_markdown::sanitize;
use super::project_publish::{rule_label, rule_state_line};
use super::settings_github::{PAGE_WIDTH, ROWS_MIN, followed, list_room, moved};
use super::{Flow, Place, Screen};
use crate::github::{GitHubRepoRules, GitHubRule, GitHubRuleOutcome, GitHubRuleState};
use crate::protocol::ProjectId;
use crate::tui::client::intake::{IntakeQuery, IntakeView};
use crate::tui::ctx::{Ctx, Load};
use crate::tui::frame::{Hint, draw_lines, hint};
use crate::tui::keys::Key;
use crate::tui::seam::Arrival;
use crate::tui::text::{cut, join, list_row, row, styled, width, wrap};
use crate::tui::theme::Role;
use crate::tui::widgets::checkbox;
use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::text::Line;

const NOT_PUBLISHED: &str = "github_rules_not_published";

pub(in crate::tui) const RULES_NOT_PUBLISHED: &str = "GitHub rules show here for a project CrazyAgent published on GitHub, once its publish is done.";
pub(in crate::tui) const RULES_NOT_READY: &str = "Changing GitHub rules isn't in this build.";
pub(in crate::tui) const RULES_UNAVAILABLE: &str = "GitHub rules aren't in this build.";
pub(in crate::tui) const SENDING_TAIL: &str = " to GitHub. This can take a few minutes.";
pub(in crate::tui) const REPO_TAIL: &str =
    ". Each switch changes the repository on GitHub at once, with your sign-in.";

/// Cuts the selected rule's detail to the rows left for it, the last kept row ending in
/// the ellipsis when any row was cut, so the lines below it stay on screen. Inputs: the
/// detail rows with their roles, the room, the width and the ellipsis. Output: at most
/// `room` rows.
fn capped(
    mut parts: Vec<(String, Role)>,
    room: usize,
    limit: usize,
    ellipsis: &str,
) -> Vec<(String, Role)> {
    if parts.len() > room {
        parts.truncate(room);
        if let Some((text, _)) = parts.last_mut() {
            let kept = cut(text, limit.saturating_sub(width(ellipsis)), "");
            *text = format!("{kept}{ellipsis}");
        }
    }
    assert!(parts.len() <= room);
    parts
}

fn held(state: &GitHubRuleState) -> bool {
    matches!(
        state,
        GitHubRuleState::NotApplicable { .. } | GitHubRuleState::Waiting { .. }
    )
}

/// Project > Settings > GitHub: the live rules of the repository a project was published
/// to. Opening it only reads; each switch sends one change for the repository the page
/// showed, one at a time, and a failed change reads the page again since it may have
/// landed on GitHub. A rule that does not apply, or waits, never sends.
pub struct ProjectGitHub {
    project: ProjectId,
    name: String,
    read: Load<GitHubRepoRules>,
    selected: usize,
    offset: usize,
    pending: Option<IntakeQuery>,
    problem: Option<String>,
}

impl ProjectGitHub {
    /// The page before its read. Takes the project and its name. Returns the screen.
    pub fn new(project: ProjectId, name: String) -> Self {
        Self {
            project,
            name,
            read: Load::Idle,
            selected: 0,
            offset: 0,
            pending: None,
            problem: None,
        }
    }

    fn reread(&mut self, ctx: &mut Ctx) {
        assert!(self.pending.is_none());
        let query = IntakeQuery::GitHubRules {
            project: self.project.clone(),
        };
        self.pending = Some(query.clone());
        ask(ctx, query);
    }

    fn reload(&mut self, ctx: &mut Ctx) {
        self.read = Load::Loading;
        self.reread(ctx);
    }

    fn toggle(&mut self, ctx: &mut Ctx) {
        assert!(self.pending.is_none());
        let Load::Ready(page) = &self.read else {
            return;
        };
        let Some(outcome) = page.rules.get(self.selected) else {
            return;
        };
        if held(&outcome.state) {
            return;
        }
        let query = IntakeQuery::GitHubRuleSet {
            project: self.project.clone(),
            repo: page.full_name.clone(),
            rule: outcome.rule,
            on: !outcome.on,
        };
        self.problem = None;
        self.pending = Some(query.clone());
        ask(ctx, query);
    }

    fn sending(&self) -> Option<GitHubRule> {
        match &self.pending {
            Some(IntakeQuery::GitHubRuleSet { rule, .. }) => Some(*rule),
            _ => None,
        }
    }

    fn arrived(&mut self, page: &GitHubRepoRules) {
        self.read = Load::Ready(page.clone());
        self.selected = self.selected.min(page.rules.len().saturating_sub(1));
    }

    fn detail(&self, page: &GitHubRepoRules, limit: usize) -> Vec<(String, Role)> {
        let mut parts = Vec::new();
        let Some(outcome) = page.rules.get(self.selected) else {
            return parts;
        };
        if let Some(line) = rule_state_line(&outcome.state) {
            let role = match outcome.state {
                GitHubRuleState::Refused { .. } => Role::Error,
                _ => Role::Muted,
            };
            parts.push((String::new(), Role::Muted));
            parts.extend(wrap(&line, limit).into_iter().map(|text| (text, role)));
        }
        if outcome.rule == GitHubRule::RequiredChecks && !page.checks.is_empty() {
            let names: Vec<String> = page.checks.iter().map(|name| sanitize(name)).collect();
            parts.push((String::new(), Role::Muted));
            parts.extend(
                wrap(
                    &format!("Checks the first CI run named: {}.", names.join(", ")),
                    limit,
                )
                .into_iter()
                .map(|text| (text, Role::Muted)),
            );
        }
        parts
    }

    fn rows(
        &self,
        page: &GitHubRepoRules,
        limit: usize,
        room: usize,
        ctx: &Ctx,
    ) -> Vec<Line<'static>> {
        let theme = &ctx.theme;
        let word = |outcome: &GitHubRuleOutcome| match outcome.state {
            GitHubRuleState::Applied if outcome.on => styled("On", Role::Muted, theme),
            GitHubRuleState::Applied => styled("Off", Role::Muted, theme),
            GitHubRuleState::Refused { .. } => styled("Refused", Role::Error, theme),
            GitHubRuleState::NotApplicable { .. } => styled("Doesn't apply", Role::Muted, theme),
            GitHubRuleState::Waiting { .. } => styled("Waiting", Role::Muted, theme),
        };
        let mut lines: Vec<Line<'static>> = page
            .rules
            .iter()
            .enumerate()
            .skip(self.offset)
            .take(room)
            .map(|(index, outcome)| {
                list_row(
                    row(
                        limit.saturating_sub(2),
                        Line::raw(checkbox(outcome.on, rule_label(outcome.rule))),
                        word(outcome),
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
            format!("{} of {}", self.selected + 1, page.rules.len()),
            Role::Muted,
            theme,
        ));
        lines
    }
}

impl Screen for ProjectGitHub {
    fn crumbs(&self) -> Vec<String> {
        vec![self.name.clone(), "GitHub".into()]
    }

    fn hints(&self, ctx: &Ctx) -> Vec<Hint> {
        let arrows = format!("{}{}", ctx.glyphs.up, ctx.glyphs.down);
        let mut hints = Vec::new();
        match (&self.read, &self.pending) {
            (Load::Ready(page), None) => {
                if page
                    .rules
                    .get(self.selected)
                    .is_some_and(|outcome| !held(&outcome.state))
                {
                    hints.push(hint("Space", "On or off"));
                }
                hints.push(hint(&arrows, "Move"));
            }
            (Load::Ready(_), Some(_)) => hints.push(hint(&arrows, "Move")),
            (Load::Failed(code), None) if code != NOT_PUBLISHED => {
                hints.push(hint("r", "Try again"));
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
        if let Load::Ready(page) = &self.read
            && !page.rules.is_empty()
            && let Some(selected) = moved(self.selected, page.rules.len(), key)
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
            (Key::Char('r'), Load::Failed(code)) if code != NOT_PUBLISHED => self.reload(ctx),
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
            (IntakeQuery::GitHubRules { .. }, Ok(IntakeView::Rules(page))) => self.arrived(page),
            (IntakeQuery::GitHubRuleSet { .. }, Ok(IntakeView::Rules(page))) => {
                self.problem = None;
                self.arrived(page);
            }
            (IntakeQuery::GitHubRules { .. }, Err(code)) if not_ready(code) => {
                self.read = Load::NotReady;
            }
            (IntakeQuery::GitHubRules { .. }, Err(code)) => {
                self.read = Load::Failed(code.clone());
            }
            (IntakeQuery::GitHubRuleSet { .. }, result) => {
                self.problem = Some(match result {
                    Err(code) if not_ready(code) => RULES_NOT_READY.to_owned(),
                    Err(code) => copy(code),
                    Ok(_) => copy("github_reply_invalid"),
                });
                self.reread(ctx);
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
        let mut lines = vec![styled("GitHub", Role::Heading, theme), Line::default()];
        let mut below = Vec::new();
        if let Some(rule) = self.sending() {
            below.push(Line::default());
            below.extend(
                wrap(
                    &format!("Sending {}{SENDING_TAIL}", rule_label(rule)),
                    limit,
                )
                .into_iter()
                .map(|text| styled(text, Role::Muted, theme)),
            );
        }
        if let Some(problem) = &self.problem {
            below.push(Line::default());
            below.extend(
                wrap(problem, limit)
                    .into_iter()
                    .map(|text| styled(text, Role::Error, theme)),
            );
        }
        let failed = |text: &str, role: Role| -> Vec<Line<'static>> {
            wrap(text, limit)
                .into_iter()
                .map(|part| styled(part, role, theme))
                .collect()
        };
        match &self.read {
            Load::Idle | Load::Loading => lines.push(ctx.loading("Loading")),
            Load::NotReady => lines.extend(failed(RULES_UNAVAILABLE, Role::Muted)),
            Load::Failed(code) if code == NOT_PUBLISHED => {
                lines.extend(failed(RULES_NOT_PUBLISHED, Role::Muted));
            }
            Load::Failed(code) => lines.extend(failed(&copy(code), Role::Error)),
            Load::Ready(page) => {
                let visibility = if page.private { "Private" } else { "Public" };
                lines.extend(failed(
                    &format!("{}, {visibility}{REPO_TAIL}", sanitize(&page.full_name)),
                    Role::Muted,
                ));
                lines.push(Line::default());
                let count = page.rules.len();
                let list = if count == 0 {
                    0
                } else {
                    count.min(ROWS_MIN) + 2
                };
                let spare = (area.height as usize).saturating_sub(lines.len() + list + below.len());
                let detail: Vec<Line<'static>> =
                    capped(self.detail(page, limit), spare, limit, ctx.glyphs.ellipsis)
                        .into_iter()
                        .map(|(text, role)| styled(text, role, theme))
                        .collect();
                if count > 0 {
                    let around = lines.len() + detail.len() + below.len() + 2;
                    let room = list_room(area.height, around).min(count);
                    self.offset = followed(self.selected, self.offset, room, count);
                    lines.extend(self.rows(page, limit, room, ctx));
                }
                lines.extend(detail);
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
        Some(Place::ProjectGitHub {
            id: self.project.clone(),
            name: self.name.clone(),
        })
    }
}
