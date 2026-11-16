use super::journal::{self, Entry, Journal};
use super::mute;
use crate::engine::EngineError;
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

/// The journal kind of one app Keep awake froze while the lid was closed (DEC-167 Task 17).
pub const KIND: &str = "keep_awake_freeze";

/// The most apps the owner may choose to freeze.
pub const APPS_MAX: usize = 16;

/// The most processes one app may have and still be frozen: its main process and its
/// descendants when the lid closes. An app with more is left running.
pub const PROCS_MAX: usize = 48;

const NAME_BYTES_MAX: usize = 128;
const ID_BYTES_MAX: usize = 1024;
const APPS_BYTES_MAX: usize = 32 * 1024;
const RUNNING_MAX: usize = 512;
#[cfg(unix)]
const AGENTS_MAX: usize = 4096;
const LOCK: &str = "lid-closed.lock";
const LOCK_WAIT: Duration = Duration::from_secs(5);
const LOCK_STEP: Duration = Duration::from_millis(50);
const GUARD_TRIES: u32 = 6;
const GUARD_RETRY: Duration = Duration::from_secs(1);
#[cfg(unix)]
const OWN_IDS: [&str; 1] = ["dev.crazyagent.desktop"];
#[cfg(unix)]
const SYSTEM: [&str; 7] = [
    "/System/",
    "/Library/Apple/",
    "/usr/libexec/",
    "/usr/sbin/",
    "/sbin/",
    "/usr/lib/systemd/",
    "/bin/",
];
#[cfg(unix)]
const DESKTOPS: [&str; 10] = [
    "gnome-shell",
    "plasmashell",
    "kwin_wayland",
    "kwin_x11",
    "Xorg",
    "Xwayland",
    "mutter",
    "sway",
    "Hyprland",
    "gnome-session-binary",
];

const TABLE: &str = "CREATE TABLE IF NOT EXISTS configured_keep_awake_freeze (
    singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
    apps TEXT NOT NULL
)";

/// One app the owner may choose: the name shown and its id, the bundle id on macOS and
/// the executable path on Linux.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FreezeApp {
    pub name: String,
    pub id: String,
}

/// One running app as the platform lists it: the app, the pid of its main process and the
/// executable that process runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Running {
    pub app: FreezeApp,
    pub pid: u32,
    pub path: PathBuf,
}

/// Who one process is: its pid, when it started and the executable it runs. A pid whose
/// start time or executable differs later belongs to another process.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Proc {
    pub pid: u32,
    pub start: String,
    pub exe: String,
}

/// One frozen app as the journal keeps it: its name, the boot session it was frozen in
/// and every process that was sent SIGSTOP.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Frozen {
    pub app: String,
    pub boot: String,
    pub procs: Vec<Proc>,
}

impl Frozen {
    /// The journal entry that thaws this app. Input: the frozen app. Output: an entry of
    /// kind `KIND` whose restore value is this record as JSON.
    pub fn entry(&self) -> Entry {
        assert!(self.procs.len() <= PROCS_MAX);
        let restore = serde_json::to_string(self).expect("a frozen record is plain data");
        Entry {
            kind: KIND.into(),
            restore,
        }
    }
}

fn error(code: &str, problem: impl std::fmt::Display) -> EngineError {
    assert!(code.starts_with("keep_awake_"));
    EngineError(format!("{code}: {problem}"))
}

/// Checks the owner's app list: at most `APPS_MAX`, each with a plain name of 1 to 128
/// bytes and an id of 1 to 1024 bytes, no id twice. Input: the list. Output: Ok, or
/// `keep_awake_apps_invalid`.
pub fn validate_apps(apps: &[FreezeApp]) -> Result<(), EngineError> {
    let invalid = || EngineError("keep_awake_apps_invalid".into());
    if apps.len() > APPS_MAX {
        return Err(invalid());
    }
    let plain = |text: &str, max: usize| {
        (1..=max).contains(&text.len()) && !text.chars().any(char::is_control)
    };
    for (at, app) in apps.iter().enumerate() {
        if !plain(&app.name, NAME_BYTES_MAX) || !plain(&app.id, ID_BYTES_MAX) {
            return Err(invalid());
        }
        if apps[..at].iter().any(|before| before.id == app.id) {
            return Err(invalid());
        }
    }
    Ok(())
}

/// Creates the app list table when it is missing. Input: the engine database. Output: Ok,
/// or the database error.
pub(super) fn initialize(connection: &Connection) -> Result<(), EngineError> {
    connection
        .execute_batch(TABLE)
        .map_err(|problem| EngineError(problem.to_string()))
}

/// Saves the owner's app list over the stored one. Inputs: the open transaction and the
/// checked list. Output: Ok, or the database error.
pub(super) fn save_apps(connection: &Connection, apps: &[FreezeApp]) -> Result<(), EngineError> {
    validate_apps(apps)?;
    let text = serde_json::to_string(apps).map_err(|problem| EngineError(problem.to_string()))?;
    assert!(text.len() <= APPS_BYTES_MAX);
    let stored = connection
        .execute(
            "INSERT INTO configured_keep_awake_freeze(singleton,apps) VALUES(1,?1) \
             ON CONFLICT(singleton) DO UPDATE SET apps=excluded.apps",
            rusqlite::params![text],
        )
        .map_err(|problem| EngineError(problem.to_string()))?;
    assert_eq!(stored, 1);
    Ok(())
}

/// Loads the owner's app list. Input: the engine database. Output: the list, empty before
/// the first save, or `keep_awake_apps_invalid` for a stored list this engine cannot read.
pub fn load_apps(connection: &Connection) -> Result<Vec<FreezeApp>, EngineError> {
    let invalid = || EngineError("keep_awake_apps_invalid".into());
    let made: Option<i64> = connection
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type='table' AND name='configured_keep_awake_freeze'",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(|problem| EngineError(problem.to_string()))?;
    if made.is_none() {
        return Ok(Vec::new());
    }
    let stored: Option<Option<String>> = connection
        .query_row(
            "SELECT CASE WHEN length(CAST(apps AS BLOB))<=?1 THEN apps END \
             FROM configured_keep_awake_freeze WHERE singleton=1",
            [APPS_BYTES_MAX as i64],
            |row| row.get(0),
        )
        .optional()
        .map_err(|problem| EngineError(problem.to_string()))?;
    let Some(text) = stored else {
        return Ok(Vec::new());
    };
    let apps: Vec<FreezeApp> =
        serde_json::from_str(&text.ok_or_else(invalid)?).map_err(|_| invalid())?;
    validate_apps(&apps)?;
    Ok(apps)
}

/// Opens the lid-closed lane's own journal below the engine root. The caller holds `lock`
/// first, so no other writer changes the file meanwhile. Input: the engine root. Output:
/// the journal, or why it cannot be opened.
pub fn journal(root: &Path) -> Result<Journal, EngineError> {
    Journal::open_named(root, journal::LID_CLOSED)
}

/// Takes the lid-closed journal's lock, waiting at most `LOCK_WAIT`, so the engine and a
/// thaw guard never write the file at once. The lock goes with the returned file, and
/// with its process. Input: the engine root. Output: the locked file, or why not.
pub fn lock(root: &Path) -> Result<File, EngineError> {
    let path = journal::folder(root)?.join(LOCK);
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let file = options
        .open(&path)
        .map_err(|problem| error("keep_awake_freeze_lock", problem))?;
    let until = Instant::now() + LOCK_WAIT;
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(file),
            Err(std::fs::TryLockError::WouldBlock) if Instant::now() < until => {
                std::thread::sleep(LOCK_STEP);
            }
            Err(problem) => return Err(error("keep_awake_freeze_lock", problem)),
        }
    }
}

/// Undoes one entry of the lid-closed journal: thaws a frozen app or puts the sound back
/// with this platform's tool. Input: the entry. Output: Ok, or why it stays recorded.
pub fn undo(entry: &Entry) -> Result<(), EngineError> {
    match entry.kind.as_str() {
        KIND => thaw(entry),
        mute::KIND => {
            let tool = mute::tool_for(entry)?;
            mute::restore(entry, &tool)
        }
        other => Err(error("keep_awake_journal_kind_unknown", other)),
    }
}

/// Builds the thaw guard's command: this program's `keep-awake guard ROOT`, in its own
/// process group, so a stop of the engine's group leaves it. The caller hands it the read
/// end of the engine's pipe as stdin. Inputs: the CrazyAgent program and the engine root.
/// Output: the command, not started.
pub fn guard_command(program: &Path, root: &Path) -> Command {
    assert!(program.is_absolute());
    assert!(root.is_absolute());
    let mut command = Command::new(program);
    command
        .args(["keep-awake", "guard"])
        .arg(root)
        .current_dir("/");
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    command
}

/// The thaw guard (leader 23:13): waits on the engine's pipe. A byte means the engine
/// thawed everything itself, so the guard leaves. The end of the pipe means the engine is
/// gone: it takes the lock, thaws every app whose identity still matches, puts the sound
/// back, settles those entries and leaves. An entry that fails is tried again at most
/// `GUARD_TRIES` times, the wait doubling from one second, with the lock let go between
/// tries (rv-secreview and rv-code T17 r1). Inputs: the engine root and the pipe. Output:
/// Ok, or the last failure (its entry stays for the next engine start).
pub fn guard(root: &Path, mut engine: impl Read) -> Result<(), EngineError> {
    let mut byte = [0_u8; 1];
    loop {
        match engine.read(&mut byte) {
            Ok(0) => break,
            Ok(_) => return Ok(()),
            Err(problem) if problem.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => break,
        }
    }
    let mut wait = GUARD_RETRY;
    let mut last = None;
    for tried in 1..=GUARD_TRIES {
        match settle_all(root) {
            Ok(()) => return Ok(()),
            Err(problem) => last = Some(problem),
        }
        if tried < GUARD_TRIES {
            std::thread::sleep(wait);
            wait = wait.saturating_mul(2);
        }
    }
    Err(last.expect("the guard tried at least once"))
}

/// Takes the lock and undoes every entry of the lid-closed journal once. Input: the engine
/// root. Output: Ok when nothing is left, or the first failure, its entry kept.
fn settle_all(root: &Path) -> Result<(), EngineError> {
    let _held = lock(root)?;
    let mut found = journal(root)?;
    found.reconcile(undo)?;
    assert!(found.entries().is_empty());
    Ok(())
}

/// What the keeper asks of the lane while the lid is closed during a hold: freeze the
/// chosen apps, mute the sound, or both.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cover {
    pub freeze: bool,
    pub mute: bool,
    pub apps: Vec<FreezeApp>,
}

impl Cover {
    /// Tells whether this cover asks for any change at all. Output: true when the sound
    /// is to be muted or at least one app frozen.
    pub fn any(&self) -> bool {
        self.mute || (self.freeze && !self.apps.is_empty())
    }
}

#[cfg(unix)]
pub use lane::Lane;

#[cfg(unix)]
pub use unix::{
    allowed, boot_session, freeze, identity, pickable, running, servers, thaw, thaw_with,
};

#[cfg(unix)]
pub(super) use unix::run;

#[cfg(not(unix))]
pub use other::{
    allowed, boot_session, freeze, identity, pickable, running, servers, thaw, thaw_with,
};

/// The engine read of the apps the owner may pick (Task 17): the running apps less every
/// one that may never be frozen. Output: the apps by name, or why they cannot be listed.
pub fn choices() -> Result<Vec<FreezeApp>, EngineError> {
    let running = running()?;
    assert!(running.len() <= RUNNING_MAX);
    pickable(&running, std::process::id(), &BTreeSet::new())
}

/// Tells whether a tmux server's command line serves a socket in one of this engine's
/// socket folders (leader 01:57, until T17b). Inputs: the command line and the folders.
/// Output: true when it does.
#[cfg(unix)]
fn serves(args: &str, folders: &[PathBuf]) -> bool {
    folders
        .iter()
        .filter(|folder| folder.is_absolute())
        .any(|folder| args.contains(&format!(" -S {}/", folder.display())))
}

/// This engine's tmux socket folders (leader 01:57, until T17b): its terminal folder and
/// the short temp folder of each agent below its team folder, at most `AGENTS_MAX`. Input:
/// the engine root. Output: the folders.
#[cfg(unix)]
fn socket_folders(root: &Path) -> Vec<PathBuf> {
    let mut folders = vec![root.join(crate::terminal::FOLDER)];
    let agents = root.join("team").join("agents");
    let listing = std::fs::read_dir(agents).into_iter().flatten().flatten();
    for found in listing.take(AGENTS_MAX) {
        if found.file_name().to_string_lossy().starts_with("agent-") {
            let home = found.path().join("home");
            folders.push(crate::sandbox::toolchains::tmp_path(&home));
        }
    }
    assert!(!folders.is_empty() && folders.len() <= AGENTS_MAX + 1);
    folders
}

/// Tells whether an executable may never be frozen: a system program, a desktop shell, or
/// CrazyAgent itself (its name or its own folder). Inputs: the path and this program.
/// Output: true when it is kept out.
#[cfg(unix)]
fn kept_out(path: &Path, here: Option<&Path>) -> bool {
    let text = path.to_string_lossy();
    let system = SYSTEM.iter().any(|prefix| text.starts_with(prefix))
        || (cfg!(target_os = "macos") && text.starts_with("/usr/bin/"));
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let desktop = DESKTOPS.contains(&name.as_str());
    let ours = name.to_ascii_lowercase().starts_with("crazyagent")
        || here
            .and_then(Path::parent)
            .is_some_and(|folder| path.starts_with(folder));
    system || desktop || ours || !path.is_absolute()
}

#[cfg(not(unix))]
mod other {
    use super::{EngineError, FreezeApp, Frozen, Journal, Proc, Running};
    use crate::keep_awake::journal::Entry;
    use std::collections::{BTreeMap, BTreeSet};
    use std::path::Path;

    fn unavailable() -> EngineError {
        EngineError("keep_awake_freeze_unavailable: Not available on Windows".into())
    }

    /// Windows has no app freezing (DEC-167 Task 17). Output: the refusal.
    pub fn identity(_pid: u32) -> Result<Proc, EngineError> {
        Err(unavailable())
    }

    /// Windows has no app freezing. Output: the refusal.
    pub fn boot_session() -> Result<String, EngineError> {
        Err(unavailable())
    }

    /// Windows has no app freezing. Output: the refusal.
    pub fn freeze(
        _journal: &mut Journal,
        _app: &Running,
        _engine: u32,
        _roots: &BTreeSet<u32>,
    ) -> Result<Frozen, EngineError> {
        Err(unavailable())
    }

    /// Windows has no app freezing. Output: the refusal.
    pub fn thaw(_entry: &Entry) -> Result<(), EngineError> {
        Err(unavailable())
    }

    /// Windows has no app freezing. Output: the refusal.
    pub fn thaw_with(
        _entry: &Entry,
        _read: impl Fn(&[u32]) -> Result<BTreeMap<u32, Proc>, EngineError>,
    ) -> Result<(), EngineError> {
        Err(unavailable())
    }

    /// Windows has no app freezing. Output: the refusal.
    pub fn allowed(
        _running: &[Running],
        _engine: u32,
        _roots: &BTreeSet<u32>,
    ) -> Result<Vec<Running>, EngineError> {
        Err(unavailable())
    }

    /// Windows has no app freezing. Output: the refusal.
    pub fn pickable(
        _running: &[Running],
        _engine: u32,
        _roots: &BTreeSet<u32>,
    ) -> Result<Vec<FreezeApp>, EngineError> {
        Err(unavailable())
    }

    /// Windows has no app freezing. Output: the refusal.
    pub fn servers(_root: &Path) -> Result<BTreeSet<u32>, EngineError> {
        Err(unavailable())
    }

    /// Windows has no app freezing. Output: the refusal.
    pub fn running() -> Result<Vec<Running>, EngineError> {
        Err(unavailable())
    }
}

#[cfg(unix)]
mod unix {
    use super::{
        EngineError, Entry, FreezeApp, Frozen, Journal, KIND, PROCS_MAX, Proc, RUNNING_MAX,
        Running, error, kept_out,
    };
    use std::collections::{BTreeMap, BTreeSet};
    use std::io::Read;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    const TEXT_BYTES_MAX: usize = 4 * 1024 * 1024;
    const TABLE_MAX: usize = 32 * 1024;
    const TREE_MAX: usize = 256;
    const ROOTS_MAX: usize = 1024;
    const SERVERS_MAX: usize = 256;
    const RUN_WAIT: Duration = Duration::from_secs(2);
    const RUN_STEP: Duration = Duration::from_millis(10);
    const SHARED: [&str; 7] = [
        "/bin",
        "/sbin",
        "/usr/bin",
        "/usr/sbin",
        "/usr/local/bin",
        "/opt/homebrew/bin",
        "/usr/games",
    ];

    /// One row of the process table: pid, parent pid and owner uid.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct Row {
        pid: u32,
        ppid: u32,
        uid: u32,
    }

    /// Runs one system tool and returns what it printed, waiting at most `RUN_WAIT` and
    /// reading at most `TEXT_BYTES_MAX` on a thread of its own, so a full pipe never stalls
    /// the wait. Input: the configured command. Output: the text, or none when the tool
    /// did not start, did not finish in time, failed or printed too much.
    pub(in crate::keep_awake) fn run(command: &mut Command) -> Option<String> {
        let (success, text) = ran(command)?;
        success.then_some(text)
    }

    /// `run`, keeping what a tool printed when it exits with a failure, as `ps -p` does
    /// when one of the pids asked for is gone. Input: the command. Output: whether it
    /// succeeded and its text, or none when it did not start or finish in time.
    fn ran(command: &mut Command) -> Option<(bool, String)> {
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut child = crate::spawn_gate::spawn(command).ok()?;
        let Some(stdout) = child.stdout.take() else {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        };
        let (sent, got) = mpsc::channel();
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let read = stdout
                .take(TEXT_BYTES_MAX as u64 + 1)
                .read_to_end(&mut bytes);
            let _ = sent.send(read.map(|_| bytes));
        });
        let until = Instant::now() + RUN_WAIT;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break Some(status),
                Ok(None) if Instant::now() < until => std::thread::sleep(RUN_STEP),
                _ => break None,
            }
        };
        if status.is_none() {
            let _ = child.kill();
            let _ = child.wait();
        }
        let left = until.saturating_duration_since(Instant::now()) + RUN_STEP;
        let bytes = got.recv_timeout(left).ok()?.ok()?;
        if bytes.len() > TEXT_BYTES_MAX {
            return None;
        }
        let success = status?.success();
        Some((success, String::from_utf8(bytes).ok()?))
    }

    /// A system tool started with an empty environment from `/`. Inputs: the program and
    /// its arguments. Output: the command, not started.
    #[cfg(not(target_os = "linux"))]
    fn tool(program: &str, arguments: &[&str]) -> Command {
        assert!(program.starts_with('/'));
        let mut command = Command::new(program);
        command.args(arguments).env_clear().current_dir("/");
        command
    }

    /// Reads the process table: every process's pid, parent and uid, at most `TABLE_MAX`.
    /// Output: the rows, or `keep_awake_freeze_table` when it cannot be read.
    fn table() -> Result<Vec<Row>, EngineError> {
        #[cfg(target_os = "linux")]
        let rows = linux::table();
        #[cfg(not(target_os = "linux"))]
        let rows = run(&mut tool("/bin/ps", &["-axo", "pid=,ppid=,uid="])).map(|text| {
            text.lines()
                .take(TABLE_MAX)
                .filter_map(|line| {
                    let mut numbers = line.split_whitespace().map(str::parse::<u32>);
                    let row = Row {
                        pid: numbers.next()?.ok()?,
                        ppid: numbers.next()?.ok()?,
                        uid: numbers.next()?.ok()?,
                    };
                    numbers.next().is_none().then_some(row)
                })
                .collect::<Vec<Row>>()
        });
        let rows = rows.ok_or_else(|| error("keep_awake_freeze_table", "no process list"))?;
        assert!(rows.len() <= TABLE_MAX);
        Ok(rows)
    }

    /// The processes below a set of pids in the table, nearest first, at most `limit`, the
    /// starting pids not included. Inputs: the table, the pids and the limit. Output: the
    /// descendants' pids.
    fn below(rows: &[Row], starts: &[u32], limit: usize) -> Vec<u32> {
        let mut children: BTreeMap<u32, Vec<u32>> = BTreeMap::new();
        for row in rows.iter().filter(|row| row.pid > 1 && row.pid != row.ppid) {
            children.entry(row.ppid).or_default().push(row.pid);
        }
        let mut seen: BTreeSet<u32> = starts.iter().copied().collect();
        let mut found: Vec<u32> = Vec::new();
        let mut next = 0;
        let mut parents: Vec<u32> = starts.to_vec();
        while next < parents.len() && found.len() < limit {
            let parent = parents[next];
            next += 1;
            for child in children.get(&parent).into_iter().flatten() {
                if seen.insert(*child) {
                    found.push(*child);
                    parents.push(*child);
                }
            }
        }
        found.truncate(limit);
        assert!(found.len() <= limit);
        found
    }

    /// The processes above one pid in the table, itself included, up to pid 1. Inputs:
    /// the table and the pid. Output: the pids.
    fn above(rows: &[Row], pid: u32) -> BTreeSet<u32> {
        let parent: BTreeMap<u32, u32> = rows.iter().map(|row| (row.pid, row.ppid)).collect();
        let mut found = BTreeSet::from([pid]);
        let mut at = pid;
        for _ in 0..rows.len().min(TABLE_MAX) {
            match parent.get(&at) {
                Some(&up) if up > 0 && found.insert(up) => at = up,
                _ => break,
            }
        }
        found
    }

    /// What may never be frozen around one engine (rv-secreview T17 r1 F1, F3, F4), checked
    /// per process: another user's process, pid 1, the engine and every process above or
    /// below it, every process at or below an agent root the engine hands in (leader 01:57,
    /// until T17b: the tmux servers it knows), and any executable `kept_out` refuses.
    struct Fence {
        own: u32,
        owners: BTreeMap<u32, u32>,
        parents: BTreeMap<u32, u32>,
        upward: BTreeSet<u32>,
        downward: BTreeSet<u32>,
        inside: BTreeSet<u32>,
        here: Option<PathBuf>,
    }

    impl Fence {
        /// Reads the fence from one look at the process table. Inputs: the table, the
        /// engine pid and the agent roots. Output: the fence.
        fn new(rows: &[Row], engine: u32, roots: &BTreeSet<u32>) -> Self {
            assert!(engine > 1);
            assert!(roots.len() <= ROOTS_MAX);
            let starts: Vec<u32> = roots.iter().copied().collect();
            let mut inside: BTreeSet<u32> = roots.clone();
            inside.extend(below(rows, &starts, TABLE_MAX));
            Self {
                own: rustix::process::getuid().as_raw(),
                owners: rows.iter().map(|row| (row.pid, row.uid)).collect(),
                parents: rows.iter().map(|row| (row.pid, row.ppid)).collect(),
                upward: above(rows, engine),
                downward: below(rows, &[engine], TABLE_MAX).into_iter().collect(),
                inside,
                here: std::env::current_exe().ok(),
            }
        }

        /// Tells whether one process may be frozen. Inputs: its pid and the executable it
        /// runs. Output: true when nothing in the fence refuses it.
        fn may(&self, pid: u32, path: &Path) -> bool {
            pid > 1
                && self.owners.get(&pid) == Some(&self.own)
                && !self.upward.contains(&pid)
                && !self.downward.contains(&pid)
                && !self.inside.contains(&pid)
                && !kept_out(path, self.here.as_deref())
        }
    }

    /// The folder an app's own code lives in: its outermost `.app` bundle, else the folder
    /// of its executable, or none when that is a shared program folder, where the app's own
    /// code is its executable alone. Input: the app's executable. Output: the folder.
    fn home(path: &Path) -> Option<PathBuf> {
        let mut at = PathBuf::new();
        for part in path.components() {
            at.push(part);
            if part.as_os_str().to_string_lossy().ends_with(".app") {
                return Some(at);
            }
        }
        let folder = path.parent()?;
        let shared = SHARED.iter().any(|shared| folder == Path::new(shared));
        (!shared && folder.is_absolute()).then(|| folder.to_path_buf())
    }

    /// The executable a process runs, less the " (deleted)" Linux adds to a replaced file.
    fn exe(proc: &Proc) -> &str {
        proc.exe.trim_end_matches(" (deleted)")
    }

    /// Tells whether a pid is gone, asked with signal 0 (rv-secreview r1 F2): no such
    /// process means gone, and a process another user owns still runs. Input: the pid.
    /// Output: true when gone, false when it runs, or why it cannot tell.
    #[cfg(not(target_os = "linux"))]
    fn gone(pid: u32) -> Result<bool, EngineError> {
        let raw = i32::try_from(pid).map_err(|problem| error("keep_awake_freeze_pid", problem))?;
        let pid = rustix::process::Pid::from_raw(raw)
            .ok_or_else(|| error("keep_awake_freeze_pid", raw))?;
        match rustix::process::test_kill_process(pid) {
            Ok(()) | Err(rustix::io::Errno::PERM) => Ok(false),
            Err(rustix::io::Errno::SRCH) => Ok(true),
            Err(problem) => Err(error("keep_awake_freeze_identity", problem)),
        }
    }

    /// Reads who several processes are in one look (rv-secreview and rv-code T17 r1): a
    /// pid is missing only when it is positively gone. On macOS `ps` reads the start time in
    /// UTC, so a time zone change keeps it, and a pid missing from its answer is gone only
    /// when signal 0 says so; on Linux only a missing `/proc` entry means gone. Input: the
    /// pids. Output: the ones running, by pid, or `keep_awake_freeze_identity` when they
    /// cannot be read, which keeps a journal entry.
    fn identities(pids: &[u32]) -> Result<BTreeMap<u32, Proc>, EngineError> {
        assert!(pids.len() <= TREE_MAX + 1);
        if pids.is_empty() {
            return Ok(BTreeMap::new());
        }
        #[cfg(target_os = "linux")]
        {
            let mut found = BTreeMap::new();
            for pid in pids {
                if let Some(proc) = linux::identity(*pid)? {
                    found.insert(*pid, proc);
                }
            }
            Ok(found)
        }
        #[cfg(not(target_os = "linux"))]
        {
            let list: Vec<String> = pids.iter().map(u32::to_string).collect();
            let list = list.join(",");
            let mut command = tool("/bin/ps", &["-o", "pid=,lstart=,comm=", "-p", &list]);
            command.env("TZ", "UTC0");
            let (_, text) = ran(&mut command)
                .ok_or_else(|| error("keep_awake_freeze_identity", "ps did not answer"))?;
            let found: BTreeMap<u32, Proc> = text
                .lines()
                .filter_map(|line| {
                    let mut words = line.split_whitespace();
                    let pid: u32 = words.next()?.parse().ok()?;
                    let start: Vec<&str> = words.by_ref().take(5).collect();
                    let exe: Vec<&str> = words.collect();
                    (start.len() == 5 && !exe.is_empty() && pids.contains(&pid)).then(|| {
                        let found = Proc {
                            pid,
                            start: start.join(" "),
                            exe: exe.join(" "),
                        };
                        (pid, found)
                    })
                })
                .collect();
            for pid in pids {
                if !found.contains_key(pid) && !gone(*pid)? {
                    return Err(error("keep_awake_freeze_identity", pid));
                }
            }
            Ok(found)
        }
    }

    /// Reads who a running process is. Input: the pid. Output: its pid, start time and
    /// executable, `keep_awake_freeze_gone` when it is not running, or
    /// `keep_awake_freeze_identity` when it cannot be read.
    pub fn identity(pid: u32) -> Result<Proc, EngineError> {
        let found = identities(&[pid])?.remove(&pid);
        let found = found.ok_or_else(|| error("keep_awake_freeze_gone", pid))?;
        assert_eq!(found.pid, pid);
        assert!(!found.start.is_empty() && !found.exe.is_empty());
        Ok(found)
    }

    /// Reads the id of this boot session, which changes at every start of the computer.
    /// Output: the id, or `keep_awake_freeze_boot` when it cannot be read.
    pub fn boot_session() -> Result<String, EngineError> {
        #[cfg(target_os = "linux")]
        let text = std::fs::read_to_string("/proc/sys/kernel/random/boot_id").ok();
        #[cfg(not(target_os = "linux"))]
        let text = run(&mut tool(
            "/usr/sbin/sysctl",
            &["-n", "kern.bootsessionuuid"],
        ));
        let boot = text.map(|text| text.trim().to_owned()).unwrap_or_default();
        if boot.is_empty() || boot.len() > 64 || boot.chars().any(char::is_whitespace) {
            return Err(error("keep_awake_freeze_boot", "no boot session id"));
        }
        Ok(boot)
    }

    /// Tells whether a process read now is the one journaled: same start time and same
    /// executable (Linux marks a replaced file " (deleted)", which still matches).
    fn same(now: &Proc, then: &Proc) -> bool {
        now.pid == then.pid && now.start == then.start && exe(now) == exe(then)
    }

    /// Sends one signal to one pid. Inputs: the pid and the signal. Output: Ok, Ok for a
    /// pid that is gone, or why not.
    fn signal(pid: u32, signal: rustix::process::Signal) -> Result<(), EngineError> {
        assert!(pid > 1);
        let raw = i32::try_from(pid).map_err(|problem| error("keep_awake_freeze_pid", problem))?;
        let pid = rustix::process::Pid::from_raw(raw)
            .ok_or_else(|| error("keep_awake_freeze_pid", raw))?;
        match rustix::process::kill_process(pid, signal) {
            Ok(()) | Err(rustix::io::Errno::SRCH) => Ok(()),
            Err(problem) => Err(error("keep_awake_freeze_signal", problem)),
        }
    }

    /// Freezes one listed instance of an app (rv-secreview T17 r1 F1, F4; leader 01:57):
    /// refuses it unless the fence allows this process and it still runs the listed app's
    /// executable, then keeps each descendant the fence allows that runs the app's own code
    /// and whose parent is kept, so a shell, a system tool, CrazyAgent or an agent below
    /// the app keeps running with everything below it. It journals those processes (at most
    /// `PROCS_MAX`) with their identity and the boot session, synced, then sends each
    /// SIGSTOP once its identity is read again and still matches. Inputs: the lane journal,
    /// the listed instance, the engine pid and the agent roots. Output: the record
    /// journaled, or why the app was left running.
    pub fn freeze(
        journal: &mut Journal,
        listed: &Running,
        engine: u32,
        roots: &BTreeSet<u32>,
    ) -> Result<Frozen, EngineError> {
        let name = listed.app.name.as_str();
        let pid = listed.pid;
        assert!(!name.is_empty());
        if pid <= 1 || engine <= 1 {
            return Err(error("keep_awake_freeze_pid", pid));
        }
        let rows = table()?;
        let fence = Fence::new(&rows, engine, roots);
        if !fence.may(pid, &listed.path) || super::OWN_IDS.contains(&listed.app.id.as_str()) {
            return Err(error("keep_awake_freeze_refused", name));
        }
        let tree = below(&rows, &[pid], TREE_MAX);
        if tree.len() >= TREE_MAX {
            return Err(error("keep_awake_freeze_too_many", name));
        }
        let mut asked = vec![pid];
        asked.extend(&tree);
        let found = identities(&asked)?;
        let main = found
            .get(&pid)
            .ok_or_else(|| error("keep_awake_freeze_gone", pid))?;
        if Path::new(exe(main)) != listed.path {
            return Err(error("keep_awake_freeze_moved", name));
        }
        let home = home(&listed.path);
        let owns = |path: &Path| {
            home.as_ref()
                .map_or(path == listed.path, |home| path.starts_with(home))
        };
        let mut kept = BTreeSet::from([pid]);
        let mut procs = vec![main.clone()];
        for child in &tree {
            let Some(proc) = found.get(child) else {
                continue;
            };
            let path = Path::new(exe(proc));
            let parent = fence.parents.get(child).copied().unwrap_or(0);
            if kept.contains(&parent) && fence.may(*child, path) && owns(path) {
                kept.insert(*child);
                procs.push(proc.clone());
            }
        }
        if procs.len() > PROCS_MAX {
            return Err(error("keep_awake_freeze_too_many", name));
        }
        let frozen = Frozen {
            app: name.into(),
            boot: boot_session()?,
            procs,
        };
        journal.record(frozen.entry())?;
        let pids: Vec<u32> = frozen.procs.iter().map(|proc| proc.pid).collect();
        let now = identities(&pids)?;
        for proc in &frozen.procs {
            if now.get(&proc.pid).is_some_and(|now| same(now, proc)) {
                signal(proc.pid, rustix::process::Signal::STOP)?;
            }
        }
        assert!(!frozen.procs.is_empty() && frozen.procs.len() <= PROCS_MAX);
        assert_eq!(frozen.procs[0].pid, pid);
        Ok(frozen)
    }

    /// Thaws one journaled app with a given reader of who runs now: in the boot session it
    /// was frozen in, one read for all its processes, then SIGCONT to each whose start time
    /// and executable still match. A process positively gone or now another is never
    /// signalled; a read that fails returns its error, so the entry stays (rv-secreview r1
    /// F2, rv-code r1 F1, F3). A record of another boot is only forgotten. Inputs: a `KIND`
    /// entry and the reader. Output: Ok when nothing is left to thaw, or why not.
    pub fn thaw_with(
        entry: &Entry,
        read: impl Fn(&[u32]) -> Result<BTreeMap<u32, Proc>, EngineError>,
    ) -> Result<(), EngineError> {
        assert_eq!(entry.kind, KIND);
        let frozen: Frozen = serde_json::from_str(&entry.restore)
            .map_err(|problem| error("keep_awake_freeze_entry_invalid", problem))?;
        if frozen.procs.is_empty() || frozen.procs.len() > PROCS_MAX {
            return Err(error("keep_awake_freeze_entry_invalid", "process count"));
        }
        if frozen.boot != boot_session()? {
            return Ok(());
        }
        let pids: Vec<u32> = frozen.procs.iter().map(|proc| proc.pid).collect();
        let now = read(&pids)?;
        let mut first = None;
        for proc in &frozen.procs {
            if !now.get(&proc.pid).is_some_and(|now| same(now, proc)) {
                continue;
            }
            if let Err(problem) = signal(proc.pid, rustix::process::Signal::CONT) {
                first = first.or(Some(problem));
            }
        }
        first.map_or(Ok(()), Err)
    }

    /// Thaws one journaled app as `thaw_with` does, reading who runs now from the system.
    /// Input: a `KIND` entry. Output: Ok when nothing is left to thaw, or why not.
    pub fn thaw(entry: &Entry) -> Result<(), EngineError> {
        thaw_with(entry, identities)
    }

    /// Keeps the listed instances that may be frozen, each checked on its own process
    /// (rv-secreview T17 r1 F1): never CrazyAgent, the engine, a process above or below the
    /// engine or at or below an agent root, another user's process, pid 1, a system program
    /// or a desktop shell. Inputs: the listed instances, the engine pid and the agent
    /// roots. Output: the instances, in the order listed, or why the process list cannot be
    /// read.
    pub fn allowed(
        running: &[Running],
        engine: u32,
        roots: &BTreeSet<u32>,
    ) -> Result<Vec<Running>, EngineError> {
        let rows = table()?;
        let fence = Fence::new(&rows, engine, roots);
        let kept: Vec<Running> = running
            .iter()
            .take(RUNNING_MAX)
            .filter(|one| {
                fence.may(one.pid, &one.path)
                    && !super::OWN_IDS.contains(&one.app.id.as_str())
                    && super::validate_apps(std::slice::from_ref(&one.app)).is_ok()
            })
            .cloned()
            .collect();
        assert!(kept.len() <= RUNNING_MAX);
        Ok(kept)
    }

    /// The tmux servers this engine knows (leader 01:57, until T17b): the owner's tmux
    /// processes whose command line serves a socket in this engine's terminal folder or an
    /// agent's temp folder. Input: the engine root. Output: their pids, or why they cannot
    /// be read, which refuses every freeze.
    pub fn servers(root: &Path) -> Result<BTreeSet<u32>, EngineError> {
        let folders = super::socket_folders(root);
        #[cfg(target_os = "linux")]
        let found = linux::tmux().ok_or_else(|| error("keep_awake_freeze_table", "no tmux"))?;
        #[cfg(not(target_os = "linux"))]
        let found = tmux()?;
        if found.len() > SERVERS_MAX {
            return Err(error("keep_awake_freeze_table", "too many tmux processes"));
        }
        let known: BTreeSet<u32> = found
            .into_iter()
            .filter(|(_, args)| super::serves(args, &folders))
            .map(|(pid, _)| pid)
            .collect();
        assert!(known.len() <= SERVERS_MAX);
        Ok(known)
    }

    /// Lists the owner's tmux processes and their command lines: one `ps` for the names,
    /// one for the command lines of those. Output: pid and command line each, or why not.
    #[cfg(not(target_os = "linux"))]
    fn tmux() -> Result<Vec<(u32, String)>, EngineError> {
        let own = rustix::process::getuid().as_raw();
        let text = run(&mut tool("/bin/ps", &["-axo", "pid=,uid=,comm="]))
            .ok_or_else(|| error("keep_awake_freeze_table", "no process list"))?;
        let pids: Vec<u32> = text
            .lines()
            .take(TABLE_MAX)
            .filter_map(|line| {
                let mut words = line.split_whitespace();
                let pid: u32 = words.next()?.parse().ok()?;
                let uid: u32 = words.next()?.parse().ok()?;
                let comm = words.collect::<Vec<_>>().join(" ");
                let named = Path::new(&comm)
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with("tmux"));
                (uid == own && pid > 1 && named).then_some(pid)
            })
            .collect();
        if pids.is_empty() {
            return Ok(Vec::new());
        }
        if pids.len() > SERVERS_MAX {
            return Err(error("keep_awake_freeze_table", "too many tmux processes"));
        }
        let list: Vec<String> = pids.iter().map(u32::to_string).collect();
        let (_, text) = ran(&mut tool(
            "/bin/ps",
            &["-o", "pid=,args=", "-p", &list.join(",")],
        ))
        .ok_or_else(|| error("keep_awake_freeze_table", "no command lines"))?;
        let found: Vec<(u32, String)> = text
            .lines()
            .filter_map(|line| {
                let (pid, args) = line.trim_start().split_once(' ')?;
                let pid: u32 = pid.parse().ok()?;
                pids.contains(&pid).then(|| (pid, args.trim().to_owned()))
            })
            .collect();
        assert!(found.len() <= pids.len());
        Ok(found)
    }

    /// Keeps the running apps the owner may pick, one per id: the ids of `allowed`.
    /// Inputs: the listed apps, the engine pid and the agent roots. Output: the pickable
    /// apps by name, or why the process list cannot be read.
    pub fn pickable(
        running: &[Running],
        engine: u32,
        roots: &BTreeSet<u32>,
    ) -> Result<Vec<FreezeApp>, EngineError> {
        let mut picked: BTreeMap<String, FreezeApp> = BTreeMap::new();
        for one in allowed(running, engine, roots)? {
            picked.entry(one.app.id.clone()).or_insert(one.app);
        }
        let mut apps: Vec<FreezeApp> = picked.into_values().collect();
        apps.sort();
        assert!(apps.len() <= RUNNING_MAX);
        Ok(apps)
    }

    /// Lists the running apps: on macOS the foreground apps of the login session from
    /// `lsappinfo`, on Linux the owner's processes that match an installed desktop entry.
    /// Output: the apps, at most 512, or `keep_awake_freeze_list` when none can be read.
    pub fn running() -> Result<Vec<Running>, EngineError> {
        #[cfg(target_os = "linux")]
        let listed = linux::running();
        #[cfg(not(target_os = "linux"))]
        let listed = run(&mut tool("/usr/bin/lsappinfo", &["list"])).map(|text| apps_of(&text));
        let listed = listed.ok_or_else(|| error("keep_awake_freeze_list", "no app list"))?;
        assert!(listed.len() <= RUNNING_MAX);
        Ok(listed)
    }

    /// Reads `lsappinfo list` text: one block per app, kept when it is a foreground app
    /// with a bundle id, an executable path and a pid. Input: the text. Output: the apps.
    #[cfg(not(target_os = "linux"))]
    fn apps_of(text: &str) -> Vec<Running> {
        let quoted = |line: &str, key: &str| {
            let rest = line.trim().strip_prefix(key)?.strip_prefix("=\"")?;
            Some(rest.split_once('"')?.0.to_owned())
        };
        let mut apps = Vec::new();
        let mut name: Option<String> = None;
        let mut id: Option<String> = None;
        let mut path: Option<String> = None;
        for line in text.lines() {
            if apps.len() == RUNNING_MAX {
                break;
            }
            let trimmed = line.trim();
            let head = trimmed.split_once(") \"").filter(|(number, _)| {
                !number.is_empty() && number.bytes().all(|byte| byte.is_ascii_digit())
            });
            if let Some((_, rest)) = head {
                name = rest.split_once("\" ASN:").map(|(name, _)| name.to_owned());
                id = None;
                path = None;
            } else if let Some(found) = quoted(trimmed, "bundleID") {
                id = Some(found);
            } else if let Some(found) = quoted(trimmed, "executable path") {
                path = Some(found);
            } else if let Some(rest) = trimmed.strip_prefix("pid = ") {
                let pid = rest
                    .split_whitespace()
                    .next()
                    .and_then(|pid| pid.parse().ok());
                let foreground = rest.contains("type=\"Foreground\"");
                if let (Some(pid), true, Some(name), Some(id), Some(path)) =
                    (pid, foreground, name.take(), id.take(), path.take())
                {
                    apps.push(Running {
                        app: FreezeApp { name, id },
                        pid,
                        path: std::path::PathBuf::from(path),
                    });
                }
            }
        }
        assert!(apps.len() <= RUNNING_MAX);
        apps
    }

    #[cfg(target_os = "linux")]
    mod linux {
        use super::super::{EngineError, FreezeApp, Proc, RUNNING_MAX, Running, error, kept_out};
        use super::{Row, TABLE_MAX};
        use std::collections::BTreeMap;
        use std::path::{Path, PathBuf};

        const ENTRIES_MAX: usize = 4096;
        const ENTRY_BYTES_MAX: u64 = 64 * 1024;
        const ARGS_BYTES_MAX: u64 = 64 * 1024;

        /// The fields after a process's name in `/proc/<pid>/stat`, from the state on.
        fn stat(pid: u32) -> Option<Vec<String>> {
            let text = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
            let (_, rest) = text.rsplit_once(')')?;
            Some(rest.split_whitespace().map(str::to_owned).collect())
        }

        /// Reads the process table from `/proc`, at most `TABLE_MAX` rows.
        pub(super) fn table() -> Option<Vec<Row>> {
            use std::os::unix::fs::MetadataExt;
            let mut rows = Vec::new();
            for found in std::fs::read_dir("/proc")
                .ok()?
                .flatten()
                .take(TABLE_MAX * 2)
            {
                if rows.len() == TABLE_MAX {
                    break;
                }
                let Some(pid) = found
                    .file_name()
                    .to_str()
                    .and_then(|name| name.parse().ok())
                else {
                    continue;
                };
                let (Some(fields), Ok(meta)) = (stat(pid), found.metadata()) else {
                    continue;
                };
                let Some(ppid) = fields.get(1).and_then(|ppid| ppid.parse().ok()) else {
                    continue;
                };
                rows.push(Row {
                    pid,
                    ppid,
                    uid: meta.uid(),
                });
            }
            Some(rows)
        }

        /// Tells whether a `/proc` read failed because the process is gone: no such file
        /// or no such process. Input: the error. Output: true when gone.
        fn gone(problem: &std::io::Error) -> bool {
            problem.kind() == std::io::ErrorKind::NotFound
                || problem.raw_os_error() == Some(libc::ESRCH)
        }

        /// Reads who one process is: field 22 of its stat (start time in clock ticks since
        /// boot) and the executable it runs (rv-code T17 r1 F1). Output: none only when it
        /// is gone, or `keep_awake_freeze_identity` for any other failure, such as too many
        /// open files, which keeps a journal entry.
        pub(super) fn identity(pid: u32) -> Result<Option<Proc>, EngineError> {
            let unread = |problem: std::io::Error| error("keep_awake_freeze_identity", problem);
            let text = match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
                Ok(text) => text,
                Err(problem) if gone(&problem) => return Ok(None),
                Err(problem) => return Err(unread(problem)),
            };
            let start = text
                .rsplit_once(')')
                .and_then(|(_, rest)| rest.split_whitespace().nth(19))
                .ok_or_else(|| error("keep_awake_freeze_identity", "stat"))?
                .to_owned();
            let exe = match std::fs::read_link(format!("/proc/{pid}/exe")) {
                Ok(exe) => exe,
                Err(problem) if gone(&problem) => return Ok(None),
                Err(problem) => return Err(unread(problem)),
            };
            assert!(!start.is_empty());
            Ok(Some(Proc {
                pid,
                start,
                exe: exe.to_string_lossy().into_owned(),
            }))
        }

        /// Lists the owner's tmux processes and their command lines from `/proc`, each line
        /// at most 64 KiB, its words joined by spaces. Output: pid and command line each, or
        /// none when the process table cannot be read.
        pub(super) fn tmux() -> Option<Vec<(u32, String)>> {
            use std::io::Read;
            let own = rustix::process::getuid().as_raw();
            let mut found = Vec::new();
            for row in table()?.iter().filter(|row| row.uid == own && row.pid > 1) {
                let Ok(exe) = std::fs::read_link(format!("/proc/{}/exe", row.pid)) else {
                    continue;
                };
                let named = exe
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with("tmux"));
                if !named {
                    continue;
                }
                let Ok(file) = std::fs::File::open(format!("/proc/{}/cmdline", row.pid)) else {
                    continue;
                };
                let mut bytes = Vec::new();
                if file.take(ARGS_BYTES_MAX).read_to_end(&mut bytes).is_err() {
                    continue;
                }
                let args = String::from_utf8_lossy(&bytes).replace('\0', " ");
                found.push((row.pid, args.trim().to_owned()));
            }
            Some(found)
        }

        /// The folders desktop entries live in: the owner's, then the system's, then the
        /// Flatpak exports, as the desktop finds them.
        fn folders() -> Vec<PathBuf> {
            let home = std::env::var_os("HOME").map(PathBuf::from);
            let data_home = std::env::var_os("XDG_DATA_HOME")
                .map(PathBuf::from)
                .or_else(|| home.as_ref().map(|home| home.join(".local/share")));
            let data_dirs = std::env::var("XDG_DATA_DIRS")
                .unwrap_or_else(|_| "/usr/local/share:/usr/share".into());
            let mut folders: Vec<PathBuf> = data_home.into_iter().collect();
            folders.extend(data_dirs.split(':').take(16).map(PathBuf::from));
            folders.push(PathBuf::from("/var/lib/flatpak/exports/share"));
            folders.retain(|folder| folder.is_absolute());
            folders
                .into_iter()
                .map(|folder| folder.join("applications"))
                .collect()
        }

        /// The program name an `Exec` or `TryExec` value starts, past a leading `env` and
        /// its settings. Input: the value. Output: the file name, if any.
        fn program(value: &str) -> Option<String> {
            let mut words = value.split_whitespace().map(|word| word.trim_matches('"'));
            let mut first = words.next()?;
            if Path::new(first).file_name()? == "env" {
                first = words.find(|word| !word.contains('='))?;
            }
            Some(Path::new(first).file_name()?.to_string_lossy().into_owned())
        }

        /// Reads the shown desktop entries: program name to app name, at most
        /// `ENTRIES_MAX` files of at most 64 KiB each.
        fn entries() -> BTreeMap<String, String> {
            use std::io::Read;
            let mut names = BTreeMap::new();
            let mut seen = 0;
            for folder in folders() {
                let Ok(listing) = std::fs::read_dir(&folder) else {
                    continue;
                };
                for found in listing.flatten() {
                    seen += 1;
                    if seen > ENTRIES_MAX {
                        return names;
                    }
                    let path = found.path();
                    if path
                        .extension()
                        .is_none_or(|extension| extension != "desktop")
                    {
                        continue;
                    }
                    let mut text = String::new();
                    let Ok(file) = std::fs::File::open(&path) else {
                        continue;
                    };
                    if file
                        .take(ENTRY_BYTES_MAX)
                        .read_to_string(&mut text)
                        .is_err()
                    {
                        continue;
                    }
                    if let Some((program, name)) = entry(&text) {
                        names.entry(program).or_insert(name);
                    }
                }
            }
            names
        }

        /// Reads one desktop entry's main group. Input: the text. Output: its program name
        /// and app name, or none for an entry that is hidden or not an application.
        fn entry(text: &str) -> Option<(String, String)> {
            let mut group = false;
            let mut fields: BTreeMap<&str, &str> = BTreeMap::new();
            for line in text.lines() {
                let line = line.trim();
                if line.starts_with('[') {
                    group = line == "[Desktop Entry]";
                    continue;
                }
                if let (true, Some((key, value))) = (group, line.split_once('=')) {
                    fields.entry(key.trim()).or_insert(value.trim());
                }
            }
            let shown = fields.get("NoDisplay") != Some(&"true")
                && fields.get("Hidden") != Some(&"true")
                && fields.get("Type").is_none_or(|kind| *kind == "Application");
            let name = fields.get("Name").filter(|name| !name.is_empty())?;
            let program = fields
                .get("TryExec")
                .and_then(|value| program(value))
                .or_else(|| fields.get("Exec").and_then(|value| program(value)))?;
            shown.then(|| (program, (*name).to_owned()))
        }

        /// Lists the owner's processes whose executable matches a shown desktop entry, one
        /// per executable: the one whose parent does not run the same executable.
        pub(super) fn running() -> Option<Vec<Running>> {
            let names = entries();
            let own = rustix::process::getuid().as_raw();
            let rows = table()?;
            let exes: BTreeMap<u32, PathBuf> = rows
                .iter()
                .filter(|row| row.uid == own && row.pid > 1)
                .filter_map(|row| {
                    let exe = std::fs::read_link(format!("/proc/{}/exe", row.pid)).ok()?;
                    Some((row.pid, exe))
                })
                .collect();
            let mut apps: BTreeMap<PathBuf, Running> = BTreeMap::new();
            for row in &rows {
                if apps.len() == RUNNING_MAX {
                    break;
                }
                let Some(exe) = exes.get(&row.pid) else {
                    continue;
                };
                if exes.get(&row.ppid) == Some(exe) || kept_out(exe, None) {
                    continue;
                }
                let file = exe
                    .file_name()
                    .map(|file| file.to_string_lossy().into_owned());
                let Some(name) = file.and_then(|file| names.get(&file).cloned()) else {
                    continue;
                };
                let id = exe.to_string_lossy().into_owned();
                apps.entry(exe.clone()).or_insert(Running {
                    app: FreezeApp { name, id },
                    pid: row.pid,
                    path: exe.clone(),
                });
            }
            Some(apps.into_values().collect())
        }
    }
}

#[cfg(unix)]
mod lane {
    use super::{Cover, FreezeApp, Frozen, Journal, KIND, guard_command, mute};
    use std::collections::BTreeSet;
    use std::fs::File;
    use std::io::Write;
    use std::path::PathBuf;
    use std::process::{Child, Stdio};
    use std::sync::{Arc, Mutex, mpsc};
    use std::thread::JoinHandle;
    use std::time::{Duration, Instant};

    const GUARD_WAIT: Duration = Duration::from_secs(2);
    const GUARD_STEP: Duration = Duration::from_millis(10);
    const RETRY_FIRST: Duration = Duration::from_millis(500);
    const RETRY_MOST: Duration = Duration::from_secs(60);
    const NOTICE_AFTER: u32 = 3;
    const FROZEN_MAX: usize = 32;
    const _: () = assert!(FROZEN_MAX < super::journal::ENTRIES_MAX);

    /// One order to the lane thread.
    #[derive(Debug)]
    enum Order {
        Cover(Cover),
        Uncover,
        Stop,
    }

    /// The lid-closed lane (DEC-167 Task 17): a thread that freezes the chosen apps and
    /// mutes the sound while the lid is closed during a hold, and undoes both when told,
    /// off the team service thread. It alone writes the lid-closed journal while it runs,
    /// holding its lock, and keeps one thaw guard alive while anything is journaled. What
    /// it could not put back it tries again on its own while uncovered, and after three
    /// failed tries in a row it names those kinds for the owner's notice (rv-secreview and
    /// rv-code T17 r1).
    #[derive(Debug)]
    pub struct Lane {
        orders: mpsc::Sender<Order>,
        worker: Option<JoinHandle<()>>,
        stuck: Arc<Mutex<Vec<String>>>,
    }

    /// The lane thread's own state.
    struct Worker {
        root: PathBuf,
        program: PathBuf,
        sound: Option<mute::Tool>,
        engine: u32,
        held: Option<(File, Journal)>,
        guard: Option<(Child, std::io::PipeWriter)>,
        covered: bool,
        tries: u32,
        stuck: Arc<Mutex<Vec<String>>>,
    }

    impl Lane {
        /// Starts the lane thread. Inputs: the engine root, the CrazyAgent program that
        /// runs the thaw guard, the sound tool when this computer has one, and the engine
        /// pid. Output: the lane, or why its thread could not start.
        pub fn start(
            root: PathBuf,
            program: PathBuf,
            sound: Option<mute::Tool>,
            engine: u32,
        ) -> std::io::Result<Self> {
            assert!(root.is_absolute() && program.is_absolute());
            assert!(engine > 1);
            let (orders, inbox) = mpsc::channel();
            let stuck = Arc::new(Mutex::new(Vec::new()));
            let mut worker = Worker {
                root,
                program,
                sound,
                engine,
                held: None,
                guard: None,
                covered: false,
                tries: 0,
                stuck: Arc::clone(&stuck),
            };
            let worker = std::thread::Builder::new()
                .name("keep-awake-lid-closed".into())
                .spawn(move || {
                    loop {
                        let order = match worker.retry_in() {
                            Some(wait) => match inbox.recv_timeout(wait) {
                                Ok(order) => Some(order),
                                Err(mpsc::RecvTimeoutError::Timeout) => None,
                                Err(mpsc::RecvTimeoutError::Disconnected) => break,
                            },
                            None => match inbox.recv() {
                                Ok(order) => Some(order),
                                Err(_) => break,
                            },
                        };
                        match order {
                            Some(Order::Cover(cover)) => worker.cover(&cover),
                            Some(Order::Uncover) | None => worker.uncover(),
                            Some(Order::Stop) => break,
                        }
                    }
                    worker.uncover();
                })?;
            Ok(Self {
                orders,
                worker: Some(worker),
                stuck,
            })
        }

        /// Asks the lane to freeze and mute as the cover says. Input: the cover.
        pub fn cover(&self, cover: Cover) {
            assert!(cover.any());
            let _ = self.orders.send(Order::Cover(cover));
        }

        /// Asks the lane to thaw every app and put the sound back.
        pub fn uncover(&self) {
            let _ = self.orders.send(Order::Uncover);
        }

        /// The kinds of entries the lane could not put back after three tries in a row,
        /// for the owner's notice; empty once everything is back. Output: the kinds.
        pub fn stuck(&self) -> Vec<String> {
            self.stuck
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone()
        }

        /// Asks the lane to undo everything and end, without waiting.
        pub fn finish(&self) {
            let _ = self.orders.send(Order::Stop);
        }

        /// Waits for the lane thread to end, at most until the deadline; one still busy
        /// then is left to end by itself and the thaw guard covers what it leaves. Input:
        /// the deadline. Output: true when the thread ended.
        pub fn wait(&mut self, until: Instant) -> bool {
            let Some(worker) = self.worker.take() else {
                return true;
            };
            while !worker.is_finished() && Instant::now() < until {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            if !worker.is_finished() {
                self.worker = Some(worker);
                return false;
            }
            worker.join().is_ok()
        }
    }

    impl Worker {
        /// Takes the lock and opens the journal once; a failure is logged and tried again
        /// at the next cover. Output: the journal, or none.
        fn journal(&mut self) -> Option<&mut Journal> {
            if self.held.is_none() {
                let opened = super::lock(&self.root)
                    .and_then(|lock| super::journal(&self.root).map(|journal| (lock, journal)));
                match opened {
                    Ok(held) => self.held = Some(held),
                    Err(problem) => eprintln!("keep awake lid closed: {}", problem.0),
                }
            }
            self.held.as_mut().map(|(_, journal)| journal)
        }

        /// The kinds of the entries the journal holds now, each once. Output: the kinds.
        fn left(&self) -> Vec<String> {
            let kinds: BTreeSet<String> = self
                .held
                .iter()
                .flat_map(|(_, journal)| journal.entries())
                .map(|entry| entry.kind.clone())
                .collect();
            kinds.into_iter().collect()
        }

        /// The main pids of the apps the journal holds frozen, so a later cover never
        /// journals one again. Output: the pids.
        fn frozen(&self) -> BTreeSet<u32> {
            self.held
                .iter()
                .flat_map(|(_, journal)| journal.entries())
                .filter(|entry| entry.kind == KIND)
                .filter_map(|entry| serde_json::from_str::<Frozen>(&entry.restore).ok())
                .filter_map(|frozen| frozen.procs.first().map(|first| first.pid))
                .collect()
        }

        /// How long to wait before trying the entries left again: none while covered or
        /// with nothing left, else half a second doubling per failed try, at most a minute.
        /// Output: the wait, or none to wait for an order alone.
        fn retry_in(&self) -> Option<Duration> {
            if self.covered || self.left().is_empty() {
                return None;
            }
            let doubled = self.tries.saturating_sub(1).min(7);
            Some(RETRY_FIRST.saturating_mul(1 << doubled).min(RETRY_MOST))
        }

        /// Starts the thaw guard when none runs: the read end of a fresh pipe is its stdin,
        /// the write end stays here, so the guard sees the end of the pipe when the engine
        /// dies. Output: true when a guard runs.
        fn guarded(&mut self) -> bool {
            if let Some((child, _)) = self.guard.as_mut()
                && matches!(child.try_wait(), Ok(None))
            {
                return true;
            }
            self.guard = None;
            let started = crate::spawn_gate::guarded(std::io::pipe).and_then(|(reader, writer)| {
                let mut command = guard_command(&self.program, &self.root);
                command
                    .stdin(reader)
                    .stdout(Stdio::null())
                    .stderr(Stdio::null());
                crate::spawn_gate::spawn(&mut command).map(|child| (child, writer))
            });
            match started {
                Ok(guard) => self.guard = Some(guard),
                Err(problem) => eprintln!("keep awake lid closed: no thaw guard: {problem}"),
            }
            self.guard.is_some()
        }

        /// Freezes the chosen apps not frozen yet and mutes the sound once, each only with
        /// the journal open and a thaw guard running. Input: the cover.
        fn cover(&mut self, cover: &Cover) {
            self.covered = true;
            if !cover.any() || self.journal().is_none() || !self.guarded() {
                return;
            }
            if cover.freeze && !cover.apps.is_empty() {
                self.freeze(&cover.apps);
            }
            let muted = self.left().iter().any(|kind| kind == mute::KIND);
            if cover.mute && !muted {
                let Some(tool) = self.sound.clone() else {
                    return;
                };
                let Some(journal) = self.journal() else {
                    return;
                };
                if let Err(problem) = mute::mute(journal, &tool) {
                    eprintln!("keep awake mute: {}", problem.0);
                }
            }
        }

        /// Freezes each listed instance of a chosen app that may be frozen now, checked
        /// per process now, not when it was chosen (rv-secreview T17 r1 F1), and never one
        /// inside a tmux server this engine knows (leader 01:57, until T17b); at most
        /// `FROZEN_MAX` at once. Input: the chosen apps.
        fn freeze(&mut self, apps: &[FreezeApp]) {
            let engine = self.engine;
            let found = super::servers(&self.root).and_then(|roots| {
                let running = super::running()?;
                super::allowed(&running, engine, &roots).map(|allowed| (roots, allowed))
            });
            let (roots, allowed) = match found {
                Ok(found) => found,
                Err(problem) => {
                    eprintln!("keep awake freeze: {}", problem.0);
                    return;
                }
            };
            let mut frozen = self.frozen();
            let chosen = allowed
                .iter()
                .filter(|one| apps.iter().any(|app| app.id == one.app.id));
            for one in chosen {
                if frozen.contains(&one.pid) {
                    continue;
                }
                if frozen.len() >= FROZEN_MAX {
                    eprintln!("keep awake freeze: at most {FROZEN_MAX} apps at once");
                    return;
                }
                let Some(journal) = self.journal() else {
                    return;
                };
                match super::freeze(journal, one, engine, &roots) {
                    Ok(_) => {
                        frozen.insert(one.pid);
                    }
                    Err(problem) => eprintln!("keep awake freeze: {}", problem.0),
                }
            }
            assert!(frozen.len() <= FROZEN_MAX);
        }

        /// Thaws every app and puts the sound back, settles those entries, then tells the
        /// guard it may leave. Entries that fail stay: the lane tries them again while
        /// uncovered, names them for the notice after `NOTICE_AFTER` failed tries in a row,
        /// and keeps a thaw guard running for them.
        fn uncover(&mut self) {
            self.covered = false;
            if let Some((_, journal)) = self.held.as_mut() {
                if let Err(problem) = journal.settle(KIND, super::thaw) {
                    eprintln!("keep awake thaw: {}", problem.0);
                }
                let sound = self.sound.clone();
                let restored = journal.settle(mute::KIND, |entry| match sound.as_ref() {
                    Some(tool) => mute::restore(entry, tool),
                    None => mute::restore(entry, &mute::tool_for(entry)?),
                });
                if let Err(problem) = restored {
                    eprintln!("keep awake sound: {}", problem.0);
                }
            }
            let left = self.left();
            let named = if left.is_empty() {
                self.tries = 0;
                Some(Vec::new())
            } else {
                self.tries = self.tries.saturating_add(1);
                (self.tries >= NOTICE_AFTER).then(|| left.clone())
            };
            if let Some(named) = named {
                *self
                    .stuck
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()) = named;
            }
            if !left.is_empty() {
                self.guarded();
                return;
            }
            if let Some((mut child, mut writer)) = self.guard.take() {
                let _ = writer.write_all(b"d");
                drop(writer);
                let until = Instant::now() + GUARD_WAIT;
                while matches!(child.try_wait(), Ok(None)) && Instant::now() < until {
                    std::thread::sleep(GUARD_STEP);
                }
                if matches!(child.try_wait(), Ok(None)) {
                    eprintln!("keep awake lid closed: the thaw guard did not leave; stopped");
                    let _ = child.kill();
                    let _ = child.wait();
                }
            }
        }
    }
}

#[cfg(all(test, unix))]
mod tests;
