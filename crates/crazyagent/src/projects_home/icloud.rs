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
use std::sync::{Arc, Mutex, PoisonError, mpsc};
use std::time::{Duration, Instant};

const SF_DATALESS: u32 = 0x4000_0000;
const BRCTL: &str = "/usr/bin/brctl";
pub const BATCH_PATHS_MAX: usize = 128;
pub const STALL_MAX: Duration = Duration::from_secs(10 * 60);
const STALL_FLOOR_BYTES_PER_SEC: u64 = 1024 * 1024;
pub const DOWNLOAD_TIME_MAX: Duration = Duration::from_secs(4 * 60 * 60);
const POLL_EVERY: Duration = Duration::from_secs(1);
const CANCEL_EVERY: Duration = Duration::from_millis(100);
const BRCTL_TIME_MAX: Duration = Duration::from_secs(60);
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
        self.claim_for(Arc::new(Download::default()))
    }

    /// Takes the slot for a download whose state another holder reads too, an intake job;
    /// one already cancelled stops at its first check. Input: the download. Output: as
    /// [`Downloads::claim`].
    pub fn claim_for(&self, download: Arc<Download>) -> Result<Claim<'_>, EngineError> {
        let mut slot = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if slot.is_some() {
            return Err(refused("icloud_download_busy"));
        }
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

/// The placeholders one listing found: their paths and the logical size of the largest,
/// which sets how long the wait allows its download.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Placeholders {
    pub paths: Vec<PathBuf>,
    pub largest_bytes: u64,
}

/// Lists the placeholders under a folder without following links and without listing a
/// placeholder folder, which a process that may not download cannot read; a folder that is
/// a placeholder itself is the one placeholder found.
/// Input: the real folder. Output: the placeholders, `home_copy_too_large` past the copy
/// bounds, or the code of the read that failed.
pub fn placeholders(root: &Path) -> Result<Placeholders, EngineError> {
    let top = fs::symlink_metadata(root).map_err(|e| read_failed(&e))?;
    if dataless(&top) {
        return Ok(Placeholders {
            paths: vec![root.to_path_buf()],
            largest_bytes: top.len(),
        });
    }
    let mut found = Placeholders::default();
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
                found.largest_bytes = found.largest_bytes.max(metadata.len());
                found.paths.push(path);
            } else if metadata.is_dir() && !metadata.file_type().is_symlink() {
                if depth + 1 > COPY_BOUNDS.depth {
                    return Err(refused("home_copy_too_large"));
                }
                stack.push((path, depth + 1));
            }
        }
    }
    assert!(found.paths.len() as u64 <= entries);
    assert!(found.largest_bytes == 0 || !found.paths.is_empty());
    Ok(found)
}

/// Downloads every placeholder in a folder: asks macOS once per placeholder through
/// `brctl download`, in batches, from a second thread, while this one lists the folder every
/// `POLL_EVERY` until no placeholder is left and records progress as files land.
/// Inputs: the owner's folder text and the claimed download. Output: how many placeholders
/// were downloaded, `home_icloud_not_downloaded` when cancelled, when no file arrives for
/// `STALL_MAX` plus the largest file's time at 1 MiB/s, after `DOWNLOAD_TIME_MAX` or when
/// brctl fails, `home_copy_too_large` past the copy bounds, `home_permission_denied` when
/// macOS keeps the folder from CrazyAgent, or the folder check's code. The folder's files are
/// never moved or changed; downloads macOS already started may still finish after a stop.
pub fn download(path: &str, download: &Download) -> Result<u64, EngineError> {
    let root = folder(path)?;
    run(&root, download, &request, &placeholders, POLL_EVERY)
}

/// The real folder a download or an intake job may work on, checked without reading its
/// files. Input: the owner's folder text. Output: the real folder, or
/// `icloud_path_invalid`, `icloud_folder_missing` or `icloud_folder_unsafe` for a folder
/// that holds the owner's home.
pub fn folder(path: &str) -> Result<PathBuf, EngineError> {
    path_text(path, "icloud_path_invalid")?;
    let root = real_folder(
        Path::new(path),
        "icloud_folder_missing",
        "icloud_folder_unsafe",
    )?;
    if holds_owner_home(&root, &owner_home()?) {
        return Err(refused("icloud_folder_unsafe"));
    }
    assert!(root.is_absolute());
    Ok(root)
}

/// Asks macOS for one batch of placeholders. Inputs: the batch and a stop check that ends
/// the wait for the asking process. Output: Ok once macOS took the batch, else the code.
type Ask<'a> = dyn Fn(&[PathBuf], &dyn Fn() -> bool) -> Result<(), EngineError> + Sync + 'a;

/// Lists the placeholders still under a folder. Input: the real folder. Output: as
/// [`placeholders`].
type List<'a> = dyn Fn(&Path) -> Result<Placeholders, EngineError> + Sync + 'a;

/// The download loop behind [`download`], with its ask, its listing and its poll given so a
/// test can stand in for iCloud. A second thread asks for the batches in order while this one
/// lists the folder every `poll` and records progress, because an ask from the engine can take
/// seconds and files land while later batches still wait to be asked. Inputs: the real
/// folder, the download, the ask, the listing and the time between listings. Output: as
/// [`download`]; the asking thread has ended when it returns.
fn run(
    root: &Path,
    download: &Download,
    ask: &Ask<'_>,
    list: &List<'_>,
    poll: Duration,
) -> Result<u64, EngineError> {
    assert!(root.is_absolute());
    assert!(poll > Duration::ZERO && poll <= POLL_EVERY);
    let ended = AtomicBool::new(false);
    let failed: Mutex<Option<EngineError>> = Mutex::new(None);
    let stop = || ended.load(Ordering::SeqCst) || download.cancelled();
    let (batches, queue) = mpsc::channel::<Vec<PathBuf>>();
    let outcome = std::thread::scope(|scope| {
        let asking = {
            let (stop, failed) = (&stop, &failed);
            move || send(queue, ask, stop, failed)
        };
        scope.spawn(asking);
        let outcome = count(root, download, list, poll, &batches, &failed);
        ended.store(true, Ordering::SeqCst);
        drop(batches);
        outcome
    });
    assert!(ended.load(Ordering::SeqCst));
    outcome
}

/// Lists the folder every `poll`, records how many placeholders it has seen and how many of
/// them are on this Mac now, and queues each placeholder for the asking thread once.
/// Inputs: the real folder, the download, the listing, the poll, the queue and the asking
/// thread's failure slot. Output: as [`download`], the asking thread's failure included.
fn count(
    root: &Path,
    download: &Download,
    list: &List<'_>,
    poll: Duration,
    batches: &mpsc::Sender<Vec<PathBuf>>,
    failed: &Mutex<Option<EngineError>>,
) -> Result<u64, EngineError> {
    let started = Instant::now();
    let mut watch = Watch::new(started, started + DOWNLOAD_TIME_MAX);
    let mut asked: HashSet<u64> = HashSet::new();
    let hasher = BuildHasherDefault::<DefaultHasher>::default();
    let passes_max = DOWNLOAD_TIME_MAX.as_millis() / poll.as_millis().max(1) + 1;
    assert!(passes_max > 1);
    for _ in 0..passes_max {
        let left = list(root)?;
        let fresh: Vec<PathBuf> = left
            .paths
            .iter()
            .filter(|path| asked.insert(hasher.hash_one(path)))
            .cloned()
            .collect();
        let total = asked.len() as u64;
        if total > COPY_BOUNDS.entries {
            return Err(refused("home_copy_too_large"));
        }
        let done = total.saturating_sub(left.paths.len() as u64);
        download.record(total, done);
        if left.paths.is_empty() {
            return Ok(total);
        }
        if download.cancelled() {
            return Err(stopped("cancelled"));
        }
        if let Some(error) = failed.lock().unwrap_or_else(PoisonError::into_inner).take() {
            return Err(error);
        }
        for batch in fresh.chunks(BATCH_PATHS_MAX) {
            if batches.send(batch.to_vec()).is_err() {
                break;
            }
        }
        match watch.observe(done, left.largest_bytes, Instant::now()) {
            Verdict::Wait => pause(download, poll)?,
            Verdict::Stalled => return Err(stopped("stalled")),
            Verdict::TimedOut => return Err(stopped("timed_out")),
        }
    }
    Err(stopped("timed_out"))
}

/// The asking thread: asks for each queued batch in order until the queue closes or the stop
/// check holds, and keeps its first failure for the counting loop, then asks nothing more.
/// Inputs: the queue, the ask, the stop check and the failure slot.
fn send(
    queue: mpsc::Receiver<Vec<PathBuf>>,
    ask: &Ask<'_>,
    stop: &(dyn Fn() -> bool + Sync),
    failed: &Mutex<Option<EngineError>>,
) {
    while let Ok(batch) = queue.recv() {
        assert!(!batch.is_empty());
        assert!(batch.len() <= BATCH_PATHS_MAX);
        if stop() {
            return;
        }
        if let Err(error) = ask(&batch, stop) {
            let mut slot = failed.lock().unwrap_or_else(PoisonError::into_inner);
            assert!(
                slot.is_none(),
                "the asking thread stops at its first failure"
            );
            *slot = Some(error);
            return;
        }
    }
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

/// Asks macOS to download one batch. brctl hands the batch to iCloud and returns, usually
/// within a second, though from the engine it has taken up to 30 s per batch, so it gets
/// `BRCTL_TIME_MAX` before it is stopped. Inputs: the batch and the stop check, which ends
/// the wait. Output: Ok once brctl took the batch.
fn request(batch: &[PathBuf], stop: &dyn Fn() -> bool) -> Result<(), EngineError> {
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
            Ok(None) if started.elapsed() < BRCTL_TIME_MAX && !stop() => {
                std::thread::sleep(CANCEL_EVERY);
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                if stop() {
                    return Err(refused(NOT_DOWNLOADED));
                }
                return Err(stopped("brctl_wait"));
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
/// more files arrived for `STALL_MAX` plus the time the largest file left takes at
/// `STALL_FLOOR_BYTES_PER_SEC`, which on a lost connection ends the wait instead of holding
/// it for hours, and as timed out at its deadline, which bounds a very slow trickle.
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

    /// Takes one pass's count of files on this Mac. Inputs: the count, the logical size of
    /// the largest placeholder left and the time. Output: the verdict.
    fn observe(&mut self, done: u64, largest_bytes: u64, now: Instant) -> Verdict {
        assert!(now >= self.since);
        if done > self.most {
            self.most = done;
            self.since = now;
        }
        let quiet_max = STALL_MAX.saturating_add(Duration::from_secs(
            largest_bytes / STALL_FLOOR_BYTES_PER_SEC,
        ));
        assert!(quiet_max >= STALL_MAX);
        if now >= self.deadline {
            Verdict::TimedOut
        } else if now.duration_since(self.since) >= quiet_max {
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
