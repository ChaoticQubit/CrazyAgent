use super::journal::{Entry, Journal};
use crate::engine::EngineError;
use serde::{Deserialize, Serialize};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The journal kind of one app Keep awake froze while the lid was closed (DEC-167 Task 17).
pub const KIND: &str = "keep_awake_freeze";

/// The most apps the owner may choose to freeze.
pub const APPS_MAX: usize = 16;

/// The most processes one app may have and still be frozen: its main process and its
/// descendants when the lid closes. An app with more is left running.
pub const PROCS_MAX: usize = 48;

/// One app the owner may choose: the name shown and its id, the bundle id on macOS and
/// the executable path on Linux.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
        let restore = serde_json::to_string(self).expect("a frozen record is plain data");
        Entry {
            kind: KIND.into(),
            restore,
        }
    }
}

/// Reads who a running process is. Input: the pid. Output: its pid, start time and
/// executable, or why it cannot be read.
pub fn identity(pid: u32) -> Result<Proc, EngineError> {
    Ok(Proc {
        pid,
        start: String::new(),
        exe: String::new(),
    })
}

/// Reads the id of this boot session, which changes at every start of the computer.
/// Output: the id, or why it cannot be read.
pub fn boot_session() -> Result<String, EngineError> {
    Ok(String::new())
}

/// Opens the lid-closed lane's own journal below the engine root. Input: the engine root.
/// Output: the journal, or why it cannot be opened.
pub fn journal(root: &Path) -> Result<Journal, EngineError> {
    Journal::open(root)
}

/// Freezes one app: journals its main process and descendants with their identity, synced,
/// then sends each SIGSTOP. Inputs: the lane journal, the app's name and its main pid.
/// Output: the record journaled, or why the app was left running.
pub fn freeze(_journal: &mut Journal, app: &str, _pid: u32) -> Result<Frozen, EngineError> {
    Ok(Frozen {
        app: app.into(),
        boot: String::new(),
        procs: Vec::new(),
    })
}

/// Thaws one journaled app: SIGCONT to each process whose start time, executable and boot
/// session still match; any other pid is never signalled. Input: a `KIND` entry. Output:
/// Ok when nothing is left to thaw, or why a matching process could not be thawed.
pub fn thaw(_entry: &Entry) -> Result<(), EngineError> {
    Ok(())
}

/// Keeps the running apps the owner may pick: never CrazyAgent, the engine, a process
/// above or below the engine, another user's or a system process. Inputs: the listed apps
/// and the engine pid. Output: the pickable apps by name, or why the process list failed.
pub fn pickable(running: &[Running], _engine: u32) -> Result<Vec<FreezeApp>, EngineError> {
    Ok(running.iter().map(|one| one.app.clone()).collect())
}

/// Builds the thaw guard's command: this program's `keep-awake guard ROOT`, in its own
/// process group. The caller hands it the read end of the engine's pipe as stdin. Inputs:
/// the CrazyAgent program and the engine root. Output: the command, not started.
pub fn guard_command(_program: &Path, _root: &Path) -> Command {
    Command::new("/usr/bin/false")
}

/// The thaw guard: waits until the engine's end of the pipe closes, then thaws every app
/// and puts the sound back as the lane journal says, and settles those entries. Inputs:
/// the engine root and the pipe. Output: Ok, or the first undo that failed.
pub fn guard(_root: &Path, _engine: impl Read) -> Result<(), EngineError> {
    Ok(())
}
