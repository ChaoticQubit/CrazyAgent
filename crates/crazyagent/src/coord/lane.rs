use super::AgentId;
use super::settings::{CoordSettings, HeavyLane, SharedResource, resource_name_valid};
#[cfg(unix)]
use super::store::Change;
use super::store::{failure, state};
use crate::engine::EngineError;
use crate::protocol::ProjectId;
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The folder in an agent's own temporary folder (`<agent>/tmp`) that holds its lane
/// tickets, made by the engine at every launch and named to the agent in its Heavy commands
/// section; the lane command takes it with `--folder`.
pub const FOLDER: &str = "crazyagent-lane";
/// The engine subcommand an agent runs a heavy command through.
pub const COMMAND: &str = "lane";
/// How the lane command is used.
pub const USAGE: &str = "usage: crazyagent lane --folder LANE_DIR [--uses NAME]... [--label TEXT] -- PROGRAM [ARGUMENT ...]";
/// The exit code of a lane command that could not run its command: a usage mistake, a
/// refused ticket or a wait past `WAIT_MAX`. 125 as `env` and `timeout` use it, so it
/// never reads as the command's own code.
pub const LANE_FAILED: i32 = 125;
/// The exit code when the command itself could not be started.
pub const NOT_STARTED: i32 = 127;
/// The most arguments one lane command takes, its own options included.
pub const ARGUMENTS_MAX: usize = 4096;
/// The most tickets the engine reads in one agent's folder each tick.
pub const TICKETS_PER_AGENT_MAX: usize = 8;
/// The most tickets one project's lane holds; a later one waits unread and runs at once
/// after `SILENCE_MAX`.
pub const TICKETS_PER_PROJECT_MAX: usize = 64;
/// The most agent folders the engine reads for one project each tick.
pub const HOLDERS_MAX: usize = 256;
/// The longest label, in bytes.
pub const LABEL_BYTES_MAX: usize = 64;
/// The most shared resources one heavy command names.
pub const USES_MAX: usize = 4;
/// How often the engine rewrites a waiting ticket's line, so the lane command knows the
/// engine still reads it.
pub const HEARTBEAT: Duration = Duration::from_secs(10);
/// How long the lane command waits without a word from the engine before it runs its
/// command at once: an engine that restarts, stops or does not read this agent never holds
/// a command for ever.
pub const SILENCE_MAX: Duration = Duration::from_secs(60);
/// How often the lane command looks for the engine's answer.
pub const POLL: Duration = Duration::from_millis(500);
/// The longest a heavy command waits for its turn before the lane command gives up.
pub const WAIT_MAX: Duration = Duration::from_secs(24 * 60 * 60);

const TICKET_BYTES_MAX: u64 = 2048;
const LINE_BYTES_MAX: u64 = 512;
const STORED_MAX: usize = 4 * TICKETS_PER_PROJECT_MAX;
const POLLS_MAX: u64 = (WAIT_MAX.as_millis() / POLL.as_millis()) as u64;
const SILENCE_MAX_MS: i64 = SILENCE_MAX.as_millis() as i64;
const HEARTBEAT_MS: i64 = HEARTBEAT.as_millis() as i64;
#[cfg(unix)]
const TICKET: &str = ".lane";
#[cfg(unix)]
const PART: &str = ".lpart";
#[cfg(unix)]
const GO: &str = ".go";
#[cfg(unix)]
const NO: &str = ".no";
#[cfg(unix)]
const WAIT: &str = ".wait";
#[cfg(unix)]
const WAIT_PART: &str = ".wpart";
#[cfg(unix)]
const SUFFIXES: [&str; 6] = [TICKET, PART, GO, NO, WAIT, WAIT_PART];
const NO_LANE: &str = "crazyagent lane: no heavy lane here (no lane folder of a CrazyAgent agent), so the command runs at once.";
#[cfg(unix)]
const SILENT: &str =
    "crazyagent lane: CrazyAgent has not answered for 60 seconds, so the command runs at once.";
#[cfg(unix)]
const TOO_LONG: &str = "crazyagent lane: the command waited 24 hours for its turn and did not run.";
const UNREADABLE: &str = "The heavy lane could not read this command's ticket.";
#[cfg(unix)]
const GRANTED: &[u8] = b"go\n";

const _: () = assert!(SILENCE_MAX.as_secs() > 2 * HEARTBEAT.as_secs());
const _: () = assert!(SILENCE_MAX_MS > 2 * HEARTBEAT_MS && HEARTBEAT_MS > 0);
const _: () = assert!(POLL.as_millis() < HEARTBEAT.as_millis() && POLLS_MAX > 0);
const _: () = assert!(TICKETS_PER_AGENT_MAX <= TICKETS_PER_PROJECT_MAX);
const _: () = assert!(LABEL_BYTES_MAX < TICKET_BYTES_MAX as usize);

/// What one heavy command asks the lane for, as the lane command writes it in its ticket:
/// the shared resources it uses and the label the Team view shows. It never holds the
/// command's arguments, which may carry secrets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Ticket {
    pub uses: Vec<String>,
    pub label: String,
}

impl Ticket {
    /// Checks a ticket: at most `USES_MAX` valid resource names, none twice, and a valid
    /// label. Takes the ticket. Returns the answer.
    pub fn valid(&self) -> bool {
        self.uses.len() <= USES_MAX
            && self
                .uses
                .iter()
                .enumerate()
                .all(|(at, name)| resource_name_valid(name) && !self.uses[..at].contains(name))
            && label_valid(&self.label)
    }
}

/// Checks a label: 1 to `LABEL_BYTES_MAX` bytes of letters, digits, spaces, `.`, `_`, `-`,
/// `:` and `/`, not starting or ending with a space. Takes the label. Returns the answer.
pub fn label_valid(label: &str) -> bool {
    !label.is_empty()
        && label.len() <= LABEL_BYTES_MAX
        && label.trim() == label
        && label.bytes().all(|b| {
            b.is_ascii_alphanumeric() || matches!(b, b' ' | b'.' | b'_' | b'-' | b':' | b'/')
        })
}

/// The label a heavy command gets when it names none: its program's file name, cut to
/// `LABEL_BYTES_MAX` bytes of the label characters, or "command" when nothing is left.
/// Its arguments never count. Takes the program. Returns the label.
pub fn program_label(program: &OsStr) -> String {
    let name = Path::new(program)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let kept: String = name
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        .take(LABEL_BYTES_MAX)
        .collect();
    let label = if kept.is_empty() {
        "command".to_owned()
    } else {
        kept
    };
    assert!(label_valid(&label));
    label
}

/// One heavy command as the lane command was asked to run it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub folder: Option<PathBuf>,
    pub uses: Vec<String>,
    pub label: Option<String>,
    pub program: OsString,
    pub arguments: Vec<OsString>,
}

/// Reads the lane command's arguments: `--folder LANE_DIR`, the absolute lane folder the
/// agent's Heavy commands section names, `--uses NAME` once per shared resource, an optional
/// `--label TEXT`, then `--` and the command. Takes the arguments after `lane`. Returns
/// the request, or the usage line with what was wrong.
pub fn parse(arguments: &[OsString]) -> Result<Request, String> {
    let refuse = |what: &str| format!("crazyagent lane: {what}\n{USAGE}");
    if arguments.len() > ARGUMENTS_MAX {
        return Err(refuse("too many arguments"));
    }
    let mut folder: Option<PathBuf> = None;
    let mut uses: Vec<String> = Vec::new();
    let mut label = None;
    let mut at = 0;
    while at < arguments.len() {
        let word = arguments[at].to_str().unwrap_or_default();
        let value = || arguments.get(at + 1).and_then(|value| value.to_str());
        match word {
            "--" => {
                let Some(program) = arguments.get(at + 1) else {
                    return Err(refuse("no command after --"));
                };
                let request = Request {
                    folder,
                    uses,
                    label,
                    program: program.clone(),
                    arguments: arguments[at + 2..].to_vec(),
                };
                let ticket = Ticket {
                    uses: request.uses.clone(),
                    label: request
                        .label
                        .clone()
                        .unwrap_or_else(|| program_label(&request.program)),
                };
                return if ticket.valid() {
                    Ok(request)
                } else {
                    Err(refuse("give at most 4 shared resources, each once"))
                };
            }
            "--folder" => {
                let Some(path) = arguments
                    .get(at + 1)
                    .map(PathBuf::from)
                    .filter(|path| path.is_absolute() && folder.is_none())
                else {
                    return Err(refuse("--folder takes the absolute lane folder, once"));
                };
                folder = Some(path);
                at += 2;
            }
            "--uses" => {
                let Some(name) = value().filter(|name| resource_name_valid(name)) else {
                    return Err(refuse(
                        "--uses takes a shared resource name of lower case letters, digits, '.', '_' or '-'",
                    ));
                };
                uses.push(name.to_owned());
                at += 2;
            }
            "--label" => {
                let Some(text) = value().filter(|text| label_valid(text)) else {
                    return Err(refuse(
                        "--label takes up to 64 letters, digits, spaces, '.', '_', '-', ':' or '/'",
                    ));
                };
                label = Some(text.to_owned());
                at += 2;
            }
            _ => return Err(refuse("put -- before the command")),
        }
    }
    Err(refuse("no command after --"))
}

/// One ticket in turn order as the grant reads it: the shared resources it uses and
/// whether it already runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Queued {
    pub uses: Vec<String>,
    pub granted: bool,
}

/// Decides which tickets run, in turn order (F28). A ticket that runs keeps running. A
/// waiting one runs when the lane has a free slot under `lane`, each resource it names has
/// a free slot, no earlier ticket waits for a full lane, and no earlier waiting ticket names
/// one of those resources; so no later command overtakes an earlier one on a resource they
/// share, which starves no command that needs several, and a command that shares nothing
/// with a waiting one does not wait behind it. A resource the settings do not name has no
/// slot. Takes the tickets in turn order, the lane option and the shared
/// resources. Returns, for each ticket, whether it runs.
pub fn grant(queue: &[Queued], lane: HeavyLane, resources: &[SharedResource]) -> Vec<bool> {
    assert!(queue.len() <= STORED_MAX);
    let slots = |name: &str| {
        resources
            .iter()
            .find(|resource| resource.name == name)
            .map_or(0, |resource| usize::from(resource.slots))
    };
    let before = queue.iter().filter(|ticket| ticket.granted).count();
    let mut lane_used = before;
    let mut used: BTreeMap<&str, usize> = BTreeMap::new();
    for ticket in queue.iter().filter(|ticket| ticket.granted) {
        for name in &ticket.uses {
            *used.entry(name.as_str()).or_default() += 1;
        }
    }
    let mut lane_blocked = false;
    let mut blocked: BTreeSet<&str> = BTreeSet::new();
    let mut runs = Vec::with_capacity(queue.len());
    for ticket in queue {
        if ticket.granted {
            runs.push(true);
            continue;
        }
        let lane_full = lane.slots().is_some_and(|slots| lane_used >= slots);
        let full: Vec<&str> = ticket
            .uses
            .iter()
            .map(String::as_str)
            .filter(|name| used.get(name).copied().unwrap_or(0) >= slots(*name))
            .collect();
        let free = !lane_full
            && !lane_blocked
            && full.is_empty()
            && ticket
                .uses
                .iter()
                .all(|name| !blocked.contains(name.as_str()));
        if free {
            lane_used += 1;
            for name in &ticket.uses {
                *used.entry(name.as_str()).or_default() += 1;
            }
        } else {
            lane_blocked |= lane_full;
            blocked.extend(ticket.uses.iter().map(String::as_str));
        }
        runs.push(free);
    }
    assert_eq!(runs.len(), queue.len());
    let after = runs.iter().filter(|runs| **runs).count();
    assert_eq!(after, lane_used);
    assert!(lane.slots().is_none_or(|slots| after <= slots.max(before)));
    runs
}

/// The line a waiting heavy command shows: what it waits for and how many commands run and
/// wait ahead of it. Takes the lane option, the commands that run, those that wait ahead of
/// it and the shared resources it uses. Returns the line, at most `LINE_BYTES_MAX` bytes.
pub fn waiting_line(lane: HeavyLane, running: usize, ahead: usize, uses: &[String]) -> String {
    let resources = if uses.is_empty() {
        String::new()
    } else {
        format!(" It uses {}.", uses.join(", "))
    };
    let line = format!(
        "Waiting for its turn in the heavy lane ({}): {running} running, {ahead} waiting ahead.{resources}",
        lane.words().to_lowercase()
    );
    assert!(line.len() as u64 <= LINE_BYTES_MAX);
    line
}

/// Says why a ticket cannot wait in a project's lane, or None when it can: a ticket that
/// is not valid, or one naming a shared resource the project's settings do not have.
/// Takes the ticket and the settings. Returns the plain reason, or None.
pub fn refusal(ticket: &Ticket, settings: &CoordSettings) -> Option<String> {
    if !ticket.valid() {
        return Some(UNREADABLE.to_owned());
    }
    let unknown: Vec<&str> = ticket
        .uses
        .iter()
        .map(String::as_str)
        .filter(|name| {
            !settings
                .shared_resources
                .iter()
                .any(|resource| resource.name == *name)
        })
        .collect();
    if unknown.is_empty() {
        return None;
    }
    let known: Vec<&str> = settings
        .shared_resources
        .iter()
        .map(|resource| resource.name.as_str())
        .collect();
    let named = if known.is_empty() {
        "This project has no shared resources.".to_owned()
    } else {
        format!("Its shared resources are: {}.", known.join(", "))
    };
    Some(format!(
        "This project has no shared resource named {}. {named}",
        unknown.join(", ")
    ))
}

/// One heavy command waiting or running, as the Team view lists it: the agent that runs
/// it, its label, the shared resources it uses, and since when it runs or waits.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaneEntry {
    pub agent: AgentId,
    pub label: String,
    pub uses: Vec<String>,
    pub since_ms: i64,
}

/// A project's heavy lane at one moment: the commands that run and those that wait, each
/// in turn order. An engine from before F28 sends none, which reads as empty.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaneView {
    pub running: Vec<LaneEntry>,
    pub waiting: Vec<LaneEntry>,
}

struct Stored {
    agent: AgentId,
    ticket: String,
    uses: Vec<String>,
    label: String,
    created_ms: i64,
    seen_ms: i64,
    granted_ms: Option<i64>,
}

fn stored(db: &Connection, project: &ProjectId) -> Result<Vec<Stored>, EngineError> {
    let mut statement = db
        .prepare(
            "SELECT agent,ticket,uses,label,created_ms,seen_ms,granted_ms FROM coord_lane
             WHERE project=?1 ORDER BY seq LIMIT ?2",
        )
        .map_err(state)?;
    let rows = statement
        .query_map(params![project.as_str(), STORED_MAX as i64], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, Option<i64>>(6)?,
            ))
        })
        .map_err(state)?;
    let mut found = Vec::new();
    for row in rows {
        let (agent, ticket, uses, label, created_ms, seen_ms, granted_ms) = row.map_err(state)?;
        let uses: Vec<String> =
            serde_json::from_str(&uses).map_err(|_| failure("coord_lane_invalid"))?;
        found.push(Stored {
            agent,
            ticket,
            uses,
            label,
            created_ms,
            seen_ms,
            granted_ms,
        });
    }
    assert!(found.len() <= STORED_MAX);
    Ok(found)
}

/// Reads a project's heavy lane for the Team view. Takes the database and the project.
/// Returns the commands that run and those that wait, in turn order, or the SQLite failure.
pub fn view(db: &Connection, project: &ProjectId) -> Result<LaneView, EngineError> {
    let mut view = LaneView::default();
    for row in stored(db, project)? {
        let entry = LaneEntry {
            agent: row.agent,
            label: row.label,
            uses: row.uses,
            since_ms: row.granted_ms.unwrap_or(row.created_ms),
        };
        match row.granted_ms {
            Some(_) => view.running.push(entry),
            None => view.waiting.push(entry),
        }
    }
    Ok(view)
}

/// The projects whose lane holds a command, so a tick also reaches a project whose last
/// agent stopped and clears what its commands left, at most `HOLDERS_MAX` projects. Takes
/// the database. Returns the projects, or the SQLite failure; a stored name that is not a
/// project id is skipped.
pub fn projects(db: &Connection) -> Result<Vec<ProjectId>, EngineError> {
    let mut statement = db
        .prepare("SELECT DISTINCT project FROM coord_lane ORDER BY project LIMIT ?1")
        .map_err(state)?;
    let rows = statement
        .query_map([HOLDERS_MAX as i64], |row| row.get::<_, String>(0))
        .map_err(state)?;
    let mut found = Vec::new();
    for row in rows {
        if let Ok(project) = ProjectId::new(row.map_err(state)?) {
            found.push(project);
        }
    }
    assert!(found.len() <= HOLDERS_MAX);
    Ok(found)
}

/// One agent's lane folder the engine reads in a tick: the agent and its
/// `<agent>/tmp/crazyagent-lane` path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Holder {
    pub agent: AgentId,
    pub folder: PathBuf,
}

#[cfg(unix)]
enum Probe {
    Gone,
    Unread,
    Held(Option<Ticket>),
}

#[cfg(unix)]
fn read_bounded(file: std::fs::File, bytes_max: u64) -> Option<Vec<u8>> {
    use std::io::Read;
    if !file.metadata().ok()?.is_file() {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(bytes_max + 1).read_to_end(&mut bytes).ok()?;
    (bytes.len() as u64 <= bytes_max).then_some(bytes)
}

#[cfg(unix)]
fn probe(folder: &crate::team::tool::Mailbox, id: &str) -> Probe {
    use rustix::fs::{FlockOperation, flock};
    let Ok(file) = folder.read(&format!("{id}{TICKET}")) else {
        return Probe::Unread;
    };
    match flock(&file, FlockOperation::NonBlockingLockExclusive) {
        Ok(()) => Probe::Gone,
        Err(rustix::io::Errno::WOULDBLOCK) => Probe::Held(
            read_bounded(file, TICKET_BYTES_MAX)
                .and_then(|bytes| serde_json::from_slice::<Ticket>(&bytes).ok()),
        ),
        Err(_) => Probe::Unread,
    }
}

#[cfg(unix)]
fn clear(folder: &crate::team::tool::Mailbox, id: &str) {
    for suffix in SUFFIXES {
        let _ = folder.remove(&format!("{id}{suffix}"));
    }
}

#[cfg(unix)]
fn answer(folder: &crate::team::tool::Mailbox, name: &str, bytes: &[u8]) {
    if let Ok(mut file) = folder.create(name) {
        let _ = file.write_all(bytes);
    }
}

#[cfg(unix)]
fn say_waiting(folder: &crate::team::tool::Mailbox, id: &str, line: &str) {
    let name = format!("{id}{WAIT}");
    let fresh = folder.read(&name).ok().and_then(|file| {
        let age = file
            .metadata()
            .ok()?
            .modified()
            .ok()?
            .elapsed()
            .unwrap_or_default();
        let said = read_bounded(file, LINE_BYTES_MAX)?;
        (said == line.as_bytes() && age < HEARTBEAT).then_some(())
    });
    if fresh.is_some() {
        return;
    }
    let part = format!("{id}{WAIT_PART}");
    let _ = folder.remove(&part);
    let Ok(mut file) = folder.create(&part) else {
        return;
    };
    if file.write_all(line.as_bytes()).is_ok() {
        let _ = folder.rename(&part, &name);
    } else {
        let _ = folder.remove(&part);
    }
}

#[cfg(unix)]
struct Found {
    agent: AgentId,
    id: String,
    ticket: Ticket,
}

/// Moves a project's heavy lane one step, each team tick (F28): reads every live agent's
/// lane folder without following a link, frees the ticket of a lane command that ended
/// (its lock is gone) or whose agent the engine no longer reads, refuses a waiting ticket
/// naming a shared resource the project lacks, keeps each new ticket's turn, lets the next
/// ones run by `grant`, offers each running one its go again and tells each waiting one its
/// place. A ticket whose folder or lock cannot be read this tick keeps its row and its slot
/// for at most `SILENCE_MAX` after it was last read (its seen time is refreshed each
/// `HEARTBEAT`), and only a ticket read this tick starts, so a passing read failure never
/// lets more commands run than the lane allows and a folder that is gone never holds the
/// lane for longer than an absent engine holds a command. The store changes in one
/// transaction; files are written after it commits. Takes the database, the project, the
/// agents' folders, the project's settings and the time. Returns Ok or the SQLite failure.
#[cfg(unix)]
pub fn tick(
    db: &Connection,
    project: &ProjectId,
    holders: &[Holder],
    settings: &CoordSettings,
    now_ms: i64,
) -> Result<(), EngineError> {
    use crate::team::tool::Mailbox;
    assert!(now_ms > 0);
    let running_before: BTreeSet<(AgentId, String)> = stored(db, project)?
        .into_iter()
        .filter(|row| row.granted_ms.is_some())
        .map(|row| (row.agent, row.ticket))
        .collect();
    let holding: BTreeSet<AgentId> = holders
        .iter()
        .take(HOLDERS_MAX)
        .map(|holder| holder.agent)
        .collect();
    let mut folders: Vec<(AgentId, Mailbox)> = Vec::new();
    let mut live: Vec<Found> = Vec::new();
    let mut unread: BTreeSet<(AgentId, String)> = BTreeSet::new();
    for holder in holders.iter().take(HOLDERS_MAX) {
        let Ok(folder) = Mailbox::open_for_engine(&holder.folder) else {
            continue;
        };
        for (at, (_, id)) in folder.entries(TICKET).into_iter().enumerate() {
            if at >= TICKETS_PER_AGENT_MAX || live.len() >= TICKETS_PER_PROJECT_MAX {
                unread.insert((holder.agent, id));
                continue;
            }
            match probe(&folder, &id) {
                Probe::Gone => clear(&folder, &id),
                Probe::Unread => {
                    unread.insert((holder.agent, id));
                }
                Probe::Held(None) => answer(&folder, &format!("{id}{NO}"), UNREADABLE.as_bytes()),
                Probe::Held(Some(ticket)) => {
                    let runs = running_before.contains(&(holder.agent, id.clone()));
                    match refusal(&ticket, settings).filter(|_| !runs) {
                        Some(reason) => answer(&folder, &format!("{id}{NO}"), reason.as_bytes()),
                        None => live.push(Found {
                            agent: holder.agent,
                            id,
                            ticket,
                        }),
                    }
                }
            }
        }
        folders.push((holder.agent, folder));
    }
    assert!(live.len() <= TICKETS_PER_PROJECT_MAX);
    let read = |agent: AgentId, ticket: &str| {
        live.iter()
            .any(|found| found.agent == agent && found.id == ticket)
    };
    let opened = |agent: AgentId| folders.iter().any(|(holder, _)| *holder == agent);
    let change = Change::begin(db)?;
    let tx = change.db();
    let before = stored(tx, project)?;
    for row in &before {
        if read(row.agent, &row.ticket) {
            if now_ms - row.seen_ms >= HEARTBEAT_MS {
                tx.execute(
                    "UPDATE coord_lane SET seen_ms=?3 WHERE agent=?1 AND ticket=?2",
                    params![row.agent, row.ticket, now_ms],
                )
                .map_err(state)?;
            }
            continue;
        }
        let unseen = unread.contains(&(row.agent, row.ticket.clone()))
            || (holding.contains(&row.agent) && !opened(row.agent));
        if !unseen || now_ms - row.seen_ms > SILENCE_MAX_MS {
            tx.execute(
                "DELETE FROM coord_lane WHERE agent=?1 AND ticket=?2",
                params![row.agent, row.ticket],
            )
            .map_err(state)?;
        }
    }
    let mut next: i64 = tx
        .query_row(
            "SELECT COALESCE(MAX(seq),0)+1 FROM coord_lane WHERE project=?1",
            [project.as_str()],
            |row| row.get(0),
        )
        .map_err(state)?;
    for found in &live {
        if before
            .iter()
            .any(|row| row.agent == found.agent && row.ticket == found.id)
        {
            continue;
        }
        let uses = serde_json::to_string(&found.ticket.uses).map_err(state)?;
        tx.execute(
            "INSERT INTO coord_lane(project,agent,ticket,uses,label,seq,created_ms,seen_ms,granted_ms)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?7,NULL)",
            params![
                project.as_str(),
                found.agent,
                found.id,
                uses,
                found.ticket.label,
                next,
                now_ms
            ],
        )
        .map_err(state)?;
        next += 1;
    }
    let queue = stored(tx, project)?;
    let runs = grant(
        &queue
            .iter()
            .map(|row| Queued {
                uses: row.uses.clone(),
                granted: row.granted_ms.is_some(),
            })
            .collect::<Vec<_>>(),
        settings.heavy_lane,
        &settings.shared_resources,
    );
    let starts = |row: &Stored, runs: bool| {
        runs && (row.granted_ms.is_some() || read(row.agent, &row.ticket))
    };
    let running = queue
        .iter()
        .zip(&runs)
        .filter(|(row, runs)| starts(row, **runs))
        .count();
    let mut granted = Vec::new();
    let mut waiting = Vec::new();
    let mut ahead = 0;
    for (row, runs) in queue.iter().zip(&runs) {
        if starts(row, *runs) {
            if row.granted_ms.is_none() {
                tx.execute(
                    "UPDATE coord_lane SET granted_ms=?3 WHERE agent=?1 AND ticket=?2",
                    params![row.agent, row.ticket, now_ms],
                )
                .map_err(state)?;
            }
            if read(row.agent, &row.ticket) {
                granted.push((row.agent, row.ticket.clone()));
            }
        } else {
            if read(row.agent, &row.ticket) {
                let line = waiting_line(settings.heavy_lane, running, ahead, &row.uses);
                waiting.push((row.agent, row.ticket.clone(), line));
            }
            ahead += 1;
        }
    }
    assert!(granted.len() + waiting.len() <= live.len());
    change.commit()?;
    let folder = |agent: AgentId| {
        folders
            .iter()
            .find(|(holder, _)| *holder == agent)
            .map(|(_, folder)| folder)
    };
    for (agent, id) in &granted {
        if let Some(folder) = folder(*agent) {
            answer(folder, &format!("{id}{GO}"), GRANTED);
        }
    }
    for (agent, id, line) in &waiting {
        if let Some(folder) = folder(*agent) {
            say_waiting(folder, id, line);
        }
    }
    Ok(())
}

/// Leaves the heavy lane alone where its file locks do not exist: no ticket is read and
/// nothing is stored. Takes what the Unix tick takes. Returns Ok.
#[cfg(not(unix))]
pub fn tick(
    _db: &Connection,
    _project: &ProjectId,
    _holders: &[Holder],
    _settings: &CoordSettings,
    _now_ms: i64,
) -> Result<(), EngineError> {
    Ok(())
}

/// How a heavy command's wait for its turn ended.
#[cfg(unix)]
enum Turn {
    Go,
    Silent,
    Refused(String),
    TooLong,
}

/// A lane command's ticket while it waits and runs: its folder, its id and the lock that
/// tells the engine it is still there. Dropping the lock frees its place in the lane.
#[cfg(unix)]
struct Entry {
    folder: crate::team::tool::Mailbox,
    id: String,
    since: std::time::SystemTime,
    lock: crate::file_lock::HeldLock,
}

#[cfg(unix)]
impl Entry {
    /// Takes a place in the lane: writes the ticket under a part name, locks it and renames
    /// it to its ticket name, so the engine never reads a ticket that is not locked yet.
    /// Takes the lane folder and the ticket. Returns the entry or the I/O error, with
    /// nothing left in the folder.
    fn enter(path: &Path, ticket: &Ticket) -> std::io::Result<Self> {
        use crate::team::tool::{Mailbox, call_id};
        use rustix::fs::FlockOperation;
        assert!(ticket.valid());
        let folder = Mailbox::open(path)?;
        let id = call_id()?;
        let part = format!("{id}{PART}");
        let mut file = folder.create(&part)?;
        let bytes = serde_json::to_vec(ticket).map_err(std::io::Error::other)?;
        assert!(bytes.len() as u64 <= TICKET_BYTES_MAX);
        if let Err(problem) = file.write_all(&bytes) {
            let _ = folder.remove(&part);
            return Err(problem);
        }
        let lock = match crate::file_lock::HeldLock::take(file, FlockOperation::LockExclusive) {
            Ok(lock) => lock,
            Err(problem) => {
                let _ = folder.remove(&part);
                return Err(problem.into());
            }
        };
        if let Err(problem) = folder.rename(&part, &format!("{id}{TICKET}")) {
            let _ = folder.remove(&part);
            return Err(problem);
        }
        Ok(Self {
            folder,
            id,
            since: std::time::SystemTime::now(),
            lock,
        })
    }

    fn line(&self, suffix: &str) -> Option<(String, std::time::SystemTime)> {
        let file = self.folder.read(&format!("{}{suffix}", self.id)).ok()?;
        let at = file.metadata().ok()?.modified().ok()?;
        let bytes = read_bounded(file, LINE_BYTES_MAX)?;
        let text = String::from_utf8_lossy(&bytes).trim().to_owned();
        Some((text, at))
    }

    /// Waits for the engine's answer: its go, its refusal, or a word at least every
    /// `SILENCE_MAX`; each new waiting line is shown once. Takes the entry and where lines
    /// go. Returns how the wait ended.
    fn wait(&self, out: &mut dyn Write) -> Turn {
        let mut said = String::new();
        for _ in 0..POLLS_MAX {
            if self.line(GO).is_some() {
                return Turn::Go;
            }
            if let Some((reason, _)) = self.line(NO) {
                return Turn::Refused(reason);
            }
            let heard = match self.line(WAIT) {
                Some((line, at)) => {
                    if line != said {
                        let _ = writeln!(out, "crazyagent lane: {line}");
                        said = line;
                    }
                    at.max(self.since)
                }
                None => self.since,
            };
            if heard.elapsed().unwrap_or_default() > SILENCE_MAX {
                return Turn::Silent;
            }
            std::thread::sleep(POLL);
        }
        Turn::TooLong
    }

    /// Leaves the lane: removes the ticket and the engine's answers, then lets the lock go.
    /// Takes the entry.
    fn leave(self) {
        clear(&self.folder, &self.id);
        drop(self.lock);
    }
}

/// Runs the command with the lane command's own input and output, passing on a terminate,
/// hang-up or interrupt sent to the lane command, listened for before the command starts,
/// so a stop that reaches only the lane command still stops its command and the lane slot
/// is held exactly as long as it runs.
/// Takes the request and where the lane command's own lines go. Returns the command's exit
/// code, 128 plus the signal that ended it, or `NOT_STARTED`.
#[cfg(unix)]
fn command(request: &Request, out: &mut dyn Write) -> i32 {
    use std::os::unix::process::ExitStatusExt;
    use tokio::signal::unix::{Signal, SignalKind, signal};
    async fn next(stream: &mut Option<Signal>) {
        match stream {
            Some(stream) => {
                if stream.recv().await.is_none() {
                    std::future::pending::<()>().await;
                }
            }
            None => std::future::pending::<()>().await,
        }
    }
    let spawn = || {
        std::process::Command::new(&request.program)
            .args(&request.arguments)
            .spawn()
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build();
    let status: Result<std::io::Result<std::process::ExitStatus>, std::io::Error> = match runtime {
        Err(_) => spawn().map(|mut child| child.wait()),
        Ok(runtime) => runtime.block_on(async {
            let mut terminate = signal(SignalKind::terminate()).ok();
            let mut hangup = signal(SignalKind::hangup()).ok();
            let mut interrupt = signal(SignalKind::interrupt()).ok();
            let mut child = match spawn() {
                Ok(child) => child,
                Err(problem) => return Err(problem),
            };
            let pid = i32::try_from(child.id())
                .ok()
                .and_then(rustix::process::Pid::from_raw);
            let mut waiter = tokio::task::spawn_blocking(move || child.wait());
            loop {
                let passed = tokio::select! {
                    done = &mut waiter => {
                        return Ok(done.unwrap_or_else(|problem| Err(std::io::Error::other(problem))));
                    }
                    () = next(&mut terminate) => rustix::process::Signal::TERM,
                    () = next(&mut hangup) => rustix::process::Signal::HUP,
                    () = next(&mut interrupt) => rustix::process::Signal::INT,
                };
                if let Some(pid) = pid {
                    let _ = rustix::process::kill_process(pid, passed);
                }
            }
        }),
    };
    match status {
        Err(problem) => {
            let _ = writeln!(
                out,
                "crazyagent lane: the command could not start: {problem}"
            );
            NOT_STARTED
        }
        Ok(Ok(status)) => status
            .code()
            .unwrap_or_else(|| 128 + status.signal().unwrap_or(0)),
        Ok(Err(problem)) => {
            let _ = writeln!(
                out,
                "crazyagent lane: the command's end could not be read: {problem}"
            );
            LANE_FAILED
        }
    }
}

/// Runs one heavy command through the project's lane (F28): takes a place in the lane
/// folder `--folder` names, waits for its turn, runs the command and leaves. With no lane
/// folder, or when the engine says nothing for `SILENCE_MAX`, the command runs at once with
/// one line saying so; a refused ticket or a wait past `WAIT_MAX` does not run it. Takes
/// the arguments after `lane` and where the lane command's own lines go. Returns the exit
/// code.
#[cfg(unix)]
pub fn run(arguments: &[OsString], out: &mut dyn Write) -> i32 {
    let request = match parse(arguments) {
        Ok(request) => request,
        Err(problem) => {
            let _ = writeln!(out, "{problem}");
            return LANE_FAILED;
        }
    };
    let ticket = Ticket {
        uses: request.uses.clone(),
        label: request
            .label
            .clone()
            .unwrap_or_else(|| program_label(&request.program)),
    };
    let entered = request
        .folder
        .as_deref()
        .and_then(|folder| Entry::enter(folder, &ticket).ok());
    let Some(entry) = entered else {
        let _ = writeln!(out, "{NO_LANE}");
        return command(&request, out);
    };
    match entry.wait(out) {
        Turn::Go => {}
        Turn::Silent => {
            let _ = writeln!(out, "{SILENT}");
        }
        Turn::Refused(reason) => {
            let _ = writeln!(out, "crazyagent lane: {reason}");
            entry.leave();
            return LANE_FAILED;
        }
        Turn::TooLong => {
            let _ = writeln!(out, "{TOO_LONG}");
            entry.leave();
            return LANE_FAILED;
        }
    }
    let code = command(&request, out);
    entry.leave();
    code
}

/// Runs one heavy command where the lane's file locks do not exist: the command runs at
/// once with one line saying so. Takes the arguments after `lane` and where the line goes.
/// Returns the exit code.
#[cfg(not(unix))]
pub fn run(arguments: &[OsString], out: &mut dyn Write) -> i32 {
    let request = match parse(arguments) {
        Ok(request) => request,
        Err(problem) => {
            let _ = writeln!(out, "{problem}");
            return LANE_FAILED;
        }
    };
    let _ = writeln!(out, "{NO_LANE}");
    match std::process::Command::new(&request.program)
        .args(&request.arguments)
        .status()
    {
        Ok(status) => status.code().unwrap_or(LANE_FAILED),
        Err(problem) => {
            let _ = writeln!(
                out,
                "crazyagent lane: the command could not start: {problem}"
            );
            NOT_STARTED
        }
    }
}

#[cfg(all(test, unix))]
mod tests;
