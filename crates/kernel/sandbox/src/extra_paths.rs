use crate::access::AccessHooks;
use crate::paths::{Walked, canonical, harness_homes, owner_home, owner_secrets};
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
    checked_in(hooks, text, data_root, &mut None)
}

/// The owner home and the walks of its harness homes and secrets, taken once for every
/// folder one check or one list judges.
type OwnerWalks = Option<(PathBuf, Walked)>;

/// Checks one extra read-only folder as [`checked`] does, walking the owner home, its
/// harness homes and its secrets only when `walks` has none yet, then keeping them there,
/// and the folder itself once. Inputs: the hooks, the typed path, the engine's data folder
/// and the walks so far. Output: the canonical folder, or a plain refusal code.
fn checked_in(
    hooks: &AccessHooks,
    text: &str,
    data_root: &Path,
    walks: &mut OwnerWalks,
) -> Result<PathBuf, &'static str> {
    let typed = Path::new(text);
    if text.len() > PATH_TEXT_MAX || !typed.is_absolute() || text.chars().any(char::is_control) {
        return Err("read_path_not_absolute");
    }
    let real = canonical(typed).map_err(|_| "read_path_missing")?;
    if !real.is_dir() {
        return Err("read_path_not_folder");
    }
    let owner = match walks {
        Some((owner, _)) => owner.clone(),
        None => owner_home().map_err(|_| "read_path_owner_unknown")?,
    };
    if owner.starts_with(&real) {
        return Err("read_path_holds_home");
    }
    if walks.is_none() {
        let harnesses = (hooks.harness_folders)();
        let secrets: Vec<PathBuf> = owner_secrets(&owner, &harnesses).collect();
        let walked = Walked::of(&owner, &harness_homes(&owner, &harnesses), &secrets)
            .map_err(|_| "read_path_credentials")?;
        *walks = Some((owner, walked));
    }
    let (_, walked) = walks.as_ref().ok_or("read_path_credentials")?;
    let root = walked
        .walk_clear(&real)
        .map_err(|_| "read_path_credentials")?;
    let data = canonical(data_root).unwrap_or_else(|_| data_root.to_path_buf());
    if real.starts_with(&data) || data.starts_with(&real) {
        return Err("read_path_engine_data");
    }
    walked
        .grant_walked(&real, &root)
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
    let mut walks = None;
    for text in texts {
        let path = checked_in(hooks, text, data_root, &mut walks)?;
        if !paths.contains(&path) {
            paths.push(path);
        }
    }
    assert!(paths.len() <= EXTRA_PATHS_MAX);
    Ok(paths)
}
