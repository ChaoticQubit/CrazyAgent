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
#[cfg(unix)]
const FILES_MAX: usize = 128;
#[cfg(unix)]
const TREE_BYTES_MAX: usize = 32 * 1024 * 1024;

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

#[cfg(unix)]
fn identity(path: &Path, directory: bool) -> Result<Identity, EngineError> {
    let metadata = std::fs::symlink_metadata(path).map_err(error)?;
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

#[cfg(unix)]
fn bounded_read(path: &Path, limit: usize) -> Result<Vec<u8>, EngineError> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
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
    let child = crate::spawn_gate::spawn(
        Process::new("/usr/bin/git")
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_NO_LAZY_FETCH", "1")
            .env("GIT_ATTR_NOSYSTEM", "1")
            .env("GIT_OPTIONAL_LOCKS", "0")
            .arg("--no-replace-objects")
            .arg("--no-optional-locks")
            .args([
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "core.fsmonitor=false",
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
            ])
            .arg("--git-dir")
            .arg(repository.join(".git"))
            .arg("--work-tree")
            .arg(repository)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null()),
    )
    .map_err(error)?;
    collect_git_output(child)
}

fn collect_git_output(mut child: std::process::Child) -> Result<Vec<u8>, EngineError> {
    let output = child
        .stdout
        .take()
        .ok_or_else(|| error("git_stdout_missing"))?;
    let reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        output
            .take((OUTPUT_BYTES_MAX + 1) as u64)
            .read_to_end(&mut bytes)?;
        Ok::<_, std::io::Error>(bytes)
    });
    let deadline = Instant::now() + Duration::from_secs(5);
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
    if !status.success() || bytes.len() > OUTPUT_BYTES_MAX {
        return Err(error("git_operation_failed_or_output_capacity"));
    }
    assert!(bytes.len() <= OUTPUT_BYTES_MAX);
    assert!(status.success());
    Ok(bytes)
}

fn validate_metadata(repository: &Path) -> Result<(), EngineError> {
    let metadata = repository.join(".git");
    let mut pending = vec![metadata.clone()];
    let mut visited = 0;
    while let Some(path) = pending.pop() {
        visited += 1;
        if visited > 8192 {
            return Err(error("git_metadata_capacity"));
        }
        let entry = std::fs::symlink_metadata(&path).map_err(error)?;
        identity(&path, entry.is_dir())?;
        if entry.is_dir() {
            for entry in std::fs::read_dir(&path).map_err(error)? {
                pending.push(entry.map_err(error)?.path());
                if pending.len() > 8192 {
                    return Err(error("git_metadata_capacity"));
                }
            }
        }
    }
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
    assert!(visited > 0);
    assert!(visited <= 8192);
    Ok(())
}

fn validate_config(repository: &Path) -> Result<(), EngineError> {
    let bytes = git(
        repository,
        &["config", "--local", "--no-includes", "--null", "--list"],
    )?;
    if bytes.len() > 65536 {
        return Err(error("git_config_capacity"));
    }
    for record in bytes.split(|b| *b == 0).filter(|r| !r.is_empty()) {
        let record = std::str::from_utf8(record).map_err(error)?;
        let (key, value) = record
            .split_once('\n')
            .ok_or_else(|| error("invalid_git_config"))?;
        let safe = match key {
            "core.repositoryformatversion" => value == "0",
            "core.bare" => value == "false",
            "core.filemode"
            | "core.logallrefupdates"
            | "core.ignorecase"
            | "core.precomposeunicode" => matches!(value, "true" | "false"),
            "user.name" | "user.email" | "init.defaultbranch" => true,
            key if key.starts_with("remote.")
                && (key.ends_with(".url") || key.ends_with(".fetch")) =>
            {
                true
            }
            key if key.starts_with("branch.")
                && (key.ends_with(".remote") || key.ends_with(".merge")) =>
            {
                true
            }
            _ => false,
        };
        if !safe {
            return Err(error("unsupported_repository_config"));
        }
    }
    assert!(bytes.len() <= 65536);
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

fn verify(admission: &Admission) -> Result<(), EngineError> {
    let current = inspect(&admission.repository)?;
    if current.root != admission.root || current.metadata != admission.metadata {
        return Err(error("admitted_repository_replaced"));
    }
    Ok(())
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
    verify(&admission)?;
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
    let mut record = load(connection, task, repository)?.ok_or_else(|| error("lease_missing"))?;
    assert_eq!(record.lease.state, LeaseState::Reserved);
    assert!(connection.is_autocommit());
    let result = materialize(&mut record);
    record.lease.state = if result.is_ok() {
        LeaseState::Ready
    } else {
        LeaseState::Uncertain
    };
    save(connection, &record)?;
    result
}

/// Refuses to materialize a worktree where no directory-relative no-follow writes exist.
/// Takes the lease record, unchanged. Returns `worktree_filesystem_unavailable`.
#[cfg(not(unix))]
fn materialize(_record: &mut LeaseRecord) -> Result<(), EngineError> {
    Err(error("worktree_filesystem_unavailable"))
}

#[cfg(unix)]
fn materialize(record: &mut LeaseRecord) -> Result<(), EngineError> {
    verify(&record.admission)?;
    let repository = Path::new(&record.lease.repository);
    let target = Path::new(&record.lease.path);
    canonical(
        target
            .parent()
            .ok_or_else(|| error("target_parent_missing"))?,
    )?;
    let entries = tree_entries(repository, &record.head)?;
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
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut total = 0;
    for (mode, oid, path) in entries {
        if Instant::now() >= deadline {
            return Err(error("materialization_deadline"));
        }
        let bytes = git(repository, &["cat-file", "blob", &oid])?;
        total += bytes.len();
        if total > TREE_BYTES_MAX {
            return Err(error("tree_byte_capacity"));
        }
        write_blob_at(&directory, &path, &bytes, mode == "100755")?;
    }
    let gitdir = metadata.to_str().ok_or_else(|| error("non_utf8_gitdir"))?;
    git(
        repository,
        &[
            "--git-dir",
            gitdir,
            "--work-tree",
            &record.lease.path,
            "read-tree",
            &record.head,
        ],
    )?;
    verify_record(record)?;
    assert!(total <= TREE_BYTES_MAX);
    assert!(record.directory.is_some());
    Ok(())
}

#[cfg(unix)]
fn tree_entries(
    repository: &Path,
    head: &str,
) -> Result<Vec<(String, String, PathBuf)>, EngineError> {
    let bytes = git(repository, &["ls-tree", "-r", "-z", head])?;
    let mut entries = Vec::new();
    for entry in bytes.split(|b| *b == 0).filter(|r| !r.is_empty()) {
        if entries.len() >= FILES_MAX {
            return Err(error("tree_file_capacity"));
        }
        let entry = std::str::from_utf8(entry).map_err(error)?;
        let (header, name) = entry
            .split_once('\t')
            .ok_or_else(|| error("invalid_tree_entry"))?;
        let fields = header.split(' ').collect::<Vec<_>>();
        let path = PathBuf::from(name);
        if fields.len() != 3
            || !matches!(fields[0], "100644" | "100755")
            || fields[1] != "blob"
            || path
                .components()
                .any(|p| !matches!(p, Component::Normal(_)))
            || path
                .components()
                .any(|p| p.as_os_str().to_string_lossy().eq_ignore_ascii_case(".git"))
            || path.file_name().is_some_and(|n| n == ".gitattributes")
        {
            return Err(error("unsupported_or_unsafe_tree_entry"));
        }
        validate_oid(fields[2])?;
        entries.push((fields[0].into(), fields[2].into(), path));
    }
    assert!(entries.len() <= FILES_MAX);
    Ok(entries)
}

#[cfg(unix)]
fn write_blob_at(
    root: &std::fs::File,
    path: &Path,
    bytes: &[u8],
    executable: bool,
) -> Result<(), EngineError> {
    use rustix::fs::{Mode, OFlags, mkdirat, openat};
    let components = path.components().collect::<Vec<_>>();
    if components.is_empty()
        || components.len() > 64
        || components
            .iter()
            .any(|c| !matches!(c, Component::Normal(_)))
    {
        return Err(error("invalid_materialization_path"));
    }
    let mut parent = root.try_clone().map_err(error)?;
    for component in &components[..components.len() - 1] {
        match mkdirat(&parent, component.as_os_str(), Mode::from_raw_mode(0o700)) {
            Ok(()) => {}
            Err(rustix::io::Errno::EXIST) => {}
            Err(e) => return Err(error(e)),
        }
        parent = openat(
            &parent,
            component.as_os_str(),
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map(std::fs::File::from)
        .map_err(error)?;
    }
    let mode = Mode::from_raw_mode(if executable { 0o700 } else { 0o600 });
    let mut file = openat(
        &parent,
        components.last().unwrap().as_os_str(),
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        mode,
    )
    .map(std::fs::File::from)
    .map_err(error)?;
    file.write_all(bytes).map_err(error)?;
    file.sync_all().map_err(error)?;
    assert_eq!(file.metadata().map_err(error)?.len(), bytes.len() as u64);
    assert_eq!(file.metadata().map_err(error)?.nlink(), 1);
    Ok(())
}

fn worktree_metadata(record: &LeaseRecord) -> Result<PathBuf, EngineError> {
    let target = Path::new(&record.lease.path);
    let bytes = bounded_read(&target.join(".git"), 4096)?;
    let text = std::str::from_utf8(&bytes).map_err(error)?;
    let path = PathBuf::from(
        text.strip_prefix("gitdir: ")
            .and_then(|s| s.strip_suffix('\n'))
            .ok_or_else(|| error("invalid_worktree_git_pointer"))?,
    );
    let root = Path::new(&record.lease.repository).join(".git/worktrees");
    canonical(&path)?;
    if path.parent() != Some(root.as_path()) {
        return Err(error("worktree_metadata_escape"));
    }
    if bounded_read(&path.join("commondir"), 32)? != b"../..\n"
        || bounded_read(&path.join("gitdir"), 4096)?
            != format!("{}/.git\n", record.lease.path).as_bytes()
    {
        return Err(error("worktree_backlink_mismatch"));
    }
    Ok(path)
}

fn verify_record(record: &LeaseRecord) -> Result<(), EngineError> {
    assert_record_consistent(record);
    verify(&record.admission)?;
    let target = Path::new(&record.lease.path);
    canonical(target)?;
    let metadata = worktree_metadata(record)?;
    if record.directory.as_ref() != Some(&identity(target, true)?)
        || record.metadata.as_ref() != Some(&identity(&metadata, true)?)
    {
        return Err(error("worktree_identity_changed"));
    }
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
    let head = bounded_read(&metadata.join("HEAD"), 512)?;
    if head != format!("ref: refs/heads/{}\n", record.lease.branch).as_bytes() {
        return Err(error("worktree_head_mismatch"));
    }
    assert!(record.directory.is_some());
    assert!(record.metadata.is_some());
    Ok(())
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
        record.lease.state = if matches!(
            record.lease.state,
            LeaseState::Ready | LeaseState::RecoveryRequired
        ) && verify_record(&record).is_ok()
        {
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
        write_blob_at(
            &directory,
            Path::new("nested/tracked"),
            b"source bytes",
            false,
        )
        .unwrap();
        assert_eq!(
            std::fs::read(retained.join("nested/tracked")).unwrap(),
            b"source bytes"
        );
        assert!(!outside.join("nested").exists());
        std::os::unix::fs::symlink(&outside, retained.join("escape")).unwrap();
        assert!(write_blob_at(&directory, Path::new("escape/file"), b"forbidden", false).is_err());
        assert!(!outside.join("file").exists());
    }
}
