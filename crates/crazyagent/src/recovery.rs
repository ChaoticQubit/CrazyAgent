use crate::engine::EngineError;
use crate::harnesses::terminal_session::PaneRecord;
use crate::protocol::ProjectId;
use crate::team::AgentRole;
use rusqlite::Connection;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

mod store;
#[cfg(all(test, unix))]
mod tests;

/// The most resumed runs one project keeps, older ones dropped, and the most waiting and
/// the most settled runs one listing of a project reads.
pub const RUNS_PER_PROJECT_MAX: usize = 64;
/// The most interrupted runs one scan returns, oldest first.
pub const SCANNED_MAX: usize = 1024;
/// The most bytes of what an interrupted turn was asked that its record keeps.
pub const ASKED_BYTES_MAX: usize = 300;
/// The most bytes of the reason a resume failed that its record keeps.
pub const REASON_BYTES_MAX: usize = 300;

/// The turn an interrupted agent was in: its number among the agent's turns, the team task
/// a sub-agent ran or the owner message a master answered, and the first line of what it
/// was asked, bounded, when the engine handed the turn out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LastTurn {
    pub number: i64,
    pub task: Option<i64>,
    pub message: Option<i64>,
    pub asked: Option<String>,
}

/// Where the resume of one interrupted run stands: waiting to resume, started again in
/// its own harness session and handed its note, resumed once that session runs, or failed
/// with a reason until the owner retries it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunState {
    Interrupted,
    Resuming,
    Resumed,
    Failed,
}

impl RunState {
    /// The name the store keeps the state under.
    pub fn name(self) -> &'static str {
        match self {
            Self::Interrupted => "interrupted",
            Self::Resuming => "resuming",
            Self::Resumed => "resumed",
            Self::Failed => "failed",
        }
    }

    /// Reads a stored state name. Input: the name. Output: the state, or none.
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "interrupted" => Some(Self::Interrupted),
            "resuming" => Some(Self::Resuming),
            "resumed" => Some(Self::Resumed),
            "failed" => Some(Self::Failed),
            _ => None,
        }
    }
}

/// One agent run an engine stop cut short: the record id, the team agent, its project,
/// whether it is the master or a sub-agent, its harness, when the engine last saw it
/// running in ms since the epoch, its last turn, the pane record naming its harness
/// session's source, its working folder (the pane's, else its worktree), the exact session
/// record file a resume continues, whether its team was paused when it was recorded, where
/// its resume stands and why a failed resume failed.
/// `record.source`, `cwd` and `record_file` are the inputs `TranscriptReader::resumed` takes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Interrupted {
    pub id: i64,
    pub agent: i64,
    pub project: ProjectId,
    pub kind: AgentRole,
    pub harness: String,
    pub at: i64,
    pub last_turn: LastTurn,
    pub record: Option<PaneRecord>,
    pub cwd: Option<PathBuf>,
    pub record_file: Option<PathBuf>,
    pub paused: bool,
    pub state: RunState,
    pub reason: Option<String>,
}

/// One project's interrupted runs as the owner is shown them: every run still waiting on its
/// resume or on the owner's retry, holding its task or message, oldest first, how many more
/// wait past those, and the newest settled runs, resumed or given up, oldest first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listed {
    pub waiting: Vec<Interrupted>,
    pub more: usize,
    pub settled: Vec<Interrupted>,
}

pub(crate) fn unavailable() -> EngineError {
    EngineError("recovery_unavailable".into())
}

/// Opens the recovery store in the engine database, adding its table when missing.
/// Input: the engine root holding `engine.db`. Output: the connection.
pub fn open(runtime: &Path) -> Result<Connection, EngineError> {
    if !runtime.is_absolute() {
        return Err(unavailable());
    }
    let db = Connection::open(runtime.join("engine.db")).map_err(|_| unavailable())?;
    db.busy_timeout(Duration::from_secs(5))
        .map_err(|_| unavailable())?;
    store::initialize(&db)?;
    Ok(db)
}

/// Notes every team agent that is in a turn as the engine stops, or as it starts after a
/// crash left agents marked running, before the team runtime resets them. Inputs: the
/// team runtime's connection, inside its stop transaction or before its start reset, and
/// the stop time, or none after a crash, when each agent's last heartbeat is used.
/// Output: done; noting the same run again changes nothing.
pub fn capture(db: &Connection, stopped_ms: Option<i64>) -> Result<(), EngineError> {
    store::capture(db, stopped_ms)
}

/// Settles the runs the last engine stop cut short, as the team runtime does on its first
/// tick, and lists them. Call it only after the runtime adopted the panes it can.
/// Input: the engine root. Output: every interrupted run not yet handled, oldest first, at
/// most `SCANNED_MAX`.
pub fn scan(runtime: &Path) -> Result<Vec<Interrupted>, EngineError> {
    let db = open(runtime)?;
    store::settle(&db)?;
    store::runs(&db, None, &[RunState::Interrupted], SCANNED_MAX)
}

/// Settles the runs the last engine stop cut short: a run whose agent the starting team
/// runtime took over again in its live pane is dropped, unless a sub-agent's run holds a
/// task it resumes there; every other one is recorded interrupted once, with whether its
/// team is paused now; a paused team stays paused.
/// Input: the team runtime's connection, after it adopted the panes it can and before it
/// runs anything else. Output: done, or `recovery_unavailable`.
pub(crate) fn settle(db: &Connection) -> Result<(), EngineError> {
    store::settle(db)
}

/// Lists a project's interrupted runs so the owner sees which wait, which resumed and which
/// failed with their reasons: the waiting runs by age, however many newer runs settled, so
/// an old failed run that holds its task keeps its retry.
/// Inputs: the engine root and the project. Output: at most `RUNS_PER_PROJECT_MAX` waiting
/// and settled runs, with the waiting runs left out counted.
pub fn list(runtime: &Path, project: &ProjectId) -> Result<Listed, EngineError> {
    let db = open(runtime)?;
    store::listed(&db, project, RUNS_PER_PROJECT_MAX)
}

/// Starts a failed resume again: the agent's newest failed run waits to resume once more,
/// on the team runtime's next tick, without its old reason; if that resume fails too, the
/// run gives up and lets go of its task or message ([`retried`]).
/// Inputs: the engine root and the team agent. Output: whether a failed run was waiting.
pub fn retry(runtime: &Path, agent: i64) -> Result<bool, EngineError> {
    let db = open(runtime)?;
    store::retry(&db, agent)
}

/// Reads one project's runs whose resume is still open, waiting or started, oldest first.
/// Inputs: the team runtime's connection and the project. Output: at most
/// `RUNS_PER_PROJECT_MAX` runs.
pub(crate) fn open_runs(
    db: &Connection,
    project: &ProjectId,
) -> Result<Vec<Interrupted>, EngineError> {
    store::runs(
        db,
        Some(project),
        &[RunState::Interrupted, RunState::Resuming],
        RUNS_PER_PROJECT_MAX,
    )
}

/// Reads the run of one agent whose resume was started and has not settled yet.
/// Inputs: the team runtime's connection and the team agent. Output: the run, or none.
pub(crate) fn resuming(db: &Connection, agent: i64) -> Result<Option<Interrupted>, EngineError> {
    store::resuming(db, agent)
}

/// Moves one run from one resume state to the next, with the plain reason a failure
/// keeps, bounded. Inputs: the connection, the run, the state it must be in, the new
/// state and the reason. Output: whether the run was in `from`.
pub(crate) fn mark(
    db: &Connection,
    run: i64,
    from: RunState,
    to: RunState,
    reason: Option<&str>,
) -> Result<bool, EngineError> {
    store::mark(db, run, from, to, reason)
}

/// Tells whether the owner retried the run, so a failure of its resume gives it up rather
/// than holding its task or message for one more retry that would fail the same way.
/// Inputs: the team runtime's connection and the run. Output: the answer.
pub(crate) fn retried(db: &Connection, run: i64) -> Result<bool, EngineError> {
    store::retried(db, run)
}

/// One team task or owner message an interrupted run can hold back for its resume.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HeldItem {
    Task(i64),
    Message(i64),
}

/// Tells whether a run still holds an item back for its resume: a run waiting, resuming or
/// failed, or one just noted that `settle` keeps: a sub-agent's, or one whose agent does
/// not run again.
/// Inputs: the team runtime's connection and the item. Output: the answer.
pub(crate) fn holds(db: &Connection, item: HeldItem) -> Result<bool, EngineError> {
    store::holds(db, item)
}

/// Lets go of an item whose delivery never reached its agent, so it is handed out as usual
/// and never resumed: a run just noted for it is dropped, any other run keeps its record
/// without the item, in the caller's transaction when it has one. Inputs: the connection and
/// the item. Output: done.
pub(crate) fn release(db: &Connection, item: HeldItem) -> Result<(), EngineError> {
    store::release(db, item)
}

/// Reads the session record each agent that starts its harness for each turn keeps for its
/// running turn ([`remember`]). Inputs: the connection and the team agent, none for all.
/// Output: the records by team agent, at most `SCANNED_MAX`.
pub(crate) fn sessions(
    db: &Connection,
    agent: Option<i64>,
) -> Result<BTreeMap<i64, PaneRecord>, EngineError> {
    store::sessions(db, agent)
}

/// Keeps the session record of an agent that starts its harness for each turn while that
/// turn runs, since no pane record is kept for it, so a stop in the middle of the turn
/// can resume that session. Inputs: the connection, the team agent and its pane record.
/// Output: done.
pub(crate) fn remember(
    db: &Connection,
    agent: i64,
    record: &PaneRecord,
) -> Result<(), EngineError> {
    store::remember(db, agent, record)
}

/// Drops the session record [`remember`] kept once the agent's harness stopped.
/// Inputs: the connection and the team agent. Output: done.
pub(crate) fn forget(db: &Connection, agent: i64) -> Result<(), EngineError> {
    store::forget(db, agent)
}
