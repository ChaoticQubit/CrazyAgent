use super::project_publish::rule_label;
use crate::github::GitHubRule;
use std::collections::BTreeSet;

#[test]
fn every_rule_has_its_own_plain_label() {
    let labels: BTreeSet<&str> = GitHubRule::ALL
        .iter()
        .map(|rule| rule_label(*rule))
        .collect();
    for rule in GitHubRule::ALL {
        let label = rule_label(rule);
        assert!(!label.is_empty(), "{rule:?} has no label");
        assert!(!label.contains('_'), "{rule:?} shows a code: {label}");
        assert!(
            !label.ends_with('.'),
            "{rule:?} reads as a sentence: {label}"
        );
    }
    assert_eq!(
        labels.len(),
        GitHubRule::ALL.len(),
        "two rules share a label: {labels:?}"
    );
}
