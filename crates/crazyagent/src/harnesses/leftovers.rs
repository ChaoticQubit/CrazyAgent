use crate::protocol::CredentialRef;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
#[cfg(unix)]
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

/// The plain code of a stop that left processes of a Full access agent running.
pub const LEFT: &str = "harness_full_access_left";
/// The plain code of a Full access stop whose processes could not be listed, so it is
/// neither confirmed clean nor names what still runs.
pub const UNCHECKED: &str = "harness_full_access_unchecked";
/// The plain code of a stop whose warden did not confirm its sandbox empty.
pub const UNCERTAIN: &str = "harness_cleanup_uncertain";
/// The plain code of a stop that left running, inside the agent's sandbox, processes of a
/// pane no warden empties, as a native lead's pane in its own server.
pub const SANDBOX_LEFT: &str = "harness_sandbox_left";
/// The plain code of such a sandboxed stop whose processes could not be listed.
pub const SANDBOX_UNCHECKED: &str = "harness_sandbox_unchecked";
/// The environment variable that carries a launch's [`LineageToken`] to every process it
/// starts.
pub const LINEAGE_VARIABLE: &str = "CRAZYAGENT_LINEAGE";
/// How often a lineage that follows a running agent reads the process table, through one
/// read shared by every lineage of the engine.
pub(crate) const FOLLOW_EVERY: Duration = Duration::from_secs(2);
/// The longest time between two tables a lineage takes, or the last and its stop's own read,
/// with nothing missed: shared reads up to [`FOLLOW_EVERY`] old, taken up to that far apart
/// (SECAUDIT-SETSID-LEFTOVER r2 L2).
#[cfg(unix)]
const FOLLOW_GAP: Duration = FOLLOW_EVERY.saturating_mul(2);
const TOKEN_BYTES: usize = 16;
/// The most bytes of an environment listing a stop reads, line by line, keeping of each line
/// that holds its token only the first `KEPT_FIELDS` fields.
#[cfg(unix)]
const ENVIRONMENT_BYTES_MAX: u64 = 256 * 1024 * 1024;
#[cfg(unix)]
const KEPT_FIELDS: usize = 8;

/// How long a stop waits, after the agent's group is killed, for the processes it found
/// before the kill to end before it names the ones still running.
#[cfg(unix)]
const SETTLE: Duration = Duration::from_secs(2);
#[cfg(unix)]
const SETTLE_POLL: Duration = Duration::from_millis(100);
/// How often a held account checks again whether the processes holding it have ended.
const HOLD_POLL: Duration = Duration::from_secs(2);
/// How long an account stays held for a stop whose processes could not be listed, when
/// no later listing works and none was named; the hold ends sooner once one works.
const UNCHECKED_HOLD_MAX: Duration = Duration::from_secs(10 * 60);
/// The most processes one report names; the rest are counted.
const NAMED_MAX: usize = 8;
#[cfg(unix)]
const PROGRAM_CHARS_MAX: usize = 32;
#[cfg(unix)]
const TABLE_ROWS_MAX: usize = 65_536;
/// How long one read of the process table may take before `ps` is killed, so a stalled
/// exec never delays the kill of a stopping agent.
#[cfg(unix)]
const TABLE_TIME: Duration = Duration::from_secs(1);
#[cfg(unix)]
const TABLE_BYTES_MAX: u64 = 4 * 1024 * 1024;
#[cfg(unix)]
const TABLE_POLL: Duration = Duration::from_millis(5);
/// How long past `TABLE_TIME` a stop waits for the listing thread to report a listing it
/// killed at the deadline.
#[cfg(unix)]
const TABLE_GRACE: Duration = Duration::from_millis(100);
/// The longest a Full access stop spends finding and checking what the agent left: one
/// listing before the kill, the listing of its token's processes after it and the
/// settling, whose last listing may start one poll after `SETTLE` ends.
#[cfg(unix)]
pub(crate) const STOP_TIME: Duration = TABLE_TIME
    .saturating_add(TABLE_GRACE)
    .saturating_mul(3)
    .saturating_add(SETTLE)
    .saturating_add(SETTLE_POLL);
/// No Full access stop looks for leftovers where no process table is read.
#[cfg(not(unix))]
pub(crate) const STOP_TIME: Duration = Duration::ZERO;
/// The command that lists every process with what the lineage needs of it.
#[cfg(unix)]
const PS_TABLE: &[&str] = &[
    "/bin/ps",
    "-A",
    "-ww",
    "-o",
    "pid=,ppid=,uid=,stat=,lstart=,comm=",
];
/// The command that lists every process with its environment as macOS shows it to its
/// user, the same as KERN_PROCARGS2.
#[cfg(target_os = "macos")]
const PS_ENVIRONMENT: &[&str] = &[
    "/bin/ps",
    "-A",
    "-ww",
    "-E",
    "-o",
    "pid=,uid=,stat=,lstart=,command=",
];

/// One hold of a Full access stop: the account it keeps, if any, the report naming what
/// still runs, and what the stop holds, such as the agent's credential copy and lease.
struct Held {
    account: Option<CredentialRef>,
    report: String,
    #[allow(dead_code)]
    kept: Box<dyn Send>,
}

/// Every account a Full access stop keeps now, by the order they were kept in, so a refused
/// sign-out can name them and the owner can let them go.
static HELD: Mutex<BTreeMap<u64, Held>> = Mutex::new(BTreeMap::new());
static NEXT_HELD: AtomicU64 = AtomicU64::new(0);

fn held() -> MutexGuard<'static, BTreeMap<u64, Held>> {
    HELD.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Lets go of every hold a Full access stop keeps on one account, while what it named may
/// still run, because the owner chose to sign the account out anyway. Input: the account.
/// Output: how many holds ended.
pub fn release(account: &CredentialRef) -> usize {
    let released: Vec<Held> = {
        let mut held = held();
        let keys: Vec<u64> = held
            .iter()
            .filter(|(_, hold)| hold.account.as_ref() == Some(account))
            .map(|(key, _)| *key)
            .collect();
        keys.iter().filter_map(|key| held.remove(key)).collect()
    };
    let count = released.len();
    drop(released);
    count
}

/// Names what keeps one account held after Full access stops, newest first. Input: the
/// account. Output: each hold's report.
pub fn held_by(account: &CredentialRef) -> Vec<String> {
    held()
        .values()
        .rev()
        .filter(|hold| hold.account.as_ref() == Some(account))
        .map(|hold| hold.report.clone())
        .collect()
}

/// Why a stop could not report every process of an agent ended.
#[derive(Debug)]
pub enum Unstopped {
    /// The run's warden did not confirm its sandbox empty.
    Uncertain,
    /// Processes of an agent no warden empties, at Full access or in a sandboxed pane its
    /// host ran without the keeper, still run after its stop, or could not be listed.
    Left(Leftovers),
}

impl Unstopped {
    /// Names the failure as a plain code. Input: the failure. Output: [`UNCERTAIN`], or the
    /// leftovers' [`LEFT`] or [`UNCHECKED`].
    pub fn code(&self) -> &'static str {
        match self {
            Self::Uncertain => UNCERTAIN,
            Self::Left(left) => left.code(),
        }
    }
}

/// A random value every process of one launch inherits in [`LINEAGE_VARIABLE`], so the
/// launch's stop finds a process that left its session and lost its parent while it kept
/// its environment (SECAUDIT-SETSID-LEFTOVER M1). Not a secret: any process of the owner's
/// user can read it. It only ever names processes, never signals one.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct LineageToken(String);

impl LineageToken {
    /// Draws a new token. Output: the token, or none when the system gives no randomness.
    pub fn new() -> Option<Self> {
        let mut bytes = [0u8; TOKEN_BYTES];
        getrandom::fill(&mut bytes).ok()?;
        let token: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
        assert!(Self::valid(&token));
        Some(Self(token))
    }

    /// Tells whether `text` has a token's shape: `TOKEN_BYTES` bytes as lowercase hex.
    /// Input: the text. Output: true only then.
    pub fn valid(text: &str) -> bool {
        text.len() == TOKEN_BYTES * 2
            && text
                .bytes()
                .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
    }

    /// The token as its variable's value. Output: the hex text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for LineageToken {
    type Error = &'static str;

    fn try_from(text: String) -> Result<Self, Self::Error> {
        if Self::valid(&text) {
            Ok(Self(text))
        } else {
            Err("lineage_token_invalid")
        }
    }
}

impl From<LineageToken> for String {
    fn from(token: LineageToken) -> Self {
        token.0
    }
}

/// One process of an agent's lineage: its id, the start identity a reused id does not
/// share, and its program's name as the owner reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Process {
    pid: i32,
    started: String,
    program: String,
}

/// The processes of a Full access agent the engine can still find: its leader, every
/// process in the leader's session while that leader runs, every process whose parent is
/// one of them, and every process whose environment holds the launch's `token`. A process
/// that left all of them before it was seen, such as a daemon whose parent already exited
/// and whose environment was cleared or is not shown, is not found. `table` is the command
/// that lists the processes; `unchecked` tells that a listing failed, so the lineage may
/// miss processes; `pending` is a listing still held up past its bound, which a later read
/// waits out instead of starting another; `sandboxed` tells the agent ran in a sandbox no
/// warden empties, not at Full access; `leader_started` is the leader's start at the first
/// read; `applied` is when the last table it took was read and `missed` tells two of them, or
/// the last and the stop's own read, lay more than [`FOLLOW_GAP`] apart.
#[derive(Debug)]
pub(crate) struct Lineage {
    #[cfg(unix)]
    leader: i32,
    known: Vec<Process>,
    #[cfg(unix)]
    table: &'static [&'static str],
    unchecked: bool,
    #[cfg(unix)]
    pending: Option<Listing>,
    sandboxed: bool,
    #[cfg(unix)]
    token: Option<LineageToken>,
    #[cfg(unix)]
    leader_started: Option<String>,
    #[cfg(unix)]
    applied: Instant,
    #[cfg(unix)]
    missed: bool,
}

/// Where a listing thread reports its output, or none when it failed or ran out of bounds.
#[cfg(unix)]
type Listing = std::sync::mpsc::Receiver<Option<Vec<u8>>>;

/// One row of a process table: the process, its parent's id and its session's id, if read.
#[cfg(unix)]
type Row = (Process, i32, Option<i32>);

/// One shared read of the process table: when it was read, and its rows.
#[cfg(unix)]
type Table = (Instant, Arc<Vec<Row>>);

/// The process table every following lineage reads from: the last read with when it was
/// taken, whether a caller reads a new one now, and a listing still held up past its bound.
#[cfg(unix)]
struct Shared {
    read: Option<Table>,
    reading: bool,
    pending: Option<Listing>,
}

/// One table read for the whole engine per [`FOLLOW_EVERY`], so a followed agent costs a
/// scan of memory instead of a `ps` of its own (SECAUDIT-SETSID-LEFTOVER r1 M1).
#[cfg(unix)]
static SHARED: Mutex<Shared> = Mutex::new(Shared {
    read: None,
    reading: false,
    pending: None,
});

/// The processes an agent no warden empties left running after its stop, at Full access
/// or in a sandboxed pane its host ran without the keeper, or a stop whose processes could
/// not be listed; never both empty and checked.
#[derive(Debug)]
pub struct Leftovers(Box<Lineage>);

impl Leftovers {
    /// Names the leftovers as a plain code. Input: the leftovers. Output: [`UNCHECKED`] when
    /// none is named because the listing failed, else [`LEFT`]; [`SANDBOX_UNCHECKED`] and
    /// [`SANDBOX_LEFT`] for a sandboxed pane's.
    pub fn code(&self) -> &'static str {
        match (self.0.sandboxed, self.0.known.is_empty()) {
            (false, true) => UNCHECKED,
            (false, false) => LEFT,
            (true, true) => SANDBOX_UNCHECKED,
            (true, false) => SANDBOX_LEFT,
        }
    }

    /// Says plainly which processes still run and that Full access, or a sandbox no warden
    /// empties, cannot stop them, or that they could not be listed. Input: the leftovers. Output: one sentence naming at
    /// most `NAMED_MAX` of them by id and program, counting the rest.
    pub fn plain(&self) -> String {
        assert!(!self.0.known.is_empty() || self.0.unchecked);
        if self.0.known.is_empty() {
            let unlisted = if self.0.sandboxed {
                "The engine could not list what the agent started outside its terminal after its stop, and no warden empties its sandbox"
            } else {
                "Full access could not list what the agent started outside its terminal after its stop"
            };
            return format!(
                "{unlisted}, so it cannot confirm they ended. Its account stays held until they can be checked, at most {} minutes, or until you sign it out",
                UNCHECKED_HOLD_MAX.as_secs() / 60
            );
        }
        let named: Vec<String> = self
            .0
            .known
            .iter()
            .take(NAMED_MAX)
            .map(|process| format!("{} {}", process.pid, process.program))
            .collect();
        let more = self.0.known.len() - named.len();
        let counted = if more == 0 {
            String::new()
        } else {
            format!(" and {more} more")
        };
        let unlisted = if self.0.unchecked {
            ". More of them may run: the process list could not be read"
        } else {
            ""
        };
        let running = if self.0.sandboxed {
            "No warden empties this agent's sandbox, so what it started outside its terminal outlived its stop; still running in its sandbox"
        } else {
            "Full access cannot stop what the agent started outside its terminal; still running"
        };
        format!(
            "{running}: {}{counted}{unlisted}. Its account stays held until they end or you sign it out",
            named.join(", ")
        )
    }

    /// Keeps `kept`, such as the agent's credential copy and account lease, listed under
    /// `account` with this report, until a listing shows no process of the agent running or
    /// the owner signs the account out ([`release`]), then drops it; the engine's exit ends
    /// the hold too. With none named, it ends after `UNCHECKED_HOLD_MAX` even while no
    /// listing works. When no watching thread starts, only the owner's sign-out ends it.
    /// Inputs: the leftovers, the account they keep, if any, and what they hold.
    /// Output: none.
    pub fn hold<T: Send + 'static>(self, account: Option<CredentialRef>, kept: T) {
        let key = NEXT_HELD.fetch_add(1, Ordering::Relaxed);
        let report = self.plain();
        let previous = held().insert(
            key,
            Held {
                account,
                report,
                kept: Box::new(kept),
            },
        );
        assert!(previous.is_none());
        let mut lineage = *self.0;
        let _ = std::thread::Builder::new()
            .name("full-access-hold".into())
            .spawn(move || {
                let begun = Instant::now();
                while held().contains_key(&key) {
                    let checked = lineage.refresh().is_ok();
                    if lineage.known.is_empty()
                        && (checked || begun.elapsed() >= UNCHECKED_HOLD_MAX)
                    {
                        break;
                    }
                    std::thread::sleep(HOLD_POLL);
                }
                let ended = held().remove(&key);
                drop(ended);
            });
    }
}

impl Lineage {
    /// Finds a Full access agent's processes before its group is killed. Inputs: the
    /// group's leader, whose id is also its session's, and the launch's token, if it gave
    /// one. Output: the lineage, marked unchecked when the processes cannot be listed, or
    /// [`Unstopped::Uncertain`] for a leader that is no process id.
    #[cfg(unix)]
    pub(crate) fn of(leader: u32, token: Option<LineageToken>) -> Result<Self, Unstopped> {
        Self::reading(leader, PS_TABLE, token)
    }

    /// Finds, before its group is killed, the processes of a sandboxed pane no warden
    /// empties, such as a native lead's in its own server, as [`Self::of`] does. Inputs: the
    /// pane's first process, whose id is also its session's, and the launch's token.
    /// Output: as for [`Self::of`].
    pub(crate) fn of_sandboxed(
        leader: u32,
        token: Option<LineageToken>,
    ) -> Result<Self, Unstopped> {
        Self::of(leader, token).map(|lineage| Self {
            sandboxed: true,
            ..lineage
        })
    }

    /// Takes the engine's shared table while the agent runs, so a process that later leaves
    /// the leader's session and loses its parent stays known (SECAUDIT-SETSID-LEFTOVER). A
    /// follow that finds another caller reading skips; a failed read keeps what was known.
    /// Input: the lineage. Output: none.
    pub(crate) fn follow(&mut self) {
        #[cfg(unix)]
        if let Some(read) = shared_rows() {
            self.noted(
                read.as_ref()
                    .map(|(at, rows)| (*at, rows.as_slice()))
                    .map_err(|_| ()),
            );
        }
    }

    /// Reads the process table itself just before the stop kills the agent's group. The
    /// lineage is unchecked when that read fails, the tables it took lay more than
    /// [`FOLLOW_GAP`] apart, since a process could start and leave unseen in that time
    /// (SECAUDIT-SETSID-LEFTOVER r2 L2), or the leader's start is unknown, so its session
    /// was never followed; an earlier unchecked mark stays (r2 L1). Input: the lineage.
    #[cfg(unix)]
    pub(crate) fn before_stop(&mut self) {
        let read = self.refresh();
        self.missed |= Instant::now().saturating_duration_since(self.applied) > FOLLOW_GAP;
        self.unchecked |= read.is_err() || self.missed || self.leader_started.is_none();
    }

    /// Marks the lineage unchecked where no process table is read. Input: the lineage.
    /// Output: none.
    #[cfg(not(unix))]
    pub(crate) fn before_stop(&mut self) {
        self.unchecked = true;
    }

    /// Takes one followed read: applies a table it got, and marks the lineage missed when that
    /// table was read more than [`FOLLOW_GAP`] after the last one it took. A failed read keeps
    /// what was known. Inputs: the lineage and the read with when its table was read.
    #[cfg(unix)]
    fn noted(&mut self, read: Result<(Instant, &[Row]), ()>) {
        if let Ok((at, rows)) = read {
            self.missed |= at.saturating_duration_since(self.applied) > FOLLOW_GAP;
            self.applied = self.applied.max(at);
            self.apply(rows);
        }
    }

    /// Finds a lineage as [`Self::of`] does with the processes listed by `table`, keeping the
    /// leader's start from this first read. Inputs: the leader, the listing command and the
    /// token. Output: as for [`Self::of`].
    #[cfg(unix)]
    fn reading(
        leader: u32,
        table: &'static [&'static str],
        token: Option<LineageToken>,
    ) -> Result<Self, Unstopped> {
        let leader = i32::try_from(leader)
            .ok()
            .filter(|leader| *leader > 1)
            .ok_or(Unstopped::Uncertain)?;
        let mut lineage = Self {
            leader,
            known: Vec::new(),
            table,
            unchecked: false,
            pending: None,
            sandboxed: false,
            token,
            leader_started: None,
            applied: Instant::now(),
            missed: false,
        };
        match rows(table, &mut lineage.pending) {
            Ok(rows) => {
                lineage.leader_started = rows
                    .iter()
                    .find(|(process, ..)| process.pid == leader)
                    .map(|(process, ..)| process.started.clone());
                lineage.apply(&rows);
            }
            Err(_) => lineage.unchecked = true,
        }
        Ok(lineage)
    }

    /// Refuses where no process table is read; no Full access agent runs there.
    /// Input: unused. Output: [`Unstopped::Uncertain`].
    #[cfg(not(unix))]
    pub(crate) fn of(_leader: u32, _token: Option<LineageToken>) -> Result<Self, Unstopped> {
        Err(Unstopped::Uncertain)
    }

    /// Adds, after the group is killed, every process whose environment holds the launch's
    /// token, from one listing, then waits up to `SETTLE` for every process found to end.
    /// Input: the lineage. Output: Ok once a listing shows none running, else the
    /// [`Leftovers`] still running at the deadline or, once a listing failed, unchecked.
    #[cfg(unix)]
    pub(crate) fn settle(mut self) -> Result<(), Unstopped> {
        if let Some(token) = &self.token {
            match carrying(token, &mut self.pending) {
                Ok(found) => {
                    for (pid, started) in found {
                        let process = Process {
                            pid,
                            started,
                            program: String::new(),
                        };
                        if !self.known.iter().any(|known| same(known, &process)) {
                            self.known.push(process);
                        }
                    }
                }
                Err(_) => self.unchecked = true,
            }
        }
        let until = Instant::now() + SETTLE;
        loop {
            self.unchecked |= self.refresh().is_err();
            if self.known.is_empty() && !self.unchecked {
                return Ok(());
            }
            if self.known.is_empty() || Instant::now() >= until {
                return Err(Unstopped::Left(Leftovers(Box::new(self))));
            }
            std::thread::sleep(SETTLE_POLL);
        }
    }

    /// Refuses where no process table is read. Input: unused. Output: uncertain.
    #[cfg(not(unix))]
    pub(crate) fn settle(self) -> Result<(), Unstopped> {
        Err(Unstopped::Uncertain)
    }

    /// Reads the process table again and applies it. Input: the lineage. Output: Ok, or
    /// uncertain when `ps` cannot be read.
    #[cfg(unix)]
    fn refresh(&mut self) -> Result<(), Unstopped> {
        let rows = rows(self.table, &mut self.pending)?;
        self.apply(&rows);
        Ok(())
    }

    /// Applies one table read: keeps each known process still running with the same start,
    /// under the program it runs now, then adds this user's processes in the leader's session
    /// and those whose parent is kept. Session members join only while the leader runs with
    /// the start its first read found, so a stranger that reuses a dead leader's id never
    /// joins (SECAUDIT-SETSID-LEFTOVER r1 L1). Inputs: the lineage and the rows. Output: none.
    #[cfg(unix)]
    fn apply(&mut self, rows: &[Row]) {
        let session = self.leader_started.as_ref().is_some_and(|started| {
            rows.iter()
                .any(|(process, ..)| process.pid == self.leader && process.started == *started)
        });
        let mut kept: Vec<Process> = rows
            .iter()
            .filter(|(process, ..)| self.known.iter().any(|known| same(known, process)))
            .map(|(process, ..)| process.clone())
            .collect();
        loop {
            let found = rows.iter().find(|(process, parent, member)| {
                !kept.iter().any(|known| known.pid == process.pid)
                    && ((session && *member == Some(self.leader))
                        || kept.iter().any(|known| known.pid == *parent))
            });
            match found {
                Some((process, ..)) => kept.push(process.clone()),
                None => break,
            }
        }
        assert!(kept.len() <= rows.len());
        self.known = kept;
    }

    /// Refuses where no process table is read. Input: unused. Output: uncertain.
    #[cfg(not(unix))]
    fn refresh(&mut self) -> Result<(), Unstopped> {
        Err(Unstopped::Uncertain)
    }
}

/// Tells whether two rows name the same process: the same id with the same start, whatever
/// program it runs now. Inputs: the rows. Output: true only then.
#[cfg(unix)]
fn same(known: &Process, now: &Process) -> bool {
    known.pid == now.pid && known.started == now.started
}

/// Reads this user's running processes, other than the engine, with the listing command;
/// a zombie has ended and is left out. Inputs: the command, [`PS_TABLE`] outside tests,
/// and the lineage's listing still held up, if any. Output: each process with its parent's
/// id, or uncertain when the listing fails, is out of bounds or is still held up.
#[cfg(unix)]
fn table(
    argv: &'static [&'static str],
    pending: &mut Option<Listing>,
) -> Result<Vec<(Process, i32)>, Unstopped> {
    let output = read_bounded(argv, pending, None)?;
    let listing = String::from_utf8_lossy(&output);
    let user = rustix::process::getuid().as_raw();
    let own = i32::try_from(std::process::id()).map_err(|_| Unstopped::Uncertain)?;
    parse(&listing, user, own)
}

/// Reads the process table with `argv` and the session of each listed process. Inputs: the
/// command and the pending listing. Output: the rows, or uncertain when the listing fails.
#[cfg(unix)]
fn rows(
    argv: &'static [&'static str],
    pending: &mut Option<Listing>,
) -> Result<Vec<Row>, Unstopped> {
    Ok(table(argv, pending)?
        .into_iter()
        .map(|(process, parent)| {
            let session = rustix::process::Pid::from_raw(process.pid)
                .and_then(|pid| rustix::process::getsid(Some(pid)).ok())
                .map(|session| session.as_raw_nonzero().get());
            (process, parent, session)
        })
        .collect())
}

/// The table every following lineage shares: the last read while it is younger than
/// [`FOLLOW_EVERY`], else a new one, which one caller reads while the others skip.
/// Output: the rows with when they were read, a failed read, or none while another reads.
#[cfg(unix)]
fn shared_rows() -> Option<Result<Table, Unstopped>> {
    let locked = || SHARED.lock().unwrap_or_else(PoisonError::into_inner);
    let (_reading, mut pending) = {
        let mut shared = locked();
        if let Some((at, rows)) = &shared.read
            && at.elapsed() < FOLLOW_EVERY
        {
            return Some(Ok((*at, Arc::clone(rows))));
        }
        if shared.reading {
            return None;
        }
        shared.reading = true;
        (Reading, shared.pending.take())
    };
    let read = rows(PS_TABLE, &mut pending).map(|rows| (Instant::now(), Arc::new(rows)));
    let mut shared = locked();
    assert!(shared.reading);
    shared.pending = pending;
    if let Ok((at, rows)) = &read {
        shared.read = Some((*at, Arc::clone(rows)));
    }
    drop(shared);
    Some(read)
}

/// Held by the caller that reads the shared table; dropping it, after the read or while a
/// panic unwinds out of it, lets the next follow read again, so following never stops for
/// the whole engine (SECAUDIT-SETSID-LEFTOVER r2 L3).
#[cfg(unix)]
struct Reading;

#[cfg(unix)]
impl Drop for Reading {
    fn drop(&mut self) {
        SHARED
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .reading = false;
    }
}

/// Finds this user's running processes, other than the engine, whose environment holds the
/// exact word `CRAZYAGENT_LINEAGE=<token>`, from one listing of every process's environment
/// as macOS shows it to its user. Only each match's id and start are kept, never environment
/// text. macOS shows no environment of its own platform programs, so those are not found
/// this way. Inputs: the token and the pending listing. Output: each match, or uncertain.
#[cfg(target_os = "macos")]
fn carrying(
    token: &LineageToken,
    pending: &mut Option<Listing>,
) -> Result<Vec<(i32, String)>, Unstopped> {
    let word = format!("{LINEAGE_VARIABLE}={}", token.as_str());
    let output = read_bounded(PS_ENVIRONMENT, pending, Some(word))?;
    let listing = String::from_utf8_lossy(&output);
    let user = rustix::process::getuid().as_raw();
    let own = i32::try_from(std::process::id()).map_err(|_| Unstopped::Uncertain)?;
    let found: Vec<(i32, String)> = listing
        .lines()
        .filter_map(|line| {
            let fields: Vec<&str> = line.split_whitespace().collect();
            let pid = fields.first()?.parse::<i32>().ok()?;
            let owner = fields.get(1)?.parse::<u32>().ok()?;
            (fields.len() == KEPT_FIELDS
                && owner == user
                && pid != own
                && pid > 1
                && !fields[2].starts_with('Z'))
            .then(|| (pid, fields[3..KEPT_FIELDS].join(" ")))
        })
        .take(TABLE_ROWS_MAX)
        .collect();
    Ok(found)
}

/// Finds no process by its environment where none is listed here: Linux reads no token yet.
/// Inputs: unused. Output: none found.
#[cfg(all(unix, not(target_os = "macos")))]
fn carrying(
    _token: &LineageToken,
    _pending: &mut Option<Listing>,
) -> Result<Vec<(i32, String)>, Unstopped> {
    Ok(Vec::new())
}

/// Runs a listing command with a cleared environment in a thread of its own and takes its
/// output, at most `TABLE_BYTES_MAX` bytes within `TABLE_TIME` counted from before the
/// spawn, so neither a spawn held up by a stalled exec nor a stalled or flooding listing
/// holds up a stop. With `keep`, only what [`kept_lines`] keeps of the lines holding it is
/// taken. A listing held up past its bound stays `pending`, and no other starts until it
/// has ended, so a lasting stall holds up one listing thread, not one per read. Inputs: the
/// program with its arguments, the pending listing and the word to keep lines by, if any.
/// Output: the output of a run that exited 0 within both bounds, or uncertain.
#[cfg(unix)]
fn read_bounded(
    argv: &'static [&'static str],
    pending: &mut Option<Listing>,
    keep: Option<String>,
) -> Result<Vec<u8>, Unstopped> {
    use std::sync::mpsc::{RecvTimeoutError, TryRecvError};
    if let Some(listing) = pending {
        if matches!(listing.try_recv(), Err(TryRecvError::Empty)) {
            return Err(Unstopped::Uncertain);
        }
        *pending = None;
    }
    let deadline = Instant::now() + TABLE_TIME;
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("lineage-table".into())
        .spawn(move || {
            let _ = sender.send(run_listing(argv, deadline, keep));
        })
        .map_err(|_| Unstopped::Uncertain)?;
    match receiver.recv_timeout(deadline.saturating_duration_since(Instant::now()) + TABLE_GRACE) {
        Ok(listing) => listing.ok_or(Unstopped::Uncertain),
        Err(RecvTimeoutError::Timeout) => {
            *pending = Some(receiver);
            Err(Unstopped::Uncertain)
        }
        Err(RecvTimeoutError::Disconnected) => Err(Unstopped::Uncertain),
    }
}

/// Starts a listing command and reads its output until `deadline`, killing it past the
/// deadline or `TABLE_BYTES_MAX`, a child whose spawn returned late included. With `keep`,
/// only what [`kept_lines`] keeps is taken. Inputs: the program with its arguments, the
/// deadline and the word to keep lines by, if any. Output: the output of a run that exited
/// 0 within both bounds, or none.
#[cfg(unix)]
fn run_listing(argv: &[&str], deadline: Instant, keep: Option<String>) -> Option<Vec<u8>> {
    use std::io::Read;
    use std::process::{Command, Stdio};
    let (program, arguments) = argv.split_first()?;
    let mut child = crate::spawn_gate::spawn(
        Command::new(program)
            .args(arguments)
            .env_clear()
            .env("LC_ALL", "C")
            .env("TZ", "UTC0")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null()),
    )
    .ok()?;
    let read = child.stdout.take().and_then(|stdout| {
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::Builder::new()
            .name("lineage-table-read".into())
            .spawn(move || {
                let mut bytes = Vec::new();
                let read = match keep {
                    None => stdout
                        .take(TABLE_BYTES_MAX + 1)
                        .read_to_end(&mut bytes)
                        .map(|_| ()),
                    Some(word) => kept_lines(stdout, word.as_bytes(), &mut bytes),
                };
                let _ = sender.send(read.map(|()| bytes));
            })
            .ok()?;
        receiver
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .ok()?
            .ok()
            .filter(|bytes| {
                u64::try_from(bytes.len()).is_ok_and(|length| length <= TABLE_BYTES_MAX)
            })
    });
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if read.is_some() && Instant::now() < deadline => {
                std::thread::sleep(TABLE_POLL);
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    match (read, status) {
        (Some(bytes), Some(status)) if status.success() => Some(bytes),
        _ => None,
    }
}

/// Reads a listing line by line, at most `ENVIRONMENT_BYTES_MAX` bytes, and keeps of each
/// line that holds `word` as a whole field only its first `KEPT_FIELDS` fields, so no
/// environment text is kept. Inputs: the listing, the word and where to keep. Output: Ok,
/// or the read's error, an overlong listing included.
#[cfg(unix)]
fn kept_lines(listing: impl std::io::Read, word: &[u8], kept: &mut Vec<u8>) -> std::io::Result<()> {
    use std::io::BufRead;
    let mut lines = std::io::BufReader::new(listing.take(ENVIRONMENT_BYTES_MAX + 1));
    let (mut line, mut total) = (Vec::new(), 0u64);
    loop {
        line.clear();
        let read = lines.read_until(b'\n', &mut line)?;
        if read == 0 {
            return Ok(());
        }
        total = total.saturating_add(u64::try_from(read).unwrap_or(u64::MAX));
        if total > ENVIRONMENT_BYTES_MAX {
            return Err(std::io::Error::other("listing_too_long"));
        }
        let fields = line
            .split(u8::is_ascii_whitespace)
            .filter(|field| !field.is_empty());
        if fields.clone().any(|field| field == word) {
            for field in fields.take(KEPT_FIELDS) {
                kept.extend_from_slice(field);
                kept.push(b' ');
            }
            kept.push(b'\n');
        }
    }
}

/// Picks from a `ps -o pid=,ppid=,uid=,stat=,lstart=,comm=` listing the processes of `user`
/// other than `own` that are not zombies. Inputs: the listing, the user id and the engine's
/// id. Output: each process with its parent's id, or uncertain past `TABLE_ROWS_MAX` rows.
#[cfg(unix)]
fn parse(listing: &str, user: u32, own: i32) -> Result<Vec<(Process, i32)>, Unstopped> {
    let mut rows = Vec::new();
    for line in listing.lines() {
        if rows.len() == TABLE_ROWS_MAX {
            return Err(Unstopped::Uncertain);
        }
        let fields: Vec<&str> = line.split_whitespace().collect();
        let (Some(pid), Some(parent), Some(owner)) = (
            fields.first().and_then(|field| field.parse::<i32>().ok()),
            fields.get(1).and_then(|field| field.parse::<i32>().ok()),
            fields.get(2).and_then(|field| field.parse::<u32>().ok()),
        ) else {
            continue;
        };
        if fields.len() < 10
            || owner != user
            || pid == own
            || pid <= 1
            || fields[3].starts_with('Z')
        {
            continue;
        }
        let path = fields[9..].join(" ");
        let program: String = crate::memory::visible_text(
            &path
                .rsplit('/')
                .next()
                .unwrap_or_default()
                .chars()
                .filter(|character| !character.is_control())
                .collect::<String>(),
        )
        .chars()
        .take(PROGRAM_CHARS_MAX)
        .collect();
        rows.push((
            Process {
                pid,
                started: fields[4..9].join(" "),
                program,
            },
            parent,
        ));
    }
    Ok(rows)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    const LISTING: &str = "  501   1 501 Ss   Wed Oct 28 03:10:00 2026     /usr/bin/perl\n\
        602 501 501 S Wed Oct 28 03:10:01 2026 /Applications/My Tool.app/Contents/MacOS/my tool\n\
        603 501   0 S Wed Oct 28 03:10:01 2026 /usr/sbin/root-owned\n\
        604 501 501 R+ Wed Oct 28 03:10:01 2026 /tmp/\u{202e}evil\n\
        605 501 501 Z Wed Oct 28 03:10:01 2026 <defunct>\n\
        700   1 501 S Wed Oct 28 03:10:02 2026 /bin/engine\n\
        garbage line\n";

    #[test]
    fn the_table_keeps_this_users_other_running_processes_with_visible_program_names() {
        let rows = parse(LISTING, 501, 700).unwrap();
        let named: Vec<(i32, i32, &str, &str)> = rows
            .iter()
            .map(|(process, parent)| {
                (
                    process.pid,
                    *parent,
                    process.started.as_str(),
                    process.program.as_str(),
                )
            })
            .collect();
        assert_eq!(
            named,
            [
                (501, 1, "Wed Oct 28 03:10:00 2026", "perl"),
                (602, 501, "Wed Oct 28 03:10:01 2026", "my tool"),
                (604, 501, "Wed Oct 28 03:10:01 2026", "\\u202eevil"),
            ]
        );
    }

    #[test]
    fn a_report_names_at_most_eight_processes_and_counts_the_rest() {
        let known = (2..12)
            .map(|pid| Process {
                pid,
                started: "Wed Oct 28 03:10:00 2026".into(),
                program: "sleep".into(),
            })
            .collect();
        let report = Leftovers(Box::new(Lineage {
            leader: 2,
            known,
            table: PS_TABLE,
            unchecked: false,
            pending: None,
            sandboxed: false,
            token: None,
            leader_started: None,
            applied: Instant::now(),
            missed: false,
        }))
        .plain();
        assert!(
            report.contains("still running: 2 sleep, 3 sleep,"),
            "{report}"
        );
        assert!(report.contains("9 sleep and 2 more."), "{report}");
        assert!(!report.contains("10 sleep"), "{report}");
    }

    /// Reads a process's program with `ps`. Input: the id. Output: its basename, if running.
    #[cfg(target_os = "macos")]
    fn program_of(pid: i32) -> Option<String> {
        let listing = crate::spawn_gate::output(
            std::process::Command::new("/bin/ps")
                .args(["-o", "comm=", "-p", &pid.to_string()])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null()),
        )
        .unwrap();
        let text = String::from_utf8_lossy(&listing.stdout);
        text.trim()
            .rsplit('/')
            .next()
            .filter(|name| !name.is_empty())
            .map(str::to_owned)
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn a_leftover_that_runs_a_new_program_after_the_snapshot_is_still_named() {
        let folder = tempfile::tempdir().unwrap();
        let mut leader = crate::spawn_gate::spawn(
            std::process::Command::new("/usr/bin/perl")
                .args([
                    "-mPOSIX",
                    "-e",
                    "POSIX::setsid() > 0 or exit 1; exec '/bin/sh', '-c', $ARGV[0] or exit 1",
                ])
                .arg(
                    "/usr/bin/perl -mPOSIX -e 'POSIX::setsid() > 0 or exit 1; open(my $file, \">\", \"child.pid\") or exit 1; print $file \"$$\\n\"; close($file); sleep 1; exec \"/bin/sleep\", \"30\"' < /dev/null > /dev/null 2>&1 & exec /bin/sleep 30",
                )
                .current_dir(folder.path())
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null()),
        )
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        let child = loop {
            if let Some(child) = std::fs::read_to_string(folder.path().join("child.pid"))
                .ok()
                .and_then(|text| text.trim().parse::<i32>().ok())
            {
                break child;
            }
            assert!(Instant::now() < deadline, "the stand-in never started");
            std::thread::sleep(Duration::from_millis(10));
        };
        let kill = || {
            if let Some(pid) = rustix::process::Pid::from_raw(child) {
                let _ = rustix::process::kill_process(pid, rustix::process::Signal::KILL);
            }
        };
        let lineage = Lineage::of(leader.id(), None).unwrap();
        assert!(
            lineage
                .known
                .iter()
                .any(|process| process.pid == child && process.program == "perl"),
            "{lineage:?}"
        );
        leader.kill().unwrap();
        leader.wait().unwrap();
        while program_of(child).as_deref() != Some("sleep") {
            if Instant::now() >= deadline {
                kill();
                panic!("the leftover never ran its new program");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let settled = lineage.settle();
        kill();
        match settled {
            Err(Unstopped::Left(left)) => assert_eq!(
                left.0
                    .known
                    .iter()
                    .map(|process| (process.pid, process.program.as_str()))
                    .collect::<Vec<_>>(),
                [(child, "sleep")]
            ),
            other => panic!(
                "a leftover that ran a new program after the snapshot was dropped: {other:?}"
            ),
        }
    }

    #[test]
    fn a_table_read_that_stalls_or_floods_ends_within_its_bound() {
        let stalled: &'static [&'static str] = &["/bin/sleep", "5"];
        let flooding: &'static [&'static str] =
            &["/bin/dd", "if=/dev/zero", "bs=1048576", "count=8"];
        for table in [stalled, flooding] {
            let started = Instant::now();
            let read = Lineage::reading(std::process::id(), table, None);
            let took = started.elapsed();
            assert!(
                matches!(&read, Ok(lineage) if lineage.unchecked),
                "{table:?} was read as a process table after {took:?}: {read:?}"
            );
            assert!(
                took < Duration::from_secs(3),
                "{table:?} held the read for {took:?}"
            );
        }
        assert!(Lineage::of(std::process::id(), None).is_ok());
    }

    #[test]
    #[should_panic]
    fn an_empty_report_trips_its_invariant() {
        let _ = Leftovers(Box::new(Lineage {
            leader: 2,
            known: Vec::new(),
            table: PS_TABLE,
            unchecked: false,
            pending: None,
            sandboxed: false,
            token: None,
            leader_started: None,
            applied: Instant::now(),
            missed: false,
        }))
        .plain();
    }

    #[test]
    fn an_unreadable_process_table_holds_the_account_only_until_it_can_be_checked() {
        let failing: &'static [&'static str] = &["/usr/bin/false"];
        let lineage = match Lineage::reading(std::process::id(), failing, None) {
            Ok(lineage) => lineage,
            Err(other) => panic!(
                "an unreadable process table at Full access came back {other:?}, which keeps the account for the engine's life"
            ),
        };
        let mut left = match lineage.settle() {
            Err(Unstopped::Left(left)) => left,
            other => panic!("an unreadable process table was reported {other:?}"),
        };
        assert_eq!(left.code(), UNCHECKED);
        let report = left.plain();
        assert!(report.contains("cannot confirm they ended"), "{report}");
        assert!(
            report.ends_with("at most 10 minutes, or until you sign it out"),
            "{report}"
        );

        left.0.table = PS_TABLE;
        let (held, released) = std::sync::mpsc::channel::<()>();
        left.hold(None, held);
        assert_eq!(
            released.recv_timeout(Duration::from_secs(5)),
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected),
            "the account stayed held after the processes could be listed and none ran"
        );
    }

    #[test]
    fn signing_out_lets_go_of_an_account_a_full_access_stop_keeps_and_names() {
        let mut leftover = crate::spawn_gate::spawn(
            std::process::Command::new("/usr/bin/perl")
                .args(["-mPOSIX", "-e", "POSIX::setsid() > 0 or exit 1; sleep 30"])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null()),
        )
        .unwrap();
        let pid = rustix::process::Pid::from_child(&leftover);
        let deadline = Instant::now() + Duration::from_secs(10);
        while rustix::process::getsid(Some(pid)) != Ok(pid) {
            assert!(
                Instant::now() < deadline,
                "the leftover never left its session"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        let lineage = Lineage::of(leftover.id(), None).unwrap();
        let account = CredentialRef::new("signed-out-full-access").unwrap();
        let other = CredentialRef::new("other-full-access").unwrap();
        let (held, released) = std::sync::mpsc::channel::<()>();
        Leftovers(Box::new(lineage)).hold(Some(account.clone()), held);

        let reports = held_by(&account);
        let release_count = release(&other);
        let after_other = released.recv_timeout(Duration::from_millis(500));
        let count = release(&account);
        let after_release = released.recv_timeout(Duration::from_secs(1));
        let remaining = held_by(&account);
        let _ = leftover.kill();
        let _ = leftover.wait();

        assert_eq!(reports.len(), 1, "{reports:?}");
        assert!(
            reports[0].contains(&format!("still running: {} perl", leftover.id())),
            "{reports:?}"
        );
        assert_eq!(release_count, 0);
        assert_eq!(
            after_other,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout),
            "signing out another account let this one go"
        );
        assert_eq!(count, 1);
        assert_eq!(
            after_release,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected),
            "the owner signed the account out and it stayed held while its leftover ran"
        );
        assert!(remaining.is_empty(), "{remaining:?}");
    }

    const FIRST: &str = "Tue Nov 10 09:00:00 2026";
    const LATER: &str = "Tue Nov 10 11:00:00 2026";
    const SLEEPER_PID: &str = "CRAZYAGENT_TEST_LINEAGE_SLEEPER";

    /// A lineage of `leader` with nothing known and no listing pending, for rows that hand it
    /// tables. Inputs: the leader and its start at the first read, if listed. Output: it.
    fn bare(leader: i32, started: Option<&str>) -> Lineage {
        Lineage {
            leader,
            known: Vec::new(),
            table: PS_TABLE,
            unchecked: false,
            pending: None,
            sandboxed: false,
            token: None,
            leader_started: started.map(str::to_owned),
            applied: Instant::now(),
            missed: false,
        }
    }

    /// One table row of a `sleep`. Inputs: its id, parent, session and start. Output: the row.
    fn row(pid: i32, parent: i32, session: i32, started: &str) -> Row {
        let process = Process {
            pid,
            started: started.into(),
            program: "sleep".into(),
        };
        (process, parent, Some(session))
    }

    #[test]
    fn a_stranger_that_reuses_a_dead_leaders_id_brings_no_session_members() {
        let mut live = bare(500, Some(FIRST));
        live.apply(&[row(500, 1, 500, FIRST), row(777, 1, 500, FIRST)]);
        let pids: Vec<i32> = live.known.iter().map(|process| process.pid).collect();
        assert_eq!(pids, [500, 777]);

        let mut reused = bare(500, Some(FIRST));
        reused.apply(&[row(500, 1, 500, LATER), row(778, 1, 500, LATER)]);
        assert!(reused.known.is_empty(), "{:?}", reused.known);

        let mut unlisted = bare(500, None);
        unlisted.apply(&[row(500, 1, 500, LATER), row(779, 1, 500, LATER)]);
        assert!(unlisted.known.is_empty(), "{:?}", unlisted.known);
    }

    #[test]
    fn followed_reads_that_fail_longer_than_a_follow_keep_the_stop_unchecked() {
        let rows = [row(500, 1, 500, FIRST)];
        let start = Instant::now();
        let mut short = bare(500, Some(FIRST));
        short.applied = start;
        short.noted(Err(()));
        short.noted(Ok((start + FOLLOW_GAP, &rows[..])));
        assert!(!short.missed);
        assert_eq!(short.known.len(), 1);

        let mut long = bare(500, Some(FIRST));
        long.applied = start;
        long.noted(Ok((start + FOLLOW_EVERY, &rows[..])));
        long.noted(Err(()));
        long.noted(Ok((
            start + FOLLOW_EVERY + FOLLOW_GAP + Duration::from_millis(1),
            &rows[..],
        )));
        assert!(
            long.missed,
            "two tables more than two follows apart, a failed one between, read as followed"
        );
        long.before_stop();
        assert!(
            long.unchecked,
            "a stop after a long run of failed follows read as checked"
        );

        let mut unended = bare(500, Some(FIRST));
        unended.applied = start.checked_sub(FOLLOW_GAP * 2).unwrap();
        unended.before_stop();
        assert!(unended.missed && unended.unchecked);
    }

    #[test]
    fn a_lineage_that_never_read_its_leader_reports_its_stop_unchecked() {
        let rows = [row(500, 1, 500, FIRST)];
        let mut unread = bare(500, None);
        unread.unchecked = true;
        unread.noted(Ok((Instant::now(), &rows[..])));
        unread.before_stop();
        assert!(
            unread.unchecked,
            "a stop whose first read failed read as checked once a later read worked"
        );
        let mut leaderless = bare(500, None);
        leaderless.before_stop();
        assert!(
            leaderless.unchecked,
            "a stop that never knew its leader's start read as checked"
        );
        for lineage in [unread, leaderless] {
            match lineage.settle() {
                Err(Unstopped::Left(left)) => assert_eq!(left.code(), UNCHECKED),
                other => panic!("a stop with no known leader was reported {other:?}"),
            }
        }
    }

    #[test]
    fn a_token_line_is_kept_only_for_the_exact_word_and_never_with_its_environment() {
        let (ours, other) = (LineageToken::new().unwrap(), LineageToken::new().unwrap());
        assert_ne!(ours, other);
        let word = format!("{LINEAGE_VARIABLE}={}", ours.as_str());
        let canary = "CANARY_VALUE=left-in-the-environment";
        let listing = format!(
            "  601   501 S    Tue Nov 10 16:32:13 2026     /opt/tool --flag {word} {canary}\n\
             602   501 S    Tue Nov 10 16:32:13 2026     /opt/tool {word}0\n\
             603   501 S    Tue Nov 10 16:32:13 2026     /opt/tool x{word}\n\
             604   501 S    Tue Nov 10 16:32:13 2026     /opt/tool {LINEAGE_VARIABLE}={} {canary}\n",
            other.as_str()
        );
        let mut kept = Vec::new();
        kept_lines(listing.as_bytes(), word.as_bytes(), &mut kept).unwrap();
        assert_eq!(
            String::from_utf8(kept).unwrap(),
            "601 501 S Tue Nov 10 16:32:13 2026 \n"
        );
        assert!(LineageToken::try_from(format!("{}0", ours.as_str())).is_err());
        assert!(LineageToken::try_from(ours.as_str().to_uppercase()).is_err());
        assert_eq!(
            LineageToken::try_from(ours.as_str().to_owned()),
            Ok(ours.clone())
        );
    }

    #[test]
    fn the_lineage_variable_reaches_an_agent_only_holding_a_token() {
        use std::ffi::OsStr;
        let token = LineageToken::new().unwrap();
        let allowed = |name: &str, value: &str| {
            crate::sandbox::agent_variable_allowed(OsStr::new(name), OsStr::new(value))
        };
        assert!(allowed(LINEAGE_VARIABLE, token.as_str()));
        assert!(!allowed(LINEAGE_VARIABLE, "not-a-token"));
        assert!(!allowed(
            LINEAGE_VARIABLE,
            &format!("{} rm", token.as_str())
        ));
        assert!(!allowed("CRAZYAGENT_WARDEN_FD", token.as_str()));
    }

    /// Not a check of its own: the detached program the token row starts. It writes its id to
    /// the file [`SLEEPER_PID`] names and sleeps, and returns at once when started without it.
    #[test]
    #[ignore]
    fn a_lineage_sleeper() {
        if let Some(path) = std::env::var_os(SLEEPER_PID) {
            std::fs::write(path, std::process::id().to_string()).unwrap();
            std::thread::sleep(Duration::from_secs(30));
        }
    }

    /// Starts this test program as [`a_lineage_sleeper`] in a session of its own from a shell
    /// that exits at once, so no parent or session leads to it. Inputs: the folder, the id
    /// file's name, the token and one more variable. Output: the shell's id and the sleeper's.
    #[cfg(target_os = "macos")]
    fn detached(
        folder: &std::path::Path,
        name: &str,
        token: &LineageToken,
        extra: (&str, &str),
    ) -> (u32, i32) {
        let mut shell = crate::spawn_gate::spawn(
            std::process::Command::new("/bin/sh")
                .arg("-c")
                .arg(
                    "/usr/bin/perl -mPOSIX -e 'POSIX::setsid() > 0 or exit 1; exec @ARGV or exit 1' \"$0\" --ignored --exact harnesses::leftovers::tests::a_lineage_sleeper --test-threads=1 < /dev/null > /dev/null 2>&1 &",
                )
                .arg(std::env::current_exe().unwrap())
                .env_clear()
                .env(LINEAGE_VARIABLE, token.as_str())
                .env(SLEEPER_PID, folder.join(name))
                .env(extra.0, extra.1)
                .current_dir(folder)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null()),
        )
        .unwrap();
        let leader = shell.id();
        assert!(shell.wait().unwrap().success());
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if let Some(pid) = std::fs::read_to_string(folder.join(name))
                .ok()
                .and_then(|text| text.trim().parse::<i32>().ok())
            {
                return (leader, pid);
            }
            assert!(
                Instant::now() < deadline,
                "the detached sleeper never started"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn a_detached_program_whose_shell_exited_at_once_is_named_by_its_token_and_holds_the_account() {
        let folder = tempfile::tempdir().unwrap();
        let (ours, other) = (LineageToken::new().unwrap(), LineageToken::new().unwrap());
        let canary = ("CRAZYAGENT_TEST_CANARY", "environment-text-never-reported");
        let (leader, named) = detached(folder.path(), "ours.pid", &ours, canary);
        let (_, stranger) = detached(folder.path(), "other.pid", &other, canary);
        let end = |pid: i32| {
            if let Some(pid) = rustix::process::Pid::from_raw(pid) {
                let _ = rustix::process::kill_process(pid, rustix::process::Signal::KILL);
            }
        };
        let settled = Lineage::of(leader, Some(ours)).unwrap().settle();
        let stranger_runs = rustix::process::Pid::from_raw(stranger)
            .is_some_and(|pid| rustix::process::test_kill_process(pid).is_ok());
        let left = match settled {
            Err(Unstopped::Left(left)) => left,
            other => {
                end(named);
                end(stranger);
                panic!("a detached program carrying the launch's token was not named: {other:?}");
            }
        };
        let (code, report, debug) = (left.code(), left.plain(), format!("{left:?}"));
        let pids: Vec<i32> = left.0.known.iter().map(|process| process.pid).collect();
        let account = CredentialRef::new("detached-full-access").unwrap();
        let (held, _released) = std::sync::mpsc::channel::<()>();
        left.hold(Some(account.clone()), held);
        let reports = held_by(&account);
        release(&account);
        end(named);
        end(stranger);

        assert_eq!(code, LEFT);
        assert_eq!(pids, [named], "{report}");
        assert!(
            report.contains(&format!("still running: {named} ")),
            "{report}"
        );
        assert!(stranger_runs, "a process with another token was signalled");
        assert_eq!(reports.len(), 1, "{reports:?}");
        for text in [&report, &debug, &reports[0]] {
            assert!(
                !text.contains(canary.1) && !text.contains(other.as_str()),
                "environment text reached a report: {text}"
            );
        }
    }
}
