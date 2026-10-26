use super::intake::{NOT_READY, ask, copy, event, not_ready};
use super::new_project::size_label;
use super::{Flow, Place, Screen, open};
use crate::engine::owner::AccountSecret;
use crate::github::{GitHubDeviceCode, GitHubRepo, GitHubStatus};
use crate::tui::client::intake::{IntakeJob, IntakeQuery, IntakeView};
use crate::tui::ctx::{Ctx, Load};
use crate::tui::frame::{Hint, Message, draw_lines, hint};
use crate::tui::keys::Key;
use crate::tui::seam::{Arrival, Request};
use crate::tui::text::{join, list_row, row, styled, wrap};
use crate::tui::theme::Role;
use crate::tui::widgets::{TextField, menu_item};
use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::text::Line;

const PAGE_WIDTH: usize = 76;
const TOKEN_CHARS_MAX: usize = 255;
const REPOS_LOADED_MAX: usize = 2_000;

const WAYS: [(&str, &str); 3] = [
    (
        "Sign in with GitHub",
        "CrazyAgent shows a code. Enter it on github.com and come back.",
    ),
    (
        "Paste a token",
        "Use a GitHub personal access token that can read your repositories.",
    ),
    (
        "Use the GitHub CLI",
        "Reuse the sign-in of the gh command on this computer.",
    ),
];

/// The Clone from GitHub page: signs in to GitHub when needed, lists the
/// owner's repositories a page at a time and clones one into the projects
/// folder, then opens its setup. A pasted token goes to the engine once, in its
/// own secret job, and the field is wiped.
pub struct CloneRepo {
    status: Load<GitHubStatus>,
    way: usize,
    device: Option<GitHubDeviceCode>,
    token: Option<TextField>,
    repos: Vec<GitHubRepo>,
    next: Option<u32>,
    listing: Load<()>,
    selected: usize,
    offset: usize,
    pending: Option<IntakeQuery>,
    problem: Option<String>,
}

impl Default for CloneRepo {
    fn default() -> Self {
        Self::new()
    }
}

impl CloneRepo {
    /// A Clone from GitHub page that asks the engine for the GitHub status first.
    /// Takes nothing.
    /// Returns the screen.
    pub fn new() -> Self {
        Self {
            status: Load::Idle,
            way: 0,
            device: None,
            token: None,
            repos: Vec::new(),
            next: None,
            listing: Load::Idle,
            selected: 0,
            offset: 0,
            pending: None,
            problem: None,
        }
    }

    fn signed_in(&self) -> bool {
        matches!(&self.status, Load::Ready(status) if status.signed_in)
    }

    fn send(&mut self, query: IntakeQuery, ctx: &mut Ctx) {
        self.problem = None;
        self.pending = Some(query.clone());
        ask(ctx, query);
    }

    fn list(&mut self, page: u32, ctx: &mut Ctx) {
        self.listing = Load::Loading;
        self.send(IntakeQuery::GitHubRepos { page }, ctx);
    }

    fn rows(ctx: &Ctx) -> usize {
        (ctx.height as usize).saturating_sub(12).max(3)
    }

    fn follow(&mut self, ctx: &Ctx) {
        let rows = Self::rows(ctx);
        if self.selected < self.offset {
            self.offset = self.selected;
        } else if self.selected >= self.offset + rows {
            self.offset = self.selected + 1 - rows;
        }
    }

    fn choose_way(&mut self, ctx: &mut Ctx) {
        match self.way {
            0 => self.send(IntakeQuery::GitHubSignIn, ctx),
            1 => {
                self.problem = None;
                self.token = Some(TextField::secret("GitHub token", TOKEN_CHARS_MAX));
            }
            _ => self.send(IntakeQuery::GitHubUseCli, ctx),
        }
    }

    fn send_token(&mut self, ctx: &mut Ctx) {
        let Some(field) = &mut self.token else {
            return;
        };
        match AccountSecret::new(field.take_secret()) {
            Ok(secret) => {
                self.token = None;
                self.problem = None;
                self.pending = Some(IntakeQuery::GitHubStatus);
                ctx.request(Request::Intake(Box::new(IntakeJob::GitHubToken(secret))));
            }
            Err(_) => self.problem = Some(copy("github_token_blank")),
        }
    }

    fn status_arrived(&mut self, status: &GitHubStatus, ctx: &mut Ctx) {
        let was_signed_in = self.signed_in();
        self.status = Load::Ready(status.clone());
        if status.signed_in {
            self.device = None;
            if !was_signed_in || matches!(self.listing, Load::Idle) {
                self.repos.clear();
                self.selected = 0;
                self.offset = 0;
                self.list(1, ctx);
            }
        } else if !status.waiting {
            self.device = None;
            if let Some(problem) = &status.problem {
                self.problem = Some(problem.clone());
            }
        }
    }

    fn signed_out_lines(
        &self,
        limit: usize,
        ctx: &Ctx,
    ) -> (Vec<Line<'static>>, Option<(u16, Position)>) {
        let theme = &ctx.theme;
        let mut lines = Vec::new();
        if let Some(device) = &self.device {
            lines.extend(
                wrap(
                    &format!("Open {} and enter this code:", device.verification_uri),
                    limit,
                )
                .into_iter()
                .map(Line::raw),
            );
            lines.push(Line::default());
            lines.push(styled(
                format!("    {}", device.user_code),
                Role::Accent,
                theme,
            ));
            lines.push(Line::default());
            lines.push(ctx.loading("Waiting for GitHub"));
            let minutes = device.expires_s.div_ceil(60);
            lines.push(styled(
                format!(
                    "The code works for {minutes} minute{}.",
                    if minutes == 1 { "" } else { "s" }
                ),
                Role::Muted,
                theme,
            ));
            return (lines, None);
        }
        if let Some(field) = &self.token {
            lines.extend(
                wrap(
                    "Paste a token. CrazyAgent keeps it in the system credential store and never shows it again.",
                    limit,
                )
                .into_iter()
                .map(Line::raw),
            );
            lines.push(Line::default());
            let top = lines.len() as u16;
            let (field_lines, cursor) = field.lines(limit, true, theme, ctx.glyphs);
            lines.extend(field_lines);
            return (lines, cursor.map(|at| (top, at)));
        }
        lines.extend(
            wrap(
                "Sign in to GitHub to see your repositories. CrazyAgent only reads them and clones the one you pick.",
                limit,
            )
            .into_iter()
            .map(Line::raw),
        );
        lines.push(Line::default());
        for (index, (title, description)) in WAYS.iter().enumerate() {
            lines.extend(menu_item(
                title,
                description,
                limit,
                index == self.way,
                theme,
                ctx.glyphs,
            ));
        }
        (lines, None)
    }

    fn repo_lines(&self, limit: usize, ctx: &Ctx) -> Vec<Line<'static>> {
        let theme = &ctx.theme;
        let login = match &self.status {
            Load::Ready(status) => status.login.clone().unwrap_or_default(),
            _ => String::new(),
        };
        let mut lines = vec![row(
            limit,
            Line::raw(format!("Signed in as {login}")),
            styled("x signs out", Role::Muted, theme),
            ctx.glyphs,
        )];
        lines.push(Line::default());
        if self.repos.is_empty() {
            lines.push(match &self.listing {
                Load::Loading | Load::Idle => ctx.loading("Loading your repositories"),
                _ => Line::raw("No repositories to clone on this account."),
            });
            return lines;
        }
        for (index, repo) in self
            .repos
            .iter()
            .enumerate()
            .skip(self.offset)
            .take(Self::rows(ctx))
        {
            let visibility = if repo.private { "Private" } else { "Public" };
            let size = size_label(repo.size_kb.saturating_mul(1024));
            lines.push(list_row(
                row(
                    limit.saturating_sub(2),
                    Line::raw(repo.full_name.clone()),
                    styled(format!("{visibility}  {size}"), Role::Muted, theme),
                    ctx.glyphs,
                ),
                limit,
                index == self.selected,
                true,
                theme,
                ctx.glyphs,
            ));
        }
        let shown = format!("{} of {}", self.selected + 1, self.repos.len());
        lines.push(Line::default());
        lines.push(match (&self.listing, self.next) {
            (Load::Loading, _) => ctx.loading("Loading more"),
            (_, Some(_)) => styled(
                format!("{shown}. More load as you reach the end."),
                Role::Muted,
                theme,
            ),
            _ => styled(shown, Role::Muted, theme),
        });
        lines
    }
}

impl Screen for CloneRepo {
    fn crumbs(&self) -> Vec<String> {
        vec!["Add project".into(), "Clone from GitHub".into()]
    }

    fn hints(&self, ctx: &Ctx) -> Vec<Hint> {
        let arrows = format!("{}{}", ctx.glyphs.up, ctx.glyphs.down);
        if self.token.is_some() {
            return vec![hint("Enter", "Sign in"), hint("Esc", "Cancel")];
        }
        if self.device.is_some() {
            return vec![hint("Esc", "Back")];
        }
        if self.signed_in() {
            return vec![
                hint("Enter", "Clone"),
                hint(&arrows, "Move"),
                hint("x", "Sign out"),
                hint("Esc", "Back"),
            ];
        }
        vec![
            hint("Enter", WAYS[self.way].0),
            hint(&arrows, "Move"),
            hint("Esc", "Back"),
        ]
    }

    fn typing(&self) -> bool {
        self.token.is_some()
    }

    fn key(&mut self, key: &Key, ctx: &mut Ctx) -> Flow {
        if self.pending.is_some() {
            return match key {
                Key::Esc => Flow::Ignored,
                _ => Flow::Handled,
            };
        }
        if self.token.is_some() {
            match key {
                Key::Enter => self.send_token(ctx),
                Key::Esc => self.token = None,
                other => {
                    if let Some(field) = &mut self.token {
                        field.key(other);
                    }
                }
            }
            return Flow::Handled;
        }
        if self.device.is_some() {
            return match key {
                Key::Esc => Flow::Ignored,
                _ => Flow::Handled,
            };
        }
        if self.signed_in() {
            let count = self.repos.len();
            match key {
                Key::Enter => {
                    if let Some(repo) = self.repos.get(self.selected) {
                        let full_name = repo.full_name.clone();
                        self.send(IntakeQuery::CloneRepo { full_name }, ctx);
                    }
                }
                Key::Char('x') => self.send(IntakeQuery::GitHubSignOut, ctx),
                Key::Up => self.selected = self.selected.saturating_sub(1),
                Key::Down if self.selected + 1 < count => self.selected += 1,
                Key::Down | Key::End | Key::PageDown => {
                    self.selected = match key {
                        Key::PageDown => {
                            (self.selected + Self::rows(ctx)).min(count.saturating_sub(1))
                        }
                        _ => count.saturating_sub(1),
                    };
                    if self.selected + 1 >= count
                        && let Some(page) = self.next
                        && count < REPOS_LOADED_MAX
                    {
                        self.list(page, ctx);
                    }
                }
                Key::PageUp => self.selected = self.selected.saturating_sub(Self::rows(ctx)),
                Key::Home => self.selected = 0,
                _ => return Flow::Ignored,
            }
            self.follow(ctx);
            return Flow::Handled;
        }
        if !matches!(self.status, Load::Ready(_)) {
            return Flow::Ignored;
        }
        match key {
            Key::Up => self.way = self.way.saturating_sub(1),
            Key::Down => self.way = (self.way + 1).min(WAYS.len() - 1),
            Key::Enter => self.choose_way(ctx),
            _ => return Flow::Ignored,
        }
        Flow::Handled
    }

    fn arrive(&mut self, arrival: &Arrival, ctx: &mut Ctx) -> Flow {
        let Some(reply) = event(arrival) else {
            return Flow::Handled;
        };
        let answered = self.pending.as_ref() == Some(&reply.query);
        if answered {
            self.pending = None;
        }
        match (&reply.query, &reply.result) {
            (IntakeQuery::GitHubStatus, Ok(IntakeView::GitHub(status)))
            | (IntakeQuery::GitHubUseCli, Ok(IntakeView::GitHub(status)))
            | (IntakeQuery::GitHubSignOut, Ok(IntakeView::GitHub(status))) => {
                self.status_arrived(status, ctx);
            }
            (IntakeQuery::GitHubSignIn, Ok(IntakeView::DeviceCode(code))) if answered => {
                self.device = Some(code.clone());
            }
            (IntakeQuery::GitHubRepos { .. }, Ok(IntakeView::Repos(page))) if answered => {
                let room = REPOS_LOADED_MAX.saturating_sub(self.repos.len());
                self.repos.extend(page.repos.iter().take(room).cloned());
                self.next = page.next;
                self.listing = Load::Ready(());
            }
            (IntakeQuery::CloneRepo { full_name }, Ok(IntakeView::Cloned(receipt))) if answered => {
                let name = full_name
                    .rsplit('/')
                    .next()
                    .unwrap_or(full_name.as_str())
                    .to_owned();
                ctx.say(Message::success(
                    &format!("Cloned {full_name}."),
                    "Now set it up.",
                ));
                return Flow::Replace(open(
                    &Place::Onboarding {
                        path: receipt.path.clone(),
                        name,
                        project: None,
                    },
                    ctx,
                ));
            }
            (IntakeQuery::GitHubStatus, Err(code)) if !matches!(self.status, Load::Ready(_)) => {
                self.status = if not_ready(code) {
                    Load::NotReady
                } else {
                    Load::Failed(code.clone())
                };
            }
            (IntakeQuery::GitHubRepos { .. }, Err(code)) if answered => {
                self.listing = Load::Failed(code.clone());
                self.problem = Some(copy(code));
            }
            (_, Err(code)) if answered => self.problem = Some(copy(code)),
            _ => {}
        }
        Flow::Handled
    }

    fn shown(&mut self, ctx: &mut Ctx) {
        let waiting = matches!(&self.status, Load::Ready(status) if status.waiting);
        if self.pending.is_none()
            && (matches!(self.status, Load::Idle) || self.device.is_some() || waiting)
        {
            if matches!(self.status, Load::Idle) {
                self.status = Load::Loading;
            }
            ask(ctx, IntakeQuery::GitHubStatus);
        }
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect, ctx: &Ctx) -> Option<Position> {
        let theme = &ctx.theme;
        let limit = (area.width as usize).saturating_sub(4).clamp(6, PAGE_WIDTH);
        let mut lines = vec![
            styled("Clone from GitHub", Role::Heading, theme),
            Line::default(),
        ];
        let mut cursor = None;
        match &self.status {
            Load::Idle | Load::Loading => lines.push(ctx.loading("Checking GitHub")),
            Load::NotReady => lines.push(styled(NOT_READY, Role::Muted, theme)),
            Load::Failed(code) => {
                lines.push(styled(copy(code), Role::Error, theme));
                lines.push(Line::raw("Press Esc and try again."));
            }
            Load::Ready(status) if status.signed_in => lines.extend(self.repo_lines(limit, ctx)),
            Load::Ready(_) => {
                let top = lines.len() as u16;
                let (body, at) = self.signed_out_lines(limit, ctx);
                cursor = at.map(|(offset, at)| Position::new(at.x, at.y + top + offset));
                lines.extend(body);
            }
        }
        if let Some(problem) = &self.problem {
            lines.push(Line::default());
            lines.extend(
                wrap(problem, limit)
                    .into_iter()
                    .map(|text| styled(text, Role::Error, theme)),
            );
        }
        if matches!(self.pending, Some(IntakeQuery::CloneRepo { .. })) {
            lines.push(Line::default());
            lines.push(ctx.loading("Cloning"));
        }
        let lines: Vec<Line<'static>> = lines
            .into_iter()
            .map(|line| join([Line::raw("  "), line]))
            .collect();
        draw_lines(frame.buffer_mut(), area, &lines);
        cursor.map(|at| Position::new(area.x + 2 + at.x, area.y + at.y))
    }

    fn busy(&self, _ctx: &Ctx) -> bool {
        self.pending.is_some() || self.device.is_some()
    }

    fn draft(&self) -> bool {
        matches!(self.pending, Some(IntakeQuery::CloneRepo { .. }))
    }

    fn loss(&self) -> &'static str {
        "The clone is still running. Quitting may leave it unfinished."
    }

    fn place(&self) -> Option<Place> {
        Some(Place::CloneRepo)
    }
}
