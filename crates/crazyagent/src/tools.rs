use kernel_processes::finding::{FindingHooks, FindingSeed, Places};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

mod last_good;
#[cfg(all(test, unix))]
mod last_good_tests;
#[cfg(all(test, unix))]
mod permission_fix_tests;
#[cfg(all(test, unix))]
mod probe_tests;
#[cfg(all(test, unix))]
mod tests;
#[cfg(all(test, windows))]
mod windows_tests;

/// How long the set-up project read waits on a busy `engine.db`.
const PROJECTS_READ_TIME: std::time::Duration = std::time::Duration::from_secs(2);

/// The engine's real program finding hooks, the only [`FindingHooks`] production code
/// builds. Output: the hooks.
pub const HOOKS: FindingHooks = FindingHooks {
    link_hops: crate::harnesses::finding::link_hops,
    home_places: crate::harnesses::home_places,
    in_crazyagent_folder: crate::harnesses::in_crazyagent_folder,
    is_shim: crate::harnesses::is_shim,
    npm_prefix: crate::harnesses::npm_prefix,
    registered_projects,
    last_good_load: last_good::load,
    last_good_store: last_good::store,
    projects_marker: crate::projects_home::CRAZYAGENT_DIRECTORY,
    data_twin: crate::projects_home::data_twin,
    one_spelling: kernel_sandbox::paths::one_spelling,
    canonical: kernel_sandbox::paths::canonical,
    in_agent_tmp: crate::sandbox::toolchains::in_agent_tmp,
    harness_prefix: crate::setup::harness_prefix,
    gate_root,
};

/// The engine's finding seed: [`HOOKS`], the layout's data folder and the harness catalog's
/// install folders. Output: the seed.
pub fn finding_seed() -> FindingSeed {
    FindingSeed {
        hooks: HOOKS,
        layout_root: layout_root(),
        harness_locations: harness_locations(),
    }
}

/// Builds places from explicit folders and HOME on the engine's seed ([`Places::new`]).
/// Inputs: the folders in order and HOME. Output: the bounded places.
pub fn tools_new(folders: Vec<PathBuf>, home: Option<PathBuf>) -> Places {
    Places::new(folders, home, finding_seed())
}

/// Builds the engine's places on the engine's hooks and harness catalog ([`Places::engine`]).
/// Inputs: the engine root, the app's folder, HOME and the PATH. Output: the places.
pub fn tools_engine(
    engine_root: Option<&Path>,
    app_folder: Option<&Path>,
    home: Option<PathBuf>,
    path: Option<&OsStr>,
) -> Places {
    Places::engine(engine_root, app_folder, home, path, finding_seed())
}

/// Builds the engine's places from this process's environment on the engine's hooks, data
/// folder and harness catalog ([`Places::from_environment`]). Input: the engine root.
/// Output: the places.
pub fn tools_places(engine_root: Option<&Path>) -> Places {
    Places::from_environment(engine_root, finding_seed())
}

/// The data folder this process's layout names, for places built with no engine root.
/// Output: the folder, or none when the layout cannot be read.
fn layout_root() -> Option<PathBuf> {
    crate::paths::Layout::current()
        .ok()
        .map(|layout| layout.root().to_path_buf())
}

/// Every harness's own install folders under HOME, from the harness catalog, in catalog
/// order. Output: the HOME-relative folders.
fn harness_locations() -> Vec<&'static str> {
    crate::harnesses::specs()
        .iter()
        .flat_map(|spec| spec.locations.iter().copied())
        .collect()
}

const _: () = assert!(
    kernel_processes::finding::PROJECT_ROOTS_MAX
        == 3 * crate::harnesses::finding::NAMED_FOLDERS_MAX
);

/// Every folder `engine.db` names that agents write whole (set-up projects, declared
/// repositories, team and leased worktrees with their repositories), as recorded, real and in
/// one spelling, read through [`crate::harnesses::finding::named_folders`]. No database names
/// none; a failed read or too many rows gives none; an unreadable row is skipped and counted.
/// Input: the data folder. Output: the folders.
fn registered_projects(data_root: &Path) -> Option<Vec<PathBuf>> {
    use crate::harnesses::finding::{BadRow, NAMED_FOLDERS_MAX, named_folders};
    let database = data_root.join("engine.db");
    match std::fs::symlink_metadata(&database) {
        Err(problem) if problem.kind() == std::io::ErrorKind::NotFound => return Some(Vec::new()),
        Err(_) => return None,
        Ok(_) => {}
    }
    let connection = rusqlite::Connection::open_with_flags(
        &database,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
            | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX
            | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )
    .ok()?;
    connection.busy_timeout(PROJECTS_READ_TIME).ok()?;
    let tx = connection.unchecked_transaction().ok()?;
    let (named, skipped) = named_folders(&tx, BadRow::Skip).ok()?;
    let mut roots = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for folder in named.into_iter().filter(|folder| folder.is_absolute()) {
        let real = folder.canonicalize().ok();
        let plain = kernel_sandbox::paths::canonical(&folder).ok();
        for root in std::iter::once(folder).chain(real).chain(plain) {
            if seen.insert(root.clone()) {
                roots.push(root);
            }
        }
    }
    if skipped > 0 {
        eprintln!("engine_places_setup_rows_skipped count={skipped}");
    }
    assert!(roots.len() <= 3 * NAMED_FOLDERS_MAX);
    assert!(roots.iter().all(|root| root.is_absolute()));
    Some(roots)
}

/// The clean-Mac gate's stand-in system root, from `CRAZYAGENT_GATE_SYSTEM_ROOT` when it
/// names an absolute folder ([`FindingHooks::gate_root`]). Output: the root, or none.
#[cfg(feature = "clean-mac-gate")]
fn gate_root() -> Option<PathBuf> {
    std::env::var_os("CRAZYAGENT_GATE_SYSTEM_ROOT")
        .map(PathBuf::from)
        .filter(|root| root.is_absolute())
}

/// A build without the clean-Mac gate feature, every release, has no stand-in system root.
/// Output: none.
#[cfg(not(feature = "clean-mac-gate"))]
fn gate_root() -> Option<PathBuf> {
    None
}
