use super::intake::{CopyBounds, FORCED_COPY_BOUNDS};
use super::{holds_owner_home, owner_home, path_text, real_folder, refused};
use crate::engine::EngineError;
use serde::{Deserialize, Serialize};
use std::collections::{HashSet, VecDeque};
use std::fs;
use std::hash::{BuildHasher, BuildHasherDefault, DefaultHasher};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError, mpsc};
use std::time::{Duration, Instant};

const SF_DATALESS: u32 = 0x4000_0000;
const BRCTL: &str = "/usr/bin/brctl";
pub const BATCH_PATHS_MAX: usize = 128;
pub const OPEN_FILES_MAX: usize = 512;
pub const JOB_OPEN_FILES_MAX: usize = 256;
pub const OPEN_BYTES_MAX: u64 = 1024 * 1024 * 1024;
pub const STALL_MAX: Duration = Duration::from_secs(10 * 60);
const STALL_FLOOR_BYTES_PER_SEC: u64 = 1024 * 1024;
pub const DOWNLOAD_TIME_MAX: Duration = Duration::from_secs(4 * 60 * 60);
const TURN_WAIT_MAX: Duration = DOWNLOAD_TIME_MAX;
const POLL_EVERY: Duration = Duration::from_secs(1);
const CANCEL_EVERY: Duration = Duration::from_millis(100);
const BRCTL_TIME_MAX: Duration = Duration::from_secs(60);
const CHECKS_MAX: usize = 3;
const LANDING_LOG_EVERY: Duration = Duration::from_secs(60);
const GITIGNORE_BYTES_MAX: u64 = 256 * 1024;
const REBUILDABLE: [&str; 3] = ["node_modules", ".venv", "__pycache__"];
const REBUILDABLE_IF_IGNORED: [&str; 2] = ["dist", "build"];
const NOT_DOWNLOADED: &str = "home_icloud_not_downloaded";

const _: () = assert!(BATCH_PATHS_MAX * 4096 < 1024 * 1024);
const _: () = assert!(JOB_OPEN_FILES_MAX >= BATCH_PATHS_MAX);
const _: () = assert!(JOB_OPEN_FILES_MAX <= OPEN_FILES_MAX);
const _: () = assert!(JOB_OPEN_FILES_MAX % BATCH_PATHS_MAX == 0);
const _: () = assert!(OPEN_FILES_MAX % BATCH_PATHS_MAX == 0);
const _: () = assert!(STALL_MAX.as_secs() < DOWNLOAD_TIME_MAX.as_secs());
const _: () = assert!(LANDING_LOG_EVERY.as_secs() < STALL_MAX.as_secs());
const _: () = assert!(CANCEL_EVERY.as_millis() < POLL_EVERY.as_millis());

/// True for a file or folder whose data lives only in iCloud: macOS marks such a placeholder
/// `SF_DATALESS` and fetches its data on first use, which a launchd agent may not do.
/// Input: the entry's own metadata, links not followed. Output: false off macOS, and under
/// test on a thread that asked for [`fake`] placeholders, true for its marked mode.
pub fn dataless(metadata: &fs::Metadata) -> bool {
    #[cfg(all(test, unix))]
    if fake::on() {
        use std::os::unix::fs::PermissionsExt;
        return metadata.permissions().mode() & 0o777 == fake::MODE;
    }
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

/// Placeholders for rows on any Unix: a thread that asks reads an entry whose mode is
/// exactly [`fake::MODE`] as only in iCloud, and a row lands it by changing its mode.
#[cfg(all(test, unix))]
pub(crate) mod fake {
    use std::cell::Cell;

    pub(crate) const MODE: u32 = 0o701;

    thread_local! {
        static ON: Cell<bool> = const { Cell::new(false) };
    }

    /// Makes this thread read entries of mode [`MODE`] as placeholders. Output: none.
    pub(crate) fn placeholders() {
        ON.with(|on| on.set(true));
    }

    /// Whether this thread asked for fake placeholders. Output: the answer.
    pub(crate) fn on() -> bool {
        ON.with(Cell::get)
    }
}

/// How far one iCloud download has come: the files and folders it asks macOS for and how many
/// of them are on this Mac now, and their logical bytes the same way.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Progress {
    pub total: u64,
    pub done: u64,
    #[serde(default)]
    pub bytes_total: u64,
    #[serde(default)]
    pub bytes_done: u64,
}

/// One running download's shared state: its progress, read through its intake job, the
/// rebuildable folders its last listing left out, and its cancel flag, set by the job's stop
/// or when the request that started it ends.
#[derive(Debug, Default)]
pub struct Download {
    total: AtomicU64,
    done: AtomicU64,
    bytes_total: AtomicU64,
    bytes_done: AtomicU64,
    skipped: Mutex<SkippedFolders>,
    cancelled: AtomicBool,
}

impl Download {
    /// The progress last recorded. Output: done never above total, for files and bytes.
    pub fn progress(&self) -> Progress {
        let total = self.total.load(Ordering::SeqCst);
        let done = self.done.load(Ordering::SeqCst).min(total);
        let bytes_total = self.bytes_total.load(Ordering::SeqCst);
        let bytes_done = self.bytes_done.load(Ordering::SeqCst).min(bytes_total);
        assert!(done <= total && bytes_done <= bytes_total);
        Progress {
            total,
            done,
            bytes_total,
            bytes_done,
        }
    }

    /// Asks the download to stop at its next check, within `CANCEL_EVERY`.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }

    /// Whether the download was asked to stop.
    pub fn cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }

    /// The rebuildable folders the download left in iCloud, as its listings found them; an
    /// add leaves the same ones behind. Output: the first `SKIPPED_LISTED_MAX` with their paths
    /// below the folder and file counts, and the count of the rest.
    pub fn skipped(&self) -> SkippedFolders {
        self.skipped
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Records the folders a listing left out. Inputs: the folders and whether the listing
    /// went through the whole folder, so they replace the ones recorded, or one landed folder.
    pub(super) fn leave(&self, folders: SkippedFolders, whole: bool) {
        let mut held = self.skipped.lock().unwrap_or_else(PoisonError::into_inner);
        if whole {
            *held = SkippedFolders::default();
        }
        held.absorb(folders);
        assert!(held.len() <= FORCED_COPY_BOUNDS.entries);
    }

    fn record(&self, now: Progress) {
        assert!(now.done <= now.total, "{} of {}", now.done, now.total);
        assert!(now.bytes_done <= now.bytes_total);
        assert!(
            now.total >= self.total.load(Ordering::SeqCst)
                && now.bytes_total >= self.bytes_total.load(Ordering::SeqCst),
            "totals never shrink"
        );
        self.total.store(now.total, Ordering::SeqCst);
        self.done.store(now.done, Ordering::SeqCst);
        self.bytes_total.store(now.bytes_total, Ordering::SeqCst);
        self.bytes_done.store(now.bytes_done, Ordering::SeqCst);
    }
}

/// The most iCloud downloads the engine holds at once: more than the intake jobs that run side
/// by side, every one opening its files through the one paced queue.
pub const DOWNLOADS_MAX: usize = 8;

/// The engine's running iCloud downloads, one per intake job that fetches: up to
/// `DOWNLOADS_MAX` run side by side, as the paced queue shares the open files among them. Each
/// job reads and cancels its own download.
#[derive(Debug, Default)]
pub struct Downloads(Mutex<Vec<Arc<Download>>>);

impl Downloads {
    /// Holds a download whose state another holder reads too, an intake job; one already
    /// cancelled stops at its first check. Input: the download, held at most once. Output:
    /// the claim, which cancels the download and lets it go when dropped, or
    /// `icloud_download_busy` while `DOWNLOADS_MAX` are held.
    pub fn claim_for(&self, download: Arc<Download>) -> Result<Claim<'_>, EngineError> {
        let mut held = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        assert!(
            !held.iter().any(|other| Arc::ptr_eq(other, &download)),
            "a download is held once"
        );
        if held.len() >= DOWNLOADS_MAX {
            return Err(refused("icloud_download_busy"));
        }
        held.push(Arc::clone(&download));
        assert!(held.len() <= DOWNLOADS_MAX);
        Ok(Claim {
            downloads: self,
            download,
        })
    }

    #[cfg(test)]
    fn held(&self) -> usize {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).len()
    }
}

/// A held download. Dropping it cancels the download and lets it go, so a request that ends
/// for any reason never leaves a download running unwatched.
#[derive(Debug)]
pub struct Claim<'a> {
    downloads: &'a Downloads,
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
        let mut held = self
            .downloads
            .0
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let at = held
            .iter()
            .position(|other| Arc::ptr_eq(other, &self.download));
        held.swap_remove(at.expect("a claim lets go only its own download"));
    }
}

/// The engine's one paced queue of iCloud asks, shared by every download: a download opens
/// its files through it, at most `JOB_OPEN_FILES_MAX` of its own and `OPEN_FILES_MAX` files
/// and `OPEN_BYTES_MAX` bytes of all, one batch per turn, the turn going to the download
/// served longest ago. A file asked for stays open until it lands or its download ends.
#[derive(Debug, Default)]
struct Pacer(Mutex<Lanes>);

#[derive(Debug, Default)]
struct Lanes {
    files: usize,
    bytes: u64,
    turns: u64,
    seated: u64,
    lanes: Vec<Lane>,
}

#[derive(Debug)]
struct Lane {
    id: u64,
    files: usize,
    bytes: u64,
    served: u64,
    wants: Want,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Want {
    Nothing,
    Files(usize),
    Alone,
}

static PACER: Pacer = Pacer::new();

impl Pacer {
    const fn new() -> Self {
        Self(Mutex::new(Lanes {
            files: 0,
            bytes: 0,
            turns: 0,
            seated: 0,
            lanes: Vec::new(),
        }))
    }

    fn lanes(&self) -> std::sync::MutexGuard<'_, Lanes> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Seats one download in the queue. Output: its seat, which gives back every file it
    /// still has open when dropped.
    fn seat(&self) -> Seat<'_> {
        let mut lanes = self.lanes();
        lanes.seated += 1;
        let id = lanes.seated;
        lanes.lanes.push(Lane {
            id,
            files: 0,
            bytes: 0,
            served: 0,
            wants: Want::Nothing,
        });
        lanes.check();
        Seat { pacer: self, id }
    }

    /// The files and bytes open across every download. Output: the two counts.
    #[cfg(test)]
    fn open_now(&self) -> (usize, u64) {
        let lanes = self.lanes();
        (lanes.files, lanes.bytes)
    }
}

impl Lanes {
    fn position(&self, id: u64) -> usize {
        let at = self.lanes.iter().position(|lane| lane.id == id);
        at.expect("a seat's lane stays until the seat is dropped")
    }

    /// The lane whose turn it is: a lane waiting to open a file larger than the byte cap
    /// first, else a lane wanting files its own cap leaves room for; of those, the one
    /// served longest ago. Output: its index, None when no lane wants files.
    fn turn(&self) -> Option<usize> {
        let first = |wanted: &dyn Fn(&Lane) -> bool| {
            self.lanes
                .iter()
                .enumerate()
                .filter(|(_, lane)| wanted(*lane))
                .min_by_key(|(_, lane)| (lane.served, lane.id))
                .map(|(at, _)| at)
        };
        first(&|lane| lane.wants == Want::Alone).or_else(|| {
            first(&|lane| matches!(lane.wants, Want::Files(count) if lane.files + count <= JOB_OPEN_FILES_MAX))
        })
    }

    /// Opens files for the lane at `at` when it has the turn and the caps leave room: the
    /// whole batch while it fits the file caps, only the first files that fit the byte cap,
    /// and a file larger than the byte cap only once nothing else is open. Inputs: the lane
    /// and the sizes of its next files. Output: how many it opened.
    fn grant(&mut self, at: usize, next: &[u64]) -> usize {
        if self.turn() != Some(at) {
            return 0;
        }
        let count = match self.lanes[at].wants {
            Want::Nothing => 0,
            Want::Alone => usize::from(self.files == 0),
            Want::Files(_) if self.bytes > OPEN_BYTES_MAX => 0,
            Want::Files(wanted) if self.files + wanted > OPEN_FILES_MAX => 0,
            Want::Files(_) => {
                let mut open = self.bytes;
                next.iter()
                    .take_while(|&&bytes| {
                        open = open.saturating_add(bytes);
                        open <= OPEN_BYTES_MAX
                    })
                    .count()
            }
        };
        if count == 0 {
            return 0;
        }
        let bytes: u64 = next[..count].iter().sum();
        self.files += count;
        self.bytes += bytes;
        self.turns += 1;
        let lane = &mut self.lanes[at];
        lane.files += count;
        lane.bytes += bytes;
        lane.served = self.turns;
        lane.wants = Want::Nothing;
        count
    }

    fn check(&self) {
        let files: usize = self.lanes.iter().map(|lane| lane.files).sum();
        let bytes: u64 = self.lanes.iter().map(|lane| lane.bytes).sum();
        assert_eq!((files, bytes), (self.files, self.bytes));
        assert!(self.files <= OPEN_FILES_MAX);
        assert!(self.bytes <= OPEN_BYTES_MAX || self.files == 1);
        assert!(
            self.lanes
                .iter()
                .all(|lane| lane.files <= JOB_OPEN_FILES_MAX)
        );
        assert!(self.lanes.iter().all(|lane| lane.served <= self.turns));
    }
}

/// One download's place in the [`Pacer`].
#[derive(Debug)]
struct Seat<'a> {
    pacer: &'a Pacer,
    id: u64,
}

impl Seat<'_> {
    /// Asks to open this download's next files. Input: the logical sizes of its next files,
    /// at most one batch, empty when none waits. Output: how many of them it may ask macOS
    /// for now, 0 while the caps are full or another download has the turn.
    fn open(&self, next: &[u64]) -> usize {
        assert!(next.len() <= BATCH_PATHS_MAX);
        let mut lanes = self.pacer.lanes();
        let at = lanes.position(self.id);
        lanes.lanes[at].wants = match next.first() {
            None => Want::Nothing,
            Some(&bytes) if bytes > OPEN_BYTES_MAX => Want::Alone,
            Some(_) => Want::Files(next.len()),
        };
        let count = lanes.grant(at, next);
        lanes.check();
        assert!(count <= next.len());
        count
    }

    /// Gives back files that landed. Inputs: how many and their logical bytes.
    fn landed(&self, files: usize, bytes: u64) {
        let mut lanes = self.pacer.lanes();
        let at = lanes.position(self.id);
        {
            let lane = &mut lanes.lanes[at];
            assert!(lane.files >= files && lane.bytes >= bytes);
            lane.files -= files;
            lane.bytes -= bytes;
        }
        lanes.files -= files;
        lanes.bytes -= bytes;
        lanes.check();
    }

    /// The files and bytes this download has open. Output: the two counts.
    #[cfg(test)]
    fn open_now(&self) -> (usize, u64) {
        let lanes = self.pacer.lanes();
        let lane = &lanes.lanes[lanes.position(self.id)];
        (lane.files, lane.bytes)
    }
}

impl Drop for Seat<'_> {
    fn drop(&mut self) {
        let mut lanes = self.pacer.lanes();
        let at = lanes.position(self.id);
        let lane = lanes.lanes.swap_remove(at);
        lanes.files -= lane.files;
        lanes.bytes -= lane.bytes;
        lanes.check();
    }
}

/// One entry a download asks macOS for: a file or folder only in iCloud, its logical size and
/// how many folders below the download's folder it sits.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Wanted {
    path: PathBuf,
    bytes: u64,
    depth: usize,
}

/// A rebuildable folder an add leaves out because it still needs downloading: its path below
/// the added folder and how many files it holds, None when the folder itself is only in
/// iCloud and cannot be listed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Skipped {
    pub path: String,
    pub files: Option<u64>,
}

/// The most rebuildable folders a folder-size reply or a download names; past it they are only
/// counted, so a tree with thousands of them keeps its reply inside one frame.
pub const SKIPPED_LISTED_MAX: usize = 32;

/// The most of them an intake job names, as its line shows three; a job list carries several
/// jobs in one reply, so each stays small.
pub const JOB_SKIPPED_LISTED_MAX: usize = 3;

/// The most bytes of one named folder's path; a longer one is cut and ends in "...".
pub const SKIPPED_PATH_MAX: usize = 256;

/// The most bytes one named folder takes in a reply: its path with every byte escaped as JSON
/// escapes a control character, six bytes each, and the field names and file count around it.
pub const SKIPPED_ENTRY_BYTES_MAX: usize = 6 * SKIPPED_PATH_MAX + 64;
const _: () = assert!(
    SKIPPED_LISTED_MAX * SKIPPED_ENTRY_BYTES_MAX + 4096 <= crate::protocol::FRAME_BYTES_MAX
);
const _: () = assert!(
    JOB_SKIPPED_LISTED_MAX * SKIPPED_ENTRY_BYTES_MAX <= crate::protocol::FRAME_BYTES_MAX / 8
);

/// The rebuildable folders past the named ones: how many, and the files they hold, None when
/// one of them is only in iCloud itself and cannot be counted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkippedMore {
    pub folders: u64,
    pub files: Option<u64>,
}

/// Rebuildable folders as a listing gathers them: `SKIPPED_LISTED_MAX` named, the first found,
/// except that a folder needing a reinstall takes the place of one that rebuilds itself; the
/// rest only counted, so the list stays bounded whatever the tree holds.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SkippedFolders {
    pub listed: Vec<Skipped>,
    pub more: Option<SkippedMore>,
}

/// Whether a rebuildable folder rebuilds itself the next time its program runs, so it needs
/// no reinstall: a `__pycache__`. Input: the folder's path. Output: the answer.
pub fn rebuilds_itself(path: &str) -> bool {
    Path::new(path).file_name() == Some(std::ffi::OsStr::new("__pycache__"))
}

impl SkippedFolders {
    /// Adds one folder, named while fewer than `SKIPPED_LISTED_MAX` are, else counted. Once the
    /// names are full, a folder that needs a reinstall takes the slot of the first named one
    /// that rebuilds itself, which is counted instead, so a reinstall offer sees every folder
    /// it needs and a job's line, which names the first few, names it before those. Input: the
    /// folder. Output: none.
    pub fn push(&mut self, folder: Skipped) {
        if self.listed.len() < SKIPPED_LISTED_MAX {
            self.listed.push(folder);
        } else if !rebuilds_itself(&folder.path)
            && let Some(at) = self
                .listed
                .iter()
                .position(|named| rebuilds_itself(&named.path))
        {
            let freed = std::mem::replace(&mut self.listed[at], folder);
            self.count(SkippedMore {
                folders: 1,
                files: freed.files,
            });
        } else {
            self.count(SkippedMore {
                folders: 1,
                files: folder.files,
            });
        }
        assert!(self.listed.len() <= SKIPPED_LISTED_MAX);
    }

    fn count(&mut self, extra: SkippedMore) {
        self.more = Some(match self.more {
            None => extra,
            Some(more) => SkippedMore {
                folders: more.folders.saturating_add(extra.folders),
                files: more
                    .files
                    .zip(extra.files)
                    .map(|(had, adds)| had.saturating_add(adds)),
            },
        });
    }

    /// Adds every folder another listing gathered. Input: those folders. Output: none.
    pub fn absorb(&mut self, other: SkippedFolders) {
        for folder in other.listed {
            self.push(folder);
        }
        if let Some(more) = other.more {
            self.count(more);
        }
    }

    /// How many folders were gathered, named or counted. Output: the count.
    pub fn len(&self) -> u64 {
        self.listed.len() as u64 + self.more.map_or(0, |more| more.folders)
    }

    /// Whether no folder was gathered. Output: the answer.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The folders as a reply names them. Input: the most to name. Output: the named folders
    /// and the count of the rest.
    pub fn cut(&self, cap: usize) -> (Vec<Skipped>, Option<SkippedMore>) {
        let at = cap.min(self.listed.len());
        let mut rest = SkippedFolders {
            listed: Vec::new(),
            more: self.more,
        };
        for folder in &self.listed[at..] {
            rest.count(SkippedMore {
                folders: 1,
                files: folder.files,
            });
        }
        assert_eq!(at as u64 + rest.len(), self.len());
        (self.listed[..at].to_vec(), rest.more)
    }
}

/// What one listing of a download's folder found: the entries to ask for, the rebuildable
/// folders left out and how many entries it went through.
#[derive(Debug, Default)]
struct Listing {
    wanted: Vec<Wanted>,
    skipped: SkippedFolders,
    entries: u64,
}

/// What a folder holds: the entries listed, the files and links among them, placeholders
/// included, the placeholders, a placeholder folder counted once and never listed, and the
/// bytes of the files on this Mac.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) struct Census {
    pub entries: u64,
    pub files: u64,
    pub placeholders: u64,
    pub bytes: u64,
}

/// Reads one entry's own metadata, links not followed: `fs::symlink_metadata`, or a row's
/// stand-in that counts the calls.
pub(super) type Look<'a> = dyn Fn(&Path) -> std::io::Result<fs::Metadata> + 'a;

/// Lists what a download asks for under one folder, without following links and without
/// listing a placeholder folder; with `bounds.skip_rebuildable`, a rebuildable folder that is
/// or holds a placeholder is left out and named, and one fully on this Mac needs nothing.
/// Inputs: the download's folder, the folder to list under it, that folder's depth, the
/// bounds and the metadata read. Output: the listing, `home_copy_too_large` past the bounds.
fn survey(
    root: &Path,
    below: &Path,
    depth: usize,
    bounds: CopyBounds,
    look: &Look<'_>,
) -> Result<Listing, EngineError> {
    assert!(below.starts_with(root));
    let mut found = Listing::default();
    let top = look(below).map_err(|e| read_failed(&e))?;
    if dataless(&top) {
        found.wanted.push(Wanted {
            path: below.to_path_buf(),
            bytes: top.len(),
            depth,
        });
        return Ok(found);
    }
    let mut stack = vec![(below.to_path_buf(), depth)];
    while let Some((folder, depth)) = stack.pop() {
        for entry in fs::read_dir(&folder).map_err(|e| read_failed(&e))? {
            found.entries += 1;
            if found.entries > bounds.entries {
                return Err(refused("home_copy_too_large"));
            }
            let path = entry.map_err(|e| read_failed(&e))?.path();
            let metadata = look(&path).map_err(|e| read_failed(&e))?;
            let inner = metadata.is_dir() && !metadata.file_type().is_symlink();
            if inner && bounds.skip_rebuildable && rebuildable(root, &path, look) {
                let held = match dataless(&metadata) {
                    true => None,
                    false => Some(census(&path, depth + 1, bounds, look)?),
                };
                found.entries += held.map_or(0, |held| held.entries);
                if found.entries > bounds.entries {
                    return Err(refused("home_copy_too_large"));
                }
                if held.is_none_or(|held| held.placeholders > 0) {
                    found.skipped.push(Skipped {
                        path: shown(root, &path),
                        files: held.map(|held| held.files),
                    });
                }
            } else if dataless(&metadata) {
                found.wanted.push(Wanted {
                    path,
                    bytes: metadata.len(),
                    depth: depth + 1,
                });
            } else if inner {
                if depth + 1 > bounds.depth {
                    return Err(refused("home_copy_too_large"));
                }
                stack.push((path, depth + 1));
            }
        }
    }
    assert!(found.wanted.len() as u64 + found.skipped.len() <= found.entries);
    Ok(found)
}

/// Counts what a folder holds without following links and without listing a placeholder
/// folder. Inputs: the folder, its depth, the bounds and the metadata read. Output: the
/// count, `home_copy_too_large` past the bounds, or the code of the read that failed.
pub(super) fn census(
    folder: &Path,
    depth: usize,
    bounds: CopyBounds,
    look: &Look<'_>,
) -> Result<Census, EngineError> {
    let mut held = Census::default();
    let mut stack = vec![(folder.to_path_buf(), depth)];
    while let Some((folder, depth)) = stack.pop() {
        for entry in fs::read_dir(&folder).map_err(|e| read_failed(&e))? {
            held.entries += 1;
            if held.entries > bounds.entries {
                return Err(refused("home_copy_too_large"));
            }
            let path = entry.map_err(|e| read_failed(&e))?.path();
            let metadata = look(&path).map_err(|e| read_failed(&e))?;
            let inner = metadata.is_dir() && !metadata.file_type().is_symlink();
            let placeholder = dataless(&metadata);
            held.placeholders += u64::from(placeholder);
            if !inner {
                held.files += 1;
                if metadata.is_file() && !placeholder {
                    held.bytes = held.bytes.saturating_add(metadata.len());
                }
            } else if !placeholder {
                if depth + 1 > bounds.depth {
                    return Err(refused("home_copy_too_large"));
                }
                stack.push((path, depth + 1));
            }
        }
    }
    assert!(held.files <= held.entries && held.placeholders <= held.entries);
    Ok(held)
}

/// Whether an add leaves a folder out: with `bounds.skip_rebuildable`, a rebuildable folder
/// that is only in iCloud itself or holds a placeholder. A folder it cannot count is not
/// left out, so the copy meets what stops it. Inputs: the added folder, the entry, its own
/// metadata, the bounds and the metadata read. Output: the answer.
pub(super) fn left_behind(
    root: &Path,
    folder: &Path,
    metadata: &fs::Metadata,
    bounds: CopyBounds,
    look: &Look<'_>,
) -> bool {
    bounds.skip_rebuildable
        && metadata.is_dir()
        && !metadata.file_type().is_symlink()
        && rebuildable(root, folder, look)
        && (dataless(metadata)
            || census(folder, 0, bounds, look).is_ok_and(|held| held.placeholders > 0))
}

/// Whether a folder can be rebuilt by the project's own tools: `node_modules`, `.venv` and
/// `__pycache__` by name, and `dist` or `build` when a `.gitignore` from the added folder down
/// names them plainly. Inputs: the added folder, the folder and the metadata read. Output:
/// the answer.
pub(super) fn rebuildable(root: &Path, folder: &Path, look: &Look<'_>) -> bool {
    let Some(name) = folder.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    REBUILDABLE.contains(&name)
        || (REBUILDABLE_IF_IGNORED.contains(&name) && git_ignores(root, folder, look))
}

/// Whether git ignores a folder by a plain line of a `.gitignore` in the added folder or a
/// folder between it and this one. Any negation, and any `.gitignore` that is a link, a
/// placeholder, too large or unreadable, reads as not ignored. Inputs: the added folder,
/// the folder and the metadata read. Output: the answer.
fn git_ignores(root: &Path, folder: &Path, look: &Look<'_>) -> bool {
    let (Some(parent), Some(name)) = (
        folder.parent(),
        folder.file_name().and_then(|name| name.to_str()),
    ) else {
        return false;
    };
    let Ok(between) = parent.strip_prefix(root) else {
        return false;
    };
    let mut dirs = vec![root.to_path_buf()];
    for part in between.components() {
        let next = dirs[dirs.len() - 1].join(part);
        dirs.push(next);
    }
    let mut named = false;
    for dir in &dirs {
        let Some(text) = gitignore(&dir.join(".gitignore"), look) else {
            return false;
        };
        let Some(below) = folder.strip_prefix(dir).ok().and_then(Path::to_str) else {
            return false;
        };
        for line in text.lines().map(str::trim_end) {
            if line.starts_with('!') {
                return false;
            }
            named |= !line.starts_with('#') && names(line, name, below);
        }
    }
    named
}

/// Whether one `.gitignore` line names a folder plainly: `name`, `name/`, `**/name`, or its
/// path below the `.gitignore`'s folder, with or without a leading or trailing slash. Inputs:
/// the line, the folder's name and its path below that folder. Output: the answer.
fn names(line: &str, name: &str, below: &str) -> bool {
    let pattern = line.strip_suffix('/').unwrap_or(line);
    if pattern.is_empty() || pattern.contains(['*', '?', '[', '\\']) {
        return pattern.strip_prefix("**/") == Some(name);
    }
    match pattern.strip_prefix('/') {
        Some(anchored) => anchored == below,
        None if pattern.contains('/') => pattern == below,
        None => pattern == name,
    }
}

/// Reads one `.gitignore` for the skip rule, opened only as the regular file its metadata
/// showed. Inputs: its path and the metadata read. Output: its text, empty when there is
/// none, or None for a link, a placeholder, a file over `GITIGNORE_BYTES_MAX` or a read error.
fn gitignore(path: &Path, look: &Look<'_>) -> Option<String> {
    let seen = match look(path) {
        Ok(seen) => seen,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Some(String::new()),
        Err(_) => return None,
    };
    if !seen.is_file() || dataless(&seen) || seen.len() > GITIGNORE_BYTES_MAX {
        return None;
    }
    let file = super::platform::open_seen(path, &seen).ok()?;
    let mut text = String::new();
    file.take(GITIGNORE_BYTES_MAX)
        .read_to_string(&mut text)
        .ok()?;
    Some(text)
}

/// A path below the added folder as Home shows it, cut at a character edge to
/// `SKIPPED_PATH_MAX` bytes ending in "..." when longer. Inputs: the added folder and the
/// path. Output: the text.
pub(super) fn shown(root: &Path, path: &Path) -> String {
    let text = path.strip_prefix(root).unwrap_or(path).to_string_lossy();
    if text.len() <= SKIPPED_PATH_MAX {
        return text.into_owned();
    }
    let mut end = SKIPPED_PATH_MAX - "...".len();
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let cut = format!("{}...", &text[..end]);
    assert!(cut.len() <= SKIPPED_PATH_MAX);
    cut
}

/// Downloads what of a folder is only in iCloud, paced through the engine's one queue: lists
/// the folder once, asks macOS for one batch at a time through `brctl download` from a
/// second thread as the queue opens room, reads only the asked entries every `POLL_EVERY`,
/// and lists the folder once more when all landed, asking again for anything macOS evicted.
/// Inputs: the owner's folder text, the claimed download and the add's bounds
/// ([`super::add_bounds`]). Output: how many entries were asked for and landed, else
/// `home_icloud_not_downloaded` (cancelled, stalled, timed out, brctl failed),
/// `home_copy_too_large`, `home_permission_denied` or the folder check's code.
pub fn download(path: &str, download: &Download, bounds: CopyBounds) -> Result<u64, EngineError> {
    assert!(bounds.entries <= FORCED_COPY_BOUNDS.entries);
    assert!(bounds.depth <= FORCED_COPY_BOUNDS.depth);
    let root = folder(path)?;
    let look = |path: &Path| fs::symlink_metadata(path);
    run(&root, download, &PACER, &request, &look, bounds, POLL_EVERY)
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

/// The download loop behind [`download`], with its queue, ask, metadata read and poll given
/// so a row can stand in for iCloud. A second thread asks for the batches in order while
/// this one paces them and records progress, because an ask can take seconds while files
/// land. Inputs: the real folder, the download, the queue, the ask, the read, the bounds and
/// the poll. Output: as [`download`]; the asking thread has ended when it returns.
fn run(
    root: &Path,
    download: &Download,
    pacer: &Pacer,
    ask: &Ask<'_>,
    look: &Look<'_>,
    bounds: CopyBounds,
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
        let paced = Paced {
            root,
            download,
            look,
            bounds,
            poll,
            batches: &batches,
            failed: &failed,
        };
        let outcome = paced.run(pacer);
        ended.store(true, Ordering::SeqCst);
        drop(batches);
        outcome
    });
    assert!(ended.load(Ordering::SeqCst));
    outcome
}

/// One download's pacing loop and what it works with.
struct Paced<'a> {
    root: &'a Path,
    download: &'a Download,
    look: &'a Look<'a>,
    bounds: CopyBounds,
    poll: Duration,
    batches: &'a mpsc::Sender<Vec<PathBuf>>,
    failed: &'a Mutex<Option<EngineError>>,
}

impl Paced<'_> {
    /// Lists the folder once, then each poll reads the open entries, lists a landed folder
    /// alone, checks the folder once all landed, opens the next batch the queue allows and
    /// watches for a stall, counted only while entries are open. Input: the queue. Output:
    /// as [`download`].
    fn run(&self, pacer: &Pacer) -> Result<u64, EngineError> {
        let started = Instant::now();
        let listing = survey(self.root, self.root, 0, self.bounds, self.look)?;
        if !listing.skipped.is_empty() {
            eprintln!("icloud_download_skipped folders={}", listing.skipped.len());
        }
        self.download.leave(listing.skipped, true);
        let mut entries = listing.entries;
        let mut queue = Queue::default();
        queue.add(listing.wanted);
        self.download.record(queue.progress);
        if queue.finished() {
            return Ok(0);
        }
        let seat = pacer.seat();
        let mut watch: Option<Watch> = None;
        let mut log = Landings::default();
        let mut checks = 0usize;
        let span = TURN_WAIT_MAX.saturating_add(DOWNLOAD_TIME_MAX);
        let passes_max = span.as_millis() / self.poll.as_millis().max(1) + 2;
        for _ in 0..passes_max {
            for folder in queue.look(self.look, &seat)? {
                let more = survey(
                    self.root,
                    &folder.path,
                    folder.depth,
                    self.bounds,
                    self.look,
                )?;
                entries += more.entries;
                if entries > self.bounds.entries {
                    return Err(refused("home_copy_too_large"));
                }
                self.download.leave(more.skipped, false);
                queue.add(more.wanted);
            }
            log.note(queue.landings, Instant::now());
            if queue.finished() {
                if checks == CHECKS_MAX {
                    return Err(stopped("evicted"));
                }
                checks += 1;
                let check = survey(self.root, self.root, 0, self.bounds, self.look)?;
                self.download.leave(check.skipped, true);
                if check.wanted.is_empty() {
                    self.download.record(queue.progress);
                    return Ok(queue.progress.total);
                }
                queue.add(check.wanted);
            }
            self.download.record(queue.progress);
            if self.download.cancelled() {
                return Err(stopped("cancelled"));
            }
            if let Some(error) = self.failure() {
                return Err(error);
            }
            let batch = queue.take(&seat);
            if !batch.is_empty() {
                log.asked(Instant::now());
                if self.batches.send(batch).is_err() {
                    return Err(self.failure().unwrap_or_else(|| stopped("ask_ended")));
                }
            }
            let now = Instant::now();
            let verdict = if queue.open.is_empty() {
                match watch.as_mut() {
                    Some(watch) => watch.rest(now),
                    None if now.duration_since(started) >= TURN_WAIT_MAX => Verdict::TimedOut,
                    None => Verdict::Wait,
                }
            } else {
                let watch = watch.get_or_insert_with(|| Watch::new(now, now + DOWNLOAD_TIME_MAX));
                watch.observe(queue.landings, queue.largest_open(), now)
            };
            match verdict {
                Verdict::Wait => pause(self.download, self.poll)?,
                Verdict::Stalled => return Err(stopped("stalled")),
                Verdict::TimedOut => return Err(stopped("timed_out")),
            }
        }
        Err(stopped("timed_out"))
    }

    fn failure(&self) -> Option<EngineError> {
        self.failed
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
    }
}

/// One download's entries: those waiting their turn, those asked for and not landed yet,
/// every path it has listed, its progress and how many landings it has seen.
#[derive(Debug, Default)]
struct Queue {
    pending: VecDeque<Wanted>,
    open: Vec<Wanted>,
    known: HashSet<u64>,
    progress: Progress,
    landings: u64,
}

impl Queue {
    fn finished(&self) -> bool {
        self.pending.is_empty() && self.open.is_empty()
    }

    /// Queues listed entries: a new one adds to the totals, one listed before is a file
    /// macOS evicted after it landed, so it counts as not done again. Input: the entries.
    fn add(&mut self, wanted: Vec<Wanted>) {
        let hasher = BuildHasherDefault::<DefaultHasher>::default();
        for entry in wanted {
            if self.known.insert(hasher.hash_one(&entry.path)) {
                self.progress.total += 1;
                self.progress.bytes_total += entry.bytes;
            } else {
                self.progress.done = self.progress.done.saturating_sub(1);
                self.progress.bytes_done = self.progress.bytes_done.saturating_sub(entry.bytes);
            }
            self.pending.push_back(entry);
        }
        assert_eq!(self.known.len() as u64, self.progress.total);
    }

    /// Reads each open entry once and gives back those that landed, or are gone, to the
    /// queue. Inputs: the read and the seat. Output: the landed folders, to list next, or
    /// the code of a read that failed.
    fn look(&mut self, look: &Look<'_>, seat: &Seat<'_>) -> Result<Vec<Wanted>, EngineError> {
        let mut folders = Vec::new();
        let mut still = Vec::with_capacity(self.open.len());
        let (mut files, mut bytes) = (0usize, 0u64);
        for entry in std::mem::take(&mut self.open) {
            let landed = match look(&entry.path) {
                Ok(metadata) if dataless(&metadata) => false,
                Ok(metadata) => {
                    if metadata.is_dir() && !metadata.file_type().is_symlink() {
                        folders.push(entry.clone());
                    }
                    true
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
                Err(error) => return Err(read_failed(&error)),
            };
            if landed {
                files += 1;
                bytes += entry.bytes;
                self.progress.done += 1;
                self.progress.bytes_done += entry.bytes;
                self.landings += 1;
            } else {
                still.push(entry);
            }
        }
        seat.landed(files, bytes);
        self.open = still;
        assert!(self.progress.done <= self.progress.total);
        Ok(folders)
    }

    /// Opens the next batch the queue allows. Input: the seat. Output: the paths to ask for,
    /// empty while the download waits.
    fn take(&mut self, seat: &Seat<'_>) -> Vec<PathBuf> {
        let sizes: Vec<u64> = self
            .pending
            .iter()
            .take(BATCH_PATHS_MAX)
            .map(|e| e.bytes)
            .collect();
        let count = seat.open(&sizes);
        let batch: Vec<Wanted> = self.pending.drain(..count).collect();
        let paths = batch.iter().map(|entry| entry.path.clone()).collect();
        self.open.extend(batch);
        assert!(self.open.len() <= JOB_OPEN_FILES_MAX);
        paths
    }

    fn largest_open(&self) -> u64 {
        self.open.iter().map(|entry| entry.bytes).max().unwrap_or(0)
    }
}

/// The live row's numbers in the engine log: the seconds from a download's first ask to its
/// first landed entry, then the entries landed per second about once a minute.
#[derive(Debug, Default)]
struct Landings {
    asked: Option<Instant>,
    logged: Option<(Instant, u64)>,
}

impl Landings {
    fn asked(&mut self, now: Instant) {
        self.asked.get_or_insert(now);
    }

    fn note(&mut self, landings: u64, now: Instant) {
        let Some(asked) = self.asked else {
            return;
        };
        match self.logged {
            None if landings > 0 => {
                let secs = now.duration_since(asked).as_secs();
                eprintln!("icloud_first_landing secs={secs}");
                self.logged = Some((now, landings));
            }
            Some((at, counted)) if now.duration_since(at) >= LANDING_LOG_EVERY => {
                let rate = (landings - counted) as f64 / now.duration_since(at).as_secs_f64();
                eprintln!("icloud_landing files_per_sec={rate:.1}");
                self.logged = Some((now, landings));
            }
            _ => {}
        }
    }
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

    /// Holds the stall clock while the download has nothing open, as while it waits its turn.
    /// Input: the time. Output: `TimedOut` past the deadline, else `Wait`.
    fn rest(&mut self, now: Instant) -> Verdict {
        assert!(now >= self.since);
        self.since = now;
        if now >= self.deadline {
            Verdict::TimedOut
        } else {
            Verdict::Wait
        }
    }

    /// Takes one pass's count of landings. Inputs: the count, the logical size of the largest
    /// entry open and the time. Output: the verdict.
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
