use super::flow::{self, ENGINE, pending_ref, verify_objects};
use super::git::{Repo, failure, object_id};
use super::owner;
use super::store::{self, Record};
use super::{AgentKind, ChangeState, diff};
use crate::engine::EngineError;
use crate::sandbox::ProjectSandbox;
use crate::sandbox::toolchains::{self, ToolHomes, Toolchains};
use rusqlite::Connection;
use std::collections::BTreeMap;
use std::io::Read;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const LOCKFILES: [(&str, &str); 6] = [
    ("Cargo.lock", "cargo"),
    ("uv.lock", "uv"),
    ("package-lock.json", "npm"),
    ("pnpm-lock.yaml", "pnpm"),
    ("yarn.lock", "yarn"),
    ("poetry.lock", "poetry"),
];
const PATHS_MAX: usize = 16;
const LOCKFILE_BYTES_MAX: usize = 32 << 20;
const VERSION_BYTES_MAX: usize = 4096;
const LISTING_BYTES_MAX: usize = 1 << 20;
const VERSION_TIME: Duration = Duration::from_secs(30);
const TOOL_TIME: Duration = Duration::from_secs(300);
const POLL: Duration = Duration::from_millis(20);
const ENV: &str = "/usr/bin/env";
const SCRATCH_FOLDER: &str = "regenerate";

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

/// Rewrites the known lockfiles that alone clash between a change set and the tip it lands
/// on, as the engine's own commit on top of the set (OS-8): it merges the tip into the set's
/// head in the owner repository taking the tip's side of each lockfile, runs each lockfile's
/// tool on that merge in a temporary linked worktree confined like an agent, commits the
/// rewritten lockfiles there as the engine with the set's head and the tip as parents, and
/// tells the set's agent to reset to it. The temporary worktree is removed on every path and
/// the engine never runs git in the agent's own clone (CC-N1). Inputs: the store, the held
/// change set and its clashing lockfiles. Output: the new head, or None, with nothing
/// changed, when a tool is missing or fails or another path clashes too.
pub(super) fn regenerate(
    db: &Connection,
    record: &mut Record,
    paths: &[String],
) -> Result<Option<String>, EngineError> {
    assert!(
        !paths.is_empty() && paths.len() <= PATHS_MAX,
        "a regeneration names 1 to 16 lockfiles"
    );
    assert!(
        paths.iter().all(|path| program_for(path).is_some()),
        "a regeneration names known lockfiles only"
    );
    if !cfg!(target_os = "macos") {
        return Ok(None);
    }
    if !matches!(record.state, ChangeState::Pending | ChangeState::Conflict)
        || record.intent.is_some()
    {
        return Err(failure("changes_stale"));
    }
    let repository = record.repository.clone();
    if super::subagent::lead_of(db, &record.project, &record.agent, &repository)?.is_some() {
        return Ok(None);
    }
    let git_dir = repository.join(".git");
    let owner = Repo::open(&repository, &git_dir, None)?;
    let Some((_, tip)) = owner::land_on(db, &owner, &record.project)? else {
        return Ok(None);
    };
    verify_objects(&repository, &[&record.head, &tip], &[&record.base])?;
    let from = owner::measured_from(db, &owner, record, &tip)?;
    let changed = diff::changed_paths(&owner, &from, &record.head)?;
    if owner::uses_configured_commands(&owner, &changed, &[&from, &tip, &record.head])? {
        return Ok(None);
    }
    let (tree, conflicts) = owner::merge(&owner, &from, &tip, &record.head)?;
    if conflicts.is_empty() || conflicts.iter().any(|path| !paths.contains(path)) {
        return Ok(None);
    }
    let Some(runtime) = db
        .path()
        .filter(|path| !path.is_empty())
        .and_then(|path| Path::new(path).parent())
        .map(Path::to_path_buf)
    else {
        return Ok(None);
    };
    let root = repository
        .canonicalize()
        .map_err(|_| failure("changes_git_failed"))?;
    let mut scratch = Scratch::make(&owner, &root, &runtime, record.id.as_str())?;
    let index =
        Repo::open(&repository, &git_dir, None)?.private_index(&scratch.folder.join("index"));
    index.capture(&["read-tree", &tree], 256)?;
    for path in paths {
        let Some((mode, blob)) = tip_blob(&owner, &tip, path)? else {
            return Ok(None);
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
        return Ok(None);
    };
    let sandbox = match confined(db, record, &root, &runtime, &worktree, &scratch) {
        Ok(sandbox) => sandbox,
        Err(code) => {
            eprintln!("landing_regenerate_unconfined code={code}");
            return Ok(None);
        }
    };
    let profile = scratch.folder.join("profile.sb");
    let mut majors: BTreeMap<&str, Option<u32>> = BTreeMap::new();
    for path in paths {
        let program = program_for(path).ok_or_else(|| failure("changes_git_failed"))?;
        let folder = match path.rsplit_once('/') {
            Some((parent, _)) => worktree.join(parent),
            None => worktree.clone(),
        };
        if !plain_folder(&worktree, &folder) {
            return Ok(None);
        }
        let major = *majors.entry(program).or_insert_with(|| {
            run(
                &sandbox,
                &profile,
                &[program, "--version"],
                &folder,
                VERSION_TIME,
                true,
            )
            .filter(|(code, _)| *code == 0)
            .and_then(|(_, printed)| major_of(&printed))
        });
        let Some(command) = tool_for(path, major) else {
            return Ok(None);
        };
        if run(&sandbox, &profile, command, &folder, TOOL_TIME, false).map(|(code, _)| code)
            != Some(0)
        {
            return Ok(None);
        }
    }
    for path in paths {
        let Some(bytes) = read_plain(&worktree, path) else {
            return Ok(None);
        };
        let Some((mode, _)) = tip_blob(&owner, &tip, path)? else {
            return Ok(None);
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
    record.state = ChangeState::Pending;
    record.waiting = false;
    record.base = tip;
    record.head = commit.clone();
    record.conflicts = Vec::new();
    let stats = diff::stats(&owner, &record.base, &record.head)?;
    (record.files, record.added, record.removed) = (stats.files, stats.added, stats.removed);
    record.updated_ms = flow::now_ms()?;
    flow::set_engine_ref(&owner, &repository, &pending_ref(&record.id), Some(&commit))?;
    store::save(db, record)?;
    tell_regenerated(db, record, &files, &commit);
    Ok(Some(commit))
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
/// most `LOCKFILE_BYTES_MAX` bytes in plain folders. Inputs: the worktree and the path.
/// Output: the bytes, or None.
fn read_plain(worktree: &Path, path: &str) -> Option<Vec<u8>> {
    let file = worktree.join(path);
    if !plain_folder(worktree, file.parent()?) {
        return None;
    }
    let metadata = std::fs::symlink_metadata(&file).ok()?;
    if !metadata.is_file() || metadata.len() > LOCKFILE_BYTES_MAX as u64 {
        return None;
    }
    let mut bytes = Vec::new();
    std::fs::File::open(&file)
        .ok()?
        .take(LOCKFILE_BYTES_MAX as u64 + 1)
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
/// temporary worktree, within `time`, and stops its whole process group when it ends or
/// runs past the time. Inputs: the sandbox, its profile, the program and its arguments,
/// the folder, the time and whether to keep at most `VERSION_BYTES_MAX` bytes of its
/// output. Output: the exit code and the kept output, or None when it could not start, was
/// stopped by a signal or ran past the time.
fn run(
    sandbox: &ProjectSandbox,
    profile: &Path,
    args: &[&str],
    folder: &Path,
    time: Duration,
    keep: bool,
) -> Option<(i32, Vec<u8>)> {
    let mut command: Command = sandbox.command(profile, Path::new(ENV));
    command
        .args(args)
        .current_dir(folder)
        .stdin(Stdio::null())
        .stdout(if keep { Stdio::piped() } else { Stdio::null() })
        .stderr(Stdio::null());
    let mut child = crate::spawn_gate::spawn(&mut command).ok()?;
    let group = child.id();
    let reader = child.stdout.take().map(|stdout| {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let mut source = stdout.take(VERSION_BYTES_MAX as u64);
            let _ = source.read_to_end(&mut bytes);
            let _ = std::io::copy(&mut source.into_inner(), &mut std::io::sink());
            bytes
        })
    });
    let deadline = Instant::now() + time;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(POLL),
            _ => {
                stop_group(group);
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    stop_group(group);
    let printed = reader
        .map(|reader| reader.join().unwrap_or_default())
        .unwrap_or_default();
    assert!(printed.len() <= VERSION_BYTES_MAX);
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

/// Tells the set's sub-agent that the engine rewrote its lockfiles on top of its work and
/// to reset to that commit, in one Note from the engine keyed to the commit. A failed send
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
        body: format!(
            "The engine regenerated {files} on top of your work as {commit}. Run git reset --hard {commit} before you change anything else."
        ),
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
        assert!(id.len() == 32 && id.bytes().all(|byte| byte.is_ascii_hexdigit()));
        let name = format!("{SCRATCH_FOLDER}-{id}");
        let worktrees = crate::projects_home::state_folder(repository).join("worktrees");
        let scratch = Self {
            owner,
            folder: runtime.join(SCRATCH_FOLDER).join(id),
            home: runtime.join(SCRATCH_FOLDER).join(id).join("home"),
            tmp: runtime.join(SCRATCH_FOLDER).join(id).join("tmp"),
            worktree: worktrees.join(&name),
            admin: repository.join(".git").join("worktrees").join(&name),
        };
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

#[cfg(test)]
mod tests;
