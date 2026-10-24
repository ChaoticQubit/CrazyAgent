use crate::engine::EngineError;
use crate::project_setup::ProjectSetup;
use crate::protocol::{CredentialRef, ProjectId};
use serde::{Deserialize, Serialize};

/// Where one onboarding step stands: not started, running, finished, or
/// failed with a plain reason the owner can act on, such as
/// "graphify not installed".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum Step {
    Waiting,
    Running,
    Done,
    Failed { reason: String },
}

/// What the manifest scan and the mapping agent found that has no field in
/// [`ProjectSetup`]: languages, build and test commands and entry points.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MappingFacts {
    pub languages: Vec<String>,
    pub build_commands: Vec<String>,
    pub test_commands: Vec<String>,
    pub entry_points: Vec<String>,
}

/// The onboarding of one found or cloned project: the graph and mapping
/// steps, the proposed name, the pre-filled setup the owner confirms with
/// `UpdateProjectSetup`, the extra facts, and `missing`, the draft fields the
/// owner must still fill, such as `about` or `platforms`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Onboarding {
    pub project_id: ProjectId,
    pub graph: Step,
    pub mapping: Step,
    pub name: String,
    pub draft: ProjectSetup,
    pub facts: MappingFacts,
    pub missing: Vec<String>,
}

/// The receipt of `StartOnboarding`: the project the folder is registered as.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OnboardingStarted {
    pub project_id: ProjectId,
}

/// An owner change the engine records for onboarding: start onboarding the
/// project folder at `path` on the chosen `pool`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum OnboardingAction {
    Start { path: String, pool: CredentialRef },
}

impl OnboardingAction {
    /// Checks the owner-supplied path text before anything touches the disk.
    /// Input: an untrusted action. Output: Ok, or a plain refusal code.
    pub fn validate(&self) -> Result<(), EngineError> {
        match self {
            Self::Start { path, .. } => {
                crate::projects_home::path_text(path, "onboarding_path_invalid")
            }
        }
    }
}
