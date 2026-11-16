use super::new_project_input::TextInput;
use super::settings_accounts::plain;
use crate::github::{
    GitHubOwner, GitHubOwners, GitHubPlanField, GitHubPublishPlan, default_description,
    default_repo_name, plan_problem,
};
use crate::tui::client::intake::{IntakeQuery, IntakeView};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

pub(in crate::tui) const PUBLISH_TOGGLE: &str = "Publish this project on GitHub";
pub(in crate::tui) const PUBLISH_LINE: &str =
    "Standard secure setup; agents can propose changes later.";
pub(in crate::tui) const PUBLISH_DOCS: &str = "Also publish the docs folder (plans, research)";
pub(in crate::tui) const SIGN_IN_WAIT: &str = "Sign in to GitHub, or turn publishing off.";
pub(in crate::tui) const CHECKING_WAIT: &str =
    "Checking your GitHub sign-in. Create waits for it, or turn publishing off.";
pub(in crate::tui) const PUBLISH_STARTED: &str = "Publishing on GitHub has started.";
pub(in crate::tui) const PUBLISH_NOT_STARTED: &str = "Publishing on GitHub didn't start.";
const NAME_BYTES_MAX: usize = 100;
const DESCRIPTION_BYTES_MAX: usize = 350 * 4;

/// Whether CrazyAgent is signed in to GitHub, as the New project page last read it: still
/// asking, signed out, signed in, or the plain problem that stopped the read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::tui) enum GitHub {
    Checking,
    SignedOut,
    SignedIn,
    Failed(String),
}

/// The rows of the GitHub panel, top to bottom.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::tui) enum PanelRow {
    Visibility,
    Account,
    Name,
    Description,
    Docs,
    Done,
}

impl PanelRow {
    pub const ALL: [PanelRow; 6] = [
        Self::Visibility,
        Self::Account,
        Self::Name,
        Self::Description,
        Self::Docs,
        Self::Done,
    ];

    fn index(self) -> usize {
        self as usize
    }
}

/// What a key on the publish row or in the GitHub panel asks of the wizard: nothing, done,
/// or opening the GitHub sign-in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::tui) enum PublishReply {
    Ignored,
    Handled,
    SignIn,
}

/// The New project publish block: whether the project goes to GitHub, its visibility, the
/// account, the name and description the owner typed (none while they follow About), the
/// docs switch, the GitHub sign-in and accounts as last read, and the open panel row.
#[derive(Clone, Debug)]
pub(in crate::tui) struct Publish {
    pub on: bool,
    pub private: bool,
    pub docs: bool,
    pub owner: Option<String>,
    pub name: Option<TextInput>,
    pub description: Option<TextInput>,
    pub github: GitHub,
    pub owners: Vec<GitHubOwner>,
    pub owners_problem: Option<String>,
    pub panel: Option<PanelRow>,
}

impl Default for Publish {
    fn default() -> Self {
        Self {
            on: true,
            private: true,
            docs: false,
            owner: None,
            name: None,
            description: None,
            github: GitHub::Checking,
            owners: Vec::new(),
            owners_problem: None,
            panel: None,
        }
    }
}

fn field(limit: usize, value: &str) -> TextInput {
    let mut input = TextInput::single(limit);
    input.set(value);
    input
}

impl Publish {
    /// Records the GitHub sign-in the page read; anything but signed in drops the accounts
    /// and closes the panel. Takes the state. Returns nothing.
    pub fn set_github(&mut self, github: GitHub) {
        if github != GitHub::SignedIn {
            self.owners.clear();
            self.owners_problem = None;
            self.panel = None;
        }
        self.github = github;
    }

    /// Records the accounts the page read and the plain problem that came with them.
    /// Takes the accounts. Returns nothing.
    pub fn set_owners(&mut self, owners: GitHubOwners) {
        self.owners = owners.owners;
        self.owners_problem = owners.problem;
    }

    /// Takes the engine's answer to a GitHub sign-in or accounts read: signed in reads the
    /// accounts next, signed out or a failed read is kept as such, and accounts that could
    /// not be read keep their problem. Takes the query and its result. Returns the read to
    /// send next, if any.
    pub fn arrived(
        &mut self,
        query: &IntakeQuery,
        result: &Result<IntakeView, String>,
    ) -> Option<IntakeQuery> {
        match (query, result) {
            (IntakeQuery::GitHubStatus, Ok(IntakeView::GitHub(status))) if status.signed_in => {
                self.set_github(GitHub::SignedIn);
                return Some(IntakeQuery::GitHubOwners);
            }
            (IntakeQuery::GitHubStatus, Ok(IntakeView::GitHub(_))) => {
                self.set_github(GitHub::SignedOut);
            }
            (IntakeQuery::GitHubStatus, Err(code)) => {
                self.set_github(GitHub::Failed(plain(code).to_owned()));
            }
            (IntakeQuery::GitHubOwners, Ok(IntakeView::Owners(owners))) => {
                self.set_owners(owners.clone());
            }
            (IntakeQuery::GitHubOwners, Err(code)) => self.set_owners(GitHubOwners {
                owners: Vec::new(),
                problem: Some(plain(code).to_owned()),
            }),
            _ => {}
        }
        None
    }

    /// Gives the account the block shows: the picked one while GitHub still offers it, else
    /// the personal one, else the first. Takes the block. Returns the login, none before
    /// any account is offered.
    pub fn shown_owner(&self) -> Option<&str> {
        let picked = self
            .owners
            .iter()
            .find(|owner| self.owner.as_deref() == Some(owner.login.as_str()));
        picked
            .or_else(|| self.owners.iter().find(|owner| owner.personal))
            .or_else(|| self.owners.first())
            .map(|owner| owner.login.as_str())
    }

    /// Gives the repository name the block shows: the typed one, else the project name made
    /// safe for GitHub. Takes the project name. Returns the name.
    pub fn shown_name(&self, project: &str) -> String {
        self.name.as_ref().map_or_else(
            || default_repo_name(project.trim()),
            |input| input.value().to_owned(),
        )
    }

    /// Gives the description the block shows: the typed one, else the about text on one
    /// line. Takes the about text. Returns the description.
    pub fn shown_description(&self, about: &str) -> String {
        self.description.as_ref().map_or_else(
            || default_description(about),
            |input| input.value().to_owned(),
        )
    }

    /// Names the first panel row a publish Start would refuse, checked by the engine's own
    /// rules. Nothing is checked while publishing is off or GitHub is not signed in.
    /// Takes the project name and the about text. Returns the row and a plain sentence.
    pub fn problem(&self, project: &str, about: &str) -> Option<(PanelRow, &'static str)> {
        if !self.on || self.github != GitHub::SignedIn {
            return None;
        }
        let Some(owner) = self.shown_owner() else {
            return Some((
                PanelRow::Account,
                "Pick the GitHub account the repository goes under.",
            ));
        };
        let name = self.shown_name(project);
        let plan = GitHubPublishPlan {
            owner: owner.to_owned(),
            name: name.clone(),
            description: self.shown_description(about),
            private: self.private,
            docs: self.docs,
        };
        match plan_problem(&plan)? {
            GitHubPlanField::Owner => Some((
                PanelRow::Account,
                "GitHub sent an account name CrazyAgent can't use. Pick another.",
            )),
            GitHubPlanField::Name if name.is_empty() => {
                Some((PanelRow::Name, "Enter a repository name."))
            }
            GitHubPlanField::Name => Some((
                PanelRow::Name,
                "Use up to 100 letters, digits, dots, dashes and underscores, not starting with a dot or dash or ending in .git.",
            )),
            GitHubPlanField::Description => Some((
                PanelRow::Description,
                "Use at most 350 characters on one line.",
            )),
        }
    }

    /// Says why Create waits on the publish block: GitHub still being checked, no sign-in, or
    /// the first row a publish Start would refuse. Takes the project name and the about text.
    /// Returns the plain sentence, or none while publishing is off or the block is valid.
    pub fn wait(&self, project: &str, about: &str) -> Option<&'static str> {
        if !self.on {
            return None;
        }
        match self.github {
            GitHub::Checking => Some(CHECKING_WAIT),
            GitHub::SignedOut | GitHub::Failed(_) => Some(SIGN_IN_WAIT),
            GitHub::SignedIn => self.problem(project, about).map(|(_, text)| text),
        }
    }

    /// Builds the plan a publish Start sends. Takes the project name and the about text.
    /// Returns the plan, or none while publishing is off, GitHub is not signed in or a row
    /// has a problem.
    pub fn plan(&self, project: &str, about: &str) -> Option<GitHubPublishPlan> {
        if !self.on || self.github != GitHub::SignedIn || self.problem(project, about).is_some() {
            return None;
        }
        let plan = GitHubPublishPlan {
            owner: self.shown_owner()?.to_owned(),
            name: self.shown_name(project),
            description: self.shown_description(about),
            private: self.private,
            docs: self.docs,
        };
        assert_eq!(plan_problem(&plan), None);
        Some(plan)
    }

    /// Says whether the open panel row takes typed text. Takes the block. Returns the answer.
    pub fn typing(&self) -> bool {
        matches!(self.panel, Some(PanelRow::Name | PanelRow::Description))
    }

    /// Handles a key on the About step's publish row: Space turns publishing on or off;
    /// Enter turns it on, opens the panel while signed in, or asks for the sign-in while
    /// signed out. Takes the key. Returns the reply.
    pub fn row_key(&mut self, key: KeyEvent) -> PublishReply {
        match key.code {
            KeyCode::Char(' ') => {
                self.on = !self.on;
                self.panel = None;
                PublishReply::Handled
            }
            KeyCode::Enter if !self.on => {
                self.on = true;
                PublishReply::Handled
            }
            KeyCode::Enter => match self.github {
                GitHub::SignedIn => {
                    self.panel = Some(PanelRow::Visibility);
                    PublishReply::Handled
                }
                GitHub::SignedOut => PublishReply::SignIn,
                GitHub::Checking | GitHub::Failed(_) => PublishReply::Handled,
            },
            _ => PublishReply::Ignored,
        }
    }

    /// Handles a key in the GitHub panel: arrows and Tab move between rows, Esc closes it,
    /// and each row takes its own keys. Takes the key, the project name and the about
    /// text. Returns the reply.
    pub fn panel_key(&mut self, key: KeyEvent, project: &str, about: &str) -> PublishReply {
        let Some(row) = self.panel else {
            return PublishReply::Ignored;
        };
        let last = PanelRow::ALL.len() - 1;
        let step = |to: usize| Some(PanelRow::ALL[to.min(last)]);
        match key.code {
            KeyCode::Esc => self.panel = None,
            KeyCode::Up | KeyCode::BackTab => self.panel = step(row.index().saturating_sub(1)),
            KeyCode::Down | KeyCode::Tab => self.panel = step(row.index() + 1),
            KeyCode::Enter if row == PanelRow::Done => self.panel = None,
            KeyCode::Enter => self.panel = step(row.index() + 1),
            _ => return self.row_edit(row, key, project, about),
        }
        PublishReply::Handled
    }

    fn row_edit(
        &mut self,
        row: PanelRow,
        key: KeyEvent,
        project: &str,
        about: &str,
    ) -> PublishReply {
        match (row, key.code) {
            (PanelRow::Visibility, KeyCode::Char(' ') | KeyCode::Left | KeyCode::Right) => {
                self.private = !self.private;
            }
            (PanelRow::Account, KeyCode::Left | KeyCode::Right) => {
                self.cycle_owner(key.code == KeyCode::Right);
            }
            (PanelRow::Docs, KeyCode::Char(' ')) => self.docs = !self.docs,
            (PanelRow::Name | PanelRow::Description, _) => {
                return self.type_key(row, key, project, about);
            }
            _ => return PublishReply::Ignored,
        }
        PublishReply::Handled
    }

    fn cycle_owner(&mut self, forward: bool) {
        let count = self.owners.len();
        if count == 0 {
            return;
        }
        let at = self
            .shown_owner()
            .and_then(|login| self.owners.iter().position(|owner| owner.login == login))
            .unwrap_or(0);
        let next = if forward {
            (at + 1) % count
        } else {
            (at + count - 1) % count
        };
        self.owner = Some(self.owners[next].login.clone());
    }

    fn input(&self, row: PanelRow, project: &str, about: &str) -> TextInput {
        match row {
            PanelRow::Name => self
                .name
                .clone()
                .unwrap_or_else(|| field(NAME_BYTES_MAX, &self.shown_name(project))),
            _ => self
                .description
                .clone()
                .unwrap_or_else(|| field(DESCRIPTION_BYTES_MAX, &self.shown_description(about))),
        }
    }

    fn keep(&mut self, row: PanelRow, input: TextInput) {
        assert!(matches!(row, PanelRow::Name | PanelRow::Description));
        if row == PanelRow::Name {
            self.name = Some(input);
        } else {
            self.description = Some(input);
        }
    }

    fn type_key(
        &mut self,
        row: PanelRow,
        key: KeyEvent,
        project: &str,
        about: &str,
    ) -> PublishReply {
        let mut input = self.input(row, project, about);
        let before = input.value().to_owned();
        if !input.handle(key) {
            let typed = matches!(key.code, KeyCode::Char(_))
                && !key.modifiers.contains(KeyModifiers::CONTROL);
            return if typed {
                PublishReply::Handled
            } else {
                PublishReply::Ignored
            };
        }
        if input.value() != before {
            self.keep(row, input);
        }
        PublishReply::Handled
    }

    /// Inserts pasted text into the open panel's text row, keeping the row as typed only when
    /// the paste changed it. Takes the text, the project name and the about text. Returns
    /// Handled when a text row took it.
    pub fn paste(&mut self, text: &str, project: &str, about: &str) -> PublishReply {
        let Some(row) = self.panel.filter(|_| self.typing()) else {
            return PublishReply::Ignored;
        };
        let mut input = self.input(row, project, about);
        let before = input.value().to_owned();
        input.insert_str(text);
        if input.value() != before {
            self.keep(row, input);
        }
        PublishReply::Handled
    }
}
