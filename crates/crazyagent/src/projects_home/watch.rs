use crate::engine::EngineError;
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

pub const FOLDERS_MAX: usize = 512;

struct Snapshot {
    folders: BTreeSet<String>,
    generation: u64,
    readable: bool,
}

struct Shared {
    home: PathBuf,
    state: Mutex<Snapshot>,
    changed: Condvar,
}

impl Shared {
    /// Rescans the top level of the home and publishes the result.
    /// Runs at start and after every folder event, dropped or coalesced
    /// events included, so a missed event never leaves a stale list.
    fn rescan(&self) {
        let scanned = scan(&self.home);
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let readable = scanned.is_ok();
        let folders: BTreeSet<String> = scanned.unwrap_or_default().into_iter().collect();
        if folders != state.folders || readable != state.readable {
            state.folders = folders;
            state.readable = readable;
            state.generation = state.generation.saturating_add(1);
            self.changed.notify_all();
        }
        assert!(state.folders.len() <= FOLDERS_MAX);
    }
}

/// A live watch of the projects home's top-level folders.
/// The OS folder watch (FSEvents on macOS) wakes it; every wake rescans the top
/// level, so the list is right even when the OS drops or merges events.
pub struct HomeWatch {
    shared: Arc<Shared>,
    _watcher: RecommendedWatcher,
}

impl HomeWatch {
    /// Starts watching a projects home and scans it once.
    /// Input: the canonical home folder. Output: the running watch, or
    /// `projects_home_watch_failed` when the OS refuses the watch.
    pub fn start(home: &Path) -> Result<Self, EngineError> {
        assert!(home.is_absolute());
        let shared = Arc::new(Shared {
            home: home.to_path_buf(),
            state: Mutex::new(Snapshot {
                folders: BTreeSet::new(),
                generation: 0,
                readable: false,
            }),
            changed: Condvar::new(),
        });
        let events = shared.clone();
        let mut watcher = notify::recommended_watcher(move |_: notify::Result<notify::Event>| {
            events.rescan();
        })
        .map_err(|_| EngineError("projects_home_watch_failed".into()))?;
        watcher
            .watch(home, RecursiveMode::NonRecursive)
            .map_err(|_| EngineError("projects_home_watch_failed".into()))?;
        shared.rescan();
        Ok(Self {
            shared,
            _watcher: watcher,
        })
    }

    /// The canonical home folder this watch covers.
    pub fn home(&self) -> &Path {
        &self.shared.home
    }

    /// True while the home folder can be read; false once it is gone.
    pub fn watching(&self) -> bool {
        self.lock().readable
    }

    /// Rescans the home now, so a read right after a folder was made, added or
    /// removed shows it before the OS event arrives. Output: none; waiters wake
    /// when the list changed.
    pub fn refresh(&self) {
        self.shared.rescan();
    }

    /// The current top-level folder names, sorted, with the change count.
    /// Output: the generation, raised on every change, and at most
    /// `FOLDERS_MAX` names.
    pub fn folders(&self) -> (u64, Vec<String>) {
        let state = self.lock();
        (state.generation, state.folders.iter().cloned().collect())
    }

    /// Waits until the folder list changes after `after` or the wait ends.
    /// Input: the generation already seen and the longest wait. Output: the
    /// generation now, equal to `after` when nothing changed in time.
    pub fn wait_changed(&self, after: u64, timeout: Duration) -> u64 {
        let deadline = Instant::now() + timeout;
        let mut state = self.lock();
        while state.generation == after {
            let now = Instant::now();
            if now >= deadline {
                break;
            }
            state = self
                .shared
                .changed
                .wait_timeout(state, deadline - now)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
        state.generation
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Snapshot> {
        self.shared.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Lists the top-level folders of a projects home.
/// Input: the home folder. Output: the names of real, non-hidden folders with
/// plain names, sorted and capped at `FOLDERS_MAX`; links, files and dot
/// folders (including copies still being made) are skipped.
pub fn scan(home: &Path) -> Result<Vec<String>, EngineError> {
    let entries =
        std::fs::read_dir(home).map_err(|_| EngineError("projects_home_unreadable".into()))?;
    let mut folders = BTreeSet::new();
    for entry in entries {
        let Ok(entry) = entry else {
            continue;
        };
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        if name.starts_with('.') {
            continue;
        }
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() && !kind.is_symlink() {
            folders.insert(name);
        }
    }
    let folders: Vec<String> = folders.into_iter().take(FOLDERS_MAX).collect();
    assert!(folders.len() <= FOLDERS_MAX);
    Ok(folders)
}
