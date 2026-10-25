mod intake;
mod layout;
mod platform;
mod watch;

pub use intake::{AddPlan, MovePlan, copy_tree, folder_size, move_by_copy, verify_tree};
pub use layout::{CRAZYAGENT_DIRECTORY, EXCLUDES, LAYOUT, exclude_from_git, prepare_layout};
pub use watch::{FOLDERS_MAX, HomeWatch, scan};

use crate::engine::EngineError;
use crate::protocol::ProjectId;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const PATH_BYTES_MAX: usize = 1024;
pub const NAME_BYTES_MAX: usize = 255;
pub const SLUG_BYTES_MAX: usize = 64;
const RESERVED_NAMES: [&str; 22] = [
    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
    "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];

#[cfg(unix)]
const TABLE: &str = "CREATE TABLE IF NOT EXISTS projects_home (
    singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
    path TEXT NOT NULL CHECK(length(CAST(path AS BLOB)) BETWEEN 1 AND 1024)
);
CREATE TABLE IF NOT EXISTS projects_home_move (
    singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
    request TEXT NOT NULL CHECK(length(request) BETWEEN 1 AND 128),
    exchange INTEGER NOT NULL CHECK(exchange > 0),
    origin TEXT NOT NULL CHECK(length(CAST(origin AS BLOB)) BETWEEN 1 AND 1024),
    target TEXT NOT NULL CHECK(length(CAST(target AS BLOB)) BETWEEN 1 AND 1024),
    folders TEXT NOT NULL CHECK(length(CAST(folders AS BLOB)) BETWEEN 2 AND 1048576)
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

/// How an existing folder is brought into the projects home when the owner
/// explicitly asks for that instead of using it where it is: moved (a rename
/// on the same disk) or copied with the source left alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AddMode {
    Move,
    Copy,
}

/// An owner change to the projects home recorded by the engine: picking the
/// home folder, switching to a new one and optionally moving every project
/// folder there, or recording a folder the transport moved or copied in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum HomeAction {
    SetHome { path: String },
    MoveHome { path: String, move_projects: bool },
    Added { source: String, mode: AddMode },
    Trash { path: String },
}

impl HomeAction {
    /// Checks the owner-supplied path text before anything touches the disk.
    /// Input: an untrusted action. Output: Ok, or the code of the bad path.
    pub fn validate(&self) -> Result<(), EngineError> {
        match self {
            Self::SetHome { path } | Self::MoveHome { path, .. } => {
                path_text(path, "projects_home_path_invalid")
            }
            Self::Added { source, .. } => path_text(source, "home_source_invalid"),
            Self::Trash { path } => path_text(path, "home_trash_invalid"),
        }
    }
}

/// What the owner transport's disk work for a projects home mutation gave:
/// Ok(Some(real source)) once an add ran, Ok(None) once a home move ran, or
/// Err with the refusal code of the plan or of the disk work.
pub type HomeEffect = Result<Option<PathBuf>, String>;

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
/// Input: none. Output: the folder, or `owner_home_unknown` when HOME
/// (USERPROFILE on Windows) is unset or does not resolve, so no safety check
/// silently runs without it.
pub fn owner_home() -> Result<PathBuf, EngineError> {
    let home = std::env::var_os(platform::OWNER_HOME_VARIABLE)
        .ok_or_else(|| refused("owner_home_unknown"))?;
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

/// True when moving `folder` would break git links kept outside it: the
/// folder is a linked worktree or submodule (its `.git` is a file or a link),
/// or its repository has linked worktrees. An unreadable `.git` counts.
fn git_links(folder: &Path) -> bool {
    let git = folder.join(".git");
    match std::fs::symlink_metadata(&git) {
        Err(error) => error.kind() != std::io::ErrorKind::NotFound,
        Ok(metadata) if !metadata.is_dir() => true,
        Ok(_) => match std::fs::read_dir(git.join("worktrees")) {
            Err(error) => error.kind() != std::io::ErrorKind::NotFound,
            Ok(mut entries) => entries.next().is_some(),
        },
    }
}

/// Plans bringing an existing folder into the projects home.
/// Input: the canonical home, the engine data folder, the owner's source
/// text, the mode and the set-up projects. Output: the real source and the
/// free destination in the home, or a refusal for sources inside the home,
/// the home itself, `/`, the user's home folder, any folder holding the home
/// or engine data, hidden folders, names already taken in the home, and a
/// Move of a folder with git worktree links or nested with a set-up project.
pub fn plan_add(
    home: &Path,
    engine_root: &Path,
    source: &str,
    mode: AddMode,
    registered: &[Registered],
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
    folder_name(name).map_err(|_| refused("home_source_name"))?;
    if mode == AddMode::Move {
        if git_links(&real) {
            return Err(refused("home_move_worktrees"));
        }
        if registered
            .iter()
            .any(|project| overlaps(&real, &project.folder))
        {
            return Err(refused("home_move_project"));
        }
    }
    let destination = destination(home, name)?;
    Ok(AddPlan::new(real, destination, mode))
}

/// Checks a name for a folder directly inside the projects home. This one rule
/// covers new projects (through `slug`) and folders brought in by intake.
/// Input: the name. Output: Ok for a plain, non-hidden name of at most 255
/// bytes with no path separators, control characters, trailing dot or space,
/// or Windows device name; otherwise `home_folder_name_invalid`.
pub fn folder_name(name: &str) -> Result<(), EngineError> {
    let stem = name.split('.').next().unwrap_or(name).to_ascii_lowercase();
    if name.is_empty()
        || name.len() > NAME_BYTES_MAX
        || name.starts_with('.')
        || name.ends_with('.')
        || name.ends_with(' ')
        || name.chars().any(|c| {
            c.is_control() || matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|')
        })
        || RESERVED_NAMES.contains(&stem.as_str())
    {
        return Err(refused("home_folder_name_invalid"));
    }
    Ok(())
}

/// Turns a project name into the folder name a new project gets in the home.
/// Input: the owner's project name. Output: lowercase ASCII letters and digits
/// with one `-` between words, at most 64 bytes, or None when the name has no
/// ASCII letter or digit. The result always passes `folder_name`.
pub fn slug(name: &str) -> Option<String> {
    let mut slug = String::with_capacity(SLUG_BYTES_MAX);
    let mut gap = false;
    for c in name.chars() {
        if !c.is_ascii_alphanumeric() {
            gap = true;
            continue;
        }
        if gap && !slug.is_empty() {
            if slug.len() + 2 > SLUG_BYTES_MAX {
                break;
            }
            slug.push('-');
        }
        gap = false;
        if slug.len() + 1 > SLUG_BYTES_MAX {
            break;
        }
        slug.push(c.to_ascii_lowercase());
    }
    if slug.is_empty() || RESERVED_NAMES.contains(&slug.as_str()) {
        return None;
    }
    assert!(slug.len() <= SLUG_BYTES_MAX && !slug.ends_with('-'));
    assert!(folder_name(&slug).is_ok());
    Some(slug)
}

/// Gives the path a folder named `name` takes in the home when it is free.
/// Input: the canonical home and a name that passes `folder_name`. Output:
/// the path, or `home_destination_exists` when anything already has that
/// name, checked before anything is written.
pub fn destination(home: &Path, name: &str) -> Result<PathBuf, EngineError> {
    folder_name(name)?;
    let path = home.join(name);
    if std::fs::symlink_metadata(&path).is_ok() {
        return Err(refused("home_destination_exists"));
    }
    assert_eq!(path.parent(), Some(home));
    Ok(path)
}

/// The folder that holds a project's CrazyAgent files (context, worktrees).
/// Input: the project folder. Output: its `.crazyagent/` folder when that is a
/// real folder, as for projects in the home, otherwise the project folder
/// itself, as for projects made before the projects home existed.
pub fn state_folder(project: &Path) -> PathBuf {
    let inner = project.join(CRAZYAGENT_DIRECTORY);
    match std::fs::symlink_metadata(&inner) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => inner,
        _ => project.to_path_buf(),
    }
}

/// Checks a folder the owner asked to delete from the projects home.
/// Input: the canonical home, the owner's path text and the folders of
/// set-up projects. Output: the real folder, or a refusal unless it is a
/// plain top-level folder of the home that no set-up project uses.
pub fn plan_trash(
    home: &Path,
    path: &str,
    registered: &[Registered],
) -> Result<PathBuf, EngineError> {
    path_text(path, "home_trash_invalid")?;
    let real = real_folder(
        Path::new(path),
        "home_trash_missing",
        "home_trash_not_folder",
    )?;
    if real.parent() != Some(home) {
        return Err(refused("home_trash_outside"));
    }
    let name = real
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| refused("home_trash_outside"))?;
    folder_name(name).map_err(|_| refused("home_trash_outside"))?;
    if registered.iter().any(|project| project.folder == real) {
        return Err(refused("home_trash_still_set_up"));
    }
    assert!(real.starts_with(home) && real != home);
    Ok(real)
}

/// Moves a folder of the projects home to the system Trash, never deleting
/// it outright. Input: a folder checked by `plan_trash`. Output: Ok once it is
/// gone from the home, or `home_trash_failed` with the folder left in place.
pub fn trash(folder: &Path) -> Result<(), EngineError> {
    platform::move_to_trash(folder).map_err(|_| refused("home_trash_failed"))?;
    if std::fs::symlink_metadata(folder).is_ok() {
        return Err(refused("home_trash_failed"));
    }
    Ok(())
}

/// The sentence shown after the owner removes a project from CrazyAgent.
/// Input: the project folder. Output: a plain note that the folder stays.
pub fn forgotten(folder: &str) -> String {
    format!(
        "CrazyAgent no longer tracks this project. Its folder is still at {folder}. Use Delete folder to move it to the Trash."
    )
}

/// Plans moving every project folder from the current home to a new one.
/// Input: the current home, the new folder the owner picked and the engine
/// data folder. Output: the plan with every top-level folder to move, or a
/// refusal for an unsafe new home, the same home, homes inside one another,
/// more than `FOLDERS_MAX` folders, or a name already taken in the new home.
pub fn plan_move(
    current: &Path,
    picked: &Path,
    engine_root: &Path,
) -> Result<MovePlan, EngineError> {
    let next = check_home(picked, engine_root)?;
    if next == current {
        return Err(refused("home_move_same"));
    }
    if overlaps(&next, current) {
        return Err(refused("home_move_nested"));
    }
    let folders = watch::scan_all(current)?.ok_or_else(|| refused("home_move_too_many"))?;
    if folders
        .iter()
        .any(|name| std::fs::symlink_metadata(next.join(name)).is_ok())
    {
        return Err(refused("home_destination_exists"));
    }
    assert!(folders.len() <= FOLDERS_MAX);
    Ok(MovePlan::new(current.to_path_buf(), next, folders))
}

/// Confirms that an add the owner asked for actually landed in the home.
/// Input: the canonical home, the real source `plan_add` resolved (never the
/// owner's text, which may reach it through a link) and the mode. Output: Ok when
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

/// Creates the projects home table, and the table that holds a home move
/// recorded before its folders move, when the engine database opens.
/// Input: the engine connection. Output: any SQLite failure.
#[cfg(unix)]
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
        "projects_home_unset" => "Choose a projects home first.",
        "home_folder_name_invalid" => {
            "Give the project a name with letters or digits, and no folder by that name."
        }
        "home_trash_invalid" | "home_trash_missing" => "That folder is no longer there.",
        "home_trash_not_folder" => "Pick a folder, not a file or a link.",
        "home_trash_outside" => {
            "Only folders directly in your projects folder can be deleted here."
        }
        "home_trash_still_set_up" => "Remove the project from CrazyAgent first.",
        "home_trash_failed" => "The folder could not be moved to the Trash. Nothing was deleted.",
        "home_new_project_failed" => "The project folder could not be created. Nothing was kept.",
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
        "home_move_worktrees" => {
            "This folder has git worktrees linked to it, so moving it would break them. Use it where it is or copy it."
        }
        "home_move_project" => {
            "This folder holds or sits inside a project you already added. Use it where it is or copy it."
        }
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
        "home_move_same" => "That is already your projects folder.",
        "home_move_nested" => "Pick a projects folder that is not inside the current one.",
        "home_move_too_many" => {
            "Your projects folder holds more than 512 folders, more than CrazyAgent moves at once. Move some out, then try again. Nothing was moved."
        }
        "home_move_partial" => {
            "Some projects were moved and could not be moved back. Check both folders."
        }
        "home_move_in_progress" => {
            "Your projects folder is still being moved. Try again when it finishes."
        }
        "projects_home_unreadable" => "Your projects folder cannot be read.",
        "folder_size_unsafe" => "That folder cannot be measured.",
        "folder_size_unreadable" => "Some files in that folder cannot be read.",
        "folder_size_timeout" | "folder_size_too_large" => "This folder is very large.",
        "owner_home_unknown" => "Your home folder could not be found.",
        _ => return None,
    })
}
