use crate::engine::EngineError;
use crate::protocol::ProjectId;
use serde::{Deserialize, Serialize};

pub const PATH_BYTES_MAX: usize = 1024;

/// The owner's projects home folder and whether the engine is watching it.
/// `path` is None until the owner picks a folder at first run or in Settings;
/// `watching` is true while a live folder watch reports changes in it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectsHome {
    pub path: Option<String>,
    pub watching: bool,
}

/// Where a folder in the projects home stands: found and not set up, set up
/// but gone from disk, being set up, or set up and present.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FoundState {
    Found,
    Missing,
    Onboarding,
    Ready,
}

/// One top-level folder of the projects home, or a set-up project whose folder
/// vanished. `known` is true when the folder holds `.crazyagent/`; `project_id`
/// names the set-up project for Ready, Onboarding and Missing entries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FoundProject {
    pub name: String,
    pub path: String,
    pub git: bool,
    pub known: bool,
    pub state: FoundState,
    #[serde(default)]
    pub project_id: Option<ProjectId>,
}

/// How an existing folder is brought into the projects home: moved (the
/// default, a rename on the same disk) or copied with the source left alone.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AddMode {
    #[default]
    Move,
    Copy,
}

/// An owner change to the projects home recorded by the engine: picking the
/// home folder, or recording a folder the host already moved or copied in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum HomeAction {
    SetHome { path: String },
    Added { source: String, mode: AddMode },
}

impl HomeAction {
    /// Checks the owner-supplied path text before anything touches the disk.
    /// Input: an untrusted action. Output: Ok, or the code of the bad path.
    pub fn validate(&self) -> Result<(), EngineError> {
        match self {
            Self::SetHome { path } => path_text(path, "projects_home_path_invalid"),
            Self::Added { source, .. } => path_text(source, "home_source_invalid"),
        }
    }
}

/// Checks one absolute path supplied by the owner as text.
/// Input: the text and the refusal code to use. Output: Ok when it is an
/// absolute, bounded path with no control characters or `..` parts.
pub fn path_text(path: &str, code: &str) -> Result<(), EngineError> {
    let candidate = std::path::Path::new(path);
    if path.is_empty()
        || path.len() > PATH_BYTES_MAX
        || path.chars().any(char::is_control)
        || !candidate.is_absolute()
        || candidate
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir))
    {
        return Err(EngineError(code.into()));
    }
    Ok(())
}
