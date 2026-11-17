use super::{Fixed, Roots};
use crate::harnesses;
use std::path::{Path, PathBuf};
use std::sync::{Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

/// The folders a command writes or runs from, which no other job may change while it runs:
/// npm's prefix, and each Homebrew prefix the program sits in (brew's own prefix, or that of
/// a Homebrew npm whose node a brew job may replace). A self-updater names none. Each folder
/// is in its real spelling, sorted, once. Inputs: the command and the roots. Output: the
/// folders.
pub(super) fn folders(fixed: &Fixed, roots: &Roots) -> Vec<PathBuf> {
    let real = |path: &Path| crate::sandbox::canonical(path).unwrap_or_else(|_| path.to_path_buf());
    let homebrew = roots
        .brew_prefixes
        .iter()
        .filter(|prefix| fixed.program.starts_with(prefix));
    let mut folders: Vec<PathBuf> = fixed
        .prefix
        .iter()
        .chain(homebrew)
        .map(|folder| real(folder.as_path()))
        .collect();
    folders.sort();
    folders.dedup();
    assert!(folders.len() <= 1 + roots.brew_prefixes.len());
    folders
}

/// The folders running update and install jobs hold and the jobs waiting for theirs, in
/// arrival order, one line for the whole engine: harness jobs and setup jobs. A waiting job
/// takes all of its folders at once, only when no running job holds one of them and no job
/// that came before it still waits for one, so turns go in arrival order and no job ever
/// holds part of what it needs. Jobs are catalog harnesses, setup tools and one setup run
/// ([`crate::setup::SETUP_JOB`]), one job each, so the line holds at most one entry each.
#[derive(Default)]
pub(crate) struct Turns {
    line: Mutex<Line>,
    freed: Condvar,
}

#[derive(Default)]
struct Line {
    held: Vec<(&'static str, Vec<PathBuf>)>,
    waiting: Vec<(&'static str, Vec<PathBuf>)>,
}

/// One job's hold on its folders; dropping it frees them on every path and wakes the waiting
/// jobs.
pub(crate) struct Turn<'a> {
    turns: &'a Turns,
    id: &'static str,
}

/// The most jobs the line can hold at once: one per catalog harness, one per setup tool and
/// one setup run. Output: the count.
pub(crate) fn jobs_max() -> usize {
    harnesses::specs().len() + crate::setup::Dependency::ALL.len() + 1
}

fn shares(one: &[PathBuf], other: &[PathBuf]) -> bool {
    one.iter().any(|folder| other.contains(folder))
}

impl Line {
    /// The job a waiting job waits behind: the first running job holding one of its folders,
    /// else the first job ahead of it in the line waiting for one. Input: the catalog id.
    /// Output: that job's id, none when the job does not wait or may take its turn now.
    fn behind(&self, id: &str) -> Option<&'static str> {
        let at = self
            .waiting
            .iter()
            .position(|(waiting, _)| *waiting == id)?;
        let folders = &self.waiting[at].1;
        self.held
            .iter()
            .chain(&self.waiting[..at])
            .find(|(_, other)| shares(folders, other))
            .map(|(other, _)| *other)
    }
}

impl Turns {
    fn line(&self) -> MutexGuard<'_, Line> {
        self.line.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Waits for a job's turn on its folders and takes them all at once; a job with no
    /// folder takes its turn at once. Inputs: the catalog id, its folders and the longest
    /// wait. Output: the hold, or none once the wait passed, with the job out of the line.
    pub(crate) fn take(
        &self,
        id: &'static str,
        folders: Vec<PathBuf>,
        wait_max: Duration,
    ) -> Option<Turn<'_>> {
        let deadline = Instant::now() + wait_max;
        let mut line = self.line();
        assert!(
            line.held
                .iter()
                .chain(&line.waiting)
                .all(|(other, _)| *other != id)
        );
        line.waiting.push((id, folders));
        assert!(line.held.len() + line.waiting.len() <= jobs_max());
        loop {
            if line.behind(id).is_none() {
                let at = line.waiting.iter().position(|(waiting, _)| *waiting == id);
                assert!(
                    at.is_some(),
                    "a waiting job stays in the line until it leaves"
                );
                let mine = line.waiting.remove(at.unwrap_or_default());
                line.held.push(mine);
                return Some(Turn { turns: self, id });
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                line.waiting.retain(|(waiting, _)| *waiting != id);
                drop(line);
                self.freed.notify_all();
                return None;
            }
            line = self
                .freed
                .wait_timeout(line, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }

    /// The job a waiting job waits behind. Input: the catalog id. Output: that job's catalog
    /// id, none when the job does not wait.
    pub(crate) fn behind(&self, id: &str) -> Option<&'static str> {
        self.line().behind(id)
    }
}

impl Drop for Turn<'_> {
    fn drop(&mut self) {
        let mut line = self.turns.line();
        line.held.retain(|(held, _)| *held != self.id);
        assert!(line.held.iter().all(|(held, _)| *held != self.id));
        drop(line);
        self.turns.freed.notify_all();
    }
}
