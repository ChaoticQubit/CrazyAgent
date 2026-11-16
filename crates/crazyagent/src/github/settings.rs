use super::rules::{self, RepoRecord};
use super::{GitHub, GitHubPublishJob, GitHubPublishStep, GitHubRepoRules, GitHubRule};
use super::{GitHubRuleState, RECORDS, state};
use crate::engine::EngineError;
use std::collections::BTreeSet;
use std::path::Path;
use std::sync::PoisonError;

#[cfg(all(test, target_os = "macos"))]
mod settings_tests;

fn failure(code: &str) -> EngineError {
    EngineError(code.to_owned())
}

impl GitHub {
    /// Reads the rules a new repository starts with, as Settings > GitHub shows them.
    /// Output: the rules on, DEC-182's defaults while the owner saved none.
    pub fn rule_defaults(&self) -> Result<BTreeSet<GitHubRule>, EngineError> {
        state::load_defaults(&self.root)
    }

    /// Saves the rules a new repository starts with. Input: the rules on. Output: the
    /// rules read back, which are another client's when it saved in between.
    pub fn set_rule_defaults(
        &self,
        rules: &BTreeSet<GitHubRule>,
    ) -> Result<BTreeSet<GitHubRule>, EngineError> {
        state::save_defaults(&self.root, rules)?;
        state::load_defaults(&self.root)
    }

    /// Reads the live rules of the repository a project was published to, for Project >
    /// Settings > GitHub. Sends nothing that changes the repository. Input: the project
    /// folder. Output: the page, or `github_rules_not_published` until its publish is done.
    pub fn repo_rules(&self, project: &Path) -> Result<GitHubRepoRules, EngineError> {
        let full_name = self.published(project)?;
        let token = self.store.get(&self.saved()?.credential)?;
        let made = self.record(&full_name)?;
        let (page, _) = rules::read(&self.endpoints, &token, &full_name, &made)?;
        Ok(page)
    }

    /// Turns one rule on or off on the repository a project was published to, with the
    /// owner's sign-in, and reads the page again. The rules the owner wants on are kept in
    /// the repository's record, so a change sends the others as the owner left them.
    /// Inputs: the project folder, the `owner/name` the request was made for, the rule and
    /// whether it is on. Output: the fresh page, with the change's own refusal kept on its
    /// rule, or `github_rule_invalid` for a rule that is a file in the project,
    /// `github_rules_not_published`, `github_rules_repo_mismatch` when the publish names
    /// another repository.
    pub fn set_repo_rule(
        &self,
        project: &Path,
        repo: &str,
        rule: GitHubRule,
        on: bool,
    ) -> Result<GitHubRepoRules, EngineError> {
        if !rule.on_github() {
            return Err(failure("github_rule_invalid"));
        }
        let full_name = self.published(project)?;
        let _ = repo;
        let token = self.store.get(&self.saved()?.credential)?;
        let made = self.record(&full_name)?;
        let (live, facts) = rules::read(&self.endpoints, &token, &full_name, &made)?;
        let outcome = self.with_record(&full_name, |made| {
            let mut wanted = made.on.clone().unwrap_or_else(|| {
                live.rules
                    .iter()
                    .filter(|outcome| outcome.on)
                    .map(|outcome| outcome.rule)
                    .collect()
            });
            if on {
                wanted.insert(rule);
            } else {
                wanted.remove(&rule);
            }
            rules::set(
                &self.endpoints,
                &token,
                &full_name,
                rule,
                &wanted,
                &facts,
                made,
            )
        })?;
        assert_eq!(outcome.rule, rule);
        assert_eq!(outcome.on, on);
        let made = self.record(&full_name)?;
        let (mut page, _) = rules::read(&self.endpoints, &token, &full_name, &made)?;
        if outcome.state != GitHubRuleState::Applied {
            let entry = page
                .rules
                .iter_mut()
                .find(|entry| entry.rule == rule)
                .expect("the page lists every rule");
            entry.state = outcome.state;
        }
        Ok(page)
    }

    /// The repository a project was published to, once its publish job is done; the
    /// project's git remote is never read, since agents can change it. Input: the project
    /// folder. Output: `owner/name`, or `github_rules_not_published`.
    fn published(&self, project: &Path) -> Result<String, EngineError> {
        match state::load_job(&self.root, project)? {
            Some(GitHubPublishJob {
                step: GitHubPublishStep::Done { full_name, .. },
                ..
            }) => Ok(full_name),
            _ => Err(failure("github_rules_not_published")),
        }
    }

    /// Reads one repository's record while no rules pass changes it. Input: `owner/name`.
    /// Output: the record, or `github_state_invalid`, `github_config_unsafe`.
    fn record(&self, full_name: &str) -> Result<RepoRecord, EngineError> {
        let _held = RECORDS.lock().unwrap_or_else(PoisonError::into_inner);
        state::load_record(&self.root, full_name)
    }
}
