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
        if !key.contains('.') {
            return Err(error("invalid_git_config"));
        }
        if let Some(rule) = refused_key(key, value) {
            return Err(error(format!("{CONFIG_REFUSED}{}", rule.code)));
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

/// Finds and checks the Git metadata folder of an engine lease worktree: it must sit in
/// the repository's .git/worktrees, link back to the lease, and hold no config.worktree,
/// which Git reads for that worktree when extensions.worktreeConfig is on. Input: the
/// lease record. Output: the metadata folder, or the refusal code.
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
    if std::fs::symlink_metadata(path.join("config.worktree")).is_ok() {
        return Err(error("worktree_config_present"));
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
