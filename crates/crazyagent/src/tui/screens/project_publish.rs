use crate::github::GitHubRule;

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
