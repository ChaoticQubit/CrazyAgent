use crate::file_lock::HeldLock;
use crate::projects_home::CRAZYAGENT_DIRECTORY;
use rustix::fs::{AtFlags, CWD, Dir, FileType, FlockOperation, Mode, OFlags};
use rustix::io::Errno;
use sha2::{Digest, Sha256};
use std::ffi::{CStr, CString};
use std::io;
use std::io::{Read, Write};
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const GRAPH_OUT: &CStr = c"graphify-out";
const GRAPH_FILE: &CStr = c"graph.json";
const PUBLISHED: &CStr = c"graph";
const RECORD: &CStr = c"current";
const RECORD_NEXT: &CStr = c"current.next";
const LOCK: &CStr = c"lock";
const COPY_PREFIX: &[u8] = b"g-";
const RECORD_VERSION: &str = "v2";
const RECORD_BYTES_MAX: u64 = 256;
const DEPTH_MAX: usize = 16;
const ENTRIES_MAX: usize = 200_000;
const STORE_ENTRIES_MAX: usize = 256;
const OLD_LINKS_MAX: usize = 8;
const LOCK_TRIES: u32 = 100;
const LOCK_WAIT: Duration = Duration::from_millis(50);
/// How often the engine looks again at one project's main branch tip for a graph refresh.
pub(crate) const CHECK_EVERY: Duration = Duration::from_secs(30);
/// How long a project whose graph build failed waits before the engine tries it again,
/// whatever its tip.
pub(crate) const RETRY_AFTER: Duration = Duration::from_secs(15 * 60);
const _: () = assert!(RETRY_AFTER.as_secs() > CHECK_EVERY.as_secs());

/// One published build of a project's main graph (DEC-123): the main branch tip it was
/// built at, when, in milliseconds since the epoch, and the digest of the folders it hid
/// ([`hidden_digest`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Build {
    pub(crate) tip: String,
    pub(crate) built_ms: i64,
    pub(crate) hidden: String,
}

impl Build {
    /// The name of the folder this build's copy is published in. Output: `g-<built_ms>`.
    fn folder(&self) -> CString {
        assert!(self.built_ms > 0);
        CString::new(format!("g-{}", self.built_ms)).expect("digits hold no NUL")
    }

    /// The text `current` holds for this build. Output: `v2 <tip> <built_ms> <hidden>` and
    /// a newline.
    fn record(&self) -> String {
        assert!(tip_valid(&self.tip));
        assert!(self.built_ms > 0);
        assert!(digest_valid(&self.hidden));
        format!(
            "{RECORD_VERSION} {} {} {}\n",
            self.tip, self.built_ms, self.hidden
        )
    }

    /// Reads a build back from the text `current` holds, as [`Build::record`] writes it; a
    /// record of an older version reads as none, so its graph is built again. Input: the
    /// text. Output: the build, or None for any other text.
    fn parse(text: &str) -> Option<Self> {
        let mut words = text.strip_suffix('\n')?.split(' ');
        let (version, tip) = (words.next()?, words.next()?);
        let (built, hidden) = (words.next()?, words.next()?);
        if words.next().is_some() || version != RECORD_VERSION || !tip_valid(tip) {
            return None;
        }
        if !digest_valid(hidden) {
            return None;
        }
        let built_ms = built.parse::<i64>().ok().filter(|ms| *ms > 0)?;
        Some(Self {
            tip: tip.to_owned(),
            built_ms,
            hidden: hidden.to_owned(),
        })
    }
}

/// The digest of the folders a project hides from its agents, as one main graph build hid
/// them (DEC-123), so a graph built before the owner hid another folder is due again; their
/// order and repeats do not count. Input: the folders. Output: 64 lower-case hex digits, the
/// SHA-256 of each distinct folder's length and bytes, in byte order.
pub(crate) fn hidden_digest(hidden: &[PathBuf]) -> String {
    let mut folders: Vec<&[u8]> = hidden
        .iter()
        .map(|path| path.as_os_str().as_bytes())
        .collect();
    folders.sort_unstable();
    folders.dedup();
    let mut hasher = Sha256::new();
    for folder in folders {
        hasher.update((folder.len() as u64).to_le_bytes());
        hasher.update(folder);
    }
    let digest = format!("{:x}", hasher.finalize());
    assert!(digest_valid(&digest));
    digest
}

/// Whether a text is a digest as [`hidden_digest`] writes it: 64 lower-case hex digits.
/// Input: the text. Output: the answer.
fn digest_valid(text: &str) -> bool {
    text.len() == 64 && tip_valid(text)
}

/// Whether a commit id is one git writes: 40 or 64 lower-case hex digits. Input: the text.
/// Output: the answer.
pub(crate) fn tip_valid(tip: &str) -> bool {
    matches!(tip.len(), 40 | 64)
        && tip
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// Whether a project's main graph needs a build (DEC-123): none is published yet, or the
/// published one was built at another tip than the main branch holds now, or while the
/// project hid other folders than it hides now. Inputs: the published build, the tip now
/// and the digest of the hidden folders now. Output: the answer.
pub(crate) fn due(published: Option<&Build>, tip: &str, hidden: &str) -> bool {
    assert!(tip_valid(tip));
    assert!(digest_valid(hidden));
    published.is_none_or(|build| build.tip != tip || build.hidden != hidden)
}

/// Whether a project whose last main graph build failed still waits before the next try
/// (DEC-123): for [`RETRY_AFTER`] after any failed or timed-out build, whatever tip the next
/// build would be at, so a build that keeps failing runs at most once per wait however often
/// main moves. Inputs: when the last build failed, None when it did not, and the time now.
/// Output: the answer.
pub(crate) fn retry_waits(failed: Option<Instant>, now: Instant) -> bool {
    failed.is_some_and(|at| now.saturating_duration_since(at) < RETRY_AFTER)
}

/// Whether the engine builds and refreshes main graphs at all (DEC-123): only while the
/// code graph switch is on, and never where no confined graph build runs, Linux in V1
/// (DEC-094); the module is not built on Windows, which runs no graph build. Inputs: the switch as Setup stores it, None while Graphify is not
/// installed, and the system name as `std::env::consts::OS` gives it. Output: the answer.
pub(crate) fn wanted(switch: Option<bool>, os: &str) -> bool {
    switch == Some(true) && super::graph::unavailable(os).is_none()
}

/// Publishes the main graph just built in a project's `graphify-out` for its agents to copy
/// (DEC-123). Keeps `graphify-out/` and `.crazyagent/` out of the project's git first
/// ([`crate::projects_home::exclude_from_git`]), so agents' copies never show as changes.
/// Then clones the graph into `.crazyagent/graph/g-<built_ms>` through a `.part` folder,
/// records the build in `current` by rename and removes every older copy, all under the
/// store's exclusive lock, so no copy reads a graph mid-write. Inputs: the project folder and
/// the build. Output: true once published, false for a project with no real `.crazyagent`
/// folder; an error, also a `current` that does not read back as written, leaves the last
/// published graph or the new one in place, and the caller counts the try as failed.
pub(crate) fn publish(root: &Path, build: &Build) -> io::Result<bool> {
    assert!(tip_valid(&build.tip));
    crate::projects_home::exclude_from_git(root).map_err(|error| io::Error::other(error.0))?;
    let Some(store) = store(root, true)? else {
        return Ok(false);
    };
    let _held = hold(&store, FlockOperation::NonBlockingLockExclusive)?;
    let graph = open_folder(open_folder(CWD, root)?, GRAPH_OUT)?;
    let shown = rustix::fs::statat(&graph, GRAPH_FILE, AtFlags::SYMLINK_NOFOLLOW)?;
    if FileType::from_raw_mode(shown.st_mode) != FileType::RegularFile {
        return Err(io::Error::other("graph_file_not_plain"));
    }
    let folder = build.folder();
    let part = CString::new([folder.as_bytes(), b".part"].concat()).expect("no NUL");
    remove_entry(&store, &part, 0)?;
    fill_new(&store, &part, &graph)?;
    rustix::fs::renameat(&store, &part, &store, &folder)?;
    write_record(&store, build)?;
    prune(&store, &folder)?;
    if read_record(&store).as_ref() != Some(build) {
        return Err(io::Error::other("graph_record_reads_back_different"));
    }
    Ok(true)
}

/// The build a project's agents copy now, read from `current` under the store's shared
/// lock without following a link. Input: the project folder. Output: the build, or None
/// while none is published or it cannot be read.
pub(crate) fn latest(root: &Path) -> Option<Build> {
    let store = store(root, false).ok()??;
    let _held = hold(&store, FlockOperation::NonBlockingLockShared).ok()?;
    read_record(&store)
}

/// Copies a project's published main graph into an agent worktree's own `graphify-out`
/// (DEC-123), file by file as APFS clones on macOS, so the agent may update its copy. Runs
/// only while that folder is missing or holds nothing but the read-only links an older
/// engine placed there into the project's graph (I3), which are removed first; anything else
/// there is the agent's and is left alone. Links in the published graph are never followed
/// nor copied. A copy that fails is removed whole, and a folder something else made there
/// meanwhile is left alone ([`fill_new`]). No copy is made of a graph built while the
/// project hid other folders than it hides now, so a folder hidden since that build never
/// reaches an agent; the next refresh rebuilds it. Inputs: the project folder, the worktree
/// and the folders the project hides now. Output: true once copied, false when nothing was
/// copied.
pub(crate) fn copy_into(root: &Path, worktree: &Path, hidden: &[PathBuf]) -> io::Result<bool> {
    let tree = open_folder(CWD, worktree)?;
    match rustix::fs::statat(&tree, GRAPH_OUT, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(found) if FileType::from_raw_mode(found.st_mode) == FileType::Directory => {
            if !old_links(&tree, root)? {
                return Ok(false);
            }
            remove_entry(&tree, GRAPH_OUT, 0)?;
        }
        Ok(_) => return Ok(false),
        Err(Errno::NOENT) => {}
        Err(error) => return Err(error.into()),
    }
    let Some(store) = store(root, false)? else {
        return Ok(false);
    };
    let _held = hold(&store, FlockOperation::NonBlockingLockShared)?;
    let Some(build) = read_record(&store) else {
        return Ok(false);
    };
    if build.hidden != hidden_digest(hidden) {
        return Ok(false);
    }
    let source = open_folder(&store, build.folder().as_c_str())?;
    fill_new(&tree, GRAPH_OUT, &source)?;
    Ok(true)
}

/// Makes `name` below `parent` as a new folder and copies `source` into it, removing the
/// folder again only when this call made it and the copy then failed. A folder something
/// else made there first, such as an agent process still running from an earlier start,
/// gives the error and is left as it was. Inputs: the parent folder, the name and the
/// source folder. Output: nothing once copied.
fn fill_new(parent: &OwnedFd, name: &CStr, source: &OwnedFd) -> io::Result<()> {
    let target = new_folder(parent, name)?;
    let mut budget = ENTRIES_MAX;
    let copied = copy_tree(source, &target, 0, &mut budget);
    if copied.is_err() {
        let _ = remove_entry(parent, name, 0);
    }
    copied
}

/// Opens one folder below `parent` without following a link. Inputs: the parent folder and
/// the name. Output: the folder's descriptor.
fn open_folder<P: rustix::path::Arg>(parent: impl AsFd, name: P) -> io::Result<OwnedFd> {
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    Ok(rustix::fs::openat(parent, name, flags, Mode::empty())?)
}

/// Makes a new folder below `parent`, never one already there, and opens it. Inputs: the
/// parent folder and the name. Output: the new folder's descriptor.
fn new_folder(parent: &OwnedFd, name: &CStr) -> io::Result<OwnedFd> {
    rustix::fs::mkdirat(parent, name, Mode::from_raw_mode(0o755))?;
    open_folder(parent, name)
}

/// Opens a project's published graph store, `.crazyagent/graph`, making `graph` when `make`
/// is set. Inputs: the project folder and `make`. Output: the store, or None for a project
/// with no real `.crazyagent` folder, or with no store while `make` is unset.
fn store(root: &Path, make: bool) -> io::Result<Option<OwnedFd>> {
    let missing = |error: &io::Error| error.kind() == io::ErrorKind::NotFound;
    let state = match open_folder(open_folder(CWD, root)?, CRAZYAGENT_DIRECTORY) {
        Ok(state) => state,
        Err(error) if missing(&error) => return Ok(None),
        Err(error) => return Err(error),
    };
    if make {
        match rustix::fs::mkdirat(&state, PUBLISHED, Mode::from_raw_mode(0o700)) {
            Ok(()) | Err(Errno::EXIST) => {}
            Err(error) => return Err(error.into()),
        }
    }
    match open_folder(&state, PUBLISHED) {
        Ok(store) => Ok(Some(store)),
        Err(error) if missing(&error) => Ok(None),
        Err(error) => Err(error),
    }
}

/// Takes the store's lock, trying `LOCK_TRIES` times `LOCK_WAIT` apart. Inputs: the store
/// and the non-blocking shared or exclusive operation. Output: the held lock, unlocked when
/// dropped, or `WouldBlock` when another holder kept it.
fn hold(store: &OwnedFd, operation: FlockOperation) -> io::Result<HeldLock> {
    assert!(matches!(
        operation,
        FlockOperation::NonBlockingLockShared | FlockOperation::NonBlockingLockExclusive
    ));
    let flags = OFlags::RDWR | OFlags::CREATE | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let lock = rustix::fs::openat(store, LOCK, flags, Mode::from_raw_mode(0o600))?;
    for _ in 0..LOCK_TRIES {
        match rustix::fs::flock(&lock, operation) {
            Ok(()) => return Ok(HeldLock(std::fs::File::from(lock))),
            Err(Errno::WOULDBLOCK) => std::thread::sleep(LOCK_WAIT),
            Err(error) => return Err(error.into()),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::WouldBlock,
        "graph_store_busy",
    ))
}

/// Copies a folder's regular files and folders into an empty one, never following a link:
/// each file as an APFS clone on macOS where the volume allows, else as a plain copy; links
/// and special files are left out. Inputs: the source and target folders, the depth, at most
/// `DEPTH_MAX`, and the entries still allowed, of `ENTRIES_MAX`. Output: Ok once copied.
fn copy_tree(
    source: &OwnedFd,
    target: &OwnedFd,
    depth: usize,
    budget: &mut usize,
) -> io::Result<()> {
    if depth > DEPTH_MAX {
        return Err(io::Error::other("graph_copy_too_deep"));
    }
    for name in names(source, ENTRIES_MAX)? {
        *budget = budget
            .checked_sub(1)
            .ok_or_else(|| io::Error::other("graph_copy_too_large"))?;
        let found = rustix::fs::statat(source, name.as_c_str(), AtFlags::SYMLINK_NOFOLLOW)?;
        match FileType::from_raw_mode(found.st_mode) {
            FileType::Directory => {
                let from = open_folder(source, name.as_c_str())?;
                let to = new_folder(target, &name)?;
                copy_tree(&from, &to, depth + 1, budget)?;
            }
            FileType::RegularFile => copy_file(source, target, &name)?,
            _ => {}
        }
    }
    Ok(())
}

/// Copies one regular file, opened without following a link and never as a pipe, into a new
/// file of the same name: an APFS clone on macOS, a plain copy of the size it had when opened
/// where no clone can be made. Inputs: the source and target folders and the name. Output:
/// Ok once copied, Ok with nothing written when the entry is no longer a regular file.
fn copy_file(source: &OwnedFd, target: &OwnedFd, name: &CStr) -> io::Result<()> {
    let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
    let from = rustix::fs::openat(source, name, flags, Mode::empty())?;
    let found = rustix::fs::fstat(&from)?;
    if FileType::from_raw_mode(found.st_mode) != FileType::RegularFile {
        return Ok(());
    }
    #[cfg(target_os = "macos")]
    match rustix::fs::fclonefileat(&from, target, name, rustix::fs::CloneFlags::NOFOLLOW) {
        Ok(()) => return Ok(()),
        Err(Errno::NOTSUP | Errno::XDEV) => {}
        Err(error) => return Err(error.into()),
    }
    let size = u64::try_from(found.st_size).map_err(io::Error::other)?;
    let flags = OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let to = rustix::fs::openat(target, name, flags, Mode::from_raw_mode(0o644))?;
    let copied = io::copy(
        &mut std::fs::File::from(from).take(size),
        &mut std::fs::File::from(to),
    )?;
    if copied != size {
        return Err(io::Error::other("graph_file_changed"));
    }
    Ok(())
}

/// Lists a folder's entries but `.` and `..`, at most `limit`. Inputs: the folder and the
/// limit. Output: the names, or `graph_copy_too_large` past the limit.
fn names(folder: &OwnedFd, limit: usize) -> io::Result<Vec<CString>> {
    let mut found = Vec::new();
    for entry in Dir::read_from(folder)? {
        let entry = entry?;
        if matches!(entry.file_name().to_bytes(), b"." | b"..") {
            continue;
        }
        if found.len() == limit {
            return Err(io::Error::other("graph_copy_too_large"));
        }
        found.push(entry.file_name().to_owned());
    }
    assert!(found.len() <= limit);
    Ok(found)
}

/// Removes one entry below `parent` and, for a folder, everything in it, never following a
/// link; a missing entry is left as it is. Inputs: the parent, the name and the depth, at
/// most `DEPTH_MAX`. Output: Ok once removed.
fn remove_entry(parent: &OwnedFd, name: &CStr, depth: usize) -> io::Result<()> {
    if depth > DEPTH_MAX {
        return Err(io::Error::other("graph_copy_too_deep"));
    }
    let found = match rustix::fs::statat(parent, name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(found) => found,
        Err(Errno::NOENT) => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    if FileType::from_raw_mode(found.st_mode) != FileType::Directory {
        return Ok(rustix::fs::unlinkat(parent, name, AtFlags::empty())?);
    }
    let folder = open_folder(parent, name)?;
    for entry in names(&folder, ENTRIES_MAX)? {
        remove_entry(&folder, &entry, depth + 1)?;
    }
    Ok(rustix::fs::unlinkat(parent, name, AtFlags::REMOVEDIR)?)
}

/// Whether a worktree's `graphify-out` holds nothing but links into the project's own
/// `graphify-out`, as an older engine placed them (I3), at most `OLD_LINKS_MAX`. Inputs:
/// the worktree folder and the project folder. Output: the answer; an empty folder counts.
fn old_links(tree: &OwnedFd, root: &Path) -> io::Result<bool> {
    let folder = open_folder(tree, GRAPH_OUT)?;
    let shared = root.join(std::ffi::OsStr::from_bytes(GRAPH_OUT.to_bytes()));
    let Ok(entries) = names(&folder, OLD_LINKS_MAX) else {
        return Ok(false);
    };
    Ok(entries.iter().all(|name| {
        rustix::fs::readlinkat(&folder, name.as_c_str(), Vec::new()).is_ok_and(|target| {
            Path::new(std::ffi::OsStr::from_bytes(target.as_bytes())).starts_with(&shared)
        })
    }))
}

/// Writes `current` for a build through `current.next` and a rename, so a reader sees the
/// old record or the new one whole. Inputs: the store and the build. Output: Ok once renamed.
fn write_record(store: &OwnedFd, build: &Build) -> io::Result<()> {
    remove_entry(store, RECORD_NEXT, 0)?;
    let flags = OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let next = rustix::fs::openat(store, RECORD_NEXT, flags, Mode::from_raw_mode(0o644))?;
    let mut next = std::fs::File::from(next);
    next.write_all(build.record().as_bytes())?;
    next.sync_all()?;
    Ok(rustix::fs::renameat(store, RECORD_NEXT, store, RECORD)?)
}

/// Reads `current` as a plain file without following a link, at most `RECORD_BYTES_MAX`
/// bytes. Input: the store. Output: the build, or None when it is missing or not a record.
fn read_record(store: &OwnedFd) -> Option<Build> {
    let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
    let file = rustix::fs::openat(store, RECORD, flags, Mode::empty()).ok()?;
    let plain = rustix::fs::fstat(&file)
        .is_ok_and(|found| FileType::from_raw_mode(found.st_mode) == FileType::RegularFile);
    let mut text = String::new();
    if !plain
        || std::fs::File::from(file)
            .take(RECORD_BYTES_MAX)
            .read_to_string(&mut text)
            .is_err()
    {
        return None;
    }
    Build::parse(&text)
}

/// Removes every published copy and leftover `.part` folder but the one `current` names.
/// Inputs: the store and the folder kept. Output: Ok once removed.
fn prune(store: &OwnedFd, kept: &CStr) -> io::Result<()> {
    for name in names(store, STORE_ENTRIES_MAX)? {
        if name.as_bytes().starts_with(COPY_PREFIX) && name.as_c_str() != kept {
            remove_entry(store, &name, 0)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
