use crate::github::GitHubRule;

/// The words a person reads for one rule of the standard secure setup.
/// Input: the rule. Output: a short plain label, the same text the desktop shows.
pub(in crate::tui) fn rule_label(_rule: GitHubRule) -> &'static str {
    ""
}
