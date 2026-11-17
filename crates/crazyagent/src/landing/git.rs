use crate::engine::EngineError;
use std::collections::BTreeSet;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub(super) const OUTPUT_BYTES_MAX: usize = 64 * 1024 * 1024;
pub(super) const LISTING_BYTES_MAX: usize = 64 * 1024;
const STEP_TIME: Duration = Duration::from_secs(90);
const POLL: Duration = Duration::from_millis(2);
/// The oldest git that every engine git call runs on, except the two that
/// [`MERGE_VERSION_MIN`] guards: 2.39, the Apple Git-154 of Xcode 16 and its Command Line
/// Tools (W-GIT-FLOOR).
const VERSION_MIN: (u32, u32) = (2, 39);
/// The oldest git with `merge-tree --merge-base` and `check-attr --source`, both new in
/// git 2.40.0, which a squash merge and the attribute checks around it run (W-GIT-FLOOR).
const MERGE_VERSION_MIN: (u32, u32) = (2, 40);

#[cfg(test)]
thread_local! {
    /// What `git version` prints for engine git on this thread in a row, in place of the real
    /// git's answer, so a row can shape the git as an older one (W-GIT-FLOOR).
    pub(super) static VERSION_PRINTED: std::cell::RefCell<Option<String>> =
        const { std::cell::RefCell::new(None) };
}
const EXCLUDES_PATH_BYTES_MAX: usize = 1024;
/// The longest the owner's ignore file is looked up for in a repository (RV-DS241-145).
const EXCLUDES_LOOKUP_TIME: Duration = Duration::from_secs(10);
const CHECK_LINE_BYTES_MAX: usize = 80;

/// The excludes setting a confined step's git takes in place of the owner's ignore file,
/// which a step that writes an object or the work tree may not read (SA vid 2,
/// [`Repo::step_excludes`]): an empty file, so git applies no excludes file at all and never
/// warns about one it cannot open.
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub(super) const NO_EXCLUDES: &str = "core.excludesFile=/dev/null";

#[cfg(test)]
thread_local! {
    /// The owner home a landing test stands in for the process's HOME, so engine git in
    /// that test reads that home's global ignore rules. None keeps the process's HOME.
    pub(super) static OWNER_HOME: std::cell::RefCell<Option<PathBuf>> =
        const { std::cell::RefCell::new(None) };
    /// How many git commands engine git started on this thread, so a landing test can
    /// count what one read costs.
    pub(super) static GIT_RUNS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// The first argument of each git command engine git started on this thread on a work
    /// tree with no confinement, so a landing test can tell which steps read an agent's work
    /// tree with the engine's own rights.
    pub(super) static UNCONFINED_WORK: std::cell::RefCell<Vec<String>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

#[cfg(all(test, target_os = "linux"))]
thread_local! {
    /// The Landlock helper a landing test stands in for the build folder's, so a test can
    /// take it away. None keeps the build folder's helper.
    pub(super) static LANDLOCK_HELPER: std::cell::RefCell<Option<PathBuf>> =
        const { std::cell::RefCell::new(None) };
}

/// The folder the engine makes in a linked worktree's git folder for a confined Linux step:
/// the only place in the git folder the step writes, holding its copy of the index.
#[cfg(target_os = "linux")]
const STEP_FOLDER: &str = "crazyagent-step";

/// The exit git gives when it finds the index already locked, which a confined Linux step
/// answers with in git's place, since the engine takes that lock for it.
#[cfg(target_os = "linux")]
const INDEX_LOCKED: i32 = 128;

/// The most folders of a repository's `.git/worktrees` the start sweep looks in for a lock
/// a stopped confined Linux step left ([`clear_step_locks`]).
#[cfg(target_os = "linux")]
const STEP_LOCK_FOLDERS_MAX: usize = 512;

/// The most bytes the start sweep reads of an `index.lock`: more than the longest text a
/// confined Linux step writes there ([`step_lock_text`]), so a longer lock is never the
/// engine's.
#[cfg(target_os = "linux")]
const STEP_LOCK_TEXT_MAX: u64 = 128;

/// Removes the `index.lock` a confined Linux step of a stopped engine left: only in a linked
/// worktree's git folder, only a plain file read without following a link, and only one
/// whose whole text names an engine process that is gone ([`left_by_gone_step`]), so a lock
/// another git took, or one a running engine holds, stays (RV-DS241-135). With such a lock,
/// or with no `index.lock` at all, the step folder goes too, where a step's git, killed with
/// its engine, can have left its own lock (RV-DS241-142). The engine runs it once at start,
/// before any landing step. Input: the repository's canonical root. Output: how many locks it
/// removed.
#[cfg(target_os = "linux")]
pub(super) fn clear_step_locks(root: &Path) -> usize {
    let real = |path: &Path| std::fs::symlink_metadata(path).is_ok_and(|meta| meta.is_dir());
    let worktrees = root.join(".git").join("worktrees");
    if !real(&root.join(".git")) || !real(&worktrees) {
        return 0;
    }
    let Ok(folders) = std::fs::read_dir(&worktrees) else {
        return 0;
    };
    let mut removed = 0;
    for folder in folders.take(STEP_LOCK_FOLDERS_MAX).flatten() {
        let private = folder.path();
        let lock = private.join("index.lock");
        let unlocked = std::fs::symlink_metadata(&lock)
            .err()
            .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound);
        if real(&private) && unlocked {
            let _ = std::fs::remove_dir_all(private.join(STEP_FOLDER));
        } else if real(&private)
            && std::fs::symlink_metadata(&lock).is_ok_and(|meta| meta.is_file())
            && left_by_gone_step(&lock)
            && std::fs::remove_dir_all(private.join(STEP_FOLDER))
                .err()
                .is_none_or(|error| error.kind() == std::io::ErrorKind::NotFound)
            && std::fs::remove_file(&lock).is_ok()
        {
            removed += 1;
        }
    }
    assert!(removed <= STEP_LOCK_FOLDERS_MAX);
    removed
}

/// The text a confined Linux step writes in the `index.lock` it takes, naming the engine
/// process by its id and start, so the start sweep can tell that lock from one another git
/// took. Inputs: the process id and its start identity. Output: the text.
#[cfg(target_os = "linux")]
fn step_lock_text(pid: i32, started: &str) -> String {
    format!("{STEP_FOLDER} {pid} {started}\n")
}

/// Tells whether an `index.lock` is one a confined Linux step of an engine process that is
/// gone took: a plain file, opened without following a link or waiting on a FIFO, whose
/// whole text is [`step_lock_text`] for a process id and start that no running process has.
/// Input: the lock. Output: false for any other lock, one a running process took, or one
/// whose process cannot be read.
#[cfg(target_os = "linux")]
fn left_by_gone_step(lock: &Path) -> bool {
    use std::os::unix::fs::OpenOptionsExt;
    let mut text = String::new();
    let read = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(lock)
        .and_then(|file| {
            if !file.metadata()?.is_file() {
                return Err(std::io::ErrorKind::InvalidInput.into());
            }
            file.take(STEP_LOCK_TEXT_MAX).read_to_string(&mut text)
        });
    let named = text
        .strip_prefix(STEP_FOLDER)
        .and_then(|rest| rest.strip_prefix(' '))
        .and_then(|rest| rest.strip_suffix('\n'))
        .and_then(|rest| rest.split_once(' '))
        .and_then(|(pid, started)| Some((pid.parse::<i32>().ok()?, started)));
    let Some((pid, started)) = named else {
        return false;
    };
    if read.is_err()
        || pid <= 1
        || started.is_empty()
        || !started.bytes().all(|byte| byte.is_ascii_digit())
        || step_lock_text(pid, started) != text
    {
        return false;
    }
    match crate::storage::harness_groups::start_identity(pid) {
        Ok(None) => true,
        Ok(Some(now)) => now != started,
        Err(_) => false,
    }
}

/// The Landlock helper a confined Linux step starts: the one shipped beside the engine, or
/// for a unit test, which runs from the build folder's `deps`, the one built in that build
/// folder unless the test stands in another (`LANDLOCK_HELPER`). Output: the path, or None
/// when the engine's own executable is unknown.
#[cfg(target_os = "linux")]
fn landlock_helper() -> Option<PathBuf> {
    #[cfg(test)]
    if let Some(helper) = LANDLOCK_HELPER.with(|helper| helper.borrow().clone()) {
        return Some(helper);
    }
    let engine = std::env::current_exe().ok()?;
    #[cfg(test)]
    let engine = engine.parent()?.to_path_buf();
    Some(engine.with_file_name(crate::sandbox::linux::HELPER_NAME))
}

/// The settings every engine git call carries on top of the research runner's
/// `GIT_SAFE`: no attributes file from outside the repository, no automatic upkeep, the
/// case-folding path checks on, no signing, no untracked or split index caches, no
/// reflog created for a ref update and no commit-graph file, which a confined process could
/// write to make git answer ancestry wrongly (land-19 r1 M1), and no advice about the grafts
/// file the command's `GIT_GRAFT_FILE=/dev/null` names (land-19 r2).
const ENGINE_SETTINGS: [&str; 22] = [
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
    "-c",
    "core.commitGraph=false",
    "-c",
    "advice.graftFileDeprecated=false",
];

/// The git arguments that list every include entry of every config file git reads, with
/// the file each came from, NUL-separated: `include.path` and `includeIf.<condition>.path`,
/// whatever the condition says now. Git answers 1 with nothing printed when there is none.
#[cfg(unix)]
const INCLUDE_KEYS: [&str; 5] = [
    "config",
    "--show-origin",
    "-z",
    "--get-regexp",
    "^include(if\\..+)?\\.path$",
];

pub(super) fn failure(code: &str) -> EngineError {
    EngineError(code.into())
}

/// What one git process ended with: its exit code, its standard output up to the
/// caller's bound, and whether it printed more than that.
pub(super) struct Output {
    pub(super) code: i32,
    pub(super) stdout: Vec<u8>,
    pub(super) overflow: bool,
}

/// The owner's home and XDG config folder as the engine process sees them, or the home
/// a landing test set in their place. Takes nothing. Returns the home and the XDG
/// config folder, each none when unset.
#[cfg(unix)]
fn owner_folders() -> (Option<PathBuf>, Option<PathBuf>) {
    #[cfg(test)]
    if let Some(home) = OWNER_HOME.with(|home| home.borrow().clone()) {
        return (Some(home), None);
    }
    (
        std::env::var_os("HOME").map(PathBuf::from),
        std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from),
    )
}

/// Finds the ignore file the owner's own git applies in a repository (RV-DS241-145): the
/// `core.excludesFile` git resolves there, by git's own precedence, from the repository's
/// worktree and local config and the owner's global config, whose `includeIf` sections are
/// matched against that repository, read by a git that sees only the owner's home, no
/// system config, and runs nothing, given `EXCLUDES_LOOKUP_TIME`; else git's default
/// `$XDG_CONFIG_HOME/git/ignore` or `~/.config/git/ignore`. A repository's own value that
/// is not usable gives none, never the owner's file in its place. Inputs: the owner's home
/// and XDG config folder and where the repository is. Output: the file's real path when it
/// is a regular file owned by the engine's user or root, such as a store-managed link.
#[cfg(unix)]
pub(super) fn owner_excludes(
    home: &Path,
    xdg: Option<&Path>,
    within: super::Within<'_>,
) -> Option<PathBuf> {
    use std::os::unix::fs::MetadataExt;
    if !home.is_absolute() {
        return None;
    }
    let xdg = xdg.filter(|xdg| xdg.is_absolute());
    let mut command = Command::new(crate::research::runner::setup_git());
    command
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("LC_ALL", "C")
        .env("HOME", home)
        .envs(crate::research::runner::GIT_SAFE_ENV)
        .args(["config", "--type=path", "--get", "core.excludesFile"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    match within {
        super::Within::Folder(folder) => command.current_dir(folder),
        super::Within::GitDir(git_dir) => command.env("GIT_DIR", git_dir).current_dir(git_dir),
    };
    if let Some(xdg) = xdg {
        command.env("XDG_CONFIG_HOME", xdg);
    }
    let (code, stdout) = lookup(&mut command)?;
    let named = match code {
        0 => PathBuf::from(String::from_utf8(stdout).ok()?.strip_suffix('\n')?),
        1 => xdg
            .map_or_else(|| home.join(".config"), Path::to_path_buf)
            .join("git")
            .join("ignore"),
        _ => return None,
    };
    if !named.is_absolute() {
        return None;
    }
    let real = std::fs::canonicalize(&named).ok()?;
    let metadata = std::fs::metadata(&real).ok()?;
    let owner = metadata.uid();
    let text = real.to_str()?;
    let fits = text.len() <= EXCLUDES_PATH_BYTES_MAX && !text.chars().any(char::is_control);
    (metadata.is_file() && (owner == rustix::process::getuid().as_raw() || owner == 0) && fits)
        .then_some(real)
}

/// Runs one git lookup that prints a short line, given `EXCLUDES_LOOKUP_TIME`, since the
/// repository config it reads can name an include that never ends, such as a FIFO. Takes
/// the command, its output piped. Returns its exit code and at most
/// `EXCLUDES_PATH_BYTES_MAX + 2` bytes of output, or none when git could not start, ran
/// over or its output could not be read.
#[cfg(unix)]
fn lookup(command: &mut Command) -> Option<(i32, Vec<u8>)> {
    let mut child = crate::spawn_gate::spawn(command).ok()?;
    let Some(stdout) = child.stdout.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return None;
    };
    let reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let mut source = stdout.take(EXCLUDES_PATH_BYTES_MAX as u64 + 2);
        let read = source.read_to_end(&mut bytes);
        let _ = std::io::copy(&mut source.into_inner(), &mut std::io::sink());
        read.map(|_| bytes)
    });
    let deadline = Instant::now() + EXCLUDES_LOOKUP_TIME;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(POLL),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    let stdout = reader.join().ok()?.ok()?;
    assert!(stdout.len() <= EXCLUDES_PATH_BYTES_MAX + 2);
    Some((status?.code()?, stdout))
}

/// The `core.excludesFile=<path>` setting naming the ignore file the owner's own git applies
/// where it is asked ([`owner_excludes`]), for an engine git that clears its environment yet
/// must keep out what the owner's own git ignores (RV-DS241-73). Asked in a repository, it
/// names the file git would pick there, a repository's own value first (RV-DS241-145), as
/// landing applies it; asked outside any repository, as onboarding does from `/`, it names
/// the owner's unscoped global file, which onboarding applies beside a repository's own
/// (RV-DS241-145 r2). Takes where to look. Returns the setting, none when there is no usable
/// ignore file or its path is not UTF-8.
#[cfg(unix)]
pub(crate) fn owner_excludes_setting(within: super::Within<'_>) -> Option<String> {
    let (home, xdg) = owner_folders();
    let path = owner_excludes(&home?, xdg.as_deref(), within)?;
    Some(format!("core.excludesFile={}", path.to_str()?))
}

/// A repository the engine runs git on through a git folder and work tree it computed
/// itself, never a `.git` file in the work tree. Every call runs the real git, not the
/// `/usr/bin/git` xcrun shim where xcrun resolves it, with a cleared environment, no
/// system or global config, no system attributes, hooks, fsmonitor, pager, submodule
/// recursion, protocols or upkeep, and with every filter
/// driver the repository's own config names blanked. A work tree also honours the
/// ignore file the owner's git applies in that repository, its own value first, so what the
/// owner's git ignores the engine never stages.
#[derive(Clone)]
pub(super) struct Repo {
    git_dir: PathBuf,
    work_tree: Option<PathBuf>,
    overrides: Vec<String>,
    excludes: Option<String>,
    index: Option<PathBuf>,
    confinement: Vec<std::ffi::OsString>,
    /// For a confined Linux step that may write the work tree, the confinement of its
    /// index-only gits, which write only the engine's step folder ([`Self::moved`]); empty for
    /// every other repository.
    #[cfg(target_os = "linux")]
    index_steps: Vec<std::ffi::OsString>,
    /// Whether a confined Linux step works on a copy of the worktree's own index
    /// ([`Self::swapped`]): false for one on the engine's private index and for every
    /// repository that is not confined.
    #[cfg(target_os = "linux")]
    copies: bool,
}

/// The Seatbelt profile of one engine git step on an agent's work tree (SECAUDIT
/// unconfined-git-add-reads-through-raced-symlink, unconfined-read-tree-u): it reads only the
/// system, its own toolchain, the work tree, the repository's git folder, the owner's ignore
/// file for a reading step alone (/dev/null for any other, [`Repo::step_excludes`]) and the
/// index it works on with its lock, which git opens read-write, and writes only
/// what the step names and that index, the worktree's own or the engine's private one, never
/// the repository's `hooks` or `config`, whatever an agent swaps a path for during the step.
/// Paths reach it as parameters, so no path text is read as profile text.
#[cfg(target_os = "macos")]
const WORK_TREE_PROFILE: &str = "(version 1)\n\
(allow default)\n\
(deny file-read-data (require-not (require-any (literal \"/\") (subpath \"/System\") (subpath \"/usr/lib\") (subpath \"/usr/share\") (subpath \"/dev\") (subpath (param \"TOOLS\")) (subpath (param \"WORK\")) (subpath (param \"COMMON\")) (literal (param \"EXCLUDES\")) (literal (param \"INDEX\")) (literal (param \"INDEX_LOCK\")))))\n\
(deny file-write* (require-not (require-any (literal \"/dev/null\") (subpath (param \"WRITE\")) (literal (param \"INDEX\")) (literal (param \"INDEX_LOCK\")))))\n";

/// What one confined git step may write besides the index it works on: the object store,
/// for a step that hashes the work tree, the work tree, for one that fills it, or nothing,
/// for one that only reads it ([`Repo::reading`]). Only a step that writes nothing but its
/// index may read the owner's ignore file, on macOS and Linux alike, so a link swapped in can
/// never hash it (SA vid 2, [`Repo::step_excludes`]).
#[derive(Clone, Copy)]
pub(super) enum Writes {
    Objects,
    WorkTree,
    Index,
}

impl Repo {
    /// Opens a repository for engine git calls: lists the filter drivers to blank, finds the
    /// ignore file for a work tree, and refuses a config hook or an include placed in the
    /// checkout or work tree (rv-sec2 N1). The git folder is the owner checkout's `.git` or a
    /// linked worktree's, never an agent's clone (CC-N1). Inputs: the checkout root, the git
    /// folder and the work tree, if any. Output: the repository or a `changes_git_*` refusal.
    #[cfg(unix)]
    pub(super) fn open(
        repository: &Path,
        git_dir: &Path,
        work_tree: Option<&Path>,
    ) -> Result<Self, EngineError> {
        let metadata = repository.join(".git");
        assert!(
            git_dir == metadata || git_dir.parent() == Some(metadata.join("worktrees").as_path()),
            "engine git opens only the owner checkout's git folder or a linked worktree's"
        );
        let excludes =
            work_tree.and_then(|_| owner_excludes_setting(super::Within::GitDir(git_dir)));
        let mut repo = Self {
            git_dir: git_dir.to_path_buf(),
            work_tree: work_tree.map(Path::to_path_buf),
            overrides: Vec::new(),
            excludes,
            index: None,
            confinement: Vec::new(),
            #[cfg(target_os = "linux")]
            index_steps: Vec::new(),
            #[cfg(target_os = "linux")]
            copies: false,
        };
        repo.require_version(VERSION_MIN)?;
        let listing = repo.capture(&crate::research::runner::FILTER_KEYS, LISTING_BYTES_MAX)?;
        let keys = crate::research::runner::listed_filter_keys(&listing)
            .ok_or_else(|| failure("changes_git_failed"))?;
        assert!(keys.len() < listing.len());
        if crate::research::runner::listed_hook(keys).is_some() {
            eprintln!("landing_config_hook_refused");
            return Err(failure("changes_git_hooks"));
        }
        let folders = std::iter::once(repository)
            .chain(work_tree)
            .map(crate::sandbox::canonical)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| failure("changes_git_failed"))?;
        let includes = repo.exec(&INCLUDE_KEYS, None, &[], LISTING_BYTES_MAX)?;
        let listed = includes.code == 0 || (includes.code == 1 && includes.stdout.is_empty());
        if includes.overflow || !listed {
            return Err(failure("changes_git_failed"));
        }
        let outside =
            crate::ide::bridge::remote_git::includes_placed_outside(&includes.stdout, &folders)
                .ok_or_else(|| failure("changes_git_failed"))?;
        if !outside {
            eprintln!("landing_config_include_refused");
            return Err(failure("changes_git_include"));
        }
        repo.overrides = crate::research::runner::filter_overrides(keys)
            .map_err(|_| failure("changes_git_unsupported"))?;
        assert!(repo.overrides.len().is_multiple_of(8));
        Ok(repo)
    }

    /// Refuses engine git where the host git and the research runner's filter listing
    /// do not exist. Takes the checkout root, git folder and work tree, unused.
    /// Returns `changes_unavailable`.
    #[cfg(not(unix))]
    pub(super) fn open(
        _repository: &Path,
        _git_dir: &Path,
        _work_tree: Option<&Path>,
    ) -> Result<Self, EngineError> {
        Err(failure("changes_unavailable"))
    }

    /// Opens a repository the engine is making itself, whose config only the engine wrote:
    /// no filter listing, no owner ignore file and no version check, which the owner
    /// repository opened before it already passed. Inputs: the git folder and the work
    /// tree, which must exist. Output: the repository.
    #[cfg(unix)]
    pub(super) fn fresh(git_dir: &Path, work_tree: &Path) -> Self {
        Self {
            git_dir: git_dir.to_path_buf(),
            work_tree: Some(work_tree.to_path_buf()),
            overrides: Vec::new(),
            excludes: None,
            index: None,
            confinement: Vec::new(),
            #[cfg(target_os = "linux")]
            index_steps: Vec::new(),
            #[cfg(target_os = "linux")]
            copies: false,
        }
    }

    /// The `-c` arguments that switch off the filter drivers this repository's config
    /// names, as every git call here carries them, for a test to read. Output: the
    /// arguments, eight for each driver.
    #[cfg(test)]
    pub(super) fn overrides(&self) -> &[String] {
        &self.overrides
    }

    /// Points every later git call at a private index file the engine owns, so reading a
    /// work tree, such as an agent's own clone, neither reads nor writes an index in it.
    /// Input: the index path, which git creates. Output: the repository.
    pub(super) fn private_index(mut self, index: &Path) -> Self {
        self.index = Some(index.to_path_buf());
        self
    }

    /// Where a confined step runs, each canonical: the work tree, the git folder and the
    /// repository's common git folder. The git folder is a linked worktree's, or, for a step
    /// on the engine's private index, the owner checkout's own `.git`, which a harness clone's
    /// check reads through. Input: the repository. Output: the three folders, or
    /// `changes_workspace_invalid`.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn places(&self) -> Result<(PathBuf, PathBuf, PathBuf), EngineError> {
        let invalid = || failure("changes_workspace_invalid");
        let real = |path: &Path| std::fs::canonicalize(path).map_err(|_| invalid());
        let work = real(self.work_tree.as_deref().ok_or_else(invalid)?)?;
        let private = real(&self.git_dir)?;
        let linked = private
            .parent()
            .filter(|parent| parent.ends_with("worktrees"))
            .and_then(Path::parent);
        let common = match (linked, &self.index) {
            (_, Some(_)) if private.file_name() == Some(std::ffi::OsStr::new(".git")) => {
                private.clone()
            }
            (Some(common), _) => common.to_path_buf(),
            (None, _) => return Err(invalid()),
        };
        assert!(work.is_absolute() && common.is_absolute());
        Ok((work, private, common))
    }

    /// The engine's private index a reading step works on and its lock, in the index's
    /// canonical folder, which must exist. Input: the repository. Output: both paths, none
    /// on the worktree's own index, or `changes_workspace_invalid`.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn private_places(&self) -> Result<Option<(PathBuf, PathBuf)>, EngineError> {
        let Some(index) = &self.index else {
            return Ok(None);
        };
        let invalid = || failure("changes_workspace_invalid");
        let folder = index
            .parent()
            .and_then(|folder| std::fs::canonicalize(folder).ok())
            .ok_or_else(invalid)?;
        let name = index.file_name().ok_or_else(invalid)?;
        let mut lock = name.to_os_string();
        lock.push(".lock");
        Ok(Some((folder.join(name), folder.join(lock))))
    }

    /// This repository with its git calls run under [`WORK_TREE_PROFILE`], for a step on a
    /// linked agent worktree whose agent may still be changing it. Git reads a path with no
    /// check it can hold, so a link swapped in during the step is stopped here. A reading step
    /// may run on the engine's private index ([`Self::places`]), which it alone writes.
    /// Input: what the step may write. Output: the repository, `changes_workspace_invalid` for
    /// one that is not a linked worktree's, or `changes_git_unsupported` where no toolchain
    /// git is found.
    #[cfg(target_os = "macos")]
    pub(super) fn confined(&self, writes: Writes) -> Result<Self, EngineError> {
        use std::ffi::OsString;
        assert!(
            self.index.is_none() || matches!(writes, Writes::Index),
            "only a reading step works on a private index"
        );
        let (work, private, common) = self.places()?;
        let unsupported = || failure("changes_git_unsupported");
        let git = std::fs::canonicalize(crate::research::runner::sandboxed_git())
            .map_err(|_| unsupported())?;
        let tools = git
            .ends_with("usr/bin/git")
            .then(|| git.ancestors().nth(3))
            .flatten()
            .filter(|root| root.parent().is_some())
            .ok_or_else(unsupported)?
            .to_path_buf();
        let setting = self.step_excludes(writes);
        let excludes = setting
            .strip_prefix("core.excludesFile=")
            .and_then(|path| std::fs::canonicalize(path).ok())
            .unwrap_or_else(|| PathBuf::from("/dev/null"));
        assert!(matches!(writes, Writes::Index) || excludes == Path::new("/dev/null"));
        let target = match writes {
            Writes::Objects => common.join("objects"),
            Writes::WorkTree => work.clone(),
            Writes::Index => PathBuf::from("/dev/null"),
        };
        let (index, lock) = self
            .private_places()?
            .unwrap_or_else(|| (private.join("index"), private.join("index.lock")));
        let mut confinement = Vec::new();
        for (name, value) in [
            ("WORK", &work),
            ("COMMON", &common),
            ("TOOLS", &tools),
            ("EXCLUDES", &excludes),
            ("WRITE", &target),
            ("INDEX", &index),
            ("INDEX_LOCK", &lock),
        ] {
            let mut pair = OsString::from(name);
            pair.push("=");
            pair.push(value);
            confinement.extend([OsString::from("-D"), pair]);
        }
        confinement.extend([
            OsString::from("-p"),
            OsString::from(WORK_TREE_PROFILE),
            git.into_os_string(),
        ]);
        Ok(Self {
            confinement,
            excludes: Some(setting),
            ..self.clone()
        })
    }

    /// The excludes setting one confined step's git takes, the one rule macOS and Linux share:
    /// the owner's ignore file for a step that writes nothing but its index ([`Writes::Index`]),
    /// else none ([`NO_EXCLUDES`]), so no step that writes an object or the work tree may read
    /// that file, which a link swapped in during the step would hand to its git (SA vid 2). A
    /// turn end's add applies that file's patterns as names instead (`flow::add_pathspecs`).
    /// Input: what the step may write. Output: the `core.excludesFile=` setting.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn step_excludes(&self, writes: Writes) -> String {
        let setting = match &self.excludes {
            Some(owner) if matches!(writes, Writes::Index) => owner.clone(),
            _ => NO_EXCLUDES.to_owned(),
        };
        assert!(setting.starts_with("core.excludesFile="));
        setting
    }

    /// This repository with its git calls started by the Landlock helper under
    /// [`crate::sandbox::linux::git_step_policy`], for a step on a linked agent worktree whose
    /// agent may still be changing it, as `WORK_TREE_PROFILE` holds it on macOS. The step
    /// reads the system, the work tree, the repository's git folder and git. A step that may
    /// write never reads the owner's ignore file, which a link swapped in during a turn-end
    /// add would otherwise copy into the object store (SA vid 2): its git takes no excludes
    /// file ([`NO_EXCLUDES`]), and a
    /// turn end applies that file's patterns as names instead (`flow::add_pathspecs`). So a
    /// `read-tree -u` counts an untracked file only that file names as the agent's and refuses
    /// to write over it, failing closed, as on macOS (rv-sec3 F1, [`Self::step_excludes`]). A
    /// step that reads the work tree writes the object store and the engine's step folder in the
    /// worktree's git folder, where it works on a copy of the index ([`Self::swapped`]). A
    /// step that may write the work tree writes only the work tree, and its index-only gits
    /// only that folder ([`Self::moved`]), so no git that writes a work-tree path, which a link
    /// swapped in during the step can point anywhere, can write anything else (SA vid 3 A).
    /// A reading step writes no object, so it alone also reads the owner's ignore file and
    /// keeps the owner's excludes, and its listing answers as the owner's git does; on the
    /// engine's private index it writes only that index's folder and copies no index.
    /// Input: what the step may write.
    /// Output: the repository, `changes_workspace_invalid` for one that is not a linked
    /// worktree's, or `changes_git_unsupported` where git, the helper or this kernel's
    /// Landlock file rules are missing: the step fails closed and never runs unconfined.
    #[cfg(target_os = "linux")]
    pub(super) fn confined(&self, writes: Writes) -> Result<Self, EngineError> {
        use std::ffi::OsString;
        assert!(
            self.index.is_none() || matches!(writes, Writes::Index),
            "only a reading step works on a private index"
        );
        let invalid = || failure("changes_workspace_invalid");
        let (work, private, common) = self.places()?;
        let unsupported = || failure("changes_git_unsupported");
        let git = std::fs::canonicalize(crate::research::runner::sandboxed_git())
            .map_err(|_| unsupported())?;
        let helper = landlock_helper()
            .filter(|helper| crate::sandbox::linux::holds_file_rules(helper))
            .ok_or_else(unsupported)?;
        let reads_ignore = matches!(writes, Writes::Index);
        let excludes = self.step_excludes(writes);
        let mut read = vec![work.clone(), common.clone(), git.clone()];
        read.extend(
            excludes
                .strip_prefix("core.excludesFile=")
                .filter(|_| reads_ignore)
                .and_then(|path| std::fs::canonicalize(path).ok()),
        );
        let wrapped = |read: &[PathBuf], write: &[PathBuf]| {
            let policy = crate::sandbox::linux::git_step_policy(read, write);
            let mut confinement = vec![helper.clone().into_os_string()];
            confinement.extend(policy.args());
            confinement.extend([OsString::from("--"), git.clone().into_os_string()]);
            assert!(confinement.len() > 4);
            confinement
        };
        if let Some((index, _)) = self.private_places()? {
            let folder = index
                .parent()
                .expect("a private index sits in its folder")
                .to_path_buf();
            read.push(folder.clone());
            return Ok(Self {
                index: Some(index),
                excludes: Some(excludes),
                confinement: wrapped(&read, &[folder]),
                index_steps: Vec::new(),
                copies: false,
                ..self.clone()
            });
        }
        let step = private.join(STEP_FOLDER);
        match std::fs::create_dir(&step) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(_) => return Err(invalid()),
        }
        if !std::fs::symlink_metadata(&step).is_ok_and(|meta| meta.is_dir()) {
            return Err(invalid());
        }
        let (confinement, index_steps) = match writes {
            Writes::Objects => (
                wrapped(&read, &[common.join("objects"), step.clone()]),
                Vec::new(),
            ),
            Writes::Index => (wrapped(&read, std::slice::from_ref(&step)), Vec::new()),
            Writes::WorkTree => (
                wrapped(&read, &[work]),
                wrapped(&read, std::slice::from_ref(&step)),
            ),
        };
        Ok(Self {
            index: Some(step.join("index")),
            excludes: Some(excludes),
            confinement,
            index_steps,
            copies: true,
            ..self.clone()
        })
    }

    /// This repository unchanged, since only macOS and Linux have a confined step: elsewhere
    /// the step runs as any engine git call does. Input: what the step may write. Output:
    /// the repository.
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    pub(super) fn confined(&self, _writes: Writes) -> Result<Self, EngineError> {
        Ok(self.clone())
    }

    /// This repository for a step that only reads an agent's work tree, confined to write
    /// nothing but its index ([`Writes::Index`]), so a link swapped in during an index refresh,
    /// compare or untracked listing leads its git nowhere the step may not read, on the
    /// worktree's own index or on the engine's private one, as a harness clone's and a
    /// lockfile tool's checks use (LINUX-LANDING part 2 and rest). A repository already
    /// confined stays as it is. Output: the repository, or the refusal [`Self::confined`] gives.
    pub(super) fn reading(&self) -> Result<Self, EngineError> {
        if !self.confinement.is_empty() {
            return Ok(self.clone());
        }
        self.confined(Writes::Index)
    }

    /// Refuses a git older than 2.40, which has neither `merge-tree --merge-base` nor
    /// `check-attr --source`, before a squash merge or an attribute check would run one, so
    /// listing, diffs and the rest still work on the 2.39 of Xcode 16 and its Command Line
    /// Tools (W-GIT-FLOOR). Output: Ok, else `changes_git_old`, or `changes_git_unsupported`
    /// when `git version` names no version.
    pub(super) fn check_merge_version(&self) -> Result<(), EngineError> {
        self.require_version(MERGE_VERSION_MIN)
    }

    /// Refuses a git older than `needed`, logging the version found and the one needed.
    /// Input: the oldest major and minor version. Output: Ok, else `changes_git_old`, or
    /// `changes_git_unsupported` when `git version` names no version.
    fn require_version(&self, needed: (u32, u32)) -> Result<(), EngineError> {
        let printed = self.capture(&["version"], 256)?;
        #[cfg(test)]
        let printed = VERSION_PRINTED
            .with_borrow(Clone::clone)
            .map_or(printed, String::into_bytes);
        let text = String::from_utf8_lossy(&printed);
        let found: Vec<u32> = text
            .trim()
            .strip_prefix("git version ")
            .unwrap_or_default()
            .split(|c: char| !c.is_ascii_digit())
            .map_while(|part| part.parse::<u32>().ok())
            .take(3)
            .collect();
        let (Some(&major), Some(&minor)) = (found.first(), found.get(1)) else {
            return Err(failure("changes_git_unsupported"));
        };
        if (major, minor) >= needed {
            return Ok(());
        }
        let shown: Vec<String> = found.iter().map(u32::to_string).collect();
        eprintln!(
            "changes_git_old found={} needed={}.{}",
            shown.join("."),
            needed.0,
            needed.1
        );
        Err(failure("changes_git_old"))
    }

    fn command(&self, args: &[&str], env: &[(&str, &str)]) -> Command {
        let mut command = if self.confinement.is_empty() {
            Command::new(crate::research::runner::setup_git())
        } else if cfg!(target_os = "linux") {
            let mut confined = Command::new(&self.confinement[0]);
            confined.args(&self.confinement[1..]);
            confined
        } else {
            let mut confined = Command::new(crate::sandbox::SANDBOX_EXEC);
            confined.args(&self.confinement);
            confined
        };
        command
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("LC_ALL", "C")
            .envs(crate::research::runner::GIT_SAFE_ENV)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_NO_REPLACE_OBJECTS", "1")
            .env("GIT_GRAFT_FILE", "/dev/null")
            .envs(env.iter().copied())
            .envs(self.index.iter().map(|index| ("GIT_INDEX_FILE", index)))
            .arg("--no-optional-locks")
            .args(crate::research::runner::GIT_SAFE)
            .args(ENGINE_SETTINGS)
            .args(&self.overrides);
        if let Some(excludes) = &self.excludes {
            command.arg("-c").arg(excludes);
        }
        command.arg("--git-dir").arg(&self.git_dir);
        if let Some(work_tree) = &self.work_tree {
            command
                .arg("--work-tree")
                .arg(work_tree)
                .current_dir(work_tree);
        } else {
            command.current_dir(&self.git_dir);
        }
        command
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        command
    }

    /// Runs one git command within `STEP_TIME`, feeding it `input` and keeping at most
    /// `limit` bytes of its output; a confined Linux step on the worktree's own index runs on
    /// a copy of it ([`Self::swapped`]), and a two-tree `read-tree -u -m` there that may write
    /// the work tree runs split ([`Self::moved`]). Inputs: the arguments, the input, extra
    /// environment and the bound. Output: the exit code, the output and whether it ran over;
    /// an error when git cannot start, is stopped by a signal or runs past the time.
    pub(super) fn exec(
        &self,
        args: &[&str],
        input: Option<&[u8]>,
        env: &[(&str, &str)],
        limit: usize,
    ) -> Result<Output, EngineError> {
        #[cfg(target_os = "linux")]
        if self.copies {
            let fills = !self.index_steps.is_empty();
            if let ["read-tree", "-u", "-m", from, to] = *args
                && fills
            {
                return self.swapped(true, || self.moved(from, to));
            }
            return self.swapped(!fills, || self.spawned(args, input, env, limit));
        }
        self.spawned(args, input, env, limit)
    }

    /// Runs one confined Linux step on the copy of the worktree's index in the engine's step
    /// folder, holding the worktree's `index.lock` the whole time as git would. The lock holds
    /// [`step_lock_text`] for this engine process, so the start sweep removes only a lock the
    /// engine took ([`clear_step_locks`]); where the process cannot be named, as for process 1, the
    /// lock is taken empty and the sweep keeps it. A step that does not take the lock removes its
    /// step folder when empty and leaves one holding another step's copy to that step
    /// (RV-DS241-135). A step whose folder holds a git `index.lock`, as a git an earlier engine
    /// started can, lets the worktree's lock go and answers [`INDEX_LOCKED`] without touching that
    /// folder (RV-DS241-142). The folder starts empty and is emptied after, before the worktree's
    /// lock goes when the copy is not put back, so a git the step killed never leaves a lock there
    /// that no sweep removes, and the index is copied only from a plain file with one link, opened
    /// without following a link or waiting on a FIFO, and keeps the index's modified time, so an
    /// entry racy against the index stays racy and git still reads its file (RV-DS241-147). Once
    /// git exits 0 having rewritten the copy, the copy takes the index's place only when it too is
    /// a plain file with one link, so whatever a raced step leaves there is never taken, and a step
    /// that only reads leaves the index as it was; the step itself never writes the git folder
    /// ([`Self::confined`]). The copy is taken only when `adopts` says gits that write no
    /// work-tree path wrote it: any step that reads the work tree, and the index-only gits of a
    /// split move ([`Self::moved`]); any other step that may write the work tree never has its
    /// copy taken (SA vid 3). Input: whether the copy may be taken and the step. Output: its
    /// output, git's [`INDEX_LOCKED`] exit when the lock is already held, or
    /// `changes_git_failed` when the index cannot be locked, copied or put back.
    #[cfg(target_os = "linux")]
    pub(super) fn swapped(
        &self,
        adopts: bool,
        step: impl FnOnce() -> Result<Output, EngineError>,
    ) -> Result<Output, EngineError> {
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
        let copy = self
            .index
            .as_deref()
            .expect("a confined Linux step works on a copy of the index");
        assert!(
            copy.parent()
                .is_some_and(|folder| folder.ends_with(STEP_FOLDER))
        );
        let index = self.git_dir.join("index");
        let lock = self.git_dir.join("index.lock");
        let failed = || failure("changes_git_failed");
        let folder = copy.parent().expect("the copy sits in the step folder");
        let own = i32::try_from(std::process::id())
            .ok()
            .and_then(|pid| {
                crate::storage::harness_groups::start_identity(pid)
                    .ok()
                    .flatten()
                    .map(|started| step_lock_text(pid, &started))
            })
            .unwrap_or_default();
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock)
        {
            Ok(mut taken) => {
                if taken.write_all(own.as_bytes()).is_err() {
                    drop(taken);
                    let _ = std::fs::remove_file(&lock);
                    let _ = std::fs::remove_dir_all(folder);
                    return Err(failed());
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                let _ = std::fs::remove_dir(folder);
                return Ok(Output {
                    code: INDEX_LOCKED,
                    stdout: Vec::new(),
                    overflow: false,
                });
            }
            Err(_) => {
                let _ = std::fs::remove_dir(folder);
                return Err(failed());
            }
        }
        if std::fs::symlink_metadata(folder.join("index.lock")).is_ok() {
            let _ = std::fs::remove_file(&lock);
            return Ok(Output {
                code: INDEX_LOCKED,
                stdout: Vec::new(),
                overflow: false,
            });
        }
        let plain = |meta: &std::fs::Metadata| meta.file_type().is_file() && meta.nlink() == 1;
        let fresh = || {
            let renewed = std::fs::remove_dir_all(folder)
                .err()
                .is_none_or(|error| error.kind() == std::io::ErrorKind::NotFound)
                && std::fs::create_dir(folder).is_ok();
            renewed
                && match std::fs::OpenOptions::new()
                    .read(true)
                    .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
                    .open(&index)
                {
                    Err(error) => error.kind() == std::io::ErrorKind::NotFound,
                    Ok(mut real) => match real.metadata() {
                        Ok(meta) if plain(&meta) => std::fs::File::create_new(copy)
                            .and_then(|mut written| {
                                std::io::copy(&mut real, &mut written)?;
                                let modified =
                                    std::fs::FileTimes::new().set_modified(meta.modified()?);
                                written.set_times(modified)
                            })
                            .is_ok(),
                        _ => false,
                    },
                }
        };
        let copied = fresh();
        let identity = |meta: &std::fs::Metadata| (meta.dev(), meta.ino());
        let ran = if copied {
            let before = std::fs::symlink_metadata(copy)
                .ok()
                .map(|meta| identity(&meta));
            step().map(|output| (before, output))
        } else {
            Err(failed())
        };
        let left = std::fs::symlink_metadata(copy).ok();
        let rewritten = matches!(&ran, Ok((before, output))
            if output.code == 0 && left.as_ref().is_some_and(|meta| Some(identity(meta)) != *before));
        let taken = rewritten && adopts;
        let kept = taken
            && std::fs::symlink_metadata(copy).is_ok_and(|meta| plain(&meta))
            && std::fs::rename(copy, &lock).is_ok()
            && std::fs::rename(&lock, &index).is_ok();
        let _ = std::fs::remove_dir_all(folder);
        if !kept {
            let _ = std::fs::remove_file(&lock);
        }
        match ran {
            Ok(_) if rewritten && !kept => Err(failed()),
            ran => ran.map(|(_, output)| output),
        }
    }

    /// The two-tree `read-tree -u -m` of a confined Linux step that may write the work tree,
    /// split so no git writing a work-tree path writes anywhere else (SA vid 3 A): index-only
    /// gits in the step folder move the copy and refuse a local edit, the engine checks and
    /// removes paths with no link followed, and `checkout-index` alone writes the work tree.
    /// Runs inside [`Self::swapped`]. Inputs: both commits. Output: a refusal, or the refresh's.
    #[cfg(target_os = "linux")]
    fn moved(&self, from: &str, to: &str) -> Result<Output, EngineError> {
        let failed = || failure("changes_git_failed");
        let index = Self {
            confinement: self.index_steps.clone(),
            ..self.clone()
        };
        assert!(!index.confinement.is_empty() && index.confinement != self.confinement);
        let work = std::fs::canonicalize(self.work_tree.as_deref().ok_or_else(failed)?)
            .map_err(|_| failed())?;
        let before = index.entries()?;
        let merged = index.spawned(&["read-tree", "-m", from, to], None, &[], 4096)?;
        if merged.code != 0 || merged.overflow {
            return Ok(merged);
        }
        let after = index.entries()?;
        let gone: BTreeSet<&[u8]> = before
            .keys()
            .filter(|path| !after.contains_key(*path))
            .map(Vec::as_slice)
            .collect();
        let written: Vec<&[u8]> = after
            .iter()
            .filter(|&(path, entry)| before.get(path) != Some(entry))
            .map(|(path, _)| path.as_slice())
            .collect();
        let refused = Output {
            code: 1,
            stdout: Vec::new(),
            overflow: false,
        };
        let blocked = written
            .iter()
            .filter(|path| !before.contains_key(**path))
            .any(|path| in_the_way(&work, path, &gone));
        if blocked {
            return Ok(refused);
        }
        if gone.iter().any(|path| remove_tracked(&work, path).is_err()) {
            return Ok(refused);
        }
        if !written.is_empty() {
            let mut listed = Vec::new();
            for path in &written {
                listed.extend_from_slice(path);
                listed.push(0);
            }
            let checkout = ["checkout-index", "-f", "-z", "--stdin"];
            let filled = self.spawned(&checkout, Some(listed.as_slice()), &[], 4096)?;
            if filled.code != 0 || filled.overflow {
                return Ok(filled);
            }
        }
        index.spawned(&["update-index", "-q", "--refresh"], None, &[], 4096)
    }

    /// The stage-0 entries of the copy of the index a confined Linux step works on, from
    /// `ls-files -s -z`: each path with its mode, object id and stage as listed. Output: the
    /// entries, or `changes_git_failed` for a listing past `OUTPUT_BYTES_MAX`, an unmerged
    /// entry or a path with an empty, `.` or `..` part.
    #[cfg(target_os = "linux")]
    fn entries(&self) -> Result<std::collections::BTreeMap<Vec<u8>, Vec<u8>>, EngineError> {
        let failed = || failure("changes_git_failed");
        let listing = self.spawned(&["ls-files", "-s", "-z"], None, &[], OUTPUT_BYTES_MAX)?;
        if listing.code != 0 || listing.overflow {
            return Err(failed());
        }
        let mut entries = std::collections::BTreeMap::new();
        for record in listing.stdout.split(|&byte| byte == 0) {
            if record.is_empty() {
                continue;
            }
            let tab = record
                .iter()
                .position(|&byte| byte == b'\t')
                .ok_or_else(failed)?;
            let (entry, path) = (&record[..tab], &record[tab + 1..]);
            let odd = |part: &[u8]| part.is_empty() || part == b"." || part == b"..";
            if !entry.ends_with(b" 0") || path.split(|&byte| byte == b'/').any(odd) {
                return Err(failed());
            }
            entries.insert(path.to_vec(), entry.to_vec());
        }
        Ok(entries)
    }

    /// Starts one git command and collects what [`Self::exec`] answers. Inputs and output:
    /// as there.
    fn spawned(
        &self,
        args: &[&str],
        input: Option<&[u8]>,
        env: &[(&str, &str)],
        limit: usize,
    ) -> Result<Output, EngineError> {
        let mut command = self.command(args, env);
        #[cfg(test)]
        GIT_RUNS.with(|runs| runs.set(runs.get() + 1));
        #[cfg(test)]
        UNCONFINED_WORK.with(|runs| {
            if self.confinement.is_empty() && self.work_tree.is_some() {
                runs.borrow_mut()
                    .extend(args.first().map(|arg| (*arg).to_owned()));
            }
        });
        command.stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        });
        let mut child =
            crate::spawn_gate::spawn(&mut command).map_err(|_| failure("changes_git_failed"))?;
        let writer = match (input, child.stdin.take()) {
            (Some(bytes), Some(mut stdin)) => {
                let bytes = bytes.to_vec();
                Some(std::thread::spawn(move || stdin.write_all(&bytes)))
            }
            _ => None,
        };
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| failure("changes_git_failed"))?;
        let reader = std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let mut source = stdout.take(limit as u64 + 1);
            let result = source.read_to_end(&mut bytes);
            let mut rest = source.into_inner();
            let _ = std::io::copy(&mut rest, &mut std::io::sink());
            result.map(|_| bytes)
        });
        let deadline = Instant::now() + STEP_TIME;
        let status = loop {
            if let Some(status) = child
                .try_wait()
                .map_err(|_| failure("changes_git_failed"))?
            {
                break status;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                return Err(failure("changes_git_failed"));
            }
            std::thread::sleep(POLL);
        };
        if let Some(writer) = writer {
            let _ = writer.join();
        }
        let mut stdout = reader
            .join()
            .map_err(|_| failure("changes_git_failed"))?
            .map_err(|_| failure("changes_git_failed"))?;
        let code = status.code().ok_or_else(|| failure("changes_git_failed"))?;
        let overflow = stdout.len() > limit;
        stdout.truncate(limit);
        assert!(stdout.len() <= limit);
        Ok(Output {
            code,
            stdout,
            overflow,
        })
    }

    /// Runs one git command that must succeed and print at most `limit` bytes.
    /// Inputs: the arguments and the bound. Output: its output, or `changes_git_failed`.
    pub(super) fn capture(&self, args: &[&str], limit: usize) -> Result<Vec<u8>, EngineError> {
        let output = self.exec(args, None, &[], limit)?;
        if output.code != 0 || output.overflow {
            return Err(failure("changes_git_failed"));
        }
        Ok(output.stdout)
    }

    /// Runs one git command that must succeed. Input: the arguments. Output: its output.
    pub(super) fn run(&self, args: &[&str]) -> Result<Vec<u8>, EngineError> {
        self.capture(args, OUTPUT_BYTES_MAX)
    }

    /// Runs one git command whose exit code answers a yes or no question: 0 is yes and 1
    /// is no. Input: the arguments. Output: the answer, or `changes_git_failed`.
    pub(super) fn test(&self, args: &[&str]) -> Result<bool, EngineError> {
        match self.exec(args, None, &[], OUTPUT_BYTES_MAX)?.code {
            0 => Ok(true),
            1 => Ok(false),
            _ => Err(failure("changes_git_failed")),
        }
    }

    /// Says which of the given object ids name a commit here, with one git run for all of
    /// them. Input: the ids; one that is not an object id is never sent. Output: the ids
    /// that are commits; a missing id or one naming another object type is left out.
    pub(super) fn commits_among(&self, ids: &[&str]) -> Result<BTreeSet<String>, EngineError> {
        let asked: Vec<&str> = ids.iter().copied().filter(|id| is_object_id(id)).collect();
        if asked.is_empty() {
            return Ok(BTreeSet::new());
        }
        let input: String = asked.iter().map(|id| format!("{id}\n")).collect();
        let output = self.exec(
            &["cat-file", "--batch-check=%(objectname) %(objecttype)"],
            Some(input.as_bytes()),
            &[],
            asked.len() * CHECK_LINE_BYTES_MAX,
        )?;
        if output.code != 0 || output.overflow {
            return Err(failure("changes_git_failed"));
        }
        let text =
            std::str::from_utf8(&output.stdout).map_err(|_| failure("changes_git_failed"))?;
        let lines: Vec<&str> = text.lines().collect();
        if lines.len() != asked.len() {
            return Err(failure("changes_git_failed"));
        }
        let commits: BTreeSet<String> = asked
            .iter()
            .zip(lines)
            .filter(|(id, line)| line.strip_prefix(**id) == Some(" commit"))
            .map(|(id, _)| (*id).to_owned())
            .collect();
        assert!(commits.len() <= asked.len());
        Ok(commits)
    }

    /// Resolves a revision to a commit id. Input: the revision. Output: the id, or None
    /// when it names no commit.
    pub(super) fn commit_of(&self, revision: &str) -> Result<Option<String>, EngineError> {
        let peeled = format!("{revision}^{{commit}}");
        let output = self.exec(
            &["rev-parse", "--verify", "-q", "--end-of-options", &peeled],
            None,
            &[],
            256,
        )?;
        match output.code {
            0 => object_id(&output.stdout).map(Some),
            1 => Ok(None),
            _ => Err(failure("changes_git_failed")),
        }
    }

    /// Resolves a commit's tree. Input: the commit. Output: the tree id.
    pub(super) fn tree_of(&self, commit: &str) -> Result<String, EngineError> {
        object_id(&self.run(&["rev-parse", "--verify", &format!("{commit}^{{tree}}")])?)
    }

    /// Makes a commit object without touching any ref, index or file. Inputs: the tree,
    /// the parents, the message and the author and committer. Output: the commit id.
    pub(super) fn commit_tree(
        &self,
        tree: &str,
        parents: &[&str],
        message: &str,
        (name, email): (&str, &str),
    ) -> Result<String, EngineError> {
        let mut args = vec!["commit-tree", "--no-gpg-sign", tree];
        for parent in parents {
            args.extend(["-p", parent]);
        }
        let env = [
            ("GIT_AUTHOR_NAME", name),
            ("GIT_AUTHOR_EMAIL", email),
            ("GIT_COMMITTER_NAME", name),
            ("GIT_COMMITTER_EMAIL", email),
        ];
        let output = self.exec(&args, Some(message.as_bytes()), &env, 256)?;
        if output.code != 0 || output.overflow {
            return Err(failure("changes_git_failed"));
        }
        object_id(&output.stdout)
    }
}

/// Reads one object id git printed on a line of its own. Input: the output.
/// Output: the id, or `changes_git_failed` for anything but 40 or 64 lowercase hex.
pub(super) fn object_id(printed: &[u8]) -> Result<String, EngineError> {
    let text = std::str::from_utf8(printed).map_err(|_| failure("changes_git_failed"))?;
    let id = text.strip_suffix('\n').unwrap_or(text);
    if is_object_id(id) {
        return Ok(id.to_owned());
    }
    Err(failure("changes_git_failed"))
}

pub(super) fn is_object_id(id: &str) -> bool {
    matches!(id.len(), 40 | 64)
        && id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// Whether something in the work tree stands where a move would write a new tracked path:
/// the path itself, or a leading part that is not a plain folder, unless that part is a
/// tracked file the move removes, as `read-tree -u` refuses to write over an untracked file.
/// Each part is read with no link followed. Inputs: the work tree, the path and the paths
/// the move removes. Output: true when the move must refuse, also when a part cannot be read.
#[cfg(target_os = "linux")]
fn in_the_way(work: &Path, path: &[u8], gone: &BTreeSet<&[u8]>) -> bool {
    use rustix::fs::{AtFlags, FileType, Mode, OFlags};
    use std::os::unix::ffi::OsStrExt;
    let flags = OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::RDONLY;
    let Ok(mut folder) = rustix::fs::open(work, flags, Mode::empty()) else {
        return true;
    };
    let mut end = 0;
    for part in path.split(|&byte| byte == b'/') {
        end += part.len();
        if gone.contains(&path[..end]) {
            return false;
        }
        let name = std::ffi::OsStr::from_bytes(part);
        let stat = match rustix::fs::statat(&folder, name, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(stat) => stat,
            Err(error) => return error != rustix::io::Errno::NOENT,
        };
        if end == path.len() || FileType::from_raw_mode(stat.st_mode) != FileType::Directory {
            return true;
        }
        folder = match rustix::fs::openat(&folder, name, flags, Mode::empty()) {
            Ok(next) => next,
            Err(_) => return true,
        };
        end += 1;
    }
    true
}

/// Removes one tracked path a move drops from the work tree, then each folder it leaves
/// empty, as `read-tree -u` does, opening every leading folder with no link followed, so a
/// link swapped in for one is never removed through (SA vid 3 A). A path already gone counts
/// as removed. Inputs: the work tree and the path. Output: unit, or the error that stopped it.
#[cfg(target_os = "linux")]
fn remove_tracked(work: &Path, path: &[u8]) -> rustix::io::Result<()> {
    use rustix::fs::{AtFlags, Mode, OFlags};
    use std::os::unix::ffi::OsStrExt;
    let flags = OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::RDONLY;
    let parts: Vec<&std::ffi::OsStr> = path
        .split(|&byte| byte == b'/')
        .map(std::ffi::OsStr::from_bytes)
        .collect();
    let (name, leading) = parts.split_last().expect("a tracked path has a name");
    let mut folders = vec![rustix::fs::open(work, flags, Mode::empty())?];
    for part in leading {
        let folder = folders.last().expect("the work tree is open");
        match rustix::fs::openat(folder, *part, flags, Mode::empty()) {
            Ok(next) => folders.push(next),
            Err(error) if error == rustix::io::Errno::NOENT => return Ok(()),
            Err(error) => return Err(error),
        }
    }
    assert_eq!(folders.len(), leading.len() + 1);
    let last = folders.last().expect("the work tree is open");
    if let Err(error) = rustix::fs::unlinkat(last, *name, AtFlags::empty())
        && error != rustix::io::Errno::NOENT
    {
        return Err(error);
    }
    for (at, part) in leading.iter().enumerate().rev() {
        if rustix::fs::unlinkat(&folders[at], *part, AtFlags::REMOVEDIR).is_err() {
            break;
        }
    }
    Ok(())
}
