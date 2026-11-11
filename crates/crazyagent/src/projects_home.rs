pub mod icloud;
mod intake;
mod layout;
mod move_project;
#[cfg(all(test, unix))]
mod move_tests;
mod platform;
mod watch;

pub use intake::{
    AddPlan, COPY_BOUNDS, CopyBounds, FORCED_COPY_BOUNDS, Measured, MovePlan, OldSource, Placed,
    copy_tree, folder_size, move_by_copy, verify_tree,
};
pub use layout::{
    CRAZYAGENT_DIRECTORY, EXCLUDES, LAYOUT, exclude_agents, exclude_from_git, prepare_layout,
};
pub(crate) use move_project::{Asked, Finished, finish, forget, pending, plan};
pub use move_project::{MOVE_STOP_WAIT, ProjectMove};
pub use watch::{FOLDERS_MAX, HomeWatch, scan};

use crate::engine::EngineError;
use crate::protocol::ProjectId;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const PATH_BYTES_MAX: usize = 1024;
pub const NAME_BYTES_MAX: usize = 255;
pub const SLUG_BYTES_MAX: usize = 64;
pub const LINKED_WORKTREES_MAX: usize = 4096;

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
);
CREATE TABLE IF NOT EXISTS projects_home_project_move (
    singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
    request TEXT NOT NULL CHECK(length(request) BETWEEN 1 AND 128),
    exchange INTEGER NOT NULL CHECK(exchange > 0),
    project TEXT NOT NULL CHECK(length(project) BETWEEN 1 AND 128),
    origin TEXT NOT NULL CHECK(length(CAST(origin AS BLOB)) BETWEEN 1 AND 1024),
    target TEXT NOT NULL CHECK(length(CAST(target AS BLOB)) BETWEEN 1 AND 1024),
    mode TEXT NOT NULL CHECK(mode IN ('move', 'copy'))
);
CREATE TABLE IF NOT EXISTS projects_delete_pending (
    exchange INTEGER PRIMARY KEY CHECK(exchange > 0)
);
CREATE TABLE IF NOT EXISTS projects_delete_files (
    name TEXT PRIMARY KEY CHECK(length(CAST(name AS BLOB)) BETWEEN 1 AND 64),
    inode INTEGER NOT NULL
)";

/// The owner's projects home folder and whether the engine is watching it.
/// `path` is None until the owner picks a folder at first run or in Settings;
/// `watching` is true while a live folder watch reports changes in it or starts.
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

/// A soft refusal of one add that the owner was shown and chose to go past for
/// that add only: a Move that breaks linked git worktrees, a Copy of a folder
/// that is itself a linked checkout, or a copy past the usual entry and level
/// bounds. Every other refusal stays; an add without one keeps its refusal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AddOverride {
    Worktrees,
    LinkedCheckout,
    TooLarge,
}

pub const ADD_OVERRIDES_MAX: usize = 3;

/// Checks the overrides of one add: at most one of each, `worktrees` only with a Move
/// and `linked_checkout` only with a Copy. Inputs: the mode and the overrides.
/// Output: Ok, or `home_override_invalid`.
fn check_overrides(mode: AddMode, overrides: &[AddOverride]) -> Result<(), EngineError> {
    let repeated = overrides
        .iter()
        .enumerate()
        .any(|(at, item)| overrides[..at].contains(item));
    let wrong_mode = overrides.iter().any(|item| match item {
        AddOverride::Worktrees => mode != AddMode::Move,
        AddOverride::LinkedCheckout => mode != AddMode::Copy,
        AddOverride::TooLarge => false,
    });
    if overrides.len() > ADD_OVERRIDES_MAX || repeated || wrong_mode {
        return Err(refused("home_override_invalid"));
    }
    assert!(overrides.len() <= ADD_OVERRIDES_MAX);
    Ok(())
}

/// An owner change to the projects home recorded by the engine: picking the
/// home folder, switching to a new one and optionally moving every project
/// folder there, recording a folder the transport moved or copied in, or
/// recording a registered project the transport moved or copied in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum HomeAction {
    SetHome {
        path: String,
    },
    MoveHome {
        path: String,
        move_projects: bool,
    },
    Added {
        source: String,
        mode: AddMode,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        overrides: Vec<AddOverride>,
    },
    Trash {
        path: String,
    },
    MoveProject {
        project_id: ProjectId,
        mode: AddMode,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        overrides: Vec<AddOverride>,
    },
}

impl HomeAction {
    /// Checks the owner-supplied path text and an add's overrides before anything
    /// touches the disk. Input: an untrusted action. Output: Ok, or the code of the
    /// bad path or `home_override_invalid`.
    pub fn validate(&self) -> Result<(), EngineError> {
        match self {
            Self::SetHome { path } | Self::MoveHome { path, .. } => {
                path_text(path, "projects_home_path_invalid")
            }
            Self::Added {
                source,
                mode,
                overrides,
            } => {
                path_text(source, "home_source_invalid")?;
                check_overrides(*mode, overrides)
            }
            Self::Trash { path } => path_text(path, "home_trash_invalid"),
            Self::MoveProject {
                mode, overrides, ..
            } => check_overrides(*mode, overrides),
        }
    }
}

/// What the owner transport's disk work for a projects home mutation gave:
/// Ok(Some(real source)) once an add ran, Ok(None) once a home move ran, or
/// Err with the refusal code of the plan or of the disk work.
pub type HomeEffect = Result<Option<PathBuf>, String>;

/// A project the engine has set up, as the projects home sees it.
/// Input to `found` and the trash and move guards: the project, its name, its
/// folder on disk, the repositories it declares, where its master works, and
/// whether its onboarding is still running.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Registered {
    pub project_id: ProjectId,
    pub name: String,
    pub folder: PathBuf,
    pub repositories: Vec<PathBuf>,
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

/// True when a real folder overlaps a folder the project uses: its own folder
/// or a repository it declares, each compared as recorded and, when it
/// resolves, at its canonical path, so one declared through a link counts.
/// Input: a canonical folder and the project. Output: whether it is in use.
pub(crate) fn in_use(real: &Path, project: &Registered) -> bool {
    assert!(real.is_absolute());
    std::iter::once(&project.folder)
        .chain(&project.repositories)
        .filter(|used| used.is_absolute())
        .any(|used| {
            overlaps(real, used)
                || std::fs::canonicalize(used).is_ok_and(|canonical| overlaps(real, &canonical))
        })
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

/// What ties a folder's git to places outside it, which a Move breaks: linked worktrees
/// of its repository, the folder being itself a linked worktree or a submodule checkout,
/// or a `.git` that cannot be read or told apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitLink {
    None,
    Worktrees,
    LinkedWorktree,
    Submodule,
    Unreadable,
}

/// Tells which git link a Move of `folder` would break. Input: the folder. Output: the
/// link, or `None` when its git, if any, is all inside it.
pub fn git_link(folder: &Path) -> GitLink {
    let git = folder.join(".git");
    let metadata = match std::fs::symlink_metadata(&git) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return GitLink::None,
        Err(_) => return GitLink::Unreadable,
        Ok(metadata) => metadata,
    };
    if metadata.file_type().is_symlink() {
        return GitLink::Unreadable;
    }
    if metadata.is_file() {
        return checkout_link(folder, &git);
    }
    if !metadata.is_dir() {
        return GitLink::Unreadable;
    }
    match std::fs::read_dir(git.join("worktrees")) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => GitLink::None,
        Err(_) => GitLink::Unreadable,
        Ok(mut entries) => match entries.next() {
            Some(_) => GitLink::Worktrees,
            None => GitLink::None,
        },
    }
}

/// Tells a checkout apart by the git data its `.git` file names, read from `folder` as git
/// reads a relative path: a linked worktree's holds a `commondir` file, a submodule's is a
/// folder without one under a `modules` folder of its parent's git data. Inputs: the folder
/// and its `.git` file. Output: the link, `Unreadable` for any other, missing or unread data.
fn checkout_link(folder: &Path, git: &Path) -> GitLink {
    let Some(text) = crate::platform::read_plain_file(git, PATH_BYTES_MAX as u64 + 16) else {
        return GitLink::Unreadable;
    };
    let Some(named) = text.trim_end_matches(['\n', '\r']).strip_prefix("gitdir: ") else {
        return GitLink::Unreadable;
    };
    let data = folder.join(named);
    match std::fs::symlink_metadata(data.join("commondir")) {
        Ok(metadata) if metadata.is_file() => return GitLink::LinkedWorktree,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        _ => return GitLink::Unreadable,
    }
    let modules = Path::new(named)
        .components()
        .any(|part| part.as_os_str() == std::ffi::OsStr::new("modules"));
    if modules && std::fs::metadata(&data).is_ok_and(|metadata| metadata.is_dir()) {
        GitLink::Submodule
    } else {
        GitLink::Unreadable
    }
}

/// True when moving `folder` would break git links kept outside it: any [`GitLink`] other
/// than `None`, an unreadable `.git` included. [`plan_add`] refuses such a Move unless the
/// owner chose to go past it.
pub fn git_links(folder: &Path) -> bool {
    git_link(folder) != GitLink::None
}

/// True when `folder` is itself a checkout whose git data lives in another repository:
/// its `.git` is a file (a linked worktree or submodule checkout) or a link. A copy of it
/// would still use that repository's data. An unreadable `.git` counts.
/// [`plan_add`] refuses such a Copy, and the windows offer no Copy for it.
pub fn linked_checkout(folder: &Path) -> bool {
    match std::fs::symlink_metadata(folder.join(".git")) {
        Err(error) => error.kind() != std::io::ErrorKind::NotFound,
        Ok(metadata) => !metadata.is_dir() || metadata.file_type().is_symlink(),
    }
}

/// Counts the git worktrees linked to the repository in `folder`, which a Copy leaves
/// with the original. Input: the folder. Output: the entries of its `.git/worktrees`,
/// at most [`LINKED_WORKTREES_MAX`]; 0 without a plain `.git` folder or when unreadable.
pub fn linked_worktrees(folder: &Path) -> usize {
    let git = folder.join(".git");
    let plain = std::fs::symlink_metadata(&git)
        .is_ok_and(|metadata| metadata.is_dir() && !metadata.file_type().is_symlink());
    if !plain {
        return 0;
    }
    let Ok(entries) = std::fs::read_dir(git.join("worktrees")) else {
        return 0;
    };
    entries
        .flatten()
        .take(LINKED_WORKTREES_MAX)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .count()
}

/// The folders of the git worktrees linked to the repository in `folder`, which a Move
/// leaves pointing at its old place. Input: the folder and how many to read. Output: each
/// worktree folder its `.git/worktrees/<name>/gitdir` names, in name order; none without a
/// plain `.git` folder, and unreadable entries left out.
pub fn linked_worktree_paths(folder: &Path, max: usize) -> Vec<PathBuf> {
    let git = folder.join(".git");
    let plain = std::fs::symlink_metadata(&git)
        .is_ok_and(|metadata| metadata.is_dir() && !metadata.file_type().is_symlink());
    if !plain || max == 0 {
        return Vec::new();
    }
    let Ok(entries) = std::fs::read_dir(git.join("worktrees")) else {
        return Vec::new();
    };
    let mut names: Vec<std::ffi::OsString> = entries
        .flatten()
        .take(LINKED_WORKTREES_MAX)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .map(|entry| entry.file_name())
        .collect();
    names.sort();
    let paths: Vec<PathBuf> = names
        .iter()
        .filter_map(|name| {
            let text = crate::platform::read_plain_file(
                &git.join("worktrees").join(name).join("gitdir"),
                PATH_BYTES_MAX as u64 + 1,
            )?;
            let named = PathBuf::from(text.trim_end_matches(['\n', '\r']));
            let worktree = if named.file_name() == Some(std::ffi::OsStr::new(".git")) {
                named.parent()?.to_path_buf()
            } else {
                named
            };
            worktree.is_absolute().then_some(worktree)
        })
        .take(max)
        .collect();
    assert!(paths.len() <= max);
    paths
}

/// Whether a folder and the projects home are on the same disk, so a Move is a quick
/// rename rather than a copy, a check and a removal. Input: the two folders. Output: the
/// answer, or None when either cannot be read or the platform cannot tell.
pub fn same_volume(folder: &Path, home: &Path) -> Option<bool> {
    Some(platform::volume(folder)? == platform::volume(home)?)
}

/// Plans bringing an existing folder into the projects home.
/// Input: the canonical home, the engine data folder, the owner's source
/// text, the mode and the set-up projects. Output: the real source and the
/// free destination in the home, or a refusal for sources inside the home,
/// the home itself, `/`, the user's home folder, any folder holding the home
/// or engine data, hidden folders, names already taken in the home, a Move
/// of a folder with git worktree links or nested with a folder or repository
/// a project uses, and a Copy of a folder that is itself a linked worktree or
/// submodule checkout. Each override lifts exactly one soft refusal for this add:
/// `worktrees` the git worktree links, `linked_checkout` the linked checkout, and
/// `too_large` raises the copy bounds to [`FORCED_COPY_BOUNDS`].
pub fn plan_add(
    home: &Path,
    engine_root: &Path,
    source: &str,
    mode: AddMode,
    overrides: &[AddOverride],
    registered: &[Registered],
) -> Result<AddPlan, EngineError> {
    path_text(source, "home_source_invalid")?;
    check_overrides(mode, overrides)?;
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
        if !overrides.contains(&AddOverride::Worktrees) && git_links(&real) {
            return Err(refused("home_move_worktrees"));
        }
        if registered.iter().any(|project| in_use(&real, project)) {
            return Err(refused("home_move_project"));
        }
    }
    if mode == AddMode::Copy
        && !overrides.contains(&AddOverride::LinkedCheckout)
        && linked_checkout(&real)
    {
        return Err(refused("home_copy_linked_checkout"));
    }
    let bounds = if overrides.contains(&AddOverride::TooLarge) {
        FORCED_COPY_BOUNDS
    } else {
        COPY_BOUNDS
    };
    let destination = destination(home, name)?;
    Ok(AddPlan::new(real, destination, mode, bounds))
}

/// Checks a name for a folder directly inside the projects home. This one rule
/// covers new projects (through `slug`) and folders brought in by intake.
/// Input: the name. Output: Ok for a plain, non-hidden name of at most 255
/// bytes with no path separators, control characters, trailing dot or space,
/// or Windows device name; otherwise `home_folder_name_invalid`.
pub fn folder_name(name: &str) -> Result<(), EngineError> {
    if name.is_empty()
        || name.len() > NAME_BYTES_MAX
        || name.starts_with('.')
        || name.ends_with('.')
        || name.ends_with(' ')
        || name.chars().any(|c| {
            c.is_control() || matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|')
        })
        || crate::protocol::windows_device(name)
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
    if slug.is_empty() || crate::protocol::windows_device(&slug) {
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
/// Input: the canonical home, the owner's path text and the projects. Output:
/// the real folder, or a refusal unless it is a plain top-level folder of the
/// home that holds no project's folder and no repository a project declares.
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
    if registered.iter().any(|project| in_use(&real, project)) {
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

/// Checks the folders a Delete everything would move to the Trash: the project folder and
/// every repository it declares. Inputs: the canonical home and the stored folders. Output:
/// the fewest real folders to move, one holding another counted once and one already
/// missing skipped, or `delete_outside_home` for any that is a link, not a folder, or not
/// strictly below the home.
pub fn plan_delete(home: &Path, folders: &[PathBuf]) -> Result<Vec<PathBuf>, EngineError> {
    assert!(home.is_absolute());
    let mut real = Vec::with_capacity(folders.len());
    for folder in folders {
        let climbs = folder
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir));
        if !folder.is_absolute() || climbs {
            return Err(refused("delete_outside_home"));
        }
        match real_folder(folder, "delete_folder_missing", "delete_outside_home") {
            Ok(path) if path.starts_with(home) && path != home => real.push(path),
            Ok(_) => return Err(refused("delete_outside_home")),
            Err(missing) if missing.0 == "delete_folder_missing" => {}
            Err(refusal) => return Err(refusal),
        }
    }
    let mut moved: Vec<PathBuf> = Vec::with_capacity(real.len());
    for path in &real {
        let held = real
            .iter()
            .any(|other| other != path && path.starts_with(other));
        if !held && !moved.contains(path) {
            moved.push(path.clone());
        }
    }
    assert!(
        moved
            .iter()
            .all(|path| path.starts_with(home) && path != home)
    );
    Ok(moved)
}

/// Tells whether Delete everything is offered on a platform: only where a move to the Trash
/// is proven, which in V2 is macOS (DEC-118 addendum). Windows and Linux offer Remove from
/// dashboard only until a qualification row per platform exists. Input: the platform name as
/// `std::env::consts::OS` gives it. Output: the answer.
pub fn delete_qualified_on(os: &str) -> bool {
    os == "macos"
}

/// Tells whether Delete everything is offered on the platform this engine runs on.
/// Output: the answer for `std::env::consts::OS`.
pub fn delete_offered() -> bool {
    delete_qualified_on(std::env::consts::OS)
}

/// How long a Delete everything waits for the project's agents to stop after Stop all,
/// before it refuses with nothing moved.
pub const DELETE_STOP_WAIT: std::time::Duration = std::time::Duration::from_secs(30);

const DELETE_POLL: std::time::Duration = std::time::Duration::from_millis(100);
const RESEARCH_RUNS_MAX: usize = 100_000;

/// A checked Delete everything, after Stop all was recorded: the engine database, the
/// project, the canonical home, the folders `plan_delete` gave and the project's agents,
/// whose engine folders go once the records are removed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeletePlan {
    pub database: PathBuf,
    pub project: ProjectId,
    pub home: PathBuf,
    pub folders: Vec<PathBuf>,
    pub agents: Vec<i64>,
}

impl DeletePlan {
    /// Waits for the project's agents to stop, then moves its folders to the system Trash
    /// in order. Output: Ok once every folder is gone from the home, or the refusal code.
    pub fn run(&self) -> Result<(), String> {
        self.run_with(DELETE_STOP_WAIT, &trash)
    }

    /// Runs the plan with a bounded wait and a mover: no folder moves while an agent of the
    /// project runs (`delete_agents_running` past the wait), the folders are checked again
    /// just before the first move, with any other project using one of them, research of the
    /// project not finished or a pending home move refusing as the plan did, and a later
    /// folder that fails leaves the earlier ones moved (`delete_trash_partial`). Inputs: the
    /// wait and the mover. Output: the outcome.
    pub(crate) fn run_with(
        &self,
        wait: std::time::Duration,
        mover: &dyn Fn(&Path) -> Result<(), EngineError>,
    ) -> Result<(), String> {
        let unavailable = |_| "delete_unavailable".to_owned();
        let mut db = rusqlite::Connection::open_with_flags(
            &self.database,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .map_err(unavailable)?;
        db.busy_timeout(std::time::Duration::from_secs(5))
            .map_err(unavailable)?;
        let deadline = std::time::Instant::now() + wait;
        loop {
            let running: i64 = db
                .query_row(
                    "SELECT COUNT(*) FROM team_agents WHERE project=?1 AND status NOT IN ('stopped','failed')",
                    [self.project.as_str()],
                    |row| row.get(0),
                )
                .map_err(unavailable)?;
            if running == 0 {
                break;
            }
            if std::time::Instant::now() >= deadline {
                return Err("delete_agents_running".into());
            }
            std::thread::sleep(DELETE_POLL);
        }
        let folders = plan_delete(&self.home, &self.folders).map_err(|refusal| refusal.0)?;
        if let Some(code) = crate::storage::delete_recheck(&mut db, &self.project, &folders)
            .map_err(|_| "delete_unavailable".to_owned())?
        {
            return Err(code);
        }
        for (moved, folder) in folders.iter().enumerate() {
            if mover(folder).is_err() {
                return Err(if moved == 0 {
                    "home_trash_failed"
                } else {
                    "delete_trash_partial"
                }
                .into());
            }
        }
        Ok(())
    }

    /// Names the engine's own files of this deleted project for the record its row removal
    /// writes: each agent, each research run of its jobs and, on macOS, the tool caches of
    /// its folders (T8), with the identity of the folder that holds it. A folder that is no
    /// real run, or that is not there, is left out. Input: the project's research jobs.
    /// Output: the records.
    pub fn engine_files(&self, research: &[i64]) -> Vec<RecordedFile> {
        let Some(root) = self.database.parent() else {
            return Vec::new();
        };
        let mut names: Vec<String> = self
            .agents
            .iter()
            .map(|agent| format!("agent-{agent}"))
            .collect();
        if let Ok(entries) = std::fs::read_dir(root.join("research").join("agents")) {
            for entry in entries.flatten().take(RESEARCH_RUNS_MAX) {
                let Ok(name) = entry.file_name().into_string() else {
                    continue;
                };
                if research
                    .iter()
                    .any(|job| name.starts_with(&format!("job-{job}-")))
                {
                    names.push(name);
                }
            }
        }
        let mut files = recorded(root, names);
        files.extend(tool_cache_files(&self.database, &self.folders));
        files
    }
}

/// The records of the tool caches (T8) the engine keeps for these project folders, each
/// with the identity of its folder, for a delete or a Remove from dashboard to remove; a
/// cache that is not there is left out. Inputs: the engine database path and the project's
/// folders. Output: the records.
#[cfg(target_os = "macos")]
pub fn tool_cache_files(database: &Path, folders: &[PathBuf]) -> Vec<RecordedFile> {
    let Some(root) = database.parent() else {
        return Vec::new();
    };
    let names = folders
        .iter()
        .filter_map(|folder| {
            let cache = crate::sandbox::tool_cache::project_folder(root, folder.to_str()?);
            assert_eq!(cache.parent(), Some(root.join(TOOL_CACHE_FOLDER).as_path()));
            Some(format!(
                "{TOOL_CACHE_PREFIX}{}",
                cache.file_name()?.to_str()?
            ))
        })
        .collect();
    recorded(root, names)
}

/// Off macOS no project has a tool cache. Inputs: unused. Output: none.
#[cfg(not(target_os = "macos"))]
pub fn tool_cache_files(_: &Path, _: &[PathBuf]) -> Vec<RecordedFile> {
    Vec::new()
}

/// Pairs each engine file name with the identity of the folder it names, leaving out a
/// name that is no engine file or whose folder is not there. Inputs: the engine root and
/// the names. Output: the records.
fn recorded(root: &Path, names: Vec<String>) -> Vec<RecordedFile> {
    names
        .into_iter()
        .filter_map(|name| {
            let inode = folder_inode(&engine_file(&name)?.folder(root, &name))?;
            Some((name, inode))
        })
        .collect()
}

/// One engine file a delete recorded: its exact name and the identity of the folder that held
/// it when the project's rows were dropped.
pub type RecordedFile = (String, i64);

/// The folder under the engine root that holds every project's tool caches (T8).
const TOOL_CACHE_FOLDER: &str = "tool-cache";

/// The start of the name a delete records for one project's tool caches (T8).
pub(crate) const TOOL_CACHE_PREFIX: &str = "tool-cache-";

/// The length of the hex digest that names one project's tool cache folder (T8).
const TOOL_CACHE_DIGEST_BYTES: usize = 32;

/// One engine file a delete's record can name.
enum EngineFile {
    Agent(i64),
    Run,
    ToolCache,
}

impl EngineFile {
    /// Gives the folder a recorded name holds. Inputs: the engine root and the exact name.
    /// Output: the path under the team or research state.
    fn folder(&self, root: &Path, name: &str) -> PathBuf {
        match self {
            Self::Agent(_) => root.join("team").join("agents").join(name),
            Self::Run => root.join("research").join("agents").join(name),
            Self::ToolCache => root
                .join(TOOL_CACHE_FOLDER)
                .join(&name[TOOL_CACHE_PREFIX.len()..]),
        }
    }
}

/// Reads a recorded name as an engine file, exact shapes only: `agent-<id>`, or
/// `job-<id>-<attempt>` with an optional `-<launch>`, each number in its one plain spelling,
/// or `tool-cache-<digest>` with 32 lowercase hex digits (T8). Input: the name. Output: what
/// it names, or None for anything else.
fn engine_file(name: &str) -> Option<EngineFile> {
    if let Some(digest) = name.strip_prefix(TOOL_CACHE_PREFIX) {
        let plain = digest.len() == TOOL_CACHE_DIGEST_BYTES
            && digest
                .bytes()
                .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'));
        return plain.then_some(EngineFile::ToolCache);
    }
    if let Some(id) = name.strip_prefix("agent-") {
        return plain_number(id).map(EngineFile::Agent);
    }
    let parts: Vec<&str> = name.strip_prefix("job-")?.split('-').collect();
    (matches!(parts.len(), 2 | 3) && parts.iter().all(|part| plain_number(part).is_some()))
        .then_some(EngineFile::Run)
}

fn plain_number(text: &str) -> Option<i64> {
    text.parse::<i64>()
        .ok()
        .filter(|number| *number >= 0 && number.to_string() == text)
}

/// Gives the number that ties a record to the folder it was written for: the inode alone,
/// since macOS renumbers a volume across reboots, or the creation time on Windows, which has
/// no inode in stable Rust. Input: the folder. Output: the number, or None when it is unread.
fn folder_inode(folder: &Path) -> Option<i64> {
    let metadata = std::fs::symlink_metadata(folder).ok()?;
    #[cfg(unix)]
    let number = std::os::unix::fs::MetadataExt::ino(&metadata);
    #[cfg(windows)]
    let number = std::os::windows::fs::MetadataExt::creation_time(&metadata);
    i64::try_from(number).ok()
}

/// Removes the engine files a delete recorded: an agent's state folder, sandbox profiles
/// and teammates' records, a research run's folder, or a project's tool caches, which an
/// agent may have closed or pinned inside and are cleared of that first. Only the
/// folder the record was written for goes: under any other identity it stays, logged as
/// `project_files_kept`, and the record is finished. A name that is no engine file removes
/// nothing. A file left behind is logged as `project_files_left` and keeps its record.
/// Inputs: the engine database path and the records. Output: the names that are finished.
pub fn remove_engine_files(database: &Path, files: &[RecordedFile]) -> Vec<String> {
    let Some(root) = database.parent() else {
        return Vec::new();
    };
    let mut done = Vec::with_capacity(files.len());
    for (name, inode) in files {
        let removed = match engine_file(name) {
            None => Ok(()),
            Some(file) => {
                let folder = file.folder(root, name);
                if folder_inode(&folder) != Some(*inode) {
                    eprintln!("project_files_kept name={name} kind=engine_folder");
                    Ok(())
                } else if let EngineFile::Agent(id) = file {
                    crate::team::cleanup::remove_agent_files(&root.join("team"), id)
                } else if matches!(file, EngineFile::ToolCache) {
                    crate::team::cleanup::remove_agent_tree(&folder)
                } else {
                    crate::team::cleanup::remove_tree(&folder)
                }
            }
        };
        if removed.is_ok() {
            done.push(name.clone());
        } else {
            eprintln!("project_files_left name={name} kind=engine_folder");
        }
    }
    done
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

/// Creates the projects home table, the table that holds a home move recorded
/// before its folders move and the table that holds each Delete everything planned
/// and not yet settled, when the engine database opens.
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

/// Tells whether a move recorded before its folders move holds `folder`: a projects home
/// move is pending and the folder is inside its old home, or a project move is pending and
/// the folder is inside that project's old folder, as given or as it resolves now. No
/// agent or research run starts there until the move is finished or forgotten.
/// Input: an engine connection and the folder. Output: the answer, or any SQLite failure.
pub(crate) fn move_holds(
    connection: &rusqlite::Connection,
    folder: &Path,
) -> rusqlite::Result<bool> {
    use rusqlite::OptionalExtension;
    let present = connection
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type='table' AND name='projects_home_move'",
            [],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if move_project::holds(connection, folder)? {
        return Ok(true);
    }
    if !present {
        return Ok(false);
    }
    let origin: Option<String> = connection
        .query_row(
            "SELECT origin FROM projects_home_move WHERE singleton=1",
            [],
            |row| row.get(0),
        )
        .optional()?;
    Ok(origin.is_some_and(|origin| {
        let origin = Path::new(&origin);
        folder.starts_with(origin)
            || std::fs::canonicalize(folder).is_ok_and(|real| real.starts_with(origin))
    }))
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
        "delete_outside_home" => {
            "Only a project whose folders are all inside your projects folder can be deleted here. Remove it from the dashboard instead."
        }
        "delete_name_mismatch" => "Type the project's name exactly as shown to delete it.",
        "delete_shared_folder" => {
            "Another project uses one of its folders, so deleting it would delete that project's files too. Remove it from the dashboard instead."
        }
        "delete_research_running" => {
            "Its research is still queued or running. Cancel it or let it finish, then delete the project. Nothing was deleted."
        }
        "delete_platform_unqualified" => {
            "Delete everything is only on macOS for now. Remove the project from the dashboard instead."
        }
        "delete_unavailable" => {
            "The engine could not check the project's agents. Nothing was deleted."
        }
        "delete_agents_running" => {
            "Its agents did not stop in time, so nothing was deleted. They stay stopped; try again."
        }
        "delete_trash_partial" => {
            "Some of its folders are in the Trash and the rest could not be moved. The project stays listed; try again."
        }
        "project_delete_requires_transport" => {
            "This engine cannot delete projects. Remove it from the dashboard instead."
        }
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
        "home_copy_linked_checkout" => {
            "This folder is a git worktree or submodule of another repository, so a copy would still share that repository. Use it where it is, or copy the main repository folder."
        }
        "home_copy_special_file" => "The folder holds a device or pipe file that cannot be copied.",
        "home_copy_too_large" => "The folder has too many files or levels to copy.",
        "home_override_invalid" => {
            "That add asked to go past a check it cannot go past. Choose the folder again."
        }
        "home_copy_failed" | "home_move_failed" => {
            "The folder could not be added. Nothing was removed."
        }
        "home_copy_unverified" => "The copy did not match the original, so the original was kept.",
        "home_move_source_left" => {
            "The folder was added, but some of the original could not be removed."
        }
        "home_icloud_not_downloaded" => {
            "Some files are still only in iCloud, so the folder was not added. Nothing was removed. Download them and try again."
        }
        "home_permission_denied" => {
            "macOS did not let CrazyAgent read or move this folder. Allow CrazyAgent in System Settings > Privacy & Security > Files and Folders, check the folder's permissions, then try again. Nothing was removed."
        }
        "icloud_download_busy" => "Another iCloud download is running. Wait for it to finish.",
        "icloud_folder_missing" => "That folder isn't there anymore.",
        "icloud_folder_unsafe" => "That folder can't be downloaded.",
        "icloud_folder_unreadable" => "Some files in this folder can't be read.",
        "icloud_path_invalid" => "Pick a folder from the file browser.",
        "icloud_outcome_unknown" | "icloud_owner_unavailable" => {
            "CrazyAgent couldn't confirm the iCloud download. Nothing was moved or copied. Try again."
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
        "home_move_delete_in_progress" => {
            "A project in your projects folder is being deleted. Try again when it finishes. Nothing was moved."
        }
        "home_move_agent_work" => {
            "Agents still have work in projects in this folder, and moving it would cut them off from it. Land their changes and remove those agents first. An agent whose change you discarded can be removed 7 days later, when the change can no longer be restored. Nothing was moved."
        }
        "home_move_changes_restorable" => {
            "A change you discarded in a project in this folder can still be restored, and moving the folder would break that. You can move it 7 days after the discard, when the change can no longer be restored. Nothing was moved."
        }
        "home_move_research_running" => {
            "A research job is running in a project in this folder, and moving it would cut the job off. Wait for the job to finish or cancel it, then try again. Nothing was moved."
        }
        "move_project_already_home" => "This project is already in your projects folder.",
        "move_project_busy" => {
            "The project is still working: an agent, a research job, onboarding or a change being landed. Stop the team or let it finish, then try again. Nothing was moved."
        }
        "move_project_master_session" => {
            "The project's master has an open session with its own working copy. Close that session, then try again. Nothing was moved."
        }
        "move_project_copy_worktrees" => {
            "Some agents of this project work in git worktrees that a copy would leave behind. Choose Move instead. Nothing was copied."
        }
        "move_project_folder_link" => {
            "This project's folder is reached through a link. Add the real folder to CrazyAgent instead. Nothing was moved."
        }
        "move_project_in_progress" => {
            "A project is still being moved into your projects folder. Try again when it finishes."
        }
        "move_project_unavailable" => {
            "The engine could not check the project's agents. Nothing was moved."
        }
        "move_project_interrupted" => {
            "The move stopped part way. Check the project folder before trying again."
        }
        "projects_home_unreadable" => "Your projects folder cannot be read.",
        "folder_size_unsafe" => "That folder cannot be measured.",
        "folder_size_unreadable" => "Some files in that folder cannot be read.",
        "folder_size_timeout" | "folder_size_too_large" => "This folder is very large.",
        "owner_home_unknown" => "Your home folder could not be found.",
        _ => return None,
    })
}
