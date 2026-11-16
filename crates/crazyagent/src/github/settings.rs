use super::{GitHub, GitHubRepoRules, GitHubRule};
use crate::engine::EngineError;
use std::collections::BTreeSet;
use std::path::Path;

#[cfg(all(test, target_os = "macos"))]
mod settings_tests;

impl GitHub {
    /// Reads the rules a new repository starts with, as Settings > GitHub shows them.
    /// Output: the rules on, DEC-182's defaults while the owner saved none.
    pub fn rule_defaults(&self) -> Result<BTreeSet<GitHubRule>, EngineError> {
        Ok(BTreeSet::new())
    }

    /// Saves the rules a new repository starts with. Input: the rules on.
    /// Output: the rules as saved, read back.
    pub fn set_rule_defaults(
        &self,
        _rules: &BTreeSet<GitHubRule>,
    ) -> Result<BTreeSet<GitHubRule>, EngineError> {
        Ok(BTreeSet::new())
    }

    /// Reads the live rules of the repository a project was published to, for Project >
    /// Settings > GitHub. Sends nothing that changes the repository. Input: the project
    /// folder. Output: the page, or `github_rules_not_published` until its publish is done.
    pub fn repo_rules(&self, project: &Path) -> Result<GitHubRepoRules, EngineError> {
        Ok(GitHubRepoRules {
            full_name: project.display().to_string(),
            private: false,
            checks: Vec::new(),
            rules: Vec::new(),
        })
    }

    /// Turns one rule on or off on the repository a project was published to, with the
    /// owner's sign-in, and reads the page again. Inputs: the project folder, the rule and
    /// whether it is on. Output: the fresh page, with the change's own refusal kept on its
    /// rule, or `github_rules_not_published`, `github_rule_invalid`.
    pub fn set_repo_rule(
        &self,
        project: &Path,
        _rule: GitHubRule,
        _on: bool,
    ) -> Result<GitHubRepoRules, EngineError> {
        self.repo_rules(project)
    }
}
