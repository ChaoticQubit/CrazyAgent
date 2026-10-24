mod intake;
mod layout;
mod watch;

pub use intake::{AddPlan, copy_tree, folder_size, move_by_copy, verify_tree};
pub use layout::{CRAZYAGENT_DIRECTORY, EXCLUDES, LAYOUT, exclude_from_git, prepare_layout};
pub use watch::{FOLDERS_MAX, HomeWatch, scan};

use crate::engine::EngineError;
use crate::protocol::ProjectId;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const PATH_BYTES_MAX: usize = 1024;

const TABLE: &str = "CREATE TABLE IF NOT EXISTS projects_home (
    singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
    path TEXT NOT NULL CHECK(length(CAST(path AS BLOB)) BETWEEN 1 AND 1024)
)";

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

/// A project the engine has set up, as the projects home sees it.
/// Input to `found`: the project, its name, its folder on disk and whether
/// its onboarding is still running.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Registered {
    pub project_id: ProjectId,
    pub name: String,
    pub folder: PathBuf,
    pub onboarding: bool,
}

/// Checks one absolute path supplied by the owner as text.
/// Input: the text and the refusal code to use. Output: Ok when it is an
/// absolute, bounded path with no control characters or `..` parts.
pub fn path_text(path: &str, code: &str) -> Result<(), EngineError> {
    let candidate = Path::new(path);
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

fn refused(code: &str) -> EngineError {
    EngineError(code.into())
}

/// Finds the signed-in user's own home folder, resolved to its real path.
/// Input: none. Output: the folder, or `owner_home_unknown` when HOME is unset
/// or does not resolve, so no safety check silently runs without it.
pub fn owner_home() -> Result<PathBuf, EngineError> {
    let home = std::env::var_os("HOME").ok_or_else(|| refused("owner_home_unknown"))?;
    let home = std::fs::canonicalize(home).map_err(|_| refused("owner_home_unknown"))?;
    assert!(home.is_absolute());
    Ok(home)
}

/// Resolves an owner-picked folder that must be a real directory, not a link.
/// Input: the path and the codes for a missing path and for a non-folder.
/// Output: the canonical path of that directory.
fn real_folder(path: &Path, missing: &str, not_folder: &str) -> Result<PathBuf, EngineError> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(refused(missing)),
        Err(_) => return Err(refused(not_folder)),
    };
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(refused(not_folder));
    }
    let real = std::fs::canonicalize(path).map_err(|_| refused(missing))?;
    assert!(real.is_absolute());
    Ok(real)
}

/// True when `path` is `/`, the owner's home folder or a folder holding it.
fn holds_owner_home(path: &Path, owner: &Path) -> bool {
    path.parent().is_none() || owner.starts_with(path)
}

/// True when one of the two folders is inside the other or they are the same.
fn overlaps(left: &Path, right: &Path) -> bool {
    left.starts_with(right) || right.starts_with(left)
}

/// Checks a folder the owner wants as the projects home.
/// Input: the picked path and the engine's private data folder. Output: the
/// canonical home path, or a refusal for a missing folder, `/`, the user's home
/// folder or any folder holding it, or one that overlaps the engine's data.
pub fn check_home(path: &Path, engine_root: &Path) -> Result<PathBuf, EngineError> {
    let home = real_folder(path, "projects_home_missing", "projects_home_not_folder")?;
    let owner = owner_home()?;
    if holds_owner_home(&home, &owner) || overlaps(&home, engine_root) {
        return Err(refused("projects_home_unsafe"));
    }
    assert!(home.is_absolute() && home.parent().is_some());
    Ok(home)
}

/// Plans bringing an existing folder into the projects home.
/// Input: the canonical home, the engine data folder, the owner's source text
/// and the mode. Output: the real source and the free destination in the home,
/// or a refusal for sources inside the home, the home itself, `/`, the user's
/// home folder, any folder holding the home or engine data, hidden folders and
/// names already taken in the home.
pub fn plan_add(
    home: &Path,
    engine_root: &Path,
    source: &str,
    mode: AddMode,
) -> Result<AddPlan, EngineError> {
    path_text(source, "home_source_invalid")?;
    let real = real_folder(
        Path::new(source),
        "home_source_missing",
        "home_source_not_folder",
    )?;
    if real.starts_with(home) {
        return Err(refused("home_source_in_home"));
    }
    let owner = owner_home()?;
    if holds_owner_home(&real, &owner) || home.starts_with(&real) || overlaps(&real, engine_root) {
        return Err(refused("home_source_unsafe"));
    }
    let name = real
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| refused("home_source_name"))?;
    if name.starts_with('.') {
        return Err(refused("home_source_hidden"));
    }
    let destination = home.join(name);
    if std::fs::symlink_metadata(&destination).is_ok() {
        return Err(refused("home_destination_exists"));
    }
    assert_eq!(destination.parent(), Some(home));
    Ok(AddPlan::new(real, destination, mode))
}

/// Confirms that an add the owner asked for actually landed in the home.
/// Input: the canonical home, the source text and the mode. Output: Ok when
/// the folder is in the home and, for Move, gone from its old place, or for
/// Copy, still there; otherwise `home_add_unverified`.
pub fn verify_added(home: &Path, source: &str, mode: AddMode) -> Result<(), EngineError> {
    path_text(source, "home_source_invalid")?;
    let name = Path::new(source)
        .file_name()
        .ok_or_else(|| refused("home_source_name"))?;
    let landed = std::fs::symlink_metadata(home.join(name))
        .is_ok_and(|metadata| metadata.is_dir() && !metadata.file_type().is_symlink());
    let source_left = std::fs::symlink_metadata(source).is_ok();
    let done = match mode {
        AddMode::Move => !source_left,
        AddMode::Copy => source_left,
    };
    if landed && done {
        Ok(())
    } else {
        Err(refused("home_add_unverified"))
    }
}

/// Lists what the projects home shows: every folder in it, plus set-up
/// projects whose folder is gone. Input: the home, the names of its top-level
/// folders and the set-up projects. Output: entries sorted by name, with git
/// and `.crazyagent/` presence read from disk now. Set-up projects that still
/// exist outside the home are left out; they are listed with the projects.
pub fn found(
    home: Option<&Path>,
    folders: &[String],
    registered: &[Registered],
) -> Vec<FoundProject> {
    let mut entries = Vec::with_capacity(folders.len() + registered.len());
    if let Some(home) = home {
        for name in folders.iter().take(FOLDERS_MAX) {
            let path = home.join(name);
            let project = registered.iter().find(|project| project.folder == path);
            entries.push(FoundProject {
                name: name.clone(),
                path: path.to_string_lossy().into_owned(),
                git: std::fs::symlink_metadata(path.join(".git")).is_ok(),
                known: std::fs::symlink_metadata(path.join(CRAZYAGENT_DIRECTORY))
                    .is_ok_and(|metadata| metadata.is_dir()),
                state: match project {
                    Some(project) if project.onboarding => FoundState::Onboarding,
                    Some(_) => FoundState::Ready,
                    None => FoundState::Found,
                },
                project_id: project.map(|project| project.project_id.clone()),
            });
        }
    }
    for project in registered {
        let present = std::fs::symlink_metadata(&project.folder)
            .is_ok_and(|metadata| metadata.is_dir() && !metadata.file_type().is_symlink());
        if !present {
            entries.push(FoundProject {
                name: project.name.clone(),
                path: project.folder.to_string_lossy().into_owned(),
                git: false,
                known: false,
                state: FoundState::Missing,
                project_id: Some(project.project_id.clone()),
            });
        }
    }
    entries.sort_by(|left, right| (&left.name, &left.path).cmp(&(&right.name, &right.path)));
    assert!(entries.len() <= FOLDERS_MAX + registered.len());
    entries
}

/// Creates the projects home table when the engine database opens.
/// Input: the engine connection. Output: any SQLite failure.
pub(crate) fn initialize(connection: &rusqlite::Connection) -> Result<(), EngineError> {
    connection
        .execute_batch(TABLE)
        .map_err(|e| EngineError(e.to_string()))
}

/// Reads the saved projects home. Input: an engine connection or transaction.
/// Output: the home path, or None before the owner picked one.
pub(crate) fn load(connection: &rusqlite::Connection) -> Result<Option<PathBuf>, EngineError> {
    use rusqlite::OptionalExtension;
    let path: Option<String> = connection
        .query_row(
            "SELECT path FROM projects_home WHERE singleton=1",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| EngineError(e.to_string()))?;
    Ok(path.map(PathBuf::from))
}

/// Saves the projects home; nothing already on disk is moved (L5).
/// Input: the transaction and a canonical home from `check_home`.
/// Output: any SQLite failure.
pub(crate) fn save(connection: &rusqlite::Connection, home: &Path) -> Result<(), EngineError> {
    let text = home
        .to_str()
        .ok_or_else(|| refused("projects_home_path_invalid"))?;
    path_text(text, "projects_home_path_invalid")?;
    connection
        .execute(
            "INSERT INTO projects_home VALUES (1, ?1)
             ON CONFLICT(singleton) DO UPDATE SET path=excluded.path",
            [text],
        )
        .map_err(|e| EngineError(e.to_string()))?;
    Ok(())
}

/// Turns a projects home refusal code into the sentence shown to the owner.
/// Input: an engine rejection code. Output: the plain sentence, or None for
/// codes that are not about the projects home.
pub fn plain_problem(code: &str) -> Option<&'static str> {
    Some(match code {
        "projects_home_path_invalid" | "home_source_invalid" | "folder_size_path_invalid" => {
            "Pick a folder from the file browser."
        }
        "projects_home_missing" => "That folder does not exist.",
        "projects_home_not_folder" => "Pick a folder, not a file or a link.",
        "projects_home_unsafe" => {
            "Pick a separate folder for your projects, not your home folder or a system folder."
        }
        "projects_home_unset" => "Choose your projects folder first.",
        "home_source_missing" => "That folder is no longer there.",
        "home_source_not_folder" => "Pick a folder, not a file or a link.",
        "home_source_in_home" => "That folder is already in your projects folder.",
        "home_source_unsafe" => {
            "That folder holds your home folder or app data and cannot be added."
        }
        "home_source_hidden" | "home_source_name" => {
            "Rename the folder so it has a plain name first."
        }
        "home_destination_exists" => "Your projects folder already has a folder with that name.",
        "home_copy_special_file" => "The folder holds a device or pipe file that cannot be copied.",
        "home_copy_too_large" => "The folder has too many files or levels to copy.",
        "home_copy_failed" | "home_move_failed" => {
            "The folder could not be added. Nothing was removed."
        }
        "home_copy_unverified" => "The copy did not match the original, so the original was kept.",
        "home_move_source_left" => {
            "The folder was added, but some of the original could not be removed."
        }
        "home_add_unverified" => "The folder did not arrive in your projects folder.",
        "folder_size_unsafe" => "That folder cannot be measured.",
        "folder_size_unreadable" => "Some files in that folder cannot be read.",
        "folder_size_timeout" | "folder_size_too_large" => "This folder is very large.",
        "owner_home_unknown" => "Your home folder could not be found.",
        _ => return None,
    })
}
