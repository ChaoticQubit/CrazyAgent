use crate::github::{GitHubPublishJob, GitHubRule, GitHubRuleState};
use crate::protocol::ProjectId;
use crate::tui::client::intake::IntakeQuery;
use crate::tui::keys::Key;

pub(in crate::tui) const DRAFT_AGENT: &str = "";
pub(in crate::tui) const PART_WAY: &str = "";

/// What the owner asked for in the publish pane, before it becomes a query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::tui) enum PublishAsk {
    Approve(Vec<String>),
    Exclude(String),
    Recheck,
    Continue,
    Discard,
}

/// What one key in the publish pane did: nothing more to send, close it for now, or an ask.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::tui) enum PaneKey {
    Handled,
    Later,
    Ask(PublishAsk),
}

/// The publish pane's words: its title, its body lines and its key hints.
pub(in crate::tui) struct PaneText {
    pub title: &'static str,
    pub body: Vec<String>,
    pub hints: Vec<(&'static str, &'static str)>,
}

/// The project page's GitHub publish pane over one job (DEC-182).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::tui) struct PublishPane {
    job: GitHubPublishJob,
}

impl PublishPane {
    /// Opens the pane on a job. Input: the job. Output: the pane.
    pub(in crate::tui) fn new(job: GitHubPublishJob) -> Self {
        Self { job }
    }

    /// The job the pane shows. Input: none. Output: the job.
    pub(in crate::tui) fn job(&self) -> &GitHubPublishJob {
        &self.job
    }

    /// Takes one key. Input: the key. Output: what it did.
    pub(in crate::tui) fn key(&mut self, _key: &Key) -> PaneKey {
        PaneKey::Handled
    }

    /// The pane's words. Input: none. Output: the title, body and hints.
    pub(in crate::tui) fn text(&self) -> PaneText {
        PaneText {
            title: "",
            body: Vec::new(),
            hints: Vec::new(),
        }
    }
}

/// Tells whether a status read opens the pane. Input: the job. Output: true when it does.
pub(in crate::tui) fn opens(_job: &GitHubPublishJob) -> bool {
    false
}

/// Turns an ask into its publish query. Inputs: the project and the ask. Output: the query.
pub(in crate::tui) fn query(project: &ProjectId, _ask: PublishAsk) -> IntakeQuery {
    IntakeQuery::GitHubPublishStatus {
        project: project.clone(),
    }
}

/// Words one rule outcome's state. Input: the state. Output: the line, or none.
pub(in crate::tui) fn rule_state_line(_state: &GitHubRuleState) -> Option<String> {
    None
}

/// The words a person reads for one rule of the standard secure setup.
/// Input: the rule. Output: a short plain label, the same text the desktop shows.
pub(in crate::tui) fn rule_label(rule: GitHubRule) -> &'static str {
    match rule {
        GitHubRule::SquashOnly => "Squash merge only",
        GitHubRule::DeleteHeadBranches => "Delete branches after merge",
        GitHubRule::IssuesOn => "Issues on",
        GitHubRule::WikiOff => "Wiki off",
        GitHubRule::ProjectsOff => "Projects off",
        GitHubRule::AutoMergeOff => "Auto-merge off",
        GitHubRule::PullRequest => "Changes go through pull requests",
        GitHubRule::ConversationResolution => "Resolve conversations before merging",
        GitHubRule::NoForcePush => "No force pushes to main",
        GitHubRule::NoDeletion => "Main can't be deleted",
        GitHubRule::LinearHistory => "Linear history",
        GitHubRule::RequiredChecks => "Required checks pass before merging",
        GitHubRule::SignedCommits => "Signed commits",
        GitHubRule::TagProtection => "Release tags protected",
        GitHubRule::DependabotAlerts => "Dependabot alerts",
        GitHubRule::DependabotSecurityUpdates => "Dependabot security updates",
        GitHubRule::SecretScanning => "Secret scanning",
        GitHubRule::PushProtection => "Push protection for secrets",
        GitHubRule::PrivateVulnerabilityReporting => "Private vulnerability reporting",
        GitHubRule::ActionsVerifiedOnly => "Actions from GitHub and verified creators only",
        GitHubRule::ActionsReadOnlyToken => "Read-only token for Actions",
        GitHubRule::ActionsOutsidePrApproval => "Approval for outside contributors' workflows",
        GitHubRule::CodeqlDefaultSetup => "CodeQL code scanning",
        GitHubRule::DependabotVersionUpdates => "Dependabot version updates",
        GitHubRule::SecurityPolicy => "Security policy (SECURITY.md)",
    }
}
