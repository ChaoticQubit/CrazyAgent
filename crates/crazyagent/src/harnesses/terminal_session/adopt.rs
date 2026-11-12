use super::{
    AgentSession, FolderStep, KEY_FILE_BYTES_MAX, KeyFile, Resume, Screen, TerminalSession,
    codex_folder, host_failure,
};
use crate::harnesses::interactive::Transcript;
use crate::harnesses::launch::{LaunchError, LaunchLimits, failure};
use crate::harnesses::leftovers::LineageToken;
use crate::harnesses::transcript::{READ_BYTES_MAX, TranscriptEvent, TranscriptReader};
use crate::launch_context::{Installed, InstalledFiles};
use crate::sandbox::policy::{AgentAccess, AgentNetwork};
use crate::sandbox::toolchains::Toolchain;
use crate::terminal::{AgentId, Host};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub const RECORD_BYTES_MAX: usize = 4096;
const SETTLE_WAIT: Duration = Duration::from_secs(10);
const REPLAY_READS_MAX: u64 = 4096;
const POLL: Duration = Duration::from_millis(100);
const ESCAPE: &[u8] = b"\x1b";

/// Where an agent's harness keeps its session record, in a form the engine can store.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RecordSource {
    CodexRollouts { sessions: PathBuf },
    ClaudeSession { file: PathBuf },
}

/// What the engine keeps about one agent's live pane, so a later engine can adopt it
/// without starting the harness again: the session record, the working folder, when the
/// harness started, the pane's first process, the key file the harness reads, if any, for a
/// resumed conversation its record and the offset where this pane's turns begin, the
/// project access level the agent started at, where its launch wrote the project context
/// files that hold MCP secrets, the toolchains its sandbox granted, the loopback port of the
/// model proxy its profile and variables name (S4b), the project network it started
/// under (CS-2) and the owner's narrow grants its profile holds (F08). A record from an
/// engine that did not keep the level, the files, the toolchains, the port, the network or
/// the grants has none; no grants means none were held.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PaneRecord {
    pub source: RecordSource,
    pub cwd: PathBuf,
    pub since_ms: u64,
    pub pid: u32,
    #[serde(default)]
    pub key: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resumed: Option<(PathBuf, u64)>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access: Option<AgentAccess>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<InstalledFiles>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub toolchains: Option<Vec<Toolchain>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network: Option<AgentNetwork>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lineage: Option<LineageToken>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grants: Option<Vec<crate::sandbox::policy::AccessGrant>>,
}

impl PaneRecord {
    /// Describes a pane the engine just started. Takes the harness's session record,
    /// its canonical working folder, the start time, the pane's first process and the
    /// key file written for it. Returns none for a harness with no session record.
    pub fn new(
        transcript: &Transcript,
        cwd: &Path,
        since: SystemTime,
        pid: u32,
        key: Option<&Path>,
    ) -> Option<Self> {
        let source = match transcript {
            Transcript::CodexRollouts { sessions } => RecordSource::CodexRollouts {
                sessions: sessions.clone(),
            },
            Transcript::ClaudeSession { file } => {
                RecordSource::ClaudeSession { file: file.clone() }
            }
            Transcript::Unavailable => return None,
        };
        let since_ms = since.duration_since(UNIX_EPOCH).ok()?.as_millis();
        Some(Self {
            source,
            cwd: cwd.to_path_buf(),
            since_ms: u64::try_from(since_ms).ok()?,
            pid,
            key: key.map(Path::to_path_buf),
            resumed: None,
            access: None,
            context: None,
            toolchains: None,
            proxy: None,
            network: None,
            lineage: None,
            grants: None,
        })
    }

    /// Writes the record for the engine database. Takes the record.
    /// Returns compact JSON of at most `RECORD_BYTES_MAX` bytes, or none past that bound.
    pub fn encode(&self) -> Option<String> {
        serde_json::to_string(self)
            .ok()
            .filter(|text| text.len() <= RECORD_BYTES_MAX)
    }

    /// Reads a stored record. Takes the JSON.
    /// Returns the record, or none for anything malformed, long, with relative paths or with
    /// a toolchain listed twice.
    pub fn decode(text: &str) -> Option<Self> {
        if text.len() > RECORD_BYTES_MAX {
            return None;
        }
        let record: Self = serde_json::from_str(text).ok()?;
        let path = match &record.source {
            RecordSource::CodexRollouts { sessions } => sessions,
            RecordSource::ClaudeSession { file } => file,
        };
        let key = record.key.as_deref().is_none_or(Path::is_absolute);
        let resumed = record
            .resumed
            .as_ref()
            .is_none_or(|(file, _)| file.is_absolute());
        let context = record
            .context
            .as_ref()
            .is_none_or(|files| files.home.is_absolute());
        let grants = record.grants.as_ref().is_none_or(|grants| {
            crate::sandbox::policy::AgentPolicy {
                grants: grants.clone(),
                ..Default::default()
            }
            .validate()
            .is_ok()
        });
        let toolchains = record.toolchains.as_ref().is_none_or(|toolchains| {
            toolchains.len() <= Toolchain::ALL.len()
                && toolchains
                    .iter()
                    .enumerate()
                    .all(|(at, toolchain)| !toolchains[..at].contains(toolchain))
        });
        (path.is_absolute()
            && record.cwd.is_absolute()
            && record.pid > 1
            && key
            && resumed
            && context
            && toolchains
            && grants
            && record.proxy != Some(0))
        .then_some(record)
    }

    /// Deletes the context files of a pane that ends without being taken over, unless its
    /// home is no longer a private agent home or another launch uses it. Takes the record.
    pub fn remove_context_files(&self) {
        if let Some(files) = &self.context {
            drop(Installed::adopt(files));
        }
    }

    fn transcript(&self) -> Transcript {
        match &self.source {
            RecordSource::CodexRollouts { sessions } => Transcript::CodexRollouts {
                sessions: sessions.clone(),
            },
            RecordSource::ClaudeSession { file } => {
                Transcript::ClaudeSession { file: file.clone() }
            }
        }
    }

    fn since(&self) -> SystemTime {
        UNIX_EPOCH + Duration::from_millis(self.since_ms)
    }

    /// Gives what is left of a launch's run time since this pane's harness started, so an
    /// engine that adopts the pane keeps the launch's deadline. Takes the launch's run time.
    /// Returns the rest, zero once it has passed.
    pub fn run_time_left(&self, run_time: Duration) -> Duration {
        let ran = SystemTime::now()
            .duration_since(self.since())
            .unwrap_or_default();
        run_time.saturating_sub(ran)
    }

    /// Guards the key file the record names, as the pane's launch wrote it. The file sits in
    /// the agent-writable account home, so it is read through one open that follows no link
    /// and waits on no FIFO, and taken only as a regular file of at most
    /// `KEY_FILE_BYTES_MAX` bytes. Input: the record. Output: the guard, or none.
    fn key_file(&self) -> Option<KeyFile> {
        let path = self.key.as_ref()?;
        let content = crate::platform::read_plain_file(path, KEY_FILE_BYTES_MAX)?;
        Some(KeyFile::new(path.clone(), content.as_bytes()))
    }
}

/// Reads an adopted pane's record on to its end to learn whether a turn is open there.
/// No byte count ends it (owner 11-11): it reads until a read finds nothing new, at most
/// `REPLAY_READS_MAX` reads more than the record held when it was opened, and a harness
/// that writes faster than that leaves the rest to the session's own polls. Takes the
/// reader. Returns whether a turn is open, none when no fact says, or the reader's problem.
fn replay(reader: &mut TranscriptReader) -> Result<Option<bool>, LaunchError> {
    let mut open = None;
    let mut reads_max: Option<u64> = None;
    let mut reads: u64 = 0;
    loop {
        let before = reader.bytes();
        let facts = reader.read()?;
        reads += 1;
        if facts.is_empty() && reader.bytes() == before {
            return Ok(open);
        }
        for fact in facts {
            match fact {
                TranscriptEvent::TurnStarted => open = Some(true),
                TranscriptEvent::TurnFinished { .. } | TranscriptEvent::Interrupted => {
                    open = Some(false);
                }
                _ => {}
            }
        }
        let max = *reads_max.get_or_insert_with(|| {
            REPLAY_READS_MAX + reader.unread().unwrap_or(0) / READ_BYTES_MAX as u64
        });
        assert!(max >= REPLAY_READS_MAX);
        if reads >= max {
            eprintln!("launch_adopt_replay_cut reads={reads}");
            return Ok(open);
        }
    }
}

impl TerminalSession {
    /// Takes over a sub-agent's pane; see `adopt_as`, whose rule for a pane that is no live
    /// master applies. Takes the host, the pane name, the stored record and the limits.
    /// Returns the idle session, or a plain code.
    pub fn adopt(
        host: Arc<dyn Host>,
        agent: AgentId,
        record: &PaneRecord,
        limits: LaunchLimits,
    ) -> Result<Self, LaunchError> {
        Self::adopt_as(host, agent, record, limits, false)
    }

    /// Takes over a live master's pane; see `adopt_as`, whose rule for a live master
    /// applies. Takes the host, the pane name, the stored record and the limits. Returns the
    /// idle session, or a plain code.
    pub fn adopt_live(
        host: Arc<dyn Host>,
        agent: AgentId,
        record: &PaneRecord,
        limits: LaunchLimits,
    ) -> Result<Self, LaunchError> {
        Self::adopt_as(host, agent, record, limits, true)
    }

    /// Takes over an agent pane a previous engine left running, without starting the harness
    /// again. A turn still running in it is stopped with Escape, since its result can no
    /// longer be matched to the engine's work. An open turn whose record the harness last
    /// wrote the launch's `stale_turn` ago, on a pane whose app works on nothing (see
    /// `waits`), is no turn: nothing is typed and the pane is taken over idle. A turn that
    /// does not end on such a pane is taken over idle as well, and either is dropped in the
    /// record's reader (see `TranscriptReader::drop_turn`) and told by `took_none`. A live
    /// master's pane counts whatever else its screen shows, since its caller holds engine
    /// input after a dropped turn; any other pane must show the app's input prompt. On a
    /// pane that does not count, a turn that does not end ends the pane, with
    /// `launch_adopt_unsettled` once its processes are confirmed gone and
    /// `launch_adopt_cleanup_uncertain` when they are not. The launch's run time and event
    /// budget go on counting from its start. A Codex pane still on its folder access screen
    /// is not ready: it goes on with its launch's one Enter before any prompt is typed. A
    /// failed look at the screen counts as not on it, so it cannot end the pane. The
    /// key and context files are removed when the session stops, or at once when the pane
    /// cannot be taken over. Takes the host, the pane name, the stored record, the limits
    /// and whether the pane is a live master's. Returns the idle session, or a plain code.
    fn adopt_as(
        host: Arc<dyn Host>,
        agent: AgentId,
        record: &PaneRecord,
        limits: LaunchLimits,
        live: bool,
    ) -> Result<Self, LaunchError> {
        let context = record.context.as_ref().map(Installed::adopt).transpose()?;
        if let Some(code) = super::agent_pane_unavailable(std::env::consts::OS) {
            return Err(failure(code));
        }
        if !host.alive().map_err(host_failure)?.contains(&agent) {
            return Err(failure("launch_closed"));
        }
        let mut reader = match &record.resumed {
            Some((file, from)) => {
                TranscriptReader::resumed(record.transcript(), &record.cwd, file, Some(*from))?
            }
            None => TranscriptReader::adopted(record.transcript(), &record.cwd, record.since()),
        };
        let open = replay(&mut reader)? == Some(true);
        let stale = open
            && reader
                .silent()
                .is_some_and(|silent| silent >= limits.stale_turn)
            && super::waits(host.as_ref(), &agent, !live);
        let mut settled = false;
        if open && !stale {
            host.input(&agent, ESCAPE).map_err(host_failure)?;
            let deadline = Instant::now() + SETTLE_WAIT;
            while !settled && Instant::now() < deadline {
                std::thread::sleep(POLL);
                let facts = reader.read()?;
                settled = facts.iter().any(|fact| {
                    matches!(
                        fact,
                        TranscriptEvent::TurnFinished { .. } | TranscriptEvent::Interrupted
                    )
                });
            }
            if !settled && !super::waits(host.as_ref(), &agent, !live) {
                return Err(failure(
                    match super::end_left_pane(host.as_ref(), &agent, "adopt") {
                        Ok(true) => "launch_adopt_unsettled",
                        Ok(false) | Err(_) => "launch_adopt_cleanup_uncertain",
                    },
                ));
            }
            if settled {
                replay(&mut reader)?;
            }
        }
        let took_none = open && !settled;
        if took_none {
            reader.drop_turn();
        }
        let unkept = host.unkept(&agent);
        let unconfined = !matches!(
            record.access,
            Some(AgentAccess::Sandboxed | AgentAccess::OwnBranch)
        );
        let on_menu = matches!(record.source, RecordSource::CodexRollouts { .. })
            && host
                .screen(&agent)
                .is_ok_and(|screen| codex_folder::read_screen(&screen) == Screen::Menu);
        Ok(Self {
            host,
            agent,
            pid: record.pid,
            reader,
            queue: VecDeque::new(),
            limits,
            deadline: Some(Instant::now() + record.run_time_left(limits.run_time)),
            checked: Instant::now(),
            ready: !on_menu,
            first: None,
            folder: on_menu.then_some(FolderStep::Waiting),
            turn: false,
            held: false,
            over_hold: false,
            noticed: false,
            own_held: false,
            live: false,
            own: false,
            grown: (0, Instant::now()),
            took_none,
            facts: VecDeque::new(),
            background: BTreeSet::new(),
            ended: VecDeque::new(),
            exited: false,
            stopped: false,
            group_ended: false,
            uncertain: false,
            #[cfg(target_os = "macos")]
            unconfined,
            #[cfg(target_os = "macos")]
            unkept,
            lineage: super::lineage_of(record.pid, unkept, unconfined, record.lineage.clone()),
            followed: Instant::now(),
            record: Some(record.clone()),
            _key: record.key_file(),
            _context: context,
            _relay: None,
        })
    }

    /// Gives what a later engine needs to adopt this pane. Takes the session.
    /// Returns the record, none when the harness keeps no session record.
    pub fn record(&self) -> Option<&PaneRecord> {
        self.record.as_ref()
    }

    /// Lets go of the pane without ending it, as the engine quits with this agent kept alive.
    /// Generated context and key files stay for the harness that still runs.
    /// Takes the session; a later engine adopts the pane from its record.
    pub fn leave(mut self) {
        self.stopped = true;
        if let Some(key) = self._key.take() {
            std::mem::forget(key);
        }
        if let Some(context) = self._context.take() {
            context.keep();
        }
    }
}

impl AgentSession {
    /// Takes over an agent pane a previous engine left running; see `TerminalSession::adopt`.
    /// Takes the host, the pane name, the stored record and the limits.
    /// Returns the idle session, or a plain code.
    pub fn adopt(
        host: Arc<dyn Host>,
        agent: AgentId,
        record: &PaneRecord,
        limits: LaunchLimits,
    ) -> Result<Self, LaunchError> {
        TerminalSession::adopt(host, agent, record, limits)
            .map(|session| Self::Terminal(Box::new(session)))
    }

    /// Takes over a live master's pane a previous engine left running; see
    /// `TerminalSession::adopt_live`. Takes the host, the pane name, the stored record and
    /// the limits. Returns the idle session, or a plain code.
    pub fn adopt_live(
        host: Arc<dyn Host>,
        agent: AgentId,
        record: &PaneRecord,
        limits: LaunchLimits,
    ) -> Result<Self, LaunchError> {
        TerminalSession::adopt_live(host, agent, record, limits)
            .map(|session| Self::Terminal(Box::new(session)))
    }

    /// Gives what a later engine needs to adopt this agent's pane. Takes the session.
    /// Returns the record for a pane, none for a headless agent on pipes.
    pub fn pane_record(&self) -> Option<PaneRecord> {
        match self {
            Self::Piped(_) => None,
            Self::Terminal(session) => session.record().cloned(),
        }
    }

    /// Names the harness conversation a pane agent records, once its record exists, so a
    /// later start can continue it. Takes the session. Returns none for a headless agent on
    /// pipes or before the record exists.
    pub fn conversation(&self) -> Option<Resume> {
        match self {
            Self::Piped(_) => None,
            Self::Terminal(session) => session.conversation(),
        }
    }

    /// Ends the engine's hold on the agent as it quits: a pane keeps running for a later
    /// engine, a headless agent on pipes stops. Takes the session.
    pub fn leave(self) {
        match self {
            Self::Piped(session) => {
                let _ = session.stop();
            }
            Self::Terminal(session) => session.leave(),
        }
    }
}

#[cfg(all(test, unix))]
#[path = "adopt/tests.rs"]
mod tests;
