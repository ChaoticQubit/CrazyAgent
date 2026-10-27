use crate::engine::EngineError;
use crate::harnesses::terminal_session::PaneRecord;
use crate::protocol::ProjectId;
use crate::team::AgentRole;
use rusqlite::Connection;
use std::path::{Path, PathBuf};
use std::time::Duration;

mod store;
#[cfg(all(test, unix))]
mod tests;

/// The most interrupted runs one project keeps and lists; older ones are dropped.
pub const RUNS_PER_PROJECT_MAX: usize = 64;
/// The most interrupted runs one scan returns, oldest first.
pub const SCANNED_MAX: usize = 1024;
/// The most bytes of what an interrupted turn was asked that its record keeps.
pub const ASKED_BYTES_MAX: usize = 300;

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

/// One agent run an engine stop cut short: the record id, the team agent, its project,
/// whether it is the master or a sub-agent, its harness, when the engine last saw it
/// running in ms since the epoch, its last turn, the pane record naming its harness
/// session's source, its working folder (the pane's, else its worktree), the exact session
/// record file a resume continues, and whether its team was paused when it was recorded.
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
}

fn unavailable() -> EngineError {
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

/// Settles the runs the last engine stop cut short: a run whose agent the starting team
/// runtime took over again in its live pane is dropped, every other one is recorded
/// interrupted once, with whether its team is paused now; a paused team stays paused.
/// Call it after the runtime adopted the panes it can and before its first tick.
/// Input: the engine root. Output: every interrupted run not yet handled, oldest first, at
/// most `SCANNED_MAX`.
pub fn scan(runtime: &Path) -> Result<Vec<Interrupted>, EngineError> {
    let db = open(runtime)?;
    store::settle(&db)?;
    store::interrupted(&db, None, SCANNED_MAX)
}

/// Lists a project's interrupted runs not yet handled, oldest first.
/// Inputs: the engine root and the project. Output: at most `RUNS_PER_PROJECT_MAX` runs.
pub fn list(runtime: &Path, project: &ProjectId) -> Result<Vec<Interrupted>, EngineError> {
    let db = open(runtime)?;
    store::interrupted(&db, Some(project), RUNS_PER_PROJECT_MAX)
}
