use super::{CHECKS_MAX, CheckSlot};
use std::path::{Path, PathBuf};

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
    /// Counts a check of `id` as running. Inputs: the server id and the engine's check
    /// folder, with a check slot held. Output: the count, ended when dropped.
    pub(super) fn take(id: &str, checks: PathBuf, _slot: &CheckSlot) -> Self {
        let mut checking = checking();
        checking.running.push(id.to_owned());
        assert!(checking.running.len() <= CHECKS_MAX);
        Self {
            id: id.to_owned(),
            checks,
        }
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
