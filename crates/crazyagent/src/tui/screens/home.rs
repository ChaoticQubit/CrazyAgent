use super::intake::{ask, event, not_ready};
use super::new_project::display_path;
use super::new_project_page::kept_draft;
use super::settings_accounts::{load_accounts, send};
use super::{Flow, Place, ProjectTab, Screen, SettingsSection};
use crate::accounts::AccountStatus;
use crate::pools::PoolSummary;
use crate::projects_home::{FoundProject, FoundState, ProjectsHome};
use crate::setup::{Dependency, DependencyState, SetupStatus};
use crate::tui::client::app_setup::AppSetupJob;
use crate::tui::client::intake::{IntakeQuery, IntakeView};
use crate::tui::client::settings::SettingsJob;
use crate::tui::clock;
use crate::tui::ctx::{Ctx, Link, Load};
use crate::tui::frame::{Hint, Message, draw_lines, hint};
use crate::tui::keys::Key;
use crate::tui::seam::{AccountRow, Arrival, ProjectCard, Request};
use crate::tui::text::{cut, join, pad, restyle, row, status, styled, width, wrap};
use crate::tui::theme::Role;
use crate::tui::widgets::TextField;
use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::text::{Line, Span};

const CARD_ROWS: usize = 4;
const PANEL_WIDE: usize = 38;
const PANEL_NARROW: usize = 30;
const FILTER_CHARS_MAX: usize = 100;
const TOOLS_TEXT_CHARS: usize = 24;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Panel {
    Projects,
    Setup,
}

/// One row of the project list: the New project draft kept after leaving the
/// wizard, a projects folder entry that is not a ready project, or a project.
/// Input: the kept draft, the found folders and the store. Output: drawn as a
/// four-row card; Enter continues the draft, sets up the folder or opens the project.
enum Entry {
    Draft(String),
    Folder(FoundProject),
    Project(ProjectCard),
}

/// Setup progress shown in the "Get set up" panel.
/// Input: engine pools, accounts and whether the tools are found. Output: which
/// items are done and what Add an account says; the panel hides when an account
/// is ready for agents, a pool exists and the tools are found.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Setup {
    pub ready_accounts: Vec<String>,
    pub accounts: usize,
    pub pool_exists: bool,
    pub tools_ready: bool,
}

impl Setup {
    /// Reads setup progress from the pool list, the account list and the tools
    /// check. An account is ready for agents when it is signed in (a reachable
    /// URL or local model counts) or has a saved key.
    /// Input: the pools, the accounts and whether every tool is found. Output:
    /// whether a ready account, a pool and the tools exist.
    pub fn of(pools: &[PoolSummary], accounts: &[AccountRow], tools_ready: bool) -> Self {
        Self {
            ready_accounts: accounts
                .iter()
                .filter(|account| ready_for_agents(account))
                .map(|account| account.name.clone())
                .collect(),
            accounts: accounts.len(),
            pool_exists: !pools.is_empty(),
            tools_ready,
        }
    }

    /// True when at least one account is ready for agents.
    /// Input: none. Output: whether Add an account is done.
    pub fn account_ready(&self) -> bool {
        !self.ready_accounts.is_empty()
    }

    /// What Add an account says: the account ready for agents, how many are,
    /// that none is yet, or how to add the first one. A name is cut to one
    /// panel line so the text stays within three lines.
    /// Input: the ellipsis glyph. Output: the sentence.
    pub fn account_line(&self, ellipsis: &str) -> String {
        match self.ready_accounts.as_slice() {
            [one] => format!(
                "{} is ready for agents.",
                cut(one, TOOLS_TEXT_CHARS, ellipsis)
            ),
            [] if self.accounts == 0 => {
                "Sign in with ChatGPT or Claude, or add an API key or local model.".into()
            }
            [] => "No account is ready for agents yet. Sign one in or check its key or URL.".into(),
            many => format!("{} accounts are ready for agents.", many.len()),
        }
    }

    /// True when nothing is left to set up.
    /// Input: none. Output: whether the panel hides.
    pub fn done(&self) -> bool {
        self.account_ready() && self.pool_exists && self.tools_ready
    }
}

/// True when agents can use the account: signed in (a reachable URL or local
/// model counts) or with a saved key.
/// Input: the account row. Output: whether it is ready for agents.
fn ready_for_agents(account: &AccountRow) -> bool {
    account.status == AccountStatus::SignedIn.label()
        || account.status == AccountStatus::KeySaved.label()
}

/// True while the engine still checks a tool's version in the background.
/// Input: the setup status. Output: whether any tool reads Checking.
fn checking(status: &SetupStatus) -> bool {
    status
        .dependencies
        .iter()
        .any(|tool| tool.state == DependencyState::Checking)
}

/// Names the tools the setup check did not find, in its order. Home's short line
/// calls the Graphify command just Graphify, as there is no pack row beside it.
/// Input: the setup status. Output: the names of tools neither found nor installed.
fn missing_tools(status: &SetupStatus) -> Vec<String> {
    status
        .dependencies
        .iter()
        .filter(|tool| {
            !matches!(
                tool.state,
                DependencyState::Found { .. } | DependencyState::Installed { .. }
            )
        })
        .map(|tool| match tool.name {
            Dependency::Graphify => "Graphify".into(),
            _ => tool.label.clone(),
        })
        .collect()
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
    folder: Load<ProjectsHome>,
    found: Load<Vec<FoundProject>>,
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
            folder: Load::Idle,
            found: Load::Idle,
        }
    }

    fn folders(&self) -> Vec<FoundProject> {
        let mut folders: Vec<FoundProject> = self
            .found
            .ready()
            .map(|found| {
                found
                    .iter()
                    .filter(|folder| folder.state != FoundState::Ready)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        folders.sort_by_key(|folder| match folder.state {
            FoundState::Onboarding => 0,
            FoundState::Found => 1,
            FoundState::Missing | FoundState::Ready => 2,
        });
        folders
    }

    fn unpicked(&self) -> bool {
        matches!(&self.folder, Load::Ready(home) if home.path.is_none())
    }

    fn setup(ctx: &Ctx) -> Option<Setup> {
        let tools_ready = ctx
            .store
            .setup
            .ready()
            .is_some_and(|status| missing_tools(status).is_empty());
        let accounts = match &ctx.store.accounts {
            Load::Ready(rows) => rows.as_slice(),
            Load::Failed(_) | Load::NotReady => &[],
            Load::Idle | Load::Loading => return None,
        };
        ctx.store
            .pools
            .ready()
            .map(|pools| Setup::of(pools, accounts, tools_ready))
    }

    fn panel_visible(ctx: &Ctx) -> bool {
        Self::setup(ctx).is_some_and(|setup| !setup.done())
    }

    fn entries(&self, ctx: &Ctx) -> Vec<Entry> {
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
        let matches = |text: &str| {
            let text = text.to_lowercase();
            words.iter().all(|word| text.contains(word))
        };
        let draft = kept_draft()
            .filter(|name| matches(name.as_str()))
            .map(Entry::Draft);
        let folders = self
            .folders()
            .into_iter()
            .filter(|folder| matches(folder.name.as_str()))
            .map(Entry::Folder);
        let projects = projects
            .iter()
            .filter(|card| {
                matches(
                    format!(
                        "{} {} {}",
                        card.name,
                        card.about,
                        card.pool.clone().unwrap_or_default()
                    )
                    .as_str(),
                )
            })
            .cloned()
            .map(Entry::Project);
        draft.into_iter().chain(folders).chain(projects).collect()
    }

    fn items(ctx: &Ctx) -> [(bool, &'static str, Vec<String>, Place); 4] {
        let setup = Self::setup(ctx).unwrap_or_default();
        let missing = ctx
            .store
            .setup
            .ready()
            .filter(|status| !checking(status))
            .map(missing_tools)
            .unwrap_or_default();
        let tools = if missing.is_empty() {
            vec!["Agents need tmux, uv and".into(), "Graphify.".into()]
        } else {
            wrap(
                &format!("Missing: {}.", missing.join(", ")),
                TOOLS_TEXT_CHARS,
            )
        };
        let harness = match &ctx.store.harnesses {
            Load::Ready((ready, all)) => vec![
                format!("{ready} of {all} harnesses ready"),
                "on this Mac.".into(),
            ],
            _ => vec!["Harness checks are not".into(), "ready yet.".into()],
        };
        [
            (
                setup.account_ready(),
                "Add an account",
                wrap(&setup.account_line(ctx.glyphs.ellipsis), TOOLS_TEXT_CHARS),
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
            (
                setup.tools_ready,
                "Install tools",
                tools,
                Place::Setup { first_run: false },
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
            projects.iter().map(ProjectCard::working).sum()
        })
    }

    fn suggestions(ctx: &Ctx) -> usize {
        ctx.store.projects.ready().map_or(0, |projects| {
            projects.iter().map(|card| card.activity.suggestions).sum()
        })
    }

    fn gutter(selected: bool, ctx: &Ctx) -> Line<'static> {
        if selected {
            Line::from(vec![
                Span::styled(ctx.glyphs.bar, ctx.theme.accent),
                Span::raw(" "),
            ])
        } else {
            Line::raw("  ")
        }
    }

    fn draft_card(name: &str, limit: usize, selected: bool, ctx: &Ctx) -> Vec<Line<'static>> {
        let theme = &ctx.theme;
        let name_role = if selected {
            Role::Accent
        } else {
            Role::Heading
        };
        vec![
            join([
                Self::gutter(selected, ctx),
                row(
                    limit - 2,
                    styled(name.to_owned(), name_role, theme),
                    styled("Draft", Role::Muted, theme),
                    ctx.glyphs,
                ),
            ]),
            join([
                Self::gutter(selected, ctx),
                pad(
                    styled("Not created yet.", Role::Muted, theme),
                    limit - 2,
                    ctx.glyphs,
                ),
            ]),
            join([
                Self::gutter(selected, ctx),
                pad(
                    Line::raw("Enter continues the draft."),
                    limit - 2,
                    ctx.glyphs,
                ),
            ]),
            Line::default(),
        ]
    }

    fn folder_card(
        folder: &FoundProject,
        limit: usize,
        selected: bool,
        ctx: &Ctx,
    ) -> Vec<Line<'static>> {
        let theme = &ctx.theme;
        let (state, next) = match folder.state {
            FoundState::Onboarding => (
                status("Setting up", theme, ctx.glyphs),
                "Enter shows the setup.",
            ),
            FoundState::Missing => (
                status("Folder missing", theme, ctx.glyphs),
                "The folder is gone from your projects folder.",
            ),
            FoundState::Found | FoundState::Ready => {
                (status("Not set up", theme, ctx.glyphs), "Enter sets it up.")
            }
        };
        let name_role = if selected {
            Role::Accent
        } else {
            Role::Heading
        };
        let home = ctx.home.as_deref().map(std::path::Path::new);
        vec![
            join([
                Self::gutter(selected, ctx),
                row(
                    limit - 2,
                    styled(folder.name.clone(), name_role, theme),
                    state,
                    ctx.glyphs,
                ),
            ]),
            join([
                Self::gutter(selected, ctx),
                pad(
                    styled(display_path(&folder.path, home), Role::Muted, theme),
                    limit - 2,
                    ctx.glyphs,
                ),
            ]),
            join([
                Self::gutter(selected, ctx),
                pad(Line::raw(next), limit - 2, ctx.glyphs),
            ]),
            Line::default(),
        ]
    }

    fn card(
        &self,
        card: &ProjectCard,
        limit: usize,
        selected: bool,
        ctx: &Ctx,
    ) -> Vec<Line<'static>> {
        let theme = &ctx.theme;
        let gutter = || Self::gutter(selected, ctx);
        let state = if card.working() > 0 {
            let count = card.working();
            styled(
                format!("{count} agent{} running", if count == 1 { "" } else { "s" }),
                Role::Running,
                theme,
            )
        } else if card.research.needs_pool > 0 {
            status("Needs a pool", theme, ctx.glyphs)
        } else if card.research.rate_limited > 0 {
            status("Rate limited", theme, ctx.glyphs)
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
        let corner = match card.activity.suggestions {
            0 => styled(created, Role::Muted, theme),
            count => styled(
                format!(
                    "{count} suggested memor{}",
                    if count == 1 { "y" } else { "ies" }
                ),
                Role::Warning,
                theme,
            ),
        };
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
                    corner,
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
            if let Some(count) = count.filter(|count| *count > 0) {
                spans.push(Span::raw("  "));
                spans.push(Span::styled(count.to_string(), theme.muted));
            }
            Line::from(spans)
        };
        let unpicked = self
            .unpicked()
            .then(|| styled("Pick your projects folder with f.", Role::Warning, theme));
        match &ctx.store.projects {
            Load::Ready(projects)
                if projects.is_empty() && kept_draft().is_none() && self.folders().is_empty() =>
            {
                let mut lines = vec![title(None), Line::default(), Line::raw("No projects yet.")];
                lines.extend(
                    wrap(
                        "Start one with n. You can add accounts first, or start a project now and pick a pool later.",
                        limit,
                    )
                    .into_iter()
                    .map(Line::raw),
                );
                if let Some(line) = unpicked {
                    lines.push(Line::default());
                    lines.push(line);
                }
                (lines, None)
            }
            Load::Ready(projects) => {
                let entries = self.entries(ctx);
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
                if let Some(line) = unpicked {
                    lines.push(pad(line, limit, ctx.glyphs));
                }
                if entries.is_empty() {
                    lines.push(Line::raw(format!(
                        "No project matches. Showing 0 of {}.",
                        projects.len()
                    )));
                }
                for (index, entry) in entries
                    .iter()
                    .enumerate()
                    .skip(self.offset)
                    .take(Self::page(ctx))
                {
                    let selected = focused && index == self.selected && !self.filtering;
                    lines.extend(match entry {
                        Entry::Draft(name) => Self::draft_card(name, limit, selected, ctx),
                        Entry::Folder(folder) => Self::folder_card(folder, limit, selected, ctx),
                        Entry::Project(card) => self.card(card, limit, selected, ctx),
                    });
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

    /// True once the tools check has a final answer: every tool checked, or the
    /// read failed or is not in this build.
    /// Input: the context. Output: whether the tools item is settled.
    fn checked(ctx: &Ctx) -> bool {
        match &ctx.store.setup {
            Load::Ready(status) => !checking(status),
            Load::Failed(_) | Load::NotReady => true,
            Load::Idle | Load::Loading => false,
        }
    }

    /// Says "Setup done." with the accounts ready for agents when the last item
    /// gets done, and focuses the panel on the first answer when something is left.
    /// Input: the context, once pools, accounts and the tools check have answered.
    /// Output: none.
    fn notice(&mut self, ctx: &mut Ctx) {
        if !Self::checked(ctx) {
            return;
        }
        let Some(setup) = Self::setup(ctx) else {
            return;
        };
        if self.was_done == Some(false) && setup.done() {
            let line = setup.account_line(ctx.glyphs.ellipsis);
            ctx.say(Message::success("Setup done.", &line));
        }
        if self.was_done.is_none() && !setup.done() {
            self.panel = Panel::Setup;
        }
        self.was_done = Some(setup.done());
    }

    fn open_selected(&self, ctx: &Ctx) -> Flow {
        if self.panel == Panel::Setup && Self::panel_visible(ctx) {
            return Flow::Open(Self::items(ctx)[self.setup_selected].3.clone());
        }
        match self.entries(ctx).into_iter().nth(self.selected) {
            Some(Entry::Draft(_)) => Flow::Open(Place::NewProject),
            Some(Entry::Folder(folder)) => match folder.state {
                FoundState::Missing => Flow::Handled,
                FoundState::Onboarding => Flow::Open(Place::Onboarding {
                    path: folder.path,
                    name: folder.name,
                    project: folder.project_id,
                }),
                FoundState::Found | FoundState::Ready => Flow::Open(Place::Onboarding {
                    path: folder.path,
                    name: folder.name,
                    project: None,
                }),
            },
            Some(Entry::Project(card)) => Flow::Open(Place::Project {
                id: card.id,
                name: card.name,
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
                hint("n", "New project"),
                hint("a", "Add project"),
                hint(&arrows, "Move"),
                hint("Tab", "Switch panel"),
                hint("s", "Settings"),
                hint("Ctrl-K", "Go to"),
                hint("Ctrl-Q", "Quit"),
            ];
        }
        let mut hints = vec![
            hint("n", "New project"),
            hint("a", "Add project"),
            hint("Enter", "Open"),
        ];
        if Self::suggestions(ctx) > 0 {
            hints.push(hint("m", "Review memories"));
        }
        hints.extend([
            hint("s", "Settings"),
            hint("/", "Filter"),
            hint("Ctrl-K", "Go to"),
            hint("Ctrl-Q", "Quit"),
        ]);
        hints
    }

    fn keys(&self, _ctx: &Ctx) -> Vec<Hint> {
        vec![
            hint("n", "New project"),
            hint("a", "Add project"),
            hint("s", "Settings"),
            hint("Enter", "Open project"),
            hint("f", "Projects folder"),
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
            Key::Char('a') => Flow::Open(Place::AddProject),
            Key::Char('f') => Flow::Open(Place::HomeFolder { first_run: false }),
            Key::Char('s') => Flow::Open(Place::Settings(SettingsSection::Accounts)),
            Key::Char('m') => Flow::Open(Place::Settings(SettingsSection::Memory)),
            Key::Char('u') => Flow::Open(Place::Usage),
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
                    (self.setup_selected + 1).min(Self::items(ctx).len() - 1)
                } else {
                    self.setup_selected.saturating_sub(1)
                };
                Flow::Handled
            }
            Key::Up | Key::Down | Key::PageUp | Key::PageDown | Key::Home | Key::End => {
                let count = self.entries(ctx).len();
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
        if let Some(reply) = event(arrival) {
            match (&reply.query, &reply.result) {
                (IntakeQuery::Home, Ok(IntakeView::Home(home))) => {
                    self.folder = Load::Ready(home.clone());
                }
                (IntakeQuery::Home, Err(code)) => {
                    self.folder = if not_ready(code) {
                        Load::NotReady
                    } else {
                        Load::Failed(code.clone())
                    };
                }
                (IntakeQuery::Found, Ok(IntakeView::Found(found))) => {
                    self.found = Load::Ready(found.clone());
                    let count = self.entries(ctx).len();
                    if self.selected >= count {
                        self.selected = count.saturating_sub(1);
                    }
                    self.follow(ctx);
                }
                (IntakeQuery::Found, Err(code)) => {
                    self.found = if not_ready(code) {
                        Load::NotReady
                    } else {
                        Load::Failed(code.clone())
                    };
                }
                _ => {}
            }
            return Flow::Handled;
        }
        match arrival {
            Arrival::Pools(Ok(_)) | Arrival::Owner(_) => {
                self.notice(ctx);
                Flow::Handled
            }
            Arrival::Projects(Ok(_)) => {
                let count = self.entries(ctx).len();
                if self.selected >= count {
                    self.selected = count.saturating_sub(1);
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
        let ready = ctx
            .store
            .accounts
            .ready()
            .is_some_and(|rows| rows.iter().any(ready_for_agents));
        if !ready {
            load_accounts(ctx);
        }
        ask(ctx, IntakeQuery::Home);
        ask(ctx, IntakeQuery::Found);
        if ctx.store.harnesses.ready().is_none() {
            send(ctx, SettingsJob::Harnesses);
        }
        if !Self::checked(ctx) || matches!(ctx.store.setup, Load::Failed(_)) {
            send(ctx, SettingsJob::AppSetup(AppSetupJob::Status));
        }
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
pub(in crate::tui) mod tests {
    use super::*;
    use crate::pools::{MemberKind, MemberStatus, PoolMember};
    use crate::protocol::CredentialRef;
    use crate::setup::{Dependency, DependencyStatus, Version};
    use crate::tui::client::OwnerEvent;
    use crate::tui::client::app_setup::AppSetupEvent;
    use crate::tui::client::settings::SettingsEvent;
    use crate::tui::screens::settings_accounts::tests::{ctx, render};
    use crate::tui::shell::Shell;

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
            provider_quota_policy: crate::config::ProviderQuotaPolicy::default(),
        }
    }

    /// A setup check where every tool is found except the `missing` ones.
    /// Input: the tools to report missing. Output: the status, with no pack entries.
    pub(in crate::tui) fn tools(missing: &[Dependency]) -> SetupStatus {
        SetupStatus {
            dependencies: Dependency::ALL
                .iter()
                .map(|name| DependencyStatus {
                    name: *name,
                    label: name.label().into(),
                    state: if missing.contains(name) {
                        DependencyState::Missing
                    } else {
                        DependencyState::Found {
                            version: Version::parse("1.0.0").unwrap(),
                        }
                    },
                })
                .collect(),
            pack: Vec::new(),
            running: false,
        }
    }

    fn status(status: SetupStatus) -> Arrival {
        Arrival::Owner(Box::new(OwnerEvent::Settings(Box::new(
            SettingsEvent::AppSetup(AppSetupEvent::Status(Ok(status))),
        ))))
    }

    fn status_reads(shell: &mut Shell) -> usize {
        std::iter::from_fn(|| shell.ctx.next_request())
            .filter(|request| {
                matches!(
                    request,
                    Request::Settings(job)
                        if matches!(job.as_ref(), SettingsJob::AppSetup(AppSetupJob::Status))
                )
            })
            .count()
    }

    fn account(index: usize, name: &str, status: AccountStatus) -> AccountRow {
        AccountRow {
            id: CredentialRef::new(format!("account-{index}")).unwrap(),
            name: name.into(),
            provider: "Claude".into(),
            status: status.label().into(),
        }
    }

    #[test]
    fn setup_is_done_with_an_account_ready_for_agents_a_pool_and_tools_found() {
        assert!(!Setup::of(&[], &[], true).done());
        let empty = Setup::of(&[pool("Main", &[])], &[], true);
        assert!(empty.pool_exists && !empty.account_ready() && !empty.done());
        assert_eq!(
            empty.account_line("…"),
            "Sign in with ChatGPT or Claude, or add an API key or local model."
        );

        let pools = [pool("Main", &[MemberStatus::SetUp, MemberStatus::SignedIn])];
        let waiting = [
            account(1, "ChaoticQubit", AccountStatus::NotSignedIn),
            account(2, "ChaoticQubit", AccountStatus::SigningIn),
            account(3, "Qwen 2.5 - 7B", AccountStatus::Unreachable),
        ];
        let none = Setup::of(&pools, &waiting, true);
        assert!(
            !none.account_ready() && !none.done(),
            "pool members count for nothing while no account is ready"
        );
        assert_eq!(
            none.account_line("…"),
            "No account is ready for agents yet. Sign one in or check its key or URL."
        );
        assert_eq!(wrap(&none.account_line("…"), TOOLS_TEXT_CHARS).len(), 3);

        let one = [
            waiting[0].clone(),
            account(4, "Qwen 2.5 - 7B", AccountStatus::SignedIn),
        ];
        let ready = Setup::of(&[pool("Empty", &[])], &one, true);
        assert!(
            ready.done(),
            "an account outside every pool still counts as ready for agents"
        );
        assert_eq!(
            ready.account_line("…"),
            "Qwen 2.5 - 7B is ready for agents."
        );
        let long = Setup::of(
            &[],
            &[account(6, &"Seat ".repeat(19), AccountStatus::SignedIn)],
            true,
        );
        assert_eq!(
            long.account_line("…"),
            "Seat Seat Seat Seat Sea… is ready for agents."
        );
        assert_eq!(wrap(&long.account_line("…"), TOOLS_TEXT_CHARS).len(), 2);
        assert!(!Setup::of(&[pool("Empty", &[])], &one, false).done());
        let two = [
            one[1].clone(),
            account(5, "Team key", AccountStatus::KeySaved),
        ];
        assert_eq!(
            Setup::of(&pools, &two, true).account_line("…"),
            "2 accounts are ready for agents."
        );
    }

    #[test]
    fn home_reads_accounts_on_each_refresh_only_until_one_is_ready_for_agents() {
        let account_reads = |shell: &mut Shell| {
            std::iter::from_fn(|| shell.ctx.next_request())
                .filter(|request| {
                    matches!(
                        request,
                        Request::Settings(job) if matches!(job.as_ref(), SettingsJob::AllAccounts)
                    )
                })
                .count()
        };
        let mut ctx = ctx(80, 24);
        ctx.store.accounts =
            Load::Ready(vec![account(1, "ChaoticQubit", AccountStatus::NotSignedIn)]);
        let mut shell = Shell::with(ctx, Box::new(Home::new()));
        assert_eq!(account_reads(&mut shell), 1);
        shell.refresh();
        assert_eq!(
            account_reads(&mut shell),
            1,
            "while no account is ready, each refresh reads them again"
        );
        shell.ctx.store.accounts =
            Load::Ready(vec![account(2, "Team key", AccountStatus::KeySaved)]);
        shell.refresh();
        assert_eq!(
            account_reads(&mut shell),
            0,
            "a ready account is not read again on every refresh"
        );
    }

    #[test]
    fn found_and_installed_tools_count_and_every_other_state_is_missing() {
        let mut status = tools(&[]);
        assert_eq!(missing_tools(&status), Vec::<String>::new());
        status.dependencies[0].state = DependencyState::Installed {
            version: Version::parse("3.5.0").unwrap(),
        };
        status.dependencies[1].state = DependencyState::Installing;
        status.dependencies[2].state = DependencyState::Failed {
            reason: "setup_uv_missing".into(),
        };
        assert_eq!(missing_tools(&status), ["uv", "Graphify"]);
        status.dependencies[1].state = DependencyState::Checking;
        assert_eq!(missing_tools(&status), ["uv", "Graphify"]);
    }

    #[test]
    fn get_set_up_stays_until_tools_are_found_and_opens_setup() {
        let mut ctx = ctx(80, 24);
        ctx.store.projects = Load::Ready(Vec::new());
        ctx.store.pools = Load::Ready(vec![pool("Main", &[MemberStatus::SetUp])]);
        ctx.store.accounts = Load::Ready(vec![account(1, "Team key", AccountStatus::KeySaved)]);
        let mut shell = Shell::with(ctx, Box::new(Home::new()));
        assert_eq!(status_reads(&mut shell), 1, "Home asks for the setup check");
        let rows = render(&mut shell, 80, 24);
        assert!(
            rows.iter().any(|row| row.contains("Get set up")),
            "{rows:#?}"
        );
        assert!(
            rows.iter().any(|row| row.contains("[ ] Install tools")),
            "checking counts as not done: {rows:#?}"
        );
        assert!(
            rows.iter()
                .any(|row| row.contains("Agents need tmux, uv and")),
            "{rows:#?}"
        );
        assert!(
            rows.iter()
                .all(|row| !row.contains("Failed") && !row.contains("setup_")),
            "{rows:#?}"
        );

        let mut still = tools(&[]);
        still.dependencies[0].state = DependencyState::Checking;
        shell.arrive(status(still));
        let rows = render(&mut shell, 80, 24);
        assert!(
            rows.iter().any(|row| row.contains("[ ] Install tools")),
            "{rows:#?}"
        );
        assert!(
            rows.iter().all(|row| !row.contains("Missing")),
            "a tool still being checked is not called missing: {rows:#?}"
        );
        shell.refresh();
        assert_eq!(
            status_reads(&mut shell),
            1,
            "Home reads the check again until it finishes"
        );

        shell.arrive(status(tools(&[Dependency::Uv, Dependency::Graphify])));
        assert!(matches!(shell.ctx.store.setup, Load::Ready(_)));
        let rows = render(&mut shell, 80, 24);
        assert!(
            rows.iter().any(|row| row.contains("[ ] Install tools")),
            "{rows:#?}"
        );
        assert!(
            rows.iter()
                .any(|row| row.contains("Missing: uv, Graphify.")),
            "{rows:#?}"
        );
        assert!(
            rows.iter().any(|row| row.contains("[x] Add an account")),
            "{rows:#?}"
        );
        assert!(
            rows.iter().all(|row| row.chars().count() <= 80),
            "{rows:#?}"
        );
        shell.refresh();
        assert_eq!(
            status_reads(&mut shell),
            0,
            "a known check is not read again"
        );

        for _ in 0..3 {
            shell.key(Key::Down);
        }
        let rows = render(&mut shell, 80, 24);
        assert!(
            rows.last().unwrap().contains("Enter Install tools"),
            "{rows:#?}"
        );
        shell.key(Key::Enter);
        assert_eq!(shell.place(), Some(Place::Setup { first_run: false }));
        shell.key(Key::Esc);
        assert_eq!(shell.place(), Some(Place::Home));

        shell.arrive(status(tools(&[])));
        assert_eq!(
            shell
                .ctx
                .message()
                .map(|message| (message.lead.clone(), message.rest.clone())),
            Some(("Setup done.".into(), "Team key is ready for agents.".into()))
        );
        let rows = render(&mut shell, 80, 24);
        assert!(
            rows.iter().all(|row| !row.contains("Get set up")),
            "the panel hides once accounts, a pool and tools are done: {rows:#?}"
        );
    }
}
