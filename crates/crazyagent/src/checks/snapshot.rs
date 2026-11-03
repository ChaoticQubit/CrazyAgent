use super::CheckRefusal;
use crate::engine::owner::failure;
use crate::landing::ChangeSetId;
use crate::protocol::ProjectId;
use rusqlite::Connection;
#[cfg(unix)]
use rusqlite::{OptionalExtension, params};
use sha2::{Digest, Sha256};
#[cfg(unix)]
use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// The most bytes of a run key.
pub const RUN_KEY_BYTES_MAX: usize = 64;

#[cfg(unix)]
const GIT: &str = "/usr/bin/git";
#[cfg(unix)]
const GIT_STEP_TIME: std::time::Duration = std::time::Duration::from_secs(300);
#[cfg(unix)]
const POLL: std::time::Duration = std::time::Duration::from_millis(5);
#[cfg(unix)]
const LISTING_BYTES_MAX: usize = 64 * 1024 * 1024;
#[cfg(unix)]
const SMALL_BYTES_MAX: usize = 4096;
const PATH_BYTES_MAX: usize = 256;
const KEPT_MARK: &str = "kept";
const SPENT_MARK: &str = "holdout-ran";
/// The refusal for any run on a snapshot a holdout check ran in.
pub const SPENT: &str = "checks_snapshot_spent";
const UNAVAILABLE: &str = "checks_unavailable";
#[cfg(unix)]
const SNAPSHOT_FAILED: &str = "checks_snapshot_failed";

/// The settings every snapshot git call carries on top of the research runner's
/// `GIT_SAFE`, as engine landing git does: no attributes file from outside the snapshot,
/// no automatic upkeep, the case-folding path checks on, no signing, no untracked or split
/// index caches and no reflog.
#[cfg(unix)]
const SNAPSHOT_SETTINGS: [&str; 18] = [
    "-c",
    "core.attributesFile=/dev/null",
    "-c",
    "gc.auto=0",
    "-c",
    "maintenance.auto=false",
    "-c",
    "core.protectHFS=true",
    "-c",
    "core.protectNTFS=true",
    "-c",
    "commit.gpgSign=false",
    "-c",
    "core.untrackedCache=false",
    "-c",
    "core.splitIndex=false",
    "-c",
    "core.logAllRefUpdates=false",
];

/// The key of one check run, which names its snapshot folder: 1 to 64 ASCII letters,
/// digits and `-`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RunKey(String);

impl RunKey {
    /// Checks a run key. Input: the text. Output: the key, or `checks_run_invalid`.
    pub fn new(value: impl Into<String>) -> Result<Self, CheckRefusal> {
        let value = value.into();
        if value.is_empty()
            || value.len() > RUN_KEY_BYTES_MAX
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        {
            return Err(failure("checks_run_invalid"));
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// One change set's head checked out by the engine for one run: `root` is the work tree,
/// with its own one-commit git folder, `commit`, `tree` and `base` the ids it binds to.
/// The run folder around it holds the run's home, temp folder and sandbox profile.
#[derive(Debug)]
pub struct Snapshot {
    pub root: PathBuf,
    pub commit: String,
    pub tree: String,
    pub base: String,
    folder: PathBuf,
}

/// Holdout files copied into a snapshot for one run; dropping it removes them and the
/// folders made for them, never through a link a check left in their place.
#[derive(Debug)]
pub struct Placed {
    root: PathBuf,
    files: Vec<PathBuf>,
    folders: Vec<PathBuf>,
}

/// The change set row a snapshot is made from.
#[cfg(unix)]
struct Source {
    project: ProjectId,
    repository: PathBuf,
    git_dir: PathBuf,
    base: String,
    head: String,
}

/// The folder name of one project's snapshots under the checks folder: 16 hex digits of
/// the SHA-256 of its id, since a project id may hold `.` and `:`. Input: the project.
/// Output: the name.
pub fn project_folder(project: &ProjectId) -> String {
    let digest = Sha256::digest(project.as_str().as_bytes());
    let name: String = digest[..8]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    assert_eq!(name.len(), 16);
    name
}

/// Checks out a change set's head for one run into `<checks>/<project>/<run>/worktrees/src`
/// as a self-contained one-commit repository: a fresh git folder borrows the agent's and the
/// owner's object stores only for the checkout, every object is then copied in and
/// verified and the borrow removed. No git step reads the repository's or the agent's
/// config, hooks or attribute drivers, so a hostile filter, fsmonitor or hooks path has
/// nothing to run; a tree holding a link or past the engine's tree bounds is refused. The
/// agent worktree and the owner checkout are only read. Inputs: the engine connection, the
/// engine's checks folder, the change set and the run. Output: the snapshot, or
/// `checks_change_set_unknown`, `checks_change_set_discarded`, `checks_run_exists`,
/// `checks_snapshot_link`, `checks_snapshot_too_large`, `checks_snapshot_store_invalid`,
/// `checks_snapshot_failed`, a git metadata refusal or `checks_unavailable`; a refused run
/// leaves no folder.
#[cfg(unix)]
pub fn prepare(
    db: &Connection,
    checks: &Path,
    change_set: &ChangeSetId,
    run: &RunKey,
) -> Result<Snapshot, CheckRefusal> {
    let source = source(db, change_set)?;
    crate::worktrees::validate_metadata(&source.repository)?;
    let stores = stores(&source)?;
    private_folder(checks, true)?;
    let checks = checks.canonicalize().map_err(|_| failure(UNAVAILABLE))?;
    let project = checks.join(project_folder(&source.project));
    private_folder(&project, true)?;
    let folder = project.join(run.as_str());
    match private_folder(&folder, false) {
        Err(_) if fs::symlink_metadata(&folder).is_ok() => {
            return Err(failure("checks_run_exists"));
        }
        made => made?,
    }
    let made = check_out(&folder, &source, &stores);
    if made.is_err() {
        let _ = fs::remove_dir_all(&folder);
    }
    let snapshot = made?;
    assert!(snapshot.root.starts_with(&folder) && snapshot.folder == folder);
    assert_eq!(snapshot.commit, source.head);
    Ok(snapshot)
}

/// Refuses a snapshot where the engine's git is missing. Takes the connection, the checks
/// folder, the change set and the run, all unused. Returns `checks_unavailable`.
#[cfg(not(unix))]
pub fn prepare(
    db: &Connection,
    checks: &Path,
    change_set: &ChangeSetId,
    run: &RunKey,
) -> Result<Snapshot, CheckRefusal> {
    let _ = (db, checks, change_set, run);
    Err(failure(UNAVAILABLE))
}

impl Snapshot {
    /// The run folder around the snapshot: the sandbox project root of its checks, holding
    /// `worktrees/src`, the run's home and temp folder and its profile.
    pub fn folder(&self) -> &Path {
        &self.folder
    }

    /// Whether a holdout check was placed in this snapshot, which ends its use: repository
    /// code that ran beside the holdout files could have copied them into the snapshot.
    /// Output: true once marked, and for anything at the mark's path or a path that cannot
    /// be read.
    pub fn spent(&self) -> bool {
        !matches!(
            fs::symlink_metadata(self.folder.join(SPENT_MARK)),
            Err(error) if error.kind() == io::ErrorKind::NotFound
        )
    }

    /// Copies holdout files into the snapshot for one run, refusing an unsafe path, a path
    /// the change set already holds and any link on the way. Any file first marks the
    /// snapshot spent in its run folder, which the check cannot write. Input: each relative
    /// path with its bytes. Output: the placed files, removed when dropped, or
    /// `checks_snapshot_spent`, `checks_holdout_invalid`, `checks_holdout_collides` or
    /// `checks_unavailable`.
    #[cfg(unix)]
    pub fn place(&self, files: &BTreeMap<String, Vec<u8>>) -> Result<Placed, CheckRefusal> {
        use std::io::Write;
        use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
        if !files.is_empty() {
            fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
                .open(self.folder.join(SPENT_MARK))
                .and_then(|file| file.sync_all())
                .map_err(|error| match error.kind() {
                    io::ErrorKind::AlreadyExists => failure(SPENT),
                    _ => failure(UNAVAILABLE),
                })?;
        }
        assert!(files.is_empty() || self.spent());
        let mut placed = Placed {
            root: self.root.clone(),
            files: Vec::new(),
            folders: Vec::new(),
        };
        for (path, bytes) in files {
            if !safe_path(path) {
                return Err(failure("checks_holdout_invalid"));
            }
            let parts: Vec<&str> = path.split('/').collect();
            let mut at = self.root.clone();
            for part in &parts[..parts.len() - 1] {
                at.push(part);
                match fs::symlink_metadata(&at) {
                    Ok(found) if found.is_dir() && !found.file_type().is_symlink() => {}
                    Ok(_) => return Err(failure("checks_holdout_collides")),
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {
                        fs::DirBuilder::new()
                            .mode(0o700)
                            .create(&at)
                            .map_err(|_| failure(UNAVAILABLE))?;
                        placed.folders.push(at.clone());
                    }
                    Err(_) => return Err(failure(UNAVAILABLE)),
                }
            }
            at.push(parts[parts.len() - 1]);
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
                .open(&at)
                .map_err(|error| match error.kind() {
                    io::ErrorKind::AlreadyExists => failure("checks_holdout_collides"),
                    _ => failure(UNAVAILABLE),
                })?;
            placed.files.push(at.clone());
            file.write_all(bytes)
                .and_then(|()| file.sync_all())
                .map_err(|_| failure(UNAVAILABLE))?;
        }
        assert_eq!(placed.files.len(), files.len());
        Ok(placed)
    }

    /// Deletes the run folder with the snapshot, its home and temp folder and its profile.
    /// Output: Ok once gone, or the disk error.
    pub fn remove(self) -> io::Result<()> {
        assert!(self.root.starts_with(&self.folder));
        fs::remove_dir_all(&self.folder)
    }

    /// Keeps a failed run's folder for the owner, then removes the oldest kept folders of
    /// the project past `keep_failed`; 0 removes this one at once. Input: the
    /// `keep_failed_snapshots` setting. Output: Ok, or the disk error.
    pub fn keep(self, keep_failed: u8) -> io::Result<()> {
        if keep_failed == 0 {
            return self.remove();
        }
        fs::write(self.folder.join(KEPT_MARK), b"")?;
        let project = self
            .folder
            .parent()
            .ok_or_else(|| io::Error::other("snapshot folder has no project folder"))?;
        let mut kept = Vec::new();
        for entry in fs::read_dir(project)? {
            let run = entry?.path();
            let mark = run.join(KEPT_MARK);
            if let Ok(found) = fs::symlink_metadata(&mark)
                && found.is_file()
            {
                kept.push((found.modified()?, run));
            }
        }
        kept.sort();
        let excess = kept.len().saturating_sub(usize::from(keep_failed));
        for (_, run) in kept.into_iter().take(excess) {
            fs::remove_dir_all(run)?;
        }
        Ok(())
    }
}

impl Drop for Placed {
    fn drop(&mut self) {
        for file in self.files.iter().rev() {
            if plain_parents(&self.root, file) {
                let _ = fs::remove_file(file);
            }
        }
        for folder in self.folders.iter().rev() {
            if !plain_parents(&self.root, folder) {
                continue;
            }
            match fs::symlink_metadata(folder) {
                Ok(found) if found.is_dir() && !found.file_type().is_symlink() => {
                    let _ = fs::remove_dir_all(folder);
                }
                Ok(_) => {
                    let _ = fs::remove_file(folder);
                }
                Err(_) => {}
            }
        }
    }
}

/// Whether every folder between `root` and `path` is still a real folder, not a link a
/// check put there, so removing `path` stays inside the snapshot. Inputs: the snapshot root
/// and a path below it. Output: the answer.
fn plain_parents(root: &Path, path: &Path) -> bool {
    let Ok(below) = path.strip_prefix(root) else {
        return false;
    };
    let mut at = root.to_path_buf();
    let mut parts: Vec<_> = below.components().collect();
    parts.pop();
    parts.into_iter().all(|part| {
        at.push(part);
        fs::symlink_metadata(&at)
            .is_ok_and(|found| found.is_dir() && !found.file_type().is_symlink())
    })
}

/// Checks a portable relative path: 1 to 256 bytes of components made of letters, digits,
/// `-`, `_` and `.`, none empty, `.` or `..`. Input: the untrusted path. Output: whether it
/// is admissible.
pub fn safe_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= PATH_BYTES_MAX
        && path.split('/').all(|part| {
            !part.is_empty()
                && part != "."
                && part != ".."
                && part
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
        })
}

#[cfg(unix)]
fn object_id(text: &str) -> bool {
    matches!(text.len(), 40 | 64)
        && text
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// Reads the change set row a snapshot is made from. Inputs: the engine connection and the
/// change set. Output: its project, repository, git folder, base and head, or
/// `checks_change_set_unknown`, `checks_change_set_discarded` or `checks_unavailable`.
#[cfg(unix)]
fn source(db: &Connection, change_set: &ChangeSetId) -> Result<Source, CheckRefusal> {
    let row = db
        .query_row(
            "SELECT project, repository, git_dir, base, head, state FROM landing_change_sets \
             WHERE id = ?1",
            params![change_set.as_str()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                ))
            },
        )
        .optional()
        .map_err(|_| failure(UNAVAILABLE))?;
    let Some((project, repository, git_dir, base, head, state)) = row else {
        return Err(failure("checks_change_set_unknown"));
    };
    if state == "discarded" {
        return Err(failure("checks_change_set_discarded"));
    }
    let project = ProjectId::new(project).map_err(|_| failure(UNAVAILABLE))?;
    let (repository, git_dir) = (PathBuf::from(repository), PathBuf::from(git_dir));
    if !object_id(&base) || !object_id(&head) || !repository.is_absolute() || !git_dir.is_absolute()
    {
        return Err(failure(UNAVAILABLE));
    }
    Ok(Source {
        project,
        repository,
        git_dir,
        base,
        head,
    })
}

/// The object stores the checkout borrows, the owner's first so an object both hold is read
/// from the owner's store: the repository's `.git/objects` and, for an agent's own clone,
/// its git folder's `objects` once it passes the check a landing fetch runs (DEC102-F1,
/// AL-1), so the agent cannot point the checkout at another store or carry past the tree
/// bounds. Input: the source. Output: the canonical stores, `checks_snapshot_store_invalid`
/// or `checks_snapshot_too_large` for an agent store landing refuses, or
/// `checks_unavailable` for a store reached through a link or a path git cannot list.
#[cfg(unix)]
fn stores(source: &Source) -> Result<Vec<PathBuf>, CheckRefusal> {
    let agent = source.git_dir.join("objects");
    let mut stores = Vec::new();
    for store in [
        source.repository.join(".git").join("objects"),
        agent.clone(),
    ] {
        match fs::symlink_metadata(&store) {
            Ok(found) if found.is_dir() => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            _ => return Err(failure(UNAVAILABLE)),
        }
        if store == agent {
            crate::landing::check_store(
                &source.repository,
                &source.git_dir,
                &crate::worktrees::TREE_BOUNDS,
            )
            .map_err(|refused| {
                failure(if refused.0 == "changes_too_large" {
                    "checks_snapshot_too_large"
                } else {
                    "checks_snapshot_store_invalid"
                })
            })?;
        }
        let real = store.canonicalize().map_err(|_| failure(UNAVAILABLE))?;
        let printable = real
            .to_str()
            .is_some_and(|text| !text.chars().any(char::is_control));
        if real != store || !printable {
            return Err(failure(UNAVAILABLE));
        }
        if !stores.contains(&real) {
            stores.push(real);
        }
    }
    if stores.is_empty() {
        return Err(failure(UNAVAILABLE));
    }
    Ok(stores)
}

/// Makes one private folder, or with `existing` accepts a real folder already there.
/// Inputs: the path and whether one may exist. Output: Ok, or `checks_unavailable`.
#[cfg(unix)]
fn private_folder(path: &Path, existing: bool) -> Result<(), CheckRefusal> {
    use std::os::unix::fs::DirBuilderExt;
    match fs::DirBuilder::new().mode(0o700).create(path) {
        Ok(()) => Ok(()),
        Err(error) if existing && error.kind() == io::ErrorKind::AlreadyExists => {
            match fs::symlink_metadata(path) {
                Ok(found) if found.is_dir() && !found.file_type().is_symlink() => Ok(()),
                _ => Err(failure(UNAVAILABLE)),
            }
        }
        Err(_) => Err(failure(UNAVAILABLE)),
    }
}

/// Makes the snapshot in a new run folder: `git init`, the borrowed stores, the tree
/// listing checked against links and the engine's tree bounds, the checkout of the head
/// detached, then every object copied in, the borrow removed and the copy verified.
/// Inputs: the run folder, the source and its stores. Output: the snapshot, or the refusal.
#[cfg(unix)]
fn check_out(folder: &Path, source: &Source, stores: &[PathBuf]) -> Result<Snapshot, CheckRefusal> {
    let worktrees = folder.join("worktrees");
    private_folder(&worktrees, false)?;
    let root = worktrees.join("src");
    private_folder(&root, false)?;
    let git_dir = root.join(".git");
    git(
        None,
        &root,
        &["init", "-q", "--template=", "."],
        SMALL_BYTES_MAX,
    )?;
    let alternates = git_dir.join("objects").join("info").join("alternates");
    let lines: String = stores
        .iter()
        .map(|store| format!("{}\n", store.display()))
        .collect();
    fs::write(&alternates, lines).map_err(|_| failure(UNAVAILABLE))?;
    let peeled = format!("{}^{{commit}}", source.head);
    let commit = git(
        Some(&git_dir),
        &root,
        &["rev-parse", "--verify", "-q", "--end-of-options", &peeled],
        SMALL_BYTES_MAX,
    )?;
    if commit.trim_ascii_end() != source.head.as_bytes() {
        return Err(failure(SNAPSHOT_FAILED));
    }
    let tree = git(
        Some(&git_dir),
        &root,
        &[
            "rev-parse",
            "--verify",
            "-q",
            &format!("{}^{{tree}}", source.head),
        ],
        SMALL_BYTES_MAX,
    )?;
    let tree = String::from_utf8(tree.trim_ascii_end().to_vec())
        .ok()
        .filter(|tree| object_id(tree))
        .ok_or_else(|| failure(SNAPSHOT_FAILED))?;
    let listing = git(
        Some(&git_dir),
        &root,
        &["ls-tree", "-r", "-z", "-l", "--full-tree", &source.head],
        LISTING_BYTES_MAX,
    )?;
    bounded_tree(&listing)?;
    git(
        Some(&git_dir),
        &root,
        &["read-tree", "--reset", "-u", &source.head],
        SMALL_BYTES_MAX,
    )?;
    git(
        Some(&git_dir),
        &root,
        &["update-ref", "--no-deref", "HEAD", &source.head],
        SMALL_BYTES_MAX,
    )?;
    fs::write(git_dir.join("shallow"), format!("{}\n", source.head))
        .map_err(|_| failure(UNAVAILABLE))?;
    git(
        Some(&git_dir),
        &root,
        &["repack", "-a", "-d", "-q"],
        SMALL_BYTES_MAX,
    )?;
    fs::remove_file(&alternates).map_err(|_| failure(UNAVAILABLE))?;
    git(
        Some(&git_dir),
        &root,
        &["fsck", "--no-progress", "--no-dangling"],
        SMALL_BYTES_MAX,
    )?;
    assert!(!alternates.exists());
    Ok(Snapshot {
        root,
        commit: source.head.clone(),
        tree,
        base: source.base.clone(),
        folder: folder.to_path_buf(),
    })
}

/// Checks the recursive tree listing of `git ls-tree -r -z -l`: no symlink (mode 120000)
/// and, beside submodule links, no more files, bytes or path components than the engine's
/// worktree bounds. Input: the listing. Output: Ok, `checks_snapshot_link`,
/// `checks_snapshot_too_large` or `checks_snapshot_failed` for a line git never prints.
#[cfg(unix)]
fn bounded_tree(listing: &[u8]) -> Result<(), CheckRefusal> {
    let bounds = &crate::worktrees::TREE_BOUNDS;
    let (mut files, mut bytes) = (0usize, 0u64);
    for record in listing
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty())
    {
        let tab = record
            .iter()
            .position(|byte| *byte == b'\t')
            .ok_or_else(|| failure(SNAPSHOT_FAILED))?;
        let (head, path) = (&record[..tab], &record[tab + 1..]);
        let head = std::str::from_utf8(head).map_err(|_| failure(SNAPSHOT_FAILED))?;
        let fields: Vec<&str> = head.split_ascii_whitespace().collect();
        let [mode, _, _, size] = fields[..] else {
            return Err(failure(SNAPSHOT_FAILED));
        };
        match mode {
            "120000" => return Err(failure("checks_snapshot_link")),
            "160000" => continue,
            _ => {}
        }
        let size: u64 = size.parse().map_err(|_| failure(SNAPSHOT_FAILED))?;
        files += 1;
        bytes = bytes.saturating_add(size);
        let components = path.split(|byte| *byte == b'/').count();
        if files > bounds.files || bytes > bounds.bytes || components > bounds.components {
            return Err(failure("checks_snapshot_too_large"));
        }
    }
    assert!(files <= bounds.files && bytes <= bounds.bytes);
    Ok(())
}

/// Runs one snapshot git step within `GIT_STEP_TIME`: the host git with a cleared
/// environment, no system or global config or attributes, the research runner's `GIT_SAFE`
/// and [`SNAPSHOT_SETTINGS`], the snapshot's own git folder and work tree, its own process
/// group, killed whole past the time. Inputs: the git folder, the work tree, the arguments
/// and the most output bytes. Output: the output, or `checks_snapshot_failed`.
#[cfg(unix)]
fn git(
    git_dir: Option<&Path>,
    work_tree: &Path,
    args: &[&str],
    limit: usize,
) -> Result<Vec<u8>, CheckRefusal> {
    use std::io::Read;
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};
    let mut command = Command::new(GIT);
    command
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("LC_ALL", "C")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_ATTR_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_NO_LAZY_FETCH", "1")
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .arg("--no-optional-locks")
        .args(crate::research::runner::GIT_SAFE)
        .args(SNAPSHOT_SETTINGS);
    if let Some(git_dir) = git_dir {
        command
            .arg("--git-dir")
            .arg(git_dir)
            .arg("--work-tree")
            .arg(work_tree);
    }
    command
        .args(args)
        .current_dir(work_tree)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0);
    let mut child = crate::spawn_gate::spawn(&mut command).map_err(|_| failure(SNAPSHOT_FAILED))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| failure(SNAPSHOT_FAILED))?;
    let reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let mut source = stdout.take(limit as u64 + 1);
        let read = source.read_to_end(&mut bytes);
        let _ = io::copy(&mut source.into_inner(), &mut io::sink());
        read.map(|_| bytes)
    });
    let deadline = std::time::Instant::now() + GIT_STEP_TIME;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if std::time::Instant::now() < deadline => std::thread::sleep(POLL),
            _ => break None,
        }
    };
    if status.is_none() {
        if let Some(group) = i32::try_from(child.id())
            .ok()
            .and_then(rustix::process::Pid::from_raw)
        {
            let _ = rustix::process::kill_process_group(group, rustix::process::Signal::KILL);
        }
        let _ = child.wait();
    }
    let bytes = reader
        .join()
        .map_err(|_| failure(SNAPSHOT_FAILED))?
        .map_err(|_| failure(SNAPSHOT_FAILED))?;
    if !status.is_some_and(|status| status.success()) || bytes.len() > limit {
        return Err(failure(SNAPSHOT_FAILED));
    }
    assert!(bytes.len() <= limit);
    Ok(bytes)
}
