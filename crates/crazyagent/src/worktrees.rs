use crate::engine::EngineError;
use crate::protocol::{Command, Outcome, StoredProject};
use crate::protocol::{ProjectId, SessionId, TaskId};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
#[cfg(unix)]
use std::fs::OpenOptions;
use std::io::Read;
#[cfg(unix)]
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
#[cfg(unix)]
use std::path::Component;
use std::path::{Path, PathBuf};
use std::process::{Command as Process, Stdio};
use std::time::{Duration, Instant};

const OUTPUT_BYTES_MAX: usize = 8 * 1024 * 1024;
/// The most entries of a repository's .git outside its object store that the metadata walk
/// reads, each one checked. A long-used repository holds thousands in its rerere cache and
/// ref logs, so the bound leaves room for those and still ends the walk on a planted tree.
const METADATA_ENTRIES_MAX: usize = 65_536;
const OBJECT_ENTRIES_MAX: usize = 262_144;
const UNTRUSTED_GIT_ENTRY: &str = "untrusted_git_entry:";
const SHOWN_ENTRY_BYTES_MAX: usize = 160;
const SHARED_WORKTREE_FILES: &[&str] = &["HEAD", "commondir", "gitdir", "locked"];
const HEAD_BYTES_MAX: usize = 512;
#[cfg(unix)]
pub(crate) const TREE_BOUNDS: TreeBounds = TreeBounds {
    files: 262_144,
    bytes: 2 * 1024 * 1024 * 1024,
    components: 64,
};
#[cfg(unix)]
const TREE_LISTING_BYTES_MAX: usize = 64 * 1024 * 1024;
#[cfg(unix)]
const LINK_TARGET_BYTES_MAX: u64 = 1024;
#[cfg(unix)]
const LINKS_LEFT_OUT_MAX: usize = 64;
#[cfg(unix)]
const MATERIALIZE_TIME_MAX: Duration = Duration::from_secs(60);

/// The Git config the engine writes as an agent worktree's `config.worktree`, the last config
/// file Git reads there once `extensions.worktreeConfig` is on: every transport is refused,
/// each built-in one by name, over any rewrite or allow in an owner config file, and a URL no
/// rule rewrites goes to the rule's transport. Not covered: `GIT_ALLOW_PROTOCOL`, `-c` or
/// environment config, and a custom remote helper the owner allows by name.
pub const REMOTE_LOCK: &[u8] = concat!(
    "[protocol]\n\tallow = never\n",
    "[protocol \"file\"]\n\tallow = never\n",
    "[protocol \"git\"]\n\tallow = never\n",
    "[protocol \"ssh\"]\n\tallow = never\n",
    "[protocol \"http\"]\n\tallow = never\n",
    "[protocol \"https\"]\n\tallow = never\n",
    "[protocol \"ftp\"]\n\tallow = never\n",
    "[protocol \"ftps\"]\n\tallow = never\n",
    "[protocol \"ext\"]\n\tallow = never\n",
    "[protocol \"fd\"]\n\tallow = never\n",
    "[url \"agent-worktrees-are-local-only-use-the-project-folder::\"]\n\tinsteadOf =\n",
)
.as_bytes();

/// The lock agent worktrees got before [`REMOTE_LOCK`] named each transport. A worktree
/// holding it still counts as locked, as one made before any lock does.
const PREVIOUS_REMOTE_LOCK: &[u8] = b"[protocol]\n\tallow = never\n[url \"agent-worktrees-are-local-only-use-the-project-folder::\"]\n\tinsteadOf =\n";

/// The line Git prints for a fetch, pull or push tried in an agent worktree.
pub const REMOTE_REFUSAL: &str =
    "fatal: transport 'agent-worktrees-are-local-only-use-the-project-folder' not allowed";

#[cfg(unix)]
const GIT_TRUE: &[&str] = &["true", "yes", "on", "1"];
#[cfg(unix)]
const WORKTREE_CONFIG_TRIES: usize = 3;

/// The largest commit tree a lease copies. Fields: the most tracked files and symlinks,
/// the most content bytes across them, and the most path components in one path.
#[cfg(unix)]
pub(crate) struct TreeBounds {
    pub(crate) files: usize,
    pub(crate) bytes: u64,
    pub(crate) components: usize,
}

/// One tracked file or symlink of the lease commit. Fields: whether it is a file (and
/// executable) or a symlink, its blob id, its size in bytes and its path in the tree.
#[cfg(unix)]
#[derive(Debug)]
struct TreeEntry {
    link: bool,
    executable: bool,
    oid: String,
    size: u64,
    path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum LeaseState {
    Reserved,
    Ready,
    RecoveryRequired,
    Uncertain,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorktreeLease {
    pub project_id: ProjectId,
    pub task_id: TaskId,
    pub repository: String,
    pub path: String,
    pub branch: String,
    pub owner_session: SessionId,
    pub native_descendant: Option<String>,
    pub state: LeaseState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Identity {
    device: u64,
    inode: u64,
}

impl Identity {
    /// Gives the identity as the (device, inode) pair the whole-volume renumber rule
    /// compares. Input: the identity. Output: its pair.
    fn pair(&self) -> (u64, u64) {
        (self.device, self.inode)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Admission {
    repository: String,
    root: Identity,
    metadata: Identity,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LeaseRecord {
    lease: WorktreeLease,
    admission: Admission,
    head: String,
    directory: Option<Identity>,
    metadata: Option<Identity>,
}

fn assert_record_consistent(record: &LeaseRecord) {
    assert_eq!(
        record.lease.repository, record.admission.repository,
        "lease repository must match its admission"
    );
    if matches!(
        record.lease.state,
        LeaseState::Ready | LeaseState::RecoveryRequired
    ) {
        assert!(
            record.directory.is_some(),
            "ready lease requires directory identity"
        );
        assert!(
            record.metadata.is_some(),
            "ready lease requires metadata identity"
        );
    }
}

fn error(value: impl std::fmt::Display) -> EngineError {
    EngineError(value.to_string())
}

fn rejected(code: &str) -> Outcome {
    Outcome::Rejected { code: code.into() }
}

/// Refuses a worktree file identity where the Unix owner, mode, device and inode do not
/// exist. Takes the path and kind, both unused. Returns `worktree_filesystem_unavailable`.
#[cfg(not(unix))]
fn identity(_path: &Path, _directory: bool) -> Result<Identity, EngineError> {
    Err(error("worktree_filesystem_unavailable"))
}

/// Refuses the identity of an entry already read where the Unix owner, mode, device and
/// inode do not exist. Takes the entry and kind, both unused. Returns
/// `worktree_filesystem_unavailable`.
#[cfg(not(unix))]
fn identified(_entry: &std::fs::Metadata, _directory: bool) -> Result<Identity, EngineError> {
    Err(error("worktree_filesystem_unavailable"))
}

#[cfg(unix)]
fn identity(path: &Path, directory: bool) -> Result<Identity, EngineError> {
    identified(&std::fs::symlink_metadata(path).map_err(error)?, directory)
}

/// Checks the identity of an entry already read without following a link, with the rules
/// of [`identity`], so its check and its kind come from one read. Inputs: the entry's
/// metadata and whether it must be a folder. Output: its device and inode, or
/// `untrusted_filesystem_identity`.
#[cfg(unix)]
fn identified(metadata: &std::fs::Metadata, directory: bool) -> Result<Identity, EngineError> {
    if metadata.uid() != rustix::process::getuid().as_raw()
        || metadata.mode() & 0o022 != 0
        || (directory && !metadata.is_dir())
        || (!directory && (!metadata.is_file() || metadata.nlink() != 1))
    {
        return Err(error("untrusted_filesystem_identity"));
    }
    assert!(!metadata.file_type().is_symlink());
    assert_eq!(metadata.uid(), rustix::process::getuid().as_raw());
    Ok(Identity {
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

fn canonical(path: &Path) -> Result<(), EngineError> {
    if !path.is_absolute() || std::fs::canonicalize(path).map_err(error)? != path {
        return Err(error("repository_path_not_canonical"));
    }
    for ancestor in path.ancestors() {
        if std::fs::symlink_metadata(ancestor)
            .map_err(error)?
            .file_type()
            .is_symlink()
        {
            return Err(error("symlink_path_rejected"));
        }
    }
    assert!(path.is_absolute());
    Ok(())
}

/// Refuses a bounded worktree file read where no no-follow open is used here.
/// Takes the path and limit, both unused. Returns `worktree_filesystem_unavailable`.
#[cfg(not(unix))]
fn bounded_read(_path: &Path, _limit: usize) -> Result<Vec<u8>, EngineError> {
    Err(error("worktree_filesystem_unavailable"))
}

/// Reads a small file an agent may have replaced: opened without following a link and
/// without waiting on a FIFO or device, kept only when it is a regular file, at most
/// `limit` bytes. Inputs: the path and the limit. Output: the bytes, or the refusal.
#[cfg(unix)]
fn bounded_read(path: &Path, limit: usize) -> Result<Vec<u8>, EngineError> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags((rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK).bits() as i32)
        .open(path)
        .map_err(error)?;
    if !file.metadata().map_err(error)?.is_file() {
        return Err(error("expected_regular_file"));
    }
    let mut bytes = Vec::new();
    file.take((limit + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(error)?;
    if bytes.len() > limit {
        return Err(error("file_capacity"));
    }
    assert!(bytes.len() <= limit);
    Ok(bytes)
}

fn git(repository: &Path, args: &[&str]) -> Result<Vec<u8>, EngineError> {
    git_until(
        repository,
        args,
        OUTPUT_BYTES_MAX,
        Instant::now() + Duration::from_secs(5),
    )
}

/// Runs one engine Git command and collects its output. Inputs: the repository, the
/// arguments, the most output bytes accepted and the instant the command is killed.
/// Output: the standard output, or an error naming the deadline, capacity or failure.
fn git_until(
    repository: &Path,
    args: &[&str],
    output_max: usize,
    deadline: Instant,
) -> Result<Vec<u8>, EngineError> {
    let child = crate::spawn_gate::spawn(&mut git_command(repository, args)).map_err(error)?;
    collect_git_output(child, output_max, deadline)
}

/// Builds the engine's Git process: the host Git with a cleared environment, no system
/// or global config, hooks, fsmonitor, replace objects, grafts, commit-graph files,
/// protocols or automatic maintenance. Inputs: the repository and the arguments. Output:
/// the unspawned process.
fn git_command(repository: &Path, args: &[&str]) -> Process {
    let mut command = Process::new("/usr/bin/git");
    command
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_NO_LAZY_FETCH", "1")
        .env("GIT_ATTR_NOSYSTEM", "1")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_GRAFT_FILE", "/dev/null")
        .arg("--no-replace-objects")
        .arg("--no-optional-locks")
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "core.fsmonitor=false",
            "-c",
            "core.pager=cat",
            "-c",
            "core.attributesFile=/dev/null",
            "-c",
            "submodule.recurse=false",
            "-c",
            "protocol.allow=never",
            "-c",
            "gc.auto=0",
            "-c",
            "maintenance.auto=false",
            "-c",
            "core.commitGraph=false",
            "-c",
            "advice.graftFileDeprecated=false",
        ])
        .arg("--git-dir")
        .arg(repository.join(".git"))
        .arg("--work-tree")
        .arg(repository)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    command
}

fn collect_git_output(
    mut child: std::process::Child,
    output_max: usize,
    deadline: Instant,
) -> Result<Vec<u8>, EngineError> {
    let output = child
        .stdout
        .take()
        .ok_or_else(|| error("git_stdout_missing"))?;
    let reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        output
            .take((output_max + 1) as u64)
            .read_to_end(&mut bytes)?;
        Ok::<_, std::io::Error>(bytes)
    });
    let status = loop {
        if let Some(status) = child.try_wait().map_err(error)? {
            break status;
        }
        if Instant::now() >= deadline {
            child.kill().map_err(error)?;
            child.wait().map_err(error)?;
            let _ = reader.join();
            return Err(error("git_deadline"));
        }
        std::thread::sleep(Duration::from_millis(2));
    };
    let bytes = reader
        .join()
        .map_err(|_| error("git_reader_failed"))?
        .map_err(error)?;
    if !status.success() || bytes.len() > output_max {
        return Err(error("git_operation_failed_or_output_capacity"));
    }
    assert!(bytes.len() <= output_max);
    assert!(status.success());
    Ok(bytes)
}

/// Checks a repository's .git for entries the engine's Git could be led through, then
/// refuses shared, partial and grafted repositories. Each lease's own admin folder is
/// checked with its lease by [`validate_admin`]. Input: the repository. Output: nothing,
/// or a code naming the first offending entry, the capacity reached or the refused file.
pub(crate) fn validate_metadata(repository: &Path) -> Result<(), EngineError> {
    let metadata = repository.join(".git");
    walk_metadata(&metadata, &metadata)?;
    for name in [
        "commondir",
        "objects/info/alternates",
        "objects/info/http-alternates",
        "shallow",
        "info/grafts",
        "config.worktree",
    ] {
        if std::fs::symlink_metadata(metadata.join(name)).is_ok() {
            return Err(error("unsupported_shared_or_partial_git_metadata"));
        }
    }
    Ok(())
}

/// Checks one lease's own admin folder in .git/worktrees with the rules of the rest of
/// .git, so an entry planted there marks only that lease. Inputs: the repository and the
/// admin folder. Output: nothing, or a code naming the offending entry or the capacity.
pub(crate) fn validate_admin(repository: &Path, admin: &Path) -> Result<(), EngineError> {
    let metadata = repository.join(".git");
    if admin.parent() != Some(metadata.join("worktrees").as_path()) {
        return Err(error("worktree_metadata_escape"));
    }
    walk_metadata(&metadata, admin)
}

/// Walks `start` inside the .git folder `metadata`. Every folder and file must pass
/// [`identity`]. In the object store, which confined agents write, only `info`, `pack`,
/// the fan-out folders and the object files Git reads are held to that, and of another
/// worktree's admin folder only the files every Git command reads. Inputs: the .git
/// folder and where to start. Output: nothing, or the refusal or capacity code.
fn walk_metadata(metadata: &Path, start: &Path) -> Result<(), EngineError> {
    let objects = metadata.join("objects");
    let worktrees = metadata.join("worktrees");
    let mut pending = vec![start.to_path_buf()];
    let mut visited = 0;
    let mut stored = 0;
    while let Some(path) = pending.pop() {
        visited += 1;
        if visited > METADATA_ENTRIES_MAX {
            return Err(error("git_metadata_capacity"));
        }
        let root = path == start;
        let found = if root {
            Some(trusted(metadata, &path)?)
        } else {
            listed(metadata, &path)?
        };
        if !found.is_some_and(|entry| entry.is_dir()) {
            continue;
        }
        let entries = if root {
            std::fs::read_dir(&path).map_err(error)?
        } else if let Some(entries) = listing(&path)? {
            entries
        } else {
            continue;
        };
        for entry in entries {
            let child = entry.map_err(error)?.path();
            let name = child.file_name().unwrap_or_default();
            if path == worktrees {
                visited += 1;
                if visited > METADATA_ENTRIES_MAX {
                    return Err(error("git_metadata_capacity"));
                }
                shared_worktree_files(metadata, &child)?;
            } else if path == objects && name.len() == 2 && hex(name) {
                stored += fan_out(metadata, &child, OBJECT_ENTRIES_MAX - stored)?;
            } else if path == objects
                && !name.eq_ignore_ascii_case("info")
                && !name.eq_ignore_ascii_case("pack")
            {
                stored += 1;
                if stored > OBJECT_ENTRIES_MAX {
                    return Err(error("git_object_store_capacity"));
                }
            } else {
                pending.push(child);
                if pending.len() > METADATA_ENTRIES_MAX {
                    return Err(error("git_metadata_capacity"));
                }
            }
        }
    }
    assert!(visited > 0);
    assert!(visited <= METADATA_ENTRIES_MAX);
    assert!(stored <= OBJECT_ENTRIES_MAX);
    Ok(())
}

/// Reads the entry a walk starts from without following a link and requires [`identity`]
/// of it from that one read. Inputs: the .git folder and the entry. Output: its metadata,
/// the read failure when it is missing, or `untrusted_git_entry:` with the entry's path
/// inside .git.
fn trusted(metadata: &Path, path: &Path) -> Result<std::fs::Metadata, EngineError> {
    let entry = std::fs::symlink_metadata(path).map_err(error)?;
    identified(&entry, entry.is_dir()).map_err(|_| untrusted(metadata, path))?;
    Ok(entry)
}

#[cfg(test)]
thread_local! {
    /// What a test does to a listed .git entry just before the walk reads it, given the
    /// entry and whether the read lists it as a folder. None leaves every entry alone.
    static BEFORE_READ: std::cell::RefCell<Option<Box<dyn Fn(&Path, bool)>>> =
        const { std::cell::RefCell::new(None) };
}

/// Runs the step a test set in `BEFORE_READ` on this thread. Inputs: the listed entry and
/// whether the coming read lists it as a folder. Output: none.
#[cfg(test)]
fn before_read(path: &Path, listing: bool) {
    BEFORE_READ.with(|step| {
        if let Some(step) = step.borrow().as_ref() {
            step(path, listing);
        }
    });
}

/// Does nothing outside tests, where no step stands between a listing and a read. Inputs:
/// the listed entry and the kind of read, both unused. Output: none.
#[cfg(not(test))]
fn before_read(_path: &Path, _listing: bool) {}

/// Reads one entry a .git folder listed, without following a link, and requires
/// [`identity`] of it from that one read. Git removes its lock files, temporary packs and
/// pruned objects at any moment, so an entry gone since its listing is as if never listed;
/// every other read failure and every refusal stays. Inputs: the .git folder and the
/// entry. Output: its metadata, None when it is gone, or `untrusted_git_entry:` with the
/// entry's path inside .git.
fn listed(metadata: &Path, path: &Path) -> Result<Option<std::fs::Metadata>, EngineError> {
    before_read(path, false);
    match std::fs::symlink_metadata(path) {
        Ok(entry) => {
            identified(&entry, entry.is_dir()).map_err(|_| untrusted(metadata, path))?;
            Ok(Some(entry))
        }
        Err(gone) if gone.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(failed) => Err(error(failed)),
    }
}

/// Lists one folder that [`listed`] found inside .git. Git removes emptied folders at any
/// moment, so a folder gone since that read is as if never listed; every other failure
/// stays. Input: the folder. Output: its entries, None when it is gone, or the failure.
fn listing(folder: &Path) -> Result<Option<std::fs::ReadDir>, EngineError> {
    before_read(folder, true);
    match std::fs::read_dir(folder) {
        Ok(entries) => Ok(Some(entries)),
        Err(gone) if gone.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(failed) => Err(error(failed)),
    }
}

/// Checks one fan-out folder of the object store and every object file in it, since Git
/// reads an object through any link put in its place. Names that are not hex digits, such
/// as a temporary object left by a killed Git, are never read as objects and are left
/// alone, and a folder or an object Git pruned since its listing counts as read and is no
/// finding. Inputs: the .git folder, the fan-out folder and how many entries may still be
/// read. Output: the entries read, or the refusal or `git_object_store_capacity` code.
fn fan_out(metadata: &Path, folder: &Path, room: usize) -> Result<usize, EngineError> {
    if room == 0 {
        return Err(error("git_object_store_capacity"));
    }
    let Some(found) = listed(metadata, folder)? else {
        return Ok(1);
    };
    if !found.is_dir() {
        return Err(untrusted(metadata, folder));
    }
    let Some(entries) = listing(folder)? else {
        return Ok(1);
    };
    let mut read = 1;
    for entry in entries {
        let path = entry.map_err(error)?.path();
        read += 1;
        if read > room {
            return Err(error("git_object_store_capacity"));
        }
        if path.file_name().is_some_and(hex)
            && listed(metadata, &path)?.is_some_and(|object| !object.is_file())
        {
            return Err(untrusted(metadata, &path));
        }
    }
    assert!(read <= room);
    Ok(read)
}

/// Checks the files of another worktree's admin folder that Git reads for every worktree,
/// as `git worktree add` does: each must be a plain file when present, so no read follows
/// a link or waits on a pipe, and HEAD must pass [`plain_head`], so Git resolves no ref of
/// that folder. The rest of that folder is checked with its own lease. An admin folder Git
/// removed since its listing is no finding.
/// Inputs: the .git folder and the admin folder. Output: nothing, or the refusal code.
fn shared_worktree_files(metadata: &Path, admin: &Path) -> Result<(), EngineError> {
    before_read(admin, false);
    let folder = match std::fs::symlink_metadata(admin) {
        Ok(folder) => folder,
        Err(gone) if gone.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(failed) => return Err(error(failed)),
    };
    if folder.file_type().is_symlink() {
        return Err(untrusted(metadata, admin));
    }
    if !folder.is_dir() {
        return Ok(());
    }
    for name in SHARED_WORKTREE_FILES {
        let path = admin.join(name);
        match std::fs::symlink_metadata(&path) {
            Err(failed) if failed.kind() == std::io::ErrorKind::NotFound => {}
            Ok(entry) if entry.is_file() && (*name != "HEAD" || plain_head(&path)) => {}
            _ => return Err(untrusted(metadata, &path)),
        }
    }
    Ok(())
}

/// Whether a worktree's HEAD names only what Git resolves outside that worktree's own
/// folder: a detached object id, or a branch in `refs/heads/`, whose ref the walk of the
/// shared refs checks. Per-worktree refs such as `refs/worktree/` or `refs/bisect/` and
/// pseudo refs are refused. It is read with [`bounded_read`], so a FIFO swapped in is
/// never waited on. Input: the HEAD file. Output: the answer.
fn plain_head(path: &Path) -> bool {
    let Ok(bytes) = bounded_read(path, HEAD_BYTES_MAX) else {
        return false;
    };
    let Ok(text) = std::str::from_utf8(&bytes) else {
        return false;
    };
    let line = text.strip_suffix('\n').unwrap_or(text);
    match line.strip_prefix("ref: refs/heads/") {
        Some(branch) => branch_name(branch),
        None => {
            matches!(line.len(), 40 | 64)
                && line
                    .bytes()
                    .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
        }
    }
}

/// Whether text is a branch name Git accepts after `refs/heads/`, by the rules of
/// `git check-ref-format`: no control character, space or any of `~^:?*[\`, no `..` or
/// `@{`, not `@`, no empty component or one starting with `.` or ending in `.lock`, and no
/// trailing `.`. Input: the name. Output: the answer.
fn branch_name(name: &str) -> bool {
    name != "@"
        && !name.ends_with('.')
        && !name.contains("..")
        && !name.contains("@{")
        && !name
            .bytes()
            .any(|byte| byte < 0x20 || byte == 0x7f || b" ~^:?*[\\".contains(&byte))
        && name
            .split('/')
            .all(|part| !part.is_empty() && !part.starts_with('.') && !part.ends_with(".lock"))
}

/// Whether a file name is made only of hex digits, in either case, since a folder that
/// ignores case resolves Git's lowercase name to it. Input: the name. Output: the answer.
fn hex(name: &std::ffi::OsStr) -> bool {
    !name.is_empty() && name.as_encoded_bytes().iter().all(u8::is_ascii_hexdigit)
}

/// Builds the refusal that names an offending .git entry for the owner, shown as its path
/// inside .git with anything outside letters, digits, `.`, `_`, `-` and `/` replaced and
/// the length capped, since an agent chooses the name. Inputs: the .git folder and the
/// entry. Output: the `untrusted_git_entry:` error.
fn untrusted(metadata: &Path, path: &Path) -> EngineError {
    let inside = path
        .strip_prefix(metadata)
        .unwrap_or(path)
        .to_string_lossy();
    let shown = inside
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '/') {
                c
            } else {
                '?'
            }
        })
        .take(SHOWN_ENTRY_BYTES_MAX)
        .collect::<String>();
    assert!(shown.len() <= SHOWN_ENTRY_BYTES_MAX);
    error(format!("{UNTRUSTED_GIT_ENTRY}{shown}"))
}

const CONFIG_REFUSED: &str = "repository_config_refused:";
const GIT_BOOLEANS: &[&str] = &["true", "false", "yes", "no", "on", "off", "1", "0"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scope {
    Plain,
    Named,
    Either,
}

/// One repository-local Git config rule: the keys it covers (a section, whether the key
/// has a subsection, and its variable names, none meaning all), the values that keep a
/// covered key inert (none meaning every value is refused), and how the refusal reads.
struct RefusedKey {
    section: &'static str,
    scope: Scope,
    names: &'static [&'static str],
    inert: &'static [&'static str],
    code: &'static str,
    key: &'static str,
    why: &'static str,
}

const fn refused(
    section: &'static str,
    scope: Scope,
    names: &'static [&'static str],
    code: &'static str,
    key: &'static str,
    why: &'static str,
) -> RefusedKey {
    RefusedKey {
        section,
        scope,
        names,
        inert: &[],
        code,
        key,
        why,
    }
}

const RUNS: &str = "runs a command the repository chooses";

const REFUSED_KEYS: [RefusedKey; 41] = [
    RefusedKey {
        inert: &["0", "1"],
        ..refused(
            "core",
            Scope::Plain,
            &["repositoryformatversion"],
            "repository_format",
            "core.repositoryFormatVersion",
            "uses a repository format the engine does not read",
        )
    },
    RefusedKey {
        inert: &["false", "no", "off", "0"],
        ..refused(
            "core",
            Scope::Plain,
            &["bare"],
            "core_bare",
            "core.bare",
            "marks the repository as having no working tree",
        )
    },
    RefusedKey {
        inert: GIT_BOOLEANS,
        ..refused(
            "extensions",
            Scope::Plain,
            &["worktreeconfig"],
            "extensions_worktreeconfig",
            "extensions.worktreeConfig",
            "holds a value Git does not read as true or false",
        )
    },
    refused(
        "extensions",
        Scope::Either,
        &[],
        "repository_extension",
        "extensions.*",
        "changes the repository format the engine reads",
    ),
    refused(
        "core",
        Scope::Plain,
        &["hookspath"],
        "core_hookspath",
        "core.hooksPath",
        "runs hooks from a folder the repository chooses",
    ),
    refused(
        "hook",
        Scope::Named,
        &[],
        "hook_command",
        "hook.<name>.command",
        "runs a hook command written in the config",
    ),
    refused(
        "core",
        Scope::Plain,
        &["fsmonitor"],
        "core_fsmonitor",
        "core.fsmonitor",
        RUNS,
    ),
    refused(
        "core",
        Scope::Plain,
        &["sshcommand"],
        "core_sshcommand",
        "core.sshCommand",
        RUNS,
    ),
    refused(
        "core",
        Scope::Plain,
        &["gitproxy"],
        "core_gitproxy",
        "core.gitProxy",
        RUNS,
    ),
    refused(
        "core",
        Scope::Plain,
        &["editor"],
        "core_editor",
        "core.editor",
        RUNS,
    ),
    refused(
        "core",
        Scope::Plain,
        &["pager"],
        "core_pager",
        "core.pager",
        RUNS,
    ),
    refused(
        "core",
        Scope::Plain,
        &["askpass"],
        "core_askpass",
        "core.askPass",
        RUNS,
    ),
    refused(
        "core",
        Scope::Plain,
        &["alternaterefscommand"],
        "core_alternaterefscommand",
        "core.alternateRefsCommand",
        RUNS,
    ),
    refused(
        "core",
        Scope::Plain,
        &["worktree"],
        "core_worktree",
        "core.worktree",
        "moves the working tree Git uses",
    ),
    refused(
        "core",
        Scope::Plain,
        &["attributesfile"],
        "core_attributesfile",
        "core.attributesFile",
        "reads attributes, which pick filters and drivers, from another file",
    ),
    refused(
        "filter",
        Scope::Named,
        &[],
        "filter_driver",
        "filter.<driver>",
        "runs a filter command when files are checked out or added",
    ),
    refused(
        "diff",
        Scope::Named,
        &["command", "textconv"],
        "diff_driver",
        "diff.<driver>.command or textconv",
        RUNS,
    ),
    refused(
        "diff",
        Scope::Plain,
        &["external"],
        "diff_external",
        "diff.external",
        RUNS,
    ),
    refused(
        "difftool",
        Scope::Either,
        &[],
        "difftool",
        "difftool.*",
        RUNS,
    ),
    refused(
        "merge",
        Scope::Named,
        &["driver"],
        "merge_driver",
        "merge.<driver>.driver",
        RUNS,
    ),
    refused(
        "mergetool",
        Scope::Either,
        &[],
        "mergetool",
        "mergetool.*",
        RUNS,
    ),
    refused(
        "sequence",
        Scope::Plain,
        &["editor"],
        "sequence_editor",
        "sequence.editor",
        RUNS,
    ),
    refused(
        "credential",
        Scope::Either,
        &[],
        "credential",
        "credential.*",
        "runs a credential helper",
    ),
    refused(
        "gpg",
        Scope::Either,
        &["program", "defaultkeycommand"],
        "gpg_program",
        "gpg.program or gpg.ssh.defaultKeyCommand",
        RUNS,
    ),
    refused(
        "pager",
        Scope::Plain,
        &[],
        "pager_command",
        "pager.<command>",
        RUNS,
    ),
    refused(
        "trailer",
        Scope::Named,
        &["cmd", "command"],
        "trailer_command",
        "trailer.<key>.command",
        RUNS,
    ),
    refused(
        "interactive",
        Scope::Plain,
        &["difffilter"],
        "interactive_difffilter",
        "interactive.diffFilter",
        RUNS,
    ),
    refused(
        "submodule",
        Scope::Named,
        &["update"],
        "submodule_update",
        "submodule.<name>.update",
        RUNS,
    ),
    refused(
        "url",
        Scope::Named,
        &["insteadof", "pushinsteadof"],
        "url_rewrite",
        "url.<base>.insteadOf",
        "rewrites the addresses Git fetches from or pushes to",
    ),
    refused(
        "include",
        Scope::Plain,
        &["path"],
        "include",
        "include.path",
        "loads more config from another file",
    ),
    refused(
        "includeif",
        Scope::Named,
        &["path"],
        "include_if",
        "includeIf.<condition>.path",
        "loads more config from another file",
    ),
    refused(
        "remote",
        Scope::Named,
        &["uploadpack", "receivepack", "vcs", "proxy"],
        "remote_command",
        "remote.<name>.uploadpack, receivepack, vcs or proxy",
        "runs a command or proxy for the remote",
    ),
    refused(
        "http",
        Scope::Either,
        &["proxy"],
        "http_proxy",
        "http.proxy",
        "sends Git traffic through a proxy",
    ),
    refused(
        "protocol",
        Scope::Either,
        &["allow"],
        "protocol_allow",
        "protocol.allow",
        "changes which transports, command transports included, Git may use",
    ),
    refused(
        "uploadpack",
        Scope::Plain,
        &["packobjectshook"],
        "uploadpack_hook",
        "uploadpack.packObjectsHook",
        RUNS,
    ),
    refused(
        "gc",
        Scope::Plain,
        &["recentobjectshook"],
        "gc_hook",
        "gc.recentObjectsHook",
        RUNS,
    ),
    refused(
        "sendemail",
        Scope::Either,
        &["smtpserver", "sendmailcmd", "tocmd", "cccmd", "headercmd"],
        "sendemail_command",
        "sendemail.sendmailCmd",
        RUNS,
    ),
    refused(
        "imap",
        Scope::Plain,
        &["tunnel"],
        "imap_tunnel",
        "imap.tunnel",
        RUNS,
    ),
    refused(
        "browser",
        Scope::Named,
        &["cmd", "path"],
        "browser_command",
        "browser.<tool>.cmd",
        RUNS,
    ),
    refused(
        "man",
        Scope::Named,
        &["cmd", "path"],
        "man_command",
        "man.<tool>.cmd",
        RUNS,
    ),
    refused(
        "guitool",
        Scope::Named,
        &[],
        "guitool_command",
        "guitool.<name>.cmd",
        RUNS,
    ),
];

/// Finds the rule that refuses one repository-local Git config entry. Input: the key as
/// `git config --list` prints it (section and variable lowercased, subsection as written)
/// and its value. Output: the refusing rule, or None for inert metadata such as branch
/// tracking, remotes, user identity, aliases and keys other tools store (VS Code, the
/// Codex app, GitHub CLI); the first rule that covers a key decides it.
fn refused_key(key: &str, value: &str) -> Option<&'static RefusedKey> {
    let (section, rest) = key.split_once('.').unwrap_or((key, ""));
    let (named, name) = match rest.rsplit_once('.') {
        Some((_, name)) => (true, name),
        None => (false, rest),
    };
    let rule = REFUSED_KEYS.iter().find(|rule| {
        rule.section == section
            && match rule.scope {
                Scope::Plain => !named,
                Scope::Named => named,
                Scope::Either => true,
            }
            && (rule.names.is_empty() || rule.names.contains(&name))
    })?;
    if rule
        .inert
        .iter()
        .any(|inert| inert.eq_ignore_ascii_case(value))
    {
        return None;
    }
    Some(rule)
}

/// Writes the plain line an owner reads when a repository's config was refused. Inputs:
/// the refusal code and the repository path. Output: the line naming the repository, the
/// key and why, or None when the code is no config refusal.
pub(crate) fn refused_config_line(code: &str, repository: &str) -> Option<String> {
    let code = code.strip_prefix(CONFIG_REFUSED)?;
    let rule = REFUSED_KEYS.iter().find(|rule| rule.code == code)?;
    Some(format!(
        "{repository} has the Git setting {}, which {}. Remove it from the repository's \
         .git/config, then send again.",
        rule.key, rule.why
    ))
}

/// Checks every entry of a repository's own Git config against [`REFUSED_KEYS`]. Input: the
/// repository. Output: nothing, or `repository_config_refused:` with the refusing rule's
/// code. The listing is bounded by the engine Git output bound, [`OUTPUT_BYTES_MAX`], so a
/// repository with thousands of tracked branches is read whole and a longer one fails as
/// that command does.
fn validate_config(repository: &Path) -> Result<(), EngineError> {
    let bytes = git(
        repository,
        &["config", "--local", "--no-includes", "--null", "--list"],
    )?;
    assert!(bytes.len() <= OUTPUT_BYTES_MAX);
    for record in bytes.split(|b| *b == 0).filter(|r| !r.is_empty()) {
        let record = std::str::from_utf8(record).map_err(error)?;
        let (key, value) = record
            .split_once('\n')
            .ok_or_else(|| error("invalid_git_config"))?;
        if !key.contains('.') {
            return Err(error("invalid_git_config"));
        }
        if let Some(rule) = refused_key(key, value) {
            return Err(error(format!("{CONFIG_REFUSED}{}", rule.code)));
        }
    }
    Ok(())
}

fn inspect(repository: &str) -> Result<Admission, EngineError> {
    let path = Path::new(repository);
    canonical(path)?;
    let root = identity(path, true)?;
    let metadata = identity(&path.join(".git"), true)?;
    validate_metadata(path)?;
    validate_config(path)?;
    let actual = git(path, &["rev-parse", "--show-toplevel"])?;
    if actual.strip_suffix(b"\n") != Some(repository.as_bytes()) {
        return Err(error("repository_root_mismatch"));
    }
    if identity(path, true)? != root || identity(&path.join(".git"), true)? != metadata {
        return Err(error("repository_changed_during_inspection"));
    }
    Ok(Admission {
        repository: repository.into(),
        root,
        metadata,
    })
}

/// Checks that an admitted repository still holds the admitted root and .git folders,
/// accepting a whole-volume device renumber with every inode exact. Input: the stored
/// admission, never rewritten. Output: the live admission, or
/// `admitted_repository_replaced`.
fn verify(admission: &Admission) -> Result<Admission, EngineError> {
    let current = inspect(&admission.repository)?;
    if !crate::file_identity::same_files(
        &[admission.root.pair(), admission.metadata.pair()],
        &[current.root.pair(), current.metadata.pair()],
    ) {
        return Err(error("admitted_repository_replaced"));
    }
    assert_eq!(current.repository, admission.repository);
    Ok(current)
}

#[cfg(unix)]
pub(crate) fn initialize(connection: &Connection) -> Result<(), EngineError> {
    connection
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS repository_admissions
        (project TEXT NOT NULL, repository TEXT NOT NULL, body TEXT NOT NULL,
         PRIMARY KEY(project,repository));
        CREATE TABLE IF NOT EXISTS worktree_leases
        (task TEXT NOT NULL, repository TEXT NOT NULL, path TEXT NOT NULL UNIQUE,
         body TEXT NOT NULL, PRIMARY KEY(task,repository));",
        )
        .map_err(error)
}

pub(crate) fn admit(
    connection: &Connection,
    project: &ProjectId,
    repository: &str,
) -> Result<Outcome, EngineError> {
    let body: Option<String> = connection
        .query_row(
            "SELECT body FROM projects WHERE id=?1",
            [project.as_str()],
            |row| row.get(0),
        )
        .optional()
        .map_err(error)?;
    let Some(body) = body else {
        return Ok(rejected("project_missing"));
    };
    let stored: StoredProject = serde_json::from_str(&body).map_err(error)?;
    if !stored.spec.repositories.iter().any(|p| p == repository) {
        return Ok(rejected("repository_not_in_project"));
    }
    let admission = match inspect(repository) {
        Ok(admission) => admission,
        Err(e) => return Ok(rejected(&e.0)),
    };
    let count = connection
        .execute(
            "INSERT OR IGNORE INTO repository_admissions VALUES (?1,?2,?3)",
            params![
                project.as_str(),
                repository,
                serde_json::to_string(&admission).map_err(error)?
            ],
        )
        .map_err(error)?;
    if count == 0 {
        return Ok(rejected("repository_already_admitted"));
    }
    assert_eq!(count, 1);
    assert!(!connection.is_autocommit());
    Ok(Outcome::Applied)
}

pub(crate) fn reserve(
    connection: &Connection,
    runtime: &Path,
    command: &Command,
) -> Result<Outcome, EngineError> {
    let Command::AllocateWorktree {
        task_id,
        repository,
        owner_session,
        native_descendant,
    } = command
    else {
        unreachable!()
    };
    let body: Option<String> = connection
        .query_row(
            "SELECT body FROM tasks WHERE id=?1",
            [task_id.as_str()],
            |row| row.get(0),
        )
        .optional()
        .map_err(error)?;
    let Some(body) = body else {
        return Ok(rejected("task_missing"));
    };
    let context: crate::protocol::ExecutionContext = serde_json::from_str(&body).map_err(error)?;
    let body: Option<String> = connection
        .query_row(
            "SELECT body FROM repository_admissions WHERE project=?1 AND repository=?2",
            params![context.project_id.as_str(), repository],
            |row| row.get(0),
        )
        .optional()
        .map_err(error)?;
    let Some(body) = body else {
        return Ok(rejected("repository_not_admitted"));
    };
    if load(connection, task_id, repository)?.is_some() {
        return Ok(rejected("writer_already_owned"));
    }
    let admission: Admission = serde_json::from_str(&body).map_err(error)?;
    let record = match prepare(
        runtime,
        admission,
        context.project_id,
        task_id.clone(),
        owner_session.clone(),
        native_descendant.clone(),
    ) {
        Ok(record) => record,
        Err(e) => return Ok(rejected(&e.0)),
    };
    persist_reservation(connection, &record)
}

pub(crate) fn reserve_master_bootstrap(
    connection: &Connection,
    runtime: &Path,
    project: &ProjectId,
    task: &TaskId,
    owner_session: &SessionId,
    repository: &str,
) -> Result<Outcome, EngineError> {
    assert!(!connection.is_autocommit());
    if crate::projects_home::project_move_holds(connection, std::path::Path::new(repository))
        .map_err(error)?
    {
        return Ok(rejected("move_project_in_progress"));
    }
    let body: Option<String> = connection
        .query_row(
            "SELECT body FROM repository_admissions WHERE project=?1 AND repository=?2",
            params![project.as_str(), repository],
            |row| row.get(0),
        )
        .optional()
        .map_err(error)?;
    let Some(body) = body else {
        return Ok(rejected("repository_not_admitted"));
    };
    if load(connection, task, repository)?.is_some() {
        return Ok(rejected("writer_already_owned"));
    }
    let admission: Admission = serde_json::from_str(&body).map_err(error)?;
    let record = match prepare(
        runtime,
        admission,
        project.clone(),
        task.clone(),
        owner_session.clone(),
        None,
    ) {
        Ok(record) => record,
        Err(error) => return Ok(rejected(&error.0)),
    };
    persist_reservation(connection, &record)
}

fn persist_reservation(
    connection: &Connection,
    record: &LeaseRecord,
) -> Result<Outcome, EngineError> {
    assert_record_consistent(record);
    let count: i64 = connection
        .query_row("SELECT COUNT(*) FROM worktree_leases", [], |r| r.get(0))
        .map_err(error)?;
    if count >= 128 {
        return Ok(rejected("lease_capacity"));
    }
    connection
        .execute(
            "INSERT INTO worktree_leases VALUES (?1,?2,?3,?4)",
            params![
                record.lease.task_id.as_str(),
                record.lease.repository,
                record.lease.path,
                serde_json::to_string(&record).map_err(error)?
            ],
        )
        .map_err(error)?;
    leases(connection, &record.lease.task_id)?;
    assert!(!connection.is_autocommit());
    assert_eq!(record.lease.state, LeaseState::Reserved);
    Ok(Outcome::Applied)
}

fn prepare(
    runtime: &Path,
    admission: Admission,
    project_id: ProjectId,
    task_id: TaskId,
    owner_session: SessionId,
    native_descendant: Option<String>,
) -> Result<LeaseRecord, EngineError> {
    let admission = verify(&admission)?;
    let root = private_worktree_root(runtime)?;
    let key = format!("{}\0{}", admission.repository, task_id.as_str());
    let digest = format!("{:x}", Sha256::digest(key.as_bytes()));
    let path = root.join(&digest);
    let branch = format!("crazyagent/{digest}");
    if std::fs::symlink_metadata(&path).is_ok() {
        return Err(error("worktree_path_exists"));
    }
    let refs = git(
        Path::new(&admission.repository),
        &[
            "for-each-ref",
            "--format=%(refname)",
            &format!("refs/heads/{branch}"),
        ],
    )?;
    if !refs.is_empty() {
        return Err(error("worktree_branch_exists"));
    }
    let head = git(
        Path::new(&admission.repository),
        &["rev-parse", "--verify", "HEAD^{commit}"],
    )?;
    let head = std::str::from_utf8(&head)
        .map_err(error)?
        .trim_end()
        .to_owned();
    validate_oid(&head)?;
    assert!(path.starts_with(&root));
    assert_eq!(path.parent(), Some(root.as_path()));
    Ok(LeaseRecord {
        lease: WorktreeLease {
            project_id,
            task_id,
            repository: admission.repository.clone(),
            path: path
                .to_str()
                .ok_or_else(|| error("non_utf8_worktree_path"))?
                .into(),
            branch,
            owner_session,
            native_descendant,
            state: LeaseState::Reserved,
        },
        admission,
        head,
        directory: None,
        metadata: None,
    })
}

/// Refuses the private worktree root where no 0700 directory can be made or checked.
/// Takes the runtime folder, unused. Returns `worktree_filesystem_unavailable`.
#[cfg(not(unix))]
fn private_worktree_root(_runtime: &Path) -> Result<PathBuf, EngineError> {
    Err(error("worktree_filesystem_unavailable"))
}

#[cfg(unix)]
fn private_worktree_root(runtime: &Path) -> Result<PathBuf, EngineError> {
    canonical(runtime)?;
    let root = runtime.join("worktrees");
    match std::fs::create_dir(&root) {
        Ok(()) => std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))
            .map_err(error)?,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(error(e)),
    }
    canonical(&root)?;
    let metadata = std::fs::symlink_metadata(&root).map_err(error)?;
    identity(&root, true)?;
    if metadata.mode() & 0o077 != 0 {
        return Err(error("private_worktree_root_required"));
    }
    Ok(root)
}

fn validate_oid(oid: &str) -> Result<(), EngineError> {
    if oid.len() != 40 || !oid.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(error("unsupported_object_id"));
    }
    Ok(())
}

fn load(
    connection: &Connection,
    task: &TaskId,
    repository: &str,
) -> Result<Option<LeaseRecord>, EngineError> {
    let body: Option<String> = connection
        .query_row(
            "SELECT body FROM worktree_leases WHERE task=?1 AND repository=?2",
            params![task.as_str(), repository],
            |r| r.get(0),
        )
        .optional()
        .map_err(error)?;
    body.map(|body| serde_json::from_str(&body).map_err(error))
        .transpose()
}

/// Finds the Git metadata folder of a lease worktree for an engine commit, trusting the
/// worktree's own `.git` file only as far as `worktree_metadata` checks it and only while
/// the folder is still the one recorded when the lease was made. Inputs: the connection
/// and the lease. Output: the folder, or `lease_missing`, `lease_changed` or the
/// worktree refusal code.
pub(crate) fn lease_git_dir(
    connection: &Connection,
    lease: &WorktreeLease,
) -> Result<PathBuf, EngineError> {
    let record = load(connection, &lease.task_id, &lease.repository)?
        .ok_or_else(|| error("lease_missing"))?;
    if record.lease.path != lease.path || record.lease.branch != lease.branch {
        return Err(error("lease_changed"));
    }
    let metadata = worktree_metadata(&record)?;
    let repository = Path::new(&record.lease.repository);
    same_lease_files(
        &record,
        [
            identity(repository, true)?,
            identity(&repository.join(".git"), true)?,
            identity(Path::new(&record.lease.path), true)?,
            identity(&metadata, true)?,
        ],
    )?;
    assert!(metadata.starts_with(Path::new(&record.lease.repository).join(".git/worktrees")));
    Ok(metadata)
}

/// Names the branch of the lease whose worktree is at `path`, the only ref a master
/// worktree's HEAD may name when the engine reads or moves it. Inputs: the connection and
/// the worktree path. Output: the branch, or `lease_missing`.
#[cfg(unix)]
pub(crate) fn lease_branch(connection: &Connection, path: &Path) -> Result<String, EngineError> {
    let path = path.to_str().ok_or_else(|| error("lease_missing"))?;
    let body: Option<String> = connection
        .query_row(
            "SELECT body FROM worktree_leases WHERE path=?1",
            [path],
            |r| r.get(0),
        )
        .optional()
        .map_err(error)?;
    let record: LeaseRecord =
        serde_json::from_str(&body.ok_or_else(|| error("lease_missing"))?).map_err(error)?;
    assert_eq!(record.lease.path, path);
    Ok(record.lease.branch)
}

fn save(connection: &Connection, record: &LeaseRecord) -> Result<(), EngineError> {
    assert_record_consistent(record);
    let count = connection
        .execute(
            "UPDATE worktree_leases SET body=?3 WHERE task=?1 AND repository=?2",
            params![
                record.lease.task_id.as_str(),
                record.lease.repository,
                serde_json::to_string(record).map_err(error)?
            ],
        )
        .map_err(error)?;
    assert_eq!(count, 1);
    Ok(())
}

pub(crate) fn leases(connection: &Connection, task: &TaskId) -> Result<Outcome, EngineError> {
    let mut statement = connection
        .prepare("SELECT body FROM worktree_leases WHERE task=?1 ORDER BY repository LIMIT 129")
        .map_err(error)?;
    let records = statement
        .query_map([task.as_str()], |r| r.get::<_, String>(0))
        .map_err(error)?;
    let leases = records
        .map(|r| {
            let mut record: LeaseRecord =
                serde_json::from_str(&r.map_err(error)?).map_err(error)?;
            if matches!(
                record.lease.state,
                LeaseState::Ready | LeaseState::RecoveryRequired
            ) && verify_record(&record).is_err()
            {
                record.lease.state = LeaseState::Uncertain;
            }
            Ok(record.lease)
        })
        .collect::<Result<Vec<_>, EngineError>>()?;
    if leases.len() > 128 {
        return Err(error("lease_capacity"));
    }
    let outcome = Outcome::Worktrees { leases };
    crate::protocol::encode_frame(&outcome).map_err(error)?;
    Ok(outcome)
}

pub(crate) fn release(
    connection: &Connection,
    task: &TaskId,
    repository: &str,
    owner: &SessionId,
) -> Result<Outcome, EngineError> {
    let Some(record) = load(connection, task, repository)? else {
        return Ok(rejected("lease_missing"));
    };
    if record.lease.owner_session != *owner {
        return Ok(rejected("lease_owner_mismatch"));
    }
    Ok(rejected("writer_quiescence_unqualified"))
}

pub(crate) fn provision(
    connection: &Connection,
    task: &TaskId,
    repository: &str,
) -> Result<(), EngineError> {
    let provisioning = begin_provision(connection, task, repository)?;
    finish_provision(connection, provisioning.materialize())
}

/// A reserved lease taken out of the database so its worktree can be checked out on
/// another thread while the database keeps serving (LB-28).
pub(crate) struct Provisioning(LeaseRecord);

/// A checked-out lease, or the refusal that stopped its checkout, waiting to be stored.
pub(crate) struct Provisioned {
    record: LeaseRecord,
    result: Result<(), EngineError>,
}

/// Takes a reserved lease out of the database for checkout. Inputs: the connection, outside
/// any transaction, the task and repository. Output: the lease to check out, or
/// `lease_missing`.
pub(crate) fn begin_provision(
    connection: &Connection,
    task: &TaskId,
    repository: &str,
) -> Result<Provisioning, EngineError> {
    let record = load(connection, task, repository)?.ok_or_else(|| error("lease_missing"))?;
    assert_eq!(record.lease.state, LeaseState::Reserved);
    assert!(connection.is_autocommit());
    Ok(Provisioning(record))
}

impl Provisioning {
    /// Checks the lease commit out into its worktree, touching only the filesystem and
    /// Git. Input: the reserved lease. Output: the checked-out lease, or the refusal.
    pub(crate) fn materialize(self) -> Provisioned {
        let mut record = self.0;
        let result = materialize(&mut record);
        Provisioned { record, result }
    }
}

/// Stores a checkout: the lease becomes Ready, or Uncertain when the checkout failed, but
/// only while the database still holds it reserved as it was taken out. Inputs: the
/// connection, outside any transaction, and the checkout. Output: the checkout's result,
/// or `lease_changed_during_checkout`.
pub(crate) fn finish_provision(
    connection: &Connection,
    provisioned: Provisioned,
) -> Result<(), EngineError> {
    let Provisioned { mut record, result } = provisioned;
    assert_eq!(record.lease.state, LeaseState::Reserved);
    assert!(connection.is_autocommit());
    let stored = load(connection, &record.lease.task_id, &record.lease.repository)?;
    if stored.map(|stored| stored.lease).as_ref() != Some(&record.lease) {
        return Err(error("lease_changed_during_checkout"));
    }
    record.lease.state = if result.is_ok() {
        LeaseState::Ready
    } else {
        LeaseState::Uncertain
    };
    save(connection, &record)?;
    result
}

/// Returns a lease an engine restart left RecoveryRequired to Ready when its worktree still
/// verifies. The caller has already proved that no writer from an earlier run can still
/// run in it. Inputs: the connection, task and repository. Output: true when the lease is
/// Ready now; any other state, or a worktree that no longer verifies, is left unchanged.
pub(crate) fn requalify(
    connection: &Connection,
    task: &TaskId,
    repository: &str,
) -> Result<bool, EngineError> {
    let mut record = load(connection, task, repository)?.ok_or_else(|| error("lease_missing"))?;
    if record.lease.state != LeaseState::RecoveryRequired || verify_record(&record).is_err() {
        return Ok(false);
    }
    record.lease.state = LeaseState::Ready;
    save(connection, &record)?;
    assert_eq!(
        load(connection, task, repository)?.map(|stored| stored.lease.state),
        Some(LeaseState::Ready)
    );
    Ok(true)
}

/// Refuses to materialize a worktree where no directory-relative no-follow writes exist.
/// Takes the lease record, unchanged. Returns `worktree_filesystem_unavailable`.
#[cfg(not(unix))]
fn materialize(_record: &mut LeaseRecord) -> Result<(), EngineError> {
    Err(error("worktree_filesystem_unavailable"))
}

/// Copies the lease commit into a new linked worktree without running anything the
/// repository configures. Lists and bounds the tree first, lets Git check every path in
/// the lease index, streams each blob through one `cat-file --batch` into files and
/// symlinks written relative to the sealed worktree folder, marks the symlinks that would
/// point outside the lease as skip-worktree instead of writing them, and requires Git to
/// see the copy as the commit. The worktree is made local-only first ([`lock_remotes`]).
/// Input: the reserved lease record, which gains its folder and metadata identities.
/// Output: success, or the refusal code.
#[cfg(unix)]
fn materialize(record: &mut LeaseRecord) -> Result<(), EngineError> {
    let deadline = Instant::now() + MATERIALIZE_TIME_MAX;
    verify(&record.admission)?;
    let repository = Path::new(&record.lease.repository);
    let target = Path::new(&record.lease.path);
    canonical(
        target
            .parent()
            .ok_or_else(|| error("target_parent_missing"))?,
    )?;
    let entries = tree_entries(repository, &record.head, &TREE_BOUNDS, deadline)?;
    git(
        repository,
        &[
            "worktree",
            "add",
            "--no-checkout",
            "-b",
            &record.lease.branch,
            &record.lease.path,
            &record.head,
        ],
    )?;
    canonical(target)?;
    record.directory = Some(identity(target, true)?);
    let directory = OpenOptions::new()
        .read(true)
        .custom_flags((rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::DIRECTORY).bits() as i32)
        .open(target)
        .map_err(error)?;
    let current = directory.metadata().map_err(error)?;
    if record.directory.as_ref()
        != Some(&Identity {
            device: current.dev(),
            inode: current.ino(),
        })
    {
        return Err(error("worktree_changed_during_materialization"));
    }
    directory
        .set_permissions(std::fs::Permissions::from_mode(0o700))
        .map_err(error)?;
    let metadata = worktree_metadata(record)?;
    record.metadata = Some(identity(&metadata, true)?);
    lock_remotes(repository, target)?;
    ensure_submodule_stores(repository, &metadata)?;
    let gitdir = metadata.to_str().ok_or_else(|| error("non_utf8_gitdir"))?;
    let lease = |args: &[&str]| {
        let mut full = vec![
            "-c",
            "core.protectHFS=true",
            "-c",
            "core.protectNTFS=true",
            "--git-dir",
            gitdir,
            "--work-tree",
            &record.lease.path,
            "-C",
            &record.lease.path,
        ];
        full.extend_from_slice(args);
        git_until(repository, &full, OUTPUT_BYTES_MAX, deadline)
    };
    lease(&["read-tree", &record.head])?;
    let left_out = write_tree(repository, &directory, &entries, deadline)?;
    if !left_out.is_empty() {
        let mut args = vec!["update-index", "--skip-worktree", "--"];
        args.extend(left_out.iter().map(String::as_str));
        lease(&args)?;
    }
    lease(&["update-index", "-q", "--refresh"])?;
    if !lease(&["diff-files", "--name-only", "-z"])?.is_empty() {
        return Err(error("worktree_content_differs_from_commit"));
    }
    verify_record(record)?;
    assert!(left_out.len() <= LINKS_LEFT_OUT_MAX);
    assert!(record.directory.is_some());
    Ok(())
}

/// Makes the two submodule-store folders a sandbox profile denies writes to, the common
/// `.git/modules` and this lease's admin `modules`, through `sandbox::ensure_store`, so every
/// platform guards a real folder. A store already there is kept when it is a plain folder;
/// a file or link there is refused, never trusted. Inputs: the repository and the lease
/// admin git dir. Output: success, or `submodule_store_invalid` or the filesystem refusal.
#[cfg(unix)]
fn ensure_submodule_stores(repository: &Path, admin: &Path) -> Result<(), EngineError> {
    for store in [
        repository.join(".git").join("modules"),
        admin.join("modules"),
    ] {
        crate::sandbox::ensure_store(&store).map_err(|problem| match problem.kind() {
            std::io::ErrorKind::InvalidData => error("submodule_store_invalid"),
            _ => error(problem),
        })?;
    }
    Ok(())
}

/// Lists the files and symlinks of a commit tree and refuses one the lease cannot copy:
/// past its bounds, holding a submodule or other non-blob entry, an unsafe or `.git`
/// path, or a symlink target longer than the system path limit. Inputs: the repository,
/// the commit, the bounds and the deadline. Output: the entries in Git's tree order.
#[cfg(unix)]
fn tree_entries(
    repository: &Path,
    head: &str,
    bounds: &TreeBounds,
    deadline: Instant,
) -> Result<Vec<TreeEntry>, EngineError> {
    let bytes = git_until(
        repository,
        &["ls-tree", "-r", "-z", "-l", head],
        TREE_LISTING_BYTES_MAX,
        deadline,
    )?;
    let mut entries = Vec::new();
    let mut total: u64 = 0;
    for entry in bytes.split(|b| *b == 0).filter(|r| !r.is_empty()) {
        if entries.len() >= bounds.files {
            return Err(error("tree_file_capacity"));
        }
        let entry = std::str::from_utf8(entry).map_err(error)?;
        let (header, name) = entry
            .split_once('\t')
            .ok_or_else(|| error("invalid_tree_entry"))?;
        let fields = header.split_whitespace().collect::<Vec<_>>();
        let path = PathBuf::from(name);
        if fields.len() != 4
            || !matches!(fields[0], "100644" | "100755" | "120000")
            || fields[1] != "blob"
            || path
                .components()
                .any(|p| !matches!(p, Component::Normal(_)))
            || path
                .components()
                .any(|p| p.as_os_str().to_string_lossy().eq_ignore_ascii_case(".git"))
        {
            return Err(error("unsupported_or_unsafe_tree_entry"));
        }
        if path.components().count() > bounds.components {
            return Err(error("tree_path_depth"));
        }
        validate_oid(fields[2])?;
        let size = fields[3]
            .parse::<u64>()
            .map_err(|_| error("invalid_tree_entry"))?;
        let link = fields[0] == "120000";
        if link && size > LINK_TARGET_BYTES_MAX {
            return Err(error("unsupported_or_unsafe_tree_entry"));
        }
        total = total.saturating_add(size);
        if total > bounds.bytes {
            return Err(error("tree_byte_capacity"));
        }
        entries.push(TreeEntry {
            link,
            executable: fields[0] == "100755",
            oid: fields[2].into(),
            size,
            path,
        });
    }
    assert!(entries.len() <= bounds.files);
    assert!(total <= bounds.bytes);
    Ok(entries)
}

/// Streams every entry's blob from one `git cat-file --batch` into the worktree folder,
/// killing Git when the deadline passes. Inputs: the repository, the sealed worktree
/// folder, the listed entries and the deadline. Output: the paths of the symlinks left
/// out because they would point outside the lease, or the refusal code.
#[cfg(unix)]
fn write_tree(
    repository: &Path,
    directory: &std::fs::File,
    entries: &[TreeEntry],
    deadline: Instant,
) -> Result<Vec<String>, EngineError> {
    let mut child = crate::spawn_gate::spawn(
        git_command(repository, &["cat-file", "--batch"]).stdin(Stdio::piped()),
    )
    .map_err(error)?;
    let input = child
        .stdin
        .take()
        .ok_or_else(|| error("git_stdin_missing"))?;
    let output = child
        .stdout
        .take()
        .ok_or_else(|| error("git_stdout_missing"))?;
    let copied = std::thread::scope(|scope| {
        let worker = scope.spawn(move || copy_blobs(input, output, directory, entries));
        loop {
            if Instant::now() >= deadline {
                child.kill().map_err(error)?;
                let _ = worker.join();
                break Err(error("materialization_deadline"));
            }
            if worker.is_finished() {
                break worker
                    .join()
                    .map_err(|_| error("materialization_worker_failed"))?;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    });
    let status = loop {
        if let Some(status) = child.try_wait().map_err(error)? {
            break status;
        }
        if Instant::now() >= deadline {
            child.kill().map_err(error)?;
            child.wait().map_err(error)?;
            return Err(error("materialization_deadline"));
        }
        std::thread::sleep(Duration::from_millis(2));
    };
    let left_out = copied?;
    if !status.success() {
        return Err(error("git_operation_failed_or_output_capacity"));
    }
    assert!(left_out.len() <= LINKS_LEFT_OUT_MAX);
    Ok(left_out)
}

/// Asks Git for each entry's blob in turn and writes it: a file with its exact bytes and
/// mode, a symlink when its target stays inside the lease. Inputs: Git's batch input and
/// output, the worktree folder and the entries. Output: the left-out symlink paths.
#[cfg(unix)]
fn copy_blobs(
    mut input: std::process::ChildStdin,
    output: std::process::ChildStdout,
    directory: &std::fs::File,
    entries: &[TreeEntry],
) -> Result<Vec<String>, EngineError> {
    use std::io::BufRead;
    let mut output = std::io::BufReader::with_capacity(64 * 1024, output);
    let mut folders = Folders::new(directory);
    let mut left_out = Vec::new();
    for entry in entries {
        input
            .write_all(format!("{}\n", entry.oid).as_bytes())
            .map_err(error)?;
        input.flush().map_err(error)?;
        let mut header = Vec::new();
        (&mut output)
            .take(128)
            .read_until(b'\n', &mut header)
            .map_err(error)?;
        if header != format!("{} blob {}\n", entry.oid, entry.size).as_bytes() {
            return Err(error("tree_object_mismatch"));
        }
        let mut body = (&mut output).take(entry.size);
        if entry.link {
            let mut target = Vec::new();
            body.read_to_end(&mut target).map_err(error)?;
            if target.len() as u64 != entry.size {
                return Err(error("tree_object_truncated"));
            }
            if link_stays_inside(&entry.path, &target) {
                folders.write_link(&entry.path, &target)?;
            } else {
                if left_out.len() >= LINKS_LEFT_OUT_MAX {
                    return Err(error("tree_link_capacity"));
                }
                let path = entry
                    .path
                    .to_str()
                    .ok_or_else(|| error("invalid_tree_entry"))?;
                left_out.push(path.to_owned());
            }
        } else {
            folders.write_file(&entry.path, &mut body, entry.size, entry.executable)?;
        }
        let mut end = [0u8; 1];
        output.read_exact(&mut end).map_err(error)?;
        if end != *b"\n" {
            return Err(error("tree_object_mismatch"));
        }
    }
    assert!(left_out.len() <= LINKS_LEFT_OUT_MAX);
    Ok(left_out)
}

/// Says whether a tracked symlink resolves inside the lease wherever it is followed from:
/// its target is relative, climbs only with leading `..` steps and no higher than the
/// folder holding the link, then names plain components other than `.git`. Inputs: the
/// link's path in the tree and its target bytes. Output: true when it stays inside.
#[cfg(unix)]
fn link_stays_inside(link: &Path, target: &[u8]) -> bool {
    let Ok(target) = std::str::from_utf8(target) else {
        return false;
    };
    let depth = link.components().count().saturating_sub(1);
    let mut climbing = true;
    let mut up = 0;
    for part in target.split('/') {
        match part {
            ".." if climbing => up += 1,
            "" | "." | ".." => return false,
            name if name.eq_ignore_ascii_case(".git") || name.contains('\0') => return false,
            _ => climbing = false,
        }
    }
    up <= depth
}

/// The worktree folder and the chain of subfolders opened for the previous entry, so
/// entries in Git's tree order reuse open folders. Every folder is created or opened
/// relative to its parent descriptor without following a symlink.
#[cfg(unix)]
struct Folders<'a> {
    root: &'a std::fs::File,
    open: Vec<(std::ffi::OsString, std::fs::File)>,
}

#[cfg(unix)]
impl<'a> Folders<'a> {
    fn new(root: &'a std::fs::File) -> Self {
        Self {
            root,
            open: Vec::new(),
        }
    }

    /// Opens, creating where missing, the folders above a tree path. Input: the path.
    /// Output: the descriptor of its parent folder and its final component.
    fn parent<'p>(
        &mut self,
        path: &'p Path,
    ) -> Result<(&std::fs::File, &'p std::ffi::OsStr), EngineError> {
        use rustix::fs::{Mode, OFlags, mkdirat, openat};
        let components = path
            .components()
            .map(|c| match c {
                Component::Normal(name) => Ok(name),
                _ => Err(error("invalid_materialization_path")),
            })
            .collect::<Result<Vec<_>, _>>()?;
        let Some((name, folders)) = components.split_last() else {
            return Err(error("invalid_materialization_path"));
        };
        if components.len() > TREE_BOUNDS.components {
            return Err(error("invalid_materialization_path"));
        }
        let shared = self
            .open
            .iter()
            .zip(folders.iter())
            .take_while(|((open, _), folder)| open.as_os_str() == **folder)
            .count();
        self.open.truncate(shared);
        for folder in &folders[shared..] {
            let parent = self.open.last().map_or(self.root, |(_, file)| file);
            match mkdirat(parent, *folder, Mode::from_raw_mode(0o700)) {
                Ok(()) | Err(rustix::io::Errno::EXIST) => {}
                Err(e) => return Err(error(e)),
            }
            let next = openat(
                parent,
                *folder,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map(std::fs::File::from)
            .map_err(error)?;
            self.open.push((folder.to_os_string(), next));
        }
        assert_eq!(self.open.len(), folders.len());
        Ok((self.open.last().map_or(self.root, |(_, file)| file), name))
    }

    /// Creates a new file at a tree path with the given bytes. Inputs: the path, the
    /// source, its exact size and whether it is executable. Output: success or the error.
    fn write_file(
        &mut self,
        path: &Path,
        source: &mut impl Read,
        size: u64,
        executable: bool,
    ) -> Result<(), EngineError> {
        use rustix::fs::{Mode, OFlags, openat};
        let (parent, name) = self.parent(path)?;
        let mode = Mode::from_raw_mode(if executable { 0o700 } else { 0o600 });
        let mut file = openat(
            parent,
            name,
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            mode,
        )
        .map(std::fs::File::from)
        .map_err(error)?;
        let written = std::io::copy(&mut source.take(size), &mut file).map_err(error)?;
        if written != size {
            return Err(error("tree_object_truncated"));
        }
        let metadata = file.metadata().map_err(error)?;
        assert_eq!(metadata.len(), size);
        assert_eq!(metadata.nlink(), 1);
        Ok(())
    }

    /// Creates a symlink at a tree path. Inputs: the path and a target that
    /// `link_stays_inside` accepted. Output: success or the error.
    fn write_link(&mut self, path: &Path, target: &[u8]) -> Result<(), EngineError> {
        assert!(link_stays_inside(path, target));
        let target = std::str::from_utf8(target).map_err(error)?;
        let (parent, name) = self.parent(path)?;
        rustix::fs::symlinkat(target, parent, name).map_err(error)?;
        Ok(())
    }
}

/// Finds a linked worktree's Git metadata folder from its `.git` file and checks that it
/// sits in the repository's .git/worktrees. Inputs: the repository and the worktree.
/// Output: the metadata folder, or the refusal code.
fn linked_metadata(repository: &Path, worktree: &Path) -> Result<PathBuf, EngineError> {
    let bytes = bounded_read(&worktree.join(".git"), 4096)?;
    let text = std::str::from_utf8(&bytes).map_err(error)?;
    let path = PathBuf::from(
        text.strip_prefix("gitdir: ")
            .and_then(|s| s.strip_suffix('\n'))
            .ok_or_else(|| error("invalid_worktree_git_pointer"))?,
    );
    canonical(&path)?;
    if path.parent() != Some(repository.join(".git/worktrees").as_path()) {
        return Err(error("worktree_metadata_escape"));
    }
    Ok(path)
}

/// Finds and checks the Git metadata folder of an engine lease worktree: it must sit in
/// the repository's .git/worktrees, pass [`validate_admin`], link back to the lease, and
/// hold no config.worktree, which Git reads for that worktree when
/// extensions.worktreeConfig is on, other than the engine's [`REMOTE_LOCK`]. Input: the
/// lease record. Output: the metadata folder, or the refusal code.
fn worktree_metadata(record: &LeaseRecord) -> Result<PathBuf, EngineError> {
    let path = linked_metadata(
        Path::new(&record.lease.repository),
        Path::new(&record.lease.path),
    )?;
    validate_admin(Path::new(&record.lease.repository), &path)?;
    if bounded_read(&path.join("commondir"), 32)? != b"../..\n"
        || bounded_read(&path.join("gitdir"), 4096)?
            != format!("{}/.git\n", record.lease.path).as_bytes()
    {
        return Err(error("worktree_backlink_mismatch"));
    }
    if !remote_lock_kept(&path) {
        return Err(error("worktree_config_present"));
    }
    Ok(path)
}

/// Reads a worktree's `config.worktree`. Input: the worktree's Git metadata folder.
/// Output: None when there is none, else whether it is a plain file holding exactly
/// [`REMOTE_LOCK`].
fn remote_lock(metadata: &Path) -> Option<bool> {
    holds(metadata, REMOTE_LOCK)
}

/// Reads a worktree's `config.worktree` against one lock. Inputs: the worktree's Git
/// metadata folder and the lock bytes. Output: None when there is none, else whether it is
/// a plain file holding exactly those bytes.
fn holds(metadata: &Path, lock: &[u8]) -> Option<bool> {
    let path = metadata.join("config.worktree");
    match std::fs::symlink_metadata(&path) {
        Err(problem) if problem.kind() == std::io::ErrorKind::NotFound => None,
        Ok(found) if found.is_file() && found.len() == lock.len() as u64 => {
            Some(bounded_read(&path, lock.len()).is_ok_and(|bytes| bytes == lock))
        }
        _ => Some(false),
    }
}

/// Tells whether a worktree's Git metadata holds no per-worktree config but the engine's
/// remote lock: none at all, as worktrees made before the lock have, exactly
/// [`REMOTE_LOCK`], or exactly [`PREVIOUS_REMOTE_LOCK`]. Input: the metadata folder.
/// Output: the answer.
pub(crate) fn remote_lock_kept(metadata: &Path) -> bool {
    remote_lock(metadata) != Some(false) || holds(metadata, PREVIOUS_REMOTE_LOCK) == Some(true)
}

/// Tells whether a repository config listing turns `extensions.worktreeConfig` on as Git
/// reads it: the last entry decides, and a key with no value is true. Input: the output of
/// `git config --null --list`. Output: the answer.
#[cfg(unix)]
fn worktree_config_on(listing: &[u8]) -> bool {
    listing
        .split(|byte| *byte == 0)
        .filter_map(|record| std::str::from_utf8(record).ok())
        .filter_map(|record| {
            let (key, value) = record.split_once('\n').unwrap_or((record, "true"));
            key.eq_ignore_ascii_case("extensions.worktreeconfig")
                .then_some(value)
        })
        .next_back()
        .is_some_and(|value| GIT_TRUE.iter().any(|on| on.eq_ignore_ascii_case(value)))
}

/// What a worktree's `config.worktree` holds as the lock writer reads it: exactly
/// [`REMOTE_LOCK`]; nothing, a strict prefix of [`REMOTE_LOCK`] or [`PREVIOUS_REMOTE_LOCK`]
/// that an engine stop cut short, or the whole previous lock, all of which the engine
/// replaces with the lock; or anything else, which it never overwrites.
#[cfg(unix)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LockState {
    Locked,
    Open,
    Foreign,
}

/// Reads which [`LockState`] a worktree's `config.worktree` is in, without following a
/// link. Input: the worktree's Git metadata folder. Output: the state.
#[cfg(unix)]
fn lock_state(metadata: &Path) -> LockState {
    let path = metadata.join("config.worktree");
    match std::fs::symlink_metadata(&path) {
        Err(problem) if problem.kind() == std::io::ErrorKind::NotFound => LockState::Open,
        Ok(found) if found.is_file() && found.len() <= REMOTE_LOCK.len() as u64 => {
            match bounded_read(&path, REMOTE_LOCK.len()) {
                Ok(bytes) if bytes == REMOTE_LOCK => LockState::Locked,
                Ok(bytes)
                    if REMOTE_LOCK.starts_with(&bytes)
                        || PREVIOUS_REMOTE_LOCK.starts_with(&bytes) =>
                {
                    LockState::Open
                }
                _ => LockState::Foreign,
            }
        }
        _ => LockState::Foreign,
    }
}

/// The partial file the lock is written through before its rename onto `config.worktree`.
#[cfg(unix)]
const LOCK_PARTIAL: &str = ".config.worktree.partial";

const _: () = assert!(PREVIOUS_REMOTE_LOCK.len() < REMOTE_LOCK.len());

/// Writes [`REMOTE_LOCK`] as a worktree's `config.worktree` through a fresh partial file,
/// synced and then renamed over it, all beneath the metadata folder opened once without
/// following a link, so a stop never leaves a torn lock and nothing is written through a
/// link. Input: the worktree's Git metadata folder. Output: success, or the I/O refusal.
#[cfg(unix)]
fn replace_lock(metadata: &Path) -> Result<(), EngineError> {
    use rustix::fs::{AtFlags, Mode, OFlags};
    let folder = rustix::fs::open(
        metadata,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(error)?;
    let _ = rustix::fs::unlinkat(&folder, LOCK_PARTIAL, AtFlags::empty());
    let created = rustix::fs::openat(
        &folder,
        LOCK_PARTIAL,
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::from_raw_mode(0o644),
    )
    .map_err(error)?;
    let mut file = std::fs::File::from(created);
    file.write_all(REMOTE_LOCK).map_err(error)?;
    file.sync_all().map_err(error)?;
    rustix::fs::renameat(&folder, LOCK_PARTIAL, &folder, "config.worktree").map_err(error)?;
    rustix::fs::fsync(&folder).map_err(error)?;
    Ok(())
}

/// Turns `extensions.worktreeConfig` on in a repository's own config when it is off, so Git
/// reads each worktree's `config.worktree`. A write that loses `.git/config.lock` to another
/// git, such as a second first-time setup or the owner's own git, reads the switch again
/// and writes again, at most [`WORKTREE_CONFIG_TRIES`] writes. Input: the repository.
/// Output: success, or the refusal code.
#[cfg(unix)]
fn turn_on_worktree_config(repository: &Path) -> Result<(), EngineError> {
    let mut tries = 0;
    loop {
        let listing = git(
            repository,
            &["config", "--local", "--no-includes", "--null", "--list"],
        )?;
        if worktree_config_on(&listing) {
            return Ok(());
        }
        tries += 1;
        let written = git(
            repository,
            &["config", "--local", "extensions.worktreeConfig", "true"],
        );
        if written.is_ok() || tries >= WORKTREE_CONFIG_TRIES {
            return written.map(drop);
        }
    }
}

/// Leaves a worktree's Git metadata holding exactly [`REMOTE_LOCK`]: a lock in place is
/// kept, an [`LockState::Open`] one is replaced, and any other config is refused untouched.
/// Input: the worktree's Git metadata folder. Output: success, or `worktree_config_present`.
#[cfg(unix)]
fn lock_metadata(metadata: &Path) -> Result<(), EngineError> {
    match lock_state(metadata) {
        LockState::Locked => {}
        LockState::Open => replace_lock(metadata)?,
        LockState::Foreign => return Err(error("worktree_config_present")),
    }
    if remote_lock(metadata) != Some(true) {
        return Err(error("worktree_config_present"));
    }
    Ok(())
}

/// Makes a linked agent worktree local-only, so a push, pull or fetch there fails with
/// [`REMOTE_REFUSAL`]: turns `extensions.worktreeConfig` on in the repository's own config
/// when it is off, then leaves the worktree's `config.worktree` holding exactly
/// [`REMOTE_LOCK`], repairing one an engine stop cut short or the previous lock. This is a
/// plain guard for the owner's tools; the agent sandbox is the boundary. Inputs: the
/// repository and the worktree. Output: success, or the refusal code.
#[cfg(unix)]
pub fn lock_remotes(repository: &Path, worktree: &Path) -> Result<(), EngineError> {
    let metadata = linked_metadata(repository, worktree)?;
    turn_on_worktree_config(repository)?;
    lock_metadata(&metadata)
}

/// The most repositories one start sweep reads.
#[cfg(unix)]
const SWEEP_REPOSITORIES_MAX: usize = 1024;
/// The most linked worktrees the start sweep reads in one repository.
#[cfg(unix)]
const SWEEP_WORKTREES_MAX: usize = 512;

/// Every repository the engine knows: those its projects declare, those its team agents'
/// worktrees were added from and those its leases are in, at most
/// `SWEEP_REPOSITORIES_MAX`. Input: the store. Output: the repositories.
#[cfg(unix)]
pub(crate) fn known_repositories(
    connection: &Connection,
) -> Result<std::collections::BTreeSet<PathBuf>, EngineError> {
    let mut repositories = std::collections::BTreeSet::new();
    let limit = SWEEP_REPOSITORIES_MAX as i64;
    let mut projects = connection
        .prepare("SELECT body FROM projects LIMIT ?1")
        .map_err(error)?;
    for body in projects
        .query_map([limit], |row| row.get::<_, String>(0))
        .map_err(error)?
    {
        let stored: StoredProject = serde_json::from_str(&body.map_err(error)?).map_err(error)?;
        repositories.extend(stored.spec.repositories.into_iter().map(PathBuf::from));
    }
    for query in [
        "SELECT DISTINCT repository FROM team_worktrees LIMIT ?1",
        "SELECT DISTINCT repository FROM worktree_leases LIMIT ?1",
    ] {
        let mut statement = connection.prepare(query).map_err(error)?;
        for repository in statement
            .query_map([limit], |row| row.get::<_, String>(0))
            .map_err(error)?
        {
            repositories.insert(PathBuf::from(repository.map_err(error)?));
        }
    }
    Ok(repositories
        .into_iter()
        .filter(|repository| repository.is_absolute())
        .take(SWEEP_REPOSITORIES_MAX)
        .collect())
}

/// The engine's own linked worktrees of one repository: each one Git lists in
/// `.git/worktrees` whose folder sits in the project's agent worktrees folder, as team and
/// research agents' do, or is a lease's folder. The owner's own linked worktrees elsewhere
/// are never listed. Inputs: the repository and the lease folders. Output: the worktrees
/// with their metadata folders.
#[cfg(unix)]
fn agent_worktrees(
    repository: &Path,
    leases: &std::collections::BTreeSet<PathBuf>,
) -> Vec<(PathBuf, PathBuf)> {
    let agents = crate::projects_home::state_folder(repository).join("worktrees");
    let Ok(entries) = std::fs::read_dir(repository.join(".git/worktrees")) else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for entry in entries.flatten().take(SWEEP_WORKTREES_MAX) {
        let metadata = entry.path();
        let Ok(bytes) = bounded_read(&metadata.join("gitdir"), 4096) else {
            continue;
        };
        let Some(worktree) = std::str::from_utf8(&bytes)
            .ok()
            .and_then(|text| text.strip_suffix("/.git\n"))
            .map(PathBuf::from)
        else {
            continue;
        };
        if worktree.parent() == Some(agents.as_path()) || leases.contains(&worktree) {
            found.push((worktree, metadata));
        }
    }
    assert!(found.len() <= SWEEP_WORKTREES_MAX);
    found
}

/// Locks the remotes of every agent worktree the engine already made, once at engine start,
/// so worktrees added before the lock, or whose lock a stop cut short, refuse remotes as new
/// ones do: team and research worktrees in each known repository's agent worktrees folder
/// and every lease's worktree, each linking back to the metadata it was found by. The
/// owner's own linked worktrees are left alone. A worktree that cannot be locked logs its
/// plain code and the sweep goes on. Input: the store. Output: how many worktrees now hold
/// the lock.
#[cfg(unix)]
pub fn lock_existing(connection: &Connection) -> Result<usize, EngineError> {
    let mut leases = std::collections::BTreeSet::new();
    let mut statement = connection
        .prepare("SELECT body FROM worktree_leases LIMIT ?1")
        .map_err(error)?;
    for body in statement
        .query_map([SWEEP_REPOSITORIES_MAX as i64], |row| {
            row.get::<_, String>(0)
        })
        .map_err(error)?
    {
        let record: LeaseRecord = serde_json::from_str(&body.map_err(error)?).map_err(error)?;
        leases.insert(PathBuf::from(record.lease.path));
    }
    let logged = |problem: EngineError| {
        eprintln!(
            "worktree_lock_failed cause={}",
            crate::protocol::plain_code(&problem.0)
        );
    };
    let mut locked = 0;
    for repository in known_repositories(connection)? {
        let found = agent_worktrees(&repository, &leases);
        if found.is_empty() {
            continue;
        }
        if let Err(problem) = turn_on_worktree_config(&repository) {
            logged(problem);
            continue;
        }
        for (worktree, metadata) in found {
            let checked = linked_metadata(&repository, &worktree).and_then(|linked| {
                if linked == metadata {
                    lock_metadata(&metadata)
                } else {
                    Err(error("worktree_metadata_escape"))
                }
            });
            match checked {
                Ok(()) => locked += 1,
                Err(problem) => logged(problem),
            }
        }
    }
    Ok(locked)
}

/// Checks a lease record's four pinned folders, the admitted root and .git, the worktree
/// folder and its Git metadata folder, against the live ones in that order under the
/// whole-volume renumber rule. Inputs: the record and the live identities. Output:
/// success, or `worktree_identity_changed`, also for a record without worktree pins.
fn same_lease_files(record: &LeaseRecord, live: [Identity; 4]) -> Result<(), EngineError> {
    let (Some(directory), Some(admin)) = (&record.directory, &record.metadata) else {
        return Err(error("worktree_identity_changed"));
    };
    let pinned = [
        &record.admission.root,
        &record.admission.metadata,
        directory,
        admin,
    ]
    .map(Identity::pair);
    if !crate::file_identity::same_files(&pinned, &live.map(|identity| identity.pair())) {
        return Err(error("worktree_identity_changed"));
    }
    Ok(())
}

/// Checks that a lease still owns its worktree: the admitted repository, the worktree
/// folder and its Git metadata folder keep their pinned identities, under the whole-volume
/// renumber rule over all four, and Git still registers the lease branch and revision.
/// Input: the stored lease record, never rewritten. Output: success, or the refusal code.
fn verify_record(record: &LeaseRecord) -> Result<(), EngineError> {
    assert_record_consistent(record);
    let current = verify(&record.admission)?;
    let target = Path::new(&record.lease.path);
    canonical(target)?;
    let metadata = worktree_metadata(record)?;
    same_lease_files(
        record,
        [
            current.root,
            current.metadata,
            identity(target, true)?,
            identity(&metadata, true)?,
        ],
    )?;
    let bytes = git(
        Path::new(&record.lease.repository),
        &["worktree", "list", "--porcelain", "-z"],
    )?;
    let needle = format!("worktree {}\0", record.lease.path);
    let registration = bytes.split(|b| *b == 0).collect::<Vec<_>>();
    let prefix = needle.strip_suffix('\0').unwrap().as_bytes();
    let Some(index) = registration.iter().position(|v| *v == prefix) else {
        return Err(error("worktree_registration_missing"));
    };
    let branch = format!("branch refs/heads/{}", record.lease.branch);
    if registration.get(index + 2).copied() != Some(branch.as_bytes()) {
        return Err(error("worktree_branch_mismatch"));
    }
    let oid = git(
        Path::new(&record.lease.repository),
        &[
            "rev-parse",
            "--verify",
            &format!("refs/heads/{}^{{commit}}", record.lease.branch),
        ],
    )?;
    let oid = std::str::from_utf8(&oid).map_err(error)?.trim_end();
    validate_oid(oid)?;
    if registration.get(index + 1).copied() != Some(format!("HEAD {oid}").as_bytes()) {
        return Err(error("worktree_revision_mismatch"));
    }
    let head = bounded_read(&metadata.join("HEAD"), HEAD_BYTES_MAX)?;
    if head != format!("ref: refs/heads/{}\n", record.lease.branch).as_bytes() {
        return Err(error("worktree_head_mismatch"));
    }
    assert!(record.directory.is_some());
    assert!(record.metadata.is_some());
    Ok(())
}

/// Tells whether an Uncertain lease may have been left Uncertain only by a volume renumber
/// that an older engine refused: its worktree identities were pinned and the live
/// repository folder now has another device. Such a lease is verified again and can only
/// become RecoveryRequired. Input: the stored record. Output: true to verify it again.
#[cfg(unix)]
fn renumbered(record: &LeaseRecord) -> bool {
    record.directory.is_some()
        && record.metadata.is_some()
        && identity(Path::new(&record.lease.repository), true)
            .is_ok_and(|live| live.device != record.admission.root.device)
}

#[cfg(unix)]
pub(crate) fn reconcile(connection: &Connection) -> Result<(), EngineError> {
    let mut statement = connection
        .prepare("SELECT body FROM worktree_leases LIMIT 129")
        .map_err(error)?;
    let records = statement
        .query_map([], |r| r.get::<_, String>(0))
        .map_err(error)?
        .map(|r| serde_json::from_str::<LeaseRecord>(&r.map_err(error)?).map_err(error))
        .collect::<Result<Vec<_>, _>>()?;
    if records.len() > 128 {
        return Err(error("lease_capacity"));
    }
    for mut record in records {
        let checked = match record.lease.state {
            LeaseState::Ready | LeaseState::RecoveryRequired => true,
            LeaseState::Uncertain => renumbered(&record),
            LeaseState::Reserved => false,
        };
        record.lease.state = if checked && verify_record(&record).is_ok() {
            LeaseState::RecoveryRequired
        } else {
            LeaseState::Uncertain
        };
        save(connection, &record)?;
    }
    assert!(connection.is_autocommit());
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn reserved_record() -> LeaseRecord {
        LeaseRecord {
            lease: WorktreeLease {
                project_id: ProjectId::new("project").unwrap(),
                task_id: TaskId::new("task").unwrap(),
                repository: "/repository".into(),
                path: "/runtime/worktrees/task".into(),
                branch: "crazyagent/task".into(),
                owner_session: SessionId::new("owner").unwrap(),
                native_descendant: None,
                state: LeaseState::Reserved,
            },
            admission: Admission {
                repository: "/repository".into(),
                root: Identity {
                    device: 1,
                    inode: 2,
                },
                metadata: Identity {
                    device: 1,
                    inode: 3,
                },
            },
            head: "a".repeat(40),
            directory: None,
            metadata: None,
        }
    }

    #[test]
    #[should_panic(expected = "lease repository must match its admission")]
    fn internal_record_assertion_detects_cross_repository_lease() {
        let mut record = reserved_record();
        record.lease.repository = "/other-repository".into();
        assert_record_consistent(&record);
    }

    #[test]
    #[should_panic(expected = "ready lease requires directory identity")]
    fn internal_record_assertion_detects_unsealed_ready_lease() {
        let mut record = reserved_record();
        record.lease.state = LeaseState::Ready;
        assert_record_consistent(&record);
    }

    #[test]
    fn repository_config_rule_refuses_keys_git_runs_and_accepts_tool_metadata() {
        for (key, value) in [
            ("branch.feature/x.vscode-merge-base", "origin/trunk"),
            ("branch.trunk.remote", "origin"),
            ("branch.trunk.merge", "refs/heads/trunk"),
            ("branch.trunk.description", "Owner notes"),
            (
                "remote.origin.url",
                "https://example.invalid/owner/project.git",
            ),
            (
                "remote.origin.pushurl",
                "https://example.invalid/owner/project.git",
            ),
            ("remote.origin.fetch", "+refs/heads/*:refs/remotes/origin/*"),
            ("remote.origin.gh-resolved", "base"),
            ("core.repositoryformatversion", "0"),
            ("core.repositoryformatversion", "1"),
            ("core.bare", "false"),
            ("core.filemode", "true"),
            ("core.logallrefupdates", "true"),
            ("core.ignorecase", "true"),
            ("core.precomposeunicode", "true"),
            ("core.autocrlf", "input"),
            ("extensions.worktreeconfig", "true"),
            ("extensions.worktreeconfig", "FALSE"),
            ("codex.localenvironmentconfigpath", "__none__"),
            ("user.name", "Owner"),
            ("user.email", "owner@example.invalid"),
            ("init.defaultbranch", "trunk"),
            ("alias.st", "status"),
            ("lfs.repositoryformatversion", "0"),
            ("submodule.lib.url", "https://example.invalid/lib.git"),
            ("pull.rebase", "true"),
        ] {
            assert!(
                refused_key(key, value).is_none(),
                "{key}={value} must stay accepted"
            );
        }
        let mut seen = Vec::new();
        for (key, value, code) in [
            ("core.repositoryformatversion", "2", "repository_format"),
            ("core.bare", "true", "core_bare"),
            (
                "extensions.worktreeconfig",
                "maybe",
                "extensions_worktreeconfig",
            ),
            ("extensions.objectformat", "sha256", "repository_extension"),
            ("core.hookspath", ".githooks", "core_hookspath"),
            ("hook.lint.command", "make lint", "hook_command"),
            ("hook.lint.event", "pre-commit", "hook_command"),
            ("core.fsmonitor", "true", "core_fsmonitor"),
            ("core.sshcommand", "ssh -F config", "core_sshcommand"),
            ("core.gitproxy", "proxy-command", "core_gitproxy"),
            ("core.editor", "vim", "core_editor"),
            ("core.pager", "less", "core_pager"),
            ("core.askpass", "prompt", "core_askpass"),
            (
                "core.alternaterefscommand",
                "list",
                "core_alternaterefscommand",
            ),
            ("core.worktree", "/elsewhere", "core_worktree"),
            ("core.attributesfile", "/elsewhere", "core_attributesfile"),
            (
                "filter.lfs.process",
                "git-lfs filter-process",
                "filter_driver",
            ),
            ("filter.lfs.required", "true", "filter_driver"),
            ("filter.a.b.smudge", "command", "filter_driver"),
            ("diff.pdf.textconv", "pdftotext", "diff_driver"),
            ("diff.pdf.command", "command", "diff_driver"),
            ("diff.external", "command", "diff_external"),
            ("difftool.vim.cmd", "command", "difftool"),
            ("difftool.prompt", "false", "difftool"),
            ("merge.ours.driver", "true", "merge_driver"),
            ("mergetool.vim.cmd", "command", "mergetool"),
            ("sequence.editor", "command", "sequence_editor"),
            ("credential.helper", "store", "credential"),
            (
                "credential.https://example.invalid.helper",
                "store",
                "credential",
            ),
            ("gpg.program", "command", "gpg_program"),
            ("gpg.openpgp.program", "command", "gpg_program"),
            ("gpg.ssh.defaultkeycommand", "command", "gpg_program"),
            ("pager.log", "command", "pager_command"),
            ("trailer.sign.command", "command", "trailer_command"),
            ("trailer.sign.cmd", "command", "trailer_command"),
            (
                "interactive.difffilter",
                "command",
                "interactive_difffilter",
            ),
            ("submodule.lib.update", "!command", "submodule_update"),
            (
                "url.https://example.invalid/.insteadof",
                "git@example.invalid:",
                "url_rewrite",
            ),
            ("url.ssh://h/.pushinsteadof", "https://h/", "url_rewrite"),
            ("include.path", "/elsewhere", "include"),
            ("includeif.gitdir:/a.b/.path", "/elsewhere", "include_if"),
            ("remote.origin.uploadpack", "command", "remote_command"),
            ("remote.origin.receivepack", "command", "remote_command"),
            ("remote.origin.vcs", "helper", "remote_command"),
            ("remote.origin.proxy", "http://proxy", "remote_command"),
            ("http.proxy", "http://proxy", "http_proxy"),
            (
                "http.https://example.invalid.proxy",
                "http://proxy",
                "http_proxy",
            ),
            ("protocol.allow", "always", "protocol_allow"),
            ("protocol.ext.allow", "always", "protocol_allow"),
            ("uploadpack.packobjectshook", "command", "uploadpack_hook"),
            ("gc.recentobjectshook", "command", "gc_hook"),
            ("sendemail.sendmailcmd", "command", "sendemail_command"),
            (
                "sendemail.smtpserver",
                "/usr/bin/command",
                "sendemail_command",
            ),
            ("imap.tunnel", "command", "imap_tunnel"),
            ("browser.firefox.cmd", "command", "browser_command"),
            ("man.viewer.path", "command", "man_command"),
            ("guitool.lint.cmd", "command", "guitool_command"),
        ] {
            assert_eq!(
                refused_key(key, value).map(|rule| rule.code),
                Some(code),
                "{key}={value}"
            );
            seen.push(code);
        }
        let mut codes: Vec<_> = REFUSED_KEYS.iter().map(|rule| rule.code).collect();
        codes.sort_unstable();
        codes.dedup();
        assert_eq!(codes.len(), REFUSED_KEYS.len(), "rule codes must be unique");
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen, codes, "every rule needs a refused case");
        assert_eq!(
            refused_config_line("repository_config_refused:core_hookspath", "/owner/repo")
                .as_deref(),
            Some(concat!(
                "/owner/repo has the Git setting core.hooksPath, which runs hooks from a ",
                "folder the repository chooses. Remove it from the repository's .git/config, ",
                "then send again."
            ))
        );
        assert_eq!(refused_config_line("unsupported", "/owner/repo"), None);
        assert_eq!(
            refused_config_line("repository_config_refused:unknown", "/owner/repo"),
            None
        );
    }

    #[test]
    fn descriptor_materialization_resists_renamed_root_symlink_swap() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("worktree");
        let retained = temporary.path().join("retained");
        let outside = temporary.path().join("outside");
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(&outside).unwrap();
        let directory = std::fs::File::open(&root).unwrap();
        std::fs::rename(&root, &retained).unwrap();
        std::os::unix::fs::symlink(&outside, &root).unwrap();
        Folders::new(&directory)
            .write_file(
                Path::new("nested/tracked"),
                &mut &b"source bytes"[..],
                12,
                false,
            )
            .unwrap();
        assert_eq!(
            std::fs::read(retained.join("nested/tracked")).unwrap(),
            b"source bytes"
        );
        assert!(!outside.join("nested").exists());
        std::os::unix::fs::symlink(&outside, retained.join("escape")).unwrap();
        assert!(
            Folders::new(&directory)
                .write_file(Path::new("escape/file"), &mut &b"forbidden"[..], 9, false)
                .is_err()
        );
        assert!(!outside.join("file").exists());
        let mut folders = Folders::new(&directory);
        folders
            .write_file(Path::new("open/first"), &mut &b"one"[..], 3, false)
            .unwrap();
        std::fs::rename(retained.join("open"), retained.join("moved")).unwrap();
        std::os::unix::fs::symlink(&outside, retained.join("open")).unwrap();
        folders
            .write_file(Path::new("open/second"), &mut &b"two"[..], 3, false)
            .unwrap();
        assert_eq!(
            std::fs::read(retained.join("moved/second")).unwrap(),
            b"two"
        );
        assert!(!outside.join("second").exists());
    }

    fn committed_repository(root: &Path) -> PathBuf {
        let repository = root.join("repository");
        std::fs::create_dir(&repository).unwrap();
        std::fs::create_dir_all(repository.join("one/two")).unwrap();
        std::fs::write(repository.join("top"), b"12345").unwrap();
        std::fs::write(repository.join("one/middle"), b"123").unwrap();
        std::fs::write(repository.join("one/two/leaf"), b"1").unwrap();
        std::os::unix::fs::symlink("../top", repository.join("one/link")).unwrap();
        for args in [
            &["init", "-q", "-b", "main"][..],
            &["add", "-A"],
            &[
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@invalid",
                "commit",
                "-q",
                "-m",
                "base",
            ],
        ] {
            git(&repository, args).unwrap();
        }
        repository
    }

    #[test]
    #[cfg(unix)]
    fn submodule_stores_are_created_private_and_idempotent() {
        let temporary = tempfile::tempdir().unwrap();
        let repository = temporary.path().join("repository");
        let admin = temporary.path().join("admin");
        std::fs::create_dir_all(repository.join(".git")).unwrap();
        std::fs::create_dir(&admin).unwrap();
        ensure_submodule_stores(&repository, &admin).unwrap();
        for store in [repository.join(".git/modules"), admin.join("modules")] {
            let metadata = std::fs::metadata(&store).unwrap();
            assert!(metadata.is_dir(), "{} is not a folder", store.display());
            assert_eq!(metadata.permissions().mode() & 0o777, 0o700);
        }
        ensure_submodule_stores(&repository, &admin).unwrap();
    }

    #[test]
    #[cfg(unix)]
    fn a_submodule_store_that_is_a_file_or_a_link_is_refused_not_trusted() {
        let temporary = tempfile::tempdir().unwrap();
        let repository = temporary.path().join("repository");
        let admin = temporary.path().join("admin");
        let elsewhere = temporary.path().join("elsewhere");
        std::fs::create_dir_all(repository.join(".git")).unwrap();
        std::fs::create_dir(&admin).unwrap();
        std::fs::create_dir(&elsewhere).unwrap();
        let common = repository.join(".git").join("modules");
        std::fs::write(&common, b"planted").unwrap();
        assert_eq!(
            ensure_submodule_stores(&repository, &admin).unwrap_err().0,
            "submodule_store_invalid"
        );
        assert_eq!(std::fs::read(&common).unwrap(), b"planted");
        std::fs::remove_file(&common).unwrap();
        let private = admin.join("modules");
        std::os::unix::fs::symlink(&elsewhere, &private).unwrap();
        assert_eq!(
            ensure_submodule_stores(&repository, &admin).unwrap_err().0,
            "submodule_store_invalid"
        );
        assert!(
            std::fs::symlink_metadata(&private)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(std::fs::read_dir(&elsewhere).unwrap().count(), 0);
    }

    /// The step a walk on this thread runs before each read of a listed .git entry, set for
    /// one test and cleared when dropped.
    struct Step;

    impl Step {
        /// Sets the step. Input: what to do with the entry and whether the coming read
        /// lists it as a folder. Output: the guard that clears it.
        fn set(step: impl Fn(&Path, bool) + 'static) -> Self {
            BEFORE_READ.with(|slot| *slot.borrow_mut() = Some(Box::new(step)));
            Self
        }
    }

    impl Drop for Step {
        fn drop(&mut self) {
            BEFORE_READ.with(|slot| *slot.borrow_mut() = None);
        }
    }

    /// A repository whose .git holds what Git makes and removes while it works: lock files,
    /// a temporary pack, loose objects in fan-out folders, another worktree's admin folder
    /// and a folder of a running rebase. Input: the folder to make it in. Output: the
    /// repository.
    fn metadata_folder(root: &Path) -> PathBuf {
        let repository = root.join("repository");
        let git = repository.join(".git");
        for folder in [
            "refs/heads",
            "objects/ab",
            "objects/cd",
            "objects/ef",
            "objects/pack",
            "worktrees/other",
            "rebase-merge",
        ] {
            std::fs::create_dir_all(git.join(folder)).unwrap();
        }
        for file in [
            "HEAD",
            "index.lock",
            "refs/heads/main.lock",
            "objects/ab/0123",
            "objects/ab/4567",
            "objects/pack/tmp_pack_1",
        ] {
            std::fs::write(git.join(file), b"kept\n").unwrap();
        }
        repository
    }

    /// Removes one entry as Git does when it is done with it. Input: the file or folder.
    fn unlink(path: &Path) {
        if std::fs::symlink_metadata(path).unwrap().is_dir() {
            std::fs::remove_dir_all(path).unwrap();
        } else {
            std::fs::remove_file(path).unwrap();
        }
    }

    #[test]
    fn a_git_entry_removed_between_its_listing_and_its_read_is_no_finding() {
        let root = tempfile::tempdir().unwrap();
        let repository = metadata_folder(root.path());
        let git = repository.join(".git");
        let gone = [
            "index.lock",
            "refs/heads/main.lock",
            "objects/pack/tmp_pack_1",
            "objects/ab/0123",
            "objects/cd",
            "worktrees/other",
        ]
        .map(|name| git.join(name));
        let emptied = ["rebase-merge", "objects/ef"].map(|name| git.join(name));
        let step = {
            let (gone, emptied) = (gone.clone(), emptied.clone());
            Step::set(move |path, listing| {
                let named = if listing { &emptied[..] } else { &gone[..] };
                if named.iter().any(|entry| entry == path) {
                    unlink(path);
                }
            })
        };
        validate_metadata(&repository).unwrap();
        drop(step);
        for entry in gone.iter().chain(&emptied) {
            assert!(
                std::fs::symlink_metadata(entry).is_err(),
                "the walk never read {}",
                entry.display()
            );
        }
        assert!(git.join("objects/ab/4567").is_file());
        validate_metadata(&repository).unwrap();
    }

    #[test]
    fn a_link_or_a_pipe_put_in_a_listed_entry_s_place_is_still_refused() {
        for (name, pipe) in [
            ("index.lock", false),
            ("index.lock", true),
            ("objects/ab/0123", false),
            ("objects/ab/0123", true),
            ("refs/heads", false),
            ("refs/heads", true),
            ("objects/cd", false),
            ("worktrees/other", false),
        ] {
            let root = tempfile::tempdir().unwrap();
            let repository = metadata_folder(root.path());
            let outside = root.path().join("outside");
            std::fs::create_dir(&outside).unwrap();
            let swapped = repository.join(".git").join(name);
            let step = {
                let swapped = swapped.clone();
                Step::set(move |path, listing| {
                    let kept = std::fs::symlink_metadata(path)
                        .is_ok_and(|entry| entry.is_file() || entry.is_dir());
                    if path != swapped || listing || !kept {
                        return;
                    }
                    unlink(path);
                    if pipe {
                        let made = crate::spawn_gate::status(
                            std::process::Command::new("/usr/bin/mkfifo").arg(path),
                        )
                        .unwrap();
                        assert!(made.success());
                    } else {
                        std::os::unix::fs::symlink(&outside, path).unwrap();
                    }
                })
            };
            let refused = validate_metadata(&repository).unwrap_err();
            drop(step);
            assert_eq!(
                refused.0,
                format!("{UNTRUSTED_GIT_ENTRY}{name}"),
                "pipe {pipe}"
            );
            let left = std::fs::symlink_metadata(&swapped).unwrap();
            assert!(
                !left.is_file() && !left.is_dir(),
                "{name} was never swapped"
            );
        }
    }

    #[test]
    fn a_missing_start_or_an_unreadable_folder_is_a_failure_not_an_entry_gone() {
        let root = tempfile::tempdir().unwrap();
        let repository = metadata_folder(root.path());
        let git = repository.join(".git");
        validate_admin(&repository, &git.join("worktrees/other")).unwrap();
        assert!(validate_admin(&repository, &git.join("worktrees/absent")).is_err());
        let closed = git.join("refs/heads");
        std::fs::set_permissions(&closed, std::fs::Permissions::from_mode(0o000)).unwrap();
        let refused = validate_metadata(&repository);
        std::fs::set_permissions(&closed, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(refused.is_err(), "an unreadable folder was skipped as gone");
        validate_metadata(&repository).unwrap();
        std::fs::remove_dir_all(&git).unwrap();
        assert!(validate_metadata(&repository).is_err());
    }

    #[test]
    fn tree_listing_refuses_a_tree_past_its_file_byte_or_depth_bound() {
        let temporary = tempfile::tempdir().unwrap();
        let repository = committed_repository(temporary.path());
        let later = Instant::now() + Duration::from_secs(5);
        let bounds = |files, bytes, components| TreeBounds {
            files,
            bytes,
            components,
        };
        let entries = tree_entries(&repository, "HEAD", &bounds(4, 15, 3), later).unwrap();
        assert_eq!(
            entries
                .iter()
                .map(|e| (e.path.to_str().unwrap(), e.size, e.link))
                .collect::<Vec<_>>(),
            [
                ("one/link", 6, true),
                ("one/middle", 3, false),
                ("one/two/leaf", 1, false),
                ("top", 5, false),
            ]
        );
        for (bounds, code) in [
            (bounds(3, 15, 3), "tree_file_capacity"),
            (bounds(4, 14, 3), "tree_byte_capacity"),
            (bounds(4, 15, 2), "tree_path_depth"),
        ] {
            assert_eq!(
                tree_entries(&repository, "HEAD", &bounds, later)
                    .unwrap_err()
                    .0,
                code
            );
        }
    }

    #[test]
    fn writing_a_tree_stops_at_its_deadline() {
        let temporary = tempfile::tempdir().unwrap();
        let repository = committed_repository(temporary.path());
        let later = Instant::now() + Duration::from_secs(5);
        let entries = tree_entries(&repository, "HEAD", &TREE_BOUNDS, later).unwrap();
        let target = temporary.path().join("copy");
        std::fs::create_dir(&target).unwrap();
        let directory = std::fs::File::open(&target).unwrap();
        assert_eq!(
            write_tree(&repository, &directory, &entries, Instant::now())
                .unwrap_err()
                .0,
            "materialization_deadline"
        );
        let copy = temporary.path().join("second-copy");
        std::fs::create_dir(&copy).unwrap();
        let directory = std::fs::File::open(&copy).unwrap();
        assert!(
            write_tree(&repository, &directory, &entries, later)
                .unwrap()
                .is_empty()
        );
        assert_eq!(std::fs::read(copy.join("one/two/leaf")).unwrap(), b"1");
        assert_eq!(
            std::fs::read_link(copy.join("one/link")).unwrap(),
            Path::new("../top")
        );
    }

    #[test]
    fn a_tracked_symlink_stays_inside_only_by_leading_parent_steps_within_its_depth() {
        for (link, target, inside) in [
            ("CLAUDE.md", "AGENTS.md", true),
            ("docs/current", "../src/m0", true),
            ("a/b/up", "../..", true),
            ("a/b/c", "../../x/y", true),
            ("top", "../outside", false),
            ("a/b/c", "../../../outside", false),
            ("abs", "/etc/passwd", false),
            ("a/mid", "x/../../outside", false),
            ("a/dot", "./x", false),
            ("a/empty", "x//y", false),
            ("a/trailing", "x/", false),
            ("none", "", false),
            ("a/git", "../.git/config", false),
            ("a/git-case", "../.GIT", false),
            ("a/nul", "x\0y", false),
        ] {
            assert_eq!(
                link_stays_inside(Path::new(link), target.as_bytes()),
                inside,
                "{link} -> {target}"
            );
        }
        assert!(!link_stays_inside(Path::new("bytes"), &[0xff, 0xfe]));
    }

    #[cfg(unix)]
    #[test]
    fn only_a_missing_config_or_the_exact_remote_lock_counts_as_kept() {
        let temporary = tempfile::tempdir().unwrap();
        let metadata = temporary.path();
        let config = metadata.join("config.worktree");
        assert_eq!(remote_lock(metadata), None);
        assert!(remote_lock_kept(metadata));
        std::fs::write(&config, REMOTE_LOCK).unwrap();
        assert_eq!(remote_lock(metadata), Some(true));
        assert!(remote_lock_kept(metadata));
        std::fs::write(&config, PREVIOUS_REMOTE_LOCK).unwrap();
        assert_eq!(remote_lock(metadata), Some(false));
        assert!(remote_lock_kept(metadata));
        let mut previous_longer = PREVIOUS_REMOTE_LOCK.to_vec();
        previous_longer.extend_from_slice(b"[core]\n\tfsmonitor = touch /tmp/x\n");
        std::fs::write(&config, &previous_longer).unwrap();
        assert!(!remote_lock_kept(metadata));
        let mut longer = REMOTE_LOCK.to_vec();
        longer.extend_from_slice(b"[core]\n\tfsmonitor = touch /tmp/x\n");
        std::fs::write(&config, &longer).unwrap();
        assert!(!remote_lock_kept(metadata));
        let mut same_length = REMOTE_LOCK.to_vec();
        same_length[2] = b'x';
        std::fs::write(&config, &same_length).unwrap();
        assert!(!remote_lock_kept(metadata));
        std::fs::remove_file(&config).unwrap();
        let elsewhere = metadata.join("elsewhere");
        std::fs::write(&elsewhere, REMOTE_LOCK).unwrap();
        std::os::unix::fs::symlink(&elsewhere, &config).unwrap();
        assert!(!remote_lock_kept(metadata));
        std::fs::remove_file(&config).unwrap();
        std::fs::create_dir(&config).unwrap();
        assert!(!remote_lock_kept(metadata));
    }

    #[cfg(unix)]
    #[test]
    fn a_missing_cut_short_or_previous_lock_is_replaced_and_any_other_config_is_left_alone() {
        let temporary = tempfile::tempdir().unwrap();
        let metadata = temporary.path();
        let config = metadata.join("config.worktree");
        let partial = metadata.join(LOCK_PARTIAL);
        let repaired: [&[u8]; 6] = [
            b"",
            &REMOTE_LOCK[..1],
            &REMOTE_LOCK[..REMOTE_LOCK.len() - 1],
            PREVIOUS_REMOTE_LOCK,
            &PREVIOUS_REMOTE_LOCK[..PREVIOUS_REMOTE_LOCK.len() - 3],
            REMOTE_LOCK,
        ];
        assert_eq!(lock_state(metadata), LockState::Open);
        lock_metadata(metadata).unwrap();
        assert_eq!(std::fs::read(&config).unwrap(), REMOTE_LOCK);
        for held in repaired {
            std::fs::write(&config, held).unwrap();
            std::fs::write(&partial, b"[core]\n\tfsmonitor = touch /tmp/x\n").unwrap();
            let expected = if held == REMOTE_LOCK {
                LockState::Locked
            } else {
                LockState::Open
            };
            assert_eq!(lock_state(metadata), expected, "{held:?}");
            lock_metadata(metadata).unwrap();
            assert_eq!(std::fs::read(&config).unwrap(), REMOTE_LOCK, "{held:?}");
            assert_eq!(remote_lock(metadata), Some(true));
            if held != REMOTE_LOCK {
                assert!(!partial.exists(), "{held:?}");
            }
        }
        let mut longer = REMOTE_LOCK.to_vec();
        longer.extend_from_slice(b"\n");
        let mut changed = REMOTE_LOCK[..REMOTE_LOCK.len() / 2].to_vec();
        changed[1] = b'x';
        let foreign: [&[u8]; 3] = [b"[core]\n\tfsmonitor = touch /tmp/x\n", &longer, &changed];
        for held in foreign {
            std::fs::write(&config, held).unwrap();
            assert_eq!(lock_state(metadata), LockState::Foreign, "{held:?}");
            assert_eq!(
                lock_metadata(metadata).unwrap_err().0,
                "worktree_config_present"
            );
            assert_eq!(std::fs::read(&config).unwrap(), held);
        }
        std::fs::remove_file(&config).unwrap();
        let elsewhere = metadata.join("elsewhere");
        std::fs::write(&elsewhere, &REMOTE_LOCK[..4]).unwrap();
        std::os::unix::fs::symlink(&elsewhere, &config).unwrap();
        assert_eq!(lock_state(metadata), LockState::Foreign);
        assert!(lock_metadata(metadata).is_err());
        assert_eq!(std::fs::read(&elsewhere).unwrap(), &REMOTE_LOCK[..4]);
        assert!(
            std::fs::symlink_metadata(&config)
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_worktree_config_switch_is_read_like_git_reads_it() {
        let cases: [(&[u8], bool); 10] = [
            (b"", false),
            (b"core.bare\nfalse\0", false),
            (b"extensions.worktreeconfig\ntrue\0", true),
            (b"extensions.worktreeConfig\nYes\0", true),
            (b"extensions.worktreeconfig\n1\0", true),
            (b"extensions.worktreeconfig\0", true),
            (b"extensions.worktreeconfig\nfalse\0", false),
            (
                b"extensions.worktreeconfig\ntrue\0extensions.worktreeconfig\noff\0",
                false,
            ),
            (
                b"extensions.worktreeconfig\nno\0extensions.worktreeconfig\non\0",
                true,
            ),
            (b"remote.origin.url\nextensions.worktreeconfig\0", false),
        ];
        for (listing, on) in cases {
            assert_eq!(
                worktree_config_on(listing),
                on,
                "{}",
                String::from_utf8_lossy(listing)
            );
        }
    }

    const WORKTREE_CONFIG_RACERS: usize = 8;
    const WORKTREE_CONFIG_ROUNDS: usize = 10;

    /// First-time setups of one repository that turn `extensions.worktreeConfig` on at the
    /// same moment all succeed: one that loses `.git/config.lock` to another reads the switch
    /// again and finds it on, so no lease or worktree fails on the race.
    #[cfg(unix)]
    #[test]
    fn first_time_worktree_config_writes_that_race_all_succeed() {
        for _ in 0..WORKTREE_CONFIG_ROUNDS {
            let temporary = tempfile::tempdir().unwrap();
            let repository = committed_repository(temporary.path());
            let barrier = std::sync::Arc::new(std::sync::Barrier::new(WORKTREE_CONFIG_RACERS));
            let racers: Vec<_> = (0..WORKTREE_CONFIG_RACERS)
                .map(|_| {
                    let (repository, barrier) = (repository.clone(), barrier.clone());
                    std::thread::spawn(move || {
                        barrier.wait();
                        turn_on_worktree_config(&repository)
                    })
                })
                .collect();
            for racer in racers {
                assert_eq!(racer.join().unwrap(), Ok(()));
            }
            let listing = git(
                &repository,
                &["config", "--local", "--no-includes", "--null", "--list"],
            )
            .unwrap();
            assert!(worktree_config_on(&listing));
        }
    }

    /// Appends tracked branch sections to a repository's config, as a repository with many
    /// branches holds them. Inputs: the repository and how many branches. Output: none; the
    /// config file grows by about 105 bytes a branch.
    #[cfg(unix)]
    fn track_branches(repository: &Path, count: usize) {
        use std::fmt::Write;
        let config = repository.join(".git/config");
        let mut text = std::fs::read_to_string(&config).unwrap();
        for branch in 0..count {
            let name = format!("feature/scholar-scope-{branch:04}");
            write!(
                text,
                "[branch \"{name}\"]\n\tremote = origin\n\tmerge = refs/heads/{name}\n"
            )
            .unwrap();
        }
        std::fs::write(&config, text).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_repository_whose_config_lists_more_than_64_kib_is_inspected() {
        let temporary = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(temporary.path()).unwrap();
        let repository = committed_repository(&root);
        track_branches(&repository, 1000);
        let listing = git(
            &repository,
            &["config", "--local", "--no-includes", "--null", "--list"],
        )
        .unwrap();
        assert!(listing.len() > 100_000);
        let admission = inspect(repository.to_str().unwrap()).unwrap();
        assert_eq!(Path::new(&admission.repository), repository);
    }

    #[cfg(unix)]
    #[test]
    fn a_refused_key_after_64_kib_of_config_is_still_refused() {
        let temporary = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(temporary.path()).unwrap();
        let repository = committed_repository(&root);
        track_branches(&repository, 1000);
        let config = repository.join(".git/config");
        let mut text = std::fs::read_to_string(&config).unwrap();
        text.push_str("[core]\n\tfsmonitor = /bin/true\n");
        std::fs::write(&config, text).unwrap();
        assert_eq!(
            inspect(repository.to_str().unwrap()).unwrap_err().0,
            "repository_config_refused:core_fsmonitor"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_config_listing_past_the_git_output_bound_is_refused() {
        let temporary = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(temporary.path()).unwrap();
        let repository = committed_repository(&root);
        let config = repository.join(".git/config");
        let mut text = std::fs::read_to_string(&config).unwrap();
        text.push_str("[user]\n\tname = ");
        text.push_str(&"n".repeat(OUTPUT_BYTES_MAX + 1));
        text.push('\n');
        std::fs::write(&config, text).unwrap();
        assert_eq!(
            inspect(repository.to_str().unwrap()).unwrap_err().0,
            "git_operation_failed_or_output_capacity"
        );
    }

    /// Fills a folder inside a repository's .git with empty files, as a long-used
    /// repository's `rr-cache` or `logs` holds them. Inputs: the repository, the folder's
    /// name and how many files. Output: none.
    #[cfg(unix)]
    fn fill_git_folder(repository: &Path, name: &str, count: usize) {
        let folder = repository.join(".git").join(name);
        std::fs::create_dir(&folder).unwrap();
        for entry in 0..count {
            std::fs::write(folder.join(format!("{entry:06}")), b"").unwrap();
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_repository_whose_git_folder_holds_more_than_8192_entries_is_inspected() {
        let temporary = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(temporary.path()).unwrap();
        let repository = committed_repository(&root);
        fill_git_folder(&repository, "rr-cache", 9000);
        let admission = inspect(repository.to_str().unwrap()).unwrap();
        assert_eq!(Path::new(&admission.repository), repository);
    }

    #[cfg(unix)]
    #[test]
    fn a_git_folder_past_the_metadata_bound_is_refused() {
        let temporary = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(temporary.path()).unwrap();
        let repository = committed_repository(&root);
        fill_git_folder(&repository, "rr-cache", METADATA_ENTRIES_MAX + 1);
        assert_eq!(
            inspect(repository.to_str().unwrap()).unwrap_err().0,
            "git_metadata_capacity"
        );
    }
}
