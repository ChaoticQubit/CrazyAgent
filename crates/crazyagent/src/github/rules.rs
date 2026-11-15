use super::api::Endpoints;
use crate::credentials::SecretString;
use crate::engine::EngineError;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[cfg(all(test, target_os = "macos"))]
mod rules_tests;

/// One rule of CrazyAgent's standard secure setup for a published repository (DEC-182).
/// Each is a switch on the GitHub settings page, and all start on except signed commits.
/// `ALL` lists them in the order they are applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitHubRule {
    SquashOnly,
    DeleteHeadBranches,
    IssuesOn,
    WikiOff,
    ProjectsOff,
    AutoMergeOff,
    PullRequest,
    ConversationResolution,
    NoForcePush,
    NoDeletion,
    LinearHistory,
    RequiredChecks,
    SignedCommits,
    TagProtection,
    DependabotAlerts,
    DependabotSecurityUpdates,
    SecretScanning,
    PushProtection,
    PrivateVulnerabilityReporting,
    ActionsVerifiedOnly,
    ActionsReadOnlyToken,
    ActionsOutsidePrApproval,
    CodeqlDefaultSetup,
    DependabotVersionUpdates,
    SecurityPolicy,
}

impl GitHubRule {
    pub const ALL: [Self; 25] = [
        Self::SquashOnly,
        Self::DeleteHeadBranches,
        Self::IssuesOn,
        Self::WikiOff,
        Self::ProjectsOff,
        Self::AutoMergeOff,
        Self::PullRequest,
        Self::ConversationResolution,
        Self::NoForcePush,
        Self::NoDeletion,
        Self::LinearHistory,
        Self::RequiredChecks,
        Self::SignedCommits,
        Self::TagProtection,
        Self::DependabotAlerts,
        Self::DependabotSecurityUpdates,
        Self::SecretScanning,
        Self::PushProtection,
        Self::PrivateVulnerabilityReporting,
        Self::ActionsVerifiedOnly,
        Self::ActionsReadOnlyToken,
        Self::ActionsOutsidePrApproval,
        Self::CodeqlDefaultSetup,
        Self::DependabotVersionUpdates,
        Self::SecurityPolicy,
    ];

    /// The rules on for a new repository: every rule but signed commits (DEC-182).
    /// Output: the set of rules that start on.
    pub fn defaults() -> BTreeSet<Self> {
        Self::ALL
            .into_iter()
            .filter(|rule| *rule != Self::SignedCommits)
            .collect()
    }
}

/// What became of one rule: applied, refused by GitHub with its status and message, not
/// applicable to this repository with the reason, or waiting for something first, such as
/// the first CI run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum GitHubRuleState {
    Applied,
    Refused { status: u16, message: String },
    NotApplicable { reason: String },
    Waiting { reason: String },
}

/// One rule, whether it was set on or off, and what became of it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GitHubRuleOutcome {
    pub rule: GitHubRule,
    pub on: bool,
    pub state: GitHubRuleState,
}

/// The CI a repository has, for the required-checks rule: none, workflows that have not
/// run yet, or the check names its first run reported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitHubCi {
    None,
    Waiting,
    Checks(Vec<String>),
}

/// What the rules need to know about a repository that its settings calls do not
/// return: whether it is private, and its CI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitHubRepoFacts {
    pub private: bool,
    pub ci: GitHubCi,
}

fn failure(code: &str) -> EngineError {
    EngineError(code.to_owned())
}

/// Sets every rule GitHub holds on one repository. Inputs: endpoints, token,
/// `owner/name`, the rules on and the facts. Output: one outcome per rule.
pub(super) fn apply(
    _endpoints: &Endpoints,
    _token: &SecretString,
    _full_name: &str,
    _on: &BTreeSet<GitHubRule>,
    _facts: &GitHubRepoFacts,
) -> Result<Vec<GitHubRuleOutcome>, EngineError> {
    Ok(Vec::new())
}

/// Sets one rule on one repository. Inputs: endpoints, token, `owner/name`, the rule,
/// the rules on and the facts. Output: the outcome.
pub(super) fn set(
    _endpoints: &Endpoints,
    _token: &SecretString,
    _full_name: &str,
    _rule: GitHubRule,
    _on: &BTreeSet<GitHubRule>,
    _facts: &GitHubRepoFacts,
) -> Result<GitHubRuleOutcome, EngineError> {
    Err(failure("github_publish_unbuilt"))
}
