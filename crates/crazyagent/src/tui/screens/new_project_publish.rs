use super::new_project_input::TextInput;
use crate::github::{GitHubOwner, GitHubOwners, GitHubPublishPlan};
use crossterm::event::KeyEvent;

pub(in crate::tui) const PUBLISH_TOGGLE: &str = "";
pub(in crate::tui) const PUBLISH_LINE: &str = "";
pub(in crate::tui) const PUBLISH_DOCS: &str = "";
pub(in crate::tui) const SIGN_IN_WAIT: &str = "";

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
            on: false,
            private: false,
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

impl Publish {
    /// Records the GitHub sign-in the page read. Takes the state. Returns nothing.
    pub fn set_github(&mut self, _github: GitHub) {}

    /// Records the accounts the page read. Takes the accounts. Returns nothing.
    pub fn set_owners(&mut self, _owners: GitHubOwners) {}

    /// Gives the account the block shows. Takes the block. Returns the login, if any.
    pub fn shown_owner(&self) -> Option<&str> {
        None
    }

    /// Gives the repository name the block shows. Takes the project name. Returns the name.
    pub fn shown_name(&self, _project: &str) -> String {
        String::new()
    }

    /// Gives the description the block shows. Takes the about text. Returns the description.
    pub fn shown_description(&self, _about: &str) -> String {
        String::new()
    }

    /// Names the first panel row a publish Start would refuse. Takes the project name and
    /// the about text. Returns the row and a plain sentence, if any.
    pub fn problem(&self, _project: &str, _about: &str) -> Option<(PanelRow, &'static str)> {
        None
    }

    /// Builds the plan a publish Start sends. Takes the project name and the about text.
    /// Returns the plan, or none.
    pub fn plan(&self, _project: &str, _about: &str) -> Option<GitHubPublishPlan> {
        None
    }

    /// Says whether the open panel row takes typed text. Takes the block. Returns the answer.
    pub fn typing(&self) -> bool {
        false
    }

    /// Handles a key on the About step's publish row. Takes the key. Returns the reply.
    pub fn row_key(&mut self, _key: KeyEvent) -> PublishReply {
        PublishReply::Ignored
    }

    /// Handles a key in the GitHub panel. Takes the key, the project name and the about
    /// text. Returns the reply.
    pub fn panel_key(&mut self, _key: KeyEvent, _project: &str, _about: &str) -> PublishReply {
        PublishReply::Ignored
    }

    /// Inserts pasted text into the open panel's text row. Takes the text, the project name
    /// and the about text. Returns the reply.
    pub fn paste(&mut self, _text: &str, _project: &str, _about: &str) -> PublishReply {
        PublishReply::Ignored
    }
}
