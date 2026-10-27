pub mod add_folder;
pub mod add_project;
pub mod app_setup;
pub(in crate::tui) mod changes;
pub mod clone_repo;
pub mod home;
pub mod home_folder;
pub(in crate::tui) mod intake;
pub(in crate::tui) mod new_project;
mod new_project_input;
pub(in crate::tui) mod new_project_page;
pub(in crate::tui) mod new_project_view;
pub mod onboarding;
pub mod profile_form;
pub(in crate::tui) mod project;
pub(in crate::tui) mod project_markdown;
pub(in crate::tui) mod project_master;
pub(in crate::tui) mod project_page;
pub(in crate::tui) mod project_plugins;
#[cfg_attr(
    not(all(test, unix)),
    expect(
        dead_code,
        reason = "v1-tui-projects plugs the live team view into the project screen"
    )
)]
pub(in crate::tui) mod project_team;
pub(in crate::tui) mod project_time;
pub(in crate::tui) mod project_view;
pub mod settings;
pub mod settings_accounts;
pub mod settings_add_account;
pub mod settings_harnesses;
pub mod settings_memory;
pub mod settings_plugins;
pub mod startup;
pub mod team;
pub mod welcome;

#[cfg(all(test, unix))]
mod intake_tests;
#[cfg(all(test, unix))]
mod new_project_frames;
#[cfg(all(test, unix))]
mod new_project_tests;
#[cfg(all(test, unix))]
mod project_frames;
#[cfg(all(test, unix))]
mod project_page_tests;
#[cfg(all(test, unix))]
mod project_tests;

use super::ctx::Ctx;
use super::frame::{Hint, Modal};
use super::keys::Key;
use super::seam::Arrival;
use crate::protocol::{CredentialRef, ProjectId};
use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::text::Line;

/// A Settings section, in tab order.
/// Input: none. Output: one section; `SettingsSection::ALL` gives the order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingsSection {
    Accounts,
    Pools,
    Harnesses,
    Plugins,
    Memory,
    Profile,
}

impl SettingsSection {
    /// Every section in tab order.
    pub const ALL: [SettingsSection; 6] = [
        Self::Accounts,
        Self::Pools,
        Self::Harnesses,
        Self::Plugins,
        Self::Memory,
        Self::Profile,
    ];

    /// The plain label.
    /// Input: none. Output: "Accounts", "Plugins and skills" and so on.
    pub fn label(self) -> &'static str {
        match self {
            Self::Accounts => "Accounts",
            Self::Pools => "Pools",
            Self::Harnesses => "Harnesses",
            Self::Plugins => "Plugins and skills",
            Self::Memory => "Memory",
            Self::Profile => "Profile",
        }
    }

    /// The neighbouring section, wrapping at both ends.
    /// Input: direction. Output: the next or previous section.
    pub fn step(self, forward: bool) -> Self {
        let index = Self::ALL
            .iter()
            .position(|section| *section == self)
            .expect("every section is listed");
        let count = Self::ALL.len();
        Self::ALL[if forward {
            (index + 1) % count
        } else {
            (index + count - 1) % count
        }]
    }
}

/// A Project tab, in tab order.
/// Input: none. Output: one tab.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProjectTab {
    Overview,
    Conversation,
    Agents,
    Changes,
    Context,
    Settings,
}

impl ProjectTab {
    /// Every tab in order.
    pub const ALL: [ProjectTab; 6] = [
        Self::Overview,
        Self::Conversation,
        Self::Agents,
        Self::Changes,
        Self::Context,
        Self::Settings,
    ];

    /// The plain label.
    /// Input: none. Output: "Overview", "Conversation" and so on.
    pub fn label(self) -> &'static str {
        match self {
            Self::Overview => "Overview",
            Self::Conversation => "Conversation",
            Self::Agents => "Agents",
            Self::Changes => "Changes",
            Self::Context => "Context",
            Self::Settings => "Settings",
        }
    }
}

/// The sentence Quit shows when a New project draft would be lost, whether the
/// wizard is open or its draft was kept after leaving it.
pub const DRAFT_LOSS: &str = "Your new project draft will be lost.";

/// A place the owner can go to; the registry builds its screen.
/// Input: from Go to, Home and Settings. Output: `open` returns its screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Place {
    Home,
    Welcome,
    Settings(SettingsSection),
    AddAccount,
    NewPool,
    NewPoolFrom(Vec<CredentialRef>),
    EditPool(CredentialRef),
    NewProject,
    Project {
        id: ProjectId,
        name: String,
        tab: ProjectTab,
    },
    Setup {
        first_run: bool,
    },
    Team {
        id: ProjectId,
        name: String,
    },
    Master {
        id: ProjectId,
        name: String,
    },
    HomeFolder {
        first_run: bool,
    },
    AddProject,
    AddFolder,
    CloneRepo,
    Onboarding {
        path: String,
        name: String,
        project: Option<ProjectId>,
    },
}

/// What the shell does after a screen handled a key or an arrival.
/// Input: returned by screens. Output: the shell changes the stack, or applies
/// its default for an ignored key (Esc goes back).
pub enum Flow {
    Handled,
    Ignored,
    Open(Place),
    Replace(Box<dyn Screen>),
    Back,
}

impl std::fmt::Debug for Flow {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Handled => formatter.write_str("Handled"),
            Self::Ignored => formatter.write_str("Ignored"),
            Self::Open(place) => write!(formatter, "Open({place:?})"),
            Self::Replace(screen) => write!(formatter, "Replace({:?})", screen.crumbs()),
            Self::Back => formatter.write_str("Back"),
        }
    }
}

/// One screen of the TUI. The shell draws the frame around it, routes keys and
/// engine arrivals to it, and keeps a stack of screens.
/// Input: keys after the global keys, arrivals from the engine. Output: body
/// drawing, header crumbs and status, help-line hints, an optional modal.
///
/// The rebuilt shell is the default; `CRAZYAGENT_TUI=classic` keeps the
/// tabbed shell. The design is
/// `docs/design/tui/DESIGN.md` and `tui/frames/` holds its text renders.
///
/// Adding a screen:
/// 1. Implement this trait. Add a `Place` when Go to or another screen opens
///    it, and build it in `open`.
/// 2. Move with `Flow`: `Open(place)` pushes, `Replace(screen)` swaps the top,
///    `Back` pops. An `Esc` returned as `Ignored` goes back.
/// 3. Ask for engine data with `ctx.request(Request::..)` in `shown`. Shared
///    data lands in `ctx.store`; every open screen sees each `Arrival`.
/// 4. Add key tests next to the screen and a render test against a design
///    frame in `render_tests.rs`.
///
/// Shared parts: colors only from `theme`, symbols only from `glyphs`;
/// `widgets::TextField` (plain, multiline, secret with wipe and
/// `take_secret`), `buttons`, `radio`, `checkbox`, `menu_item`;
/// `frame::Modal`, `frame::tab_strip`, `settings::draw_page`;
/// `text::list_row`, `text::cells`, `text::with_bar` and `text::thumb` for
/// long lists. A status is a glyph and a word. Never show invented data:
/// missing engine support shows "Not ready yet".
pub trait Screen {
    /// Breadcrumbs after "CrazyAgent", such as ["Settings", "Accounts"].
    fn crumbs(&self) -> Vec<String>;

    /// The status at the right of the header; empty by default.
    fn status(&self, _ctx: &Ctx) -> Line<'static> {
        Line::default()
    }

    /// Keys for the help line, in priority order.
    fn hints(&self, ctx: &Ctx) -> Vec<Hint>;

    /// Keys for "This screen" in the Keys overlay; the help line by default.
    fn keys(&self, ctx: &Ctx) -> Vec<Hint> {
        self.hints(ctx)
    }

    /// True while a text field has focus, so letters type instead of acting.
    fn typing(&self) -> bool {
        false
    }

    /// True while an agent terminal has focus: every key, Ctrl-C and Esc
    /// included, goes to the screen and the shell's own keys wait.
    fn raw(&self) -> bool {
        false
    }

    /// Handles one key the shell did not take as a global key.
    fn key(&mut self, key: &Key, ctx: &mut Ctx) -> Flow;

    /// Moves to the previous or next tab, section or step (Shift-←→).
    fn step(&mut self, _forward: bool, _ctx: &mut Ctx) -> Flow {
        Flow::Ignored
    }

    /// Reacts to engine data; every open screen sees every arrival.
    fn arrive(&mut self, _arrival: &Arrival, _ctx: &mut Ctx) -> Flow {
        Flow::Handled
    }

    /// Called when the screen comes to the top of the stack, to refresh data.
    fn shown(&mut self, _ctx: &mut Ctx) {}

    /// Called when the window size changed while the screen is on top.
    fn resized(&mut self, _ctx: &mut Ctx) {}

    /// Draws the body and returns the cursor position for a focused field.
    fn draw(&mut self, frame: &mut Frame, area: Rect, ctx: &Ctx) -> Option<Position>;

    /// A modal the screen shows over itself, with its help-line keys.
    fn modal(&self, _ctx: &Ctx) -> Option<(Modal, Vec<Hint>)> {
        None
    }

    /// True while a spinner shows, so the shell ticks every 100 ms.
    fn busy(&self, _ctx: &Ctx) -> bool {
        false
    }

    /// True while leaving would lose typed work, so Quit asks first.
    fn draft(&self) -> bool {
        false
    }

    /// The sentence Quit shows for this screen's unsaved work.
    /// Input: none. Output: a plain sentence; read only while `draft` is true.
    fn loss(&self) -> &'static str {
        DRAFT_LOSS
    }

    /// The place this screen stands for, used by Go to to avoid duplicates.
    fn place(&self) -> Option<Place> {
        None
    }
}

/// Builds the screen for a place.
/// Inputs: the place and the context. Output: a fresh screen; places whose
/// screens are not built yet return the "Not ready yet" screen.
pub fn open(place: &Place, ctx: &mut Ctx) -> Box<dyn Screen> {
    let screen: Box<dyn Screen> = match place {
        Place::Home => Box::new(home::Home::new()),
        Place::Welcome => Box::new(welcome::Welcome::new(ctx)),
        Place::Settings(section) => settings::open(*section, ctx),
        Place::AddAccount => settings::open_add_account(ctx),
        Place::NewPool => Box::new(settings::pool_editor::PoolEditor::new(None, ctx)),
        Place::NewPoolFrom(ids) => {
            Box::new(settings::pool_editor::PoolEditor::with_members(ids, ctx))
        }
        Place::EditPool(id) => Box::new(settings::pool_editor::PoolEditor::new(Some(id), ctx)),
        Place::NewProject => Box::new(new_project_page::NewProjectPage::new(ctx)),
        Place::Setup { first_run } => Box::new(app_setup::AppSetup::new(*first_run)),
        Place::Team { id, name } => Box::new(team::Team::new(id.clone(), name.clone())),
        Place::Master { id, name } => Box::new(team::Team::master(id.clone(), name.clone())),
        Place::HomeFolder { first_run } => Box::new(home_folder::HomeFolder::new(*first_run, ctx)),
        Place::AddProject => Box::new(add_project::AddProject::new()),
        Place::AddFolder => Box::new(add_folder::AddFolder::new(ctx)),
        Place::CloneRepo => Box::new(clone_repo::CloneRepo::new()),
        Place::Onboarding {
            path,
            name,
            project,
        } => Box::new(onboarding::OnboardingPage::new(
            path.clone(),
            name.clone(),
            project.clone(),
        )),
        Place::Project { id, name, tab } => Box::new(project_page::ProjectPage::new(
            id.clone(),
            name.clone(),
            *tab,
            ctx,
        )),
    };
    screen
}
