use crate::protocol::CredentialRef;
use std::collections::BTreeMap;
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
/// listing before the kill and the settling after it, whose last listing may start one
/// poll after `SETTLE` ends.
#[cfg(unix)]
pub(crate) const STOP_TIME: Duration = TABLE_TIME
    .saturating_add(TABLE_GRACE)
    .saturating_mul(2)
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

/// One process of an agent's lineage: its id, the start identity a reused id does not
/// share, and its program's name as the owner reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Process {
    pid: i32,
    started: String,
    program: String,
}

/// The processes of a Full access agent the engine can still find: its leader, every
/// process in the leader's session, and every process whose parent is one of them. A
/// process that left both before it was seen, such as a daemon whose parent already
/// exited, is not found. `table` is the command that lists the processes; `unchecked`
/// tells that a listing failed, so the lineage may miss processes; `pending` is a listing
/// still held up past its bound, which a later read waits out instead of starting another;
/// `sandboxed` tells the agent ran in a sandbox no warden empties, not at Full access.
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
}

/// Where a listing thread reports its output, or none when it failed or ran out of bounds.
#[cfg(unix)]
type Listing = std::sync::mpsc::Receiver<Option<Vec<u8>>>;

/// The processes an agent no warden empties left running after its stop, at Full access
/// or in a sandboxed pane its host ran without the keeper, or a stop whose processes could
/// not be listed; never both empty and checked.
#[derive(Debug)]
pub struct Leftovers(Lineage);

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
        let mut lineage = self.0;
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
    /// Finds a Full access agent's processes before its group is killed. Input: the
    /// group's leader, whose id is also its session's. Output: the lineage, marked
    /// unchecked when the processes cannot be listed, or [`Unstopped::Uncertain`] for a
    /// leader that is no process id.
    #[cfg(unix)]
    pub(crate) fn of(leader: u32) -> Result<Self, Unstopped> {
        Self::reading(leader, PS_TABLE)
    }

    /// Finds, before its group is killed, the processes of a sandboxed pane no warden
    /// empties, such as a native lead's in its own server, as [`Self::of`] does. Input: the
    /// pane's first process, whose id is also its session's. Output: as for [`Self::of`].
    pub(crate) fn of_sandboxed(leader: u32) -> Result<Self, Unstopped> {
        Self::of(leader).map(|lineage| Self {
            sandboxed: true,
            ..lineage
        })
    }

    /// Reads the process table again while the agent runs, and once more just before its
    /// stop, so a process that later leaves the leader's session and loses its parent stays
    /// known (SECAUDIT-SETSID-LEFTOVER). Only the last read decides whether the lineage is
    /// unchecked; a failed read keeps what was known. Input: the lineage. Output: none.
    pub(crate) fn follow(&mut self) {
        self.unchecked = self.refresh().is_err();
    }

    /// Finds a lineage as [`Self::of`] does with the processes listed by `table`.
    /// Inputs: the leader and the listing command. Output: as for [`Self::of`].
    #[cfg(unix)]
    fn reading(leader: u32, table: &'static [&'static str]) -> Result<Self, Unstopped> {
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
        };
        lineage.unchecked = lineage.refresh().is_err();
        Ok(lineage)
    }

    /// Refuses where no process table is read; no Full access agent runs there.
    /// Input: unused. Output: [`Unstopped::Uncertain`].
    #[cfg(not(unix))]
    pub(crate) fn of(_leader: u32) -> Result<Self, Unstopped> {
        Err(Unstopped::Uncertain)
    }

    /// Waits, after the group is killed, up to `SETTLE` for every process found to end.
    /// Input: the lineage. Output: Ok once a listing shows none running, else the
    /// [`Leftovers`] still running at the deadline or, once a listing failed, unchecked.
    #[cfg(unix)]
    pub(crate) fn settle(mut self) -> Result<(), Unstopped> {
        let until = Instant::now() + SETTLE;
        loop {
            self.unchecked |= self.refresh().is_err();
            if self.known.is_empty() && !self.unchecked {
                return Ok(());
            }
            if self.known.is_empty() || Instant::now() >= until {
                return Err(Unstopped::Left(Leftovers(self)));
            }
            std::thread::sleep(SETTLE_POLL);
        }
    }

    /// Refuses where no process table is read. Input: unused. Output: uncertain.
    #[cfg(not(unix))]
    pub(crate) fn settle(self) -> Result<(), Unstopped> {
        Err(Unstopped::Uncertain)
    }

    /// Reads the process table again: keeps each known process still running with the same
    /// start, under the program it runs now, then adds this user's processes in the leader's
    /// session and those whose parent is kept. Input: the lineage. Output: Ok, or uncertain
    /// when `ps` cannot be read.
    #[cfg(unix)]
    fn refresh(&mut self) -> Result<(), Unstopped> {
        let table = table(self.table, &mut self.pending)?;
        let mut kept: Vec<Process> = table
            .iter()
            .filter(|(process, _)| self.known.iter().any(|known| same(known, process)))
            .map(|(process, _)| process.clone())
            .collect();
        let session = rustix::process::Pid::from_raw(self.leader);
        let members: Vec<bool> = table
            .iter()
            .map(|(process, _)| {
                rustix::process::Pid::from_raw(process.pid)
                    .is_some_and(|pid| rustix::process::getsid(Some(pid)).ok() == session)
            })
            .collect();
        loop {
            let found = table
                .iter()
                .zip(&members)
                .find(|((process, parent), member)| {
                    !kept.iter().any(|known| known.pid == process.pid)
                        && (**member || kept.iter().any(|known| known.pid == *parent))
                });
            match found {
                Some(((process, _), _)) => kept.push(process.clone()),
                None => break,
            }
        }
        assert!(kept.len() <= table.len());
        self.known = kept;
        Ok(())
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
    let output = read_bounded(argv, pending)?;
    let listing = String::from_utf8_lossy(&output);
    let user = rustix::process::getuid().as_raw();
    let own = i32::try_from(std::process::id()).map_err(|_| Unstopped::Uncertain)?;
    parse(&listing, user, own)
}

/// Runs a listing command with a cleared environment in a thread of its own and takes its
/// output, at most `TABLE_BYTES_MAX` bytes within `TABLE_TIME` counted from before the
/// spawn, so neither a spawn held up by a stalled exec nor a stalled or flooding listing
/// holds up a stop. A listing held up past its bound stays `pending`, and no other starts
/// until it has ended, so a lasting stall holds up one listing thread, not one per read.
/// Inputs: the program with its arguments and the pending listing. Output: the output of a
/// run that exited 0 within both bounds, or uncertain.
#[cfg(unix)]
fn read_bounded(
    argv: &'static [&'static str],
    pending: &mut Option<Listing>,
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
            let _ = sender.send(run_listing(argv, deadline));
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
/// deadline or `TABLE_BYTES_MAX`, a child whose spawn returned late included.
/// Inputs: the program with its arguments and the deadline. Output: the output of a run
/// that exited 0 within both bounds, or none.
#[cfg(unix)]
fn run_listing(argv: &[&str], deadline: Instant) -> Option<Vec<u8>> {
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
                let read = stdout.take(TABLE_BYTES_MAX + 1).read_to_end(&mut bytes);
                let _ = sender.send(read.map(|_| bytes));
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
        let report = Leftovers(Lineage {
            leader: 2,
            known,
            table: PS_TABLE,
            unchecked: false,
            pending: None,
            sandboxed: false,
        })
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
        let lineage = Lineage::of(leader.id()).unwrap();
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
            let read = Lineage::reading(std::process::id(), table);
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
        assert!(Lineage::of(std::process::id()).is_ok());
    }

    #[test]
    #[should_panic]
    fn an_empty_report_trips_its_invariant() {
        let _ = Leftovers(Lineage {
            leader: 2,
            known: Vec::new(),
            table: PS_TABLE,
            unchecked: false,
            pending: None,
            sandboxed: false,
        })
        .plain();
    }

    #[test]
    fn an_unreadable_process_table_holds_the_account_only_until_it_can_be_checked() {
        let failing: &'static [&'static str] = &["/usr/bin/false"];
        let lineage = match Lineage::reading(std::process::id(), failing) {
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
        let lineage = Lineage::of(leftover.id()).unwrap();
        let account = CredentialRef::new("signed-out-full-access").unwrap();
        let other = CredentialRef::new("other-full-access").unwrap();
        let (held, released) = std::sync::mpsc::channel::<()>();
        Leftovers(lineage).hold(Some(account.clone()), held);

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
}
