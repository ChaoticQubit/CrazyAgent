use super::{GitHub, GitHubCommitted, GitHubRuleOutcome, GitHubSecretScan};
use crate::engine::EngineError;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// What the owner chose for a publish: the account and name of the new repository, its
/// description, whether it is private, and whether the docs folder goes up with it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GitHubPublishPlan {
    pub owner: String,
    pub name: String,
    pub description: String,
    pub private: bool,
    pub docs: bool,
}

/// Where a publish job stands: waiting for the owner to approve the .gitignore lines or
/// to deal with a secret, or the last step it finished on the way to the rules.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "step", rename_all = "snake_case", deny_unknown_fields)]
pub enum GitHubPublishStep {
    Lines {
        lines: Vec<String>,
    },
    Blocked {
        scan: GitHubSecretScan,
    },
    Checked,
    Committed {
        committed: GitHubCommitted,
    },
    Creating {
        committed: GitHubCommitted,
        sent_at: u64,
    },
    Created {
        committed: GitHubCommitted,
        full_name: String,
        private: bool,
    },
    Pushed {
        full_name: String,
        private: bool,
        workflows: bool,
    },
    Done {
        full_name: String,
        outcomes: Vec<GitHubRuleOutcome>,
    },
}

/// One project's publish as the engine records it: the plan, the lines the owner
/// approved, the paths publish wrote, the step reached, and the code of the failure that
/// stopped it there, if one did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GitHubPublishJob {
    pub project: PathBuf,
    pub plan: GitHubPublishPlan,
    pub lines: Vec<String>,
    pub written: Vec<String>,
    pub step: GitHubPublishStep,
    pub stopped: Option<String>,
}

/// Holds one project's publish for the engine, so a second run of it is refused while
/// this one goes on.
pub(super) struct Publishing;

fn unbuilt() -> EngineError {
    EngineError("github_publish_unbuilt".to_owned())
}

/// Takes one project's publish for this engine root. Inputs: the engine root and the
/// project folder. Output: the hold, or `github_publish_running`.
pub(super) fn publishing(root: &Path, project: &Path) -> Result<Publishing, EngineError> {
    let _ = (root, project);
    Ok(Publishing)
}

impl GitHub {
    /// Starts a publish: checks the plan, the sign-in and the project's git, and records
    /// the .gitignore lines the owner is shown first. Nothing is written into the project.
    /// Inputs: the project folder and the plan. Output: the job at its lines.
    pub fn publish_start(
        &self,
        project: &Path,
        plan: &GitHubPublishPlan,
    ) -> Result<GitHubPublishJob, EngineError> {
        let _ = (project, plan);
        Err(unbuilt())
    }

    /// Takes the .gitignore lines the owner approved and runs the publish as far as it
    /// goes. Inputs: the project folder, the owner's home folder and the lines.
    /// Output: the job where it stands.
    pub fn publish_approve(
        &self,
        project: &Path,
        owner_home: &Path,
        lines: &[String],
    ) -> Result<GitHubPublishJob, EngineError> {
        let _ = (project, owner_home, lines);
        Err(unbuilt())
    }

    /// Leaves a file that holds a secret out of the publish through the .gitignore, then
    /// checks again and runs on. Inputs: the project folder, the owner's home folder and
    /// the file's path. Output: the job where it stands.
    pub fn publish_exclude(
        &self,
        project: &Path,
        owner_home: &Path,
        path: &str,
    ) -> Result<GitHubPublishJob, EngineError> {
        let _ = (project, owner_home, path);
        Err(unbuilt())
    }

    /// Checks a blocked publish for secrets again, after the owner fixed the files, and
    /// runs on. Inputs: the project folder and the owner's home folder. Output: the job
    /// where it stands.
    pub fn publish_recheck(
        &self,
        project: &Path,
        owner_home: &Path,
    ) -> Result<GitHubPublishJob, EngineError> {
        let _ = (project, owner_home);
        Err(unbuilt())
    }

    /// Runs a publish on from the last step it finished. Inputs: the project folder and
    /// the owner's home folder. Output: the job where it stands.
    pub fn publish_continue(
        &self,
        project: &Path,
        owner_home: &Path,
    ) -> Result<GitHubPublishJob, EngineError> {
        let _ = (project, owner_home);
        Err(unbuilt())
    }

    /// Reads a project's publish record. Input: the project folder. Output: the job, or
    /// None when the project has none.
    pub fn publish_status(&self, project: &Path) -> Result<Option<GitHubPublishJob>, EngineError> {
        let _ = project;
        Ok(None)
    }
}
