use super::*;

/// The folder under the engine root where an MCP server check keeps what it wrote, one
/// folder per server id.
pub(crate) const CHECK_FOLDER: &str = "mcp-checks";

/// The folder in the check folder that holds one sandbox profile per checked server.
pub(crate) const CHECK_PROFILES: &str = "profiles";

/// How many check profiles one engine start looks at, so a folder of many files cannot hold
/// up the start; what is left is looked at by the next start.
#[cfg(not(unix))]
const CHECK_SWEEP_MAX: usize = 10_000;

/// Names the sandbox profile file the checks of one server run under.
/// Inputs: the engine's check folder and the server id. Output: the profile path.
pub(crate) fn check_profile(checks: &Path, id: &str) -> PathBuf {
    checks.join(CHECK_PROFILES).join(format!("{id}.sb"))
}

/// The project, home and temp folders the checks of one server run in, under the canonical
/// check folder: the paths its boundary profile is made from and the release derives its
/// container from (V243-WIN-CHECK-PROFILE-BIND). Inputs: the engine's check folder and the
/// server id. Output: the three folders, or the error when the check folder is missing.
pub(crate) fn check_sandbox_folders(checks: &Path, id: &str) -> std::io::Result<[PathBuf; 3]> {
    let check = crate::sandbox::canonical(checks)?.join(id);
    Ok(["project", "home", "tmp"].map(|name| check.join(name)))
}

/// Takes back what a server's check container holds, then deletes its check profile, so a
/// release that stops keeps the profile and with it the record of what is left for a retry.
/// Inputs: the engine's check folder and the server id. A failure other than a missing
/// profile is logged.
pub(crate) fn withdraw_check_profile(checks: &Path, id: &str) {
    if grants_withdrawn(checks, id) {
        match std::fs::remove_file(check_profile(checks, id)) {
            Err(problem) if problem.kind() != std::io::ErrorKind::NotFound => {
                eprintln!("plugin_check_folder_not_removed cause={problem}");
            }
            _ => {}
        }
    }
}

/// Takes back every access entry the boundary helper granted a server's check container and
/// deletes the container ([`crazyagent_win_sandbox::release`]), once the profile names the
/// container derived from [`check_sandbox_folders`]. Callers move the check folder aside
/// first. Inputs: the check folder and the server id. Output: whether nothing is left, true
/// for a missing profile, false for a folder that cannot be read or a refused release.
#[cfg(windows)]
fn grants_withdrawn(checks: &Path, id: &str) -> bool {
    let profile = check_profile(checks, id);
    match std::fs::symlink_metadata(&profile) {
        Err(problem) if problem.kind() == std::io::ErrorKind::NotFound => return true,
        _ => {}
    }
    let container = match check_sandbox_folders(checks, id) {
        Ok([project, home, _]) => crate::sandbox::windows::container_for(&project, &home),
        Err(problem) => {
            eprintln!("plugin_check_grants_not_withdrawn cause={problem}");
            return false;
        }
    };
    match crazyagent_win_sandbox::release(&profile, &container) {
        Ok(()) => true,
        Err(problem) => {
            eprintln!("plugin_check_grants_not_withdrawn cause={problem}");
            false
        }
    }
}

/// Reports nothing granted where checks run under a profile that grants no access entries,
/// so there is no container to release.
/// Inputs: the check folder and the server id, unused. Output: true.
#[cfg(not(windows))]
fn grants_withdrawn(_checks: &Path, _id: &str) -> bool {
    true
}

/// Releases, when the engine starts, each check container a crash left for a server no
/// longer stored: its check folder moves aside, what its profile holds is taken back
/// ([`withdraw_check_profile`]), then the folder and each copy moved aside are deleted. Only
/// plain `<server id>.sb` files are read. Inputs: the engine connection and root. Output: any
/// SQLite failure; a file failure is logged, and a profile whose release stops stays for the
/// next start.
#[cfg(not(unix))]
pub(crate) fn sweep_check_containers(
    connection: &Connection,
    root: &Path,
) -> Result<(), EngineError> {
    let checks = root.join(CHECK_FOLDER);
    let listed = match std::fs::read_dir(checks.join(CHECK_PROFILES)) {
        Ok(listed) => listed,
        Err(problem) if problem.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(problem) => {
            eprintln!("plugin_check_sweep_failed cause={problem}");
            return Ok(());
        }
    };
    let ids: Vec<String> = listed
        .take(CHECK_SWEEP_MAX)
        .filter_map(|entry| match entry {
            Ok(entry) => Some(entry),
            Err(problem) => {
                eprintln!("plugin_check_sweep_failed cause={problem}");
                None
            }
        })
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
        .filter_map(|entry| {
            let name = entry.file_name();
            let id = name.to_str()?.strip_suffix(".sb")?;
            check_folder_name(id).ok().map(|()| id.to_owned())
        })
        .collect();
    assert!(ids.len() <= CHECK_SWEEP_MAX);
    for id in ids {
        let stored: bool = connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM plugin_items WHERE id=?1)",
                [&id],
                |row| row.get(0),
            )
            .map_err(error)?;
        if stored {
            continue;
        }
        if let Err(problem) = move_check_folder_aside(&checks, &id) {
            eprintln!("plugin_check_folder_not_removed cause={}", problem.0);
        }
        withdraw_check_profile(&checks, &id);
        if let Err(problem) = remove_check_folder(root, &id) {
            eprintln!("plugin_check_folder_not_removed cause={}", problem.0);
        }
    }
    Ok(())
}

/// The name prefix of a removed MCP server's check folder moved aside in the check folder
/// to be deleted: `.removed-<server id>-<32 hex digits>`.
const REMOVED_CHECK: &str = ".removed-";

/// Moves one removed MCP server's check folder aside to `.removed-<id>-<nonce>` in the same
/// folder with one rename, so its path is free at once however much a check wrote there.
/// Inputs: the engine's check folder and the server id. Output: the moved folder, none when
/// there was no folder, `plugin_check_folder_invalid` for a name that is not one plain
/// folder name, or `plugin_check_folder_not_removed`.
pub(crate) fn move_check_folder_aside(
    checks: &Path,
    id: &str,
) -> Result<Option<PathBuf>, EngineError> {
    check_folder_name(id)?;
    let nonce = crate::engine::owner::random_id()?;
    let name = format!("{REMOVED_CHECK}{id}-{}", &nonce[..32]);
    assert!(moved_aside(&name, id));
    let aside = checks.join(name);
    match std::fs::rename(checks.join(id), &aside) {
        Ok(()) => Ok(Some(aside)),
        Err(problem) if problem.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(error("plugin_check_folder_not_removed")),
    }
}

/// Deletes one removed MCP server's check folder with everything in it, and each copy of it
/// moved aside for deletion that a crash left, for the start sweeps. Inputs: the engine root
/// and the server id. Output: Ok when all are gone, including when already missing;
/// `plugin_check_folder_invalid` for a name that is not one plain folder name.
pub(crate) fn remove_check_folder(root: &Path, id: &str) -> Result<(), EngineError> {
    check_folder_name(id)?;
    let checks = root.join(CHECK_FOLDER);
    let mut folders = vec![checks.join(id)];
    match std::fs::read_dir(&checks) {
        Ok(entries) => {
            for entry in entries {
                let entry = entry.map_err(|_| error("plugin_check_folder_not_removed"))?;
                if moved_aside(&entry.file_name().to_string_lossy(), id) {
                    folders.push(entry.path());
                }
            }
        }
        Err(problem) if problem.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err(error("plugin_check_folder_not_removed")),
    }
    for folder in folders {
        match std::fs::remove_dir_all(folder) {
            Ok(()) => {}
            Err(problem) if problem.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(error("plugin_check_folder_not_removed")),
        }
    }
    Ok(())
}

fn check_folder_name(id: &str) -> Result<(), EngineError> {
    if id.is_empty() || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
        return Err(error("plugin_check_folder_invalid"));
    }
    Ok(())
}

fn moved_aside(name: &str, id: &str) -> bool {
    name.strip_prefix(REMOVED_CHECK)
        .and_then(|rest| rest.rsplit_once('-'))
        .is_some_and(|(moved, nonce)| {
            moved == id && nonce.len() == 32 && nonce.bytes().all(|b| b.is_ascii_hexdigit())
        })
}
