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
const VERSION_MIN: (u32, u32) = (2, 40);
const EXCLUDES_PATH_BYTES_MAX: usize = 1024;
const CHECK_LINE_BYTES_MAX: usize = 80;

#[cfg(test)]
thread_local! {
    /// The owner home a landing test stands in for the process's HOME, so engine git in
    /// that test reads that home's global ignore rules. None keeps the process's HOME.
    pub(super) static OWNER_HOME: std::cell::RefCell<Option<PathBuf>> =
        const { std::cell::RefCell::new(None) };
    /// How many git commands engine git started on this thread, so a landing test can
    /// count what one read costs.
    pub(super) static GIT_RUNS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
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

/// Finds the ignore file the owner's own git applies in every repository: the global
/// `core.excludesFile`, read by a git that sees only the owner's home and runs nothing,
/// else git's default `$XDG_CONFIG_HOME/git/ignore` or `~/.config/git/ignore`.
/// Inputs: the owner's home and XDG config folder. Output: the file's real path when it
/// is a regular file owned by the engine's user or root, such as a store-managed link.
#[cfg(unix)]
pub(super) fn owner_excludes(home: &Path, xdg: Option<&Path>) -> Option<PathBuf> {
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
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .args([
            "config",
            "--global",
            "--type=path",
            "--get",
            "core.excludesFile",
        ])
        .current_dir("/")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if let Some(xdg) = xdg {
        command.env("XDG_CONFIG_HOME", xdg);
    }
    let output = crate::spawn_gate::output(&mut command).ok()?;
    let named = match output.status.code() {
        Some(0) => PathBuf::from(String::from_utf8(output.stdout).ok()?.strip_suffix('\n')?),
        Some(1) => xdg
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

/// A repository the engine runs git on through a git folder and work tree it computed
/// itself, never a `.git` file in the work tree. Every call runs the real git, not the
/// `/usr/bin/git` xcrun shim where xcrun resolves it, with a cleared environment, no
/// system or global config, no system attributes, hooks, fsmonitor, pager, submodule
/// recursion, protocols or upkeep, and with every filter
/// driver the repository's own config names blanked. A work tree also honours the
/// owner's global ignore file, so what the owner's git ignores the engine never stages.
#[derive(Clone)]
pub(super) struct Repo {
    git_dir: PathBuf,
    work_tree: Option<PathBuf>,
    overrides: Vec<String>,
    excludes: Option<String>,
    index: Option<PathBuf>,
    confinement: Vec<std::ffi::OsString>,
}

/// The Seatbelt profile of one engine git step on an agent's work tree (SECAUDIT
/// unconfined-git-add-reads-through-raced-symlink, unconfined-read-tree-u): it reads only the
/// system, its own toolchain, the work tree, the repository's git folder and the owner's
/// ignore file, and writes only what the step names and the worktree's index, never the
/// repository's `hooks` or `config`, whatever an agent swaps a path for during the step.
/// Paths reach it as parameters, so no path text is read as profile text.
#[cfg(target_os = "macos")]
const WORK_TREE_PROFILE: &str = "(version 1)\n\
(allow default)\n\
(deny file-read-data (require-not (require-any (literal \"/\") (subpath \"/System\") (subpath \"/usr/lib\") (subpath \"/usr/share\") (subpath \"/dev\") (subpath (param \"TOOLS\")) (subpath (param \"WORK\")) (subpath (param \"COMMON\")) (literal (param \"EXCLUDES\")))))\n\
(deny file-write* (require-not (require-any (literal \"/dev/null\") (subpath (param \"WRITE\")) (literal (param \"INDEX\")) (literal (param \"INDEX_LOCK\")))))\n";

/// What one confined git step may write besides the worktree's index: the object store,
/// for a step that only reads the work tree, or the work tree, for one that fills it.
#[derive(Clone, Copy)]
pub(super) enum Writes {
    Objects,
    WorkTree,
}

impl Repo {
    /// Opens a repository for engine git calls, lists the filter drivers to blank and,
    /// for a work tree, finds the owner's global ignore file. The git folder is always the
    /// owner checkout's own `.git` or a linked worktree's folder directly in its
    /// `.git/worktrees`, never an agent's own clone, whose config is the agent's (CC-N1).
    /// Inputs: the owner checkout root, the git folder and the work tree, none for object
    /// and ref work only. Output: the repository, or `changes_git_unsupported` for a git
    /// older than 2.40 or a filter name a `-c` cannot carry.
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
        let excludes = work_tree
            .and_then(|_| {
                let (home, xdg) = owner_folders();
                owner_excludes(&home?, xdg.as_deref())
            })
            .and_then(|path| {
                path.to_str()
                    .map(|path| format!("core.excludesFile={path}"))
            });
        let mut repo = Self {
            git_dir: git_dir.to_path_buf(),
            work_tree: work_tree.map(Path::to_path_buf),
            overrides: Vec::new(),
            excludes,
            index: None,
            confinement: Vec::new(),
        };
        repo.check_version()?;
        let listing = repo.capture(&crate::research::runner::FILTER_KEYS, LISTING_BYTES_MAX)?;
        let keys = crate::research::runner::listed_filter_keys(&listing)
            .ok_or_else(|| failure("changes_git_failed"))?;
        assert!(keys.len() < listing.len());
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

    /// This repository with its git calls run under [`WORK_TREE_PROFILE`], for a step on a
    /// linked agent worktree whose agent may still be changing it. Git reads a path with no
    /// check it can hold, so a link swapped in during the step is stopped here. Input: what
    /// the step may write. Output: the repository, `changes_workspace_invalid` for one that
    /// is not a linked worktree's, or `changes_git_unsupported` where no toolchain git is
    /// found.
    #[cfg(target_os = "macos")]
    pub(super) fn confined(&self, writes: Writes) -> Result<Self, EngineError> {
        use std::ffi::OsString;
        assert!(
            self.index.is_none(),
            "a confined step uses the worktree's own index"
        );
        let invalid = || failure("changes_workspace_invalid");
        let real = |path: &Path| std::fs::canonicalize(path).map_err(|_| invalid());
        let work = real(self.work_tree.as_deref().ok_or_else(invalid)?)?;
        let private = real(&self.git_dir)?;
        let common = private
            .parent()
            .filter(|parent| parent.ends_with("worktrees"))
            .and_then(Path::parent)
            .ok_or_else(invalid)?
            .to_path_buf();
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
        let excludes = self
            .excludes
            .as_deref()
            .and_then(|setting| setting.strip_prefix("core.excludesFile="))
            .and_then(|path| std::fs::canonicalize(path).ok())
            .unwrap_or_else(|| PathBuf::from("/dev/null"));
        let target = match writes {
            Writes::Objects => common.join("objects"),
            Writes::WorkTree => work.clone(),
        };
        let mut confinement = Vec::new();
        for (name, value) in [
            ("WORK", &work),
            ("COMMON", &common),
            ("TOOLS", &tools),
            ("EXCLUDES", &excludes),
            ("WRITE", &target),
            ("INDEX", &private.join("index")),
            ("INDEX_LOCK", &private.join("index.lock")),
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
            ..self.clone()
        })
    }

    /// This repository with its git calls started by the Landlock helper under
    /// [`crate::sandbox::linux::git_step_policy`], for a step on a linked agent worktree whose
    /// agent may still be changing it, as `WORK_TREE_PROFILE` holds it on macOS. The step
    /// reads the system, the work tree, the repository's git folder, git and the owner's
    /// ignore file. It writes only what it names and the engine's step folder in the
    /// worktree's git folder, where it works on a copy of the index ([`Self::swapped`]), so a
    /// link swapped in during the step reaches nothing else. Input: what the step may write.
    /// Output: the repository, `changes_workspace_invalid` for one that is not a linked
    /// worktree's, or `changes_git_unsupported` where git, the helper or this kernel's
    /// Landlock file rules are missing: the step fails closed and never runs unconfined.
    #[cfg(target_os = "linux")]
    pub(super) fn confined(&self, writes: Writes) -> Result<Self, EngineError> {
        use std::ffi::OsString;
        assert!(
            self.index.is_none(),
            "a confined step uses the worktree's own index"
        );
        let invalid = || failure("changes_workspace_invalid");
        let real = |path: &Path| std::fs::canonicalize(path).map_err(|_| invalid());
        let work = real(self.work_tree.as_deref().ok_or_else(invalid)?)?;
        let private = real(&self.git_dir)?;
        let common = private
            .parent()
            .filter(|parent| parent.ends_with("worktrees"))
            .and_then(Path::parent)
            .ok_or_else(invalid)?
            .to_path_buf();
        let unsupported = || failure("changes_git_unsupported");
        let git = std::fs::canonicalize(crate::research::runner::sandboxed_git())
            .map_err(|_| unsupported())?;
        let helper = landlock_helper()
            .filter(|helper| crate::sandbox::linux::holds_file_rules(helper))
            .ok_or_else(unsupported)?;
        let step = private.join(STEP_FOLDER);
        match std::fs::create_dir(&step) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(_) => return Err(invalid()),
        }
        if !std::fs::symlink_metadata(&step).is_ok_and(|meta| meta.is_dir()) {
            return Err(invalid());
        }
        let mut read = vec![work.clone(), common.clone(), git.clone()];
        read.extend(
            self.excludes
                .as_deref()
                .and_then(|setting| setting.strip_prefix("core.excludesFile="))
                .and_then(|path| std::fs::canonicalize(path).ok()),
        );
        let target = match writes {
            Writes::Objects => common.join("objects"),
            Writes::WorkTree => work,
        };
        let policy = crate::sandbox::linux::git_step_policy(&read, &[target, step.clone()]);
        let mut confinement = vec![helper.into_os_string()];
        confinement.extend(policy.args());
        confinement.extend([OsString::from("--"), git.into_os_string()]);
        assert!(confinement.len() > 4);
        Ok(Self {
            index: Some(step.join("index")),
            confinement,
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

    fn check_version(&self) -> Result<(), EngineError> {
        let printed = self.capture(&["version"], 256)?;
        let text = String::from_utf8_lossy(&printed);
        let mut numbers = text
            .trim()
            .strip_prefix("git version ")
            .unwrap_or_default()
            .split(|c: char| !c.is_ascii_digit())
            .map(|part| part.parse::<u32>().ok());
        match (numbers.next().flatten(), numbers.next().flatten()) {
            (Some(major), Some(minor)) if (major, minor) >= VERSION_MIN => Ok(()),
            _ => Err(failure("changes_git_unsupported")),
        }
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
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_ATTR_NOSYSTEM", "1")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_NO_LAZY_FETCH", "1")
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
    /// `limit` bytes of its output; a confined Linux step runs on a copy of the worktree's
    /// index ([`Self::swapped`]). Inputs: the arguments, the input, extra environment and
    /// the bound. Output: the exit code, the output and whether it ran over; an error when
    /// git cannot start, is stopped by a signal or runs past the time.
    pub(super) fn exec(
        &self,
        args: &[&str],
        input: Option<&[u8]>,
        env: &[(&str, &str)],
        limit: usize,
    ) -> Result<Output, EngineError> {
        #[cfg(target_os = "linux")]
        if !self.confinement.is_empty() {
            return self.swapped(|| self.spawned(args, input, env, limit));
        }
        self.spawned(args, input, env, limit)
    }

    /// Runs one confined Linux step on the copy of the worktree's index in the engine's step
    /// folder, holding the worktree's `index.lock` the whole time as git would, and once git
    /// exits 0 having rewritten the copy puts it in place of the index, so a step that only
    /// reads leaves the index as it was; the step itself never writes the git folder
    /// ([`Self::confined`]). Input: the step. Output: its output, git's [`INDEX_LOCKED`] exit
    /// when the lock is already held, or `changes_git_failed` when the index cannot be
    /// locked, copied or put back.
    #[cfg(target_os = "linux")]
    fn swapped(
        &self,
        step: impl FnOnce() -> Result<Output, EngineError>,
    ) -> Result<Output, EngineError> {
        use std::os::unix::fs::MetadataExt;
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
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock)
        {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                return Ok(Output {
                    code: INDEX_LOCKED,
                    stdout: Vec::new(),
                    overflow: false,
                });
            }
            Err(_) => return Err(failed()),
        }
        let gone = |done: std::io::Result<()>| {
            done.err()
                .is_none_or(|error| error.kind() == std::io::ErrorKind::NotFound)
        };
        let file = |path: &Path| {
            std::fs::symlink_metadata(path)
                .ok()
                .map(|meta| (meta.dev(), meta.ino()))
        };
        let prepared = gone(std::fs::remove_file(copy))
            && gone(std::fs::remove_file(copy.with_extension("lock")))
            && gone(std::fs::copy(&index, copy).map(|_| ()));
        let ran = if prepared {
            let copied = file(copy);
            step().map(|output| (copied, output))
        } else {
            Err(failed())
        };
        let rewritten = matches!(&ran, Ok((copied, output))
            if output.code == 0 && file(copy).is_some_and(|now| Some(now) != *copied));
        let kept = rewritten
            && std::fs::rename(copy, &lock).is_ok()
            && std::fs::rename(&lock, &index).is_ok();
        if !kept {
            let _ = std::fs::remove_file(&lock);
        }
        let _ = std::fs::remove_file(copy);
        match ran {
            Ok(_) if rewritten && !kept => Err(failed()),
            ran => ran.map(|(_, output)| output),
        }
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
