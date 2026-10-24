use super::{AgentSession, KeyFile, TerminalSession, host_failure};
use crate::harnesses::interactive::Transcript;
use crate::harnesses::launch::{LaunchError, LaunchLimits, failure};
use crate::harnesses::transcript::{TranscriptEvent, TranscriptReader};
use crate::terminal::{AgentId, Host};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
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
/// harness started, the pane's first process and the key file the harness reads, if any.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PaneRecord {
    pub source: RecordSource,
    pub cwd: PathBuf,
    pub since_ms: u64,
    pub pid: u32,
    #[serde(default)]
    pub key: Option<PathBuf>,
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
        (path.is_absolute() && record.cwd.is_absolute() && record.pid > 0 && key).then_some(record)
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

    fn key_file(&self) -> Option<KeyFile> {
        let path = self.key.as_ref()?;
        std::fs::symlink_metadata(path)
            .is_ok_and(|metadata| metadata.file_type().is_file())
            .then(|| KeyFile(path.clone()))
    }
}

fn replay(
    reader: &mut TranscriptReader,
    limits: &LaunchLimits,
) -> Result<Option<bool>, LaunchError> {
    let mut open = None;
    for _ in 0..REPLAY_READS_MAX {
        let before = reader.bytes();
        let facts = reader.read()?;
        if reader.bytes() > limits.output_bytes as u64 {
            return Err(failure("launch_output_limit"));
        }
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
    /// longer be matched to the engine's work; the session then waits idle for the next prompt
    /// and removes the pane's key file when it stops. Takes the host, the pane name, the
    /// stored record and the limits. Returns the session, or a plain code when it cannot.
    pub fn adopt(
        host: Arc<dyn Host>,
        agent: AgentId,
        record: &PaneRecord,
        limits: LaunchLimits,
    ) -> Result<Self, LaunchError> {
        if !host.alive().map_err(host_failure)?.contains(&agent) {
            return Err(failure("launch_closed"));
        }
        let mut reader = TranscriptReader::new(record.transcript(), &record.cwd, record.since());
        if replay(&mut reader, &limits)? == Some(true) {
            host.input(&agent, ESCAPE).map_err(host_failure)?;
            let deadline = Instant::now() + SETTLE_WAIT;
            let mut settled = false;
            while !settled && Instant::now() < deadline {
                std::thread::sleep(POLL);
                settled = reader.read()?.iter().any(|fact| {
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
            replay(&mut reader, &limits)?;
        }
        Ok(Self {
            host,
            agent,
            pid: record.pid,
            reader,
            queue: VecDeque::new(),
            limits,
            deadline: Instant::now() + limits.run_time,
            checked: Instant::now(),
            events: 0,
            ready: true,
            turn: false,
            exited: false,
            stopped: false,
            record: Some(record.clone()),
            _key: record.key_file(),
            _context: None,
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
            std::mem::forget(context);
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

#[cfg(test)]
#[path = "adopt/tests.rs"]
mod tests;
