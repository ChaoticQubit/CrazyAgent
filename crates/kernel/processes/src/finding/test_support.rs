use super::{FindingHooks, FindingSeed, Missing, Places, Resolved};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::time::Instant;

/// Homebrew's program folders on Apple silicon and Intel ([`super::homebrew_folders`]).
pub const HOMEBREW_FOLDERS: [&str; 2] = super::HOMEBREW_FOLDERS;

/// The most folders a probe keeps ([`super::probe::kept`]).
pub const FOLDERS_MAX: usize = super::probe::FOLDERS_MAX;

/// A step a row runs once between reading an entry's real file and checking the entry.
pub type Step = Box<dyn FnOnce() + Send>;

/// The engine's places at a given and a recorded engine root ([`Places::from_environment`]
/// with the recorded root named). Inputs: both roots and the seed. Output: the places.
pub fn at_roots(engine_root: Option<&Path>, recorded: Option<&Path>, seed: FindingSeed) -> Places {
    Places::at_roots(engine_root, recorded, seed)
}

/// The PATH folders a child keeps ([`super::probe::kept`]). Inputs: the hooks, the PATH and
/// HOME. Output: the folders.
pub fn kept(hooks: &FindingHooks, path: &OsStr, home: Option<&Path>) -> Vec<PathBuf> {
    super::probe::kept(hooks, path, home)
}

/// Resolves one entry at a depth ([`super::entry_at`]). Inputs: the entry, the places and
/// the depth. Output: the tool, or why it cannot run.
pub fn entry_at(entry: &Path, places: &Places, depth: usize) -> Result<Resolved, Missing> {
    super::entry_at(entry, places, depth)
}

/// The places with other refused temp folders ([`Places::with_temps`]). Inputs: the places
/// and the folders. Output: the places.
#[cfg(unix)]
pub fn with_temps(places: Places, temps: Vec<PathBuf>) -> Places {
    places.with_temps(temps)
}

/// The places with the app's folder set ([`Places::with_app_folder`]). Inputs: the places
/// and the folder. Output: the places.
#[cfg(unix)]
pub fn with_app_folder(places: Places, app_folder: PathBuf) -> Places {
    places.with_app_folder(app_folder)
}

/// The same places with the set-up projects read as given, none when the read failed
/// ([`FindingHooks::registered_projects`]). Inputs: the places and the projects. Output:
/// the places.
pub fn with_projects(places: Places, projects: Option<Vec<PathBuf>>) -> Places {
    Places { projects, ..places }
}

/// Logs a passed-over own `node` once per file ([`super::log_own_node_passed_over`]).
/// Inputs: the file and why. Output: whether this call wrote the line.
pub fn log_own_node_passed_over(node: &Path, passed: &Missing) -> bool {
    super::log_own_node_passed_over(node, passed)
}

/// Sets the step resolving this entry runs between reading its real file and checking it.
/// Inputs: the entry and the step.
pub fn run_between_program_and_check(entry: PathBuf, step: Step) {
    super::BETWEEN_PROGRAM_AND_CHECK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .push((entry, step));
}

/// A path below a base, its parts joined one by one ([`super::below`]). Inputs: the base and
/// the `/`-separated relative path. Output: the path.
pub fn below(base: &Path, relative: &str) -> PathBuf {
    super::below(base, relative)
}

/// Whether the permission fix may change a path ([`super::permission_fix`]). Inputs: the
/// path and the places. Output: the answer.
#[cfg(unix)]
pub fn allowed(path: &Path, places: &Places) -> bool {
    super::permission_fix::allowed(path, places)
}

/// When the last folder probe run ended ([`super::probe::last_run`]). Output: the time, or
/// none before the first run ends.
pub fn last_run_finished() -> Option<Instant> {
    super::probe::last_run().map(|run| run.finished)
}
