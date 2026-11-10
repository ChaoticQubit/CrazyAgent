use super::flow::{self, ENGINE, hold, pending_ref, verify_objects};
use super::git::{Repo, failure, object_id};
use super::owner;
use super::store::{self, Intent, Record};
use super::{AfterRegeneration, AgentKind, ChangeSetId, ChangeState, diff};
use crate::engine::EngineError;
use crate::sandbox::ProjectSandbox;
use crate::sandbox::toolchains::{self, ToolHomes, Toolchains};
use rusqlite::Connection;
use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

const LOCKFILES: [(&str, &str); 6] = [
    ("Cargo.lock", "cargo"),
    ("uv.lock", "uv"),
    ("package-lock.json", "npm"),
    ("pnpm-lock.yaml", "pnpm"),
    ("yarn.lock", "yarn"),
    ("poetry.lock", "poetry"),
];
/// The most lockfiles one regeneration rewrites.
pub(super) const PATHS_MAX: usize = 16;
const LOCKFILE_BYTES_MAX: usize = 32 << 20;
const VERSION_BYTES_MAX: usize = 4096;
const LISTING_BYTES_MAX: usize = 1 << 20;
const VERSION_TIME: Duration = Duration::from_secs(30);
const REGENERATE_TIME: Duration = Duration::from_secs(300);
const POLL: Duration = Duration::from_millis(20);
const ENV: &str = "/usr/bin/env";
const SCRATCH_FOLDER: &str = "regenerate";

/// The change sets a regeneration is rewriting now, by id with their repository: one at a
/// time for each set.
static RUNNING: Mutex<BTreeMap<String, PathBuf>> = Mutex::new(BTreeMap::new());

#[cfg(test)]
/// Stand-in tools a landing test commits into its repository: the owner checkout maps to the
/// worktree folder put first on its tools' PATH and the time one whole regeneration may take.
pub(super) static TEST_TOOLS: std::sync::Mutex<BTreeMap<PathBuf, (String, Duration)>> =
    std::sync::Mutex::new(BTreeMap::new());

/// The folder of the temporary worktree a landing test puts first on its tools' PATH, with
/// the time its whole regeneration may take. Input: the owner checkout. Output: both, never
/// any outside tests.
fn test_tools(repository: &Path) -> Option<(String, Duration)> {
    #[cfg(test)]
    return TEST_TOOLS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(repository)
        .cloned();
    #[cfg(not(test))]
    {
        let _ = repository;
        None
    }
}

/// The program that writes a known lockfile, by the lockfile's file name: `cargo` for
/// `Cargo.lock`, `uv`, `npm`, `pnpm`, `yarn` and `poetry` for theirs. Input: the lockfile's
/// path in the repository. Output: the program's name, or None for another file or a path
/// that leaves the repository.
pub(super) fn program_for(path: &str) -> Option<&'static str> {
    let parts: Vec<&str> = path.split('/').collect();
    if parts
        .iter()
        .any(|part| part.is_empty() || *part == "." || *part == "..")
    {
        return None;
    }
    let name = parts.last()?;
    LOCKFILES
        .iter()
        .find(|(file, _)| file == name)
        .map(|(_, program)| *program)
}

/// The command that rewrites a known lockfile from its manifests, for the major version of
/// its tool (OS-8): none for Yarn 1, which has no lockfile-only mode, `--no-update` for
/// Poetry 1, and none when the tool is missing or printed no version. Inputs: the lockfile's
/// path in the repository and the tool's major version. Output: the program and its
/// arguments, or None when no command fits.
pub(super) fn tool_for(path: &str, major: Option<u32>) -> Option<&'static [&'static str]> {
    let major = major?;
    let program = program_for(path)?;
    let command: &'static [&'static str] = match (program, major) {
        ("cargo", _) => &["cargo", "update", "--workspace", "--offline"],
        ("uv", _) => &["uv", "lock"],
        ("npm", _) => &["npm", "install", "--package-lock-only", "--ignore-scripts"],
        ("pnpm", _) => &["pnpm", "install", "--lockfile-only", "--ignore-scripts"],
        ("yarn", 2..) => &["yarn", "install", "--mode", "update-lockfile"],
        ("poetry", 2..) => &["poetry", "lock"],
        ("poetry", _) => &["poetry", "lock", "--no-update"],
        _ => return None,
    };
    assert_eq!(
        command[0], program,
        "a lockfile's command runs its own tool"
    );
    Some(command)
}

/// Reads a tool's major version from what its `--version` printed: the leading number of
/// the first word that starts with a digit, or with `v` and a digit, and goes on with a dot.
/// Input: the printed bytes. Output: the major version, or None when there is none.
pub(super) fn major_of(printed: &[u8]) -> Option<u32> {
    let text = std::str::from_utf8(printed).ok()?;
    let leading = |word: &str| -> Option<usize> {
        let digits = word.find(|c: char| !c.is_ascii_digit())?;
        (digits > 0 && word[digits..].starts_with('.')).then_some(digits)
    };
    let word = text
        .split(|c: char| c.is_whitespace() || c == '(' || c == ')')
        .map(|word| word.strip_prefix('v').unwrap_or(word))
        .find(|word| leading(word).is_some())?;
    word[..leading(word)?].parse().ok()
}

/// How one regeneration ended: its commit is the set's head and the agent's worktree moved
/// there, it rewrote nothing and the clash goes to the agent, or the set or main moved
/// meanwhile and the clash is checked again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Regenerated {
    Applied,
    Refused,
    Stale,
}

/// Marks one set's regeneration as running until it is dropped, also when it panics.
pub(super) struct Running(String);

impl Drop for Running {
    fn drop(&mut self) {
        running().remove(&self.0);
    }
}

/// The ids of the sets whose regeneration runs now. Output: the locked set.
fn running() -> MutexGuard<'static, BTreeMap<String, PathBuf>> {
    RUNNING.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Counts a stand-in regeneration of a repository as running until the guard drops, for the
/// rows on the repository limit. Inputs: the repository and a unique id. Output: the guard.
#[cfg(test)]
pub(super) fn occupy(repository: &Path, id: &str) -> Running {
    running().insert(id.to_owned(), repository.to_path_buf());
    Running(id.to_owned())
}

/// Tells whether a set's regeneration still runs, for the rows that wait for its end. Input:
/// the set. Output: the answer.
#[cfg(test)]
pub(super) fn is_running(id: &ChangeSetId) -> bool {
    running().contains_key(id.as_str())
}

/// The engine runtime folder of the store, where regenerations keep their scratch. Input:
/// the store. Output: the folder, or None for a store with no file.
fn runtime_of(db: &Connection) -> Option<PathBuf> {
    db.path()
        .filter(|path| !path.is_empty())
        .and_then(|path| Path::new(path).parent())
        .map(Path::to_path_buf)
}

/// Tells whether a set may still be regenerated: Pending with no intent, of an agent no
/// stop holds, in a project whose team is not paused (OS-8). Inputs: the store
/// and the set. Output: the answer.
fn current(db: &Connection, record: &Record) -> Result<bool, EngineError> {
    if record.state != ChangeState::Pending || record.intent.is_some() {
        return Ok(false);
    }
    let stopped = match record.agent.id.parse::<i64>() {
        Ok(agent) if record.agent.kind == AgentKind::Sub => crate::team::stopped(db, agent)?,
        _ => false,
    };
    Ok(!stopped && !crate::team::paused(db, &record.project)?)
}

/// Starts rewriting a clash made of known lockfiles only off the caller's thread (OS-8), so
/// a turn end never waits on a tool or holds the repository while one runs. One runs at a
/// time for each set; a set already being rewritten counts as started. When it ends without
/// landing, `after` gets the set and whether another regeneration may start: false after a
/// refusal, which hands the clash to the agent, true after the set or main moved. Inputs:
/// the store, the set, its clashing paths and what runs after. Output: whether one runs now.
pub(super) fn regenerate_later(
    db: &Connection,
    record: &Record,
    paths: &[String],
    after: AfterRegeneration,
) -> Result<bool, EngineError> {
    if !cfg!(target_os = "macos")
        || paths.is_empty()
        || paths.len() > PATHS_MAX
        || paths.iter().any(|path| program_for(path).is_none())
        || !current(db, record)?
    {
        return Ok(false);
    }
    let Some(runtime) = runtime_of(db) else {
        return Ok(false);
    };
    if running()
        .insert(record.id.as_str().to_owned(), record.repository.clone())
        .is_some()
    {
        return Ok(true);
    }
    let guard = Running(record.id.as_str().to_owned());
    let (id, paths) = (record.id.clone(), paths.to_vec());
    #[cfg(test)]
    let home = super::git::OWNER_HOME.with(|home| home.borrow().clone());
    let spawned = std::thread::Builder::new()
        .name("crazyagent-regenerate".into())
        .spawn(move || {
            #[cfg(test)]
            super::git::OWNER_HOME.with(|owner| *owner.borrow_mut() = home);
            let ended = super::open(&runtime)
                .and_then(|db| regenerated(&db, &id, &paths).map(|how| (db, how)));
            drop(guard);
            match ended {
                Ok((_, Regenerated::Applied)) => {}
                Ok((db, Regenerated::Refused)) => after(&db, &id, false),
                Ok((db, Regenerated::Stale)) => after(&db, &id, true),
                Err(error) => eprintln!(
                    "landing_regenerate_failed code={}",
                    crate::protocol::plain_code(&error.0)
                ),
            }
        });
    Ok(spawned.is_ok())
}

/// Rewrites the clashing lockfiles of a held set at once, as a regeneration does, for the
/// lockfile tests. Inputs: the store, the set and its clashing lockfiles. Output: the new
/// head, or None when nothing landed; the set is reloaded either way.
#[cfg(test)]
pub(super) fn regenerate(
    db: &Connection,
    record: &mut Record,
    paths: &[String],
) -> Result<Option<String>, EngineError> {
    let how = regenerated(db, &record.id, paths)?;
    *record = store::load(db, &record.id)?;
    Ok((how == Regenerated::Applied).then(|| record.head.clone()))
}

/// Rewrites the known lockfiles that alone clash between a change set and the tip it lands
/// on, as the engine's own commit on top of the set (OS-8). Under [`hold`] it checks the
/// set and merges the tip into its head in the owner repository, refusing an owner merge
/// driver or filter on any path either side touched. Without the hold it takes the tip's
/// side of each lockfile, runs each tool confined like an agent in a temporary linked
/// worktree within one deadline, ends every process of each run, refuses any other path
/// the tools changed and reads each lockfile through handles that follow no link. Under
/// the hold again, unless the set, main, the owner's stop or the team's pause changed, it
/// saves the commit as the set's head, moves the pending ref and the agent's quiet
/// worktree there and tells the agent. The engine never runs git in the agent's own clone
/// (CC-N1) and the temporary worktree is removed on every path. Inputs: the store, the set
/// and its clashing lockfiles. Output: how it ended.
fn regenerated(
    db: &Connection,
    id: &ChangeSetId,
    paths: &[String],
) -> Result<Regenerated, EngineError> {
    assert!(
        !paths.is_empty() && paths.len() <= PATHS_MAX,
        "a regeneration names 1 to 16 lockfiles"
    );
    assert!(
        paths.iter().all(|path| program_for(path).is_some()),
        "a regeneration names known lockfiles only"
    );
    if !cfg!(target_os = "macos") {
        return Ok(Regenerated::Refused);
    }
    let repository = store::load(db, id)?.repository;
    let Some(runtime) = runtime_of(db) else {
        return Ok(Regenerated::Refused);
    };
    let git_dir = repository.join(".git");
    let owner = Repo::open(&repository, &git_dir, None)?;
    let root = repository
        .canonicalize()
        .map_err(|_| failure("changes_git_failed"))?;
    let mut scratch = Scratch::make(&owner, &root, &runtime, id.as_str())?;
    let (record, tip, tree) = {
        let _held = hold(&repository);
        let record = store::load(db, id)?;
        if !current(db, &record)? {
            return Ok(Regenerated::Stale);
        }
        if super::subagent::lead_of(db, &record.project, &record.agent, &repository)?.is_some() {
            return Ok(Regenerated::Refused);
        }
        let Some((_, tip)) = owner::land_on(db, &owner, &record.project)? else {
            return Ok(Regenerated::Refused);
        };
        verify_objects(&repository, &[&record.head, &tip], &[&record.base])?;
        let from = owner::measured_from(db, &owner, &record, &tip)?;
        let touched = owner::merge_paths(&owner, &from, &tip, &record.head)?;
        if owner::uses_configured_commands(&owner, &touched, &[&from, &tip, &record.head])? {
            return Ok(Regenerated::Refused);
        }
        let (tree, conflicts) = owner::merge(&owner, &from, &tip, &record.head)?;
        if conflicts.is_empty() || conflicts.iter().any(|path| !paths.contains(path)) {
            return Ok(Regenerated::Refused);
        }
        (record, tip, tree)
    };
    let index =
        Repo::open(&repository, &git_dir, None)?.private_index(&scratch.folder.join("index"));
    index.capture(&["read-tree", &tree], 256)?;
    for path in paths {
        let Some((mode, blob)) = tip_blob(&owner, &tip, path)? else {
            return Ok(Regenerated::Refused);
        };
        index.capture(&["update-index", "--cacheinfo", &mode, &blob, path], 256)?;
    }
    let merged_tree = object_id(&index.capture(&["write-tree"], 256)?)?;
    let merged = owner.commit_tree(
        &merged_tree,
        &[&record.head, &tip],
        "Merge main for its lockfiles\n",
        ENGINE,
    )?;
    let Some(worktree) = scratch.add_worktree(&merged)? else {
        return Ok(Regenerated::Refused);
    };
    let sandbox = match confined(db, &record, &root, &runtime, &worktree, &scratch) {
        Ok(sandbox) => sandbox,
        Err(code) => {
            eprintln!("landing_regenerate_unconfined code={code}");
            return Ok(Regenerated::Refused);
        }
    };
    let profile = scratch.folder.join("profile.sb");
    let (first, total) = match test_tools(&repository) {
        Some((folder, total)) => (Some(worktree.join(folder)), total),
        None => (None, REGENERATE_TIME),
    };
    let deadline = Instant::now() + total;
    let mut majors: BTreeMap<&str, Option<u32>> = BTreeMap::new();
    for path in paths {
        let program = program_for(path).ok_or_else(|| failure("changes_git_failed"))?;
        let folder = match path.rsplit_once('/') {
            Some((parent, _)) => worktree.join(parent),
            None => worktree.clone(),
        };
        if !plain_folder(&worktree, &folder) {
            return Ok(Regenerated::Refused);
        }
        let major = *majors.entry(program).or_insert_with(|| {
            run(
                &sandbox,
                &profile,
                &[program, "--version"],
                &folder,
                deadline.min(Instant::now() + VERSION_TIME),
                true,
                first.as_deref(),
            )
            .filter(|(code, _)| *code == 0)
            .and_then(|(_, printed)| major_of(&printed))
        });
        let Some(command) = tool_for(path, major) else {
            return Ok(Regenerated::Refused);
        };
        let ran = run(
            &sandbox,
            &profile,
            command,
            &folder,
            deadline,
            false,
            first.as_deref(),
        );
        if ran.map(|(code, _)| code) != Some(0) {
            return Ok(Regenerated::Refused);
        }
    }
    if changed_outside(&repository, &scratch, &worktree, &merged, paths)? {
        return Ok(Regenerated::Refused);
    }
    for path in paths {
        let Some(bytes) = read_plain(&worktree, path) else {
            return Ok(Regenerated::Refused);
        };
        let Some((mode, _)) = tip_blob(&owner, &tip, path)? else {
            return Ok(Regenerated::Refused);
        };
        let output = owner.exec(&["hash-object", "-w", "--stdin"], Some(&bytes), &[], 256)?;
        if output.code != 0 || output.overflow {
            return Err(failure("changes_git_failed"));
        }
        let blob = object_id(&output.stdout)?;
        index.capture(&["update-index", "--cacheinfo", &mode, &blob, path], 256)?;
    }
    let final_tree = object_id(&index.capture(&["write-tree"], 256)?)?;
    let files = paths.join(", ");
    let commit = owner.commit_tree(
        &final_tree,
        &[&record.head, &tip],
        &format!("Regenerate {files} on main\n"),
        ENGINE,
    )?;
    verify_objects(&repository, &[&commit], &[&record.head, &tip, &record.base])?;
    drop(scratch);
    let _held = hold(&repository);
    let mut now = store::load(db, id)?;
    let tip_now = owner::land_on(db, &owner, &now.project)?.map(|(_, tip)| tip);
    if !current(db, &now)? || now.head != record.head || tip_now.as_ref() != Some(&tip) {
        return Ok(Regenerated::Stale);
    }
    let agent = flow::agent(db, &now)?;
    if agent.head()? != now.head || !flow::holds_only(&agent.repo, &now.head)? {
        return Ok(Regenerated::Refused);
    }
    let before = now.clone();
    now.waiting = false;
    now.base = tip;
    now.head = commit.clone();
    now.conflicts = Vec::new();
    let stats = diff::stats(&owner, &now.base, &now.head)?;
    (now.files, now.added, now.removed) = (stats.files, stats.added, stats.removed);
    now.updated_ms = flow::now_ms()?;
    now.intent = Some(Intent::Reset {
        from: Some(before.head.clone()),
        target: commit.clone(),
        earlier: Some(super::store::Earlier::of(&before)),
    });
    store::save(db, &now)?;
    flow::set_engine_ref(&owner, &repository, &pending_ref(&now.id), Some(&commit))?;
    if !flow::reset_agent(&agent, &before.head, &commit)? {
        store::save(db, &before)?;
        flow::set_engine_ref(
            &owner,
            &repository,
            &pending_ref(&before.id),
            Some(&before.head),
        )?;
        return Ok(Regenerated::Refused);
    }
    now.intent = None;
    store::save(db, &now)?;
    tell_regenerated(db, &now, &files, &commit);
    Ok(Regenerated::Applied)
}

/// Tells whether the tools changed any path of the temporary worktree, tracked or new,
/// other than the lockfiles, against the merge they ran on, through the worktree's own git
/// folder and a private index, writing no object. Inputs: the owner checkout, the scratch,
/// the worktree, the merge and the lockfiles. Output: the answer; a listing git refuses or
/// cuts short counts as yes.
fn changed_outside(
    repository: &Path,
    scratch: &Scratch,
    worktree: &Path,
    merged: &str,
    paths: &[String],
) -> Result<bool, EngineError> {
    let check = Repo::open(repository, &scratch.admin, Some(worktree))?
        .private_index(&scratch.folder.join("check-index"));
    check.capture(&["read-tree", merged], 256)?;
    check.exec(
        &["update-index", "-q", "--refresh"],
        None,
        &[],
        LISTING_BYTES_MAX,
    )?;
    let mut changed = Vec::new();
    for args in [
        &["diff-files", "--name-only", "-z", "--no-renames"][..],
        &["ls-files", "-z", "--others", "--exclude-standard"][..],
    ] {
        let output = check.exec(args, None, &[], LISTING_BYTES_MAX)?;
        if output.code != 0 || output.overflow {
            return Ok(true);
        }
        changed.extend(
            output
                .stdout
                .split(|byte| *byte == 0)
                .filter(|name| !name.is_empty())
                .map(|name| String::from_utf8_lossy(name).into_owned()),
        );
    }
    Ok(changed.iter().any(|path| !paths.contains(path)))
}

/// A lockfile as a plain file in a commit, read from the listing of its folder there.
/// Inputs: the owner repository, the commit and the path. Output: its mode, `100644` or
/// `100755`, and blob id, or None when the commit has no plain file there.
fn tip_blob(
    owner: &Repo,
    commit: &str,
    path: &str,
) -> Result<Option<(String, String)>, EngineError> {
    let (folder, name) = path.rsplit_once('/').unwrap_or(("", path));
    let tree = match folder {
        "" => commit.to_owned(),
        folder => format!("{commit}:{folder}"),
    };
    let output = owner.exec(&["ls-tree", "-z", &tree], None, &[], LISTING_BYTES_MAX)?;
    if output.code != 0 || output.overflow {
        return Ok(None);
    }
    for entry in output.stdout.split(|byte| *byte == 0) {
        let entry = String::from_utf8_lossy(entry);
        let Some((meta, listed)) = entry.split_once('\t') else {
            continue;
        };
        let fields: Vec<&str> = meta.split(' ').collect();
        if listed == name
            && let [mode @ ("100644" | "100755"), "blob", blob] = fields.as_slice()
        {
            return Ok(Some(((*mode).to_owned(), object_id(blob.as_bytes())?)));
        }
    }
    Ok(None)
}

/// Tells whether a folder in the temporary worktree is a real folder reached through real
/// folders only. Inputs: the worktree and the folder. Output: the answer.
fn plain_folder(worktree: &Path, folder: &Path) -> bool {
    folder.starts_with(worktree)
        && std::fs::canonicalize(folder).is_ok_and(|real| real == folder)
        && std::fs::symlink_metadata(folder).is_ok_and(|metadata| metadata.is_dir())
}

/// Reads a rewritten lockfile from the temporary worktree when it is a plain file of at
/// most `LOCKFILE_BYTES_MAX` bytes, opening each folder from the worktree down and the
/// file itself without following a link, checking the open handle and reading from it.
/// Inputs: the worktree and the path. Output: the bytes, or None.
fn read_plain(worktree: &Path, path: &str) -> Option<Vec<u8>> {
    use rustix::fs::{CWD, Mode, OFlags};
    let folders = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let mut folder = rustix::fs::openat(CWD, worktree, folders, Mode::empty()).ok()?;
    let (parents, name) = path.rsplit_once('/').unwrap_or(("", path));
    for part in parents.split('/').filter(|part| !part.is_empty()) {
        folder = rustix::fs::openat(&folder, part, folders, Mode::empty()).ok()?;
    }
    let file = rustix::fs::openat(
        &folder,
        name,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .ok()?;
    let file = std::fs::File::from(file);
    let metadata = file.metadata().ok()?;
    if !metadata.is_file() || metadata.len() > LOCKFILE_BYTES_MAX as u64 {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(LOCKFILE_BYTES_MAX as u64 + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    (bytes.len() <= LOCKFILE_BYTES_MAX).then_some(bytes)
}

/// Builds the sandbox a lockfile's tool runs in, as an agent's is built: the project's
/// network choice with no model, its toolchains, hidden folders and the engine's guarded
/// secrets, written to the given worktree only beside its own home and temp folders, and
/// installs its profile. Inputs: the store, the change set, the owner checkout's canonical
/// root, the engine runtime folder, the temporary worktree and the scratch folders. Output:
/// the sandbox, or the plain code of what refused it.
fn confined(
    db: &Connection,
    record: &Record,
    root: &Path,
    runtime: &Path,
    worktree: &Path,
    scratch: &Scratch,
) -> Result<ProjectSandbox, &'static str> {
    let project = record.project.as_str();
    let policy = crate::sandbox::policy::load(db, project).map_err(|_| "agent_access_unread")?;
    let hidden =
        crate::sandbox::hidden_paths::load(db, project).map_err(|_| "hidden_paths_unread")?;
    let homes = ToolHomes::owner().map_err(|_| "tool_homes_unread")?;
    let tmp = toolchains::make_tmp(&scratch.home).map_err(|_| "tmp_unmade")?;
    toolchains::prepare_cargo_home(&scratch.home, &homes).map_err(|_| "cargo_home_unmade")?;
    let mut sandbox = ProjectSandbox::new(
        root,
        &scratch.home,
        &scratch.tmp,
        policy.network.sandbox(None),
    )
    .and_then(|sandbox| sandbox.guard_engine(runtime))
    .and_then(|sandbox| sandbox.allow_tools(&Toolchains::grant(&homes, &policy.allowed(root))))
    .and_then(|sandbox| sandbox.allow_scratch(&tmp))
    .and_then(|sandbox| sandbox.confine_to_worktree(worktree))
    .map_err(|_| "sandbox_refused")?;
    for folder in &hidden {
        sandbox = sandbox.hide(folder).map_err(|_| "sandbox_refused")?;
    }
    sandbox
        .install(&scratch.folder.join("profile.sb"))
        .map_err(|_| "sandbox_refused")?;
    Ok(sandbox)
}

/// Runs one program through `/usr/bin/env` in the tool sandbox, from a folder of the
/// temporary worktree, until `until`, stops its whole process group when it ends or runs
/// past that, and fires the sandbox's warden so no process of the run outlives it. Its
/// output is waited for until `until` only, never past it. Inputs: the sandbox, its
/// profile, the program and its arguments, the folder, the time it must end by, whether
/// to keep at most `VERSION_BYTES_MAX` bytes of its output and a folder to put first on
/// its PATH. Output: the exit code and the kept output, or None when it could not start,
/// was stopped by a signal, ran past the time or its processes were not all ended.
fn run(
    sandbox: &ProjectSandbox,
    profile: &Path,
    args: &[&str],
    folder: &Path,
    until: Instant,
    keep: bool,
    first: Option<&Path>,
) -> Option<(i32, Vec<u8>)> {
    let mut command: Command = sandbox.command(profile, Path::new(ENV));
    command
        .args(args)
        .current_dir(folder)
        .stdin(Stdio::null())
        .stdout(if keep { Stdio::piped() } else { Stdio::null() })
        .stderr(Stdio::null());
    if let Some(first) = first {
        let path = command
            .get_envs()
            .find(|(name, _)| *name == "PATH")
            .and_then(|(_, value)| value)
            .map(|value| value.to_os_string())
            .unwrap_or_default();
        let mut joined = first.as_os_str().to_os_string();
        joined.push(":");
        joined.push(path);
        command.env("PATH", joined);
    }
    let (mut child, warden) = crate::sandbox::warden::spawn(&mut command).ok()?;
    let group = child.id();
    let (sent, printed) = std::sync::mpsc::channel();
    if let Some(stdout) = child.stdout.take() {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let mut source = stdout.take(VERSION_BYTES_MAX as u64);
            let _ = source.read_to_end(&mut bytes);
            let _ = sent.send(bytes);
            let _ = std::io::copy(&mut source.into_inner(), &mut std::io::sink());
        });
    } else {
        drop(sent);
    }
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if Instant::now() < until => std::thread::sleep(POLL),
            _ => {
                stop_group(group);
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    stop_group(group);
    let ended = crate::sandbox::warden::fire_reported(warden, "landing_regenerate").is_ok();
    let printed = printed
        .recv_timeout(until.saturating_duration_since(Instant::now()))
        .unwrap_or_default();
    assert!(printed.len() <= VERSION_BYTES_MAX);
    if !ended {
        return None;
    }
    Some((status?.code()?, printed))
}

/// Sends SIGKILL to a tool's process group, which leads its own group, so nothing it
/// started outlives the run; a group already gone is fine. Input: the group leader's id.
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

/// Tells the set's sub-agent that the engine rewrote its lockfiles on top of its work as a
/// commit its worktree now holds, in one Note from the engine keyed to the commit. A failed send
/// is logged by its plain code. Inputs: the store, the change set, the files and the
/// commit. Output: none.
fn tell_regenerated(db: &Connection, record: &Record, files: &str, commit: &str) {
    if record.agent.kind != AgentKind::Sub {
        return;
    }
    let Ok(agent) = record.agent.id.parse::<i64>() else {
        return;
    };
    let note = crate::coord::NewMessage {
        project: record.project.clone(),
        from: crate::coord::Party::Engine,
        to: crate::coord::Party::Agent(agent),
        kind: crate::coord::MessageKind::Note,
        question: None,
        work: None,
        reply_to: None,
        body: format!("The engine regenerated {files} on top of your work as {commit}."),
        idempotency_key: Some(format!("regenerated-{commit}")),
    };
    if let Err(error) = crate::coord::send(db, note, crate::team::now_ms()) {
        eprintln!(
            "landing_regenerate_note_not_sent code={}",
            crate::protocol::plain_code(&error.0)
        );
    }
}

/// The temporary pieces of one regeneration: the engine folder holding the tool's home,
/// temp folder, private index and profile, and the linked worktree of the owner repository
/// with its admin folder, all removed when it is dropped, on every path.
struct Scratch<'a> {
    owner: &'a Repo,
    folder: PathBuf,
    home: PathBuf,
    tmp: PathBuf,
    worktree: PathBuf,
    admin: PathBuf,
}

impl<'a> Scratch<'a> {
    /// Makes the engine folder `<runtime>/regenerate/<set>` with the tool's home and temp
    /// folders, private to the owner, and names the temporary worktree and its admin
    /// folder, removing any a stopped engine left there. Inputs: the owner repository, its
    /// canonical checkout root, the engine runtime folder and the set's id. Output: the
    /// scratch.
    fn make(
        owner: &'a Repo,
        repository: &Path,
        runtime: &Path,
        id: &str,
    ) -> Result<Self, EngineError> {
        let worktrees = crate::projects_home::state_folder(repository).join("worktrees");
        let scratch = Self::named(owner, repository, runtime, id);
        scratch.clear();
        let private = |folder: &Path| {
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(folder)
                .map_err(|_| failure("changes_git_failed"))
        };
        private(&worktrees)?;
        private(&scratch.home)?;
        private(&scratch.tmp)?;
        Ok(scratch)
    }

    /// Names the pieces of one set's regeneration without making any. Inputs: the owner
    /// repository, its canonical checkout root, the engine runtime folder and the set's id.
    /// Output: the scratch, removed in full when dropped.
    fn named(owner: &'a Repo, repository: &Path, runtime: &Path, id: &str) -> Self {
        assert!(id.len() == 32 && id.bytes().all(|byte| byte.is_ascii_hexdigit()));
        let name = format!("{SCRATCH_FOLDER}-{id}");
        Self {
            owner,
            folder: runtime.join(SCRATCH_FOLDER).join(id),
            home: runtime.join(SCRATCH_FOLDER).join(id).join("home"),
            tmp: runtime.join(SCRATCH_FOLDER).join(id).join("tmp"),
            worktree: crate::projects_home::state_folder(repository)
                .join("worktrees")
                .join(&name),
            admin: repository.join(".git").join("worktrees").join(&name),
        }
    }

    /// Adds the temporary worktree, detached at a commit, and checks that git made it where
    /// it was named with the admin folder named for it. Input: the commit. Output: the
    /// canonical worktree, or None when git put it elsewhere.
    fn add_worktree(&mut self, commit: &str) -> Result<Option<PathBuf>, EngineError> {
        let path = self
            .worktree
            .to_str()
            .ok_or_else(|| failure("changes_git_unsupported"))?;
        self.owner
            .capture(&["worktree", "add", "-q", "--detach", path, commit], 4096)?;
        let link = std::fs::read_to_string(self.worktree.join(".git")).unwrap_or_default();
        let named = link
            .trim_end()
            .strip_prefix("gitdir: ")
            .and_then(|admin| Path::new(admin).canonicalize().ok());
        if named.as_deref() != Some(self.admin.as_path()) {
            return Ok(None);
        }
        let real = self
            .worktree
            .canonicalize()
            .map_err(|_| failure("changes_git_failed"))?;
        self.worktree = real.clone();
        Ok(Some(real))
    }

    /// Removes the worktree, its admin folder, the tool's temp folder and the engine folder,
    /// and the engine's `regenerate` folder when nothing else is in it.
    fn clear(&self) {
        if let Some(path) = self.worktree.to_str() {
            let _ = self.owner.exec(
                &["worktree", "remove", "--force", "--force", "--", path],
                None,
                &[],
                4096,
            );
        }
        let _ = std::fs::remove_dir_all(&self.worktree);
        let _ = std::fs::remove_dir_all(&self.admin);
        let _ = toolchains::remove_tmp(&self.home);
        let _ = std::fs::remove_dir_all(&self.folder);
        if let Some(parent) = self.folder.parent() {
            let _ = std::fs::remove_dir(parent);
        }
    }
}

impl Drop for Scratch<'_> {
    fn drop(&mut self) {
        self.clear();
    }
}

/// Removes what regenerations a stopped engine left (OS-8): in each repository the engine
/// knows, every `regenerate-<set>` worktree the project state folder or `.git/worktrees`
/// still holds, with its admin folder, then `git worktree prune`, and the engine's
/// `regenerate` scratch folders, leaving those of a regeneration running now. Input: the
/// store. Output: how many temporary worktrees it removed.
pub(super) fn sweep(db: &Connection) -> Result<usize, EngineError> {
    let Some(runtime) = runtime_of(db) else {
        return Ok(0);
    };
    let busy: BTreeSet<String> = running().keys().cloned().collect();
    let mut removed = 0;
    for repository in crate::worktrees::known_repositories(db)? {
        let Ok(root) = repository.canonicalize() else {
            continue;
        };
        let Ok(owner) = Repo::open(&root, &root.join(".git"), None) else {
            continue;
        };
        let mut left = BTreeSet::new();
        for folder in [
            crate::projects_home::state_folder(&root).join("worktrees"),
            root.join(".git").join("worktrees"),
        ] {
            left.extend(regeneration_ids(&folder, &format!("{SCRATCH_FOLDER}-")));
        }
        for id in left.difference(&busy) {
            drop(Scratch::named(&owner, &root, &runtime, id));
            removed += 1;
        }
        let _ = owner.exec(&["worktree", "prune"], None, &[], 4096);
    }
    for id in regeneration_ids(&runtime.join(SCRATCH_FOLDER), "").difference(&busy) {
        let _ = std::fs::remove_dir_all(runtime.join(SCRATCH_FOLDER).join(id));
    }
    let _ = std::fs::remove_dir(runtime.join(SCRATCH_FOLDER));
    Ok(removed)
}

/// The set ids named by the entries of a folder that start with `prefix` and go on with
/// 32 hex digits. Inputs: the folder and the prefix. Output: the ids, none for no folder.
fn regeneration_ids(folder: &Path, prefix: &str) -> BTreeSet<String> {
    std::fs::read_dir(folder)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            let id = name.strip_prefix(prefix)?;
            (id.len() == 32 && id.bytes().all(|byte| byte.is_ascii_hexdigit()))
                .then(|| id.to_owned())
        })
        .collect()
}

#[cfg(test)]
mod tests;
