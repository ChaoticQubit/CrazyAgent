use super::{CHECKS_MAX, EngineError, failure};
use kernel_sandbox::engine_scratch::EngineScratch;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

/// The servers whose checks run now, one entry per running check, and the servers removed
/// while one of their checks ran, whose check folder is cleared when their last check ends.
/// Both stay within [`CHECKS_MAX`], since every entry holds a check slot.
pub(super) struct Checking {
    pub(super) running: Vec<String>,
    pub(super) removed: Vec<String>,
}

static CHECKING: std::sync::Mutex<Checking> = std::sync::Mutex::new(Checking {
    running: Vec::new(),
    removed: Vec::new(),
});

pub(super) fn checking() -> std::sync::MutexGuard<'static, Checking> {
    CHECKING
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// One check of one server, counted from before the engine reads the server until its run
/// in the boundary ended, before its result is recorded, so a removal in between leaves its
/// check folder to that end ([`removed_server_checks`]).
pub(super) struct CheckOf {
    id: String,
    checks: PathBuf,
}

impl CheckOf {
    /// Counts a check of `id` as running, one at a time per server, so a check never clears
    /// the fresh check folder another check of the same server runs in. Inputs: the server
    /// id and the engine's check folder, with a check slot held. Output: the count, ended
    /// when dropped, or `server_check_busy` while another check of `id` runs.
    pub(super) fn take(id: &str, checks: PathBuf, _slot: &CheckSlot) -> Result<Self, EngineError> {
        let mut checking = checking();
        if checking.running.iter().any(|running| running == id) {
            return Err(failure("server_check_busy"));
        }
        checking.running.push(id.to_owned());
        assert!(checking.running.len() <= CHECKS_MAX);
        Ok(Self {
            id: id.to_owned(),
            checks,
        })
    }
}

impl Drop for CheckOf {
    /// Ends the count, and clears the server's check folder when the server was removed while
    /// it ran and this was its last check.
    fn drop(&mut self) {
        let last_of_removed = {
            let mut checking = checking();
            let at = checking.running.iter().position(|id| *id == self.id);
            assert!(at.is_some());
            if let Some(at) = at {
                checking.running.swap_remove(at);
            }
            let left = checking.running.contains(&self.id);
            match checking.removed.iter().position(|id| *id == self.id) {
                Some(at) if !left => {
                    checking.removed.swap_remove(at);
                    true
                }
                _ => false,
            }
        };
        if last_of_removed {
            drop(remove_check_folder(&self.checks, &self.id));
        }
    }
}

/// Clears a removed server's check folder now, or when its last running check ends, so no
/// check still running puts back the folder, the profile or a grant after the clearing.
/// Inputs: the engine's check folder and the server id.
pub(super) fn removed_server_checks(checks: &Path, id: &str) {
    let running = {
        let mut checking = checking();
        let running = checking.running.iter().any(|running| running == id);
        if running && !checking.removed.iter().any(|removed| removed == id) {
            checking.removed.push(id.to_owned());
        }
        assert!(checking.removed.len() <= CHECKS_MAX);
        running
    };
    if !running {
        drop(remove_check_folder(checks, id));
    }
}

/// Clears a removed server's check folder, with whatever its checks wrote there while they
/// held the server's secrets, and its check profile: the folder moves aside at once, then on
/// a blocking thread of its own that nothing in the engine waits for, so neither a check home
/// size nor the boundary's access lists hold up the answer, the profile goes once the grants
/// its checks held are taken back ([`crate::storage::plugins::withdraw_check_profile`]) and
/// the moved folder is deleted. The start sweeps delete what a crash left. Inputs: the
/// engine's check folder and the server id. Output: the blocking task, for a row to wait on.
/// A failure other than a missing entry is logged.
pub(super) fn remove_check_folder(checks: &Path, id: &str) -> tokio::task::JoinHandle<()> {
    let aside = match crate::storage::plugins::move_check_folder_aside(checks, id) {
        Ok(aside) => aside,
        Err(problem) => {
            eprintln!("plugin_check_folder_not_removed cause={}", problem.0);
            None
        }
    };
    let (checks, id) = (checks.to_path_buf(), id.to_owned());
    tokio::task::spawn_blocking(move || {
        crate::storage::plugins::withdraw_check_profile(&checks, &id);
        if let Some(aside) = aside
            && let Err(problem) = std::fs::remove_dir_all(aside)
        {
            eprintln!("plugin_check_folder_not_removed cause={problem}");
        }
    })
}

static CHECKS: AtomicUsize = AtomicUsize::new(0);

pub(super) struct CheckSlot;

impl CheckSlot {
    pub(super) fn take() -> Result<Self, EngineError> {
        if CHECKS.fetch_add(1, Ordering::SeqCst) >= CHECKS_MAX {
            CHECKS.fetch_sub(1, Ordering::SeqCst);
            return Err(failure("server_check_busy"));
        }
        Ok(Self)
    }
}

impl Drop for CheckSlot {
    fn drop(&mut self) {
        let before = CHECKS.fetch_sub(1, Ordering::SeqCst);
        assert!(before > 0);
    }
}

/// Makes a server's check folders fresh for one check: its project folder is the engine
/// scratch made new in `<checks>/<id>`, after the last check's one is cleared, and its home
/// and temp folders are the plain private folders the engine made there. Inputs: the engine
/// folder, its check folder and the server id. Output: the project and the home and temp
/// folders, or the cause of the refusal.
pub(super) fn fresh_check_folders(
    root: &Path,
    checks: &Path,
    id: &str,
) -> Result<(EngineScratch, [PathBuf; 2]), String> {
    let cause = |error: &dyn std::fmt::Display| error.to_string();
    let [project, home, tmp] =
        crate::storage::plugins::check_sandbox_folders(checks, id).map_err(|e| cause(&e))?;
    let place = project
        .parent()
        .ok_or("check folder has no server folder")?;
    let mut private = std::fs::DirBuilder::new();
    private.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut private, 0o700);
    private.create(place).map_err(|e| cause(&e))?;
    clear_check_project(&project).map_err(|e| cause(&e))?;
    let made = crate::sandbox::engine_scratch(root, place, "project").map_err(|e| cause(&e))?;
    let plain = "check folder is not the plain folder the engine made";
    if made.path() != project {
        return Err(plain.into());
    }
    for folder in [&home, &tmp] {
        private.create(folder).map_err(|e| cause(&e))?;
        if kernel_sandbox::paths::canonical(folder).map_err(|e| cause(&e))? != *folder {
            return Err(plain.into());
        }
    }
    Ok((made, [home, tmp]))
}

/// Clears the project folder a server's last check left, so each check runs in a fresh
/// one: only a real folder owned by this user is removed, never through a link, and a
/// missing one is left as it is. Input: the check's project folder. Output: unit, or the
/// error naming why it was not removed.
pub(super) fn clear_check_project(project: &Path) -> std::io::Result<()> {
    let metadata = match std::fs::symlink_metadata(project) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    #[cfg(unix)]
    let owned =
        std::os::unix::fs::MetadataExt::uid(&metadata) == rustix::process::geteuid().as_raw();
    #[cfg(not(unix))]
    let owned = true;
    if !metadata.is_dir() || metadata.file_type().is_symlink() || !owned {
        return Err(std::io::Error::other(
            "check project folder is not one the engine made",
        ));
    }
    std::fs::remove_dir_all(project)
}
