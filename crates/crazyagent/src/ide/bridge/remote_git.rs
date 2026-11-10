use super::{GIT_ARG_BYTES_MAX, GIT_ARGS_MAX, GIT_TIME};
use crate::engine::EngineError;
use crate::engine::owner::failure;
use crate::protocol::ProjectId;
use rustix::process::{Pid, Signal};
use serde::Serialize;
use std::ffi::{OsStr, OsString};
use std::io::Read;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

const GIT: &str = "/usr/bin/git";
const PROTOCOLS: &str = "https:http:ssh:git";
const SCHEMES: [&str; 6] = ["https", "http", "ssh", "git", "git+ssh", "ssh+git"];
const PASSED: [&str; 7] = [
    "HOME",
    "PATH",
    "USER",
    "LOGNAME",
    "SSH_AUTH_SOCK",
    "TMPDIR",
    "XDG_CONFIG_HOME",
];
const FIXED: [(&str, &str); 9] = [
    ("GIT_TERMINAL_PROMPT", "0"),
    ("GIT_ASKPASS", ""),
    ("SSH_ASKPASS_REQUIRE", "never"),
    ("GIT_ALLOW_PROTOCOL", PROTOCOLS),
    ("GIT_NO_LAZY_FETCH", "1"),
    ("GIT_OPTIONAL_LOCKS", "0"),
    ("LANGUAGE", "en"),
    ("LANG", "en_US.UTF-8"),
    ("LC_ALL", "en_US.UTF-8"),
];
const HARDENING: [&str; 21] = [
    "--no-replace-objects",
    "-c",
    "core.hooksPath=/dev/null",
    "-c",
    "core.fsmonitor=false",
    "-c",
    "core.pager=cat",
    "-c",
    "core.askPass=",
    "-c",
    "credential.interactive=never",
    "-c",
    "submodule.recurse=false",
    "-c",
    "fetch.recurseSubmodules=false",
    "-c",
    "push.recurseSubmodules=no",
    "-c",
    "gc.auto=0",
    "-c",
    "maintenance.auto=false",
];
/// The most bytes of each of git's two output streams an answer carries, its last ones, so
/// both fit one bridge frame once escaped for JSON.
pub(super) const OUTPUT_BYTES_MAX: usize = 12 * 1024;
const CONFIG_BYTES_MAX: usize = 512 * 1024;
const CHECK_TIME: Duration = Duration::from_secs(20);
const DRAIN_TIME: Duration = Duration::from_secs(2);
const REMOTES_MAX: usize = 64;
const DEPTH_DIGITS_MAX: usize = 9;
const POLL: Duration = Duration::from_millis(20);
/// The most `ide.git` calls one owner server runs at once, across its projects, and for
/// one project: each holds an IDE connection for up to its bound, so a hung remote cannot
/// take the connections every other IDE call needs.
pub(super) const GIT_CALLS_MAX: usize = 3;
pub(super) const PROJECT_GIT_CALLS_MAX: usize = 2;
const TEMP_FOLDERS: [&str; 2] = ["/private/tmp", "/private/var/folders"];

const _: () = assert!(PROJECT_GIT_CALLS_MAX <= GIT_CALLS_MAX);
const _: () = assert!(
    super::terminal::TERMINALS_MAX + super::SUBSCRIBERS_MAX + GIT_CALLS_MAX
        < super::CONNECTIONS_MAX
);

/// The git network commands the IDE may ask the engine to run for its project folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Verb {
    Fetch,
    Pull,
    Push,
    LsRemote,
}

/// Which configured remotes one command reaches: the one it names, the one git picks when
/// none is named, or every remote (`fetch --all`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Target {
    Named(String),
    Default,
    All,
}

/// What one checked IDE git command does: its verb and the remotes it reaches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Plan {
    pub(super) verb: Verb,
    pub(super) target: Target,
}

/// The engine's answer to one IDE git command: git's exit code and the last
/// `OUTPUT_BYTES_MAX` bytes of what it printed on each stream, which the IDE's git shows as
/// its own output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(super) struct GitOutcome {
    pub(super) code: i32,
    pub(super) stdout: String,
    pub(super) stderr: String,
}

/// One configuration entry as `git config --list --show-origin -z` prints it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    origin: String,
    key: String,
    value: String,
}

/// What one git process left: its exit code, none when it was ended at its time limit, and
/// the last bytes of each stream with whether earlier bytes were dropped.
struct Ran {
    status: Option<i32>,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    cut: bool,
}

/// The projects of the `ide.git` calls one owner server runs now, one entry per call.
#[derive(Default)]
pub(super) struct GitCalls {
    running: Vec<ProjectId>,
}

/// One running `ide.git` call's place in [`GitCalls`], given back when dropped, which is
/// when its git process ended or was killed.
pub(super) struct GitPlace {
    calls: Arc<Mutex<GitCalls>>,
    project: ProjectId,
}

impl Drop for GitPlace {
    fn drop(&mut self) {
        if let Ok(mut calls) = self.calls.lock()
            && let Some(at) = calls
                .running
                .iter()
                .position(|known| *known == self.project)
        {
            calls.running.swap_remove(at);
        }
    }
}

/// Takes a place for one `ide.git` call of a project: at most `PROJECT_GIT_CALLS_MAX` for
/// one project and `GIT_CALLS_MAX` in all. Inputs: the registry and the project. Output:
/// the place, or `ide_git_busy`, or `ide_unavailable` for a poisoned registry.
pub(super) fn place(
    calls: &Arc<Mutex<GitCalls>>,
    project: &ProjectId,
) -> Result<GitPlace, EngineError> {
    let mut held = calls.lock().map_err(|_| failure("ide_unavailable"))?;
    let of_project = held
        .running
        .iter()
        .filter(|known| *known == project)
        .count();
    if held.running.len() >= GIT_CALLS_MAX || of_project >= PROJECT_GIT_CALLS_MAX {
        return Err(failure("ide_git_busy"));
    }
    held.running.push(project.clone());
    assert!(held.running.len() <= GIT_CALLS_MAX);
    Ok(GitPlace {
        calls: Arc::clone(calls),
        project: project.clone(),
    })
}

/// Answers one `ide.git` call (DEC-098 addendum 13): the IDE's push, pull, fetch or
/// ls-remote for the project folder, checked by [`plan`] and [`checked_folder`], then run by
/// [`run`] outside the IDE sandbox with the owner's credentials, on a blocking thread that
/// holds the call's place until git ends. Of a pull only the fetch runs here; the IDE
/// merges or rebases what it fetched in its sandbox. git finds its helpers only on the
/// [`owner_path`] outside the project folder and the engine data root.
/// Inputs: the engine root, the project, git's arguments, the folder the IDE's git ran in
/// and the call's place. Output: the [`GitOutcome`] as JSON, or a plain refusal.
pub(super) async fn answer(
    root: PathBuf,
    project: ProjectId,
    args: Vec<String>,
    folder: String,
    place: GitPlace,
) -> Result<serde_json::Value, EngineError> {
    tokio::task::spawn_blocking(move || {
        let _held = place;
        let plan = plan(&args)?;
        let project_folder = crate::ide::workspace::folder(&root, &project)?;
        let folder = checked_folder(Path::new(&folder), &project_folder)?;
        let env = owner_env(
            std::env::vars_os(),
            &[project_folder.as_path(), root.as_path()],
        );
        let outcome = run(&folder, &env, &plan, &args, GIT_TIME)?;
        serde_json::to_value(outcome).map_err(|_| failure("ide_git_outcome_unknown"))
    })
    .await
    .map_err(|_| failure("ide_git_outcome_unknown"))?
}

/// Checks git's arguments against what VS Code's built-in git sends for its network
/// commands: `fetch`, `pull`, `push` and `ls-remote` first, then only their known flags,
/// none of which takes a separate value, a remote name and plain ref names. No `-c`,
/// `--upload-pack`, `--receive-pack`, `--exec`, `--` or `+` forced refspec passes. Input:
/// the arguments after `git`. Output: the plan, or `ide_git_refused`.
pub(super) fn plan(args: &[String]) -> Result<Plan, EngineError> {
    let refused = || failure("ide_git_refused");
    if args.len() > GIT_ARGS_MAX {
        return Err(refused());
    }
    let (first, rest) = args.split_first().ok_or_else(refused)?;
    let verb = match first.as_str() {
        "fetch" => Verb::Fetch,
        "pull" => Verb::Pull,
        "push" => Verb::Push,
        "ls-remote" => Verb::LsRemote,
        _ => return Err(refused()),
    };
    let mut positional = Vec::new();
    let (mut all, mut delete) = (false, false);
    for arg in rest {
        if arg.starts_with('-') {
            if !option(verb, arg) {
                return Err(refused());
            }
            all |= arg == "--all";
            delete |= arg == "--delete";
        } else {
            positional.push(arg.as_str());
        }
    }
    let (remote, refs) = match positional.split_first() {
        Some((remote, refs)) => (Some(*remote), refs),
        None => (None, &[][..]),
    };
    let shaped = match verb {
        Verb::Fetch => !(all && remote.is_some()),
        Verb::Pull => refs.len() <= 1,
        Verb::Push => !delete || !refs.is_empty(),
        Verb::LsRemote => remote.is_some() && refs.is_empty(),
    };
    let named = remote.is_none_or(reference);
    let specs = refs.iter().all(|spec| {
        if delete {
            reference(spec)
        } else {
            refspec(spec)
        }
    });
    if !shaped || !named || !specs {
        return Err(refused());
    }
    let target = match remote {
        Some(remote) => Target::Named(remote.to_owned()),
        None if all => Target::All,
        None => Target::Default,
    };
    Ok(Plan { verb, target })
}

/// Whether a flag is one VS Code's git sends with this verb. Inputs: the verb and the
/// argument. Output: the answer.
fn option(verb: Verb, arg: &str) -> bool {
    match verb {
        Verb::Fetch => {
            matches!(arg, "--all" | "--prune" | "--force")
                || arg.strip_prefix("--depth=").is_some_and(depth)
        }
        Verb::Pull => matches!(arg, "--tags" | "--unshallow" | "--autostash" | "-r"),
        Verb::Push => {
            matches!(
                arg,
                "--force"
                    | "--force-with-lease"
                    | "--force-if-includes"
                    | "-u"
                    | "--follow-tags"
                    | "--no-follow-tags"
                    | "--tags"
                    | "--delete"
            ) || arg.strip_prefix("--force-with-lease=").is_some_and(lease)
        }
        Verb::LsRemote => matches!(arg, "--heads" | "--tags"),
    }
}

/// Whether a fetch depth is a positive decimal of at most `DEPTH_DIGITS_MAX` digits.
fn depth(text: &str) -> bool {
    (1..=DEPTH_DIGITS_MAX).contains(&text.len())
        && !text.starts_with('0')
        && text.bytes().all(|byte| byte.is_ascii_digit())
}

/// Whether a lease is `refs/heads/<branch>:<commit>`, as VS Code's force push sends it.
fn lease(text: &str) -> bool {
    text.rsplit_once(':').is_some_and(|(name, commit)| {
        name.strip_prefix("refs/heads/")
            .is_some_and(|branch| !branch.is_empty())
            && reference(name)
            && matches!(commit.len(), 40 | 64)
            && commit.bytes().all(|byte| byte.is_ascii_hexdigit())
    })
}

/// Whether a text is a plain ref or remote name by git's naming rules, at most
/// `GIT_ARG_BYTES_MAX` bytes: no leading `-`, `+`, `/` or `.`, no space, control character,
/// `~^:?*[\`, `..`, `@{` or `//`, no part starting with `.` or ending in `.lock`, and no
/// trailing `/` or `.`. Input: the text. Output: the answer.
fn reference(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= GIT_ARG_BYTES_MAX
        && !text.starts_with(['-', '+', '/', '.'])
        && !text.ends_with(['/', '.'])
        && !text.contains("..")
        && !text.contains("@{")
        && !text.contains("//")
        && !text
            .chars()
            .any(|c| c.is_control() || c == ' ' || "~^:?*[\\".contains(c))
        && text
            .split('/')
            .all(|part| !part.starts_with('.') && !part.ends_with(".lock"))
}

/// Whether a refspec is `<ref>` or `<ref>:<ref>` with plain names, never forced or empty.
fn refspec(text: &str) -> bool {
    match text.split_once(':') {
        Some((source, destination)) => reference(source) && reference(destination),
        None => reference(text),
    }
}

/// Checks the folder the IDE's git ran in: it must be the project folder itself, a
/// repository whose `.git` is a folder, never an agent worktree or another section of the
/// workspace, whose remotes stay locked. Inputs: the folder asked for and the project's
/// canonical folder. Output: the canonical folder, `ide_git_folder_refused` or
/// `ide_git_repository_unsupported`.
pub(super) fn checked_folder(asked: &Path, project: &Path) -> Result<PathBuf, EngineError> {
    assert!(project.is_absolute());
    let refused = || failure("ide_git_folder_refused");
    if !asked.is_absolute() {
        return Err(refused());
    }
    let asked = crate::sandbox::canonical(asked).map_err(|_| refused())?;
    if asked != project {
        return Err(refused());
    }
    let unsupported = || failure("ide_git_repository_unsupported");
    let dot = std::fs::symlink_metadata(asked.join(".git")).map_err(|_| unsupported())?;
    if !dot.is_dir() {
        return Err(unsupported());
    }
    Ok(asked)
}

/// The environment the engine's git runs with for the owner: from the engine's own
/// variables only the home, path, user, owner key agent socket, temp and XDG config folders,
/// the path kept to [`owner_path`], then no prompt or askpass of any kind, only the https,
/// http, ssh and git transports, and English messages, which VS Code's git reads. Inputs:
/// the engine's variables and the folders the IDE or an agent may write. Output: the
/// pairs, each name once.
pub(super) fn owner_env(
    source: impl IntoIterator<Item = (OsString, OsString)>,
    denied: &[&Path],
) -> Vec<(OsString, OsString)> {
    let mut env: Vec<(OsString, OsString)> = Vec::new();
    for (name, value) in source {
        let passed = PASSED
            .iter()
            .any(|kept| name.as_os_str() == OsStr::new(kept));
        if passed && !env.iter().any(|(known, _)| *known == name) {
            let value = if name == "PATH" {
                owner_path(&value, denied)
            } else {
                value
            };
            env.push((name, value));
        }
    }
    env.extend(
        FIXED
            .iter()
            .map(|(name, value)| (OsString::from(name), OsString::from(value))),
    );
    assert!(env.len() <= PASSED.len() + FIXED.len());
    env
}

/// The folders of a PATH git may run `ssh`, a credential helper or `gpg` from outside the
/// IDE sandbox: absolute and existing, given by their real path so no link can be swapped
/// later, and outside every denied folder and the system temp folders, where the IDE or an
/// agent could plant a program. Inputs: the PATH value and the denied folders. Output: the
/// kept folders joined as a PATH.
pub(super) fn owner_path(value: &OsStr, denied: &[&Path]) -> OsString {
    let denied: Vec<PathBuf> = denied
        .iter()
        .copied()
        .chain(TEMP_FOLDERS.iter().map(Path::new))
        .map(|folder| crate::sandbox::canonical(folder).unwrap_or_else(|_| folder.to_path_buf()))
        .collect();
    let kept: Vec<PathBuf> = std::env::split_paths(value)
        .filter(|entry| entry.is_absolute())
        .filter_map(|entry| crate::sandbox::canonical(&entry).ok())
        .filter(|real| {
            real.is_dir()
                && real.to_str().is_some_and(|text| !text.contains(':'))
                && !denied.iter().any(|folder| real.starts_with(folder))
        })
        .collect();
    let joined = std::env::join_paths(&kept).unwrap_or_default();
    assert!(kept.is_empty() || !joined.is_empty());
    joined
}

/// Runs one checked IDE git command in the project folder outside the IDE sandbox, after
/// three checks against the configuration git will use: every file it reads lies outside
/// the project folder or is the folder's own `.git/config` or `.git/config.worktree`, which
/// the IDE sandbox cannot write; every remote reached is configured there with a URL; and
/// every URL it fetches from or pushes to, `insteadOf` applied, uses https, http, ssh or
/// git. git runs the [`engine_args`], so a pull only fetches, with [`HARDENING`] (no hooks,
/// fsmonitor, pager, askpass, submodules, replace objects or automatic maintenance), the
/// [`owner_env`] environment and its own process group, which is killed at `limit`.
/// Inputs: the canonical project folder, the environment, the plan, git's arguments and
/// the time limit. Output: the outcome, or
/// `ide_git_config_refused`, `ide_git_remote_unknown`, `ide_git_transport_refused`,
/// `ide_git_unavailable` or `ide_git_outcome_unknown`.
pub(super) fn run(
    folder: &Path,
    env: &[(OsString, OsString)],
    plan: &Plan,
    args: &[String],
    limit: Duration,
) -> Result<GitOutcome, EngineError> {
    assert!(
        args.first()
            .is_some_and(|first| first == verb_word(plan.verb))
    );
    let entries = config(folder, env)?;
    if !entries
        .iter()
        .all(|entry| origin_allowed(&entry.origin, folder))
    {
        return Err(failure("ide_git_config_refused"));
    }
    let known = remotes(&entries);
    if known.len() > REMOTES_MAX {
        return Err(failure("ide_git_config_refused"));
    }
    let default = match plan.target {
        Target::Default => {
            let branch = branch(folder, env)?;
            Some(default_remote(plan.verb, branch.as_deref(), &entries))
        }
        Target::Named(_) | Target::All => None,
    };
    let reached: Vec<String> = match &plan.target {
        Target::Named(name) => vec![name.clone()],
        Target::All => known.iter().map(|name| (*name).to_owned()).collect(),
        Target::Default => default.iter().cloned().collect(),
    };
    assert!(!reached.is_empty() || plan.target == Target::All);
    for remote in &reached {
        if !known.contains(&remote.as_str()) {
            return Err(failure("ide_git_remote_unknown"));
        }
        let urls = urls(folder, env, remote, plan.verb == Verb::Push)?;
        if urls.is_empty() || !urls.iter().all(|url| transport_allowed(url)) {
            return Err(failure("ide_git_transport_refused"));
        }
    }
    let argv = engine_args(plan, args, default.as_deref());
    let ran = ran(git(folder, env, &argv), OUTPUT_BYTES_MAX, limit)?;
    let code = ran
        .status
        .ok_or_else(|| failure("ide_git_outcome_unknown"))?;
    Ok(GitOutcome {
        code,
        stdout: text(&ran.stdout, OUTPUT_BYTES_MAX),
        stderr: text(&ran.stderr, OUTPUT_BYTES_MAX),
    })
}

/// The arguments the engine's git runs for one checked command. A pull runs only its
/// network step, a fetch of the same remote and branch keeping `--tags` and
/// `--unshallow`, since its merge or rebase writes the working tree and so runs in the IDE
/// sandbox. A command that names no remote names the one checked, so git cannot pick
/// another. Inputs: the plan, the IDE's arguments and the checked default remote, present
/// exactly for `Target::Default`. Output: git's arguments after the hardening.
pub(super) fn engine_args(plan: &Plan, args: &[String], default: Option<&str>) -> Vec<String> {
    assert_eq!(default.is_some(), plan.target == Target::Default);
    assert!(
        args.first()
            .is_some_and(|first| first == verb_word(plan.verb))
    );
    let rest = &args[1..];
    let verb = match plan.verb {
        Verb::Pull => Verb::Fetch,
        verb => verb,
    };
    let mut argv = vec![verb_word(verb).to_owned()];
    argv.extend(
        rest.iter()
            .filter(|arg| {
                plan.verb != Verb::Pull
                    || !arg.starts_with('-')
                    || matches!(arg.as_str(), "--tags" | "--unshallow")
            })
            .cloned(),
    );
    argv.extend(default.map(str::to_owned));
    assert!(argv.len() <= args.len() + 1);
    argv
}

/// The first argument of a verb. Input: the verb. Output: its word.
fn verb_word(verb: Verb) -> &'static str {
    match verb {
        Verb::Fetch => "fetch",
        Verb::Pull => "pull",
        Verb::Push => "push",
        Verb::LsRemote => "ls-remote",
    }
}

/// Whether a URL git reaches uses an allowed transport: `https`, `http`, `ssh`, `git`,
/// `git+ssh` or `ssh+git` URLs, or the scp form `host:path`. Local paths, `file:`, `ext::`
/// and every other `<helper>::` form are refused, as is anything starting with `-`, any
/// control character and an empty host. Input: the URL. Output: the answer.
pub(super) fn transport_allowed(url: &str) -> bool {
    if url.is_empty()
        || url.starts_with('-')
        || url.contains("::")
        || url.chars().any(char::is_control)
    {
        return false;
    }
    if let Some((scheme, rest)) = url.split_once("://") {
        return SCHEMES.contains(&scheme.to_ascii_lowercase().as_str())
            && !rest.is_empty()
            && !rest.starts_with(['-', '/']);
    }
    url.split_once(':').is_some_and(|(host, path)| {
        !host.is_empty() && !host.contains('/') && !host.starts_with('-') && !path.is_empty()
    })
}

/// Reads the configuration git will use in the folder, with [`HARDENING`] and the owner's
/// environment, every entry with the file it came from. A `.git/commondir` is refused first:
/// no project folder has one, it would send git to another folder's configuration, and the
/// lookup finds every spelling a case-insensitive volume folds together, where git reads it
/// too. Inputs: the folder and environment.
/// Output: the entries, or `ide_git_config_refused` when git fails, prints more than
/// `CONFIG_BYTES_MAX` bytes or prints something else than `-z` entries.
fn config(folder: &Path, env: &[(OsString, OsString)]) -> Result<Vec<Entry>, EngineError> {
    let refused = || failure("ide_git_config_refused");
    if std::fs::symlink_metadata(folder.join(".git").join("commondir")).is_ok() {
        return Err(refused());
    }
    let ran = ran(
        git(folder, env, &["config", "--list", "--show-origin", "-z"]),
        CONFIG_BYTES_MAX,
        CHECK_TIME,
    )?;
    if ran.status != Some(0) || ran.cut {
        return Err(refused());
    }
    entries(&ran.stdout).ok_or_else(refused)
}

/// Splits `git config --list --show-origin -z` output into entries: an origin, a NUL, the
/// key, a newline and the value, a NUL, again and again; a key without a value has no
/// newline. Input: the bytes. Output: the entries, or None for anything else.
fn entries(bytes: &[u8]) -> Option<Vec<Entry>> {
    let text = std::str::from_utf8(bytes).ok()?;
    if text.is_empty() {
        return Some(Vec::new());
    }
    let parts: Vec<&str> = text.strip_suffix('\0')?.split('\0').collect();
    if !parts.len().is_multiple_of(2) {
        return None;
    }
    Some(
        parts
            .chunks(2)
            .map(|pair| {
                let (key, value) = pair[1].split_once('\n').unwrap_or((pair[1], ""));
                Entry {
                    origin: pair[0].to_owned(),
                    key: key.to_owned(),
                    value: value.to_owned(),
                }
            })
            .collect(),
    )
}

/// Whether one entry's origin is safe for git running outside the IDE sandbox: the command
/// line, or a file that is outside the project folder or is its `.git/config` or
/// `.git/config.worktree`, which the IDE sandbox denies writing. An include planted in the
/// project folder, an origin that is not a file and a file that cannot be resolved are
/// refused. Inputs: the origin and the canonical project folder. Output: the answer.
fn origin_allowed(origin: &str, folder: &Path) -> bool {
    if origin == "command line:" {
        return true;
    }
    let Some(file) = origin.strip_prefix("file:") else {
        return false;
    };
    let Ok(real) = crate::sandbox::canonical(&folder.join(file)) else {
        return false;
    };
    let git = folder.join(".git");
    !real.starts_with(folder) || real == git.join("config") || real == git.join("config.worktree")
}

/// The configured remotes: every name with a `remote.<name>.url` entry, sorted, each once.
/// A remote known only from a `.git/remotes` or `.git/branches` file is not one.
fn remotes(entries: &[Entry]) -> Vec<&str> {
    let mut names: Vec<&str> = entries
        .iter()
        .filter_map(|entry| entry.key.strip_prefix("remote.")?.strip_suffix(".url"))
        .filter(|name| !name.is_empty())
        .collect();
    names.sort_unstable();
    names.dedup();
    names
}

/// The last value of a key, the one git uses. Inputs: the entries and the key. Output: the
/// value, or None.
fn last<'a>(entries: &'a [Entry], key: &str) -> Option<&'a str> {
    entries
        .iter()
        .rev()
        .find(|entry| entry.key == key)
        .map(|entry| entry.value.as_str())
}

/// The remote git picks when a command names none, as git does: for a push the branch's
/// `pushRemote`, then `remote.pushDefault`, then the branch's `remote`; for the others the
/// branch's `remote`; then `origin`. Inputs: the verb, the current branch and the entries.
/// Output: the remote name, checked by the caller against the configured remotes.
fn default_remote(verb: Verb, branch: Option<&str>, entries: &[Entry]) -> String {
    let of_branch =
        |name: &str| branch.and_then(|branch| last(entries, &format!("branch.{branch}.{name}")));
    let chosen = match verb {
        Verb::Push => of_branch("pushremote")
            .or_else(|| last(entries, "remote.pushdefault"))
            .or_else(|| of_branch("remote")),
        Verb::Fetch | Verb::Pull | Verb::LsRemote => of_branch("remote"),
    };
    chosen.unwrap_or("origin").to_owned()
}

/// The branch HEAD names, or None when HEAD is detached. Inputs: the folder and
/// environment. Output: the branch name, or `ide_git_config_refused`.
fn branch(folder: &Path, env: &[(OsString, OsString)]) -> Result<Option<String>, EngineError> {
    let ran = ran(
        git(folder, env, &["symbolic-ref", "--quiet", "HEAD"]),
        GIT_ARG_BYTES_MAX + 64,
        CHECK_TIME,
    )?;
    match ran.status {
        Some(0) if !ran.cut => Ok(std::str::from_utf8(&ran.stdout)
            .ok()
            .and_then(|text| text.trim_end().strip_prefix("refs/heads/"))
            .map(str::to_owned)),
        Some(1) => Ok(None),
        _ => Err(failure("ide_git_config_refused")),
    }
}

/// Every URL git uses for a remote, `insteadOf` or `pushInsteadOf` applied: its push URLs
/// for a push, its fetch URLs otherwise. Inputs: the folder, environment, remote and whether
/// it is a push. Output: the URLs, or `ide_git_remote_unknown`.
fn urls(
    folder: &Path,
    env: &[(OsString, OsString)],
    remote: &str,
    push: bool,
) -> Result<Vec<String>, EngineError> {
    let mut args = vec!["remote", "get-url", "--all"];
    if push {
        args.push("--push");
    }
    args.push(remote);
    let ran = ran(git(folder, env, &args), CONFIG_BYTES_MAX, CHECK_TIME)?;
    if ran.status != Some(0) || ran.cut {
        return Err(failure("ide_git_remote_unknown"));
    }
    let text = String::from_utf8(ran.stdout).map_err(|_| failure("ide_git_transport_refused"))?;
    Ok(text.lines().map(str::to_owned).collect())
}

/// Builds one engine git process for the IDE: `/usr/bin/git` with [`HARDENING`] before the
/// arguments, a cleared environment holding only `env` and the folder's `.git` as git's
/// common folder, so a `commondir` planted after the check in [`config`] redirects nothing,
/// the project folder as its working folder, no input, both outputs piped and its own process group. Inputs: the folder, the
/// environment and the arguments. Output: the unspawned command.
fn git<S: AsRef<OsStr>>(folder: &Path, env: &[(OsString, OsString)], args: &[S]) -> Command {
    let mut command = Command::new(GIT);
    command
        .env_clear()
        .envs(env.iter().cloned())
        .env("GIT_COMMON_DIR", folder.join(".git"))
        .current_dir(folder)
        .args(HARDENING)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    command
}

/// Runs one process to its end or its time limit, keeping the last `keep` bytes of each
/// output stream; at the limit its whole process group is killed. Readers that outlive the
/// process, such as through a descendant holding a pipe, are waited for at most
/// `DRAIN_TIME`. Inputs: the command, the bytes kept per stream and the limit. Output: what
/// it left, or `ide_git_unavailable` when it cannot start.
fn ran(mut command: Command, keep: usize, limit: Duration) -> Result<Ran, EngineError> {
    let mut child =
        crate::spawn_gate::spawn(&mut command).map_err(|_| failure("ide_git_unavailable"))?;
    let group = Pid::from_child(&child);
    assert!(group.as_raw_nonzero().get() > 1);
    let stdout = reader(child.stdout.take(), keep);
    let stderr = reader(child.stderr.take(), keep);
    let deadline = Instant::now() + limit;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status.code().unwrap_or(128)),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(POLL),
            _ => break None,
        }
    };
    if status.is_none() {
        let _ = rustix::process::kill_process_group(group, Signal::KILL);
        let _ = child.kill();
        let _ = child.wait();
    }
    let drained = |receiver: mpsc::Receiver<(Vec<u8>, bool)>| {
        receiver
            .recv_timeout(DRAIN_TIME)
            .unwrap_or((Vec::new(), true))
    };
    let (stdout, out_cut) = drained(stdout);
    let (stderr, err_cut) = drained(stderr);
    assert!(stdout.len() <= keep && stderr.len() <= keep);
    Ok(Ran {
        status,
        stdout,
        stderr,
        cut: out_cut || err_cut,
    })
}

/// Reads one output pipe on its own thread with [`tail`], so neither stream fills while the
/// other is read. Inputs: the pipe, always present for a piped stream, and the bound.
/// Output: where the thread sends what it kept.
fn reader<R: Read + Send + 'static>(
    pipe: Option<R>,
    keep: usize,
) -> mpsc::Receiver<(Vec<u8>, bool)> {
    let (sender, receiver) = mpsc::channel();
    if let Some(pipe) = pipe {
        std::thread::spawn(move || {
            let _ = sender.send(tail(pipe, keep));
        });
    }
    receiver
}

/// Reads a pipe to its end, keeping only its last `keep` bytes. Inputs: the pipe and the
/// bound. Output: the kept bytes and whether earlier ones were dropped.
fn tail(mut pipe: impl Read, keep: usize) -> (Vec<u8>, bool) {
    let mut kept = Vec::new();
    let mut cut = false;
    let mut chunk = [0u8; 8192];
    loop {
        match pipe.read(&mut chunk) {
            Ok(0) => break,
            Ok(read) => {
                kept.extend_from_slice(&chunk[..read]);
                if kept.len() > keep.saturating_mul(2) {
                    kept.drain(..kept.len() - keep);
                    cut = true;
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => break,
        }
    }
    if kept.len() > keep {
        kept.drain(..kept.len() - keep);
        cut = true;
    }
    assert!(kept.len() <= keep);
    (kept, cut)
}

/// The text of one output stream as the IDE shows it: UTF-8 with invalid bytes replaced, a
/// carriage return as a newline, other control characters but tab removed, and only its
/// last `keep` bytes. Inputs: the bytes and the bound. Output: the text.
pub(super) fn text(bytes: &[u8], keep: usize) -> String {
    let mut text: String = String::from_utf8_lossy(bytes)
        .chars()
        .filter_map(|c| match c {
            '\r' => Some('\n'),
            '\n' | '\t' => Some(c),
            c if c.is_control() => None,
            c => Some(c),
        })
        .collect();
    if text.len() > keep {
        let mut start = text.len() - keep;
        while !text.is_char_boundary(start) {
            start += 1;
        }
        text.drain(..start);
    }
    assert!(text.len() <= keep);
    text
}

pub(super) mod index;

#[cfg(test)]
pub(crate) mod tests;
