use crate::engine::EngineError;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const GIT: &str = "/usr/bin/git";
pub(super) const OUTPUT_BYTES_MAX: usize = 64 * 1024 * 1024;
const LISTING_BYTES_MAX: usize = 64 * 1024;
const STEP_TIME: Duration = Duration::from_secs(90);
const POLL: Duration = Duration::from_millis(2);
const VERSION_MIN: (u32, u32) = (2, 40);

/// The settings every engine git call carries on top of the research runner's
/// `GIT_SAFE`: no attributes file from outside the repository, no automatic upkeep, the
/// case-folding path checks on, no signing and no untracked or split index caches.
const ENGINE_SETTINGS: [&str; 16] = [
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

/// A repository the engine runs git on through a git folder and work tree it computed
/// itself, never a `.git` file in the work tree. Every call runs the host git with a
/// cleared environment, no system or global config, no system attributes, hooks,
/// fsmonitor, pager, submodule recursion, protocols or upkeep, and with every filter
/// driver the repository's own config names blanked.
pub(super) struct Repo {
    git_dir: PathBuf,
    work_tree: Option<PathBuf>,
    overrides: Vec<String>,
}

impl Repo {
    /// Opens a repository for engine git calls and lists the filter drivers to blank.
    /// Inputs: the git folder and the work tree, none for object and ref work only.
    /// Output: the repository, or `changes_git_unsupported` for a git older than 2.40
    /// or a filter name a `-c` cannot carry.
    #[cfg(unix)]
    pub(super) fn open(git_dir: &Path, work_tree: Option<&Path>) -> Result<Self, EngineError> {
        let mut repo = Self {
            git_dir: git_dir.to_path_buf(),
            work_tree: work_tree.map(Path::to_path_buf),
            overrides: Vec::new(),
        };
        repo.check_version()?;
        let listing = repo.capture(
            &["config", "--list", "--name-only", "-z"],
            LISTING_BYTES_MAX,
        )?;
        repo.overrides = crate::research::runner::filter_overrides(&listing)
            .map_err(|_| failure("changes_git_unsupported"))?;
        assert!(repo.overrides.len().is_multiple_of(8));
        Ok(repo)
    }

    /// Refuses engine git where the host git and the research runner's filter listing
    /// do not exist. Takes the git folder and work tree, unused.
    /// Returns `changes_unavailable`.
    #[cfg(not(unix))]
    pub(super) fn open(_git_dir: &Path, _work_tree: Option<&Path>) -> Result<Self, EngineError> {
        Err(failure("changes_unavailable"))
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
            .envs(env.iter().copied())
            .arg("--no-optional-locks")
            .args(crate::research::runner::GIT_SAFE)
            .args(ENGINE_SETTINGS)
            .args(&self.overrides)
            .arg("--git-dir")
            .arg(&self.git_dir);
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
    /// `limit` bytes of its output. Inputs: the arguments, the input, extra environment
    /// and the bound. Output: the exit code, the output and whether it ran over; an
    /// error when git cannot start, is stopped by a signal or runs past the time.
    pub(super) fn exec(
        &self,
        args: &[&str],
        input: Option<&[u8]>,
        env: &[(&str, &str)],
        limit: usize,
    ) -> Result<Output, EngineError> {
        let mut command = self.command(args, env);
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
