use crate::policy::{AgentAccess, ToolCacheScope};
use rustix::fs::{AtFlags, FileType, Mode, OFlags};
use rustix::io::Errno;
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub const FOLDER: &str = "tool-cache";
pub const UV: &str = "uv";
pub const PNPM: &str = "pnpm-store";
pub const MARKER: &str = ".git";
pub const MARKER_TARGET: &str = "/dev/null";
const BUCKETS_MAX: usize = 16;
const NAME_BYTES_MAX: usize = 64;
const RECORD_BYTES_MAX: u64 = 4096;
const PRIMING_TIME: Duration = Duration::from_secs(30);
const POLL: Duration = Duration::from_millis(20);

/// The count of priming records this process has written, which names each staging file.
static STAGING: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
const OPEN_FOLDER: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);

/// The names the priming process's environment holds, and no other: its own home and cache in the
/// priming folder, no uv config file, no network, the system PATH and the C locale.
pub const PRIMING_ENV: [&str; 6] = [
    "HOME",
    "UV_CACHE_DIR",
    "UV_NO_CONFIG",
    "UV_OFFLINE",
    "PATH",
    "LC_ALL",
];

/// Where a confined agent writes, which decides who may share its tool cache: the project folder,
/// which is the owner's checkout, a linked worktree, an own clone or a clone teammate. A tool can
/// link a cache file into the tree it installs into, so no cache is shared between a profile that
/// can write the owner's checkout and one that cannot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheShape {
    ProjectRoot,
    Worktree,
    Clone,
    Teammate,
}

impl CacheShape {
    /// Every shape.
    pub const ALL: [Self; 4] = [
        Self::ProjectRoot,
        Self::Worktree,
        Self::Clone,
        Self::Teammate,
    ];

    /// The shape's stable id, part of a shared cache's folder name.
    pub fn id(self) -> &'static str {
        match self {
            Self::ProjectRoot => "root",
            Self::Worktree => "worktree",
            Self::Clone => "clone",
            Self::Teammate => "teammate",
        }
    }
}

/// A tool cache ready for one launch: its folder, whether uv's markers are in place there,
/// so the launch sets `UV_CACHE_DIR`, and whether this preparation ran the priming process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prepared {
    pub root: PathBuf,
    pub uv: bool,
    pub primed: bool,
}

/// The folder name of one launch's tool cache inside its project's folder: none at Full
/// access, which gets no cache; under Shared the level and the shape; under One per agent
/// the level and the agent, so an agent's cache does not follow it to a wider level.
/// Inputs: the setting, the level, the shape and the agent's id. Output: a plain name, or
/// None.
pub fn key(
    scope: ToolCacheScope,
    access: AgentAccess,
    shape: CacheShape,
    agent: &str,
) -> Option<String> {
    let level = access.id().replace('_', "-");
    let key = match (access, scope) {
        (AgentAccess::Full, _) => return None,
        (_, ToolCacheScope::Shared) => format!("{level}-{}", shape.id()),
        (_, ToolCacheScope::PerAgent) => format!("{level}-agent-{}", digest(agent, 16)),
    };
    assert!(plain_name(&key));
    Some(key)
}

/// The folder holding one project's tool caches and their priming records,
/// `<runtime>/tool-cache/<digest of the project>`, the project named by its canonical folder
/// path, which the master and the team name alike. Inputs: the engine runtime folder and
/// the project. Output: the path.
pub fn project_folder(runtime: &Path, project: &str) -> PathBuf {
    runtime.join(FOLDER).join(digest(project, 32))
}

/// The first `length` hex digits of the SHA-256 of `text`, a plain folder name for an id of
/// any form. Inputs: the text and the length. Output: the digits.
fn digest(text: &str, length: usize) -> String {
    let hex: String = Sha256::digest(text.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    assert!(length <= hex.len());
    hex[..length].to_owned()
}

/// Tells whether a name is one the engine makes, or takes from uv, as a folder name: only
/// `a` to `z`, `0` to `9` and `-`, 1 to 64 bytes, so never `.`, `..` or `.git` in any case.
/// Input: the name. Output: the answer.
fn plain_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= NAME_BYTES_MAX
        && name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

/// Removes the tool caches One per agent gave an agent and its native teammates in one project, at
/// every level, with their priming records, when Clean up removes the agent's worktree, since a
/// removed agent never runs again. A shared cache stays, since other agents of its level use it. No
/// link is followed: a link in a cache's place is unlinked, the `/dev/null` markers are unlinked,
/// and a folder the agent closed or a file it pinned in its cache is opened and unpinned first. The
/// folders from the runtime down to the project's are opened one part at a time with no link
/// followed and accepted only as the engine's own, as [`prepare`] opens them, so a link or another
/// user's folder above the caches fails with its plain code and removes nothing. Inputs: the hooks,
/// the engine runtime, the project as [`project_folder`] names it and the agent's id. Output: Ok
/// once none is left, also when the folders were never made.
pub fn remove_agent(
    hooks: &ToolCacheHooks,
    runtime: &Path,
    project: &str,
    agent: &str,
) -> std::io::Result<()> {
    let base = match rustix::fs::open(runtime, OPEN_FOLDER, Mode::empty()) {
        Ok(base) => base,
        Err(Errno::NOENT) => return Ok(()),
        Err(_) => return Err(std::io::Error::other("tool_cache_link")),
    };
    let Some(tools) = existing_engine_folder(&base, FOLDER).map_err(std::io::Error::other)? else {
        return Ok(());
    };
    let Some(owned) =
        existing_engine_folder(&tools, &digest(project, 32)).map_err(std::io::Error::other)?
    else {
        return Ok(());
    };
    for own in [agent.to_owned(), format!("{agent}-teammates")] {
        for access in AgentAccess::ALL {
            let Some(key) = key(ToolCacheScope::PerAgent, access, CacheShape::Worktree, &own)
            else {
                continue;
            };
            for name in [format!("{key}.primed"), key] {
                match rustix::fs::statat(&owned, name.as_str(), AtFlags::SYMLINK_NOFOLLOW) {
                    Err(Errno::NOENT) => {}
                    Err(problem) => return Err(problem.into()),
                    Ok(found) if FileType::from_raw_mode(found.st_mode) == FileType::Directory => {
                        (hooks.remove_agent_entry)(
                            owned.as_fd(),
                            name.as_str(),
                            "cleanup_folder_failed",
                        )
                        .map_err(std::io::Error::other)?
                    }
                    Ok(_) => rustix::fs::unlinkat(&owned, name.as_str(), AtFlags::empty())?,
                }
                assert!(
                    rustix::fs::statat(&owned, name.as_str(), AtFlags::SYMLINK_NOFOLLOW).is_err()
                );
            }
        }
    }
    Ok(())
}

/// Makes, or finds, the tool cache `<project folder>/<key>` for one launch, with `uv` and
/// `pnpm-store` folders inside, and, when a uv binary is given, a `.git` link to exactly
/// `/dev/null` in each cache bucket uv makes a `.git` file in, so uv runs under the deny of `.git`
/// in every writable root that is not a working tree, with no exemption. Every folder is made and
/// opened one part at a time with no link followed, the engine's own as plain 0700 folders of this
/// user; a link or someone else's folder anywhere fails closed. The buckets come from a record kept
/// per cache and uv binary; with none, or after uv changed, [`prime`] finds them. A launch with a
/// record only checks each marker and makes one an agent's move of its bucket took away. Inputs:
/// the hooks, the engine runtime folder, the project as [`project_folder`] names it, the key and
/// the canonical uv binary, if any. Output: the prepared cache, or the plain code that refused it.
pub fn prepare(
    hooks: &ToolCacheHooks,
    runtime: &Path,
    project: &str,
    key: &str,
    uv: Option<&Path>,
) -> Result<Prepared, &'static str> {
    assert!(plain_name(key));
    let base =
        rustix::fs::open(runtime, OPEN_FOLDER, Mode::empty()).map_err(|_| "tool_cache_unmade")?;
    let tools = engine_folder(&base, FOLDER)?;
    let owned = engine_folder(&tools, &digest(project, 32))?;
    let cache = reopened(hooks, engine_folder(&owned, key)?)?;
    agent_folder(hooks, &cache, PNPM)?;
    let store = agent_folder(hooks, &cache, UV)?;
    let root = project_folder(runtime, project).join(key);
    let Some(uv) = uv else {
        return Ok(Prepared {
            root,
            uv: false,
            primed: false,
        });
    };
    let identity = identity(uv)?;
    let record = format!("{key}.primed");
    let (buckets, primed) = match read_record(&owned, &record) {
        Some((known, buckets)) if known == identity => (buckets, false),
        _ => {
            let buckets = prime(hooks, &tools, &runtime.join(FOLDER), uv)?;
            write_record(&owned, &record, &identity, &buckets)?;
            (buckets, true)
        }
    };
    assert!(buckets.len() <= BUCKETS_MAX);
    for bucket in &buckets {
        marker(&agent_folder(hooks, &store, bucket)?)?;
    }
    Ok(Prepared {
        root,
        uv: true,
        primed,
    })
}

/// Makes or opens one of the engine's own folders below `parent` with no link followed, and
/// accepts it only as a folder of this user that nobody else may enter. Inputs: the open
/// parent and the name. Output: the open folder, or the plain code that refused it.
fn engine_folder(parent: &OwnedFd, name: &str) -> Result<OwnedFd, &'static str> {
    engine_owned(open_folder(parent, name)?)
}

/// Opens one of the engine's own folders below `parent` that is already there, with no link
/// followed, by the rule [`engine_folder`] applies, and makes none. Inputs: the open parent
/// and a plain name. Output: the open folder, None when nothing has the name, else
/// `tool_cache_link`, `tool_cache_unmade` or `tool_cache_folder_refused`.
fn existing_engine_folder(parent: &OwnedFd, name: &str) -> Result<Option<OwnedFd>, &'static str> {
    assert!(plain_name(name), "{name}");
    match rustix::fs::openat(parent, name, OPEN_FOLDER, Mode::empty()) {
        Ok(folder) => engine_owned(folder).map(Some),
        Err(Errno::NOENT) => Ok(None),
        Err(_) => Err("tool_cache_link"),
    }
}

/// Accepts an open folder only as one of this user's that nobody else may enter. Input: the
/// open folder. Output: the folder, else `tool_cache_unmade` or `tool_cache_folder_refused`.
fn engine_owned(folder: OwnedFd) -> Result<OwnedFd, &'static str> {
    let stat = rustix::fs::fstat(&folder).map_err(|_| "tool_cache_unmade")?;
    if stat.st_uid != rustix::process::geteuid().as_raw() || u32::from(stat.st_mode) & 0o077 != 0 {
        return Err("tool_cache_folder_refused");
    }
    Ok(folder)
}

/// Makes or opens a folder below `parent` with no link followed, mode 0700 when made.
/// Inputs: the open parent and a plain name. Output: the open folder, else
/// `tool_cache_unmade` or `tool_cache_link`.
fn open_folder(parent: &OwnedFd, name: &str) -> Result<OwnedFd, &'static str> {
    assert!(plain_name(name), "{name}");
    match rustix::fs::mkdirat(parent, name, Mode::from_raw_mode(0o700)) {
        Ok(()) | Err(Errno::EXIST) => {}
        Err(_) => return Err("tool_cache_unmade"),
    }
    rustix::fs::openat(parent, name, OPEN_FOLDER, Mode::empty()).map_err(|_| "tool_cache_link")
}

/// Makes or opens a folder in the agent's cache, which the agent may have moved, emptied or remade.
/// A link or a file an agent left under the folder's name is unlinked, never followed, and a folder
/// of this user it closed gets owner rwx again, once, so no agent at a Shared key keeps every later
/// launch there from a cache. A file it pinned there is unpinned before the unlink, and the open
/// folder goes through [`reopened`]. Inputs: the open parent and the name. Output: the open folder,
/// or the plain code that refused it.
fn agent_folder(
    hooks: &ToolCacheHooks,
    parent: &OwnedFd,
    name: &str,
) -> Result<OwnedFd, &'static str> {
    match open_folder(parent, name) {
        Err("tool_cache_link") => {}
        opened => return reopened(hooks, opened?),
    }
    let found = rustix::fs::statat(parent, name, AtFlags::SYMLINK_NOFOLLOW)
        .map_err(|_| "tool_cache_link")?;
    let repaired = match FileType::from_raw_mode(found.st_mode) {
        FileType::Directory if found.st_uid == rustix::process::geteuid().as_raw() => {
            rustix::fs::chmodat(parent, name, Mode::RWXU, AtFlags::SYMLINK_NOFOLLOW)
        }
        FileType::Directory => return Err("tool_cache_link"),
        _ => {
            (hooks.unpin)(parent.as_fd(), name);
            rustix::fs::unlinkat(parent, name, AtFlags::empty())
        }
    };
    repaired.map_err(|_| "tool_cache_link")?;
    reopened(hooks, open_folder(parent, name)?)
}

/// Gives an open folder of the cache agents write back to its owner: clears the immutable and
/// append-only flags an agent may set on it and owner rwx it took away, so no agent at a Shared key
/// keeps every later launch there from making what it needs inside. A folder of another user keeps
/// its mode. A link pinned in a folder's place, or an entry pinned at mode 000, cannot be unpinned
/// safely and still refuses the cache. Input: the open folder. Output: the folder, or
/// `tool_cache_unmade`.
fn reopened(hooks: &ToolCacheHooks, folder: OwnedFd) -> Result<OwnedFd, &'static str> {
    (hooks.unpin)(folder.as_fd(), ".");
    let stat = rustix::fs::fstat(&folder).map_err(|_| "tool_cache_unmade")?;
    if stat.st_uid == rustix::process::geteuid().as_raw()
        && u32::from(stat.st_mode) & 0o700 != 0o700
    {
        rustix::fs::fchmod(&folder, Mode::RWXU).map_err(|_| "tool_cache_unmade")?;
    }
    Ok(folder)
}

/// Puts the uv marker in one cache bucket: makes `.git` as a link to exactly `/dev/null`, or finds
/// one an earlier or a parallel launch made, and then accepts what is there only as that link, so
/// launches racing on one Shared key all get the marker. A file, a folder or a link to anything
/// else there fails closed. Input: the open bucket. Output: Ok, else `tool_cache_marker_unexpected`
/// or `tool_cache_marker_unmade`.
fn marker(bucket: &OwnedFd) -> Result<(), &'static str> {
    match rustix::fs::symlinkat(MARKER_TARGET, bucket, MARKER) {
        Ok(()) | Err(Errno::EXIST) => {}
        Err(_) => return Err("tool_cache_marker_unmade"),
    }
    let target = rustix::fs::readlinkat(bucket, MARKER, Vec::new())
        .map_err(|_| "tool_cache_marker_unexpected")?;
    if target.as_bytes() == MARKER_TARGET.as_bytes() {
        Ok(())
    } else {
        Err("tool_cache_marker_unexpected")
    }
}

/// The identity of a uv binary a priming record holds: its device, inode, size, modification time
/// and change time, so a replaced or upgraded uv primes again, even one rewritten in place or given
/// a freed inode with its old size and modification time. Input: the canonical binary. Output: the
/// identity line, or `tool_cache_uv_unread`.
fn identity(uv: &Path) -> Result<String, &'static str> {
    let metadata = std::fs::metadata(uv).map_err(|_| "tool_cache_uv_unread")?;
    Ok(format!(
        "{} {} {} {}.{} {}.{}",
        metadata.dev(),
        metadata.ino(),
        metadata.len(),
        metadata.mtime(),
        metadata.mtime_nsec(),
        metadata.ctime(),
        metadata.ctime_nsec()
    ))
}

/// Reads a priming record, a regular file in the engine's project folder: the uv identity
/// on the first line and one bucket name per line after it. A record that is missing, a
/// link, too long or holds a name [`plain_name`] refuses or too many reads as none, so the
/// launch primes again. Inputs: the open project folder and the record's name. Output: the
/// identity and the buckets, or None.
fn read_record(owned: &OwnedFd, record: &str) -> Option<(String, Vec<String>)> {
    let file = rustix::fs::openat(
        owned,
        record,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .ok()?;
    let file = std::fs::File::from(file);
    if !file.metadata().ok()?.is_file() {
        return None;
    }
    let mut text = String::new();
    file.take(RECORD_BYTES_MAX + 1)
        .read_to_string(&mut text)
        .ok()?;
    if text.len() as u64 > RECORD_BYTES_MAX {
        return None;
    }
    let mut lines = text.lines();
    let identity = lines.next()?.to_owned();
    let buckets: Vec<String> = lines.map(str::to_owned).collect();
    (buckets.len() <= BUCKETS_MAX && buckets.iter().all(|name| plain_name(name)))
        .then_some((identity, buckets))
}

/// Writes a priming record in place of the last one, through a staging file of this writer
/// alone, named by this process and a count of its writes, so launches priming one cache at
/// once never share one. Inputs: the open project folder, the record's name, the uv
/// identity and the buckets. Output: Ok, or `tool_cache_record_unwritten`.
fn write_record(
    owned: &OwnedFd,
    record: &str,
    identity: &str,
    buckets: &[String],
) -> Result<(), &'static str> {
    let count = STAGING.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let side = format!("{record}.{}.{count}.new", std::process::id());
    let mut text = format!("{identity}\n");
    for bucket in buckets {
        text.push_str(bucket);
        text.push('\n');
    }
    publish(owned, record, &side, text.as_bytes())
}

/// Puts a record in place through a staging file made with exclusive creation, so no other
/// writer opens it; one already there can only be left by a dead process with this pid and
/// is removed once. The rename comes after the whole text, and any failure removes the
/// staging file. Inputs: the folder, the record, the staging name and the text. Output: Ok,
/// or `tool_cache_record_unwritten`.
fn publish(owned: &OwnedFd, record: &str, side: &str, text: &[u8]) -> Result<(), &'static str> {
    assert!(side.ends_with(".new") && side != record);
    let unwritten = |_| "tool_cache_record_unwritten";
    let flags = OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let mode = Mode::from_raw_mode(0o600);
    let file = match rustix::fs::openat(owned, side, flags, mode) {
        Err(Errno::EXIST) => {
            rustix::fs::unlinkat(owned, side, AtFlags::empty()).map_err(unwritten)?;
            rustix::fs::openat(owned, side, flags, mode)
        }
        opened => opened,
    }
    .map_err(unwritten)?;
    let published = std::fs::File::from(file)
        .write_all(text)
        .map_err(|_| "tool_cache_record_unwritten")
        .and_then(|()| rustix::fs::renameat(owned, side, owned, record).map_err(unwritten));
    if published.is_err() {
        let _ = rustix::fs::unlinkat(owned, side, AtFlags::empty());
    }
    published
}

/// Finds the cache buckets this uv makes a `.git` file in by running it once in a fresh folder
/// under the priming profile ([`ToolCacheHooks::priming_command`]), then removes the folder.
/// Nothing an agent wrote is read, and no regular `.git` file is ever made anywhere else. Inputs:
/// the engine's open tool cache folder, its path and the canonical uv binary. Output: the bucket
/// names, sorted, or the plain code that refused them.
fn prime(
    hooks: &ToolCacheHooks,
    tools: &OwnedFd,
    folder: &Path,
    uv: &Path,
) -> Result<Vec<String>, &'static str> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.subsec_nanos());
    let name = format!(".priming-{}-{nanos}", std::process::id());
    rustix::fs::mkdirat(tools, name.as_str(), Mode::from_raw_mode(0o700))
        .map_err(|_| "tool_cache_priming_failed")?;
    let priming = folder.join(&name);
    let found = run_priming(hooks, &priming, uv).and_then(|()| buckets_in(&priming.join("cache")));
    let removed = std::fs::remove_dir_all(&priming);
    let found = found?;
    removed.map_err(|_| "tool_cache_priming_failed")?;
    Ok(found)
}

/// Runs the priming process until it ends or [`PRIMING_TIME`] passes, then ends its whole
/// process group. Inputs: the priming folder and the uv binary. Output: Ok when uv ended
/// with success, else `tool_cache_priming_failed`.
fn run_priming(hooks: &ToolCacheHooks, folder: &Path, uv: &Path) -> Result<(), &'static str> {
    let failed = |_| "tool_cache_priming_failed";
    std::fs::create_dir(folder.join("home")).map_err(failed)?;
    let mut command = (hooks.priming_command)(folder, uv);
    let mut child = kernel_processes::spawn_gate::spawn(&mut command).map_err(failed)?;
    let until = Instant::now() + PRIMING_TIME;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if Instant::now() < until => std::thread::sleep(POLL),
            _ => break None,
        }
    };
    stop_group(child.id());
    if status.is_none() {
        let _ = child.kill();
        let _ = child.wait();
    }
    match status {
        Some(status) if status.success() => Ok(()),
        _ => Err("tool_cache_priming_failed"),
    }
}

/// Reads the buckets uv made in a fresh cache: each folder holding an empty regular `.git` file.
/// Every name must pass [`plain_name`] and there may be at most [`BUCKETS_MAX`], else the whole
/// cache is refused, so a swapped uv cannot name a bucket `.git` or `..`. Input: the fresh cache.
/// Output: the names, sorted, or `tool_cache_bucket_refused`.
fn buckets_in(cache: &Path) -> Result<Vec<String>, &'static str> {
    let refused = |_| "tool_cache_bucket_refused";
    let mut found = Vec::new();
    for entry in std::fs::read_dir(cache).map_err(refused)? {
        let entry = entry.map_err(refused)?;
        if !entry.file_type().map_err(refused)?.is_dir() {
            continue;
        }
        let marked = std::fs::symlink_metadata(entry.path().join(MARKER))
            .is_ok_and(|marker| marker.is_file() && marker.len() == 0);
        if !marked {
            continue;
        }
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| "tool_cache_bucket_refused")?;
        if !plain_name(&name) || found.len() == BUCKETS_MAX {
            return Err("tool_cache_bucket_refused");
        }
        found.push(name);
    }
    found.sort();
    Ok(found)
}

/// Sends SIGKILL to the priming process's group, which it leads, so nothing it started
/// outlives it; a group already gone is fine. Input: the group leader's id.
fn stop_group(id: u32) {
    let Some(pid) = i32::try_from(id)
        .ok()
        .and_then(rustix::process::Pid::from_raw)
    else {
        return;
    };
    assert!(pid.as_raw_nonzero().get() > 1);
    let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
}

/// What the tool cache asks of the engine that hosts it: clearing an agent's pinning flags,
/// removing a folder an agent wrote and building the priming process. The engine passes one
/// value; rows pass stand-ins.
#[derive(Debug)]
pub struct ToolCacheHooks {
    /// Clears the immutable and append-only flags an agent may set on an entry, never
    /// following a link. Inputs: the open parent and the entry's name. Output: none.
    pub unpin: fn(std::os::fd::BorrowedFd<'_>, &str),
    /// Removes the entry `name` of an open folder with all it holds, each step relative to an
    /// open folder handle and none following a link, also a folder an agent shut. Inputs:
    /// the open parent, the entry name and the code for a refusal the engine has no own code
    /// for. Output: done once nothing has the name, or the refusal code.
    pub remove_agent_entry:
        fn(std::os::fd::BorrowedFd<'_>, &str, &'static str) -> Result<(), &'static str>,
    /// Builds the unspawned priming process. Inputs: the priming folder and the canonical uv
    /// binary. Output: the command.
    pub priming_command: fn(&std::path::Path, &std::path::Path) -> std::process::Command,
}

#[cfg(test)]
mod tests;
