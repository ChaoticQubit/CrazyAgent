/**
 * The words a person reads for each rule of the standard secure setup, keyed by the
 * engine's rule name. The TUI shows the same text (DEC-182).
 */
export const RULE_LABELS: Readonly<Record<string, string>> = {
  squash_only: "Squash merge only",
  delete_head_branches: "Delete branches after merge",
  issues_on: "Issues on",
  wiki_off: "Wiki off",
  projects_off: "Projects off",
  auto_merge_off: "Auto-merge off",
  pull_request: "Changes go through pull requests",
  conversation_resolution: "Resolve conversations before merging",
  no_force_push: "No force pushes to main",
  no_deletion: "Main can't be deleted",
  linear_history: "Linear history",
  required_checks: "Required checks pass before merging",
  signed_commits: "Signed commits",
  tag_protection: "Release tags protected",
  dependabot_alerts: "Dependabot alerts",
  dependabot_security_updates: "Dependabot security updates",
  secret_scanning: "Secret scanning",
  push_protection: "Push protection for secrets",
  private_vulnerability_reporting: "Private vulnerability reporting",
  actions_verified_only: "Actions from GitHub and verified creators only",
  actions_read_only_token: "Read-only token for Actions",
  actions_outside_pr_approval: "Approval for outside contributors' workflows",
  codeql_default_setup: "CodeQL code scanning",
  dependabot_version_updates: "Dependabot version updates",
  security_policy: "Security policy (SECURITY.md)",
};
