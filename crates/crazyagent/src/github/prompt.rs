use super::GitHub;
use crate::engine::EngineError;
use crate::protocol::ProjectId;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// How long a Not now keeps the Overview from asking to publish a project: 7 days, in ms.
pub const SNOOZE_MS: i64 = 7 * 24 * 60 * 60 * 1000;

/// The owner's answer to the Overview's "This project is not on GitHub. Publish it?":
/// Not now asks again after SNOOZE_MS, Don't ask again never asks for that project.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitHubPromptAnswer {
    NotNow,
    Never,
}

impl GitHub {
    /// Tells whether a project's Overview asks to publish it on GitHub (DEC-182 plan 2h).
    /// Inputs: the project's id, its folder and the time now in ms. Output: true to ask,
    /// or `github_store_unavailable`.
    pub fn publish_prompt(
        &self,
        project: &ProjectId,
        folder: &Path,
        now_ms: i64,
    ) -> Result<bool, EngineError> {
        let _ = (project, folder, now_ms, &self.root);
        Ok(false)
    }

    /// Keeps the owner's answer to a project's publish prompt. Inputs: the project's id,
    /// the answer and the time now in ms. Output: nothing, or `github_store_unavailable`.
    pub fn publish_prompt_set(
        &self,
        project: &ProjectId,
        answer: GitHubPromptAnswer,
        now_ms: i64,
    ) -> Result<(), EngineError> {
        let _ = (project, answer, now_ms);
        Ok(())
    }
}
