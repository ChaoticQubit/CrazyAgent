use super::new_project_input::TextInput;
#[cfg(all(test, unix))]
use crate::config::EffectiveConfig;
use crate::project_setup::{
    ABOUT_BYTES_MAX, DOCUMENT_BYTES_MAX, DOCUMENT_EXTENSIONS, DOCUMENTS_MAX, HARNESS_AGENTS_MAX,
    HarnessSlot, PROJECT_DOCUMENT_BYTES_MAX, Platform, ProjectCatalog, ProjectSetup, ResearchKind,
    SUB_HARNESSES_MAX, TEAM_AGENTS_MAX, TECH_ENTRIES_MAX, TECH_ENTRY_BYTES_MAX, TeamPlan,
};
use crate::protocol::{CredentialRef, ProjectSpec};
use crate::tui::text::coded;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::path::{Path, PathBuf};

pub(in crate::tui) const NAME_BYTES_MAX: usize = 256;
pub(in crate::tui) const PLATFORM_ORDER: [Platform; 6] = [
    Platform::Website,
    Platform::MobileApp,
    Platform::DesktopApp,
    Platform::ApiBackend,
    Platform::Cli,
    Platform::Other,
];
const PATH_BYTES_MAX: usize = 1024;
const COUNT_BYTES_MAX: usize = 2;

/// Names the seven New project steps in order.
/// Each step has a full title for the rail and a short label for the 80-column tab strip.
/// The order is the order of the wizard.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(in crate::tui) enum Step {
    About,
    Stack,
    Documents,
    Work,
    Pool,
    Team,
    Review,
}

impl Step {
    pub const ALL: [Step; 7] = [
        Self::About,
        Self::Stack,
        Self::Documents,
        Self::Work,
        Self::Pool,
        Self::Team,
        Self::Review,
    ];

    /// Gives the step title shown in the rail and the breadcrumb.
    /// Takes the step.
    /// Returns the title, for example "Stack and platforms".
    pub fn title(self) -> &'static str {
        match self {
            Self::About => "About",
            Self::Stack => "Stack and platforms",
            Self::Documents => "Documents",
            Self::Work => "Work to start",
            Self::Pool => "Pool",
            Self::Team => "Team",
            Self::Review => "Review",
        }
    }

    /// Gives the short label used by the narrow tab strip.
    /// Takes the step.
    /// Returns the label, for example "Stack".
    pub fn short(self) -> &'static str {
        match self {
            Self::Stack => "Stack",
            Self::Work => "Work",
            other => other.title(),
        }
    }

    /// Gives the position of the step in the wizard.
    /// Takes the step.
    /// Returns 0 for About through 6 for Review.
    pub fn index(self) -> usize {
        self as usize
    }
}

/// Names each focusable control of the wizard.
/// The focused control decides what keys do and which help line shows.
/// Text controls take printable keys while typing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::tui) enum Control {
    Name,
    About,
    Grid,
    Custom,
    Platforms,
    Path,
    Documents,
    Work,
    Pools,
    Team,
    Min,
    Max,
    Back,
    Continue,
}

impl Control {
    fn text(self) -> bool {
        matches!(
            self,
            Self::Name | Self::About | Self::Custom | Self::Path | Self::Min | Self::Max
        )
    }
}

/// One document picked for the new project.
/// Holds the absolute path the engine copies, the file name and its size.
/// The kind label comes from the extension.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::tui) struct Document {
    pub path: String,
    pub name: String,
    pub bytes: u64,
}

impl Document {
    /// Names the document type in plain words.
    /// Takes the document.
    /// Returns a label such as "PDF" or "Markdown".
    pub fn kind(&self) -> &'static str {
        match extension(&self.name).as_str() {
            "pdf" => "PDF",
            "md" => "Markdown",
            "txt" => "Text",
            "docx" => "Word",
            "csv" => "CSV",
            "png" => "PNG image",
            "jpg" | "jpeg" => "JPEG image",
            _ => "File",
        }
    }
}

/// One pool the owner can pick, read from the engine settings.
/// Holds the pool reference, its plain name, its account count and accounts per harness.
/// The reference is sent to the engine and never shown.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::tui) struct PoolChoice {
    pub id: CredentialRef,
    pub name: String,
    pub accounts: usize,
    pub harnesses: Vec<(String, usize)>,
}

impl PoolChoice {
    /// Counts the pool's accounts that run on one harness.
    /// Takes the harness id.
    /// Returns the count, zero when none.
    pub fn accounts_for(&self, harness: &str) -> usize {
        self.harnesses
            .iter()
            .filter(|(id, _)| same_harness(id, harness))
            .map(|(_, count)| *count)
            .sum()
    }
}

/// Says whether two harness ids name the same harness.
/// Takes ids from a pool, a team plan or the catalog; "claude" and "claude-code" are one harness.
/// Returns true when the engine resolves both to the same catalog entry, or the ids are equal.
pub(in crate::tui) fn same_harness(a: &str, b: &str) -> bool {
    let catalog_id = |id: &str| crate::harnesses::spec(id).map(|spec| spec.id);
    a == b || catalog_id(a).is_some_and(|id| catalog_id(b) == Some(id))
}

/// Builds the pool choices from the engine's effective settings.
/// Takes the configuration from the latest snapshot.
/// Returns every pool in settings order with plain names and counts.
#[cfg(all(test, unix))]
pub(in crate::tui) fn pool_choices(config: &EffectiveConfig) -> Vec<PoolChoice> {
    let Some(access) = config.settings.model_access.as_ref() else {
        return Vec::new();
    };
    access
        .pools
        .iter()
        .map(|pool| {
            let mut harnesses: Vec<(String, usize)> = Vec::new();
            for connection in access
                .connections
                .iter()
                .filter(|c| pool.members.contains(&c.id))
            {
                let id = connection.harness.as_str();
                match harnesses.iter_mut().find(|(known, _)| known == id) {
                    Some((_, count)) => *count += 1,
                    None => harnesses.push((id.to_owned(), 1)),
                }
            }
            PoolChoice {
                id: pool.id.clone(),
                name: pool
                    .name
                    .clone()
                    .filter(|name| !name.trim().is_empty())
                    .unwrap_or_else(|| "Unnamed pool".into()),
                accounts: pool.members.len(),
                harnesses,
            }
        })
        .collect()
}

/// States whether a harness is on this machine and CrazyAgent can start it.
/// CantStart is installed but cannot run team agents yet; Unknown is used until the engine's catalog arrives.
/// The catalog's own words decide; the wizard never guesses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::tui) enum HarnessState {
    Ready,
    NotInstalled,
    CantStart,
    Unknown,
}

/// One harness the Team step can pick.
/// Holds the engine id, the plain name, its state and the engine's limited-control sentence, if any.
/// Harnesses that are not installed or cannot run team agents are listed but cannot be picked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::tui) struct HarnessChoice {
    pub id: String,
    pub name: String,
    pub state: HarnessState,
    pub limited: Option<String>,
}

/// Says which team row the harness picker fills.
/// Master is the leading harness, Slot an existing sub-harness row, NewSlot the "+ Add harness" row.
/// The index refers to the team's sub-harness list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::tui) enum PickFor {
    Master,
    Slot(usize),
    NewSlot,
}

/// The open harness picker over the Team step.
/// Holds the row being filled and the focused choice.
/// Closing it changes nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::tui) struct Picker {
    pub target: PickFor,
    pub cursor: usize,
}

/// Where the wizard is in its life.
/// Editing takes input, Creating waits for the engine, Failed shows why creation did not happen.
/// Failed names the step that can fix the problem.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::tui) enum Phase {
    Editing,
    Creating {
        pool: bool,
        created: bool,
    },
    Failed {
        title: String,
        detail: String,
        step: Step,
    },
}

/// What the wizard asks its host to do after a key.
/// Ignored lets global keys run; Create carries the validated request for the engine.
/// The host owns navigation, pool creation and engine calls.
#[derive(Debug, PartialEq, Eq)]
pub(in crate::tui) enum Reply {
    Ignored,
    Handled,
    Leave,
    Background,
    CreatePool,
    CheckHarnesses,
    Create(Box<(ProjectSpec, ProjectSetup)>),
}

/// A message under one control: an error or a success such as "Added".
/// Holds the control it belongs to and the plain sentence.
/// Errors say what to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::tui) struct Note {
    pub control: Control,
    pub text: String,
    pub error: bool,
}

impl Note {
    fn error(control: Control, text: impl Into<String>) -> Self {
        Self {
            control,
            text: text.into(),
            error: true,
        }
    }
}

type CatalogKey = (Option<CredentialRef>, Vec<ResearchKind>);

/// The New project wizard: every field, the focused control and the creation phase.
/// Takes keys and engine results; produces a validated project request on Create.
/// The draft lives here until the project exists, so Esc keeps it.
pub(in crate::tui) struct Wizard {
    pub step: Step,
    pub done: [bool; 7],
    focus: usize,
    pub typing: bool,
    pub width: u16,
    pub home: Option<PathBuf>,
    pub name: TextInput,
    pub about: TextInput,
    pub catalog: Option<ProjectCatalog>,
    catalog_for: Option<CatalogKey>,
    catalog_pending: Option<CatalogKey>,
    pub catalog_problem: Option<String>,
    pub tech: Vec<String>,
    pub filter: TextInput,
    pub filtering: bool,
    pub grid: usize,
    pub custom: TextInput,
    pub platforms: Vec<Platform>,
    pub platform_cursor: usize,
    pub path: TextInput,
    pub documents: Vec<Document>,
    pub document_cursor: usize,
    pub research: Vec<ResearchKind>,
    pub work_cursor: usize,
    pub pools: Vec<PoolChoice>,
    pub pool: Option<CredentialRef>,
    pub team: Option<TeamPlan>,
    pub team_edited: bool,
    pub team_row: usize,
    pub min: TextInput,
    pub max: TextInput,
    pub harnesses: Option<Vec<HarnessChoice>>,
    pub picker: Option<Picker>,
    pub note: Option<Note>,
    pub phase: Phase,
}

impl Wizard {
    /// Starts an empty draft on the About step with the name field focused.
    /// Takes the pools from the engine settings and the owner's home folder for `~` paths.
    /// Returns the wizard; the first pool is preselected and shown again in Review.
    pub fn new(pools: Vec<PoolChoice>, home: Option<PathBuf>) -> Self {
        let pool = pools.first().map(|pool| pool.id.clone());
        let wizard = Self {
            step: Step::About,
            done: [false; 7],
            focus: 0,
            typing: true,
            width: 80,
            home,
            name: TextInput::single(NAME_BYTES_MAX),
            about: TextInput::multi(ABOUT_BYTES_MAX),
            catalog: None,
            catalog_for: None,
            catalog_pending: None,
            catalog_problem: None,
            tech: Vec::new(),
            filter: TextInput::single(TECH_ENTRY_BYTES_MAX),
            filtering: false,
            grid: 0,
            custom: TextInput::single(TECH_ENTRY_BYTES_MAX),
            platforms: Vec::new(),
            platform_cursor: 0,
            path: TextInput::single(PATH_BYTES_MAX),
            documents: Vec::new(),
            document_cursor: 0,
            research: Vec::new(),
            work_cursor: 0,
            pools,
            pool,
            team: None,
            team_edited: false,
            team_row: 0,
            min: TextInput::single(COUNT_BYTES_MAX),
            max: TextInput::single(COUNT_BYTES_MAX),
            harnesses: None,
            picker: None,
            note: None,
            phase: Phase::Editing,
        };
        wizard.assert_consistent();
        wizard
    }

    /// Replaces the pool list after the engine settings change.
    /// Takes the new choices; a picked pool that no longer exists becomes "No pool yet".
    /// Returns nothing.
    pub fn set_pools(&mut self, pools: Vec<PoolChoice>) {
        if self
            .pool
            .as_ref()
            .is_some_and(|picked| !pools.iter().any(|pool| &pool.id == picked))
        {
            self.pool = None;
        }
        self.pools = pools;
        self.assert_consistent();
    }

    /// Stores the harness catalog for the Team step and its picker.
    /// Takes every harness the engine knows, installed or not.
    /// Returns nothing.
    pub fn set_harnesses(&mut self, harnesses: Vec<HarnessChoice>) {
        self.harnesses = Some(harnesses);
        if let Some(picker) = self.picker.as_mut() {
            picker.cursor = 0;
        }
        self.fit_team();
    }

    fn fit_team(&mut self) {
        if self.team_edited {
            return;
        }
        let (Some(harnesses), Some(team)) = (&self.harnesses, &self.team) else {
            return;
        };
        let missing = |id: &str| {
            harnesses.iter().any(|h| {
                same_harness(&h.id, id)
                    && matches!(
                        h.state,
                        HarnessState::NotInstalled | HarnessState::CantStart
                    )
            })
        };
        let ready = || harnesses.iter().filter(|h| h.state == HarnessState::Ready);
        let Some(best) = ready()
            .find(|h| self.pool_accounts(&h.id).is_some_and(|count| count > 0))
            .or_else(|| ready().next())
            .map(|h| h.id.clone())
        else {
            return;
        };
        let mut fitted = team.clone();
        if missing(&fitted.master) {
            fitted.master = best.clone();
        }
        for slot in &mut fitted.sub_harnesses {
            if missing(&slot.harness) {
                slot.harness = best.clone();
            }
        }
        if &fitted != team {
            assert_eq!(fitted.total_agents(), team.total_agents());
            self.set_team(fitted);
        }
    }

    /// Gives the limited-control sentence of the harness on the focused team row.
    /// Takes the wizard; only the engine catalog has these sentences.
    /// Returns "<harness>: Limited control: <reason>." or none.
    pub fn limited_note(&self) -> Option<String> {
        let team = self.team.as_ref()?;
        let id = match self.team_row {
            0 => &team.master,
            row => &team.sub_harnesses.get(row - 1)?.harness,
        };
        let choice = self
            .harnesses
            .as_ref()?
            .iter()
            .find(|h| same_harness(&h.id, id))?;
        Some(format!("{}: {}", choice.name, choice.limited.as_ref()?))
    }

    /// Gives the controls of the current step in Tab order.
    /// Takes the wizard.
    /// Returns at least the Back and Continue buttons.
    pub fn controls(&self) -> Vec<Control> {
        let mut controls = match self.step {
            Step::About => vec![Control::Name, Control::About],
            Step::Stack => vec![Control::Grid, Control::Custom, Control::Platforms],
            Step::Documents if self.documents.is_empty() => vec![Control::Path],
            Step::Documents => vec![Control::Path, Control::Documents],
            Step::Work => vec![Control::Work],
            Step::Pool => vec![Control::Pools],
            Step::Team if self.team.is_some() => vec![Control::Team, Control::Min, Control::Max],
            Step::Team | Step::Review => Vec::new(),
        };
        controls.extend([Control::Back, Control::Continue]);
        controls
    }

    /// Gives the focused control.
    /// Takes the wizard.
    /// Returns the control under focus.
    pub fn focused(&self) -> Control {
        let controls = self.controls();
        controls[self.focus.min(controls.len() - 1)]
    }

    /// Moves focus to one control of the current step.
    /// Takes the control; a control not on this step leaves focus unchanged.
    /// Returns nothing.
    pub fn focus_on(&mut self, control: Control) {
        if let Some(index) = self.controls().iter().position(|c| *c == control) {
            self.focus = index;
            self.typing = control.text();
        }
    }

    /// Opens a step with its first control focused; Review focuses Create project.
    /// Takes the step.
    /// Returns nothing.
    pub fn go(&mut self, step: Step) {
        self.step = step;
        self.picker = None;
        self.filtering = false;
        self.note = None;
        self.focus = 0;
        if step == Step::Review {
            self.focus_on(Control::Continue);
        } else {
            self.typing = self.focused().text();
        }
        if step == Step::Team {
            self.note = self.check_team().err();
        }
        self.assert_consistent();
    }

    /// Says whether the wizard should read the engine's catalog for the current pool and research.
    /// Takes the wizard; marks the request as waiting.
    /// Returns the pool and research toggles to ask for, or none when already loaded or waiting.
    pub fn catalog_request(&mut self) -> Option<(Option<CredentialRef>, Vec<ResearchKind>)> {
        let key = (self.pool.clone(), self.research_sorted());
        if self.catalog_for.as_ref() == Some(&key) || self.catalog_pending.as_ref() == Some(&key) {
            return None;
        }
        self.catalog_pending = Some(key.clone());
        Some(key)
    }

    /// Reopens a kept draft: forgets a catalog read lost while it was away and any failure shown.
    /// Takes the wizard; the next `catalog_request` asks again.
    /// Returns nothing.
    pub fn resume(&mut self) {
        self.catalog_pending = None;
        self.picker = None;
        if matches!(self.phase, Phase::Failed { .. }) {
            self.phase = Phase::Editing;
        }
    }

    /// Applies the engine's catalog: choices, limits and the smart team defaults.
    /// Takes the pool and research it was read for and the engine result.
    /// Returns nothing; a team the owner edited is kept.
    pub fn apply_catalog(
        &mut self,
        pool: Option<CredentialRef>,
        research: Vec<ResearchKind>,
        result: Result<ProjectCatalog, String>,
    ) {
        let key = (pool, research);
        if self.catalog_pending.as_ref() == Some(&key) {
            self.catalog_pending = None;
        }
        match result {
            Ok(catalog) => {
                self.catalog_problem = None;
                let current = key == (self.pool.clone(), self.research_sorted());
                if current && !self.team_edited {
                    self.set_team(catalog.default_team.clone());
                    self.fit_team();
                }
                self.catalog = Some(catalog);
                self.catalog_for = Some(key);
            }
            Err(problem) => self.catalog_problem = Some(problem),
        }
        self.assert_consistent();
    }

    fn set_team(&mut self, team: TeamPlan) {
        self.min.set(&team.min_agents.to_string());
        self.max.set(&team.max_agents.to_string());
        self.team_row = self.team_row.min(team.sub_harnesses.len() + 1);
        self.team = Some(team);
        if self.step == Step::Team {
            self.note = self.check_team().err();
        }
    }

    /// Gives the research toggles in the engine's order.
    /// Takes the wizard.
    /// Returns the picked research kinds without duplicates.
    pub fn research_sorted(&self) -> Vec<ResearchKind> {
        ResearchKind::ALL
            .into_iter()
            .filter(|kind| self.research.contains(kind))
            .collect()
    }

    /// Gives the catalog's tech stack labels after the filter.
    /// Takes the wizard.
    /// Returns the matching labels in catalog order.
    pub fn tech_options(&self) -> Vec<String> {
        let words: Vec<String> = self
            .filter
            .value()
            .split_whitespace()
            .map(str::to_lowercase)
            .collect();
        self.catalog
            .as_ref()
            .map(|catalog| {
                catalog
                    .tech_stack
                    .iter()
                    .map(|option| option.label.clone())
                    .filter(|label| {
                        let label = label.to_lowercase();
                        words.iter().all(|word| label.contains(word))
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Gives the shape of the visible tech grid.
    /// Takes the wizard width: 4 columns of 5 at 100 columns and up, otherwise 3 of 4.
    /// Returns the columns and rows.
    pub fn grid_shape(&self) -> (usize, usize) {
        if self.width >= 100 { (4, 5) } else { (3, 4) }
    }

    fn grid_visible(&self) -> usize {
        let (columns, rows) = self.grid_shape();
        self.tech_options().len().min(columns * rows)
    }

    /// Gives the maximum documents, bytes per file and bytes per project.
    /// Takes the wizard; the engine catalog wins over the built-in limits.
    /// Returns the three limits.
    pub fn document_limits(&self) -> (usize, u64, u64) {
        self.catalog.as_ref().map_or(
            (
                DOCUMENTS_MAX,
                DOCUMENT_BYTES_MAX,
                PROJECT_DOCUMENT_BYTES_MAX,
            ),
            |catalog| {
                (
                    catalog.documents_max,
                    catalog.document_bytes_max,
                    catalog.project_document_bytes_max,
                )
            },
        )
    }

    /// Gives the plain name of the picked pool.
    /// Takes the wizard.
    /// Returns the name, or none for "No pool yet".
    pub fn pool_name(&self) -> Option<&str> {
        let picked = self.pool.as_ref()?;
        self.pools
            .iter()
            .find(|pool| &pool.id == picked)
            .map(|pool| pool.name.as_str())
    }

    fn picked_pool(&self) -> Option<&PoolChoice> {
        let picked = self.pool.as_ref()?;
        self.pools.iter().find(|pool| &pool.id == picked)
    }

    /// Counts the picked pool's accounts on one harness.
    /// Takes the harness id.
    /// Returns the count, or none without a pool.
    pub fn pool_accounts(&self, harness: &str) -> Option<usize> {
        self.picked_pool().map(|pool| pool.accounts_for(harness))
    }

    /// Gives the harnesses the picker lists.
    /// Takes the wizard; without the engine catalog it lists the harnesses of the pool's accounts with an unknown state.
    /// Returns the choices in display order.
    pub fn harness_choices(&self) -> Vec<HarnessChoice> {
        if let Some(harnesses) = &self.harnesses {
            return harnesses.clone();
        }
        let mut ids: Vec<String> = self
            .picked_pool()
            .map(|pool| pool.harnesses.iter().map(|(id, _)| id.clone()).collect())
            .unwrap_or_default();
        if let Some(team) = &self.team {
            for id in std::iter::once(&team.master)
                .chain(team.sub_harnesses.iter().map(|slot| &slot.harness))
            {
                if !ids.iter().any(|known| same_harness(known, id)) {
                    ids.push(id.clone());
                }
            }
        }
        ids.into_iter()
            .map(|id| HarnessChoice {
                name: harness_name(&id, None),
                id,
                state: HarnessState::Unknown,
                limited: None,
            })
            .collect()
    }

    /// Checks one step the way Continue does.
    /// Takes the step.
    /// Returns the first problem as a note under the control that can fix it.
    pub fn check(&self, step: Step) -> Result<(), Note> {
        match step {
            Step::About => self.check_name(),
            Step::Stack => {
                if self.tech.len() > TECH_ENTRIES_MAX {
                    return Err(Note::error(
                        Control::Grid,
                        format!("Pick at most {TECH_ENTRIES_MAX} for the tech stack."),
                    ));
                }
                Ok(())
            }
            Step::Documents => {
                let (count, _, total) = self.document_limits();
                if self.documents.len() > count {
                    return Err(Note::error(
                        Control::Path,
                        format!("A project holds at most {count} documents. Remove one."),
                    ));
                }
                if self.documents.iter().map(|d| d.bytes).sum::<u64>() > total {
                    return Err(Note::error(
                        Control::Path,
                        format!(
                            "These files add up to more than {}. Remove one.",
                            size_label(total)
                        ),
                    ));
                }
                Ok(())
            }
            Step::Work | Step::Pool | Step::Review => Ok(()),
            Step::Team => self.check_team(),
        }
    }

    fn check_name(&self) -> Result<(), Note> {
        let name = self.name.value().trim();
        if name.is_empty() {
            return Err(Note::error(Control::Name, "Give the project a name."));
        }
        assert!(name.len() <= NAME_BYTES_MAX);
        Ok(())
    }

    /// Checks the team the way the engine will: counts, Min, Max and installed harnesses.
    /// Takes the wizard; a missing team means the engine picks the defaults.
    /// Returns the first problem as a note under Team, Min or Max.
    pub fn check_team(&self) -> Result<(), Note> {
        let Some(team) = &self.team else {
            return Ok(());
        };
        let parse = |input: &TextInput, control| {
            input
                .value()
                .parse::<u8>()
                .ok()
                .filter(|value| (1..=TEAM_AGENTS_MAX).contains(value))
                .ok_or_else(|| {
                    Note::error(
                        control,
                        format!("Type a number from 1 to {TEAM_AGENTS_MAX}."),
                    )
                })
        };
        let min = parse(&self.min, Control::Min)?;
        let max = parse(&self.max, Control::Max)?;
        assert_eq!((min, max), (team.min_agents, team.max_agents));
        if let Some(harnesses) = &self.harnesses {
            let blocked = std::iter::once(&team.master)
                .chain(team.sub_harnesses.iter().map(|slot| &slot.harness))
                .find_map(|id| {
                    let state = harnesses.iter().find(|h| same_harness(&h.id, id))?.state;
                    let problem = match state {
                        HarnessState::NotInstalled => "is not installed",
                        HarnessState::CantStart => "can't run team agents yet",
                        HarnessState::Ready | HarnessState::Unknown => return None,
                    };
                    Some((id, problem))
                });
            if let Some((id, problem)) = blocked {
                return Err(Note::error(
                    Control::Team,
                    format!(
                        "{} {problem}. Pick another harness.",
                        harness_name(id, Some(harnesses))
                    ),
                ));
            }
        }
        let sub: u32 = team.sub_harnesses.iter().map(|s| u32::from(s.agents)).sum();
        let total = team.total_agents();
        if min > max {
            return Err(Note::error(
                Control::Min,
                format!("Min is {min}, but Max is {max}. Min can't be more than Max."),
            ));
        }
        if total > u32::from(max) {
            return Err(Note::error(
                Control::Max,
                format!(
                    "Sub-agents add up to {sub}, plus the master makes {total}, but Max is {max}. Raise Max or lower a count."
                ),
            ));
        }
        if total < u32::from(min) {
            return Err(Note::error(
                Control::Min,
                format!(
                    "The team has {total} with the master, but Min is {min}. Lower Min or add agents."
                ),
            ));
        }
        assert!(team.validate().is_ok());
        Ok(())
    }

    /// Builds the engine request after checking every step.
    /// Takes the wizard.
    /// Returns the project name and setup, or the first step and problem to fix.
    pub fn submission(&self) -> Result<(ProjectSpec, ProjectSetup), (Step, Note)> {
        for step in &Step::ALL[..Step::Review.index()] {
            self.check(*step).map_err(|note| (*step, note))?;
        }
        let spec = ProjectSpec {
            name: self.name.value().trim().to_owned(),
            repositories: Vec::new(),
        };
        let setup = ProjectSetup {
            about: self.about.value().trim().to_owned(),
            tech_stack: self.tech.clone(),
            platforms: Platform::ALL
                .into_iter()
                .filter(|platform| self.platforms.contains(platform))
                .collect(),
            research: self.research_sorted(),
            pool: self.pool.clone(),
            team: self.team.clone(),
            documents: self.documents.iter().map(|d| d.path.clone()).collect(),
        };
        if spec.validate().is_err() || setup.validate().is_err() {
            return Err((
                Step::Review,
                Note::error(
                    Control::Continue,
                    "This project can't be created as it is. Check each step.",
                ),
            ));
        }
        Ok((spec, setup))
    }

    /// Records that the engine created the project.
    /// Takes the wizard in the Creating phase.
    /// Returns nothing; the host assigns the pool next when one was picked.
    pub fn created(&mut self) {
        if let Phase::Creating { created, .. } = &mut self.phase {
            *created = true;
        }
    }

    /// Shows why creation failed and which step fixes it.
    /// Takes the engine's rejection code.
    /// Returns nothing; Enter goes to that step and the draft is kept.
    pub fn failed(&mut self, code: &str) {
        let (title, detail, step) = failure_copy(code, self.pool_name());
        self.phase = Phase::Failed {
            title: title.into(),
            detail,
            step,
        };
    }

    /// Handles one key for the focused control, the picker or the creation phase.
    /// Takes the key event.
    /// Returns what the host should do next.
    pub fn key(&mut self, key: KeyEvent) -> Reply {
        match &self.phase {
            Phase::Creating { .. } => {
                return if key.code == KeyCode::Esc {
                    Reply::Background
                } else {
                    Reply::Ignored
                };
            }
            Phase::Failed { step, .. } => {
                let step = *step;
                return match key.code {
                    KeyCode::Enter => {
                        self.phase = Phase::Editing;
                        self.go(step);
                        Reply::Handled
                    }
                    KeyCode::Esc => {
                        self.phase = Phase::Editing;
                        Reply::Leave
                    }
                    _ => Reply::Ignored,
                };
            }
            Phase::Editing => {}
        }
        if self.picker.is_some() {
            return self.picker_key(key);
        }
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        match key.code {
            KeyCode::Left if shift => return self.step_by(-1),
            KeyCode::Right if shift => return self.step_by(1),
            KeyCode::Esc if self.filtering => {
                self.filtering = false;
                self.filter.set("");
                self.grid = 0;
                return Reply::Handled;
            }
            KeyCode::Esc if self.typing => {
                self.typing = false;
                return Reply::Handled;
            }
            KeyCode::Esc => return Reply::Leave,
            KeyCode::Tab if self.focused() == Control::Path && !self.path.value().is_empty() => {
                if self.complete_path() {
                    return Reply::Handled;
                }
                return self.tab(true);
            }
            KeyCode::Tab => return self.tab(true),
            KeyCode::BackTab => return self.tab(false),
            _ => {}
        }
        if !control && !self.typing && !self.filtering {
            match key.code {
                KeyCode::Char('[') => return self.step_by(-1),
                KeyCode::Char(']') => return self.step_by(1),
                _ => {}
            }
        }
        let reply = self.control_key(key);
        self.assert_consistent();
        reply
    }

    /// Inserts pasted text into the focused text field.
    /// Takes the pasted text; single-line fields drop newlines.
    /// Returns Handled when a field took it.
    pub fn paste(&mut self, text: &str) -> Reply {
        if self.phase != Phase::Editing || self.picker.is_some() {
            return Reply::Ignored;
        }
        if self.filtering {
            self.filter.insert_str(text);
            self.grid = 0;
            return Reply::Handled;
        }
        let focused = self.focused();
        let Some(input) = self.input(focused) else {
            return Reply::Ignored;
        };
        input.insert_str(text);
        self.typing = true;
        self.edited(focused);
        Reply::Handled
    }

    fn input(&mut self, control: Control) -> Option<&mut TextInput> {
        match control {
            Control::Name => Some(&mut self.name),
            Control::About => Some(&mut self.about),
            Control::Custom => Some(&mut self.custom),
            Control::Path => Some(&mut self.path),
            Control::Min => Some(&mut self.min),
            Control::Max => Some(&mut self.max),
            _ => None,
        }
    }

    fn edited(&mut self, control: Control) {
        if self
            .note
            .as_ref()
            .is_some_and(|note| note.control == control)
        {
            self.note = None;
        }
        if matches!(control, Control::Min | Control::Max) {
            let min = self.min.value().parse::<u8>().ok();
            let max = self.max.value().parse::<u8>().ok();
            if let Some(team) = self.team.as_mut() {
                if let Some(min) = min.filter(|v| (1..=TEAM_AGENTS_MAX).contains(v)) {
                    team.min_agents = min;
                }
                if let Some(max) = max.filter(|v| (1..=TEAM_AGENTS_MAX).contains(v)) {
                    team.max_agents = max;
                }
            }
            self.team_edited = true;
            self.note = self.check_team().err();
        }
    }

    fn step_by(&mut self, offset: isize) -> Reply {
        let count = Step::ALL.len() as isize;
        let next = (self.step.index() as isize + offset).rem_euclid(count);
        self.go(Step::ALL[next as usize]);
        Reply::Handled
    }

    fn tab(&mut self, forward: bool) -> Reply {
        let leaving = self.focused();
        let count = self.controls().len();
        self.focus = if forward {
            (self.focus + 1) % count
        } else {
            (self.focus + count - 1) % count
        };
        self.typing = self.focused().text();
        if leaving == Control::Name
            && let Err(note) = self.check_name()
        {
            self.note = Some(note);
        }
        Reply::Handled
    }

    fn advance(&mut self) -> Reply {
        if self.step == Step::Review {
            return self.create();
        }
        match self.check(self.step) {
            Ok(()) => {
                self.done[self.step.index()] = true;
                self.go(Step::ALL[self.step.index() + 1]);
            }
            Err(note) => {
                self.focus_on(note.control);
                self.note = Some(note);
            }
        }
        Reply::Handled
    }

    fn create(&mut self) -> Reply {
        match self.submission() {
            Ok((spec, setup)) => {
                self.phase = Phase::Creating {
                    pool: setup.pool.is_some(),
                    created: false,
                };
                Reply::Create(Box::new((spec, setup)))
            }
            Err((step, note)) => {
                self.go(step);
                self.focus_on(note.control);
                self.note = Some(note);
                Reply::Handled
            }
        }
    }

    fn back(&mut self) -> Reply {
        if self.step == Step::About {
            return Reply::Leave;
        }
        self.go(Step::ALL[self.step.index() - 1]);
        Reply::Handled
    }

    fn control_key(&mut self, key: KeyEvent) -> Reply {
        let focused = self.focused();
        if self.filtering {
            return self.filter_key(key);
        }
        if focused.text() && !self.typing {
            if key.code == KeyCode::Enter {
                self.typing = true;
                return Reply::Handled;
            }
            return Reply::Ignored;
        }
        match focused {
            Control::Name | Control::About | Control::Custom | Control::Path => {
                self.text_key(focused, key)
            }
            Control::Min | Control::Max => self.count_key(focused, key),
            Control::Grid => self.grid_key(key),
            Control::Platforms => self.platform_key(key),
            Control::Documents => self.document_key(key),
            Control::Work => self.work_key(key),
            Control::Pools => self.pool_key(key),
            Control::Team => self.team_key(key),
            Control::Back if key.code == KeyCode::Enter => self.back(),
            Control::Continue if key.code == KeyCode::Enter => self.advance(),
            Control::Back | Control::Continue => Reply::Ignored,
        }
    }

    fn text_key(&mut self, focused: Control, key: KeyEvent) -> Reply {
        match (focused, key.code) {
            (Control::Name, KeyCode::Enter) => {
                if let Err(note) = self.check_name() {
                    self.note = Some(note);
                    return Reply::Handled;
                }
                self.focus_on(Control::About);
                Reply::Handled
            }
            (Control::About, KeyCode::Enter) => self.advance(),
            (Control::Custom, KeyCode::Enter) => {
                self.add_custom();
                Reply::Handled
            }
            (Control::Path, KeyCode::Enter) => {
                self.add_path();
                Reply::Handled
            }
            (Control::Path, KeyCode::Down) if !self.documents.is_empty() => {
                self.focus_on(Control::Documents);
                Reply::Handled
            }
            _ => {
                let Some(input) = self.input(focused) else {
                    return Reply::Ignored;
                };
                if !input.handle(key) {
                    return if matches!(key.code, KeyCode::Char(_))
                        && !key.modifiers.contains(KeyModifiers::CONTROL)
                    {
                        Reply::Handled
                    } else {
                        Reply::Ignored
                    };
                }
                self.edited(focused);
                Reply::Handled
            }
        }
    }

    fn count_key(&mut self, focused: Control, key: KeyEvent) -> Reply {
        match key.code {
            KeyCode::Enter if focused == Control::Min => {
                self.focus_on(Control::Max);
                Reply::Handled
            }
            KeyCode::Enter => self.advance(),
            KeyCode::Char(c)
                if !c.is_ascii_digit() && !key.modifiers.contains(KeyModifiers::CONTROL) =>
            {
                Reply::Handled
            }
            _ => {
                let Some(input) = self.input(focused) else {
                    return Reply::Ignored;
                };
                if !input.handle(key) {
                    return Reply::Ignored;
                }
                self.edited(focused);
                Reply::Handled
            }
        }
    }

    fn filter_key(&mut self, key: KeyEvent) -> Reply {
        match key.code {
            KeyCode::Enter => {
                self.filtering = false;
                Reply::Handled
            }
            _ => {
                if self.filter.handle(key) {
                    self.grid = 0;
                }
                Reply::Handled
            }
        }
    }

    fn grid_key(&mut self, key: KeyEvent) -> Reply {
        let (_, rows) = self.grid_shape();
        let visible = self.grid_visible();
        let row = self.grid % rows;
        match key.code {
            KeyCode::Char('/') => {
                self.filtering = true;
                return Reply::Handled;
            }
            KeyCode::Up if row > 0 => self.grid -= 1,
            KeyCode::Down if row + 1 < rows && self.grid + 1 < visible => self.grid += 1,
            KeyCode::Left if self.grid >= rows => self.grid -= rows,
            KeyCode::Right if self.grid + rows < visible => self.grid += rows,
            KeyCode::Char(' ') | KeyCode::Enter => {
                let Some(label) = self.tech_options().get(self.grid).cloned() else {
                    return Reply::Handled;
                };
                if let Some(at) = self.tech.iter().position(|t| t == &label) {
                    self.tech.remove(at);
                } else if self.tech.len() >= TECH_ENTRIES_MAX {
                    self.note = Some(Note::error(
                        Control::Grid,
                        format!("Pick at most {TECH_ENTRIES_MAX} for the tech stack."),
                    ));
                } else {
                    self.tech.push(label);
                }
            }
            KeyCode::Up | KeyCode::Down | KeyCode::Left | KeyCode::Right => {}
            _ => return Reply::Ignored,
        }
        Reply::Handled
    }

    fn add_custom(&mut self) {
        let entry = self.custom.value().trim().to_owned();
        let note = |text: String, error| Note {
            control: Control::Custom,
            text,
            error,
        };
        if entry.is_empty() {
            self.note = Some(note("Type a name first.".into(), true));
            return;
        }
        if self.tech.len() >= TECH_ENTRIES_MAX {
            self.note = Some(note(
                format!("Pick at most {TECH_ENTRIES_MAX} for the tech stack."),
                true,
            ));
            return;
        }
        let lower = entry.to_lowercase();
        if self.tech.iter().any(|t| t.to_lowercase() == lower) {
            self.note = Some(note(format!("{entry} is already in the list."), true));
            return;
        }
        let known = self.catalog.as_ref().and_then(|catalog| {
            catalog
                .tech_stack
                .iter()
                .find(|option| option.label.to_lowercase() == lower)
                .map(|option| option.label.clone())
        });
        self.tech.push(known.unwrap_or(entry));
        self.note = Some(note("Added".into(), false));
    }

    fn platform_key(&mut self, key: KeyEvent) -> Reply {
        let last = PLATFORM_ORDER.len() - 1;
        match key.code {
            KeyCode::Left => self.platform_cursor = self.platform_cursor.saturating_sub(1),
            KeyCode::Right => self.platform_cursor = (self.platform_cursor + 1).min(last),
            KeyCode::Up => self.platform_cursor = self.platform_cursor.saturating_sub(3),
            KeyCode::Down => self.platform_cursor = (self.platform_cursor + 3).min(last),
            KeyCode::Char(' ') | KeyCode::Enter => {
                let platform = PLATFORM_ORDER[self.platform_cursor];
                if let Some(at) = self.platforms.iter().position(|p| *p == platform) {
                    self.platforms.remove(at);
                } else {
                    self.platforms.push(platform);
                }
            }
            _ => return Reply::Ignored,
        }
        Reply::Handled
    }

    fn add_path(&mut self) {
        match self.documents_at(self.path.value()) {
            Ok(added) => {
                let count = added.len();
                self.documents.extend(added);
                self.document_cursor = self.documents.len() - count;
                self.path.set("");
                self.note = None;
            }
            Err(problem) => self.note = Some(Note::error(Control::Path, problem)),
        }
    }

    /// Reads the file or the folder's files at a typed path and checks them against the limits.
    /// Takes the typed path; `~` means the home folder.
    /// Returns the documents to add, or a plain sentence saying what to fix.
    pub fn documents_at(&self, typed: &str) -> Result<Vec<Document>, String> {
        let typed = typed.trim();
        if typed.is_empty() {
            return Err("Type the path of a file or folder.".into());
        }
        let path =
            expand(typed, self.home.as_deref()).ok_or("Type a full path, starting with / or ~.")?;
        let metadata =
            std::fs::metadata(&path).map_err(|_| format!("No file or folder at {typed}."))?;
        let files = if metadata.is_dir() {
            let mut entries: Vec<PathBuf> = std::fs::read_dir(&path)
                .map_err(|_| format!("Could not read the folder {typed}."))?
                .filter_map(|entry| entry.ok().map(|entry| entry.path()))
                .filter(|entry| {
                    entry.is_file()
                        && !entry
                            .file_name()
                            .and_then(|name| name.to_str())
                            .is_some_and(|name| name.starts_with('.'))
                        && allowed(entry)
                })
                .collect();
            entries.sort();
            if entries.is_empty() {
                return Err(format!(
                    "No PDF, Markdown, text, CSV, Word or image files in {typed}."
                ));
            }
            entries
        } else {
            vec![path]
        };
        let (count, per_file, total) = self.document_limits();
        let mut used: u64 = self.documents.iter().map(|d| d.bytes).sum();
        let mut added: Vec<Document> = Vec::new();
        for file in files {
            let real = std::fs::canonicalize(&file).map_err(|_| format!("No file at {typed}."))?;
            let name = real
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or("Use a file name with plain characters.")?
                .to_owned();
            let path = real
                .to_str()
                .ok_or("Use a path with plain characters.")?
                .to_owned();
            if !allowed(&real) {
                return Err(format!(
                    "{name} is not a supported type. Add PDF, Markdown, text, CSV, Word or image files."
                ));
            }
            let bytes = std::fs::metadata(&real)
                .map_err(|_| format!("Could not read {name}."))?
                .len();
            if bytes > per_file {
                return Err(format!(
                    "Too big: {name} is {}. The limit is {} per file.",
                    size_label(bytes),
                    size_label(per_file)
                ));
            }
            if self
                .documents
                .iter()
                .chain(added.iter())
                .any(|d| d.name == name)
            {
                return Err(format!("{name} is already in the list."));
            }
            if self.documents.len() + added.len() >= count {
                return Err(format!(
                    "A project holds at most {count} documents. Remove one first."
                ));
            }
            used += bytes;
            if used > total {
                return Err(format!(
                    "These files add up to more than {}. Remove one first.",
                    size_label(total)
                ));
            }
            added.push(Document { path, name, bytes });
        }
        assert!(!added.is_empty());
        Ok(added)
    }

    /// Completes the typed path from the folder's entries, like a shell.
    /// Takes the wizard's path field.
    /// Returns true when the text changed.
    pub fn complete_path(&mut self) -> bool {
        match complete(self.path.value(), self.home.as_deref(), false) {
            Some(completed) => {
                self.path.set(&completed);
                true
            }
            None => false,
        }
    }

    fn document_key(&mut self, key: KeyEvent) -> Reply {
        match key.code {
            KeyCode::Up if self.document_cursor == 0 => self.focus_on(Control::Path),
            KeyCode::Up => self.document_cursor -= 1,
            KeyCode::Down if self.document_cursor + 1 < self.documents.len() => {
                self.document_cursor += 1;
            }
            KeyCode::Down => {}
            KeyCode::Char('d') | KeyCode::Delete => {
                self.documents.remove(self.document_cursor);
                if self.documents.is_empty() {
                    self.document_cursor = 0;
                    self.focus_on(Control::Path);
                } else {
                    self.document_cursor = self.document_cursor.min(self.documents.len() - 1);
                }
            }
            _ => return Reply::Ignored,
        }
        Reply::Handled
    }

    fn work_key(&mut self, key: KeyEvent) -> Reply {
        let last = ResearchKind::ALL.len() - 1;
        match key.code {
            KeyCode::Up => self.work_cursor = self.work_cursor.saturating_sub(1),
            KeyCode::Down => self.work_cursor = (self.work_cursor + 1).min(last),
            KeyCode::Char(' ') | KeyCode::Enter => {
                let kind = ResearchKind::ALL[self.work_cursor];
                if let Some(at) = self.research.iter().position(|k| *k == kind) {
                    self.research.remove(at);
                } else {
                    self.research.push(kind);
                }
            }
            _ => return Reply::Ignored,
        }
        Reply::Handled
    }

    fn pool_key(&mut self, key: KeyEvent) -> Reply {
        let rows = self.pools.len() + 1;
        let row = self
            .pool
            .as_ref()
            .and_then(|picked| self.pools.iter().position(|pool| &pool.id == picked))
            .unwrap_or(self.pools.len());
        let row = match key.code {
            KeyCode::Up => row.saturating_sub(1),
            KeyCode::Down => (row + 1).min(rows - 1),
            KeyCode::Char('a') => return Reply::CreatePool,
            KeyCode::Enter => return self.advance(),
            _ => return Reply::Ignored,
        };
        self.pool = self.pools.get(row).map(|pool| pool.id.clone());
        Reply::Handled
    }

    fn team_key(&mut self, key: KeyEvent) -> Reply {
        let Some(team) = self.team.as_mut() else {
            return Reply::Ignored;
        };
        let slots = team.sub_harnesses.len();
        let row = self.team_row.min(slots + 1);
        match key.code {
            KeyCode::Up => self.team_row = row.saturating_sub(1),
            KeyCode::Down => self.team_row = (row + 1).min(slots + 1),
            KeyCode::Left | KeyCode::Right if (1..=slots).contains(&row) => {
                let slot = &mut team.sub_harnesses[row - 1];
                slot.agents = if key.code == KeyCode::Left {
                    slot.agents.saturating_sub(1).max(1)
                } else {
                    (slot.agents + 1).min(HARNESS_AGENTS_MAX)
                };
                self.team_edited = true;
            }
            KeyCode::Char(' ') if row == 0 => {
                team.master_tools = !team.master_tools;
                self.team_edited = true;
            }
            KeyCode::Char(' ') if row <= slots => {
                let slot = &mut team.sub_harnesses[row - 1];
                slot.tools = !slot.tools;
                self.team_edited = true;
            }
            KeyCode::Char('d') | KeyCode::Delete if (1..=slots).contains(&row) => {
                team.sub_harnesses.remove(row - 1);
                self.team_row = row.min(team.sub_harnesses.len() + 1);
                self.team_edited = true;
            }
            KeyCode::Enter => {
                let target = match row {
                    0 => PickFor::Master,
                    row if row <= slots => PickFor::Slot(row - 1),
                    _ if slots >= SUB_HARNESSES_MAX => {
                        self.note = Some(Note::error(
                            Control::Team,
                            format!("A team has at most {SUB_HARNESSES_MAX} sub-harnesses."),
                        ));
                        return Reply::Handled;
                    }
                    _ => PickFor::NewSlot,
                };
                self.picker = Some(Picker { target, cursor: 0 });
                return Reply::Handled;
            }
            KeyCode::Left | KeyCode::Right | KeyCode::Char(' ' | 'd') | KeyCode::Delete => {}
            _ => return Reply::Ignored,
        }
        self.note = self.check_team().err();
        Reply::Handled
    }

    fn picker_key(&mut self, key: KeyEvent) -> Reply {
        let choices = self.harness_choices();
        let Some(picker) = self.picker.as_mut() else {
            return Reply::Ignored;
        };
        match key.code {
            KeyCode::Esc => self.picker = None,
            KeyCode::Up => picker.cursor = picker.cursor.saturating_sub(1),
            KeyCode::Down => {
                picker.cursor = (picker.cursor + 1).min(choices.len().saturating_sub(1));
            }
            KeyCode::Char('r') => return Reply::CheckHarnesses,
            KeyCode::Enter => {
                let Some(choice) = choices.get(picker.cursor) else {
                    return Reply::Handled;
                };
                if matches!(
                    choice.state,
                    HarnessState::NotInstalled | HarnessState::CantStart
                ) {
                    return Reply::Handled;
                }
                let (target, id) = (picker.target, choice.id.clone());
                self.pick(target, id);
            }
            _ if key.modifiers.contains(KeyModifiers::CONTROL) => return Reply::Ignored,
            _ => {}
        }
        Reply::Handled
    }

    fn pick(&mut self, target: PickFor, id: String) {
        let Some(team) = self.team.as_mut() else {
            return;
        };
        match target {
            PickFor::Master => team.master = id,
            PickFor::Slot(index) => team.sub_harnesses[index].harness = id,
            PickFor::NewSlot => {
                team.sub_harnesses.push(HarnessSlot {
                    harness: id,
                    agents: 1,
                    tools: true,
                });
                self.team_row = team.sub_harnesses.len();
            }
        }
        self.team_edited = true;
        self.picker = None;
        self.note = self.check_team().err();
    }

    fn assert_consistent(&self) {
        assert!(self.focus < self.controls().len());
        assert!(self.tech.len() <= TECH_ENTRIES_MAX);
        assert!(self.platform_cursor < PLATFORM_ORDER.len());
        assert!(self.work_cursor < ResearchKind::ALL.len());
        assert!(self.documents.is_empty() || self.document_cursor < self.documents.len());
        assert!(
            self.pool
                .as_ref()
                .is_none_or(|picked| self.pools.iter().any(|pool| &pool.id == picked))
        );
        if let Some(team) = &self.team {
            assert!(team.sub_harnesses.len() <= SUB_HARNESSES_MAX);
            assert!(self.team_row <= team.sub_harnesses.len() + 1);
        }
    }
}

/// Names a harness in plain words.
/// Takes the engine id and the harness catalog when known.
/// Returns the catalog name, a known product name, or the id itself.
pub(in crate::tui) fn harness_name(id: &str, catalog: Option<&[HarnessChoice]>) -> String {
    if let Some(choice) = catalog.and_then(|c| c.iter().find(|h| same_harness(&h.id, id))) {
        return choice.name.clone();
    }
    match id {
        "codex" => "Codex",
        "claude" | "claude-code" => "Claude Code",
        "opencode" => "OpenCode",
        "gemini" | "gemini-cli" => "Gemini CLI",
        "cursor" | "cursor-cli" => "Cursor CLI",
        "fx" => "fx by Vercel",
        "antigravity" => "Antigravity",
        "openclaw" => "OpenClaw",
        "pi" => "Pi",
        other => other,
    }
    .to_owned()
}

/// What an empty sub-agent list means (DEC-096 addendum 3), shown where the list would be.
pub(in crate::tui) const ANY_HARNESS_NOTE: &str =
    "Any harness, no limit. Add a harness to limit which ones run and how many.";

/// Says what an empty sub-agent list runs: any harness, or no sub-agent at all under a team
/// maximum of one. Takes the team plan. Returns the note for the place the list would be.
pub(in crate::tui) fn any_harness_note(team: &TeamPlan) -> &'static str {
    if team.max_agents <= 1 {
        "No sub-agents run with a team maximum of 1. Raise the maximum to let any harness run."
    } else {
        ANY_HARNESS_NOTE
    }
}

/// Names the sub-agents an empty sub-agent list allows: the team maximum less the master.
/// Takes the team plan. Returns, for example, "Any harness, up to 63 agents".
pub(in crate::tui) fn any_harness_total(team: &TeamPlan) -> String {
    let subs = team.max_agents.saturating_sub(1);
    match subs {
        0 => "No sub-agents (team maximum is 1)".to_owned(),
        1 => "Any harness, up to 1 agent".to_owned(),
        _ => format!("Any harness, up to {subs} agents"),
    }
}

/// Describes a team in one plain sentence.
/// Takes the team plan and the harness catalog when known.
/// Returns, for example, "Codex leads. Claude Code x2, Codex x1. Tools on."
pub(in crate::tui) fn team_summary(team: &TeamPlan, catalog: Option<&[HarnessChoice]>) -> String {
    let mut text = format!("{} leads.", harness_name(&team.master, catalog));
    if team.sub_harnesses.is_empty() {
        text.push_str(&format!(" {}.", any_harness_total(team)));
    } else {
        let slots: Vec<String> = team
            .sub_harnesses
            .iter()
            .map(|slot| format!("{} x{}", harness_name(&slot.harness, catalog), slot.agents))
            .collect();
        text.push(' ');
        text.push_str(&slots.join(", "));
        text.push('.');
    }
    let tools: Vec<bool> = std::iter::once(team.master_tools)
        .chain(team.sub_harnesses.iter().map(|slot| slot.tools))
        .collect();
    text.push_str(if tools.iter().all(|on| *on) {
        " Tools on."
    } else if tools.iter().all(|on| !on) {
        " Tools off."
    } else {
        " Tools on for some."
    });
    text
}

/// Formats a byte count the way the design shows sizes.
/// Takes the number of bytes.
/// Returns text such as "38 KB", "2.4 MB" or "1.2 GB".
pub(in crate::tui) fn size_label(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;
    let tenths = |unit: u64| {
        let scaled = (u128::from(bytes) * 10 + u128::from(unit) / 2) / u128::from(unit);
        if scaled % 10 == 0 {
            format!("{}", scaled / 10)
        } else {
            format!("{}.{}", scaled / 10, scaled % 10)
        }
    };
    if bytes < KB {
        format!("{bytes} B")
    } else if bytes < MB {
        format!("{} KB", (bytes + KB / 2) / KB)
    } else if bytes < GB {
        format!("{} MB", tenths(MB))
    } else {
        format!("{} GB", tenths(GB))
    }
}

/// Shortens a path under the home folder to start with `~`.
/// Takes the absolute path and the home folder.
/// Returns the path to show.
pub(in crate::tui) fn display_path(path: &str, home: Option<&Path>) -> String {
    match home.and_then(|home| home.to_str()) {
        Some(home) if !home.is_empty() && path.starts_with(home) => {
            let rest = &path[home.len()..];
            if rest.is_empty() || rest.starts_with('/') {
                format!("~{rest}")
            } else {
                path.to_owned()
            }
        }
        _ => path.to_owned(),
    }
}

/// Completes a typed path from its folder's entries, like a shell: one match
/// completes fully, several complete to their common start; hidden entries
/// need a typed dot. Takes the text, the home for `~` and whether only
/// folders count. Returns the new text, or none when nothing changes.
pub(in crate::tui) fn complete(typed: &str, home: Option<&Path>, folders: bool) -> Option<String> {
    let at = typed.rfind('/')?;
    let (folder, prefix) = (&typed[..=at], &typed[at + 1..]);
    let directory = expand(folder, home)?;
    let entries = std::fs::read_dir(&directory).ok()?;
    let mut matches: Vec<(String, bool)> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            let hidden = name.starts_with('.') && !prefix.starts_with('.');
            let dir = entry.path().is_dir();
            (name.starts_with(prefix) && !hidden && (dir || !folders)).then_some((name, dir))
        })
        .collect();
    matches.sort();
    let completed = match matches.as_slice() {
        [] => return None,
        [(name, true)] => format!("{folder}{name}/"),
        [(name, false)] => format!("{folder}{name}"),
        many => {
            let first = &many[0].0;
            let common = first
                .char_indices()
                .map(|(at, c)| at + c.len_utf8())
                .take_while(|end| {
                    many.iter()
                        .all(|(name, _)| name.starts_with(&first[..*end]))
                })
                .last()
                .unwrap_or(0);
            format!("{folder}{}", &first[..common])
        }
    };
    (completed != typed).then_some(completed)
}

/// Turns a typed path into an absolute one, reading a leading `~` as home.
/// Takes the text and the home folder.
/// Returns the absolute path, or none for a relative path or a missing home.
pub(in crate::tui) fn expand(typed: &str, home: Option<&Path>) -> Option<PathBuf> {
    let path = if typed == "~" {
        home?.to_path_buf()
    } else if let Some(rest) = typed.strip_prefix("~/") {
        home?.join(rest)
    } else {
        PathBuf::from(typed)
    };
    path.is_absolute().then_some(path)
}

fn extension(name: &str) -> String {
    Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
}

fn allowed(path: &Path) -> bool {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    DOCUMENT_EXTENSIONS.contains(&extension(name).as_str())
}

fn failure_copy(code: &str, pool: Option<&str>) -> (&'static str, String, Step) {
    match code {
        "project_folder_exists" | "project_exists" => (
            "Could not create the folder.",
            "A folder for this project already exists. Press Enter, then create the project again."
                .into(),
            Step::Review,
        ),
        "pool_missing" => (
            "Could not create the project.",
            format!(
                "Pool {} no longer exists. Pick another pool.",
                pool.unwrap_or("you picked")
            ),
            Step::Pool,
        ),
        "document_not_found"
        | "document_not_a_regular_file"
        | "document_unreadable"
        | "document_changed_while_copying" => (
            "Could not copy a document.",
            "A document moved or changed. Check the list in Documents.".into(),
            Step::Documents,
        ),
        "document_too_large"
        | "project_documents_too_large"
        | "documents_too_many"
        | "document_type_not_allowed"
        | "document_name_duplicate"
        | "document_name_invalid"
        | "document_path_invalid" => (
            "Could not copy a document.",
            "A document is over the limits or not a supported type. Check Documents.".into(),
            Step::Documents,
        ),
        "team_harness_unavailable" => (
            "Could not create the project.",
            "A harness in this team can't run team agents yet. Pick another harness.".into(),
            Step::Team,
        ),
        code if code.starts_with("team_") => (
            "Could not create the project.",
            "The team does not fit the limits. Check Team.".into(),
            Step::Team,
        ),
        "trusted_connection_unavailable" => (
            "Could not create the project.",
            "Not connected to the engine. Wait until it reconnects, then press Enter and try again."
                .into(),
            Step::Review,
        ),
        "owner_busy" => (
            "CrazyAgent is busy with other windows.",
            "Your draft is kept. Wait a moment, then press Enter and create the project again."
                .into(),
            Step::Review,
        ),
        "creation_outcome_unknown" => (
            "CrazyAgent could not confirm the new project.",
            "Check Home for it before you create it again. Your draft is kept.".into(),
            Step::Review,
        ),
        "creation_not_ready" => (
            "Creating projects is not ready yet.",
            "This window can't send a new project to CrazyAgent yet. Your draft is kept.".into(),
            Step::Review,
        ),
        "project_about_invalid" => (
            "Could not create the project.",
            "The description has characters CrazyAgent can't store. Check About.".into(),
            Step::About,
        ),
        _ => (
            "Could not create the project.",
            super::intake::known(code).map_or_else(
                || coded("Press Enter to go back, then try again.", code),
                |sentence| format!("{sentence} Press Enter to go back, then try again."),
            ),
            Step::Review,
        ),
    }
}
