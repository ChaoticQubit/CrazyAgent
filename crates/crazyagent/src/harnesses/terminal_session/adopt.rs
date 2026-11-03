use super::{AgentSession, KeyFile, Resume, TerminalSession, host_failure};
use crate::harnesses::interactive::Transcript;
use crate::harnesses::launch::{LaunchError, LaunchLimits, failure};
use crate::harnesses::transcript::{TranscriptEvent, TranscriptReader};
use crate::launch_context::{Installed, InstalledFiles};
use crate::sandbox::policy::AgentAccess;
use crate::terminal::{AgentId, Host};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub const RECORD_BYTES_MAX: usize = 4096;
const SETTLE_WAIT: Duration = Duration::from_secs(10);
const REPLAY_READS_MAX: usize = 4096;
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
/// project access level the agent started at, and where its launch wrote the project
/// context files that hold MCP secrets. A record from an engine that did not keep the
/// level or the files has none.
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
    /// Returns the record, or none for anything malformed, long or with relative paths.
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
        (path.is_absolute()
            && record.cwd.is_absolute()
            && record.pid > 0
            && key
            && resumed
            && context)
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

    fn key_file(&self) -> Option<KeyFile> {
        let path = self.key.as_ref()?;
        std::fs::symlink_metadata(path)
            .is_ok_and(|metadata| metadata.file_type().is_file())
            .then(|| KeyFile(path.clone()))
    }
}

/// Adds the transcript facts an adopted pane already had to its event count, since the
/// engine that launched it may have reported each of them. Fails with `launch_event_limit`
/// past the launch's event budget, as the session itself would.
fn count(events: &mut usize, facts: usize, limits: &LaunchLimits) -> Result<(), LaunchError> {
    *events = events.saturating_add(facts);
    if *events > limits.events {
        return Err(failure("launch_event_limit"));
    }
    Ok(())
}

fn replay(
    reader: &mut TranscriptReader,
    limits: &LaunchLimits,
    events: &mut usize,
) -> Result<Option<bool>, LaunchError> {
    let mut open = None;
    for _ in 0..REPLAY_READS_MAX {
        let before = reader.bytes();
        let facts = reader.read()?;
        if reader.bytes() > limits.output_bytes as u64 {
            return Err(failure("launch_output_limit"));
        }
        count(events, facts.len(), limits)?;
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
    }
    Err(failure("launch_output_limit"))
}

impl TerminalSession {
    /// Takes over an agent pane a previous engine left running, without starting the harness
    /// again. A turn still running in it is stopped with Escape, since its result can no
    /// longer be matched to the engine's work. The launch's run time and event budget go on
    /// counting from its start. The key and context files are removed when the session stops,
    /// or at once when the pane cannot be taken over. Takes the host, the pane name, the
    /// stored record and the limits. Returns the idle session, or a plain code.
    pub fn adopt(
        host: Arc<dyn Host>,
        agent: AgentId,
        record: &PaneRecord,
        limits: LaunchLimits,
    ) -> Result<Self, LaunchError> {
        let context = record.context.as_ref().map(Installed::adopt).transpose()?;
        if !host.alive().map_err(host_failure)?.contains(&agent) {
            return Err(failure("launch_closed"));
        }
        let mut reader = match &record.resumed {
            Some((file, from)) => {
                TranscriptReader::resumed(record.transcript(), &record.cwd, file, Some(*from))?
            }
            None => TranscriptReader::adopted(record.transcript(), &record.cwd, record.since()),
        };
        let mut events = 0;
        if replay(&mut reader, &limits, &mut events)? == Some(true) {
            host.input(&agent, ESCAPE).map_err(host_failure)?;
            let deadline = Instant::now() + SETTLE_WAIT;
            let mut settled = false;
            while !settled && Instant::now() < deadline {
                std::thread::sleep(POLL);
                let facts = reader.read()?;
                count(&mut events, facts.len(), &limits)?;
                settled = facts.iter().any(|fact| {
                    matches!(
                        fact,
                        TranscriptEvent::TurnFinished { .. } | TranscriptEvent::Interrupted
                    )
                });
            }
            if !settled {
                let _ = host.kill(&agent);
                return Err(failure("launch_adopt_unsettled"));
            }
            replay(&mut reader, &limits, &mut events)?;
        }
        Ok(Self {
            host,
            agent,
            pid: record.pid,
            reader,
            queue: VecDeque::new(),
            limits,
            deadline: Instant::now() + record.run_time_left(limits.run_time),
            checked: Instant::now(),
            events,
            ready: true,
            turn: false,
            held: false,
            live: false,
            own: false,
            facts: VecDeque::new(),
            background: BTreeSet::new(),
            exited: false,
            stopped: false,
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
