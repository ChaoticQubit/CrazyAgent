use super::intake::COPY_BOUNDS;
use super::{holds_owner_home, owner_home, path_text, real_folder, refused};
use crate::engine::EngineError;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs;
use std::hash::{BuildHasher, BuildHasherDefault, DefaultHasher};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

const SF_DATALESS: u32 = 0x4000_0000;
const BRCTL: &str = "/usr/bin/brctl";
pub const BATCH_PATHS_MAX: usize = 128;
pub const STALL_MAX: Duration = Duration::from_secs(10 * 60);
pub const DOWNLOAD_TIME_MAX: Duration = Duration::from_secs(4 * 60 * 60);
const POLL_EVERY: Duration = Duration::from_secs(2);
const CANCEL_EVERY: Duration = Duration::from_millis(100);
const BRCTL_TIME_MAX: Duration = Duration::from_secs(60);
const PASSES_MAX: u64 = DOWNLOAD_TIME_MAX.as_secs() / POLL_EVERY.as_secs() + 1;
const NOT_DOWNLOADED: &str = "home_icloud_not_downloaded";

const _: () = assert!(BATCH_PATHS_MAX * 4096 < 1024 * 1024);
const _: () = assert!(STALL_MAX.as_secs() < DOWNLOAD_TIME_MAX.as_secs());
const _: () = assert!(CANCEL_EVERY.as_millis() < POLL_EVERY.as_millis());

/// True for a file or folder whose data lives only in iCloud: macOS marks such a placeholder
/// `SF_DATALESS` and fetches its data on first use, which a launchd agent may not do.
/// Input: the entry's own metadata, links not followed. Output: false off macOS.
pub fn dataless(metadata: &fs::Metadata) -> bool {
    #[cfg(target_os = "macos")]
    {
        use std::os::macos::fs::MetadataExt;
        metadata.st_flags() & SF_DATALESS != 0
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (metadata, SF_DATALESS);
        false
    }
}

/// How far one iCloud download has come: the placeholders seen so far and how many of them
/// are on this Mac now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Progress {
    pub total: u64,
    pub done: u64,
}

/// One running download's shared state: its progress, read by progress requests, and its
/// cancel flag, set by a cancel request or when the request that started it ends.
#[derive(Debug, Default)]
pub struct Download {
    total: AtomicU64,
    done: AtomicU64,
    cancelled: AtomicBool,
}

impl Download {
    /// The progress last recorded. Output: done never above total.
    pub fn progress(&self) -> Progress {
        let total = self.total.load(Ordering::SeqCst);
        let done = self.done.load(Ordering::SeqCst).min(total);
        assert!(done <= total);
        Progress { total, done }
    }

    /// Asks the download to stop at its next check, within `CANCEL_EVERY`.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }

    /// Whether the download was asked to stop.
    pub fn cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }

    fn record(&self, total: u64, done: u64) {
        assert!(done <= total, "{done} of {total}");
        assert!(
            total >= self.total.load(Ordering::SeqCst),
            "total never shrinks"
        );
        self.total.store(total, Ordering::SeqCst);
        self.done.store(done, Ordering::SeqCst);
    }
}

/// The engine's one download slot, so at most one iCloud download runs at a time.
#[derive(Debug, Default)]
pub struct Downloads(Mutex<Option<Arc<Download>>>);

impl Downloads {
    /// Takes the slot for a new download. Output: the claim, which cancels the download and
    /// frees the slot when dropped, or `icloud_download_busy` while another one runs.
    pub fn claim(&self) -> Result<Claim<'_>, EngineError> {
        let mut slot = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if slot.is_some() {
            return Err(refused("icloud_download_busy"));
        }
        let download = Arc::new(Download::default());
        *slot = Some(Arc::clone(&download));
        assert!(slot.is_some());
        Ok(Claim {
            slot: self,
            download,
        })
    }

    /// The running download's progress. Output: None when none runs.
    pub fn progress(&self) -> Option<Progress> {
        let slot = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        slot.as_ref().map(|download| download.progress())
    }

    /// Cancels the running download. Output: whether one was running.
    pub fn cancel(&self) -> bool {
        let slot = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        match slot.as_ref() {
            Some(download) => {
                download.cancel();
                true
            }
            None => false,
        }
    }
}

/// A held download slot. Dropping it cancels its download and frees the slot, so a request
/// that ends for any reason never leaves a download running unwatched.
#[derive(Debug)]
pub struct Claim<'a> {
    slot: &'a Downloads,
    download: Arc<Download>,
}

impl Claim<'_> {
    /// The download this claim holds, to hand to the thread that runs it.
    pub fn download(&self) -> Arc<Download> {
        Arc::clone(&self.download)
    }
}

impl Drop for Claim<'_> {
    fn drop(&mut self) {
        self.download.cancel();
        let mut slot = self.slot.0.lock().unwrap_or_else(PoisonError::into_inner);
        let held = slot
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, &self.download));
        assert!(held, "a claim frees only its own slot");
        *slot = None;
    }
}

/// Lists the placeholders under a folder without following links and without listing a
/// placeholder folder, which a process that may not download cannot read.
/// Input: the real folder. Output: their paths, `home_copy_too_large` past the copy bounds,
/// or the code of the read that failed.
pub fn placeholders(root: &Path) -> Result<Vec<PathBuf>, EngineError> {
    let mut found = Vec::new();
    let mut stack = vec![(root.to_path_buf(), 0usize)];
    let mut entries = 0u64;
    while let Some((folder, depth)) = stack.pop() {
        for entry in fs::read_dir(&folder).map_err(|e| read_failed(&e))? {
            entries += 1;
            if entries > COPY_BOUNDS.entries {
                return Err(refused("home_copy_too_large"));
            }
            let path = entry.map_err(|e| read_failed(&e))?.path();
            let metadata = fs::symlink_metadata(&path).map_err(|e| read_failed(&e))?;
            if dataless(&metadata) {
                found.push(path);
            } else if metadata.is_dir() && !metadata.file_type().is_symlink() {
                if depth + 1 > COPY_BOUNDS.depth {
                    return Err(refused("home_copy_too_large"));
                }
                stack.push((path, depth + 1));
            }
        }
    }
    assert!(found.len() as u64 <= entries);
    Ok(found)
}

/// Downloads every placeholder in a folder: asks macOS once per placeholder through
/// `brctl download`, in batches, then lists the folder again every `POLL_EVERY` until no
/// placeholder is left, recording progress as it goes.
/// Inputs: the owner's folder text and the claimed download. Output: how many placeholders
/// were downloaded, `home_icloud_not_downloaded` when cancelled, when no file arrives for
/// `STALL_MAX`, after `DOWNLOAD_TIME_MAX` or when brctl fails, `home_permission_denied` when
/// macOS keeps the folder from CrazyAgent, or the folder check's code. The folder's files are
/// never moved or changed; downloads macOS already started may still finish after a stop.
pub fn download(path: &str, download: &Download) -> Result<u64, EngineError> {
    path_text(path, "icloud_path_invalid")?;
    let root = real_folder(
        Path::new(path),
        "icloud_folder_missing",
        "icloud_folder_unsafe",
    )?;
    if holds_owner_home(&root, &owner_home()?) {
        return Err(refused("icloud_folder_unsafe"));
    }
    let started = Instant::now();
    let mut watch = Watch::new(started, started + DOWNLOAD_TIME_MAX);
    let mut asked: HashSet<u64> = HashSet::new();
    let hasher = BuildHasherDefault::<DefaultHasher>::default();
    for _ in 0..PASSES_MAX {
        let left = placeholders(&root)?;
        let fresh: Vec<PathBuf> = left
            .iter()
            .filter(|path| asked.insert(hasher.hash_one(path)))
            .cloned()
            .collect();
        let total = asked.len() as u64;
        let done = total.saturating_sub(left.len() as u64);
        download.record(total, done);
        if left.is_empty() {
            return Ok(total);
        }
        if download.cancelled() {
            return Err(stopped("cancelled"));
        }
        for batch in fresh.chunks(BATCH_PATHS_MAX) {
            request(batch, download)?;
        }
        match watch.observe(done, Instant::now()) {
            Verdict::Wait => pause(download, POLL_EVERY)?,
            Verdict::Stalled => return Err(stopped("stalled")),
            Verdict::TimedOut => return Err(stopped("timed_out")),
        }
    }
    Err(stopped("timed_out"))
}

/// Builds the `brctl download` command for one batch: the absolute program, the paths as
/// plain arguments, no shell, an empty environment and no input or output.
/// Input: absolute paths, at most `BATCH_PATHS_MAX`. Output: the command.
pub fn brctl(batch: &[PathBuf]) -> Command {
    assert!(!batch.is_empty() && batch.len() <= BATCH_PATHS_MAX);
    assert!(batch.iter().all(|path| path.is_absolute()));
    let mut command = Command::new(BRCTL);
    command
        .arg("download")
        .args(batch)
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command
}

/// Asks macOS to download one batch. brctl hands the batch to iCloud and returns at once,
/// so it gets `BRCTL_TIME_MAX` before it is stopped. Inputs: the batch and the download,
/// whose cancel ends the wait. Output: Ok once brctl took the batch.
fn request(batch: &[PathBuf], download: &Download) -> Result<(), EngineError> {
    let mut child = crate::spawn_gate::spawn(&mut brctl(batch)).map_err(|error| {
        eprintln!(
            "icloud_download_failed cause=brctl_spawn errno={}",
            errno(&error)
        );
        refused(NOT_DOWNLOADED)
    })?;
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(status)) => {
                eprintln!(
                    "icloud_download_failed cause=brctl_exit code={:?}",
                    status.code()
                );
                return Err(refused(NOT_DOWNLOADED));
            }
            Ok(None) if started.elapsed() < BRCTL_TIME_MAX && !download.cancelled() => {
                std::thread::sleep(CANCEL_EVERY);
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                let cause = if download.cancelled() {
                    "cancelled"
                } else {
                    "brctl_wait"
                };
                return Err(stopped(cause));
            }
        }
    }
}

/// Sleeps for one poll, waking every `CANCEL_EVERY` to see a cancel.
/// Inputs: the download and how long to sleep. Output: Err once cancelled.
fn pause(download: &Download, span: Duration) -> Result<(), EngineError> {
    let until = Instant::now() + span;
    while Instant::now() < until {
        if download.cancelled() {
            return Err(stopped("cancelled"));
        }
        std::thread::sleep(CANCEL_EVERY.min(until.saturating_duration_since(Instant::now())));
    }
    if download.cancelled() {
        return Err(stopped("cancelled"));
    }
    Ok(())
}

/// Decides after each pass whether a download keeps waiting: it stops as stalled once no
/// more files arrived for `STALL_MAX`, which on a lost connection ends the wait instead of
/// holding it for hours, and as timed out at its deadline, which bounds a very slow trickle.
#[derive(Debug)]
struct Watch {
    deadline: Instant,
    most: u64,
    since: Instant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verdict {
    Wait,
    Stalled,
    TimedOut,
}

impl Watch {
    fn new(now: Instant, deadline: Instant) -> Self {
        assert!(deadline > now);
        Self {
            deadline,
            most: 0,
            since: now,
        }
    }

    /// Takes one pass's count of files on this Mac. Inputs: the count and the time.
    /// Output: the verdict.
    fn observe(&mut self, done: u64, now: Instant) -> Verdict {
        assert!(now >= self.since);
        if done > self.most {
            self.most = done;
            self.since = now;
        }
        if now >= self.deadline {
            Verdict::TimedOut
        } else if now.duration_since(self.since) >= STALL_MAX {
            Verdict::Stalled
        } else {
            Verdict::Wait
        }
    }
}

fn read_failed(error: &std::io::Error) -> EngineError {
    let code = match error.kind() {
        std::io::ErrorKind::PermissionDenied => "home_permission_denied",
        std::io::ErrorKind::Deadlock => NOT_DOWNLOADED,
        _ => "icloud_folder_unreadable",
    };
    eprintln!(
        "icloud_download_failed cause=read code={code} errno={}",
        errno(error)
    );
    refused(code)
}

fn stopped(cause: &str) -> EngineError {
    eprintln!("icloud_download_failed cause={cause}");
    refused(NOT_DOWNLOADED)
}

fn errno(error: &std::io::Error) -> i32 {
    error.raw_os_error().unwrap_or(0)
}

#[cfg(test)]
mod tests;
