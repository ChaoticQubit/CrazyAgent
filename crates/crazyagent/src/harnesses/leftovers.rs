use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;
#[cfg(unix)]
use std::time::Instant;

/// The plain code of a stop that left processes of a Full access agent running.
pub const LEFT: &str = "harness_full_access_left";
/// The plain code of a stop whose warden did not confirm its sandbox empty, or whose
/// processes could not be read.
pub const UNCERTAIN: &str = "harness_cleanup_uncertain";

/// How long a stop waits, after the agent's group is killed, for the processes it found
/// before the kill to end before it names the ones still running.
#[cfg(unix)]
const SETTLE: Duration = Duration::from_secs(2);
#[cfg(unix)]
const SETTLE_POLL: Duration = Duration::from_millis(100);
/// How often a held account checks again whether the processes holding it have ended.
const HOLD_POLL: Duration = Duration::from_secs(2);
/// The most processes one report names; the rest are counted.
const NAMED_MAX: usize = 8;
const PROGRAM_CHARS_MAX: usize = 32;
#[cfg(unix)]
const TABLE_ROWS_MAX: usize = 65_536;

/// Why a stop could not report every process of an agent ended.
#[derive(Debug)]
pub enum Unstopped {
    /// The run's warden did not confirm its sandbox empty, or the processes of a Full
    /// access agent could not be read.
    Uncertain,
    /// Processes of a Full access agent still run after its stop.
    Left(Leftovers),
}

impl Unstopped {
    /// Names the failure as a plain code. Input: the failure. Output: [`UNCERTAIN`] or [`LEFT`].
    pub fn code(&self) -> &'static str {
        match self {
            Self::Uncertain => UNCERTAIN,
            Self::Left(_) => LEFT,
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
/// exited, is not found.
#[derive(Debug)]
pub(crate) struct Lineage {
    leader: i32,
    known: Vec<Process>,
}

/// The processes a Full access agent left running after its stop, which no Seatbelt
/// warden can end, never empty.
#[derive(Debug)]
pub struct Leftovers(Lineage);

impl Leftovers {
    /// Says plainly which processes still run and that Full access cannot stop them.
    /// Input: the leftovers. Output: one sentence naming at most `NAMED_MAX` of them by id
    /// and program, counting the rest.
    pub fn plain(&self) -> String {
        assert!(!self.0.known.is_empty());
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
        format!(
            "Full access cannot stop what the agent started outside its terminal; still running: {}{counted}. Its account stays held until they end",
            named.join(", ")
        )
    }

    /// Keeps `held`, such as the agent's credential copy and account lease, until every
    /// process of the agent has ended, then drops it; the engine's exit ends the hold too.
    /// When no watching thread starts, `held` is kept for the engine's life instead.
    /// Inputs: the leftovers and what they hold. Output: none.
    pub fn hold<T: Send + 'static>(self, held: T) {
        let slot = Arc::new(Mutex::new(Some(held)));
        let watched = Arc::clone(&slot);
        let mut lineage = self.0;
        let started = std::thread::Builder::new()
            .name("full-access-hold".into())
            .spawn(move || {
                while lineage.refresh().is_err() || !lineage.known.is_empty() {
                    std::thread::sleep(HOLD_POLL);
                }
                drop(
                    watched
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .take(),
                );
            });
        if started.is_err()
            && let Some(held) = slot.lock().unwrap_or_else(PoisonError::into_inner).take()
        {
            std::mem::forget(held);
        }
    }
}

impl Lineage {
    /// Finds a Full access agent's processes before its group is killed. Input: the
    /// group's leader, whose id is also its session's. Output: the lineage, or
    /// [`Unstopped::Uncertain`] when the processes cannot be read.
    #[cfg(unix)]
    pub(crate) fn of(leader: u32) -> Result<Self, Unstopped> {
        let leader = i32::try_from(leader)
            .ok()
            .filter(|leader| *leader > 1)
            .ok_or(Unstopped::Uncertain)?;
        let mut lineage = Self {
            leader,
            known: Vec::new(),
        };
        lineage.refresh()?;
        Ok(lineage)
    }

    /// Refuses where no process table is read; no Full access agent runs there.
    /// Input: unused. Output: [`Unstopped::Uncertain`].
    #[cfg(not(unix))]
    pub(crate) fn of(_leader: u32) -> Result<Self, Unstopped> {
        Err(Unstopped::Uncertain)
    }

    /// Waits, after the group is killed, up to `SETTLE` for every process found to end.
    /// Input: the lineage. Output: Ok once none runs, the [`Leftovers`] still running at
    /// the deadline, or [`Unstopped::Uncertain`] when the processes cannot be read.
    #[cfg(unix)]
    pub(crate) fn settle(mut self) -> Result<(), Unstopped> {
        let until = Instant::now() + SETTLE;
        loop {
            self.refresh()?;
            if self.known.is_empty() {
                return Ok(());
            }
            if Instant::now() >= until {
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
    /// start, then adds this user's processes in the leader's session and those whose parent
    /// is kept. Input: the lineage. Output: Ok, or uncertain when `ps` cannot be read.
    #[cfg(unix)]
    fn refresh(&mut self) -> Result<(), Unstopped> {
        let table = table()?;
        let mut kept: Vec<Process> = self
            .known
            .iter()
            .filter(|known| table.iter().any(|(process, _)| process == *known))
            .cloned()
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

/// Reads this user's running processes, other than the engine, with `ps`; a zombie has
/// ended and is left out. Output: each process with its parent's id, or uncertain when
/// `ps` fails or its listing is out of bounds.
#[cfg(unix)]
fn table() -> Result<Vec<(Process, i32)>, Unstopped> {
    use std::process::{Command, Stdio};
    let output = crate::spawn_gate::output(
        Command::new("/bin/ps")
            .env_clear()
            .env("LC_ALL", "C")
            .env("TZ", "UTC0")
            .args(["-A", "-ww", "-o", "pid=,ppid=,uid=,stat=,lstart=,comm="])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null()),
    )
    .map_err(|_| Unstopped::Uncertain)?;
    if !output.status.success() {
        return Err(Unstopped::Uncertain);
    }
    let listing = String::from_utf8_lossy(&output.stdout);
    let user = rustix::process::getuid().as_raw();
    let own = i32::try_from(std::process::id()).map_err(|_| Unstopped::Uncertain)?;
    parse(&listing, user, own)
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
        let report = Leftovers(Lineage { leader: 2, known }).plain();
        assert!(
            report.contains("still running: 2 sleep, 3 sleep,"),
            "{report}"
        );
        assert!(report.contains("9 sleep and 2 more."), "{report}");
        assert!(!report.contains("10 sleep"), "{report}");
    }

    #[test]
    #[should_panic]
    fn an_empty_report_trips_its_invariant() {
        let _ = Leftovers(Lineage {
            leader: 2,
            known: Vec::new(),
        })
        .plain();
    }
}
