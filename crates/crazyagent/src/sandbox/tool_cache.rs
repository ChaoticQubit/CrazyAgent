use super::ProjectSandbox;
use super::policy::{AgentAccess, AgentPolicy, ToolCacheScope};
use rustix::fs::{AtFlags, FileType, Mode, OFlags};
use rustix::io::Errno;
use sha2::{Digest, Sha256};
use std::ffi::OsStr;
use std::io::{Read, Write};
use std::os::fd::OwnedFd;
use std::os::unix::fs::MetadataExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const FOLDER: &str = "tool-cache";
const UV: &str = "uv";
const PNPM: &str = "pnpm-store";
const MARKER: &str = ".git";
const MARKER_TARGET: &str = "/dev/null";
const BUCKETS_MAX: usize = 16;
const NAME_BYTES_MAX: usize = 64;
const RECORD_BYTES_MAX: u64 = 4096;
const PRIMING_TIME: Duration = Duration::from_secs(30);
const POLL: Duration = Duration::from_millis(20);
const OPEN_FOLDER: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);

/// The names the priming process's environment holds, and no other (T8 C3): its own home
/// and cache in the priming folder, no uv config file, no network, the system PATH and the
/// C locale.
pub const PRIMING_ENV: [&str; 6] = [
    "HOME",
    "UV_CACHE_DIR",
    "UV_NO_CONFIG",
    "UV_OFFLINE",
    "PATH",
    "LC_ALL",
];

/// Where a confined agent writes, which decides who may share its tool cache (T8 C2): the
/// project folder, which is the owner's checkout, a linked worktree, an own clone or a clone
/// teammate. A tool can link a cache file into the tree it installs into, so no cache is
/// shared between a profile that can write the owner's checkout and one that cannot.
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

/// Gives a launch's sandbox the tool cache its project's policy grants (T8): the cache
/// [`key`] names under the engine runtime for the project, prepared with the uv the agent's
/// PATH finds when the engine may run it. A refusal is logged with its code and the sandbox
/// goes on without a cache, so uv keeps its own default, which the SM1-R rule refuses
/// (fail closed). Inputs: the sandbox, the engine runtime, the canonical project folder, the
/// policy, the shape and the agent. Output: the sandbox, with the cache when granted.
pub fn grant(
    sandbox: ProjectSandbox,
    runtime: &Path,
    project: &Path,
    policy: &AgentPolicy,
    shape: CacheShape,
    agent: &str,
) -> ProjectSandbox {
    let Some(key) = key(policy.tool_cache, policy.access, shape, agent) else {
        return sandbox;
    };
    let Some(project) = project.to_str() else {
        eprintln!("agent_tool_cache_unavailable code=tool_cache_project_name");
        return sandbox;
    };
    let uv = sandbox.tool_program(UV);
    let prepared = match prepare(runtime, project, &key, uv.as_deref()) {
        Ok(prepared) => prepared,
        Err(code) => {
            eprintln!("agent_tool_cache_unavailable code={code}");
            return sandbox;
        }
    };
    match sandbox
        .clone()
        .allow_tool_cache(&prepared.root, prepared.uv)
    {
        Ok(granted) => granted,
        Err(_) => {
            eprintln!("agent_tool_cache_unavailable code=tool_cache_root_refused");
            sandbox
        }
    }
}

/// Removes the tool caches One per agent gave an agent and its native teammates in one
/// project, at every level, with their priming records (T8, DEC-123), when the agent's files
/// go. A shared cache stays, since other agents of its level use it. No link is followed: a
/// link in a cache's place is unlinked, and `remove_dir_all` unlinks the `/dev/null` markers.
/// Inputs: the engine runtime, the project as [`project_folder`] names it and the agent's
/// id. Output: Ok once none is left.
pub fn remove_agent(runtime: &Path, project: &str, agent: &str) -> std::io::Result<()> {
    let folder = project_folder(runtime, project);
    for own in [agent.to_owned(), format!("{agent}-teammates")] {
        for access in AgentAccess::ALL {
            let Some(key) = key(ToolCacheScope::PerAgent, access, CacheShape::Worktree, &own)
            else {
                continue;
            };
            for name in [format!("{key}.primed"), key] {
                let path = folder.join(name);
                match std::fs::symlink_metadata(&path) {
                    Err(problem) if problem.kind() == std::io::ErrorKind::NotFound => {}
                    Err(problem) => return Err(problem),
                    Ok(found) if found.is_dir() => std::fs::remove_dir_all(&path)?,
                    Ok(_) => std::fs::remove_file(&path)?,
                }
                assert!(std::fs::symlink_metadata(&path).is_err());
            }
        }
    }
    Ok(())
}

/// Makes, or finds, the tool cache `<project folder>/<key>` for one launch, with `uv` and
/// `pnpm-store` folders inside, and, when a uv binary is given, a `.git` link to exactly
/// `/dev/null` in each cache bucket uv makes a `.git` file in, so uv runs under the SM1-R
/// rule with no `.git` exemption (T8). Every folder is made and opened one part at a time
/// with no link followed, the engine's own as plain 0700 folders of this user (C3); a link
/// or someone else's folder anywhere fails closed. The buckets come from a record kept per
/// cache and uv binary (C4); with none, or after uv changed, [`prime`] finds them. A launch
/// with a record only checks each marker and makes one an agent's move of its bucket took
/// away. Inputs: the engine runtime folder, the project as [`project_folder`] names it, the
/// key and the canonical uv binary, if any. Output: the prepared cache, or the plain code
/// that refused it.
pub fn prepare(
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
    let cache = engine_folder(&owned, key)?;
    agent_folder(&cache, PNPM)?;
    let store = agent_folder(&cache, UV)?;
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
            let buckets = prime(&tools, &runtime.join(FOLDER), uv)?;
            write_record(&owned, &record, &identity, &buckets)?;
            (buckets, true)
        }
    };
    assert!(buckets.len() <= BUCKETS_MAX);
    for bucket in &buckets {
        marker(&agent_folder(&store, bucket)?)?;
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
    let folder = agent_folder(parent, name)?;
    let stat = rustix::fs::fstat(&folder).map_err(|_| "tool_cache_unmade")?;
    if stat.st_uid != rustix::process::geteuid().as_raw() || u32::from(stat.st_mode) & 0o077 != 0 {
        return Err("tool_cache_folder_refused");
    }
    Ok(folder)
}

/// Makes or opens a folder below `parent` with no link followed, mode 0700 when made: a
/// folder in the agent's cache, which the agent may have moved, emptied or remade. Inputs:
/// the open parent and the name. Output: the open folder, or the plain code that refused
/// it.
fn agent_folder(parent: &OwnedFd, name: &str) -> Result<OwnedFd, &'static str> {
    match rustix::fs::mkdirat(parent, name, Mode::from_raw_mode(0o700)) {
        Ok(()) | Err(Errno::EXIST) => {}
        Err(_) => return Err("tool_cache_unmade"),
    }
    rustix::fs::openat(parent, name, OPEN_FOLDER, Mode::empty()).map_err(|_| "tool_cache_link")
}

/// Puts the uv marker in one cache bucket: makes `.git` as a link to exactly `/dev/null`
/// when missing and accepts one that is already that link. A file, a folder or a link to
/// anything else there fails closed. Input: the open bucket. Output: Ok, else
/// `tool_cache_marker_unexpected` or `tool_cache_marker_unmade`.
fn marker(bucket: &OwnedFd) -> Result<(), &'static str> {
    match rustix::fs::statat(bucket, MARKER, AtFlags::SYMLINK_NOFOLLOW) {
        Err(Errno::NOENT) => rustix::fs::symlinkat(MARKER_TARGET, bucket, MARKER)
            .map_err(|_| "tool_cache_marker_unmade"),
        Ok(stat) if FileType::from_raw_mode(stat.st_mode) == FileType::Symlink => {
            let target = rustix::fs::readlinkat(bucket, MARKER, Vec::new())
                .map_err(|_| "tool_cache_marker_unexpected")?;
            if target.as_bytes() == MARKER_TARGET.as_bytes() {
                Ok(())
            } else {
                Err("tool_cache_marker_unexpected")
            }
        }
        _ => Err("tool_cache_marker_unexpected"),
    }
}

/// The identity of a uv binary a priming record holds: its device, inode, size and
/// modification time, so a replaced or upgraded uv primes again (C4). Input: the canonical binary.
/// Output: the identity line, or `tool_cache_uv_unread`.
fn identity(uv: &Path) -> Result<String, &'static str> {
    let metadata = std::fs::metadata(uv).map_err(|_| "tool_cache_uv_unread")?;
    Ok(format!(
        "{} {} {} {}.{}",
        metadata.dev(),
        metadata.ino(),
        metadata.len(),
        metadata.mtime(),
        metadata.mtime_nsec()
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

/// Writes a priming record in place of the last one, through a side file renamed over it.
/// Inputs: the open project folder, the record's name, the uv identity and the buckets.
/// Output: Ok, or `tool_cache_record_unwritten`.
fn write_record(
    owned: &OwnedFd,
    record: &str,
    identity: &str,
    buckets: &[String],
) -> Result<(), &'static str> {
    let unwritten = |_| "tool_cache_record_unwritten";
    let side = format!("{record}.new");
    let mut text = format!("{identity}\n");
    for bucket in buckets {
        text.push_str(bucket);
        text.push('\n');
    }
    let file = rustix::fs::openat(
        owned,
        side.as_str(),
        OFlags::WRONLY | OFlags::CREATE | OFlags::TRUNC | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::from_raw_mode(0o600),
    )
    .map_err(unwritten)?;
    std::fs::File::from(file)
        .write_all(text.as_bytes())
        .map_err(|_| "tool_cache_record_unwritten")?;
    rustix::fs::renameat(owned, side.as_str(), owned, record).map_err(unwritten)
}

/// Finds the cache buckets this uv makes a `.git` file in by running it once in a fresh
/// folder under the priming profile ([`priming_command`]), then removes the folder. Nothing
/// an agent wrote is read, and no regular `.git` file is ever made anywhere else (C3).
/// Inputs: the engine's open tool cache folder, its path and the canonical uv binary.
/// Output: the bucket names, sorted, or the plain code that refused them.
fn prime(tools: &OwnedFd, folder: &Path, uv: &Path) -> Result<Vec<String>, &'static str> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.subsec_nanos());
    let name = format!(".priming-{}-{nanos}", std::process::id());
    rustix::fs::mkdirat(tools, name.as_str(), Mode::from_raw_mode(0o700))
        .map_err(|_| "tool_cache_priming_failed")?;
    let priming = folder.join(&name);
    let found = run_priming(&priming, uv).and_then(|()| buckets_in(&priming.join("cache")));
    let removed = std::fs::remove_dir_all(&priming);
    let found = found?;
    removed.map_err(|_| "tool_cache_priming_failed")?;
    Ok(found)
}

/// Runs the priming process until it ends or [`PRIMING_TIME`] passes, then ends its whole
/// process group. Inputs: the priming folder and the uv binary. Output: Ok when uv ended
/// with success, else `tool_cache_priming_failed`.
fn run_priming(folder: &Path, uv: &Path) -> Result<(), &'static str> {
    let failed = |_| "tool_cache_priming_failed";
    std::fs::create_dir(folder.join("home")).map_err(failed)?;
    let mut command = priming_command(folder, uv);
    let mut child = crate::spawn_gate::spawn(&mut command).map_err(failed)?;
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

/// Builds the priming process: uv's `tool list`, which makes the cache and its markers and
/// nothing more, under [`super::priming_profile`], from a cleared environment holding only
/// [`PRIMING_ENV`], in its own process group with no input or output. Inputs: the priming
/// folder and the canonical uv binary. Output: the unspawned command.
pub fn priming_command(folder: &Path, uv: &Path) -> Command {
    let mut command = Command::new(super::SANDBOX_EXEC);
    command
        .arg("-p")
        .arg(super::priming_profile(folder, uv))
        .arg(uv)
        .args(["tool", "list"])
        .env_clear()
        .env("HOME", folder.join("home"))
        .env("UV_CACHE_DIR", folder.join("cache"))
        .env("UV_NO_CONFIG", "1")
        .env("UV_OFFLINE", "1")
        .env("PATH", "/usr/bin:/bin")
        .env("LC_ALL", "C")
        .current_dir(folder)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0);
    let mut names: Vec<&OsStr> = command.get_envs().map(|(name, _)| name).collect();
    names.sort();
    let mut listed: Vec<&OsStr> = PRIMING_ENV.iter().map(OsStr::new).collect();
    listed.sort();
    assert_eq!(names, listed);
    command
}

/// Reads the buckets uv made in a fresh cache: each folder holding an empty regular `.git`
/// file. Every name must pass [`plain_name`] and there may be at most [`BUCKETS_MAX`], else
/// the whole cache is refused, so a swapped uv cannot name a bucket `.git` or `..` (C3).
/// Input: the fresh cache. Output: the names, sorted, or `tool_cache_bucket_refused`.
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

#[cfg(test)]
mod tests;
