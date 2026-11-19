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
/// outside the project, the project root itself, or a path that is not a folder is refused,
/// as is text that is not plain: on Windows a drive path joined by backslashes
/// ([`crate::sandbox::windows_clean_text`]), elsewhere one with no quote or backslash.
/// Inputs: the typed path and the project root. Output: the canonical folder, or a plain
/// refusal code.
pub fn checked(text: &str, project_root: &Path) -> Result<PathBuf, &'static str> {
    let typed = Path::new(text);
    if text.len() > PATH_TEXT_MAX || !typed.is_absolute() {
        return Err("hidden_path_not_absolute");
    }
    #[cfg(unix)]
    let plain = !text
        .chars()
        .any(|c| c.is_control() || c == '"' || c == '\\' || c == '\'');
    #[cfg(not(unix))]
    let plain = crate::sandbox::windows_clean_text(text);
    if !plain {
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
/// ever saved. A folder saved under the data volume's long name comes back under its short
/// name, the one a sandbox root has. Inputs: the open connection or transaction and the
/// project id. Output: the folders, or the database error.
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
            row.get::<_, String>(0)
                .map(|text| crate::projects_home::one_spelling(PathBuf::from(text)))
        })?
        .collect::<rusqlite::Result<Vec<PathBuf>>>()?;
    assert!(paths.len() <= HIDDEN_PATHS_MAX);
    Ok(paths)
}

/// The longest the tracked-files check lets git run before it is killed and the folder is
/// refused as unchecked.
const CHECK_TIME: std::time::Duration = std::time::Duration::from_secs(5);
const CHECK_POLL: std::time::Duration = std::time::Duration::from_millis(2);
/// The most of git's index listing the check reads, 64 MiB, about 600,000 paths of 100 bytes;
/// a longer listing is refused as unchecked.
const CHECK_BYTES_MAX: u64 = 64 * 1024 * 1024;
/// The longest one path in git's index listing may be, 32 KiB, eight times Linux's 4096-byte
/// path limit; a longer path is refused as unchecked.
const CHECK_LINE_BYTES_MAX: usize = 32 * 1024;
/// The read buffer of the tracked-files check, 64 KiB.
const CHECK_BUFFER_BYTES: usize = 64 * 1024;
const _: () = assert!(CHECK_LINE_BYTES_MAX as u64 <= CHECK_BYTES_MAX);
const _: () = assert!(CHECK_POLL.as_millis() < CHECK_TIME.as_millis());

/// Folds a file name so that names NTFS (simple uppercase) or APFS (full case folding, NFC or
/// NFD) take for one fold equal, and names Linux keeps apart too, on purpose: NFD, then std's
/// full lowercase, uppercase and lowercase again (`ẞ` and `ß` to `ss`, `ﬁ` to `fi`, `ı` to
/// `i`), then NFC. Too wide refuses a folder; too narrow lets a tracked file into a hidden one.
/// Input: the name. Output: its fold.
fn fold(name: &str) -> String {
    let decomposed = icu_normalizer::DecomposingNormalizerBorrowed::new_nfd().normalize(name);
    let cased = decomposed.to_lowercase().to_uppercase().to_lowercase();
    let folded = icu_normalizer::ComposingNormalizerBorrowed::new_nfc()
        .normalize(&cased)
        .into_owned();
    assert_eq!(folded.is_empty(), name.is_empty());
    assert_eq!(folded.contains('/'), name.contains('/'));
    folded
}

/// Whether a name from git's listing is this folded folder name: an ASCII name when it is with
/// ASCII case ignored, as its fold is its lowercase, any other name when its fold is. Inputs:
/// the listed name and the folded name. Output: whether they are one name.
fn same_name(listed: &str, folded: &str) -> bool {
    if listed.is_ascii() {
        listed.eq_ignore_ascii_case(folded)
    } else {
        fold(listed) == folded
    }
}

/// Whether a path from git's listing is a folder given as its folded names, or lies below it,
/// compared name by name with [`same_name`]. Inputs: the listed path and the folder's names.
/// Output: whether the path falls in the folder.
fn within(listed: &str, names: &[String]) -> bool {
    assert!(!names.is_empty());
    assert!(!listed.is_empty());
    let mut parts = listed.split('/');
    names
        .iter()
        .all(|name| parts.next().is_some_and(|part| same_name(part, name)))
}

/// What [`scan`] found in git's NUL-separated index listing.
#[derive(Debug, PartialEq, Eq)]
enum Scan {
    /// A listed path lies in one of the folders.
    Tracked,
    /// The whole listing was read, each path ended by a NUL, and none lies in the folders.
    End,
    /// A path was empty, not UTF-8, cut short or past [`CHECK_LINE_BYTES_MAX`], the listing
    /// passed [`CHECK_BYTES_MAX`], or it could not be read.
    Doubt,
}

/// Reads git's `ls-files -z` listing path by path and stops at the first path in one of the
/// folders, each given as its folded names, so the rest is never read. Inputs: the listing
/// and the folders. Output: what it found.
fn scan(listing: impl std::io::Read, folders: &[Vec<String>]) -> Scan {
    use std::io::{BufRead, Read};
    assert!(!folders.is_empty());
    assert!(folders.iter().all(|names| !names.is_empty()));
    let capped = listing.take(CHECK_BYTES_MAX + 1);
    let mut reader = std::io::BufReader::with_capacity(CHECK_BUFFER_BYTES, capped);
    let mut line = Vec::with_capacity(256);
    let mut total: u64 = 0;
    for _ in 0..=CHECK_BYTES_MAX {
        line.clear();
        let cap = CHECK_LINE_BYTES_MAX as u64 + 1;
        let Ok(read) = (&mut reader).take(cap).read_until(0, &mut line) else {
            return Scan::Doubt;
        };
        if read == 0 {
            return Scan::End;
        }
        assert_eq!(read, line.len());
        assert!(read <= CHECK_LINE_BYTES_MAX + 1);
        total += read as u64;
        if total > CHECK_BYTES_MAX || line.pop() != Some(0) {
            return Scan::Doubt;
        }
        let Ok(path) = std::str::from_utf8(&line) else {
            return Scan::Doubt;
        };
        if path.is_empty() {
            return Scan::Doubt;
        }
        if folders.iter().any(|names| within(path, names)) {
            return Scan::Tracked;
        }
    }
    unreachable!("each path read is at least one byte, so the listing cap ends the loop")
}

/// Each folder inside the root as its names below the root, folded. Inputs: the canonical
/// root and the folders. Output: the folded names of each folder inside the root, or
/// unchecked when a folder cannot be resolved or a name is not plain UTF-8.
fn folded_folders(root: &Path, folders: &[PathBuf]) -> Result<Vec<Vec<String>>, &'static str> {
    use std::path::Component;
    let unchecked = "hidden_path_unchecked";
    let mut wanted = Vec::with_capacity(folders.len());
    for folder in folders {
        let real = canonical(folder).map_err(|_| unchecked)?;
        let Ok(relative) = real.strip_prefix(root) else {
            continue;
        };
        let names = relative
            .components()
            .map(|part| match part {
                Component::Normal(name) => name.to_str().map(fold),
                _ => None,
            })
            .collect::<Option<Vec<String>>>()
            .filter(|names| !names.is_empty())
            .ok_or(unchecked)?;
        assert!(names.iter().all(|name| !name.is_empty() && fold(name) == *name));
        wanted.push(names);
    }
    assert!(wanted.len() <= folders.len());
    Ok(wanted)
}

/// Kills git and reaps it, whether or not it already exited. Input: the running git.
fn stop(child: &mut std::process::Child) {
    let _ = child.kill();
    let _ = child.wait();
}

/// Waits up to [`CHECK_TIME`] for [`scan`]'s verdict on git's listing, and kills git unless
/// the whole listing was read and git then exits clean in that time. Inputs: the running git
/// and the verdict. Output: `Ok(true)` for a tracked path, `Ok(false)` for a whole listing
/// with none and a clean exit, else unchecked.
fn listing_answer(
    child: &mut std::process::Child,
    verdict: &std::sync::mpsc::Receiver<Scan>,
) -> Result<bool, &'static str> {
    let unchecked = "hidden_path_unchecked";
    let deadline = std::time::Instant::now() + CHECK_TIME;
    let found = verdict.recv_timeout(CHECK_TIME).unwrap_or(Scan::Doubt);
    if found != Scan::End {
        stop(child);
        return if found == Scan::Tracked {
            Ok(true)
        } else {
            Err(unchecked)
        };
    }
    let polls_max = CHECK_TIME.as_millis() / CHECK_POLL.as_millis() + 1;
    for _ in 0..polls_max {
        match child.try_wait() {
            Ok(Some(status)) => return if status.success() { Ok(false) } else { Err(unchecked) },
            Ok(None) if std::time::Instant::now() < deadline => std::thread::sleep(CHECK_POLL),
            Ok(None) | Err(_) => break,
        }
    }
    stop(child);
    Err(unchecked)
}

/// Whether git tracks any file inside any of these folders: one `ls-files -z` run of the whole
/// index through [`crate::project_files::owner_git`], read as it streams by [`scan`], names
/// compared after [`fold`], git killed once the answer is known or after 5 s; a tracked file
/// lives in every agent's worktree and in history. Inputs: the project root and the folders.
/// Output: `Ok(false)` with no folder inside the root or no `.git`, else [`listing_answer`]'s.
fn any_tracked(project_root: &Path, folders: &[PathBuf]) -> Result<bool, &'static str> {
    use crate::project_files::NoOwnerGit;
    let unchecked = "hidden_path_unchecked";
    if folders.is_empty() {
        return Ok(false);
    }
    let root = canonical(project_root).map_err(|_| unchecked)?;
    let wanted = folded_folders(&root, folders)?;
    if wanted.is_empty() {
        return Ok(false);
    }
    let mut command = match crate::project_files::owner_git(&root) {
        Ok(command) => command,
        Err(NoOwnerGit::Absent) => return Ok(false),
        Err(NoOwnerGit::Refused | NoOwnerGit::Unavailable) => return Err(unchecked),
    };
    command
        .args(["ls-files", "-z"])
        .stdout(std::process::Stdio::piped());
    let mut child = crate::spawn_gate::spawn(&mut command).map_err(|_| unchecked)?;
    let Some(listing) = child.stdout.take() else {
        stop(&mut child);
        return Err(unchecked);
    };
    let (sender, verdict) = std::sync::mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let _ = sender.send(scan(listing, &wanted));
    });
    listing_answer(&mut child, &verdict)
}

/// Refuses a list that holds a folder git tracks, and refuses the whole list when the check
/// cannot answer for it, with one git run for the whole list (`any_tracked`). Inputs: the
/// project root and the checked folders. Output: ok, `hidden_path_tracked` or
/// `hidden_path_unchecked`.
pub fn none_tracked(project_root: &Path, folders: &[PathBuf]) -> Result<(), &'static str> {
    if any_tracked(project_root, folders)? {
        Err("hidden_path_tracked")
    } else {
        Ok(())
    }
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
        #[cfg(unix)]
        let parent = std::fs::canonicalize("/tmp").unwrap();
        #[cfg(not(unix))]
        let parent = canonical(&std::env::temp_dir()).unwrap();
        tempfile::Builder::new()
            .prefix("ca-hidden-")
            .tempdir_in(parent)
            .unwrap()
    }

    #[test]
    fn a_folder_inside_the_project_is_accepted_and_one_outside_is_refused() {
        let root = project();
        let inside = root.path().join("notes");
        std::fs::create_dir(&inside).unwrap();
        let got = checked(inside.to_str().unwrap(), root.path()).unwrap();
        assert_eq!(got, canonical(&inside).unwrap());

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

    /// On Windows a hidden folder is typed as a drive path joined by backslashes, so one inside
    /// the project is accepted; a quote, a forward slash or a UNC path is still refused.
    #[cfg(not(unix))]
    #[test]
    fn a_windows_drive_path_inside_the_project_is_accepted() {
        let root = project();
        let base = canonical(root.path()).unwrap();
        let inside = base.join("notes");
        std::fs::create_dir(&inside).unwrap();
        let text = inside.to_str().unwrap();
        assert!(text.contains('\\'), "{text}");
        assert_eq!(checked(text, root.path()), Ok(inside.clone()));
        for bad in [
            format!("{text}\\a\"b"),
            format!("{}/notes", base.display()),
            r"\\server\share\notes".to_owned(),
        ] {
            assert_eq!(
                checked(&bad, root.path()),
                Err("hidden_path_not_plain"),
                "{bad}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_folder_whose_name_holds_a_quote_or_backslash_is_refused() {
        let root = project();
        for bad in ["/tmp/a\"b", "/tmp/a\\b", "/tmp/a'b", "/tmp/a\nb"] {
            assert_eq!(checked(bad, root.path()), Err("hidden_path_not_plain"));
        }
    }

    /// The check for one folder, as a list of one. Inputs: the project root and the folder.
    /// Output: what [`any_tracked`] answers.
    fn git_tracks(project_root: &Path, folder: &Path) -> Result<bool, &'static str> {
        any_tracked(project_root, &[folder.to_path_buf()])
    }

    /// V244-HIDDEN-GIT-NONASCII-CASE (leader FROM-LEADER-2006): names any supported file system
    /// takes for one fold equal, among them `ẞ`, `ß` and `ss` (APFS), long s and `S` (NTFS),
    /// and the fold is the same for a letter's fold, uppercase and lowercase over every cased
    /// letter, which a fold of NFC(lower(upper(NFD))) misses for `ẞ`; other names stay apart.
    #[test]
    fn names_a_file_system_takes_for_one_fold_equal_and_others_do_not() {
        for (one, other) in [
            ("\u{c9}crits", "\u{e9}crits"),
            ("cl\u{e9}", "cle\u{301}"),
            ("\u{17f}ecret", "SECRET"),
            ("\u{212a}eys", "keys"),
            ("stra\u{df}e", "STRASSE"),
            ("\u{1e9e}", "ss"),
            ("\u{1e9e}", "\u{df}"),
            ("\u{fb01}le", "FILE"),
            ("\u{3a3}\u{391}\u{3a3}", "\u{3c3}\u{3b1}\u{3c2}"),
            ("\u{131}", "I"),
            ("\u{1f80}", "\u{1f88}"),
        ] {
            assert_eq!(fold(one), fold(other), "{one:?} {other:?}");
        }
        for (one, other) in [("secret", "secrets"), ("secret", "secre"), ("cle", "cl\u{e9}")] {
            assert_ne!(fold(one), fold(other), "{one:?} {other:?}");
        }
        assert_eq!(fold("Conf.D"), "conf.d");
        let mut cased = 0;
        for letter in char::MIN..=char::MAX {
            let upper: String = letter.to_uppercase().collect();
            let lower: String = letter.to_lowercase().collect();
            let alone = letter.to_string();
            if upper == alone && lower == alone {
                continue;
            }
            cased += 1;
            let folded = fold(&alone);
            assert_eq!(fold(&folded), folded, "{letter:?}");
            assert_eq!(fold(&upper), folded, "{letter:?}");
            assert_eq!(fold(&lower), folded, "{letter:?}");
        }
        assert!(cased > 2_000, "{cased}");
    }

    /// V244-HIDDEN-GIT-NONASCII-CASE: a listing is tracked by a whole path in a folder, at any
    /// depth and in any fold-equal spelling, and only then; a sibling sharing a name's start
    /// is not. An empty path, a path cut short, one not UTF-8 or one past the line cap is
    /// doubt, and so is a listing past the total cap; the scan stops at a tracked path.
    #[test]
    fn a_listing_is_tracked_only_by_a_whole_path_in_a_folder_and_doubt_on_a_broken_one() {
        use std::io::Read;
        let folders = vec![vec![fold("secret")], vec![fold("conf"), fold("keys")]];
        let scanned = |bytes: &[u8]| scan(bytes, &folders);
        assert_eq!(scanned(b""), Scan::End);
        assert_eq!(scanned(b"secrets/key.txt\0conf/key\0Secrets/x\0conf\0"), Scan::End);
        for tracked in [
            &b"secret/key.txt\0"[..],
            &b"a\0conf/keys\0"[..],
            &b"Secret/x\0"[..],
            &b"CONF/Keys/y/z\0"[..],
            &b"secret\0"[..],
            "\u{17f}ecret/k\0".as_bytes(),
            "conf/\u{212a}eys/x\0".as_bytes(),
        ] {
            let shown = String::from_utf8_lossy(tracked).into_owned();
            assert_eq!(scanned(tracked), Scan::Tracked, "{shown:?}");
        }
        for doubt in [
            &b"secret/key.txt"[..],
            &b"\0"[..],
            &b"a\0\0secret/x\0"[..],
            &b"\xff/x\0"[..],
        ] {
            let shown = String::from_utf8_lossy(doubt).into_owned();
            assert_eq!(scanned(doubt), Scan::Doubt, "{shown:?}");
        }
        let longest = [b"x".repeat(CHECK_LINE_BYTES_MAX), vec![0]].concat();
        assert_eq!(scanned(&longest), Scan::End);
        let longer = [b"x".repeat(CHECK_LINE_BYTES_MAX + 1), vec![0]].concat();
        assert_eq!(scanned(&longer), Scan::Doubt);
        let line = b"plain/x.txt\0";
        let whole = CHECK_BYTES_MAX - CHECK_BYTES_MAX % line.len() as u64;
        assert_eq!(scan(Cycle(line, 0).take(whole), &folders), Scan::End);
        let past = whole + line.len() as u64;
        assert!(past > CHECK_BYTES_MAX);
        assert_eq!(scan(Cycle(line, 0).take(past), &folders), Scan::Doubt);
        let broken = b"plain/x.txt\0secret/key.txt\0".chain(Broken);
        assert_eq!(scan(broken, &folders), Scan::Tracked);
        assert_eq!(scan(b"plain/x.txt\0".chain(Broken), &folders), Scan::Doubt);
    }

    /// A listing that repeats one line without end, from a byte offset into it.
    struct Cycle(&'static [u8], usize);

    impl std::io::Read for Cycle {
        /// Fills the buffer with the line repeated. Input: the buffer. Output: its length.
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            for byte in buffer.iter_mut() {
                *byte = self.0[self.1];
                self.1 = (self.1 + 1) % self.0.len();
            }
            Ok(buffer.len())
        }
    }

    /// A listing whose every read fails, so a scan that reads it answers doubt.
    struct Broken;

    impl std::io::Read for Broken {
        /// Fails. Input: the buffer, untouched. Output: an error.
        fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("broken listing"))
        }
    }

    /// Runs the test's own git in `path` with no system or global config, as the owner makes
    /// a repository: `/usr/bin/git` on Unix, the git on PATH on Windows. Inputs: the folder and
    /// the arguments.
    fn git_in(path: &Path, args: &[&str]) {
        let program = if cfg!(unix) { "/usr/bin/git" } else { "git" };
        let mut command = std::process::Command::new(program);
        command
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", crate::github::NULL_DEVICE)
            .current_dir(path)
            .args(args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        let status = crate::spawn_gate::status(&mut command).unwrap();
        assert!(status.success(), "git {args:?}");
    }

    /// A repository with `secret/key.txt` tracked and `scratch/tmp.txt` not. Input: the
    /// canonical project folder. Output: the tracked and the untracked folder.
    fn tracked_and_untracked(path: &Path) -> (PathBuf, PathBuf) {
        git_in(path, &["init", "-q"]);
        let tracked = path.join("secret");
        let untracked = path.join("scratch");
        std::fs::create_dir(&tracked).unwrap();
        std::fs::create_dir(&untracked).unwrap();
        std::fs::write(tracked.join("key.txt"), "k\n").unwrap();
        std::fs::write(untracked.join("tmp.txt"), "t\n").unwrap();
        git_in(path, &["add", "secret/key.txt"]);
        (tracked, untracked)
    }

    /// V243-WIN-HIDDEN-GIT: a `commondir` planted in `.git`, naming a common folder whose config
    /// holds an unknown repository extension, redirects nothing. On Unix git's common folder is
    /// pinned to `.git`, so the tracked folder is still found; on Windows the check before git
    /// starts refuses a `.git` that points elsewhere, so the folder is unchecked.
    #[test]
    fn a_commondir_planted_in_the_git_folder_redirects_nothing() {
        let root = project();
        let path = canonical(root.path()).unwrap();
        let (tracked, _) = tracked_and_untracked(&path);
        let store = project();
        for name in ["objects", "refs"] {
            std::fs::create_dir(store.path().join(name)).unwrap();
        }
        std::fs::write(
            store.path().join("config"),
            "[core]\n\trepositoryformatversion = 1\n[extensions]\n\tplanted = true\n",
        )
        .unwrap();
        let common = format!("{}\n", canonical(store.path()).unwrap().display());
        std::fs::write(path.join(".git").join("commondir"), common).unwrap();
        let answer = git_tracks(&path, &tracked);
        if cfg!(windows) {
            assert_eq!(answer, Err("hidden_path_unchecked"));
        } else {
            assert_eq!(answer, Ok(true));
        }
    }

    /// V243-WIN-HIDDEN-GIT: a tracked folder two levels down is found, and so is the folder
    /// holding it; on Windows, where folder names ignore case, the folder typed in another case
    /// is found too, since git is asked about its canonical spelling joined by `/`.
    #[test]
    fn a_tracked_folder_inside_another_is_found_in_any_case_spelling_windows_accepts() {
        let root = project();
        let path = canonical(root.path()).unwrap();
        git_in(&path, &["init", "-q"]);
        let nested = path.join("conf").join("secret");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(nested.join("key.txt"), "k\n").unwrap();
        git_in(&path, &["add", "conf/secret/key.txt"]);
        assert_eq!(git_tracks(&path, &nested), Ok(true));
        assert_eq!(git_tracks(&path, &path.join("conf")), Ok(true));
        if cfg!(windows) {
            let typed = path.join("CONF").join("Secret");
            assert_eq!(git_tracks(&path, &typed), Ok(true));
        }
    }

    /// #590 (rv-sec3 F1): a folder renamed on disk to another case without git, so the index
    /// still holds its old case, is found tracked; a folder whose name reads as a pattern
    /// (`[s]ecret`) matches only itself, so the tracked `secret` beside it stays out of its
    /// answer.
    #[test]
    fn a_folder_the_index_tracks_under_another_case_is_found_and_a_pattern_name_is_literal() {
        let root = project();
        let path = canonical(root.path()).unwrap();
        let (tracked, _) = tracked_and_untracked(&path);
        let pattern = path.join("[s]ecret");
        std::fs::create_dir(&pattern).unwrap();
        std::fs::write(pattern.join("tmp.txt"), "t\n").unwrap();
        assert_eq!(git_tracks(&path, &pattern), Ok(false));
        let renamed = path.join("Secret");
        std::fs::rename(&tracked, &renamed).unwrap();
        assert_eq!(git_tracks(&path, &renamed), Ok(true));
        assert_eq!(none_tracked(&path, &[renamed]), Err("hidden_path_tracked"));
    }

    /// V244-HIDDEN-GIT-NONASCII-CASE: a folder with a non-ASCII name that git does not track
    /// is checked like any other folder, with or without a repository, on every system.
    #[test]
    fn an_untracked_non_ascii_folder_is_not_tracked() {
        let plain = project();
        let plain_path = canonical(plain.path()).unwrap();
        let loose = plain_path.join("cl\u{e9}");
        std::fs::create_dir(&loose).unwrap();
        assert_eq!(git_tracks(&plain_path, &loose), Ok(false));
        let root = project();
        let path = canonical(root.path()).unwrap();
        let _ = tracked_and_untracked(&path);
        let folder = path.join("cl\u{e9}");
        std::fs::create_dir(&folder).unwrap();
        std::fs::write(folder.join("tmp.txt"), "t\n").unwrap();
        assert_eq!(git_tracks(&path, &folder), Ok(false));
        assert_eq!(none_tracked(&path, &[folder]), Ok(()));
    }

    /// V244-HIDDEN-GIT-NONASCII-CASE: a folder git added as `Écrits` and renamed on disk to
    /// `écrits` without git still has its files in the index under the old name, so it is
    /// found tracked; on Linux too, where the fold is the same on purpose.
    #[test]
    fn a_folder_the_index_names_with_another_non_ascii_case_is_found_tracked() {
        let root = project();
        let path = canonical(root.path()).unwrap();
        git_in(&path, &["init", "-q"]);
        let indexed = path.join("\u{c9}crits");
        std::fs::create_dir(&indexed).unwrap();
        std::fs::write(indexed.join("a.txt"), "a\n").unwrap();
        git_in(&path, &["add", "--", "\u{c9}crits/a.txt"]);
        let renamed = path.join("\u{e9}crits");
        std::fs::rename(&indexed, &renamed).unwrap();
        assert_eq!(git_tracks(&path, &renamed), Ok(true));
        assert_eq!(none_tracked(&path, &[renamed]), Err("hidden_path_tracked"));
    }

    /// V244-HIDDEN-GIT-NONASCII-CASE: the index holds `clé` composed, as git on macOS
    /// stores it, while the folder on disk is spelled decomposed (`e` and a combining
    /// accent); the two are one name to macOS, so the folder is found tracked everywhere.
    #[test]
    fn a_folder_made_nfd_on_disk_is_found_under_its_nfc_index_name() {
        let root = project();
        let path = canonical(root.path()).unwrap();
        git_in(&path, &["init", "-q"]);
        std::fs::write(path.join("empty.txt"), "").unwrap();
        git_in(&path, &["hash-object", "-w", "--", "empty.txt"]);
        let entry = format!("100644,{EMPTY_BLOB},cl\u{e9}/k.txt");
        git_in(&path, &["update-index", "--add", "--cacheinfo", &entry]);
        let folder = path.join("cle\u{301}");
        std::fs::create_dir(&folder).unwrap();
        assert_eq!(git_tracks(&path, &folder), Ok(true));
        assert_eq!(none_tracked(&path, &[folder]), Err("hidden_path_tracked"));
    }

    /// V244-HIDDEN-GIT-NONASCII-CASE: an index entry under `ſecret` (long s), which macOS and
    /// Windows read as the folder `secret`, makes the plain ASCII folder `secret` tracked,
    /// though git's own `icase` folds ASCII letters only.
    #[test]
    fn an_ascii_folder_whose_index_name_is_a_twin_spelling_is_found_tracked() {
        let root = project();
        let path = canonical(root.path()).unwrap();
        git_in(&path, &["init", "-q"]);
        std::fs::write(path.join("empty.txt"), "").unwrap();
        git_in(&path, &["hash-object", "-w", "--", "empty.txt"]);
        let entry = format!("100644,{EMPTY_BLOB},\u{17f}ecret/key.txt");
        git_in(&path, &["update-index", "--add", "--cacheinfo", &entry]);
        let folder = path.join("secret");
        std::fs::create_dir(&folder).unwrap();
        assert_eq!(git_tracks(&path, &folder), Ok(true));
        assert_eq!(none_tracked(&path, &[folder]), Err("hidden_path_tracked"));
    }

    /// V244-HIDDEN-GIT-NONASCII-CASE (leader FROM-LEADER-2006): on Linux, where `Écrits` and
    /// `écrits` are two folders, a hidden `écrits` beside a tracked `Écrits` is refused as
    /// tracked on purpose; the owner is told a tracked path matches it when case is ignored.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_folder_beside_a_tracked_one_of_another_case_is_refused_on_linux() {
        let root = project();
        let path = canonical(root.path()).unwrap();
        git_in(&path, &["init", "-q"]);
        let tracked = path.join("\u{c9}crits");
        std::fs::create_dir(&tracked).unwrap();
        std::fs::write(tracked.join("a.txt"), "a\n").unwrap();
        git_in(&path, &["add", "--", "\u{c9}crits/a.txt"]);
        let hidden = path.join("\u{e9}crits");
        std::fs::create_dir(&hidden).unwrap();
        std::fs::write(hidden.join("b.txt"), "b\n").unwrap();
        assert_eq!(git_tracks(&path, &tracked), Ok(true));
        assert_eq!(git_tracks(&path, &hidden), Ok(true));
        assert_eq!(none_tracked(&path, &[hidden]), Err("hidden_path_tracked"));
    }

    /// The object id of the empty file, which the rows write with `hash-object -w` before an
    /// index entry names it.
    const EMPTY_BLOB: &str = "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391";

    /// V243-WIN-HIDDEN-GIT (leader 09:21): a saved list of several folders costs one git run,
    /// wherever the tracked folder stands in it, and an empty list runs no git at all.
    #[test]
    fn a_list_of_folders_is_checked_with_one_git_run() {
        let root = project();
        let path = canonical(root.path()).unwrap();
        let (tracked, untracked) = tracked_and_untracked(&path);
        let mut folders = vec![untracked];
        for name in ["plain-a", "plain-b", "plain-c"] {
            let folder = path.join(name);
            std::fs::create_dir(&folder).unwrap();
            folders.push(folder);
        }
        let runs = crate::project_files::owner_git_runs(|| {
            assert_eq!(none_tracked(&path, &folders), Ok(()));
        });
        assert_eq!(runs, 1, "four untracked folders");
        folders.push(tracked);
        let runs = crate::project_files::owner_git_runs(|| {
            assert_eq!(none_tracked(&path, &folders), Err("hidden_path_tracked"));
        });
        assert_eq!(runs, 1, "the tracked folder last of five");
        let runs = crate::project_files::owner_git_runs(|| {
            assert_eq!(none_tracked(&path, &[]), Ok(()));
        });
        assert_eq!(runs, 0, "no folder");
    }

    /// Adds an index entry naming the empty file for each of these paths, with one
    /// `update-index --index-info` run of the test's own git after it writes that file's
    /// object. Inputs: the repository and the paths.
    fn add_to_index(path: &Path, listed: &[String]) {
        use std::io::Write;
        git_in(path, &["hash-object", "-w", "--stdin"]);
        let program = if cfg!(unix) { "/usr/bin/git" } else { "git" };
        let mut command = std::process::Command::new(program);
        command
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", crate::github::NULL_DEVICE)
            .current_dir(path)
            .args(["update-index", "--add", "--index-info"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        let mut child = crate::spawn_gate::spawn(&mut command).unwrap();
        let lines: String = listed
            .iter()
            .map(|name| format!("100644 {EMPTY_BLOB}\t{name}\n"))
            .collect();
        child.stdin.take().unwrap().write_all(lines.as_bytes()).unwrap();
        assert!(child.wait().unwrap().success(), "{} entries", listed.len());
    }

    /// V243-WIN-HIDDEN-GIT (leader 08:33, 09:21), V244-HIDDEN-GIT-NONASCII-CASE (leader
    /// FROM-LEADER-2006): times `none_tracked` per call as each launch gate makes it, on one
    /// untracked folder and on a full list, 20 calls each, in a tiny index and in one of 100,000
    /// paths (two fifths under non-ASCII names) read whole; prints the median and the slowest.
    /// The farm runs it quiet and under a CPU load step; it asserts only the answers.
    #[test]
    #[ignore = "timing, run by the farm quiet and under a CPU load step"]
    fn none_tracked_time_per_call() {
        let root = project();
        let path = canonical(root.path()).unwrap();
        let (tracked, untracked) = tracked_and_untracked(&path);
        assert_eq!(none_tracked(&path, &[tracked]), Err("hidden_path_tracked"));
        let mut full = vec![untracked.clone()];
        for n in 1..HIDDEN_PATHS_MAX {
            let folder = path.join(format!("plain-{n}-\u{e9}"));
            std::fs::create_dir(&folder).unwrap();
            full.push(folder);
        }
        let tops = ["src", "docs", "Donn\u{e9}es", "\u{30c6}\u{30b9}\u{30c8}", "vendor"];
        let bulk: Vec<String> = (1..100_000)
            .map(|n| format!("{}/d{:03}/file-{n:06}.txt", tops[n % 5], n / 100))
            .collect();
        let ms = |time: std::time::Duration| time.as_secs_f64() * 1000.0;
        for entries in [1, 100_000] {
            if entries > 1 {
                add_to_index(&path, &bulk);
            }
            for folders in [std::slice::from_ref(&untracked), &full[..]] {
                let mut times: Vec<std::time::Duration> = (0..20)
                    .map(|_| {
                        let start = std::time::Instant::now();
                        assert_eq!(none_tracked(&path, folders), Ok(()));
                        start.elapsed()
                    })
                    .collect();
                times.sort();
                println!(
                    "TIMING none_tracked entries={entries} folders={} calls=20 median_ms={:.1} max_ms={:.1}",
                    folders.len(),
                    ms(times[10]),
                    ms(times[19])
                );
            }
        }
    }

    #[test]
    fn a_folder_git_tracks_is_refused_and_an_untracked_one_is_accepted() {
        let root = project();
        let path = canonical(root.path()).unwrap();
        let git = |args: &[&str]| git_in(&path, args);
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
        let path = canonical(root.path()).unwrap();
        let folder = path.join("plain");
        std::fs::create_dir(&folder).unwrap();
        assert_eq!(git_tracks(&path, &folder), Ok(false));
        assert_eq!(none_tracked(&path, &[folder]), Ok(()));
    }

    #[cfg(unix)]
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
        let path = canonical(root.path()).unwrap();
        let git = |args: &[&str]| git_in(&path, args);
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

    /// V244-HIDDEN-GIT-NONASCII-CASE: a tracked folder listed after more than 64 KiB of other
    /// paths, the old read cap, is still found, and so is the folder those paths fill.
    #[test]
    fn a_tracked_folder_listed_after_64_kib_of_other_paths_is_still_tracked() {
        let root = project();
        let path = canonical(root.path()).unwrap();
        let (tracked, _) = tracked_and_untracked(&path);
        let folder = path.join("bulk");
        std::fs::create_dir(&folder).unwrap();
        let listed: Vec<String> = (0..320)
            .map(|n| format!("bulk/{n:06}-{}.txt", "x".repeat(200)))
            .collect();
        assert!(listed.iter().map(|name| name.len() + 1).sum::<usize>() > 64 * 1024);
        add_to_index(&path, &listed);
        assert_eq!(git_tracks(&path, &tracked), Ok(true));
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
        assert!(covers(Some(&built), std::slice::from_ref(&secrets)));
        assert!(
            covers(Some(&built), std::slice::from_ref(&inner)),
            "a folder above it was hidden"
        );
        assert!(!covers(Some(&built), &[secrets.clone(), vendor.clone()]));
        assert!(
            !covers(Some(&[]), std::slice::from_ref(&secrets)),
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
            let run = starting(RunKind::Lockfiles, project).ok_or("full")?;
            run.hid(&[]);
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
                    "if active.host.settled()",
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
                    .components()
                    .map(|part| part.as_os_str().to_str().unwrap())
                    .collect::<Vec<_>>()
                    .join("/");
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
