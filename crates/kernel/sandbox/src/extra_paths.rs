use crate::access::AccessHooks;
use crate::paths::{canonical, owner_home};
use crate::paths::{clear_of_secrets, granted_root, owner_secrets};
use std::path::{Path, PathBuf};

/// The most extra read-only folders one project may name.
pub const EXTRA_PATHS_MAX: usize = 8;

const PATH_TEXT_MAX: usize = 1024;

/// Checks one extra read-only folder the owner named for a project: an absolute existing
/// folder, taken in its canonical form, that neither holds the owner's home nor holds or
/// sits in a credential location, the CrazyAgent data folder or a harness home.
/// Inputs: the hooks, the typed path and the engine's data folder. Output: the canonical folder, or a
/// plain refusal code.
pub fn checked(hooks: &AccessHooks, text: &str, data_root: &Path) -> Result<PathBuf, &'static str> {
    let typed = Path::new(text);
    if text.len() > PATH_TEXT_MAX || !typed.is_absolute() || text.chars().any(char::is_control) {
        return Err("read_path_not_absolute");
    }
    let real = canonical(typed).map_err(|_| "read_path_missing")?;
    if !real.is_dir() {
        return Err("read_path_not_folder");
    }
    let owner = owner_home().map_err(|_| "read_path_owner_unknown")?;
    if owner.starts_with(&real) {
        return Err("read_path_holds_home");
    }
    let secrets: Vec<PathBuf> = owner_secrets(&owner, &(hooks.harness_folders)()).collect();
    if clear_of_secrets(&real, &secrets).is_err() {
        return Err("read_path_credentials");
    }
    let data = canonical(data_root).unwrap_or_else(|_| data_root.to_path_buf());
    if real.starts_with(&data) || data.starts_with(&real) {
        return Err("read_path_engine_data");
    }
    granted_root(&real, &owner, &secrets, &(hooks.harness_folders)())
        .map_err(|_| "read_path_refused")?;
    Ok(real)
}

/// Checks a project's whole list of extra read-only folders as [`checked`] does, at most
/// [`EXTRA_PATHS_MAX`], dropping repeats. Inputs: the hooks, the typed paths and the
/// engine's data folder. Output: the canonical folders in order, or the first refusal code.
pub fn checked_all(
    hooks: &AccessHooks,
    texts: &[String],
    data_root: &Path,
) -> Result<Vec<PathBuf>, &'static str> {
    if texts.len() > EXTRA_PATHS_MAX {
        return Err("read_paths_too_many");
    }
    let mut paths: Vec<PathBuf> = Vec::with_capacity(texts.len());
    for text in texts {
        let path = checked(hooks, text, data_root)?;
        if !paths.contains(&path) {
            paths.push(path);
        }
    }
    assert!(paths.len() <= EXTRA_PATHS_MAX);
    Ok(paths)
}
