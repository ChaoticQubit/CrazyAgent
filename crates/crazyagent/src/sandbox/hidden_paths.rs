use super::canonical;
use rusqlite::{Connection, params};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

/// The most folders one project may keep its agents out of.
pub const HIDDEN_PATHS_MAX: usize = 8;

/// The length of one hidden folder's digest as an agent's launch records it, in hex.
pub const DIGEST_HEX_LEN: usize = 32;

const PATH_TEXT_MAX: usize = 1024;

const TABLE: &str = "CREATE TABLE IF NOT EXISTS project_hidden_paths (
    project TEXT NOT NULL,
    position INTEGER NOT NULL,
    path TEXT NOT NULL,
    PRIMARY KEY (project, position)
)";

const SEERS: &str = "team_hidden_seers";

const SEERS_TABLE: &str = "CREATE TABLE IF NOT EXISTS team_hidden_seers (
    agent INTEGER PRIMARY KEY,
    project TEXT NOT NULL,
    restart INTEGER NOT NULL DEFAULT 0 CHECK(restart IN (0, 1))
)";

/// Names one hidden folder as an agent's launch records it: the first 16 bytes of the
/// SHA-256 of its canonical path in lowercase hex, so a pane record stays small.
/// Input: the canonical folder. Output: the digest of `DIGEST_HEX_LEN` characters.
pub fn digest(folder: &Path) -> String {
    let bytes = Sha256::digest(folder.as_os_str().as_encoded_bytes());
    let text: String = bytes[..DIGEST_HEX_LEN / 2]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    assert_eq!(text.len(), DIGEST_HEX_LEN);
    text
}

/// Tells whether a recorded digest has the shape [`digest`] writes: `DIGEST_HEX_LEN`
/// lowercase hex characters. Input: the text. Output: the answer.
pub fn digest_plain(text: &str) -> bool {
    text.len() == DIGEST_HEX_LEN
        && text
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

/// Tells whether an agent's sandbox keeps it out of every folder saved hidden now: each
/// saved folder, or a folder above it, was hidden when the agent started. A launch that
/// recorded no hidden folders, as a pane from before the engine kept them, hides none.
/// Inputs: the digests its launch recorded and the saved folders. Output: the answer.
pub fn covers(built: Option<&[String]>, saved: &[PathBuf]) -> bool {
    assert!(saved.len() <= HIDDEN_PATHS_MAX);
    assert!(built.is_none_or(|built| built.len() <= HIDDEN_PATHS_MAX));
    saved.iter().all(|folder| {
        built.is_some_and(|built| {
            folder
                .ancestors()
                .any(|above| built.contains(&digest(above)))
        })
    })
}

/// Replaces the running agents of one project that can still open a folder hidden after
/// they started, writing only when the set changed; an agent kept on the list keeps the
/// owner's restart ask. Inputs: the engine connection, the project id and the agents.
/// Output: done, or the database error.
pub fn set_seers(
    connection: &Connection,
    project: &str,
    agents: &BTreeSet<i64>,
) -> rusqlite::Result<()> {
    let listed = seers(connection, project)?;
    if &listed == agents {
        return Ok(());
    }
    let tx = connection.unchecked_transaction()?;
    tx.execute(SEERS_TABLE, [])?;
    for gone in listed.difference(agents) {
        tx.execute(
            "DELETE FROM team_hidden_seers WHERE agent=?1 AND project=?2",
            params![gone, project],
        )?;
    }
    for agent in agents.difference(&listed) {
        tx.execute(
            "INSERT OR REPLACE INTO team_hidden_seers(agent, project, restart) VALUES (?1, ?2, 0)",
            params![agent, project],
        )?;
    }
    tx.commit()
}

/// Drops one project's list of agents that can still open a hidden folder, as removing
/// the project does. Inputs: the open transaction and the project id. Output: done, or the
/// database error.
pub fn forget_seers(connection: &Connection, project: &str) -> rusqlite::Result<()> {
    if present(connection)? {
        connection.execute("DELETE FROM team_hidden_seers WHERE project=?1", [project])?;
    }
    Ok(())
}

/// Tells whether the engine ever kept a list of agents that can still open a hidden
/// folder. Input: the connection. Output: the answer, or the database error.
fn present(connection: &Connection) -> rusqlite::Result<bool> {
    connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name=?1)",
        [SEERS],
        |row| row.get(0),
    )
}

/// Reads one project's listed agents, only those the owner asked to restart when `asked`
/// holds. Inputs: the connection, the project id and the choice. Output: their ids, none
/// without a list, or the database error.
fn listed(connection: &Connection, project: &str, asked: bool) -> rusqlite::Result<BTreeSet<i64>> {
    if !present(connection)? {
        return Ok(BTreeSet::new());
    }
    let mut statement = connection.prepare(if asked {
        "SELECT agent FROM team_hidden_seers WHERE project=?1 AND restart=1"
    } else {
        "SELECT agent FROM team_hidden_seers WHERE project=?1"
    })?;
    statement.query_map([project], |row| row.get(0))?.collect()
}

/// Reads the agents of one project listed as able to open a folder hidden after they
/// started; a database without the list has none. Inputs: the connection and the project
/// id. Output: their ids.
pub fn seers(connection: &Connection, project: &str) -> rusqlite::Result<BTreeSet<i64>> {
    listed(connection, project, false)
}

/// Asks every listed agent of one project to restart, as the owner's Restart them does.
/// Inputs: the open transaction and the project id. Output: how many listed agents were
/// asked, none for a project with none listed, or the database error.
pub fn ask_restart(connection: &Connection, project: &str) -> rusqlite::Result<usize> {
    if !present(connection)? {
        return Ok(0);
    }
    connection.execute(
        "UPDATE team_hidden_seers SET restart=1 WHERE project=?1",
        [project],
    )
}

/// Reads the listed agents of one project the owner asked to restart. Inputs: the
/// connection and the project id. Output: their ids.
pub fn restarts(connection: &Connection, project: &str) -> rusqlite::Result<BTreeSet<i64>> {
    listed(connection, project, true)
}

/// The most engine runs the hidden folders run list holds at once; a run that would pass it
/// is refused as if its hidden folders could not be read, so the list never misses one.
const RUNS_MAX: usize = 256;

/// One kind of engine run, not a team agent, that hides a project's saved folders from its
/// own start (SA-VID14-R2-N2): headless master turns (SA-VID14-MASTER-TURNS), research
/// jobs, the overview, roadmap and onboarding agents, code graph builds and lockfile
/// rebuilds. An instructions cleanup run reads only an empty
/// engine folder, so it never sees a project folder and is not one.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum RunKind {
    Master,
    Research,
    Overview,
    Roadmap,
    Onboarding,
    CodeGraph,
    Lockfiles,
}

/// One running engine run as the list keeps it: its project and the folders it hid as
/// [`digest`]s, none until it has read them, and its kind. No path or project text is kept.
struct Running {
    project: String,
    hidden: Option<Vec<String>>,
    kind: RunKind,
}

/// The engine runs now running that hid folders from their start, by a key their
/// [`HiddenRun`] holds, at most [`RUNS_MAX`].
struct Registry {
    runs: BTreeMap<u64, Running>,
    next: u64,
}

impl Registry {
    const fn new() -> Self {
        Self {
            runs: BTreeMap::new(),
            next: 0,
        }
    }

    /// Lists one run under a key never handed out before. Input: the run. Output: its key,
    /// or none when the list is full or the keys ran out.
    fn add(&mut self, running: Running) -> Option<u64> {
        assert!(digest_plain(&running.project));
        assert!(
            running
                .hidden
                .as_ref()
                .is_none_or(|hidden| hidden.len() <= HIDDEN_PATHS_MAX)
        );
        if self.runs.len() >= RUNS_MAX {
            return None;
        }
        let key = self.next;
        self.next = key.checked_add(1)?;
        let fresh = self.runs.insert(key, running).is_none();
        assert!(fresh);
        assert!(self.runs.len() <= RUNS_MAX);
        Some(key)
    }

    /// Takes one run off the list. Input: the key [`Registry::add`] gave it.
    fn remove(&mut self, key: u64) {
        let removed = self.runs.remove(&key);
        assert!(removed.is_some());
    }

    /// Names the kinds of one project's runs whose start hid neither a saved folder nor a
    /// folder above it, each once. Inputs: the project digest and the saved folders.
    /// Output: the kinds in their order.
    fn seeing(&self, project: &str, saved: &[PathBuf]) -> Vec<RunKind> {
        assert!(digest_plain(project));
        let kinds: BTreeSet<RunKind> = self
            .runs
            .values()
            .filter(|run| run.project == project && !covers(run.hidden.as_deref(), saved))
            .map(|run| run.kind)
            .collect();
        kinds.into_iter().collect()
    }
}

static RUNS: Mutex<Registry> = Mutex::new(Registry::new());

/// Held by one engine run for its whole life, from before it reads its hidden folders until
/// it ends; dropping it, on a return, an error or a panic, takes the run off the list.
#[must_use = "the run is listed only while this is held"]
#[derive(Debug)]
pub struct HiddenRun(u64);

impl HiddenRun {
    /// Records, once, the folders this run's sandbox hides after it has read them, so from
    /// then on only a folder saved hidden later names it (SA-VID14 r1). Input: the folders,
    /// at most [`HIDDEN_PATHS_MAX`], none for a run that is not confined.
    pub fn hid(&self, hidden: &[PathBuf]) {
        assert!(hidden.len() <= HIDDEN_PATHS_MAX);
        let digests = hidden.iter().map(|folder| digest(folder)).collect();
        let mut runs = RUNS.lock().unwrap_or_else(PoisonError::into_inner);
        let run = runs.runs.get_mut(&self.0);
        assert!(run.as_ref().is_some_and(|run| run.hidden.is_none()));
        if let Some(run) = run {
            run.hidden = Some(digests);
        }
    }
}

impl Drop for HiddenRun {
    fn drop(&mut self) {
        RUNS.lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(self.0);
    }
}

/// Puts one engine run on the list as it starts, before it reads the project's saved hidden
/// folders, keeping only a digest of its project and its kind, so a hide saved meanwhile
/// never misses it: until [`HiddenRun::hid`] records what it hid, it hides nothing. Inputs:
/// the kind and the project id. Output: the guard to hold until it ends, or none when full.
pub fn starting(kind: RunKind, project: &str) -> Option<HiddenRun> {
    let entry = Running {
        project: digest(Path::new(project)),
        hidden: None,
        kind,
    };
    let key = RUNS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .add(entry)?;
    Some(HiddenRun(key))
}

/// Names the kinds of one project's running engine runs that can still open a folder saved
/// hidden now, each once: those whose start hid neither it nor a folder above it. Inputs:
/// the project id and the saved folders. Output: the kinds in their order.
pub fn runs_seeing(project: &str, saved: &[PathBuf]) -> Vec<RunKind> {
    RUNS.lock()
        .unwrap_or_else(PoisonError::into_inner)
        .seeing(&digest(Path::new(project)), saved)
}

/// Checks one folder the owner wants this project's agents kept out of: an absolute existing
/// folder, taken in its canonical form, that sits strictly inside the project root. A folder
/// outside the project, the project root itself, or a path that is not a folder is refused.
/// Inputs: the typed path and the project root. Output: the canonical folder, or a plain
/// refusal code.
pub fn checked(text: &str, project_root: &Path) -> Result<PathBuf, &'static str> {
    let typed = Path::new(text);
    if text.len() > PATH_TEXT_MAX || !typed.is_absolute() {
        return Err("hidden_path_not_absolute");
    }
    if text
        .chars()
        .any(|c| c.is_control() || c == '"' || c == '\\' || c == '\'')
    {
        return Err("hidden_path_not_plain");
    }
    let real = canonical(typed).map_err(|_| "hidden_path_missing")?;
    if !real.is_dir() {
        return Err("hidden_path_not_folder");
    }
    let root = canonical(project_root).unwrap_or_else(|_| project_root.to_path_buf());
    if !real.starts_with(&root) || real == root {
        return Err("hidden_path_outside_project");
    }
    Ok(real)
}

/// Checks a project's whole list of hidden folders as [`checked`] does, at most
/// [`HIDDEN_PATHS_MAX`], dropping repeats. Inputs: the typed paths and the project root.
/// Output: the canonical folders in order, or the first refusal code.
pub fn checked_all(texts: &[String], project_root: &Path) -> Result<Vec<PathBuf>, &'static str> {
    if texts.len() > HIDDEN_PATHS_MAX {
        return Err("hidden_paths_too_many");
    }
    let mut paths: Vec<PathBuf> = Vec::with_capacity(texts.len());
    for text in texts {
        let path = checked(text, project_root)?;
        if !paths.contains(&path) {
            paths.push(path);
        }
    }
    assert!(paths.len() <= HIDDEN_PATHS_MAX);
    Ok(paths)
}

/// Replaces a project's hidden folders. Inputs: the open connection or transaction, the
/// project id and the checked folders. Output: done, or the database error.
pub fn save(connection: &Connection, project: &str, paths: &[PathBuf]) -> rusqlite::Result<()> {
    assert!(paths.len() <= HIDDEN_PATHS_MAX);
    connection.execute(TABLE, [])?;
    connection.execute(
        "DELETE FROM project_hidden_paths WHERE project=?1",
        [project],
    )?;
    for (position, path) in paths.iter().enumerate() {
        let text = path
            .to_str()
            .ok_or_else(|| rusqlite::Error::InvalidPath(path.clone()))?;
        connection.execute(
            "INSERT INTO project_hidden_paths (project, position, path) VALUES (?1, ?2, ?3)",
            params![project, position as i64, text],
        )?;
    }
    Ok(())
}

/// Reads a project's hidden folders in the order the owner saved them; none when nothing was
/// ever saved. Inputs: the open connection or transaction and the project id. Output: the
/// folders, or the database error.
pub fn load(connection: &Connection, project: &str) -> rusqlite::Result<Vec<PathBuf>> {
    let present: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name='project_hidden_paths')",
        [],
        |row| row.get(0),
    )?;
    if !present {
        return Ok(Vec::new());
    }
    let mut statement = connection.prepare(
        "SELECT path FROM project_hidden_paths WHERE project=?1 ORDER BY position LIMIT ?2",
    )?;
    let paths = statement
        .query_map(params![project, HIDDEN_PATHS_MAX as i64], |row| {
            row.get::<_, String>(0).map(PathBuf::from)
        })?
        .collect::<rusqlite::Result<Vec<PathBuf>>>()?;
    assert!(paths.len() <= HIDDEN_PATHS_MAX);
    Ok(paths)
}

/// The longest the tracked-files check lets git run before it is killed and the folder is
/// refused as unchecked.
#[cfg(unix)]
const CHECK_TIME: std::time::Duration = std::time::Duration::from_secs(5);
#[cfg(unix)]
const CHECK_POLL: std::time::Duration = std::time::Duration::from_millis(2);
/// The most of git's answer the check reads; a tracked folder shows in the first bytes.
#[cfg(any(unix, test))]
const CHECK_BYTES_MAX: u64 = 64 * 1024;

/// Whether git tracks any file inside this folder, asked in the project's own checkout with
/// the real git under no outside config, hooks, fsmonitor or lazy fetch, so a hostile
/// imported repository cannot run a command as the owner. A hidden folder covers only files
/// git does not track; a tracked file is copied into each agent's worktree and lives in
/// history, so hiding one would be a false promise. Run outside any sandbox with a deadline
/// that kills git. Inputs: the project root and the canonical folder. Output: `Ok(true)` when
/// git names any path (whatever its exit status, so a listing past the read cap still counts
/// as tracked), `Ok(false)` when the folder is outside the root or the root has no `.git` at
/// all or git lists nothing and exits clean, and `Err("hidden_path_unchecked")` when `.git`
/// exists but is not a real folder (a linked worktree or submodule gitlink), or on any git
/// error, non-zero exit with no output, timeout or unreadable answer, so the caller fails
/// closed.
#[cfg(unix)]
pub fn git_tracks(project_root: &Path, folder: &Path) -> Result<bool, &'static str> {
    use std::io::Read;
    use std::process::{Command, Stdio};
    let unchecked = "hidden_path_unchecked";
    let root = canonical(project_root).map_err(|_| unchecked)?;
    let real = canonical(folder).map_err(|_| unchecked)?;
    let Ok(relative) = real.strip_prefix(&root) else {
        return Ok(false);
    };
    let git_dir = root.join(".git");
    match std::fs::symlink_metadata(&git_dir) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(_) => return Err(unchecked),
        Ok(meta) if !meta.is_dir() => return Err(unchecked),
        Ok(_) => {}
    }
    let mut command = Command::new(crate::research::runner::setup_git());
    command
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("LC_ALL", "C")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .envs(crate::research::runner::GIT_SAFE_ENV)
        .env("GIT_LITERAL_PATHSPECS", "1")
        .arg("--no-optional-locks")
        .args(crate::research::runner::GIT_SAFE)
        .arg("--git-dir")
        .arg(&git_dir)
        .arg("--work-tree")
        .arg(&root)
        .current_dir(&root)
        .args(["ls-files", "-z", "--"])
        .arg(relative)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = crate::spawn_gate::spawn(&mut command).map_err(|_| unchecked)?;
    let mut stdout = child.stdout.take().ok_or(unchecked)?;
    let reader = std::thread::spawn(move || {
        let mut buffer = Vec::new();
        let _ = (&mut stdout).take(CHECK_BYTES_MAX).read_to_end(&mut buffer);
        buffer
    });
    let deadline = std::time::Instant::now() + CHECK_TIME;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(unchecked);
            }
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(unchecked);
        }
        std::thread::sleep(CHECK_POLL);
    };
    let bytes = reader.join().map_err(|_| unchecked)?;
    if !bytes.is_empty() {
        Ok(true)
    } else if status.success() {
        Ok(false)
    } else {
        Err(unchecked)
    }
}

/// Refuses the tracked-files check where no confined git runs. Inputs unused. Output:
/// `Err("hidden_path_unchecked")`, so the caller fails closed.
#[cfg(not(unix))]
pub fn git_tracks(_project_root: &Path, _folder: &Path) -> Result<bool, &'static str> {
    Err("hidden_path_unchecked")
}

/// Refuses a list that holds a folder git tracks, and refuses the whole list when the check
/// cannot answer for any folder. Inputs: the project root and the checked folders. Output:
/// ok, `hidden_path_tracked` for the first tracked folder, or `hidden_path_unchecked`.
pub fn none_tracked(project_root: &Path, folders: &[PathBuf]) -> Result<(), &'static str> {
    for folder in folders {
        if git_tracks(project_root, folder)? {
            return Err("hidden_path_tracked");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Lists one run as a seam does: before its load, then what it hid. Inputs: the kind,
    /// the project id and the folders. Output: the guard.
    fn listed_run(kind: RunKind, project: &str, hidden: &[PathBuf]) -> HiddenRun {
        let run = starting(kind, project).unwrap();
        run.hid(hidden);
        run
    }
    use rusqlite::Connection;

    fn project() -> tempfile::TempDir {
        tempfile::Builder::new()
            .prefix("ca-hidden-")
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap()
    }

    #[test]
    fn a_folder_inside_the_project_is_accepted_and_one_outside_is_refused() {
        let root = project();
        let inside = root.path().join("notes");
        std::fs::create_dir(&inside).unwrap();
        let got = checked(inside.to_str().unwrap(), root.path()).unwrap();
        assert_eq!(got, std::fs::canonicalize(&inside).unwrap());

        let outside = project();
        assert_eq!(
            checked(outside.path().to_str().unwrap(), root.path()),
            Err("hidden_path_outside_project")
        );
        assert_eq!(
            checked(root.path().to_str().unwrap(), root.path()),
            Err("hidden_path_outside_project")
        );
        assert_eq!(
            checked("relative/notes", root.path()),
            Err("hidden_path_not_absolute")
        );
    }

    #[test]
    fn at_most_eight_folders_are_checked() {
        let root = project();
        let mut texts = Vec::new();
        for n in 0..9 {
            let folder = root.path().join(format!("f{n}"));
            std::fs::create_dir(&folder).unwrap();
            texts.push(folder.to_str().unwrap().to_owned());
        }
        assert_eq!(
            checked_all(&texts, root.path()),
            Err("hidden_paths_too_many")
        );
        assert_eq!(
            checked_all(&texts[..HIDDEN_PATHS_MAX], root.path())
                .unwrap()
                .len(),
            HIDDEN_PATHS_MAX
        );
    }

    #[test]
    fn a_folder_whose_name_holds_a_quote_or_backslash_is_refused() {
        let root = project();
        for bad in ["/tmp/a\"b", "/tmp/a\\b", "/tmp/a'b", "/tmp/a\nb"] {
            assert_eq!(checked(bad, root.path()), Err("hidden_path_not_plain"));
        }
    }

    #[test]
    fn a_folder_git_tracks_is_refused_and_an_untracked_one_is_accepted() {
        let root = project();
        let path = root.path().canonicalize().unwrap();
        let git = |args: &[&str]| {
            let mut command = std::process::Command::new("/usr/bin/git");
            command
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .current_dir(&path)
                .args(args)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null());
            let status = crate::spawn_gate::status(&mut command).unwrap();
            assert!(status.success(), "git {args:?}");
        };
        git(&["init", "-q"]);
        let tracked = path.join("secret");
        let untracked = path.join("scratch");
        std::fs::create_dir(&tracked).unwrap();
        std::fs::create_dir(&untracked).unwrap();
        std::fs::write(tracked.join("key.txt"), "k\n").unwrap();
        std::fs::write(untracked.join("tmp.txt"), "t\n").unwrap();
        git(&["add", "secret/key.txt"]);
        assert_eq!(git_tracks(&path, &tracked), Ok(true));
        assert_eq!(git_tracks(&path, &untracked), Ok(false));
        assert_eq!(
            none_tracked(&path, &[tracked.clone(), untracked.clone()]),
            Err("hidden_path_tracked")
        );
        assert_eq!(none_tracked(&path, &[untracked]), Ok(()));
    }

    #[test]
    fn a_folder_with_no_repository_is_not_tracked() {
        let root = project();
        let path = root.path().canonicalize().unwrap();
        let folder = path.join("plain");
        std::fs::create_dir(&folder).unwrap();
        assert_eq!(git_tracks(&path, &folder), Ok(false));
        assert_eq!(none_tracked(&path, &[folder]), Ok(()));
    }

    #[test]
    fn a_hostile_fsmonitor_command_never_runs_during_the_check() {
        let root = project();
        let path = root.path().canonicalize().unwrap();
        let git = |args: &[&str]| {
            let mut command = std::process::Command::new("/usr/bin/git");
            command
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .current_dir(&path)
                .args(args)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null());
            assert!(crate::spawn_gate::status(&mut command).unwrap().success());
        };
        git(&["init", "-q"]);
        let marker = path.join("pwned");
        let folder = path.join("secret");
        std::fs::create_dir(&folder).unwrap();
        std::fs::write(folder.join("key.txt"), "k\n").unwrap();
        git(&["add", "secret/key.txt"]);
        let hostile = format!("touch {}", marker.display());
        std::fs::write(
            path.join(".git").join("config"),
            format!(
                "[core]\n\trepositoryformatversion = 0\n\tfsmonitor = {hostile}\n\tfsmonitorHookVersion = 1\n"
            ),
        )
        .unwrap();
        let _ = git_tracks(&path, &folder);
        assert!(!marker.exists(), "fsmonitor command ran: marker written");
    }

    #[test]
    fn a_corrupt_index_fails_the_check_closed() {
        let root = project();
        let path = root.path().canonicalize().unwrap();
        let git = |args: &[&str]| {
            let mut command = std::process::Command::new("/usr/bin/git");
            command
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .current_dir(&path)
                .args(args)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null());
            assert!(crate::spawn_gate::status(&mut command).unwrap().success());
        };
        git(&["init", "-q"]);
        let folder = path.join("secret");
        std::fs::create_dir(&folder).unwrap();
        std::fs::write(folder.join("key.txt"), "k\n").unwrap();
        git(&["add", "secret/key.txt"]);
        std::fs::write(path.join(".git").join("index"), b"not a git index\n").unwrap();
        assert_eq!(git_tracks(&path, &folder), Err("hidden_path_unchecked"));
        assert_eq!(none_tracked(&path, &[folder]), Err("hidden_path_unchecked"));
    }

    #[test]
    fn a_dot_git_file_fails_the_check_closed() {
        let root = project();
        let path = root.path().canonicalize().unwrap();
        let folder = path.join("secret");
        std::fs::create_dir(&folder).unwrap();
        std::fs::write(folder.join("key.txt"), "k\n").unwrap();
        std::fs::write(path.join(".git"), b"gitdir: /elsewhere/.git/worktrees/x\n").unwrap();
        assert_eq!(git_tracks(&path, &folder), Err("hidden_path_unchecked"));
        assert_eq!(none_tracked(&path, &[folder]), Err("hidden_path_unchecked"));
    }

    #[test]
    fn a_folder_with_more_than_64_kib_of_tracked_names_is_still_tracked() {
        let root = project();
        let path = root.path().canonicalize().unwrap();
        let git = |args: &[&str]| {
            let mut command = std::process::Command::new("/usr/bin/git");
            command
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .current_dir(&path)
                .args(args)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null());
            assert!(crate::spawn_gate::status(&mut command).unwrap().success());
        };
        git(&["init", "-q"]);
        let folder = path.join("bulk");
        std::fs::create_dir(&folder).unwrap();
        let mut total = 0usize;
        let mut index = 0usize;
        while total <= CHECK_BYTES_MAX as usize {
            let name = format!("{index:06}-{}.txt", "x".repeat(200));
            std::fs::write(folder.join(&name), "t\n").unwrap();
            total += "bulk/".len() + name.len() + 1;
            index += 1;
        }
        git(&["add", "bulk"]);
        assert_eq!(git_tracks(&path, &folder), Ok(true));
        assert_eq!(none_tracked(&path, &[folder]), Err("hidden_path_tracked"));
    }

    #[test]
    fn saved_folders_read_back_in_order_and_are_empty_without_a_table() {
        let connection = Connection::open_in_memory().unwrap();
        assert!(load(&connection, "app").unwrap().is_empty());
        let paths = vec![PathBuf::from("/tmp/a"), PathBuf::from("/tmp/b")];
        save(&connection, "app", &paths).unwrap();
        assert_eq!(load(&connection, "app").unwrap(), paths);
        save(&connection, "app", &[]).unwrap();
        assert!(load(&connection, "app").unwrap().is_empty());
    }

    #[test]
    fn an_agent_sees_a_hidden_folder_unless_it_or_a_folder_above_was_hidden_at_its_start() {
        let secrets = PathBuf::from("/tmp/app/secrets");
        let inner = secrets.join("keys");
        let vendor = PathBuf::from("/tmp/app/vendor");
        let built = vec![digest(&secrets)];
        assert!(digest_plain(&built[0]), "{}", built[0]);
        assert_ne!(digest(&secrets), digest(&vendor));
        assert!(covers(Some(&built), &[secrets.clone()]));
        assert!(
            covers(Some(&built), &[inner.clone()]),
            "a folder above it was hidden"
        );
        assert!(!covers(Some(&built), &[secrets.clone(), vendor.clone()]));
        assert!(
            !covers(Some(&[]), &[secrets.clone()]),
            "started before the hide"
        );
        assert!(
            !covers(None, &[inner]),
            "a launch that kept no list hides none"
        );
        assert!(covers(None, &[]), "nothing hidden, nothing to see");
        assert!(
            !digest_plain("ABCDEF"),
            "upper case or short is not a digest"
        );
    }

    #[test]
    fn listed_agents_keep_their_restart_ask_until_they_leave_the_list() {
        let connection = Connection::open_in_memory().unwrap();
        assert!(seers(&connection, "app").unwrap().is_empty());
        assert_eq!(ask_restart(&connection, "app").unwrap(), 0, "none listed");
        let both = BTreeSet::from([3, 4]);
        set_seers(&connection, "app", &both).unwrap();
        set_seers(&connection, "web", &BTreeSet::from([9])).unwrap();
        assert_eq!(seers(&connection, "app").unwrap(), both);
        assert!(restarts(&connection, "app").unwrap().is_empty());
        assert_eq!(ask_restart(&connection, "app").unwrap(), 2);
        assert_eq!(restarts(&connection, "app").unwrap(), both);
        assert!(
            restarts(&connection, "web").unwrap().is_empty(),
            "one project only"
        );
        set_seers(&connection, "app", &BTreeSet::from([4, 5])).unwrap();
        assert_eq!(seers(&connection, "app").unwrap(), BTreeSet::from([4, 5]));
        assert_eq!(restarts(&connection, "app").unwrap(), BTreeSet::from([4]));
        set_seers(&connection, "app", &BTreeSet::new()).unwrap();
        assert!(seers(&connection, "app").unwrap().is_empty());
        assert_eq!(seers(&connection, "web").unwrap(), BTreeSet::from([9]));
    }

    /// SA-VID14-R2-N2: an engine run that started before a folder was hidden is listed by
    /// its kind until it ends; one whose start hid the folder, or a folder above it, is
    /// not, and another project's list never shows it.
    #[test]
    fn a_run_started_before_a_folder_was_hidden_is_listed_by_kind_until_it_ends() {
        let project = "n2-listed";
        let (above, inside, other) = (
            PathBuf::from("/tmp/n2-listed/vault"),
            PathBuf::from("/tmp/n2-listed/vault/keys"),
            PathBuf::from("/tmp/n2-listed/dump"),
        );
        let research = listed_run(RunKind::Research, project, std::slice::from_ref(&above));
        assert!(runs_seeing(project, std::slice::from_ref(&inside)).is_empty());
        assert_eq!(
            runs_seeing(project, &[inside.clone(), other.clone()]),
            vec![RunKind::Research]
        );
        assert!(runs_seeing("n2-listed-elsewhere", std::slice::from_ref(&other)).is_empty());
        let overview = listed_run(RunKind::Overview, project, &[above.clone(), other.clone()]);
        let twice = listed_run(RunKind::Research, project, &[]);
        assert_eq!(
            runs_seeing(project, &[inside.clone(), other.clone()]),
            vec![RunKind::Research],
            "a kind is named once, and a run that hid both is not named"
        );
        drop((research, twice));
        assert!(runs_seeing(project, &[inside, other]).is_empty());
        drop(overview);
    }

    /// SA-VID14-R2-N2 (leader 08:04): a run leaves the list on every exit, a normal
    /// return, an early error return and a panic.
    #[test]
    fn a_run_leaves_the_list_on_every_exit_path() {
        let project = "n2-exits";
        let saved = [PathBuf::from("/tmp/n2-exits/secret")];
        let held = listed_run(RunKind::Roadmap, project, &[]);
        assert_eq!(runs_seeing(project, &saved), vec![RunKind::Roadmap]);
        drop(held);
        assert!(runs_seeing(project, &saved).is_empty(), "a return");

        let refused = || -> Result<(), &'static str> {
            let _run = starting(RunKind::Lockfiles, project).ok_or("full")?;
            _run.hid(&[]);
            assert_eq!(runs_seeing(project, &saved), vec![RunKind::Lockfiles]);
            Err("sandbox_refused")?;
            Ok(())
        };
        assert_eq!(refused(), Err("sandbox_refused"));
        assert!(
            runs_seeing(project, &saved).is_empty(),
            "an early error return"
        );

        let panicked = std::panic::catch_unwind(|| {
            let _run = listed_run(RunKind::Onboarding, project, &[]);
            assert_eq!(runs_seeing(project, &saved), vec![RunKind::Onboarding]);
            panic!("the run ended in a panic");
        });
        let payload = panicked.unwrap_err();
        assert_eq!(
            payload.downcast_ref::<&str>(),
            Some(&"the run ended in a panic"),
            "the run panicked after it was listed, not in an assert"
        );
        assert!(runs_seeing(project, &saved).is_empty(), "a panic");
    }

    /// SA-VID14-R2-N2 (leader 08:04): the list keeps digests and the run kind only, never a
    /// folder path or the project id.
    #[test]
    fn the_run_list_holds_digests_and_kinds_only() {
        let project = "n2-plain-project";
        let secret = PathBuf::from("/tmp/n2-plain/secret-plain");
        let _run = listed_run(RunKind::CodeGraph, project, std::slice::from_ref(&secret));
        let runs = RUNS.lock().unwrap_or_else(PoisonError::into_inner);
        let ours: Vec<&Running> = runs
            .runs
            .values()
            .filter(|run| run.project == digest(Path::new(project)))
            .collect();
        assert_eq!(ours.len(), 1, "the run is kept while it runs");
        let run = ours[0];
        assert_eq!(run.kind, RunKind::CodeGraph);
        assert!(digest_plain(&run.project));
        assert_eq!(run.hidden, Some(vec![digest(&secret)]));
        for kept in std::iter::once(&run.project).chain(run.hidden.iter().flatten()) {
            assert!(digest_plain(kept), "{kept}");
            assert!(
                !kept.contains("n2-plain") && !kept.contains("secret"),
                "{kept}"
            );
        }
    }

    /// SA-VID14-R2-N2: the list refuses a run past its bound, so callers fail closed, and
    /// takes one again once a run ends; keys are never reused.
    #[test]
    fn the_run_list_refuses_past_its_bound_and_takes_one_again_once_one_ends() {
        let mut registry = Registry::new();
        let entry = || Running {
            project: digest(Path::new("n2-bound")),
            hidden: None,
            kind: RunKind::Research,
        };
        let keys: Vec<u64> = (0..RUNS_MAX)
            .map(|_| registry.add(entry()).unwrap())
            .collect();
        assert_eq!(
            keys.iter().collect::<BTreeSet<_>>().len(),
            RUNS_MAX,
            "every key is new"
        );
        assert_eq!(registry.add(entry()), None, "a full list refuses");
        assert_eq!(registry.runs.len(), RUNS_MAX);
        registry.remove(keys[0]);
        let again = registry.add(entry()).unwrap();
        assert!(!keys.contains(&again), "a key is never reused");
        assert_eq!(registry.runs.len(), RUNS_MAX);
    }

    /// SA-VID14-R2-N2: every source file that loads a project's saved hidden folders was
    /// checked against the run list. Those that start an engine run hold its guard for each
    /// kind; the rest are team agents (SA-VID14-A), the master turn bundle whose guard sits in
    /// master_runtime.rs, or reads that feed no sandbox. A new loader fails until classified.
    #[test]
    fn every_loader_of_saved_hidden_folders_holds_the_guard_or_is_classified() {
        let guarded: BTreeMap<&str, &[&str]> = BTreeMap::from([
            (
                "landing/lockfiles.rs",
                &["RunKind::Lockfiles", "listed.hid(&hidden)"][..],
            ),
            (
                "onboarding/service.rs",
                &[
                    "RunKind::CodeGraph",
                    "listed.hid(&hidden)",
                    "std::mem::forget(listed)",
                ][..],
            ),
            (
                "research/runner.rs",
                &["RunKind::Research", "run.kind", "listed.hid(&hidden)"][..],
            ),
            (
                "engine/owner/transport/master_runtime.rs",
                &[
                    "RunKind::Master",
                    "listed.hid(master_turn_hides(&prepared))",
                    "keep_listed_if_uncertain(listed, &",
                ][..],
            ),
        ]);
        let classified = BTreeSet::from([
            "demo.rs",
            "project_overview/agent.rs",
            "project_overview/run.rs",
            "research/jobs.rs",
            "roadmap_status/run.rs",
            "storage.rs",
            "storage/master_session.rs",
            "team/runtime.rs",
        ]);
        let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut folders = vec![src.clone()];
        let mut loaders = BTreeSet::new();
        while let Some(folder) = folders.pop() {
            for entry in std::fs::read_dir(&folder).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    if path.file_name() != Some("tests".as_ref()) {
                        folders.push(path);
                    }
                    continue;
                }
                let name = path.file_name().unwrap().to_str().unwrap();
                if path.extension() != Some("rs".as_ref())
                    || name == "tests.rs"
                    || name.ends_with("_tests.rs")
                {
                    continue;
                }
                let relative = path
                    .strip_prefix(&src)
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .to_owned();
                if relative != "sandbox/hidden_paths.rs"
                    && std::fs::read_to_string(&path)
                        .unwrap()
                        .contains("hidden_paths::load(")
                {
                    loaders.insert(relative);
                }
            }
        }
        assert!(
            loaders.len() >= classified.len(),
            "the walk read the sources"
        );
        for loader in &loaders {
            assert!(
                guarded.contains_key(loader.as_str()) || classified.contains(loader.as_str()),
                "{loader} loads saved hidden folders: hold the run list guard or classify it"
            );
        }
        for (file, kinds) in &guarded {
            let source = std::fs::read_to_string(src.join(file)).unwrap();
            assert!(source.contains("hidden_paths::starting("), "{file}");
            for kind in *kinds {
                assert!(source.contains(kind), "{file} lists {kind}");
            }
            assert!(
                source.matches("std::mem::forget(listed)").count()
                    >= source.matches("std::mem::forget(lease)").count(),
                "{file}: every exit that keeps a lease because its processes may still run keeps its run named"
            );
        }
    }

    /// SA-VID14 r1 (rv-code4 F1, rv-sec2 F2): a run is listed before it reads the saved
    /// hidden folders, so a hide saved meanwhile names it. Until it records what it hid it
    /// counts as hiding nothing; after, only folders it did not hide name it.
    #[test]
    fn a_run_listed_before_its_load_is_named_for_any_saved_folder_until_it_records_its_folders() {
        let project = "n2-before-load";
        let (vault, dump) = (
            PathBuf::from("/tmp/n2-before-load/vault"),
            PathBuf::from("/tmp/n2-before-load/dump"),
        );
        let run = starting(RunKind::Research, project).unwrap();
        assert!(
            runs_seeing(project, &[]).is_empty(),
            "nothing saved, nothing named"
        );
        assert_eq!(
            runs_seeing(project, std::slice::from_ref(&vault)),
            vec![RunKind::Research],
            "a hide saved before its load names it"
        );
        run.hid(std::slice::from_ref(&vault));
        assert!(
            runs_seeing(project, std::slice::from_ref(&vault)).is_empty(),
            "a folder it hid does not"
        );
        assert_eq!(
            runs_seeing(project, &[vault, dump]),
            vec![RunKind::Research],
            "a folder hidden after its load does"
        );
        drop(run);
    }

    /// SA-VID14 r1 (rv-sec2 F1): a run whose stop was uncertain, or whose processes the
    /// owner keeps, forgets its guard as it forgets its lease, so it stays named while its
    /// processes may still run, until the engine starts again.
    #[test]
    fn a_run_left_running_stays_named_after_its_launch_returns() {
        let project = "n2-left-running";
        let saved = [PathBuf::from("/tmp/n2-left-running/secret")];
        let run = listed_run(RunKind::Research, project, &[]);
        std::mem::forget(run);
        assert_eq!(runs_seeing(project, &saved), vec![RunKind::Research]);
    }
}
