use crate::protocol::CredentialRef;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
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
/// How long the process table one read shared by every lineage of the engine is reused
/// before a follow reads a new one.
pub(crate) const FOLLOW_EVERY: Duration = Duration::from_secs(2);
/// What [`Leftovers::names`] says when no process of a stop could be listed.
pub const UNLISTED: &str = "unknown, the process list could not be read";
/// The longest time between two tables a lineage takes, or the last and its stop's own read,
/// with nothing missed: shared reads up to [`FOLLOW_EVERY`] old, taken up to that far apart
/// (SECAUDIT-SETSID-LEFTOVER r2 L2).
#[cfg(unix)]
const FOLLOW_GAP: Duration = FOLLOW_EVERY.saturating_mul(2);
/// How often a [`Follower`] or a followed pane takes the shared table: half of
/// [`FOLLOW_EVERY`], so with tables up to [`FOLLOW_EVERY`] old, the last one it took and its
/// stop's own read lie within [`FOLLOW_GAP`] while those two reads take under a second
/// together (SETSID-L2 r1 L1).
pub(crate) const FOLLOWER_PACE: Duration = Duration::from_secs(1);
const _: () = assert!(2 * FOLLOWER_PACE.as_nanos() == FOLLOW_EVERY.as_nanos());
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
/// How often a hold that ends its processes checks them, so the kill follows the grace soon.
const ENDING_POLL: Duration = Duration::from_millis(250);
/// How long processes an agent left get to exit after SIGTERM before SIGKILL, under the End
/// them setting (DEC-147.4).
pub const LEFTOVER_GRACE: Duration = Duration::from_secs(10);
/// The most processes of the engine's own parent chain a signal check walks.
#[cfg(unix)]
const PARENTS_MAX: usize = 64;
/// The most characters of the label naming who left a hold.
const WHO_CHARS_MAX: usize = 80;
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
/// listing before the kill, the listing of its token's processes after it, the settling,
/// whose last listing may start one poll after `SETTLE` ends, and the listing that names
/// what is left.
#[cfg(unix)]
pub(crate) const STOP_TIME: Duration = TABLE_TIME
    .saturating_add(TABLE_GRACE)
    .saturating_mul(4)
    .saturating_add(SETTLE)
    .saturating_add(SETTLE_POLL);
/// No Full access stop looks for leftovers where no process table is read.
#[cfg(not(unix))]
pub(crate) const STOP_TIME: Duration = Duration::ZERO;
/// The command that lists every process with what the lineage needs of it: fixed fields
/// only, none of which an agent can put text in, so one line is always one process
/// (L1 r1 F1). Names come from [`PS_NAMES`], never from this table.
#[cfg(unix)]
const PS_TABLE: &[&str] = &[
    "/bin/ps",
    "-A",
    "-ww",
    "-o",
    "pid=,ppid=,uid=,stat=,lstart=",
];
/// The fields of one [`PS_TABLE`] line: id, parent, user, state and the five of the start.
#[cfg(unix)]
const TABLE_FIELDS: usize = 9;
/// The command, before its `-p` list, that names the processes a report shows.
#[cfg(unix)]
const PS_NAMES: &[&str] = &["/bin/ps", "-ww", "-o", "pid=,comm="];
/// The command, before its `-p` list, that checks the processes a hold is about to signal
/// right before it does, with fixed fields only: id, user and the five of the start.
#[cfg(unix)]
const PS_RECHECK: &[&str] = &["/bin/ps", "-o", "pid=,uid=,lstart="];
#[cfg(unix)]
const RECHECK_FIELDS: usize = 7;
/// The most processes one hold signals in one pass, so the `-p` list stays bounded.
#[cfg(unix)]
const SIGNALLED_MAX: usize = 256;
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

/// What happens to the processes an agent left running when its stop named them or could not
/// list them, a per-project owner setting (DEC-147.4): `End` ends them after
/// [`LEFTOVER_GRACE`] and lets the account go once none run, `Keep` lets the account go at
/// once and leaves them running, `Ask` keeps the account held until the owner picks End or
/// Keep in the team view, or they end.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeftoverMode {
    #[default]
    End,
    Keep,
    Ask,
}

impl LeftoverMode {
    /// The stored name. Input: the mode. Output: `end`, `keep` or `ask`.
    pub fn stored(self) -> &'static str {
        match self {
            Self::End => "end",
            Self::Keep => "keep",
            Self::Ask => "ask",
        }
    }

    /// Reads a stored name. Input: the name. Output: the mode, or none for a name it never
    /// stores.
    pub fn named(name: &str) -> Option<Self> {
        match name {
            "end" => Some(Self::End),
            "keep" => Some(Self::Keep),
            "ask" => Some(Self::Ask),
            _ => None,
        }
    }
}

/// Where one hold stands: `Held` keeps the account while its processes run, `Ending` sends
/// them SIGTERM, then SIGKILL after [`LEFTOVER_GRACE`], and `Kept` lets them run with the
/// account let go.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HoldState {
    Held,
    Ending,
    Kept,
}

/// Who left a hold, for the owner view: the project, if any, and a plain label such as
/// "the live master" or "team agent 156".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HoldBy {
    pub project: Option<String>,
    pub who: String,
}

/// One hold as the owner view shows it: its engine key, who left it, what still runs, where
/// it stands, when it began and whether it still keeps an account. Never environment text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeldView {
    pub key: u64,
    pub who: String,
    pub names: String,
    pub state: HoldState,
    pub since_ms: i64,
    pub holds_account: bool,
}

/// One hold of a Full access stop: the account it keeps, if any, the report naming what
/// still runs, what the stop holds, such as the agent's account lease, until the
/// hold ends or the owner keeps the processes running, who left it, the names it shows, where
/// it stands and when it began.
struct Held {
    account: Option<CredentialRef>,
    report: String,
    #[allow(dead_code)]
    kept: Option<Box<dyn Send>>,
    by: HoldBy,
    names: String,
    state: HoldState,
    since_ms: i64,
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

/// Lists the holds one project's agents left, oldest first, for the owner view. Input: the
/// project. Output: each hold's view.
pub fn held_in(project: &str) -> Vec<HeldView> {
    held()
        .iter()
        .filter(|(_, hold)| hold.by.project.as_deref() == Some(project))
        .map(|(key, hold)| HeldView {
            key: *key,
            who: hold.by.who.clone(),
            names: hold.names.clone(),
            state: hold.state,
            since_ms: hold.since_ms,
            holds_account: hold.account.is_some(),
        })
        .collect()
}

/// Has one hold of a project end its processes, the owner's End: SIGTERM, then SIGKILL after
/// [`LEFTOVER_GRACE`], each only to a process the same pass of its watcher found still
/// running with the start it was known by. A hold already ending is left as it is. Inputs:
/// the hold's key and the project it must belong to. Output: whether it is now ending.
pub fn end_held(key: u64, project: &str) -> bool {
    let mut held = held();
    let Some(hold) = held
        .get_mut(&key)
        .filter(|hold| hold.by.project.as_deref() == Some(project))
    else {
        return false;
    };
    hold.state = HoldState::Ending;
    true
}

/// Lets one hold of a project keep its processes running, the owner's Keep: the account and
/// what the stop held for it, such as the account lease, are let go at once, and
/// the processes stay listed until they end. Inputs: the hold's key and the project it must
/// belong to. Output: whether it is now kept.
pub fn keep_held(key: u64, project: &str) -> bool {
    let released = {
        let mut held = held();
        let Some(hold) = held
            .get_mut(&key)
            .filter(|hold| hold.by.project.as_deref() == Some(project))
        else {
            return false;
        };
        hold.state = HoldState::Kept;
        hold.account = None;
        hold.kept.take()
    };
    drop(released);
    true
}

/// Reads where one hold stands. Input: the key. Output: its state, or none once it ended.
fn state_of(key: u64) -> Option<HoldState> {
    held().get(&key).map(|hold| hold.state)
}

/// Writes what one hold names now, after a watcher pass. Inputs: the key, the names and the
/// report. Output: none.
fn named(key: u64, names: String, report: String) {
    if let Some(hold) = held().get_mut(&key) {
        hold.names = names;
        hold.report = report;
    }
}

/// Each project's leftover setting as the engine last read or wrote it, so a stop on an
/// agent's own thread, which has no database, holds by the setting that applies now.
static MODES: Mutex<BTreeMap<String, LeftoverMode>> = Mutex::new(BTreeMap::new());

/// Records one project's leftover setting for the stops that follow. Inputs: the project and
/// its setting. Output: none.
pub fn set_mode(project: &str, mode: LeftoverMode) {
    assert!(!project.is_empty());
    MODES
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(project.to_owned(), mode);
}

/// Reads the leftover setting a stop holds by. Input: the project, if any. Output: its
/// recorded setting, or End them when none was recorded or there is no project.
pub fn mode_of(project: Option<&str>) -> LeftoverMode {
    project
        .and_then(|project| {
            MODES
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .get(project)
                .copied()
        })
        .unwrap_or_default()
}

/// The wall clock in milliseconds since the Unix epoch, or 0 before it.
fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| {
            i64::try_from(since.as_millis()).unwrap_or(i64::MAX)
        })
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
/// user can read it, so it only lets a same-user process get itself named or, under End them
/// (DEC-147.4), ended; an agent only ever learns its own launch's token.
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
/// the last and the stop's own read, lay more than [`FOLLOW_GAP`] apart; `scanned` tells the
/// token's processes were read, or that there is no token, so a hold retries a failed scan.
#[derive(Debug)]
pub(crate) struct Lineage {
    #[cfg(unix)]
    leader: i32,
    known: Vec<Process>,
    #[cfg(unix)]
    table: &'static [&'static str],
    unchecked: bool,
    scanned: bool,
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
    /// empties, cannot stop them, or that they could not be listed. Input: the leftovers.
    /// Output: one sentence naming at most `NAMED_MAX` of them by id and program, counting
    /// the rest.
    pub fn plain(&self) -> String {
        self.0.report_now()
    }

    /// Says plainly what still runs, or that it could not be listed, and what the owner's
    /// Leftover processes setting does with it. Inputs: the leftovers and the setting.
    /// Output: one sentence; under Ask, [`Self::plain`] with where to choose.
    pub fn told(&self, mode: LeftoverMode) -> String {
        match mode {
            LeftoverMode::Ask => format!(
                "{}. End them or keep them running from the team view",
                self.plain()
            ),
            LeftoverMode::End => format!(
                "{}. The engine ends them after a {} second grace, finding them again until it can list them, and lets the account go once none run",
                self.0.what(),
                LEFTOVER_GRACE.as_secs()
            ),
            LeftoverMode::Keep => format!(
                "{}. They keep running as you chose, and the account was let go",
                self.0.what()
            ),
        }
    }

    /// Names what still runs, for a stop that holds no account as well as in [`Self::plain`]:
    /// at most `NAMED_MAX` by id and program, counting the rest and saying when more may run
    /// unlisted, or [`UNLISTED`] when none could be listed. Input: the leftovers. Output: the
    /// names.
    pub fn names(&self) -> String {
        self.0.names_now()
    }

    /// Keeps `kept`, such as the agent's account lease, listed under
    /// `account` with this report and `by`, until a watcher pass shows no process of the agent
    /// running, both the table and the token's processes read, or the owner signs the account
    /// out ([`release`]), then drops it; the engine's exit ends the hold too. A pass that
    /// cannot read both is retried, never taken as clean; with none named, the hold ends after
    /// `UNCHECKED_HOLD_MAX` even while no listing works. `mode` starts it: End has the watcher
    /// end the processes, Keep lets `kept` and the account go at once, Ask waits for the
    /// owner's [`end_held`] or [`keep_held`]. When no watching thread starts, only the owner's
    /// sign-out ends it. Inputs: the leftovers, the account they keep, if any, what they hold,
    /// who left them and the owner's setting. Output: none.
    pub fn hold<T: Send + 'static>(
        self,
        account: Option<CredentialRef>,
        kept: T,
        by: HoldBy,
        mode: LeftoverMode,
    ) {
        let key = NEXT_HELD.fetch_add(1, Ordering::Relaxed);
        let report = self.plain();
        let names = self.names();
        let by = HoldBy {
            project: by.project,
            who: by.who.chars().take(WHO_CHARS_MAX).collect(),
        };
        let (state, account, kept): (HoldState, Option<CredentialRef>, Option<Box<dyn Send>>) =
            match mode {
                LeftoverMode::End => (HoldState::Ending, account, Some(Box::new(kept))),
                LeftoverMode::Ask => (HoldState::Held, account, Some(Box::new(kept))),
                LeftoverMode::Keep => {
                    drop(kept);
                    (HoldState::Kept, None, None)
                }
            };
        let previous = held().insert(
            key,
            Held {
                account,
                report,
                kept,
                by,
                names,
                state,
                since_ms: now_ms(),
            },
        );
        assert!(previous.is_none());
        let lineage = *self.0;
        let _ = std::thread::Builder::new()
            .name("full-access-hold".into())
            .spawn(move || watch(key, lineage));
    }
}

/// How a watcher pass treats the processes of a hold that ends them: SIGTERM once, nothing
/// while the grace runs, then SIGKILL on every pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ending {
    Term,
    Kill,
}

/// Watches one hold until its processes are gone and could be checked, or `UNCHECKED_HOLD_MAX`
/// passes with none named, or the hold is let go: each pass reads the table, retries a token
/// scan that failed, and under Ending sends SIGTERM once, then SIGKILL past [`LEFTOVER_GRACE`],
/// only to processes that same pass found with the start they were known by. Inputs: the
/// hold's key and its lineage. Output: none; the hold is removed at the end.
fn watch(key: u64, mut lineage: Lineage) {
    let begun = Instant::now();
    let mut termed: Option<Instant> = None;
    while let Some(state) = state_of(key) {
        let signal = match (state, termed) {
            (HoldState::Ending, None) => Some(Ending::Term),
            (HoldState::Ending, Some(at)) if at.elapsed() >= LEFTOVER_GRACE => Some(Ending::Kill),
            _ => None,
        };
        let read = lineage.pass(signal);
        if signal == Some(Ending::Term) && read.is_some() {
            termed = Some(Instant::now());
        }
        let checked = read == Some(true);
        assert_eq!(checked, !lineage.unchecked);
        if lineage.known.is_empty() && (checked || begun.elapsed() >= UNCHECKED_HOLD_MAX) {
            break;
        }
        lineage.name();
        named(key, lineage.names_now(), lineage.report_now());
        std::thread::sleep(if state == HoldState::Ending {
            ENDING_POLL
        } else {
            HOLD_POLL
        });
    }
    let ended = held().remove(&key);
    drop(ended);
}

impl Lineage {
    /// The report [`Leftovers::plain`] gives for this lineage now. Input: the lineage, with
    /// a process named or a failed listing. Output: the sentence.
    fn report_now(&self) -> String {
        if self.known.is_empty() {
            return format!(
                "{}. Its account stays held until they can be checked, at most {} minutes, or until you sign it out",
                self.what(),
                UNCHECKED_HOLD_MAX.as_secs() / 60
            );
        }
        format!(
            "{}. Its account stays held until they end or you sign it out",
            self.what()
        )
    }

    /// Names what still runs and why the stop could not end it, or that the processes could
    /// not be listed. Input: the lineage. Output: the clause, with no closing stop.
    fn what(&self) -> String {
        assert!(!self.known.is_empty() || self.unchecked);
        if self.known.is_empty() {
            let unlisted = if self.sandboxed {
                "The engine could not list what the agent started outside its terminal after its stop, and no warden empties its sandbox"
            } else {
                "Full access could not list what the agent started outside its terminal after its stop"
            };
            return format!("{unlisted}, so it cannot confirm they ended");
        }
        let running = if self.sandboxed {
            "No warden empties this agent's sandbox, so what it started outside its terminal outlived its stop; still running in its sandbox"
        } else {
            "Full access cannot stop what the agent started outside its terminal; still running"
        };
        format!("{running}: {}", self.names_now())
    }

    /// The names [`Leftovers::names`] gives for this lineage now. Input: the lineage, with a
    /// process named or a failed listing. Output: the names.
    fn names_now(&self) -> String {
        assert!(!self.known.is_empty() || self.unchecked);
        if self.known.is_empty() {
            return UNLISTED.to_owned();
        }
        let named: Vec<String> = self
            .known
            .iter()
            .take(NAMED_MAX)
            .map(|process| format!("{} {}", process.pid, process.program))
            .collect();
        let more = self.known.len() - named.len();
        let counted = if more == 0 {
            String::new()
        } else {
            format!(" and {more} more")
        };
        let unlisted = if self.unchecked {
            ". More of them may run: the process list could not be read"
        } else {
            ""
        };
        format!("{}{counted}{unlisted}", named.join(", "))
    }

    /// Adds every process whose environment holds the launch's token, from one listing that
    /// keeps only each match's id and start. Input: the lineage. Output: Ok, also with no
    /// token, or uncertain when the listing failed.
    #[cfg(unix)]
    fn add_carriers(&mut self) -> Result<(), Unstopped> {
        let Some(token) = self.token.clone() else {
            return Ok(());
        };
        for (pid, started) in carrying(&token, &mut self.pending)? {
            let process = Process {
                pid,
                started,
                program: String::new(),
            };
            if !self.known.iter().any(|known| same(known, &process)) {
                self.known.push(process);
            }
        }
        Ok(())
    }

    /// One pass of a hold's watcher: reads the table and applies it, sends `signal`, if any,
    /// to the known processes that same table shows with the start they were known by, then
    /// retries the token scan while it has not read. A failed read is a retry, never clean.
    /// Inputs: the lineage and the signal. Output: none when the table could not be read,
    /// else whether the lineage is now checked, the token's processes read too; `unchecked`
    /// is set to the opposite.
    #[cfg(unix)]
    fn pass(&mut self, signal: Option<Ending>) -> Option<bool> {
        let Ok(rows) = rows(self.table, &mut self.pending) else {
            self.unchecked = true;
            return None;
        };
        self.apply(&rows);
        if let Some(signal) = signal {
            self.signal(&rows, signal);
        }
        if !self.scanned {
            self.scanned = self.add_carriers().is_ok();
        }
        self.unchecked = !self.scanned;
        Some(self.scanned)
    }

    /// Reads nothing where no process table is read. Input: unused. Output: none.
    #[cfg(not(unix))]
    fn pass(&mut self, _signal: Option<Ending>) -> Option<bool> {
        self.unchecked = true;
        None
    }

    /// Sends `signal` to each target [`targets`] picks from `rows` that [`Self::recheck`] still
    /// finds as known right before, one process at a time, never to a group. Inputs: the
    /// lineage, a table read in this pass and the signal. Output: how many were signalled.
    #[cfg(unix)]
    fn signal(&self, rows: &[Row], signal: Ending) -> usize {
        let Ok(own) = i32::try_from(std::process::id()) else {
            return 0;
        };
        let mut picked = targets(&self.known, rows, own, &parents(rows));
        picked.truncate(SIGNALLED_MAX);
        let picked = self.recheck(&picked);
        let sent = match signal {
            Ending::Term => rustix::process::Signal::TERM,
            Ending::Kill => rustix::process::Signal::KILL,
        };
        for pid in &picked {
            if let Some(pid) = rustix::process::Pid::from_raw(*pid) {
                let _ = rustix::process::kill_process(pid, sent);
            }
        }
        picked.len()
    }

    /// Reads, right before a signal, each picked process's id, user and start alone in a
    /// [`PS_RECHECK`] listing of fixed fields, and keeps only those that show this user and
    /// the start they are known by, so neither a reused id nor a line an agent wrote is ever
    /// signalled. A listing that fails or is malformed keeps none. Inputs: the lineage and the
    /// picked ids. Output: the ids to signal now.
    #[cfg(unix)]
    fn recheck(&self, picked: &[i32]) -> Vec<i32> {
        if picked.is_empty() {
            return Vec::new();
        }
        let user = rustix::process::getuid().as_raw();
        let mut argv = owned(PS_RECHECK);
        argv.push("-p".to_owned());
        argv.push(
            picked
                .iter()
                .map(i32::to_string)
                .collect::<Vec<_>>()
                .join(","),
        );
        let Ok(output) = read_bounded(argv, &mut None, None) else {
            return Vec::new();
        };
        let Some(found) = rechecked(&String::from_utf8_lossy(&output), picked) else {
            return Vec::new();
        };
        let confirmed: Vec<i32> = picked
            .iter()
            .copied()
            .filter(|pid| {
                found.get(pid).is_some_and(|(owner, started)| {
                    *owner == user
                        && self
                            .known
                            .iter()
                            .any(|known| known.pid == *pid && known.started == *started)
                })
            })
            .collect();
        assert!(confirmed.len() <= picked.len());
        confirmed
    }

    /// Names the first `NAMED_MAX` known processes by what they run now, from one
    /// [`PS_NAMES`] listing of only those ids. A listing that fails or is malformed keeps the
    /// names they had. Input: the lineage. Output: none.
    #[cfg(unix)]
    fn name(&mut self) {
        let asked: Vec<i32> = self
            .known
            .iter()
            .take(NAMED_MAX)
            .map(|process| process.pid)
            .collect();
        if asked.is_empty() {
            return;
        }
        let mut argv = owned(PS_NAMES);
        argv.push("-p".to_owned());
        argv.push(
            asked
                .iter()
                .map(i32::to_string)
                .collect::<Vec<_>>()
                .join(","),
        );
        let Ok(output) = read_bounded(argv, &mut None, None) else {
            return;
        };
        let Some(named) = names(&String::from_utf8_lossy(&output), &asked) else {
            return;
        };
        for process in &mut self.known {
            if let Some(program) = named.get(&process.pid) {
                process.program.clone_from(program);
            }
        }
    }

    /// Names nothing where no process table is read. Input: unused. Output: none.
    #[cfg(not(unix))]
    fn name(&mut self) {}

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
            scanned: token.is_none(),
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
    /// token, from one listing, then waits up to `SETTLE` for every process found to end,
    /// and no longer while a held-up listing is still in flight, since no other can start.
    /// Input: the lineage. Output: Ok once a listing shows none running, else the
    /// [`Leftovers`] still running at the deadline or, once a listing failed, unchecked.
    #[cfg(unix)]
    pub(crate) fn settle(mut self) -> Result<(), Unstopped> {
        if !self.scanned {
            match self.add_carriers() {
                Ok(()) => self.scanned = true,
                Err(_) => self.unchecked = true,
            }
        }
        let until = Instant::now() + SETTLE;
        loop {
            self.unchecked |= self.refresh().is_err();
            assert!(self.pending.is_none() || self.unchecked);
            if self.known.is_empty() && !self.unchecked {
                return Ok(());
            }
            if self.known.is_empty() || self.pending.is_some() || Instant::now() >= until {
                self.name();
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
    /// under the name it was last read by, then adds this user's processes in the leader's session
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
            .filter_map(|(process, ..)| {
                self.known
                    .iter()
                    .find(|known| same(known, process))
                    .map(|known| Process {
                        program: known.program.clone(),
                        ..process.clone()
                    })
            })
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

/// Where a [`Follower`]'s lineage stands: its first read still running, read, or taken by the
/// stop.
#[cfg_attr(not(unix), allow(dead_code))]
enum Following {
    Reading,
    Read(Result<Lineage, Unstopped>),
    Taken,
}

/// A Full access headless run's processes, followed from its start on a thread of its own,
/// since such a run has no poll that could follow them while it waits (SETSID-L2): the thread
/// takes the first read, then the engine's shared table every [`FOLLOWER_PACE`], so a process
/// a table showed in the run stays known once it leaves the run's session and loses its
/// parent. One that does both between two tables it took, up to the pace plus the table's age
/// plus the reads, is never seen, and only its token can still find it at the stop: the
/// SECAUDIT-SETSID-LEFTOVER option (a) residual. The lineage sits under a lock taken only to
/// apply a table, so a stop never waits for a read in flight. Holds the leader, the launch's
/// token, the lineage and the sender whose drop ends the thread.
pub(crate) struct Follower {
    leader: u32,
    token: Option<LineageToken>,
    lineage: Arc<Mutex<Following>>,
    _wake: std::sync::mpsc::Sender<()>,
}

impl Follower {
    /// Starts following a Full access run from its first read on a thread of its own. When no
    /// thread starts, or no table is read here, the stop reads the lineage itself. Inputs: the
    /// run's leader, whose id is also its session's, and the launch's token. Output: the
    /// follower.
    pub(crate) fn start(leader: u32, token: Option<LineageToken>) -> Self {
        let lineage = Arc::new(Mutex::new(Following::Reading));
        let (wake, woken) = std::sync::mpsc::channel();
        #[cfg(unix)]
        {
            let followed = Arc::clone(&lineage);
            let first = token.clone();
            let _ = std::thread::Builder::new()
                .name("full-access-follow".into())
                .spawn(move || follow_run(leader, first, &followed, &woken));
        }
        #[cfg(not(unix))]
        drop(woken);
        Self {
            leader,
            token,
            lineage,
            _wake: wake,
        }
    }

    /// Ends the follow and takes the lineage for the stop, with the stop's own read before the
    /// group is killed ([`Lineage::before_stop`]). It never waits for a read the thread has in
    /// flight: while the first read is not in, the stop reads the lineage itself. Blocks up to
    /// two table reads. Input: the follower. Output: the lineage, or why it cannot be read.
    pub(crate) fn stop(self) -> Result<Lineage, Unstopped> {
        let taken = std::mem::replace(&mut *following(&self.lineage), Following::Taken);
        let mut lineage = match taken {
            Following::Read(read) => read?,
            Following::Reading | Following::Taken => Lineage::of(self.leader, self.token)?,
        };
        lineage.before_stop();
        Ok(lineage)
    }
}

/// Locks a follower's lineage, also after a panic of the thread that held it. Input: the
/// lock. Output: the guard.
fn following(lineage: &Mutex<Following>) -> MutexGuard<'_, Following> {
    lineage.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Takes a follower's first read, then the shared table every [`FOLLOWER_PACE`] until its
/// stop. Each read runs with no lock held, and the lock is taken only to apply it. Ends at
/// the stop, or once the first read found no lineage. Inputs: the leader, the token, the
/// lineage's lock and the receiver the stop disconnects. Output: none.
#[cfg(unix)]
fn follow_run(
    leader: u32,
    token: Option<LineageToken>,
    lineage: &Mutex<Following>,
    woken: &std::sync::mpsc::Receiver<()>,
) {
    let first = Lineage::of(leader, token);
    {
        let mut slot = following(lineage);
        if !matches!(*slot, Following::Reading) {
            return;
        }
        *slot = Following::Read(first);
    }
    while woken.recv_timeout(FOLLOWER_PACE) == Err(std::sync::mpsc::RecvTimeoutError::Timeout) {
        let Some(read) = shared_rows() else {
            continue;
        };
        let mut slot = following(lineage);
        let Following::Read(Ok(followed)) = &mut *slot else {
            return;
        };
        followed.noted(
            read.as_ref()
                .map(|(at, rows)| (*at, rows.as_slice()))
                .map_err(|_| ()),
        );
    }
}

/// Tells whether two rows name the same process: the same id with the same start, whatever
/// program it runs now. Inputs: the rows. Output: true only then.
#[cfg(unix)]
fn same(known: &Process, now: &Process) -> bool {
    known.pid == now.pid && known.started == now.started
}

/// Picks which known processes a hold may signal now: each one a table read in the same pass
/// shows with the same id and start, so a reused id never is, and that table lists only this
/// user's processes. Never process 0 or 1, the engine itself or any process of its parent
/// chain. Inputs: the known processes, the rows, the engine's id and its parent chain.
/// Output: the ids, each at most once.
#[cfg(unix)]
fn targets(known: &[Process], rows: &[Row], own: i32, parents: &[i32]) -> Vec<i32> {
    let mut picked: Vec<i32> = Vec::new();
    for process in known {
        if process.pid > 1
            && process.pid != own
            && !parents.contains(&process.pid)
            && !picked.contains(&process.pid)
            && rows.iter().any(|(row, ..)| same(row, process))
        {
            picked.push(process.pid);
        }
    }
    assert!(picked.len() <= known.len());
    picked
}

/// Walks the engine's parent chain from its parent through `rows`, at most `PARENTS_MAX`
/// steps, stopping at process 1 or a loop. Input: a table read. Output: the chain's ids.
#[cfg(unix)]
fn parents(rows: &[Row]) -> Vec<i32> {
    let mut chain: Vec<i32> = Vec::new();
    let mut next = rustix::process::getppid().map(|pid| pid.as_raw_nonzero().get());
    while let Some(pid) = next
        && pid > 1
        && chain.len() < PARENTS_MAX
        && !chain.contains(&pid)
    {
        chain.push(pid);
        next = rows
            .iter()
            .find(|(row, ..)| row.pid == pid)
            .map(|(_, parent, _)| *parent);
    }
    assert!(chain.len() <= PARENTS_MAX);
    chain
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
    let output = read_bounded(owned(argv), pending, None)?;
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
    let output = read_bounded(owned(PS_ENVIRONMENT), pending, Some(word))?;
    let user = rustix::process::getuid().as_raw();
    let own = i32::try_from(std::process::id()).map_err(|_| Unstopped::Uncertain)?;
    Ok(carriers(&output, user, own))
}

/// Reads the carriers out of what [`kept_lines`] kept, split by the same [`fields`] it
/// split by: each line of `user`'s, not `own`, above 1 and not a zombie, whose id and user
/// are plain digits. Inputs: the kept lines, the user id and the engine's id. Output: each
/// carrier's id and start, at most `TABLE_ROWS_MAX`.
#[cfg(target_os = "macos")]
fn carriers(kept: &[u8], user: u32, own: i32) -> Vec<(i32, String)> {
    let found: Vec<(i32, String)> = kept
        .split(|byte| *byte == b'\n')
        .filter_map(|line| {
            let parts: Vec<&str> = fields(line)
                .map(std::str::from_utf8)
                .collect::<Result<_, _>>()
                .ok()?;
            let pid: i32 = number(parts.first()?.as_bytes())?;
            let owner: u32 = number(parts.get(1)?.as_bytes())?;
            (parts.len() == KEPT_FIELDS
                && owner == user
                && pid != own
                && pid > 1
                && !parts[2].starts_with('Z'))
            .then(|| (pid, parts[3..KEPT_FIELDS].join(" ")))
        })
        .take(TABLE_ROWS_MAX)
        .collect();
    assert!(found.len() <= TABLE_ROWS_MAX);
    found
}

/// Splits one listing line into its fields on ASCII whitespace only, the one way every
/// environment listing reader splits, so a byte another reader would take as a separator,
/// such as a vertical tab or a no-break space, stays inside a field (L1 r2 L1). Input: the
/// line. Output: its non-empty fields.
#[cfg(unix)]
fn fields(line: &[u8]) -> impl Iterator<Item = &[u8]> + Clone {
    line.split(u8::is_ascii_whitespace)
        .filter(|field| !field.is_empty())
}

/// Reads a field that must be a plain decimal number, as `ps` prints ids: 1 to 10 ASCII
/// digits, no sign and nothing else. Input: the field. Output: the number, or none.
#[cfg(unix)]
fn number<T: std::str::FromStr>(field: &[u8]) -> Option<T> {
    if field.is_empty() || field.len() > 10 || !field.iter().all(u8::is_ascii_digit) {
        return None;
    }
    std::str::from_utf8(field).ok()?.parse().ok()
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
    argv: Vec<String>,
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
            let _ = sender.send(run_listing(&argv, deadline, keep));
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
fn run_listing(argv: &[String], deadline: Instant, keep: Option<String>) -> Option<Vec<u8>> {
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

/// Copies a listing command into the owned arguments [`read_bounded`] takes. Input: the
/// command. Output: its arguments.
#[cfg(unix)]
fn owned(argv: &[&str]) -> Vec<String> {
    argv.iter().map(|argument| (*argument).to_owned()).collect()
}

/// Reads a listing line by line, at most `ENVIRONMENT_BYTES_MAX` bytes, and keeps of each
/// line that holds `word` as a whole field only its first `KEPT_FIELDS` fields, so no
/// environment text is kept. A listing in which two lines start with the same process id is
/// refused whole: a line that copies a real process's id can only come from text an agent
/// put in its arguments or environment (L1 r1 F1). So is one whose line holding `word` does
/// not start with a plain digits id, as a line led by a vertical tab or a no-break space
/// (L1 r2 L1). Fields are split by [`fields`], as [`carriers`] splits them. Inputs: the
/// listing, the word and where to keep. Output: Ok, or the read's error, an overlong or
/// forged listing included.
#[cfg(unix)]
fn kept_lines(listing: impl std::io::Read, word: &[u8], kept: &mut Vec<u8>) -> std::io::Result<()> {
    use std::io::BufRead;
    let mut lines = std::io::BufReader::new(listing.take(ENVIRONMENT_BYTES_MAX + 1));
    let (mut line, mut total) = (Vec::new(), 0u64);
    let mut seen: std::collections::BTreeSet<i32> = std::collections::BTreeSet::new();
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
        let parts = fields(&line);
        let holds = parts.clone().any(|field| field == word);
        match parts.clone().next().and_then(number::<i32>) {
            Some(pid) if !seen.insert(pid) => {
                return Err(std::io::Error::other("listing_pid_twice"));
            }
            Some(_) => {}
            None if holds => return Err(std::io::Error::other("listing_line_forged")),
            None => {}
        }
        if seen.len() > TABLE_ROWS_MAX {
            return Err(std::io::Error::other("listing_too_long"));
        }
        if holds {
            for field in parts.take(KEPT_FIELDS) {
                kept.extend_from_slice(field);
                kept.push(b' ');
            }
            kept.push(b'\n');
        }
    }
}

/// Picks from a [`PS_TABLE`] listing the processes of `user` other than `own` that are not
/// zombies. Every line must be one process in the table's fixed fields, and no id may show
/// twice; otherwise the whole listing is refused, never read around (L1 r1 F1). The
/// processes carry no name: [`Lineage::name`] reads names apart. Inputs: the listing, the
/// user id and the engine's id. Output: each process with its parent's id, or uncertain.
#[cfg(unix)]
fn parse(listing: &str, user: u32, own: i32) -> Result<Vec<(Process, i32)>, Unstopped> {
    let mut rows = Vec::new();
    let mut seen: std::collections::BTreeSet<i32> = std::collections::BTreeSet::new();
    for line in listing.lines() {
        if seen.len() == TABLE_ROWS_MAX {
            return Err(Unstopped::Uncertain);
        }
        let fields: Vec<&str> = line.split_whitespace().collect();
        let (Some(pid), Some(parent), Some(owner)) = (
            fields.first().and_then(|field| field.parse::<i32>().ok()),
            fields.get(1).and_then(|field| field.parse::<i32>().ok()),
            fields.get(2).and_then(|field| field.parse::<u32>().ok()),
        ) else {
            return Err(Unstopped::Uncertain);
        };
        if fields.len() != TABLE_FIELDS || !seen.insert(pid) {
            return Err(Unstopped::Uncertain);
        }
        if owner != user || pid == own || pid <= 1 || fields[3].starts_with('Z') {
            continue;
        }
        rows.push((
            Process {
                pid,
                started: fields[4..TABLE_FIELDS].join(" "),
                program: String::new(),
            },
            parent,
        ));
    }
    assert!(rows.len() <= seen.len());
    Ok(rows)
}

/// Reads the names a [`PS_NAMES`] listing gives the processes it was asked for: each line one
/// asked-for id, then its program's path, cut to the basename's visible characters. A line
/// that is not an asked-for id, or an id named twice, refuses every name. Inputs: the listing
/// and the asked-for ids. Output: each id's name, or none.
#[cfg(unix)]
fn names(listing: &str, asked: &[i32]) -> Option<BTreeMap<i32, String>> {
    let mut named = BTreeMap::new();
    for line in listing.lines() {
        let line = line.trim_start();
        let (pid, path) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
        let pid = pid.parse::<i32>().ok().filter(|pid| asked.contains(pid))?;
        let program: String = crate::memory::visible_text(
            &path
                .trim()
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
        if named.insert(pid, program).is_some() {
            return None;
        }
    }
    assert!(named.len() <= asked.len());
    Some(named)
}

/// Reads a [`PS_RECHECK`] listing: each line one asked-for id with its user and start, no id
/// twice and nothing else. Inputs: the listing and the asked-for ids. Output: each id's user
/// and start, or none when any line is not so.
#[cfg(unix)]
fn rechecked(listing: &str, asked: &[i32]) -> Option<BTreeMap<i32, (u32, String)>> {
    let mut found = BTreeMap::new();
    for line in listing.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() != RECHECK_FIELDS {
            return None;
        }
        let pid = fields[0]
            .parse::<i32>()
            .ok()
            .filter(|pid| asked.contains(pid))?;
        let owner = fields[1].parse::<u32>().ok()?;
        if found
            .insert(pid, (owner, fields[2..RECHECK_FIELDS].join(" ")))
            .is_some()
        {
            return None;
        }
    }
    assert!(found.len() <= asked.len());
    Some(found)
}

#[cfg(all(test, unix))]
pub(crate) mod tests {
    use super::*;

    const LISTING: &str = "  501   1 501 Ss   Wed Oct 28 03:10:00 2026\n\
        602 501 501 S Wed Oct 28 03:10:01 2026\n\
        603 501   0 S Wed Oct 28 03:10:01 2026\n\
        605 501 501 Z Wed Oct 28 03:10:01 2026\n\
        700   1 501 S Wed Oct 28 03:10:02 2026\n";

    #[test]
    fn the_table_keeps_this_users_other_running_processes_by_fixed_fields_only() {
        let rows = parse(LISTING, 501, 700).unwrap();
        let read: Vec<(i32, i32, &str, &str)> = rows
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
            read,
            [
                (501, 1, "Wed Oct 28 03:10:00 2026", ""),
                (602, 501, "Wed Oct 28 03:10:01 2026", ""),
            ]
        );
        assert!(
            PS_TABLE
                .last()
                .is_some_and(|fields| !fields.contains("comm")
                    && !fields.contains("command")
                    && !fields.contains("args"))
        );
    }

    /// A table line an agent could forge, by putting a newline and a made-up row in its
    /// arguments or program path, copies a real process's id, or breaks the fixed fields;
    /// either refuses the whole table, so no forged row is ever read (L1 r1 F1).
    #[test]
    fn a_table_with_an_id_twice_or_a_line_out_of_its_fixed_fields_is_refused_whole() {
        let forged = format!("{LISTING}  602 501 501 S Wed Oct 28 03:10:01 2026\n");
        assert!(matches!(
            parse(&forged, 501, 700),
            Err(Unstopped::Uncertain)
        ));
        for broken in [
            format!("{LISTING}garbage line\n"),
            format!("{LISTING}  800 501 501 S Wed Oct 28 03:10:01 2026 /opt/tool\n"),
            format!("{LISTING}  801 501 501 S Wed Oct 28 03:10:01\n"),
        ] {
            assert!(
                matches!(parse(&broken, 501, 700), Err(Unstopped::Uncertain)),
                "{broken}"
            );
        }
        assert_eq!(parse(LISTING, 501, 700).unwrap().len(), 2);
    }

    /// Names come apart from the table, only for the ids asked, each id once: a name that
    /// carries a newline and a made-up line refuses every name rather than misname a
    /// process, and names keep only their basename's visible characters (L1 r1 F1).
    #[test]
    fn names_are_read_only_for_the_asked_ids_and_a_forged_line_refuses_them_all() {
        let named = names(
            "  602 /Applications/My Tool.app/Contents/MacOS/my tool\n  604 /tmp/\u{202e}evil\n",
            &[602, 604],
        )
        .unwrap();
        assert_eq!(named[&602], "my tool");
        assert_eq!(named[&604], "\\u202eevil");
        assert_eq!(
            names("  602 /tmp/a\n  604 /tmp/b\n  602 /tmp/c\n", &[602, 604]),
            None
        );
        assert_eq!(names("  602 /tmp/a\n  999 /tmp/b\n", &[602, 604]), None);
        assert_eq!(names("  602 /tmp/a\nnot an id\n", &[602]), None);
    }

    /// The recheck right before a signal reads fixed fields only and refuses any line that
    /// is not one asked-for id with its user and start (L1 r1 F1).
    #[test]
    fn a_recheck_listing_reads_each_asked_id_once_or_refuses() {
        let found = rechecked(
            "  602 501 Wed Oct 28 03:10:01 2026\n  604   0 Wed Oct 28 03:10:02 2026\n",
            &[602, 604],
        )
        .unwrap();
        assert_eq!(found[&602], (501, "Wed Oct 28 03:10:01 2026".to_owned()));
        assert_eq!(found[&604].0, 0);
        for forged in [
            "  602 501 Wed Oct 28 03:10:01 2026\n  602 501 Wed Oct 28 03:10:01 2026\n",
            "  602 501 Wed Oct 28 03:10:01 2026\n  777 501 Wed Oct 28 03:10:01 2026\n",
            "  602 501 Wed Oct 28 03:10:01 2026 extra\n",
        ] {
            assert_eq!(rechecked(forged, &[602, 604]), None, "{forged}");
        }
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
            scanned: true,
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
        let mut lineage = Lineage::of(leader.id(), None).unwrap();
        lineage.name();
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
            scanned: true,
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
    fn a_settle_ends_at_once_while_a_held_up_listing_is_still_in_flight() {
        let (_listing, held_up) = std::sync::mpsc::channel::<Option<Vec<u8>>>();
        let lineage = Lineage {
            leader: 2,
            known: vec![Process {
                pid: 2,
                started: "Wed Oct 28 03:10:00 2026".into(),
                program: "sleep".into(),
            }],
            table: PS_TABLE,
            unchecked: true,
            scanned: true,
            pending: Some(held_up),
            sandboxed: false,
            token: None,
            leader_started: None,
            applied: Instant::now(),
            missed: false,
        };
        let started = Instant::now();
        let settled = lineage.settle();
        let took = started.elapsed();
        assert!(
            took < SETTLE / 2,
            "a settle waited {took:?} behind a listing no other can start after"
        );
        match settled {
            Err(Unstopped::Left(left)) => {
                assert_eq!(left.code(), LEFT);
                assert_eq!(
                    left.names(),
                    "2 sleep. More of them may run: the process list could not be read"
                );
            }
            other => panic!("a settle behind a held-up listing was reported {other:?}"),
        }
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
        left.hold(None, held, by(), LeftoverMode::Ask);
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
        Leftovers(Box::new(lineage)).hold(Some(account.clone()), held, by(), LeftoverMode::Ask);

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
            scanned: true,
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

    /// An agent whose arguments hold a newline and a made-up line with an unrelated process's
    /// id and the token makes that id show twice in the environment listing, which is then
    /// refused whole, never read as a carrier (L1 r1 F1).
    #[test]
    fn a_token_listing_with_an_id_twice_is_refused_whole() {
        let ours = LineageToken::new().unwrap();
        let word = format!("{LINEAGE_VARIABLE}={}", ours.as_str());
        let listing = format!(
            "  601   501 S    Tue Nov 10 16:32:13 2026     /opt/tool x\n\
             900   501 S    Tue Nov 10 16:30:00 2026 {word}\n\
             900   501 S    Tue Nov 10 16:30:00 2026     /Applications/Owner.app/owner\n"
        );
        let mut kept = Vec::new();
        assert!(kept_lines(listing.as_bytes(), word.as_bytes(), &mut kept).is_err());
        let mut kept = Vec::new();
        let honest = format!(
            "  601   501 S    Tue Nov 10 16:32:13 2026     /opt/tool {word}\n\
             900   501 S    Tue Nov 10 16:30:00 2026     /Applications/Owner.app/owner\n"
        );
        kept_lines(honest.as_bytes(), word.as_bytes(), &mut kept).unwrap();
        assert_eq!(
            String::from_utf8(kept).unwrap(),
            "601 501 S Tue Nov 10 16:32:13 2026 \n"
        );
    }

    /// L1 r2 L1: a forged token line led by a vertical tab or a no-break space before a real
    /// process's id is refused whole by the listing reader, and the carrier reader, which
    /// splits by the same ASCII fields, never reads an id that is not plain digits.
    #[test]
    fn a_token_line_led_by_a_vertical_tab_or_no_break_space_is_refused_and_never_a_carrier() {
        let ours = LineageToken::new().unwrap();
        let word = format!("{LINEAGE_VARIABLE}={}", ours.as_str());
        for lead in ["\x0b", "\u{a0}"] {
            let listing = format!(
                "  602   501 S    Tue Nov 10 16:32:13 2026     /Applications/Owner.app/owner\n\
                 900   501 S    Tue Nov 10 16:30:00 2026     /opt/agent x\n\
                 {lead}602   501 S    Tue Nov 10 16:32:13 2026 {word}\n"
            );
            let mut kept = Vec::new();
            assert!(
                kept_lines(listing.as_bytes(), word.as_bytes(), &mut kept).is_err(),
                "{lead:?}"
            );
        }
        #[cfg(target_os = "macos")]
        {
            let kept = "\x0b602 501 S Tue Nov 10 16:32:13 2026 \n\
                        \u{a0}603 501 S Tue Nov 10 16:32:13 2026 \n\
                        +604 501 S Tue Nov 10 16:32:13 2026 \n\
                        605 501 S Tue Nov 10 16:32:13 2026 \n";
            assert_eq!(
                carriers(kept.as_bytes(), 501, 1),
                vec![(605, "Tue Nov 10 16:32:13 2026".to_owned())]
            );
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
    pub(crate) fn detached(
        folder: &std::path::Path,
        name: &str,
        token: &LineageToken,
        extra: (&str, &str),
    ) -> (u32, i32) {
        detached_with(folder, name, Some(token), extra)
    }

    /// Starts a sleeper as [`detached`] does, with the launch's token or with none at all,
    /// as an unrelated program of the owner's user would run. Inputs: the folder, the id
    /// file's name, the token, if any, and one more variable. Output: the shell's id and the
    /// sleeper's.
    #[cfg(target_os = "macos")]
    pub(crate) fn detached_with(
        folder: &std::path::Path,
        name: &str,
        token: Option<&LineageToken>,
        extra: (&str, &str),
    ) -> (u32, i32) {
        let mut command = std::process::Command::new("/bin/sh");
        command
            .arg("-c")
            .arg(
                "/usr/bin/perl -mPOSIX -e 'POSIX::setsid() > 0 or exit 1; exec @ARGV or exit 1' \"$0\" --ignored --exact harnesses::leftovers::tests::a_lineage_sleeper --test-threads=1 < /dev/null > /dev/null 2>&1 &",
            )
            .arg(std::env::current_exe().unwrap())
            .env_clear();
        if let Some(token) = token {
            command.env(LINEAGE_VARIABLE, token.as_str());
        }
        let mut shell = crate::spawn_gate::spawn(
            command
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
        left.hold(Some(account.clone()), held, by(), LeftoverMode::Ask);
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

    /// Who left a test hold, in no project. Output: the label.
    fn by() -> HoldBy {
        HoldBy {
            project: None,
            who: "a test".into(),
        }
    }

    /// Who left a test hold in `project`. Input: the project. Output: the label.
    fn by_in(project: &str) -> HoldBy {
        HoldBy {
            project: Some(project.into()),
            who: "team agent 7".into(),
        }
    }

    /// Tells whether a process still runs and is no zombie. Input: the id. Output: true then.
    fn runs(pid: i32) -> bool {
        table(PS_TABLE, &mut None)
            .is_ok_and(|rows| rows.iter().any(|(process, _)| process.pid == pid))
    }

    /// Waits up to `wait` for `done`. Inputs: the bound and the condition. Output: whether it
    /// held in time.
    fn within(wait: Duration, mut done: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + wait;
        while Instant::now() < deadline {
            if done() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        done()
    }

    /// Ends a test process by id. Input: the id. Output: none.
    fn end_pid(pid: i32) {
        if let Some(pid) = rustix::process::Pid::from_raw(pid) {
            let _ = rustix::process::kill_process(pid, rustix::process::Signal::KILL);
        }
    }

    /// Starts a perl process in a session of its own that ignores SIGTERM, or not, and waits
    /// until it left its session. Input: whether it ignores SIGTERM. Output: the child.
    fn session_sleeper(ignores_term: bool) -> std::process::Child {
        let script = if ignores_term {
            "$SIG{TERM} = 'IGNORE'; POSIX::setsid() > 0 or exit 1; sleep 60"
        } else {
            "POSIX::setsid() > 0 or exit 1; sleep 60"
        };
        let child = crate::spawn_gate::spawn(
            std::process::Command::new("/usr/bin/perl")
                .args(["-mPOSIX", "-e", script])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null()),
        )
        .unwrap();
        let pid = rustix::process::Pid::from_child(&child);
        assert!(
            within(Duration::from_secs(10), || rustix::process::getsid(Some(
                pid
            )) == Ok(pid)),
            "the sleeper never left its session"
        );
        child
    }

    #[test]
    fn the_leftover_setting_defaults_to_end_and_reads_back_each_stored_name() {
        assert_eq!(LeftoverMode::default(), LeftoverMode::End);
        for mode in [LeftoverMode::End, LeftoverMode::Keep, LeftoverMode::Ask] {
            assert_eq!(LeftoverMode::named(mode.stored()), Some(mode));
            let json = serde_json::to_string(&mode).unwrap();
            assert_eq!(json, format!("\"{}\"", mode.stored()));
        }
        assert_eq!(LeftoverMode::named("kill"), None);
        assert_eq!(LeftoverMode::named(""), None);
    }

    #[test]
    fn a_reused_id_the_engine_and_its_parents_are_never_signalled() {
        let own = 900;
        let parent = 901;
        let rows = [
            row(500, 1, 500, LATER),
            row(501, 1, 501, FIRST),
            row(own, parent, own, FIRST),
            row(parent, 1, parent, FIRST),
        ];
        let known = |pid: i32, started: &str| Process {
            pid,
            started: started.into(),
            program: "sleep".into(),
        };
        let picked = targets(
            &[
                known(500, FIRST),
                known(501, FIRST),
                known(501, FIRST),
                known(1, FIRST),
                known(0, FIRST),
                known(own, FIRST),
                known(parent, FIRST),
                known(502, FIRST),
            ],
            &rows,
            own,
            &[parent],
        );
        assert_eq!(
            picked,
            [501],
            "only a process the same table shows with its known start may be signalled"
        );
    }

    #[test]
    fn end_terms_then_kills_past_the_grace_and_lets_the_account_go_once_none_run() {
        let mut leftover = session_sleeper(true);
        let pid = i32::try_from(leftover.id()).unwrap();
        let lineage = Lineage::of(leftover.id(), None).unwrap();
        let account = CredentialRef::new("ended-full-access").unwrap();
        let (held, released) = std::sync::mpsc::channel::<()>();
        Leftovers(Box::new(lineage)).hold(
            Some(account.clone()),
            held,
            by_in("project-end"),
            LeftoverMode::End,
        );
        std::thread::sleep(Duration::from_secs(2));
        let _ = leftover.try_wait();
        let alive_in_grace = runs(pid);
        let held_in_grace = held_by(&account);
        let view = held_in("project-end");
        let ended = within(LEFTOVER_GRACE + Duration::from_secs(5), || {
            let _ = leftover.try_wait();
            !runs(pid)
        });
        let after = released.recv_timeout(Duration::from_secs(5));
        let remaining = held_by(&account);
        end_pid(pid);
        let _ = leftover.wait();

        assert!(alive_in_grace, "SIGKILL came before the grace ended");
        assert_eq!(held_in_grace.len(), 1, "{held_in_grace:?}");
        assert!(
            matches!(view.as_slice(), [one] if one.state == HoldState::Ending && one.holds_account && one.names.contains(&pid.to_string())),
            "{view:?}"
        );
        assert!(ended, "a process that ignores SIGTERM outlived the grace");
        assert_eq!(
            after,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected),
            "the account stayed held after its leftover was ended"
        );
        assert!(remaining.is_empty(), "{remaining:?}");
    }

    #[test]
    fn keep_lets_the_account_go_at_once_and_lists_the_processes_until_they_end() {
        let mut leftover = session_sleeper(false);
        let pid = i32::try_from(leftover.id()).unwrap();
        let lineage = Lineage::of(leftover.id(), None).unwrap();
        let account = CredentialRef::new("kept-full-access").unwrap();
        let (held, released) = std::sync::mpsc::channel::<()>();
        Leftovers(Box::new(lineage)).hold(
            Some(account.clone()),
            held,
            by_in("project-keep"),
            LeftoverMode::Keep,
        );
        let let_go = released.recv_timeout(Duration::from_millis(500));
        let reports = held_by(&account);
        let view = held_in("project-keep");
        std::thread::sleep(HOLD_POLL * 2);
        let _ = leftover.try_wait();
        let still_runs = runs(pid);
        end_pid(pid);
        let _ = leftover.wait();
        let cleared = within(HOLD_POLL * 3, || held_in("project-keep").is_empty());

        assert_eq!(
            let_go,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected),
            "Keep held the account"
        );
        assert!(reports.is_empty(), "{reports:?}");
        assert!(
            matches!(view.as_slice(), [one] if one.state == HoldState::Kept && !one.holds_account && one.names.contains(&pid.to_string()) && one.who == "team agent 7"),
            "{view:?}"
        );
        assert!(still_runs, "Keep ended the process");
        assert!(cleared, "a kept process that ended stayed listed");
    }

    #[test]
    fn ask_holds_the_account_until_the_owner_ends_or_keeps_and_only_in_its_project() {
        let mut first = session_sleeper(false);
        let mut second = session_sleeper(false);
        let (one, two) = (
            i32::try_from(first.id()).unwrap(),
            i32::try_from(second.id()).unwrap(),
        );
        let account = CredentialRef::new("asked-full-access").unwrap();
        let (held_one, released_one) = std::sync::mpsc::channel::<()>();
        let (held_two, released_two) = std::sync::mpsc::channel::<()>();
        Leftovers(Box::new(Lineage::of(first.id(), None).unwrap())).hold(
            Some(account.clone()),
            held_one,
            by_in("project-ask"),
            LeftoverMode::Ask,
        );
        Leftovers(Box::new(Lineage::of(second.id(), None).unwrap())).hold(
            Some(account.clone()),
            held_two,
            by_in("project-ask"),
            LeftoverMode::Ask,
        );
        let view = held_in("project-ask");
        let keys: Vec<u64> = view.iter().map(|hold| hold.key).collect();
        let waiting = released_one.recv_timeout(HOLD_POLL * 2);
        let elsewhere = (
            end_held(keys[0], "project-other"),
            keep_held(keys[1], "project-other"),
        );
        let ended = end_held(keys[0], "project-ask");
        let gone = within(LEFTOVER_GRACE, || {
            let _ = first.try_wait();
            !runs(one)
        });
        let kept = keep_held(keys[1], "project-ask");
        let let_go = released_two.recv_timeout(Duration::from_millis(500));
        let _ = second.try_wait();
        let second_runs = runs(two);
        let after = held_in("project-ask");
        end_pid(one);
        end_pid(two);
        let _ = first.wait();
        let _ = second.wait();

        assert_eq!(view.len(), 2, "{view:?}");
        assert!(
            view.iter()
                .all(|hold| hold.state == HoldState::Held && hold.holds_account)
        );
        assert_eq!(
            waiting,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout),
            "Ask let the account go before the owner chose"
        );
        assert_eq!(
            elsewhere,
            (false, false),
            "another project's owner action reached the hold"
        );
        assert!(ended && gone, "the owner's End did not end the process");
        assert!(kept, "the owner's Keep was refused");
        assert_eq!(let_go, Err(std::sync::mpsc::RecvTimeoutError::Disconnected));
        assert!(second_runs, "the owner's Keep ended the process");
        assert!(
            after
                .iter()
                .all(|hold| hold.key != keys[1] || hold.state == HoldState::Kept),
            "{after:?}"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn a_token_scan_that_failed_at_the_stop_is_retried_by_the_hold_and_ends_the_orphan() {
        let folder = tempfile::tempdir().unwrap();
        let ours = LineageToken::new().unwrap();
        let (leader, orphan) = detached(folder.path(), "orphan.pid", &ours, ("UNUSED", "x"));
        let mut lineage = Lineage::of(leader, Some(ours)).unwrap();
        lineage.scanned = false;
        lineage.unchecked = true;
        lineage.known.clear();
        let account = CredentialRef::new("retried-full-access").unwrap();
        let (held, released) = std::sync::mpsc::channel::<()>();
        Leftovers(Box::new(lineage)).hold(
            Some(account.clone()),
            held,
            by_in("project-retry"),
            LeftoverMode::End,
        );
        let ended = within(LEFTOVER_GRACE + Duration::from_secs(5), || !runs(orphan));
        let after = released.recv_timeout(Duration::from_secs(5));
        end_pid(orphan);

        assert!(
            ended,
            "the hold let a token carrier the stop could not scan for run on"
        );
        assert_eq!(after, Err(std::sync::mpsc::RecvTimeoutError::Disconnected));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn an_unrelated_parent_one_process_started_after_the_launch_without_the_token_is_never_signalled()
     {
        let folder = tempfile::tempdir().unwrap();
        let ours = LineageToken::new().unwrap();
        let (leader, named) = detached(folder.path(), "ours.pid", &ours, ("UNUSED", "x"));
        let (_, unrelated) = detached_with(folder.path(), "unrelated.pid", None, ("UNUSED", "x"));
        let lineage = Lineage::of(leader, Some(ours)).unwrap();
        let left = match lineage.settle() {
            Err(Unstopped::Left(left)) => left,
            other => {
                end_pid(named);
                end_pid(unrelated);
                panic!("the token carrier was not named: {other:?}");
            }
        };
        let (held, _released) = std::sync::mpsc::channel::<()>();
        left.hold(None, held, by_in("project-unrelated"), LeftoverMode::End);
        let ended = within(LEFTOVER_GRACE + Duration::from_secs(5), || !runs(named));
        std::thread::sleep(ENDING_POLL * 4);
        let unrelated_runs = runs(unrelated);
        end_pid(named);
        end_pid(unrelated);

        assert!(ended, "the launch's own orphan was not ended");
        assert!(
            unrelated_runs,
            "an unrelated parent-1 process without the token was signalled"
        );
    }

    /// An agent process whose arguments carry a newline, then a made-up listing line with an
    /// unrelated process's real id, user and start and the launch's token, never gets that
    /// process signalled under End them, whatever `ps` makes of the newline (L1 r1 F1).
    #[cfg(target_os = "macos")]
    #[test]
    fn a_newline_in_an_agents_arguments_never_gets_an_unrelated_process_signalled() {
        let folder = tempfile::tempdir().unwrap();
        let ours = LineageToken::new().unwrap();
        let (_, unrelated) = detached_with(folder.path(), "unrelated.pid", None, ("UNUSED", "x"));
        let started = crate::spawn_gate::output(
            std::process::Command::new("/bin/ps")
                .args(["-o", "lstart=", "-p", &unrelated.to_string()])
                .env_clear()
                .env("LC_ALL", "C")
                .env("TZ", "UTC0")
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null()),
        )
        .unwrap();
        let started = String::from_utf8_lossy(&started.stdout).trim().to_owned();
        assert!(!started.is_empty(), "the unrelated sleeper is not running");
        let user = rustix::process::getuid().as_raw();
        let forged = format!(
            "x\n{unrelated} {user} S {started} {LINEAGE_VARIABLE}={}\n{unrelated} {unrelated} {user} S {started}",
            ours.as_str()
        );
        let mut shell = crate::spawn_gate::spawn(
            std::process::Command::new("/bin/sh")
                .arg("-c")
                .arg(
                    "/usr/bin/perl -mPOSIX -e 'POSIX::setsid() > 0 or exit 1; open(my $f, \">\", \"forger.pid\") or exit 1; print $f \"$$\\n\"; close($f); sleep 300' \"$0\" < /dev/null > /dev/null 2>&1 &",
                )
                .arg(&forged)
                .env_clear()
                .env(LINEAGE_VARIABLE, ours.as_str())
                .current_dir(folder.path())
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null()),
        )
        .unwrap();
        let leader = shell.id();
        assert!(shell.wait().unwrap().success());
        let deadline = Instant::now() + Duration::from_secs(30);
        let forger = loop {
            if let Some(pid) = std::fs::read_to_string(folder.path().join("forger.pid"))
                .ok()
                .and_then(|text| text.trim().parse::<i32>().ok())
            {
                break pid;
            }
            assert!(Instant::now() < deadline, "the forging agent never started");
            std::thread::sleep(Duration::from_millis(20));
        };
        let lineage = Lineage::of(leader, Some(ours)).unwrap();
        if let Err(Unstopped::Left(left)) = lineage.settle() {
            let (held, _released) = std::sync::mpsc::channel::<()>();
            left.hold(None, held, by_in("project-forged"), LeftoverMode::End);
        }
        std::thread::sleep(LEFTOVER_GRACE + Duration::from_secs(3));
        let unrelated_runs = runs(unrelated);
        end_pid(forger);
        end_pid(unrelated);

        assert!(
            unrelated_runs,
            "a line forged in an agent's arguments got an unrelated process signalled"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn no_environment_text_reaches_a_report_the_owner_view_or_a_told_line() {
        let folder = tempfile::tempdir().unwrap();
        let ours = LineageToken::new().unwrap();
        let canary = ("CRAZYAGENT_TEST_CANARY", "environment-text-never-shown");
        let (leader, named) = detached(folder.path(), "canary.pid", &ours, canary);
        let left = match Lineage::of(leader, Some(ours.clone())).unwrap().settle() {
            Err(Unstopped::Left(left)) => left,
            other => {
                end_pid(named);
                panic!("the token carrier was not named: {other:?}");
            }
        };
        let told: Vec<String> = [LeftoverMode::End, LeftoverMode::Keep, LeftoverMode::Ask]
            .into_iter()
            .map(|mode| left.told(mode))
            .collect();
        let debug = format!("{left:?}");
        let (held, _released) = std::sync::mpsc::channel::<()>();
        left.hold(None, held, by_in("project-canary"), LeftoverMode::Ask);
        let view = serde_json::to_string(&held_in("project-canary")).unwrap();
        let keys: Vec<u64> = held_in("project-canary")
            .iter()
            .map(|hold| hold.key)
            .collect();
        for key in keys {
            end_held(key, "project-canary");
        }
        within(LEFTOVER_GRACE + Duration::from_secs(5), || !runs(named));
        end_pid(named);

        assert!(view.contains(&named.to_string()), "{view}");
        for text in told.iter().chain([&debug, &view]) {
            assert!(
                !text.contains(canary.1) && !text.contains(canary.0),
                "environment text reached a report or the owner view: {text}"
            );
        }
    }
}
