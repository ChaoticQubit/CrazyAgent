use super::clone;
use crate::engine::EngineError;
use crate::project_setup::SECRET_PREFIXES;
use crate::research::runner::GIT_SAFE;
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, ExitStatus, Output, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

#[cfg(all(test, target_os = "macos"))]
mod publish_tests;

const SCAN_FILES_MAX: usize = 20_000;
const SCAN_BYTES_MAX: u64 = 200 << 20;
const SCAN_FILE_BYTES_MAX: u64 = 1 << 20;
const BINARY_PROBE_BYTES: usize = 8 << 10;
const LISTING_BYTES_MAX: u64 = 16 << 20;
const CONFIG_BYTES_MAX: u64 = 64 << 10;
const OUTPUT_BYTES_MAX: u64 = 1 << 20;
const GITIGNORE_BYTES_MAX: u64 = 1 << 20;
const FINDINGS_MAX: usize = 1000;
const FILE_FINDINGS_MAX: usize = 20;
const LOOKUP_TIME: Duration = Duration::from_secs(20);
const WORK_TIME: Duration = Duration::from_secs(600);
const EMPTY_TREES: [&str; 2] = [
    "4b825dc642cb6eb9a060e54bf8d69288fbee4904",
    "6ef19b41225c5369f1c104d45d8d85efa9b057b53b14b4b9b939dd74decc5321",
];
const BATCH_HEADER_BYTES_MAX: u64 = 256;
const BRANCH_BYTES_MAX: usize = 255;
const IGNORE_LINES_MAX: usize = 1000;
const IGNORE_LINE_BYTES_MAX: usize = 1024;
const TOKEN_BODY_MIN: usize = 16;
const NAMED_VALUE_MIN: usize = 8;
const MAIN: &str = "main";
const ENGINE_EMAIL: &str = "engine@crazyagent.invalid";
const ENGINE_START: &str = "Start project";
const GITIGNORE: &str = ".gitignore";
const MESSAGE: &str = "Publish with CrazyAgent";
const AUTHOR_BYTES_MAX: usize = 256;
const PUSH_TIME: Duration = Duration::from_secs(1800);
const STAGE: &str = "stage.git";
const FETCH: &str = "+refs/heads/*:refs/remotes/origin/*";
const DOCS_OFF: [&str; 2] = ["/.crazyagent/", "/graphify-out/"];
const DOCS_ON: [&str; 4] = [
    "!/.crazyagent/",
    "/.crazyagent/*",
    "!/.crazyagent/context/",
    "/graphify-out/",
];
const OLDER_LAYOUT: [&str; 1] = ["/graphify-out/"];
const DOCS_ON_ONLY: [&str; 3] = ["!/.crazyagent/", "/.crazyagent/*", "!/.crazyagent/context/"];
const SECURITY_PATHS: [&str; 3] = ["SECURITY.md", ".github/SECURITY.md", "docs/SECURITY.md"];
const SECURITY_POLICY: &str = "# Security Policy\n\n## Reporting a Vulnerability\n\nPlease report a vulnerability privately through GitHub's private vulnerability reporting: open this repository's Security tab and choose \"Report a vulnerability\". Please do not open a public issue for it.\n";
const DEPENDABOT: &str = ".github/dependabot.yml";
const DEPENDABOT_PATHS: [&str; 2] = [DEPENDABOT, ".github/dependabot.yaml"];
const WRITTEN: [&str; 3] = [GITIGNORE, SECURITY_PATHS[0], DEPENDABOT];
const ECOSYSTEMS: [(&str, &str); 6] = [
    ("Cargo.toml", "cargo"),
    ("package.json", "npm"),
    ("requirements.txt", "pip"),
    ("pyproject.toml", "pip"),
    ("go.mod", "gomod"),
    (".github/workflows", "github-actions"),
];
const KEY_ENDS: [&str; 14] = [
    "password",
    "passwd",
    "passphrase",
    "secret",
    "token",
    "apikey",
    "privatekey",
    "secretkey",
    "accesskey",
    "credential",
    "credentials",
    "_pass",
    "_pwd",
    "_key",
];
const KEY_NAMES: [&str; 2] = ["pass", "pwd"];
const SETTINGS_NAMES: [&str; 6] = [
    ".env",
    ".npmrc",
    ".pypirc",
    ".netrc",
    ".pgpass",
    "credentials",
];
const SETTINGS_EXTENSIONS: [&str; 8] = [
    ".properties",
    ".ini",
    ".cfg",
    ".conf",
    ".toml",
    ".yaml",
    ".yml",
    ".json",
];
const PLACEHOLDERS: [&str; 6] = [
    "changeit",
    "password",
    "undefined",
    "notasecret",
    "secretkey",
    "yoursecret",
];

/// What a project's history holds before it is published: no commit yet, only the empty
/// start commit the engine makes in a project it creates, or the owner's own commits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitHubHistory {
    Empty,
    EngineStart,
    Owner,
}

/// How a project's git stands before it is published: whether it is a repository, the
/// branch HEAD names and what its history holds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GitHubLocal {
    pub repository: bool,
    pub branch: Option<String>,
    pub history: GitHubHistory,
}

/// What the owner chose for the local side of a publish: the repository's visibility,
/// whether the docs folder goes up, and the .gitignore lines the owner approved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GitHubPrepare {
    pub private: bool,
    pub docs: bool,
    pub approved: Vec<String>,
}

/// The kind of secret found on a line: a known token, a private key block, or a
/// credential-named setting holding a value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitHubSecretKind {
    Token,
    PrivateKey,
    NamedValue,
}

/// What the secret check found: the lines, by file and line, and whether the list was cut
/// at its bound, so more lines may hold secrets than it names.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GitHubSecretScan {
    pub findings: Vec<GitHubSecretFinding>,
    pub truncated: bool,
}

/// One line that would push a secret: the file and the line number, never the value.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GitHubSecretFinding {
    pub path: String,
    pub line: u32,
    pub kind: GitHubSecretKind,
}

fn failure(code: &str) -> EngineError {
    EngineError(code.to_owned())
}

/// A private empty folder under the engine root that git uses as its home for one call,
/// so no owner configuration applies; it is deleted when dropped.
pub(super) struct GitHome(PathBuf);

impl GitHome {
    /// Makes the folder. Input: the engine root. Output: the home, or
    /// `github_publish_failed`.
    pub(super) fn new(root: &Path) -> Result<Self, EngineError> {
        let mut token = [0_u8; 8];
        getrandom::fill(&mut token).map_err(|_| failure("github_publish_failed"))?;
        let name: String = token.iter().map(|byte| format!("{byte:02x}")).collect();
        let path = root.join(format!(".publish-home-{name}"));
        clone::private_folder(&path).map_err(|_| failure("github_publish_failed"))?;
        Ok(Self(path))
    }

    pub(super) fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for GitHome {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The git the engine runs for a publish, as for a clone. Output: its path, or
/// `github_git_missing`.
pub(super) fn git() -> Result<PathBuf, EngineError> {
    clone::git_program(std::env::var_os("PATH").as_deref())
        .ok_or_else(|| failure("github_git_missing"))
}

/// Starts a git command in the project's own repository with the engine's safe options and
/// the empty home, so no hook folder, pager, fsmonitor or owner configuration outside the
/// repository runs, and git never looks for a repository above the project. A config hook
/// these options do not stop is refused by [`refuse_hooks`] first. Inputs: git, its empty
/// home and the project. Output: the command, without arguments.
fn project_git(git: &Path, home: &Path, project: &Path) -> Command {
    let mut command = clone::git_command(git, home);
    command
        .current_dir(project)
        .env("GIT_DIR", project.join(".git"))
        .env("GIT_WORK_TREE", project)
        .args(GIT_SAFE)
        .stdin(Stdio::null())
        .stderr(Stdio::null());
    command
}

/// A started git that a watcher thread kills when it still runs at its time limit, so a
/// config include, an ignore file or any pipe that never ends, such as a FIFO, never holds
/// publish.
struct Watched {
    child: Arc<Mutex<Child>>,
    done: mpsc::Sender<()>,
    watcher: std::thread::JoinHandle<bool>,
}

impl Watched {
    /// Starts the command and its watcher. Inputs: the command and the time limit.
    /// Output: the watched git, or `github_publish_failed` when it could not start.
    fn start(command: &mut Command, limit: Duration) -> Result<Self, EngineError> {
        let child =
            crate::spawn_gate::spawn(command).map_err(|_| failure("github_publish_failed"))?;
        let child = Arc::new(Mutex::new(child));
        let (done, told) = mpsc::channel::<()>();
        let watched = Arc::clone(&child);
        let watcher = std::thread::spawn(move || {
            let late = matches!(told.recv_timeout(limit), Err(RecvTimeoutError::Timeout));
            if late {
                let _ = watched
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .kill();
            }
            late
        });
        Ok(Self {
            child,
            done,
            watcher,
        })
    }

    /// Takes git's piped output and input. Output: each, None when it is not piped.
    fn pipes(&self) -> (Option<ChildStdout>, Option<ChildStdin>) {
        let mut child = self.child.lock().unwrap_or_else(PoisonError::into_inner);
        (child.stdout.take(), child.stdin.take())
    }

    /// Stops git at once, for a reply publish no longer reads.
    fn stop(&self) {
        let _ = self
            .child
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .kill();
    }

    /// Ends the watch and waits for git. Output: its status, or `github_publish_failed`
    /// when it ran past its limit or could not be waited for.
    fn finish(self) -> Result<ExitStatus, EngineError> {
        let _ = self.done.send(());
        let late = self.watcher.join().unwrap_or(true);
        let status = self
            .child
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .wait();
        match status {
            Ok(status) if !late => Ok(status),
            _ => Err(failure("github_publish_failed")),
        }
    }
}

/// Runs a git command to its end within `limit`, keeping at most `cap` bytes and one more
/// of what it prints and draining the rest, so git never waits on a full pipe. Inputs:
/// the command, the limit and the cap. Output: its status and what was kept, or
/// `github_publish_failed`.
fn collect(
    command: &mut Command,
    limit: Duration,
    cap: u64,
) -> Result<(ExitStatus, Vec<u8>), EngineError> {
    let watched = Watched::start(command.stdout(Stdio::piped()), limit)?;
    let stdout = watched.pipes().0.expect("git stdout is piped");
    let mut bytes = Vec::new();
    let mut source = stdout.take(cap + 1);
    let read = source.read_to_end(&mut bytes).is_ok()
        && std::io::copy(&mut source.into_inner(), &mut std::io::sink()).is_ok();
    if !read {
        watched.stop();
    }
    let status = watched.finish()?;
    if !read {
        return Err(failure("github_publish_failed"));
    }
    Ok((status, bytes))
}

/// Runs a git command whose output is small within `LOOKUP_TIME`. Input: the command.
/// Output: as [`run_within`].
fn run(command: &mut Command) -> Result<Output, EngineError> {
    run_within(command, LOOKUP_TIME)
}

/// Runs a git command that writes the repository, such as add or commit, within
/// `WORK_TIME`. Input: the command. Output: as [`run_within`].
fn work(command: &mut Command) -> Result<Output, EngineError> {
    run_within(command, WORK_TIME)
}

/// Runs a git command whose output is small, within `limit`. Inputs: the command and the
/// limit. Output: what it printed and its status, or `github_publish_failed` when it
/// could not start, ran over or printed past 1 MiB.
fn run_within(command: &mut Command, limit: Duration) -> Result<Output, EngineError> {
    let (status, stdout) = collect(command, limit, OUTPUT_BYTES_MAX)?;
    if stdout.len() as u64 > OUTPUT_BYTES_MAX {
        return Err(failure("github_publish_failed"));
    }
    Ok(Output {
        status,
        stdout,
        stderr: Vec::new(),
    })
}

/// Runs a git command that lists files within `limit`, keeping at most
/// `LISTING_BYTES_MAX` of it. Inputs: the command and the limit. Output: as
/// [`listing_within`].
fn listing(command: &mut Command, limit: Duration) -> Result<Vec<u8>, EngineError> {
    listing_within(command, limit, LISTING_BYTES_MAX)
}

/// Runs a git command that lists files within `limit`, keeping at most `cap` bytes of it.
/// Inputs: the command, the limit and the cap. Output: the listing, or
/// `github_publish_too_large`, `github_publish_failed`.
fn listing_within(
    command: &mut Command,
    limit: Duration,
    cap: u64,
) -> Result<Vec<u8>, EngineError> {
    let (status, bytes) = collect(command, limit, cap)?;
    if bytes.len() as u64 > cap {
        return Err(failure("github_publish_too_large"));
    }
    if !status.success() {
        return Err(failure("github_publish_failed"));
    }
    Ok(bytes)
}

/// Reads how a project's git stands. Inputs: git, its empty home, the project folder and
/// GitHub's web host. Output: the state, or `github_publish_on_github`,
/// `github_publish_other_host`, `github_publish_detached`, `github_publish_not_plain_repo`,
/// `project_folder_missing`.
pub(super) fn inspect(
    git: &Path,
    home: &Path,
    project: &Path,
    host: &str,
) -> Result<GitHubLocal, EngineError> {
    inspect_within(git, home, project, host, LOOKUP_TIME)
}

/// Refuses a project whose git config, its includes too, names a hook: `hook.<name>.<key>`,
/// which git 2.54 runs whatever `core.hooksPath` says. It runs before any publish git that
/// can fire one (add, commit, status, update-ref) and reads the keys as landing does.
/// Inputs: git, its empty home and the project. Output: `()`, or
/// `github_publish_hooks_set`, `github_publish_failed`.
fn refuse_hooks(git: &Path, home: &Path, project: &Path) -> Result<(), EngineError> {
    let listed = run(project_git(git, home, project).args(crate::research::runner::FILTER_KEYS))?;
    let keys = Some(listed.stdout.as_slice())
        .filter(|_| listed.status.success())
        .and_then(crate::research::runner::listed_filter_keys)
        .ok_or_else(|| failure("github_publish_failed"))?;
    if crate::research::runner::listed_hook(keys).is_some() {
        return Err(failure("github_publish_hooks_set"));
    }
    Ok(())
}

/// Reads how a project's git stands as [`inspect`] does, each git run within `limit`.
/// Inputs: git, its empty home, the project, GitHub's web host and the limit. Output: as
/// [`inspect`].
fn inspect_within(
    git: &Path,
    home: &Path,
    project: &Path,
    host: &str,
    limit: Duration,
) -> Result<GitHubLocal, EngineError> {
    let folder =
        std::fs::symlink_metadata(project).map_err(|_| failure("project_folder_missing"))?;
    if !project.is_absolute() || !folder.is_dir() {
        return Err(failure("project_folder_missing"));
    }
    if !plain_repository(project)? {
        return Ok(GitHubLocal {
            repository: false,
            branch: None,
            history: GitHubHistory::Empty,
        });
    }
    remotes(git, home, project, host, limit)?;
    let (branch, history) = head_state(git, home, project, limit)?;
    Ok(GitHubLocal {
        repository: true,
        branch: Some(branch),
        history,
    })
}

/// Tells whether a project holds its own repository: a plain `.git` folder, not a link
/// or a file that names a git folder elsewhere. Input: the project. Output: true for a
/// plain folder, false for none, or `github_publish_not_plain_repo`.
fn plain_repository(project: &Path) -> Result<bool, EngineError> {
    match std::fs::symlink_metadata(project.join(".git")) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => Ok(true),
        _ => Err(failure("github_publish_not_plain_repo")),
    }
}

/// Reads the branch HEAD names and what the history holds: none while HEAD is unborn,
/// the engine's start when the one commit is the engine's empty "Start project", else the
/// owner's. Inputs: git, its empty home, the project and the limit for each git run.
/// Output: the branch and the history, or `github_publish_detached`,
/// `github_publish_not_plain_repo`, `github_publish_failed` for a history git cannot read.
fn head_state(
    git: &Path,
    home: &Path,
    project: &Path,
    limit: Duration,
) -> Result<(String, GitHubHistory), EngineError> {
    let failed = || failure("github_publish_failed");
    let head = run_within(
        project_git(git, home, project).args(["symbolic-ref", "-q", "HEAD"]),
        limit,
    )?;
    match head.status.code() {
        Some(0) => {}
        Some(1) => return Err(failure("github_publish_detached")),
        _ => return Err(failure("github_publish_not_plain_repo")),
    }
    let branch = String::from_utf8(head.stdout)
        .ok()
        .as_deref()
        .map(str::trim_end)
        .and_then(|reference| reference.strip_prefix("refs/heads/"))
        .filter(|branch| {
            !branch.is_empty()
                && branch.len() <= BRANCH_BYTES_MAX
                && !branch.chars().any(char::is_control)
        })
        .ok_or_else(|| failure("github_publish_detached"))?
        .to_owned();
    let born = run_within(
        project_git(git, home, project).args(["rev-parse", "-q", "--verify", "HEAD"]),
        limit,
    )?;
    match born.status.code() {
        Some(0) => {}
        Some(1) => return Ok((branch, GitHubHistory::Empty)),
        _ => return Err(failed()),
    }
    let log = run_within(
        project_git(git, home, project).args([
            "log",
            "-2",
            "--format=%ae%x00%s%x00%T%x00",
            "HEAD",
            "--",
        ]),
        limit,
    )?;
    if !log.status.success() {
        return Err(failed());
    }
    let text = String::from_utf8_lossy(&log.stdout);
    let mut fields: Vec<&str> = text
        .split('\0')
        .map(|field| field.trim_matches('\n'))
        .collect();
    if fields.last() == Some(&"") {
        fields.pop();
    }
    let engine = fields.len() == 3
        && fields[0] == ENGINE_EMAIL
        && fields[1] == ENGINE_START
        && EMPTY_TREES.contains(&fields[2]);
    let history = if engine {
        GitHubHistory::EngineStart
    } else {
        GitHubHistory::Owner
    };
    Ok((branch, history))
}

/// Refuses a repository that already has a remote: one on GitHub is published already,
/// and any other is another host's. git lists the keys of `.git/config` alone, includes
/// left out. Inputs: git, its empty home, the project and GitHub's web host.
/// Output: `()`, or `github_publish_on_github`, `github_publish_other_host`.
fn remotes(
    git: &Path,
    home: &Path,
    project: &Path,
    host: &str,
    limit: Duration,
) -> Result<(), EngineError> {
    let config = project.join(".git").join("config");
    match std::fs::symlink_metadata(&config) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Ok(metadata) if metadata.is_file() && metadata.len() <= CONFIG_BYTES_MAX => {}
        _ => return Err(failure("github_publish_not_plain_repo")),
    }
    let listed = run_within(
        clone::git_command(git, home)
            .args(["config", "--file"])
            .arg(&config)
            .args(["--no-includes", "-z", "--list"])
            .stdin(Stdio::null())
            .stderr(Stdio::null()),
        limit,
    )?;
    if !listed.status.success() {
        return Err(failure("github_publish_failed"));
    }
    let mut remote = false;
    let mut on_github = false;
    for entry in listed.stdout.split(|byte| *byte == 0) {
        let (key, value) = match entry.iter().position(|byte| *byte == b'\n') {
            Some(at) => (&entry[..at], &entry[at + 1..]),
            None => (entry, &entry[entry.len()..]),
        };
        if !key.starts_with(b"remote.") {
            continue;
        }
        remote = true;
        if key.ends_with(b".url") || key.ends_with(b".pushurl") {
            on_github |= github_url(&String::from_utf8_lossy(value), host);
        }
    }
    match (remote, on_github) {
        (false, _) => Ok(()),
        (true, true) => Err(failure("github_publish_on_github")),
        (true, false) => Err(failure("github_publish_other_host")),
    }
}

/// Tells whether a remote URL points at GitHub: its host, from a URL or the scp-like
/// `user@host:path` form, is GitHub's web host or one of its subdomains.
/// Inputs: the URL and the web host. Output: true for GitHub.
fn github_url(url: &str, host: &str) -> bool {
    let found = if url.contains("://") {
        url::Url::parse(url)
            .ok()
            .and_then(|parsed| parsed.host_str().map(str::to_owned))
    } else {
        url.split_once(':')
            .filter(|(before, _)| !before.contains('/'))
            .map(|(before, _)| before.rsplit('@').next().unwrap_or(before).to_owned())
    };
    found.is_some_and(|found| {
        let found = found.to_ascii_lowercase();
        found == host || found.ends_with(&format!(".{host}"))
    })
}

/// Gets a project ready for its first publish commit. Everything is checked before
/// anything is written: the approved lines, `.gitignore` and the `.github` folder.
/// Then git init on main when there is no repository, CrazyAgent's folders hidden in
/// `.git/info/exclude`, the docs and approved lines added to `.gitignore` once, and the
/// security policy (public only) and Dependabot files written where none exists.
/// Inputs: git, its empty home, the project folder, its state and the owner's choices.
/// Output: the paths publish wrote, or `github_publish_ignore_invalid`,
/// `github_publish_gitignore_invalid`, `github_publish_file_invalid`,
/// `github_publish_failed`.
pub(super) fn prepare(
    git: &Path,
    home: &Path,
    project: &Path,
    local: &GitHubLocal,
    choices: &GitHubPrepare,
) -> Result<Vec<String>, EngineError> {
    let approved = &choices.approved;
    if !lines_valid(approved) {
        return Err(failure("github_publish_ignore_invalid"));
    }
    let ignore = project.join(GITIGNORE);
    let existing = read_gitignore(&ignore)?;
    let files = generated(project, choices.private)?;
    if !local.repository {
        let made = run(project_git(git, home, project)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .args(["init", "-q", "--template=", "-b", MAIN]))?;
        if !made.status.success() {
            return Err(failure("github_publish_failed"));
        }
    }
    let hidden = crate::projects_home::exclude_from_git(project)?
        && crate::projects_home::exclude_agents(project)?;
    if !hidden {
        return Err(failure("github_publish_not_plain_repo"));
    }
    let older = crate::projects_home::state_folder(project).as_path() == project;
    let docs: &[&str] = if older {
        &OLDER_LAYOUT
    } else if choices.docs {
        &DOCS_ON
    } else {
        &DOCS_OFF
    };
    let mut lines: Vec<&str> = docs.to_vec();
    lines.extend(approved.iter().map(String::as_str));
    let mut written = Vec::new();
    let text = existing.unwrap_or_default();
    let stale = |line: &str| DOCS_ON_ONLY.contains(&line.trim_end());
    if !older && !choices.docs && text.lines().any(stale) {
        let mut kept: Vec<&str> = text.lines().filter(|line| !stale(*line)).collect();
        for line in lines {
            if !kept.iter().any(|present| present.trim_end() == line) {
                kept.push(line);
            }
        }
        let mut replaced = kept.join("\n");
        replaced.push('\n');
        replace_file(project, &ignore, &replaced)?;
        written.push(GITIGNORE.to_owned());
    } else if crate::projects_home::append_lines(
        &ignore,
        &lines,
        "github_publish_gitignore_invalid",
    )? > 0
    {
        written.push(GITIGNORE.to_owned());
    }
    for (path, text) in files {
        write_new(project, path, &text)?;
        written.push(path.to_owned());
    }
    Ok(written)
}

/// Tells whether the .gitignore lines an owner approved can be written: at most 1000, each
/// one plain line of at most `IGNORE_LINE_BYTES_MAX` bytes with no control character and
/// no trailing space. Input: the lines. Output: true when every one is fine.
pub(super) fn lines_valid(approved: &[String]) -> bool {
    approved.len() <= IGNORE_LINES_MAX
        && approved.iter().all(|line| {
            !line.is_empty()
                && line.len() <= IGNORE_LINE_BYTES_MAX
                && !line.chars().any(char::is_control)
                && !line.ends_with(char::is_whitespace)
        })
}

/// Reads a project's .gitignore before publish writes anything: a plain file, never a
/// link or a FIFO, of at most 1 MiB of UTF-8, as `append_lines` then needs it.
/// Input: the path. Output: its text, None when there is none, or
/// `github_publish_gitignore_invalid`.
fn read_gitignore(path: &Path) -> Result<Option<String>, EngineError> {
    let invalid = || failure("github_publish_gitignore_invalid");
    match std::fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Ok(metadata) if metadata.is_file() => {}
        _ => return Err(invalid()),
    }
    let file = crate::platform::open_plain(path).map_err(|_| invalid())?;
    let metadata = file.metadata().map_err(|_| invalid())?;
    if !metadata.is_file() || metadata.len() > GITIGNORE_BYTES_MAX {
        return Err(invalid());
    }
    let mut text = String::new();
    file.take(GITIGNORE_BYTES_MAX + 1)
        .read_to_string(&mut text)
        .map_err(|_| invalid())?;
    if text.len() as u64 > GITIGNORE_BYTES_MAX {
        return Err(invalid());
    }
    Ok(Some(text))
}

/// Replaces a project file with new text through a new file beside it, renamed over the
/// old one, so a link put in its place is replaced, never written through. Inputs: the
/// project, the file and the text. Output: `()`, or `github_publish_gitignore_invalid`.
fn replace_file(project: &Path, path: &Path, text: &str) -> Result<(), EngineError> {
    let invalid = || failure("github_publish_gitignore_invalid");
    let mut token = [0_u8; 8];
    getrandom::fill(&mut token).map_err(|_| invalid())?;
    let name: String = token.iter().map(|byte| format!("{byte:02x}")).collect();
    let temporary = project.join(format!(".gitignore.publish-{name}"));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .map_err(|_| invalid())?;
    let replaced = file
        .write_all(text.as_bytes())
        .and_then(|()| file.sync_all())
        .and_then(|()| std::fs::rename(&temporary, path));
    if replaced.is_err() {
        let _ = std::fs::remove_file(&temporary);
        return Err(invalid());
    }
    Ok(())
}

/// The files publish writes into a project where none of their kind exists: the
/// security policy on a public repository, and Dependabot version updates for each
/// ecosystem the project's top level shows. A `.github` that is not a plain folder is
/// refused. Inputs: the project and whether the repository is private.
/// Output: each file's path and text, or `github_publish_file_invalid`.
fn generated(project: &Path, private: bool) -> Result<Vec<(&'static str, String)>, EngineError> {
    let present = |path: &str| std::fs::symlink_metadata(project.join(path)).is_ok();
    let mut files: Vec<(&'static str, String)> = Vec::new();
    if !private && !SECURITY_PATHS.iter().any(|path| present(path)) {
        files.push((SECURITY_PATHS[0], SECURITY_POLICY.to_owned()));
    }
    let mut ecosystems: Vec<&str> = Vec::new();
    for (marker, ecosystem) in ECOSYSTEMS {
        if present(marker) && !ecosystems.contains(&ecosystem) {
            ecosystems.push(ecosystem);
        }
    }
    if !ecosystems.is_empty() && !DEPENDABOT_PATHS.iter().any(|path| present(path)) {
        let mut text = String::from("version: 2\nupdates:\n");
        for ecosystem in &ecosystems {
            text.push_str(&format!(
                "  - package-ecosystem: \"{ecosystem}\"\n    directory: \"/\"\n    schedule:\n      interval: \"weekly\"\n"
            ));
        }
        files.push((DEPENDABOT, text));
    }
    if files.iter().any(|(path, _)| path.starts_with(".github/")) {
        match std::fs::symlink_metadata(project.join(".github")) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
            _ => return Err(failure("github_publish_file_invalid")),
        }
    }
    Ok(files)
}

/// Writes a new file publish makes, never over or through an existing entry, making its
/// one parent folder when missing. Inputs: the project, the relative path and the text.
/// Output: `()`, or `github_publish_file_invalid`.
fn write_new(project: &Path, path: &str, text: &str) -> Result<(), EngineError> {
    let invalid = || failure("github_publish_file_invalid");
    let target = project.join(path);
    if let Some(parent) = target.parent().filter(|parent| *parent != project) {
        match std::fs::create_dir(parent) {
            Err(error) if error.kind() != std::io::ErrorKind::AlreadyExists => {
                return Err(invalid());
            }
            _ => {}
        }
        let metadata = std::fs::symlink_metadata(parent).map_err(|_| invalid())?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(invalid());
        }
    }
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&target)
        .map_err(|_| invalid())?;
    file.write_all(text.as_bytes())
        .and_then(|()| file.sync_all())
        .map_err(|_| invalid())
}

/// Finds the lines that would push a secret: with no owner history, the files the first
/// commit would take, from the working tree; with owner history, the files of HEAD's
/// tree, from git, since that is what is pushed. Older commits are not read. Inputs: git,
/// its empty home, the project folder and its history. Output: the findings by file and
/// line, or `github_publish_too_large` past 20,000 files or 200 MiB.
pub(super) fn secret_findings(
    git: &Path,
    home: &Path,
    project: &Path,
    history: GitHubHistory,
) -> Result<GitHubSecretScan, EngineError> {
    findings_within(
        git,
        home,
        project,
        history,
        SCAN_FILES_MAX,
        SCAN_BYTES_MAX,
        WORK_TIME,
    )
}

/// Finds the secret lines as [`secret_findings`] does, within the given bounds.
/// Inputs: git, its empty home, the project, its history, the most files and the most
/// bytes read. Output: the findings, or `github_publish_too_large`, `github_publish_failed`.
fn findings_within(
    git: &Path,
    home: &Path,
    project: &Path,
    history: GitHubHistory,
    files_max: usize,
    bytes_max: u64,
    limit: Duration,
) -> Result<GitHubSecretScan, EngineError> {
    let too_large = || failure("github_publish_too_large");
    let failed = || failure("github_publish_failed");
    if !plain_repository(project)? {
        return Err(failure("github_publish_not_plain_repo"));
    }
    let mut findings = Vec::new();
    let mut truncated = false;
    let mut total: u64 = 0;
    if history == GitHubHistory::Owner {
        let listed = listing(
            project_git(git, home, project).args([
                "ls-tree",
                "-r",
                "-z",
                "-l",
                "--full-tree",
                "HEAD",
            ]),
            limit,
        )?;
        let mut blobs: Vec<(String, String)> = Vec::new();
        for entry in listed
            .split(|byte| *byte == 0)
            .filter(|entry| !entry.is_empty())
        {
            let tab = entry
                .iter()
                .position(|byte| *byte == b'\t')
                .ok_or_else(failed)?;
            let meta = std::str::from_utf8(&entry[..tab]).map_err(|_| failed())?;
            let mut words = meta.split_whitespace();
            let (mode, kind, object, size) =
                (words.next(), words.next(), words.next(), words.next());
            if kind != Some("blob") || mode == Some("120000") {
                continue;
            }
            let size: u64 = size.and_then(|size| size.parse().ok()).ok_or_else(failed)?;
            total = total.saturating_add(size);
            let object = object.ok_or_else(failed)?.to_owned();
            blobs.push((
                object,
                String::from_utf8_lossy(&entry[tab + 1..]).into_owned(),
            ));
            if total > bytes_max || blobs.len() > files_max {
                return Err(too_large());
            }
        }
        read_blobs(
            git,
            home,
            project,
            &blobs,
            &mut findings,
            &mut truncated,
            limit,
        )?;
    } else {
        let listed = listing(
            project_git(git, home, project).args([
                "ls-files",
                "-z",
                "--cached",
                "--others",
                "--exclude-standard",
            ]),
            limit,
        )?;
        let paths: Vec<&[u8]> = listed
            .split(|byte| *byte == 0)
            .filter(|path| !path.is_empty())
            .collect();
        if paths.len() > files_max {
            return Err(too_large());
        }
        for path in paths {
            let relative = listed_path(path).ok_or_else(failed)?;
            let Ok(file) = crate::platform::open_plain_beneath(project, &relative) else {
                continue;
            };
            let Ok(metadata) = file.metadata() else {
                continue;
            };
            if !metadata.is_file() {
                continue;
            }
            total = total.saturating_add(metadata.len());
            if total > bytes_max {
                return Err(too_large());
            }
            let mut head = Vec::new();
            file.take(SCAN_FILE_BYTES_MAX)
                .read_to_end(&mut head)
                .map_err(|_| failed())?;
            truncated |= scan(&String::from_utf8_lossy(path), &head, &mut findings);
        }
    }
    findings.sort();
    Ok(GitHubSecretScan {
        findings,
        truncated,
    })
}

/// Turns a path git listed into a path, as raw bytes on Unix and as UTF-8 elsewhere.
/// Input: the listed bytes. Output: the relative path, or None when it cannot be one.
#[cfg(unix)]
fn listed_path(bytes: &[u8]) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    Some(PathBuf::from(std::ffi::OsStr::from_bytes(bytes)))
}

/// Turns a path git listed into a path, as raw bytes on Unix and as UTF-8 elsewhere.
/// Input: the listed bytes. Output: the relative path, or None when it cannot be one.
#[cfg(not(unix))]
fn listed_path(bytes: &[u8]) -> Option<PathBuf> {
    std::str::from_utf8(bytes).ok().map(PathBuf::from)
}

/// Reads blobs through one `git cat-file --batch` within `limit`, scanning the first
/// 1 MiB of each. Inputs: git, its empty home, the project, each blob's id and path, the
/// findings to add to, the flag a cut list sets and the limit. Output: `()`, or
/// `github_publish_failed`.
fn read_blobs(
    git: &Path,
    home: &Path,
    project: &Path,
    blobs: &[(String, String)],
    findings: &mut Vec<GitHubSecretFinding>,
    truncated: &mut bool,
    limit: Duration,
) -> Result<(), EngineError> {
    if blobs.is_empty() {
        return Ok(());
    }
    let failed = || failure("github_publish_failed");
    let mut command = project_git(git, home, project);
    command
        .args(["cat-file", "--batch"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped());
    let watched = Watched::start(&mut command, limit)?;
    let (stdout, stdin) = watched.pipes();
    let mut stdin = stdin.expect("cat-file stdin is piped");
    let names: Vec<u8> = blobs
        .iter()
        .flat_map(|(object, _)| format!("{object}\n").into_bytes())
        .collect();
    let writer = std::thread::spawn(move || stdin.write_all(&names));
    let mut reader = BufReader::new(stdout.expect("cat-file stdout is piped"));
    let read = read_batch(&mut reader, blobs, findings, truncated);
    if read.is_err() {
        watched.stop();
    }
    drop(reader);
    let written = writer.join();
    let status = watched.finish()?;
    read?;
    if !matches!(written, Ok(Ok(()))) || !status.success() {
        return Err(failed());
    }
    Ok(())
}

/// Reads each blob `cat-file --batch` prints, in the order asked: its header, the first
/// 1 MiB scanned and the rest skipped. Inputs: the reader, the blobs, the findings and
/// the flag a cut list sets. Output: `()`, or `github_publish_failed` for a reply that
/// does not match.
fn read_batch(
    reader: &mut BufReader<ChildStdout>,
    blobs: &[(String, String)],
    findings: &mut Vec<GitHubSecretFinding>,
    truncated: &mut bool,
) -> Result<(), EngineError> {
    let failed = || failure("github_publish_failed");
    for (object, path) in blobs {
        let mut header = Vec::new();
        reader
            .by_ref()
            .take(BATCH_HEADER_BYTES_MAX)
            .read_until(b'\n', &mut header)
            .map_err(|_| failed())?;
        let header = std::str::from_utf8(&header).map_err(|_| failed())?;
        let mut words = header.split_whitespace();
        if !header.ends_with('\n')
            || words.next() != Some(object.as_str())
            || words.next() != Some("blob")
        {
            return Err(failed());
        }
        let size: u64 = words
            .next()
            .and_then(|size| size.parse().ok())
            .ok_or_else(failed)?;
        let kept = size.min(SCAN_FILE_BYTES_MAX);
        let mut head = Vec::new();
        reader
            .by_ref()
            .take(kept)
            .read_to_end(&mut head)
            .map_err(|_| failed())?;
        let rest = size - kept + 1;
        let skipped = std::io::copy(&mut reader.by_ref().take(rest), &mut std::io::sink())
            .map_err(|_| failed())?;
        if head.len() as u64 != kept || skipped != rest {
            return Err(failed());
        }
        *truncated |= scan(path, &head, findings);
    }
    Ok(())
}

/// Adds a finding for each line of a file's first bytes that holds a secret: a private
/// key block, a known token, or, in a settings file only, a credential-named key with a
/// literal value. A file with a NUL in its first 8 KiB is binary and skipped. At most 20
/// findings come from one file and 1000 in all. Inputs: the path, the bytes and the
/// findings. Output: true when a line holding a secret was left out at those bounds.
fn scan(path: &str, head: &[u8], findings: &mut Vec<GitHubSecretFinding>) -> bool {
    if head[..head.len().min(BINARY_PROBE_BYTES)].contains(&0) {
        return false;
    }
    let settings = settings_file(path);
    let mut found = 0_usize;
    for (number, line) in (1u32..).zip(head.split(|byte| *byte == b'\n')) {
        let line = String::from_utf8_lossy(line);
        let kind = if line.contains("-----BEGIN ") && line.contains("PRIVATE KEY-----") {
            GitHubSecretKind::PrivateKey
        } else if token(&line) {
            GitHubSecretKind::Token
        } else if settings && named_value(&line) {
            GitHubSecretKind::NamedValue
        } else {
            continue;
        };
        if found == FILE_FINDINGS_MAX || findings.len() == FINDINGS_MAX {
            return true;
        }
        findings.push(GitHubSecretFinding {
            path: path.to_owned(),
            line: number,
            kind,
        });
        found += 1;
    }
    false
}

/// Tells whether a line holds a known token: a run of letters, digits, `-` and `_` that
/// starts with a known prefix and has at least 16 more bytes holding a digit and an
/// uppercase letter, as generated tokens do and identifiers do not; a JSON Web Token
/// prefix also needs the `.eyJ` of its second part, which a base64 source map lacks.
/// Input: the line. Output: true when it holds one.
fn token(line: &str) -> bool {
    let bytes = line.as_bytes();
    let name = |byte: u8| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_';
    let mut start = 0;
    while start < bytes.len() {
        if !name(bytes[start]) {
            start += 1;
            continue;
        }
        let end = start
            + bytes[start..]
                .iter()
                .take_while(|byte| name(**byte))
                .count();
        let run = &line[start..end];
        let found = SECRET_PREFIXES.iter().any(|prefix| {
            run.strip_prefix(prefix).is_some_and(|body| {
                body.len() >= TOKEN_BODY_MIN
                    && body.bytes().any(|byte| byte.is_ascii_digit())
                    && body.bytes().any(|byte| byte.is_ascii_uppercase())
                    && (*prefix != "eyJ" || line[end..].starts_with(".eyJ"))
            })
        });
        if found {
            return true;
        }
        start = end;
    }
    false
}

/// Tells whether a file is a settings file, where a credential-named key's value is a
/// literal rather than code: env files, package manager and netrc credentials, and
/// properties, ini, cfg, conf, TOML, YAML and JSON files. Input: the path.
/// Output: true for a settings file.
fn settings_file(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path).to_ascii_lowercase();
    SETTINGS_NAMES.contains(&name.as_str())
        || name.starts_with(".env.")
        || name.ends_with(".env")
        || SETTINGS_EXTENSIONS
            .iter()
            .any(|extension| name.ends_with(extension))
}

/// Tells whether a settings line sets a credential-named key to a literal value: the
/// first `=` or `:` whose key, quotes trimmed, ends like a credential name, followed by a
/// quoted or bare value that is not a placeholder. Comment lines are skipped.
/// Input: the line. Output: true when it holds such a value.
fn named_value(line: &str) -> bool {
    let trimmed = line.trim_start();
    let trimmed = trimmed.strip_prefix("export ").unwrap_or(trimmed);
    if trimmed.starts_with(['#', ';', '!']) {
        return false;
    }
    for (at, separator) in trimmed.match_indices(['=', ':']) {
        let key = trimmed[..at].trim_end().trim_end_matches(['"', '\'']);
        let start = key
            .bytes()
            .rposition(|byte| !(byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_'))
            .map_or(0, |at| at + 1);
        let name = key[start..].to_ascii_lowercase().replace('-', "_");
        let credential = KEY_NAMES.contains(&name.as_str())
            || KEY_ENDS.iter().any(|ending| name.ends_with(ending));
        if credential {
            return literal(&trimmed[at + separator.len()..]);
        }
    }
    false
}

/// Tells whether a setting's value is a literal secret: the quoted text or the bare word,
/// at least 8 bytes, not a placeholder or a reference, and not a plain lowercase word,
/// name or path, since it holds a digit, mixed case or a symbol. Input: the text after
/// the separator. Output: true for a literal secret.
fn literal(rest: &str) -> bool {
    let rest = rest.trim_start();
    let value = match rest.chars().next() {
        Some(quote @ ('"' | '\'')) => rest[1..].split(quote).next().unwrap_or(""),
        _ => rest
            .split(|c: char| c.is_whitespace() || c == ',' || c == ';')
            .next()
            .unwrap_or(""),
    };
    let lower = value.to_ascii_lowercase();
    let placeholder = value.starts_with(['$', '%', '<', '{', '[', '!', '*'])
        || lower.starts_with("your")
        || lower.starts_with("process.env")
        || lower.starts_with("os.environ")
        || lower.starts_with("env(")
        || [
            "example",
            "changeme",
            "change_me",
            "placeholder",
            "redacted",
        ]
        .iter()
        .any(|word| lower.contains(word))
        || PLACEHOLDERS.contains(&lower.as_str())
        || value
            .bytes()
            .all(|byte| matches!(byte, b'x' | b'X' | b'*' | b'.' | b'-' | b'_' | b'0'));
    if value.len() < NAMED_VALUE_MIN || placeholder {
        return false;
    }
    let digit = value.bytes().any(|byte| byte.is_ascii_digit());
    let mixed = value.bytes().any(|byte| byte.is_ascii_uppercase())
        && value.bytes().any(|byte| byte.is_ascii_lowercase());
    let symbol = value
        .bytes()
        .any(|byte| !byte.is_ascii_alphanumeric() && !matches!(byte, b'_' | b'-' | b'.' | b'/'));
    digit || mixed || symbol
}

/// The commit a publish pushes: the branch it is on, its id, and how many changed files
/// publish left out of it because they are the owner's work in progress.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GitHubCommitted {
    pub branch: String,
    pub commit: String,
    pub left_out: usize,
}

/// The name and email a publish commit is made with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Author {
    pub(super) name: String,
    pub(super) email: String,
}

/// The identity the owner's own git commits with in the project: git reads the project's
/// settings and the owner's own, from the owner's home, as a commit there would. A name
/// or email git would refuse or mangle counts as none. Inputs: git, the owner's home
/// folder and the project. Output: the author, or None.
pub(super) fn configured_author(git: &Path, owner_home: &Path, project: &Path) -> Option<Author> {
    let value = |key: &str| {
        let mut command = clone::git_command(git, owner_home);
        command
            .env_remove("GIT_CONFIG_GLOBAL")
            .current_dir(project)
            .env("GIT_DIR", project.join(".git"))
            .args(GIT_SAFE)
            .args(["config", "--get", key])
            .stdin(Stdio::null())
            .stderr(Stdio::null());
        let output = run(&mut command).ok()?;
        let text = String::from_utf8(output.stdout)
            .ok()
            .filter(|_| output.status.success())?;
        let text = text.strip_suffix('\n').unwrap_or(&text).to_owned();
        let fine = !text.trim().is_empty()
            && text.len() <= AUTHOR_BYTES_MAX
            && !text.chars().any(|c| c.is_control() || c == '<' || c == '>');
        fine.then_some(text)
    };
    let name = value("user.name")?;
    let email = value("user.email")
        .filter(|email| email.contains('@') && !email.contains(char::is_whitespace))?;
    Some(Author { name, email })
}

/// The GitHub account's noreply identity, which GitHub links to the account without
/// showing a real address. Inputs: the login and the numeric id when GitHub sent it.
/// Output: the author.
pub(super) fn noreply_author(login: &str, id: Option<u64>) -> Author {
    let email = match id {
        Some(id) => format!("{id}+{login}@users.noreply.github.com"),
        None => format!("{login}@users.noreply.github.com"),
    };
    Author {
        name: login.to_owned(),
        email,
    }
}

/// Makes the publish commit, "Publish with CrazyAgent", by the author. With no history,
/// or only the engine's start commit, it takes everything the ignore rules keep. With
/// owner history it takes only the files publish wrote, and only when they changed, so
/// the owner's work in progress, staged or not, stays as it is and is counted.
/// Inputs: git, its empty home, the project, the paths prepare wrote and the author.
/// Output: the commit to push, or `github_publish_file_invalid` for a path publish does
/// not write, `github_publish_detached`, `github_publish_not_plain_repo`,
/// `github_publish_hooks_set`, `github_publish_too_large`, `github_publish_failed`.
pub(super) fn commit(
    git: &Path,
    home: &Path,
    project: &Path,
    written: &[String],
    author: &Author,
) -> Result<GitHubCommitted, EngineError> {
    commit_within(git, home, project, written, author, LISTING_BYTES_MAX)
}

/// Makes the publish commit as [`commit`] does, reading at most `status_max` bytes of the
/// status that counts what the commit leaves out. The status is read before anything is
/// staged or committed, so a count that cannot be made leaves HEAD and the index as they
/// were. Inputs: as [`commit`], and the cap. Output: as [`commit`], or
/// `github_publish_too_large` past the cap.
fn commit_within(
    git: &Path,
    home: &Path,
    project: &Path,
    written: &[String],
    author: &Author,
    status_max: u64,
) -> Result<GitHubCommitted, EngineError> {
    let failed = || failure("github_publish_failed");
    if written.iter().any(|path| !WRITTEN.contains(&path.as_str())) {
        return Err(failure("github_publish_file_invalid"));
    }
    let paths: Vec<&str> = WRITTEN
        .into_iter()
        .filter(|known| written.iter().any(|path| path == known))
        .collect();
    if !plain_repository(project)? {
        return Err(failure("github_publish_not_plain_repo"));
    }
    refuse_hooks(git, home, project)?;
    let (branch, history) = head_state(git, home, project, LOOKUP_TIME)?;
    let status = listing_within(
        project_git(git, home, project).args([
            "status",
            "--porcelain",
            "-z",
            "--no-renames",
            "--untracked-files=all",
        ]),
        WORK_TIME,
        status_max,
    )?;
    let taken = |entry: &[u8]| {
        history != GitHubHistory::Owner
            || entry
                .get(3..)
                .is_some_and(|path| paths.iter().any(|known| known.as_bytes() == path))
    };
    let left_out = status
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty() && !taken(*entry))
        .count();
    let succeeded = |command: &mut Command| -> Result<(), EngineError> {
        let output = work(command)?;
        if output.status.success() {
            Ok(())
        } else {
            Err(failed())
        }
    };
    let committing = || {
        let mut command = project_git(git, home, project);
        command
            .env("GIT_AUTHOR_NAME", &author.name)
            .env("GIT_AUTHOR_EMAIL", &author.email)
            .env("GIT_COMMITTER_NAME", &author.name)
            .env("GIT_COMMITTER_EMAIL", &author.email)
            .args(["-c", "commit.gpgSign=false", "-c", "gc.auto=0"])
            .args(["-c", "maintenance.auto=false"])
            .args(["commit", "-q", "--no-verify", "-m", MESSAGE]);
        command
    };
    if history == GitHubHistory::Owner {
        if !paths.is_empty() {
            succeeded(
                project_git(git, home, project)
                    .args(["add", "-f", "--"])
                    .args(&paths),
            )?;
            let changed = work(
                project_git(git, home, project)
                    .args(["diff", "--cached", "--quiet", "HEAD", "--"])
                    .args(&paths),
            )?;
            match changed.status.code() {
                Some(0) => {}
                Some(1) => succeeded(committing().args(["--only", "--"]).args(&paths))?,
                _ => return Err(failed()),
            }
        }
    } else {
        succeeded(project_git(git, home, project).args(["add", "-A"]))?;
        succeeded(committing().arg("--allow-empty"))?;
    }
    let commit = head_commit(git, home, project)?;
    Ok(GitHubCommitted {
        branch,
        commit,
        left_out,
    })
}

/// Reads the commit HEAD points at. Inputs: git, its empty home and the project.
/// Output: the commit id, or `github_publish_failed`.
fn head_commit(git: &Path, home: &Path, project: &Path) -> Result<String, EngineError> {
    let output =
        run(project_git(git, home, project).args(["rev-parse", "--verify", "-q", "HEAD"]))?;
    let commit = String::from_utf8(output.stdout)
        .ok()
        .filter(|_| output.status.success())
        .map(|text| text.trim_end().to_owned())
        .filter(|commit| {
            matches!(commit.len(), 40 | 64) && commit.bytes().all(|byte| byte.is_ascii_hexdigit())
        });
    commit.ok_or_else(|| failure("github_publish_failed"))
}

/// Tells whether a commit's tree holds GitHub Actions workflows, which a push needs the
/// `workflow` scope for. Inputs: git, its empty home, the project and the commit.
/// Output: true when `.github/workflows` is in it, or `github_publish_failed`.
pub(super) fn has_workflows(
    git: &Path,
    home: &Path,
    project: &Path,
    commit: &str,
) -> Result<bool, EngineError> {
    if commit.is_empty() || !commit.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(failure("github_publish_failed"));
    }
    let listed = run(project_git(git, home, project).args([
        "ls-tree",
        "--name-only",
        commit,
        "--",
        ".github/workflows",
    ]))?;
    if !listed.status.success() {
        return Err(failure("github_publish_failed"));
    }
    Ok(!listed.stdout.is_empty())
}

/// Pushes the publish commit to `owner/name` and then points the project at it. The push
/// runs from a private staging repository in git's empty home that borrows the project's
/// objects and holds one branch, so no setting of the project's own, such as a URL
/// rewrite, an extra header or a credential helper, ever meets the token. The project
/// gets its origin, upstream and remote branch only after the push landed. Inputs: the
/// endpoints, the store's helper, git, its empty home, the project, `owner/name` and the
/// commit. Output: `()`, or `github_repo_name_invalid`, `github_publish_head_moved` when
/// HEAD is no longer that branch and commit, `github_publish_push_failed`,
/// `github_publish_push_timed_out`, `github_publish_not_plain_repo`,
/// `github_publish_hooks_set`, `github_publish_failed`.
pub(super) fn push(
    endpoints: &super::Endpoints,
    helper: &str,
    git: &Path,
    home: &Path,
    project: &Path,
    full_name: &str,
    committed: &GitHubCommitted,
) -> Result<(), EngineError> {
    let failed = || failure("github_publish_failed");
    let (remote, scheme) = clone::remote_url(endpoints, full_name)?;
    if !plain_repository(project)? {
        return Err(failure("github_publish_not_plain_repo"));
    }
    refuse_hooks(git, home, project)?;
    let (branch, _) = head_state(git, home, project, LOOKUP_TIME)?;
    let commit = head_commit(git, home, project)?;
    if branch != committed.branch || commit != committed.commit {
        return Err(failure("github_publish_head_moved"));
    }
    let config = project.join(".git").join("config");
    match std::fs::symlink_metadata(&config) {
        Ok(metadata) if metadata.is_file() => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        _ => return Err(failure("github_publish_not_plain_repo")),
    }
    let objects = project.join(".git").join("objects");
    let mut alternates = objects.as_os_str().as_encoded_bytes().to_vec();
    if alternates.contains(&b'\n') {
        return Err(failed());
    }
    alternates.push(b'\n');
    let stage = home.join(STAGE);
    let reference = format!("refs/heads/{branch}");
    let mut init = clone::git_command(git, home);
    init.args(GIT_SAFE)
        .args(["init", "--bare", "-q", "--template="]);
    if commit.len() == 64 {
        init.arg("--object-format=sha256");
    }
    let made = run(init
        .arg("--")
        .arg(&stage)
        .stdin(Stdio::null())
        .stderr(Stdio::null()))?;
    if !made.status.success() {
        return Err(failed());
    }
    let info = stage.join("objects").join("info");
    std::fs::create_dir_all(&info)
        .and_then(|()| std::fs::write(info.join("alternates"), &alternates))
        .map_err(|_| failed())?;
    let staged = run(clone::git_command(git, home)
        .env("GIT_DIR", &stage)
        .args(GIT_SAFE)
        .args(["update-ref", reference.as_str(), commit.as_str()])
        .stdin(Stdio::null())
        .stderr(Stdio::null()))?;
    if !staged.status.success() {
        return Err(failed());
    }
    clone::finish(
        clone::credential_git(git, home, helper, scheme)
            .env("GIT_DIR", &stage)
            .args(["push", "--quiet", "--no-verify", "--"])
            .arg(remote.as_str())
            .arg(format!("{reference}:{reference}"))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped()),
        PUSH_TIME,
        "github_publish_push_failed",
        "github_publish_push_timed_out",
    )?;
    let settings = [
        ("remote.origin.url".to_owned(), remote.as_str()),
        ("remote.origin.fetch".to_owned(), FETCH),
        (format!("branch.{branch}.remote"), "origin"),
        (format!("branch.{branch}.merge"), reference.as_str()),
    ];
    for (key, value) in &settings {
        let set = run(clone::git_command(git, home)
            .args(["config", "--file"])
            .arg(&config)
            .args(["--replace-all", key.as_str(), *value])
            .stdin(Stdio::null())
            .stderr(Stdio::null()))?;
        if !set.status.success() {
            return Err(failed());
        }
    }
    let tracking = format!("refs/remotes/origin/{branch}");
    let tracked = run(project_git(git, home, project).args([
        "update-ref",
        tracking.as_str(),
        commit.as_str(),
    ]))?;
    let origin = run(clone::git_command(git, home)
        .args(["config", "--file"])
        .arg(&config)
        .args(["--no-includes", "--get-all", "remote.origin.url"])
        .stdin(Stdio::null())
        .stderr(Stdio::null()))?;
    if !tracked.status.success() || origin.stdout != format!("{remote}\n").as_bytes() {
        return Err(failed());
    }
    Ok(())
}
