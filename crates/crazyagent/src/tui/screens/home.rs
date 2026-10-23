use super::{Flow, Place, ProjectTab, Screen, SettingsSection};
use crate::pools::{MemberStatus, PoolSummary};
use crate::tui::clock;
use crate::tui::ctx::{Ctx, Link, Load};
use crate::tui::frame::{Hint, Message, draw_lines, hint};
use crate::tui::keys::Key;
use crate::tui::seam::{Arrival, ProjectCard, Request};
use crate::tui::text::{join, pad, restyle, row, status, styled, width, wrap};
use crate::tui::theme::Role;
use crate::tui::widgets::TextField;
use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::text::{Line, Span};

const CARD_ROWS: usize = 4;
const PANEL_WIDE: usize = 38;
const PANEL_NARROW: usize = 30;
const FILTER_CHARS_MAX: usize = 100;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Panel {
    Projects,
    Setup,
}

/// Setup progress shown in the "Get set up" panel.
/// Input: engine pools. Output: which items are done; the panel hides when an
/// account is ready and a pool exists.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Setup {
    pub account_ready: bool,
    pub pool_exists: bool,
    pub ready_in_first_pool: usize,
}

impl Setup {
    /// Reads setup progress from the pool list.
    /// Input: the pools. Output: whether a ready account and a pool exist.
    pub fn of(pools: &[PoolSummary]) -> Self {
        let ready = |pool: &PoolSummary| {
            pool.members
                .iter()
                .filter(|member| {
                    matches!(member.status, MemberStatus::SignedIn | MemberStatus::SetUp)
                })
                .count()
        };
        Self {
            account_ready: pools.iter().any(|pool| ready(pool) > 0),
            pool_exists: !pools.is_empty(),
            ready_in_first_pool: pools
                .iter()
                .map(ready)
                .find(|count| *count > 0)
                .unwrap_or(0),
        }
    }

    /// True when nothing is left to set up.
    /// Input: none. Output: whether the panel hides.
    pub fn done(&self) -> bool {
        self.account_ready && self.pool_exists
    }
}

/// Home: the project list and, until setup is done, the "Get set up" panel.
/// Input: keys, project and pool arrivals. Output: project cards with status,
/// research progress and pool; Enter opens a project or a Settings section.
pub struct Home {
    panel: Panel,
    selected: usize,
    offset: usize,
    setup_selected: usize,
    filter: Option<TextField>,
    filtering: bool,
    was_done: Option<bool>,
}

impl Default for Home {
    fn default() -> Self {
        Self::new()
    }
}

impl Home {
    /// A Home screen with the project list focused.
    /// Input: none. Output: the screen; data arrives from the store.
    pub fn new() -> Self {
        Self {
            panel: Panel::Projects,
            selected: 0,
            offset: 0,
            setup_selected: 0,
            filter: None,
            filtering: false,
            was_done: None,
        }
    }

    fn setup(ctx: &Ctx) -> Option<Setup> {
        ctx.store.pools.ready().map(|pools| Setup::of(pools))
    }

    fn panel_visible(ctx: &Ctx) -> bool {
        Self::setup(ctx).is_some_and(|setup| !setup.done())
    }

    fn visible(&self, ctx: &Ctx) -> Vec<ProjectCard> {
        let Some(projects) = ctx.store.projects.ready() else {
            return Vec::new();
        };
        let words: Vec<String> = self
            .filter
            .as_ref()
            .map(|field| {
                field
                    .value()
                    .split_whitespace()
                    .map(str::to_lowercase)
                    .collect()
            })
            .unwrap_or_default();
        projects
            .iter()
            .filter(|card| {
                let text = format!(
                    "{} {} {}",
                    card.name,
                    card.about,
                    card.pool.clone().unwrap_or_default()
                )
                .to_lowercase();
                words.iter().all(|word| text.contains(word))
            })
            .cloned()
            .collect()
    }

    fn items(ctx: &Ctx) -> [(bool, &'static str, Vec<String>, Place); 3] {
        let setup = Self::setup(ctx).unwrap_or_default();
        let harness = match &ctx.store.harnesses {
            Load::Ready((ready, all)) => vec![format!("{ready} of {all} harnesses ready"), "on this Mac.".into()],
            _ => vec!["Harness checks are not".into(), "ready yet.".into()],
        };
        [
            (
                setup.account_ready,
                "Add an account",
                vec![
                    "Sign in with ChatGPT or".into(),
                    "Claude, or add an API".into(),
                    "key or local model.".into(),
                ],
                Place::Settings(SettingsSection::Accounts),
            ),
            (
                setup.pool_exists,
                "Create a pool",
                vec![
                    "Group up to 15 accounts".into(),
                    "for projects to use.".into(),
                ],
                Place::Settings(SettingsSection::Pools),
            ),
            (
                matches!(ctx.store.harnesses, Load::Ready((ready, _)) if ready > 0),
                "Check your harnesses",
                harness,
                Place::Settings(SettingsSection::Harnesses),
            ),
        ]
    }

    fn rows(ctx: &Ctx) -> usize {
        ctx.height.saturating_sub(4) as usize
    }

    fn page(ctx: &Ctx) -> usize {
        (Self::rows(ctx).saturating_sub(2) / CARD_ROWS).max(1)
    }

    fn follow(&mut self, ctx: &Ctx) {
        let page = Self::page(ctx);
        if self.selected < self.offset {
            self.offset = self.selected;
        } else if self.selected >= self.offset + page {
            self.offset = self.selected + 1 - page;
        }
    }

    fn running(ctx: &Ctx) -> usize {
        ctx.store.projects.ready().map_or(0, |projects| {
            projects.iter().map(|card| card.research.running).sum()
        })
    }

    fn card(
        &self,
        card: &ProjectCard,
        limit: usize,
        selected: bool,
        ctx: &Ctx,
    ) -> Vec<Line<'static>> {
        let theme = &ctx.theme;
        let gutter = || {
            if selected {
                Line::from(vec![
                    Span::styled(ctx.glyphs.bar, theme.accent),
                    Span::raw(" "),
                ])
            } else {
                Line::raw("  ")
            }
        };
        let state = if card.research.running > 0 {
            let count = card.research.running;
            styled(
                format!("{count} agent{} running", if count == 1 { "" } else { "s" }),
                Role::Running,
                theme,
            )
        } else if card.research.needs_pool > 0 {
            status("Needs a pool", theme, ctx.glyphs)
        } else {
            styled("Idle", Role::Muted, theme)
        };
        let platforms = card
            .platforms
            .iter()
            .map(|platform| platform.label())
            .collect::<Vec<_>>()
            .join(", ");
        let about = if card.about.is_empty() {
            "No description."
        } else {
            card.about.as_str()
        };
        let first_line = about.lines().next().unwrap_or_default().to_owned();
        let created = card
            .created_ms
            .map(|ms| {
                format!(
                    "Created {}",
                    clock::ago(ms, clock::now_ms(), clock::local_offset())
                )
            })
            .unwrap_or_default();
        let pool = format!(
            "Pool: {}",
            card.pool.clone().unwrap_or_else(|| "none".into())
        );
        vec![
            join([
                gutter(),
                row(
                    limit - 2,
                    styled(
                        card.name.clone(),
                        if selected {
                            Role::Accent
                        } else {
                            Role::Heading
                        },
                        theme,
                    ),
                    state,
                    ctx.glyphs,
                ),
            ]),
            join([
                gutter(),
                pad(
                    Line::from(vec![
                        Span::raw(first_line),
                        Span::raw("  "),
                        Span::styled(platforms, theme.muted),
                    ]),
                    limit - 2,
                    ctx.glyphs,
                ),
            ]),
            join([
                gutter(),
                row(
                    limit - 2,
                    styled(
                        format!("{}   {pool}", card.research.summary()),
                        Role::Muted,
                        theme,
                    ),
                    styled(created, Role::Muted, theme),
                    ctx.glyphs,
                ),
            ]),
            Line::default(),
        ]
    }

    fn projects_column(&self, limit: usize, ctx: &Ctx) -> (Vec<Line<'static>>, Option<Position>) {
        let theme = &ctx.theme;
        let focused = self.panel == Panel::Projects;
        let title = |count: Option<usize>| {
            let mut spans = vec![Span::styled(
                "Projects",
                if focused && count.is_some() {
                    theme.accent
                } else {
                    theme.heading
                },
            )];
            if let Some(count) = count {
                spans.push(Span::raw("  "));
                spans.push(Span::styled(count.to_string(), theme.muted));
            }
            Line::from(spans)
        };
        match &ctx.store.projects {
            Load::Ready(projects) if projects.is_empty() => {
                let mut lines = vec![title(None), Line::default(), Line::raw("No projects yet.")];
                lines.extend(
                    wrap(
                        "Start one with n. You can add accounts first, or start a project now and pick a pool later.",
                        limit,
                    )
                    .into_iter()
                    .map(Line::raw),
                );
                (lines, None)
            }
            Load::Ready(projects) => {
                let visible = self.visible(ctx);
                let right = if ctx.link == Link::Reconnecting {
                    styled(
                        ctx.store
                            .projects_seen
                            .as_ref()
                            .map(|time| format!("Last updated {time}"))
                            .unwrap_or_default(),
                        Role::Muted,
                        theme,
                    )
                } else {
                    styled("Most recent first", Role::Muted, theme)
                };
                let mut lines = vec![row(limit, title(Some(projects.len())), right, ctx.glyphs)];
                let mut cursor = None;
                match &self.filter {
                    Some(field) => {
                        lines.push(pad(
                            Line::from(vec![
                                Span::styled("/", theme.accent),
                                Span::raw(format!(" {}", field.value())),
                            ]),
                            limit,
                            ctx.glyphs,
                        ));
                        if self.filtering {
                            let column = (2 + width(field.value())).min(limit.saturating_sub(1));
                            cursor = Some(Position::new(column as u16, 1));
                        }
                    }
                    None => lines.push(Line::default()),
                }
                if visible.is_empty() {
                    lines.push(Line::raw(format!(
                        "No project matches. Showing 0 of {}.",
                        projects.len()
                    )));
                }
                for (index, card) in visible
                    .iter()
                    .enumerate()
                    .skip(self.offset)
                    .take(Self::page(ctx))
                {
                    lines.extend(self.card(
                        card,
                        limit,
                        focused && index == self.selected && !self.filtering,
                        ctx,
                    ));
                }
                if ctx.link == Link::Reconnecting {
                    lines = lines
                        .into_iter()
                        .enumerate()
                        .map(|(index, line)| {
                            if index == 0 {
                                line
                            } else {
                                restyle(line, theme.backdrop)
                            }
                        })
                        .collect();
                }
                (lines, cursor)
            }
            Load::Failed(_) => (
                vec![
                    title(None),
                    Line::default(),
                    status("Failed to load projects.", theme, ctx.glyphs),
                    Line::raw("Press r to try again."),
                ],
                None,
            ),
            _ => (
                vec![
                    title(None),
                    Line::default(),
                    ctx.loading("Loading projects"),
                ],
                None,
            ),
        }
    }

    fn setup_column(&self, ctx: &Ctx) -> Vec<Line<'static>> {
        let theme = &ctx.theme;
        let focused = self.panel == Panel::Setup;
        let mut lines = vec![
            styled(
                "Get set up",
                if focused { Role::Accent } else { Role::Heading },
                theme,
            ),
            Line::default(),
        ];
        for (index, (done, label, description, _)) in Self::items(ctx).into_iter().enumerate() {
            let selected = focused && index == self.setup_selected;
            let gutter = || {
                if selected {
                    Span::styled(ctx.glyphs.bar, theme.accent)
                } else {
                    Span::raw(" ")
                }
            };
            lines.push(Line::from(vec![
                gutter(),
                Span::raw(" "),
                if done {
                    Span::styled("[x]", theme.success)
                } else {
                    Span::raw("[ ]")
                },
                Span::raw(" "),
                Span::styled(label, if selected { theme.accent } else { theme.text }),
            ]));
            for part in description {
                lines.push(Line::from(vec![
                    gutter(),
                    Span::raw("     "),
                    Span::styled(part, theme.muted),
                ]));
            }
            lines.push(Line::default());
        }
        lines.push(styled("Enter opens Settings", Role::Muted, theme));
        lines
    }

    fn open_selected(&self, ctx: &Ctx) -> Flow {
        if self.panel == Panel::Setup && Self::panel_visible(ctx) {
            return Flow::Open(Self::items(ctx)[self.setup_selected].3.clone());
        }
        match self.visible(ctx).get(self.selected) {
            Some(card) => Flow::Open(Place::Project {
                id: card.id.clone(),
                name: card.name.clone(),
                tab: ProjectTab::Overview,
            }),
            None => Flow::Handled,
        }
    }
}

impl Screen for Home {
    fn crumbs(&self) -> Vec<String> {
        vec!["Home".into()]
    }

    fn status(&self, ctx: &Ctx) -> Line<'static> {
        if ctx.link == Link::Reconnecting {
            return styled("Reconnecting", Role::Error, &ctx.theme);
        }
        match Self::running(ctx) {
            0 => Line::default(),
            count => styled(
                format!("{count} agent{} running", if count == 1 { "" } else { "s" }),
                Role::Running,
                &ctx.theme,
            ),
        }
    }

    fn hints(&self, ctx: &Ctx) -> Vec<Hint> {
        if self.filtering {
            return vec![
                hint("Enter", "Keep filter"),
                hint(ctx.glyphs.down, "To the list"),
                hint("Esc", "Clear filter"),
            ];
        }
        let arrows = format!("{}{}", ctx.glyphs.up, ctx.glyphs.down);
        if self.panel == Panel::Setup && Self::panel_visible(ctx) {
            return vec![
                hint("Enter", Self::items(ctx)[self.setup_selected].1),
                hint(&arrows, "Move"),
                hint("Tab", "Switch panel"),
                hint("n", "New project"),
                hint("s", "Settings"),
                hint("Ctrl-K", "Go to"),
                hint("Ctrl-Q", "Quit"),
            ];
        }
        vec![
            hint("n", "New project"),
            hint("Enter", "Open"),
            hint("s", "Settings"),
            hint("/", "Filter"),
            hint("Ctrl-K", "Go to"),
            hint("Ctrl-Q", "Quit"),
        ]
    }

    fn keys(&self, _ctx: &Ctx) -> Vec<Hint> {
        vec![
            hint("n", "New project"),
            hint("s", "Settings"),
            hint("Enter", "Open project"),
        ]
    }

    fn typing(&self) -> bool {
        self.filtering
    }

    fn key(&mut self, key: &Key, ctx: &mut Ctx) -> Flow {
        if self.filtering {
            match key {
                Key::Esc => {
                    self.filter = None;
                    self.filtering = false;
                }
                Key::Enter | Key::Down => {
                    self.filtering = false;
                    if self.filter.as_ref().is_some_and(TextField::is_empty) {
                        self.filter = None;
                    }
                }
                other => {
                    if let Some(field) = &mut self.filter {
                        field.key(other);
                    }
                    self.selected = 0;
                    self.offset = 0;
                }
            }
            return Flow::Handled;
        }
        let panel_visible = Self::panel_visible(ctx);
        if !panel_visible {
            self.panel = Panel::Projects;
        }
        match key {
            Key::Char('n') => Flow::Open(Place::NewProject),
            Key::Char('s') => Flow::Open(Place::Settings(SettingsSection::Accounts)),
            Key::Char('r') if matches!(ctx.store.projects, Load::Failed(_)) => {
                ctx.request(Request::Projects);
                Flow::Handled
            }
            Key::Char('/') if self.panel == Panel::Projects => {
                self.filter
                    .get_or_insert_with(|| TextField::new("Filter", FILTER_CHARS_MAX));
                self.filtering = true;
                Flow::Handled
            }
            Key::Esc if self.filter.is_some() => {
                self.filter = None;
                Flow::Handled
            }
            Key::Tab | Key::BackTab if panel_visible => {
                self.panel = match self.panel {
                    Panel::Projects => Panel::Setup,
                    Panel::Setup => Panel::Projects,
                };
                Flow::Handled
            }
            Key::Up | Key::Down if self.panel == Panel::Setup => {
                self.setup_selected = if matches!(key, Key::Down) {
                    (self.setup_selected + 1).min(2)
                } else {
                    self.setup_selected.saturating_sub(1)
                };
                Flow::Handled
            }
            Key::Up | Key::Down | Key::PageUp | Key::PageDown | Key::Home | Key::End => {
                let count = self.visible(ctx).len();
                if count == 0 {
                    return Flow::Handled;
                }
                let page = Self::page(ctx);
                self.selected = match key {
                    Key::Up => self.selected.saturating_sub(1),
                    Key::Down => (self.selected + 1).min(count - 1),
                    Key::PageUp => self.selected.saturating_sub(page.saturating_sub(1).max(1)),
                    Key::PageDown => (self.selected + page.saturating_sub(1).max(1)).min(count - 1),
                    Key::Home => 0,
                    _ => count - 1,
                };
                self.follow(ctx);
                Flow::Handled
            }
            Key::Enter => self.open_selected(ctx),
            _ => Flow::Ignored,
        }
    }

    fn arrive(&mut self, arrival: &Arrival, ctx: &mut Ctx) -> Flow {
        match arrival {
            Arrival::Pools(Ok(pools)) => {
                let setup = Setup::of(pools);
                if self.was_done == Some(false) && setup.done() {
                    let pool = pools
                        .iter()
                        .find(|pool| !pool.members.is_empty())
                        .map_or_else(String::new, |pool| pool.name.clone());
                    let count = setup.ready_in_first_pool;
                    ctx.say(Message::success(
                        "Setup done.",
                        &format!(
                            "{count} account{} ready in pool {pool}.",
                            if count == 1 { "" } else { "s" }
                        ),
                    ));
                }
                if self.was_done.is_none() && !setup.done() {
                    self.panel = Panel::Setup;
                }
                self.was_done = Some(setup.done());
                Flow::Handled
            }
            Arrival::Projects(Ok(projects)) => {
                if self.selected >= projects.len() {
                    self.selected = projects.len().saturating_sub(1);
                }
                self.follow(ctx);
                Flow::Handled
            }
            _ => Flow::Handled,
        }
    }

    fn shown(&mut self, ctx: &mut Ctx) {
        ctx.request(Request::Projects);
        ctx.request(Request::Pools);
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect, ctx: &Ctx) -> Option<Position> {
        let width = area.width as usize;
        let (lines, cursor) = if Self::panel_visible(ctx) {
            let right = if ctx.wide() { PANEL_WIDE } else { PANEL_NARROW };
            let left = width - 4 - 3 - right;
            let (projects, cursor) = self.projects_column(left, ctx);
            let setup = self.setup_column(ctx);
            let lines = (0..area.height as usize)
                .map(|index| {
                    join([
                        Line::raw("  "),
                        pad(
                            projects.get(index).cloned().unwrap_or_default(),
                            left,
                            ctx.glyphs,
                        ),
                        Line::from(Span::styled(
                            format!(" {} ", ctx.glyphs.vline),
                            ctx.theme.muted,
                        )),
                        pad(
                            setup.get(index).cloned().unwrap_or_default(),
                            right,
                            ctx.glyphs,
                        ),
                    ])
                })
                .collect::<Vec<_>>();
            (lines, cursor)
        } else {
            let (projects, cursor) = self.projects_column(width - 4, ctx);
            (
                projects
                    .into_iter()
                    .map(|line| join([Line::raw("  "), line]))
                    .collect(),
                cursor,
            )
        };
        draw_lines(frame.buffer_mut(), area, &lines);
        cursor.map(|at| Position::new(area.x + 2 + at.x, area.y + at.y))
    }

    fn busy(&self, ctx: &Ctx) -> bool {
        matches!(ctx.store.projects, Load::Loading | Load::Idle)
    }

    fn place(&self) -> Option<Place> {
        Some(Place::Home)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pools::{MemberKind, PoolMember};
    use crate::protocol::CredentialRef;

    fn pool(name: &str, statuses: &[MemberStatus]) -> PoolSummary {
        PoolSummary {
            id: CredentialRef::new(format!("pool-{name}")).unwrap(),
            name: name.into(),
            summary: String::new(),
            members: statuses
                .iter()
                .enumerate()
                .map(|(index, status)| PoolMember {
                    id: CredentialRef::new(format!("member-{index}")).unwrap(),
                    kind: MemberKind::Chatgpt,
                    provider: "ChatGPT".into(),
                    status: *status,
                    status_label: String::new(),
                })
                .collect(),
            project_count: 0,
        }
    }

    #[test]
    fn setup_is_done_with_a_ready_account_in_a_pool() {
        assert!(!Setup::of(&[]).done());
        let empty = Setup::of(&[pool("Main", &[])]);
        assert!(empty.pool_exists && !empty.account_ready && !empty.done());
        let waiting = Setup::of(&[pool("Main", &[MemberStatus::NotSignedIn])]);
        assert!(!waiting.done());
        let ready = Setup::of(&[
            pool("Empty", &[]),
            pool(
                "Main",
                &[
                    MemberStatus::SignedIn,
                    MemberStatus::SetUp,
                    MemberStatus::NotSignedIn,
                ],
            ),
        ]);
        assert!(ready.done());
        assert_eq!(ready.ready_in_first_pool, 2);
    }
}
