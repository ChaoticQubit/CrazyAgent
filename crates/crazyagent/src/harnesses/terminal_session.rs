use super::interactive::{self, Run, Support, Transcript};
use super::launch::{
    LaunchError, LaunchEvent, LaunchLimits, LaunchRequest, Session, failure, harness_id, route,
    validate,
};
use super::transcript::{TranscriptEvent, TranscriptReader};
use crate::launch_context::{AgentContext, Installed};
use crate::model_gateway::relay::{Relay, is_placeholder};
use crate::terminal::{AgentId, Host, HostError, INPUT_BYTES_MAX, TerminalStream};
use std::collections::{BTreeSet, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime};
use tokio::sync::mpsc::error::TryRecvError;

mod adopt;
pub use adopt::{PaneRecord, RecordSource};

pub const PROMPT_BYTES_MAX: usize = 1024 * 1024;
pub(crate) const READY_WAIT: Duration = Duration::from_secs(30);
const READY_QUIET: Duration = Duration::from_millis(700);
const PASTE_SETTLE: Duration = Duration::from_millis(200);
const POLL: Duration = Duration::from_millis(100);
/// The most facts of turns the app started by itself that wait to be taken at once.
const OWN_FACTS_MAX: usize = 4096;
/// The most background commands of the app one session follows at once.
const BACKGROUND_MAX: usize = 64;
const ALIVE_EVERY: Duration = Duration::from_secs(1);
const PASTE_START: &str = "\x1b[200~";
const PASTE_END: &str = "\x1b[201~";
const ENTER: &[u8] = b"\r";
const ESCAPE: &[u8] = b"\x1b";

/// A harness running its own interactive app in a terminal pane of the engine's host.
/// Prompts are typed into the pane; replies, tool use and turn ends are read from the
/// harness's own session record, never from the screen. Stop or drop ends the pane.
pub struct TerminalSession {
    host: Arc<dyn Host>,
    agent: AgentId,
    pid: u32,
    reader: TranscriptReader,
    queue: VecDeque<LaunchEvent>,
    limits: LaunchLimits,
    deadline: Instant,
    checked: Instant,
    events: usize,
    ready: bool,
    first: Option<FirstScreen>,
    turn: bool,
    held: bool,
    own_held: bool,
    live: bool,
    own: bool,
    facts: VecDeque<TranscriptEvent>,
    background: BTreeSet<String>,
    exited: bool,
    stopped: bool,
    record: Option<PaneRecord>,
    _key: Option<KeyFile>,
    _context: Option<Installed>,
    _relay: Option<Relay>,
}

/// What the engine saw of an app's first screen while it waits for the app to read keys:
/// the attached stream once an attach worked, when the wait began, whether anything was
/// drawn and when the screen last changed.
struct FirstScreen {
    stream: Option<TerminalStream>,
    started: Instant,
    drawn: bool,
    quiet: Instant,
}

/// An account's key file, removed when the agent session holding it ends.
pub(super) struct KeyFile(pub(super) PathBuf);

impl Drop for KeyFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// A harness conversation an agent continues: the id the harness resumes it by and the
/// record the harness keeps it in, as a transcript reader named them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resume {
    pub id: String,
    pub record: PathBuf,
}

/// Writes a routed launch's relay token to the 0600 key file in its account home, the only
/// place its harness reads it from, so the token is in no argument or variable of any agent
/// process. Input: the request after [`route`]. Output: the file's guard, none for a
/// keyless launch, or a plain refusal code.
pub(super) fn write_key_file(request: &LaunchRequest) -> Result<Option<KeyFile>, LaunchError> {
    request
        .account
        .key()
        .map(|key| {
            assert!(is_placeholder(key.reveal()));
            interactive::write_key(request.account.home(), key).map(KeyFile)
        })
        .transpose()
}

fn host_failure(error: HostError) -> LaunchError {
    LaunchError(error.0)
}

impl TerminalSession {
    /// Starts an agent's interactive app in a new pane named `agent`, with the project's
    /// approved context installed into the agent home when given, the way every team agent
    /// launches. Inputs: the host, the pane name, the request and the project context.
    /// Output: the running session, or a plain refusal code; nothing is left behind on refusal.
    pub fn start(
        host: Arc<dyn Host>,
        agent: AgentId,
        request: LaunchRequest,
        context: Option<AgentContext>,
    ) -> Result<Self, LaunchError> {
        Self::start_resuming(host, agent, request, context, None)
    }

    /// Starts an agent's interactive app as `start` does, continuing the harness
    /// conversation `resume` names when given: the app resumes it by its id and the session
    /// reads only the turns added from now on. Inputs: as for `start`, and the conversation.
    /// Output: the running session, or a plain refusal code such as
    /// `launch_resume_unavailable` when the record cannot be followed; nothing is left
    /// behind on refusal.
    pub fn start_resuming(
        host: Arc<dyn Host>,
        agent: AgentId,
        request: LaunchRequest,
        context: Option<AgentContext>,
        resume: Option<&Resume>,
    ) -> Result<Self, LaunchError> {
        validate(&request)?;
        let installed = context
            .as_ref()
            .map(|context| context.install(&request.harness, request.account.home(), request.tools))
            .transpose()?;
        Self::open(host, agent, request, installed, context.as_ref(), resume)
    }

    /// Starts an agent whose caller already installed the project context into its home,
    /// such as a research run. Inputs: the host, the pane name, the request, the installed
    /// files and the context they came from. Output: the running session, or a refusal code.
    pub fn start_installed(
        host: Arc<dyn Host>,
        agent: AgentId,
        request: LaunchRequest,
        installed: Installed,
        context: &AgentContext,
    ) -> Result<Self, LaunchError> {
        Self::open(host, agent, request, Some(installed), Some(context), None)
    }

    fn open(
        host: Arc<dyn Host>,
        agent: AgentId,
        request: LaunchRequest,
        installed: Option<Installed>,
        context: Option<&AgentContext>,
        resume: Option<&Resume>,
    ) -> Result<Self, LaunchError> {
        validate(&request)?;
        if interactive::support(&request.harness) != Support::Terminal {
            return Err(failure("launch_terminal_unsupported"));
        }
        let (request, relay) = route(request)?;
        let home = request.account.home().to_path_buf();
        let key = write_key_file(&request)?;
        let cwd = request
            .working_dir
            .canonicalize()
            .map_err(|_| failure("launch_working_dir_unavailable"))?;
        let claude = harness_id(&request) == "claude-code";
        if claude {
            interactive::prepare_claude_home(&home, &cwd)?;
        }
        let session_id = match resume {
            Some(resume) => resume.id.clone(),
            None => interactive::session_id()?,
        };
        let command = interactive::command(
            &request,
            installed.as_ref(),
            &Run {
                session_id: &session_id,
                key_file: key.as_ref().map(|key| key.0.as_path()),
                resume: resume.is_some(),
            },
        )?;
        if let Some(team) = installed.as_ref().and_then(Installed::native) {
            interactive::write_teammate_command(&home, &command, &request, team)?;
        }
        if command.transcript == Transcript::Unavailable {
            return Err(failure("launch_transcript_unavailable"));
        }
        if let Some(context) = context {
            context.refuse_exposed(
                command.argv.iter().map(String::as_bytes).chain(
                    command
                        .env
                        .iter()
                        .flat_map(|(name, value)| [name.as_bytes(), value.as_bytes()]),
                ),
            )?;
        }
        let since = SystemTime::now();
        let reader = match resume {
            Some(resume) => TranscriptReader::resumed(
                command.transcript.clone(),
                &command.cwd,
                &resume.record,
                None,
            )?,
            None => TranscriptReader::new(command.transcript.clone(), &command.cwd, since),
        };
        let resumed = resume.map(|resume| (resume.record.clone(), reader.start()));
        let pane = host.spawn(&agent, &command).map_err(host_failure)?;
        assert_eq!(pane.agent, agent);
        let ready = LaunchEvent::Ready {
            native_session: claude.then_some(session_id),
        };
        Ok(Self {
            host,
            agent,
            pid: pane.pid,
            reader,
            queue: VecDeque::from([ready]),
            limits: request.limits,
            deadline: Instant::now() + request.limits.run_time,
            checked: Instant::now(),
            events: 0,
            ready: false,
            first: None,
            turn: false,
            held: false,
            own_held: false,
            live: false,
            own: false,
            facts: VecDeque::new(),
            background: BTreeSet::new(),
            exited: false,
            stopped: false,
            record: PaneRecord::new(
                &command.transcript,
                &command.cwd,
                since,
                pane.pid,
                key.as_ref().map(|key| key.0.as_path()),
            )
            .map(|record| PaneRecord {
                resumed,
                context: installed.as_ref().and_then(Installed::files).cloned(),
                proxy: installed.as_ref().and_then(Installed::model_proxy),
                ..record
            }),
            _key: key,
            _context: installed,
            _relay: relay,
        })
    }

    /// Names the harness conversation this pane records, once its record exists: the id
    /// the harness resumes it by and the record's path. Output: none before that.
    pub fn conversation(&self) -> Option<Resume> {
        self.reader
            .conversation()
            .map(|(id, record)| Resume { id, record })
    }

    /// Returns the pane's first process, which leads the process group of the harness.
    pub fn process_group(&self) -> u32 {
        self.pid
    }

    /// Counts the commands the app started in the background whose end it has not reported
    /// yet. An engine-typed turn that ends while any runs stays open until the app's turn on
    /// their end notification finishes, so the task sees their result instead of their being
    /// ended with it. Output: the count, at most `BACKGROUND_MAX`.
    pub fn background_running(&self) -> usize {
        self.background.len()
    }

    /// Says whether the engine-typed turn has ended in the app, is held open only for
    /// commands it started in the background, and every event it gave has been read.
    /// Output: the answer.
    pub fn held(&self) -> bool {
        self.held && self.queue.is_empty()
    }

    /// Types one prompt into the app as a bracketed paste, then Enter. The first prompt
    /// waits, at most `READY_WAIT`, until the app has drawn its screen, gone quiet and
    /// reads keys itself (see [`Host::input_live`]), so its Enter is not lost.
    /// Input: prompt text. Output: Ok, `launch_busy` while a turn runs, or a refusal code.
    pub fn send(&mut self, prompt: &str) -> Result<(), LaunchError> {
        self.send_or_cancel(prompt, &AtomicBool::new(false))
    }

    /// Sends one prompt as [`Self::send`] does, but gives up waiting for the first screen
    /// as soon as `cancel` is set, so a stop never waits on a harness that does not draw.
    /// Inputs: prompt text and the cancel flag. Output: Ok, `launch_cancelled` when the
    /// flag was set before the prompt was typed, or the refusal codes of `send`.
    pub fn send_or_cancel(&mut self, prompt: &str, cancel: &AtomicBool) -> Result<(), LaunchError> {
        self.require_live()?;
        if self.turn {
            return Err(failure("launch_busy"));
        }
        if prompt.len() > PROMPT_BYTES_MAX {
            return Err(failure("launch_prompt_too_long"));
        }
        if !self.ready {
            self.wait_ready(cancel)?;
        }
        for part in paste(prompt) {
            self.type_bytes(part.as_bytes())?;
        }
        std::thread::sleep(PASTE_SETTLE);
        self.type_bytes(ENTER)?;
        self.turn = true;
        Ok(())
    }

    /// Presses Escape in the app, which stops the running turn in Codex and Claude Code. A
    /// turn held open for the app's background commands has no request running for Escape
    /// to stop, so it ends here as interrupted, its commands still counted as running.
    /// Output: Ok once typed or ended, or `launch_not_running` with no turn.
    pub fn interrupt(&mut self) -> Result<(), LaunchError> {
        self.require_live()?;
        if !self.turn {
            return Err(failure("launch_not_running"));
        }
        if self.held {
            self.held = false;
            self.turn = false;
            self.queue.extend([
                LaunchEvent::Interrupted,
                LaunchEvent::TurnFinished { success: false },
            ]);
            return Ok(());
        }
        self.type_bytes(ESCAPE)
    }

    /// Waits up to `wait` for the next event of the turn the engine started. Text, tool
    /// use and turn ends from turns the owner typed into the pane are not reported.
    /// Output: Some(event), None on timeout, or an error past a limit or once the pane ended.
    pub fn next_event(&mut self, wait: Duration) -> Result<Option<LaunchEvent>, LaunchError> {
        let until = Instant::now() + wait;
        loop {
            if let Some(event) = self.queue.pop_front() {
                self.events += 1;
                if self.events > self.limits.events {
                    self.halt();
                    return Err(failure("launch_event_limit"));
                }
                return Ok(Some(event));
            }
            if self.exited {
                return Err(failure("launch_closed"));
            }
            if Instant::now() >= self.deadline {
                self.halt();
                return Err(failure("launch_deadline"));
            }
            self.poll()?;
            if !self.queue.is_empty() {
                continue;
            }
            let left = until
                .min(self.deadline)
                .saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Ok(None);
            }
            std::thread::sleep(left.min(POLL));
        }
    }

    /// Follows every turn in the pane from now on, the ones the owner types included, as a
    /// live master needs: turn facts then come from [`Self::next_fact`] and `next_event`
    /// reports nothing more. Takes the session. Returns nothing.
    pub fn follow_every_turn(&mut self) {
        assert!(!self.own);
        self.live = true;
        self.queue
            .retain(|event| matches!(event, LaunchEvent::Ready { .. }));
    }

    /// Keeps the turns the app starts by itself between the engine's turns, as a native
    /// lead's app does when its teammates write to it: their facts then come from
    /// [`Self::next_own_fact`], tool use left out, at most `OWN_FACTS_MAX` waiting. The
    /// engine's turns still come from `next_event`. Takes the session. Returns nothing.
    pub fn follow_own_turns(&mut self) {
        assert!(!self.live);
        self.own = true;
    }

    /// Takes the oldest kept fact of a turn the app started by itself, read by the last
    /// poll of `next_event`. A successful end of such a turn while commands it started in
    /// the background still run is held back, and the app's turn on their end notification
    /// continues it, as for an engine-typed turn. Takes the session. Returns the fact, or none.
    pub fn next_own_fact(&mut self) -> Option<TranscriptEvent> {
        assert!(self.own && !self.live);
        self.facts.pop_front()
    }

    /// Says whether a turn the app started by itself has ended in the app and is held open
    /// only for commands it started in the background, every fact it gave taken. Output:
    /// the answer.
    pub fn own_held(&self) -> bool {
        self.own_held && self.facts.is_empty()
    }

    /// Waits up to `wait` for the next turn fact of a session that follows every turn: a
    /// turn started, said text, ended, hit an error, was interrupted or waits for an
    /// answer. Tool use is not kept. Output: Some(fact), None on timeout, or an error once the
    /// pane ended or past a limit, as for `next_event`.
    pub fn next_fact(&mut self, wait: Duration) -> Result<Option<TranscriptEvent>, LaunchError> {
        assert!(self.live);
        let until = Instant::now() + wait;
        loop {
            if let Some(fact) = self.facts.pop_front() {
                return Ok(Some(fact));
            }
            if self.exited {
                return Err(failure("launch_closed"));
            }
            if Instant::now() >= self.deadline {
                self.halt();
                return Err(failure("launch_deadline"));
            }
            self.poll()?;
            if !self.facts.is_empty() {
                continue;
            }
            let left = until
                .min(self.deadline)
                .saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Ok(None);
            }
            std::thread::sleep(left.min(POLL));
        }
    }

    /// Ends the pane and every process in it, then removes the key file and the
    /// generated context. Output: none, since a pane reports no exit code.
    pub fn stop(mut self) -> Option<i32> {
        self.halt();
        None
    }

    fn require_live(&self) -> Result<(), LaunchError> {
        if self.exited || self.stopped {
            return Err(failure("launch_closed"));
        }
        Ok(())
    }

    fn type_bytes(&self, bytes: &[u8]) -> Result<(), LaunchError> {
        assert!(bytes.len() <= INPUT_BYTES_MAX);
        self.host.input(&self.agent, bytes).map_err(host_failure)
    }

    /// Looks once, without waiting, whether the app reads keys yet by the rule its first
    /// prompt waits for: it has drawn its screen, nothing changed on it for `READY_QUIET`
    /// and it turned bracketed paste on (see [`Host::input_live`]), or `READY_WAIT` passed
    /// since the first look. A failed look keeps the wait, so the next look tries again
    /// within the same bound. Output: the answer, the failed look's problem, or
    /// `launch_closed` once the pane ended.
    pub fn poll_ready(&mut self) -> Result<bool, LaunchError> {
        if self.ready {
            return Ok(true);
        }
        let first = self.first.get_or_insert_with(|| FirstScreen {
            stream: None,
            started: Instant::now(),
            drawn: false,
            quiet: Instant::now(),
        });
        if first.started.elapsed() >= READY_WAIT {
            self.ready = true;
            self.first = None;
            return Ok(true);
        }
        let stream = match &mut first.stream {
            Some(stream) => stream,
            None => {
                let stream = self.host.attach(&self.agent).map_err(host_failure)?;
                first.drawn = !stream.screen.iter().all(u8::is_ascii_whitespace);
                first.quiet = Instant::now();
                first.stream.insert(stream)
            }
        };
        loop {
            match stream.chunks.try_recv() {
                Ok(chunk) => {
                    first.drawn |= !chunk.is_empty();
                    first.quiet = Instant::now();
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.first = None;
                    self.exited = true;
                    return Err(failure("launch_closed"));
                }
            }
        }
        let ready = first.drawn
            && first.quiet.elapsed() >= READY_QUIET
            && self.host.input_live(&self.agent).map_err(host_failure)?;
        if ready {
            self.ready = true;
            self.first = None;
        }
        Ok(ready)
    }

    fn wait_ready(&mut self, cancel: &AtomicBool) -> Result<(), LaunchError> {
        loop {
            match self.poll_ready() {
                Ok(true) => return Ok(()),
                Ok(false) => {}
                Err(problem) => {
                    self.first = None;
                    return Err(problem);
                }
            }
            if cancel.load(Ordering::SeqCst) {
                self.first = None;
                return Err(failure("launch_cancelled"));
            }
            std::thread::sleep(POLL);
        }
    }

    fn poll(&mut self) -> Result<(), LaunchError> {
        let facts = match self.reader.read() {
            Ok(facts) => facts,
            Err(problem) => {
                self.halt();
                return Err(problem);
            }
        };
        if self.reader.bytes() > self.limits.output_bytes as u64 {
            self.halt();
            return Err(failure("launch_output_limit"));
        }
        for fact in facts {
            if self.live {
                match fact {
                    TranscriptEvent::ToolUse { .. }
                    | TranscriptEvent::BackgroundStarted { .. }
                    | TranscriptEvent::BackgroundEnded { .. } => {}
                    TranscriptEvent::TurnStarted => {
                        self.turn = true;
                        self.facts.push_back(fact);
                    }
                    TranscriptEvent::TurnFinished { .. } => {
                        self.turn = false;
                        self.facts.push_back(fact);
                    }
                    other => self.facts.push_back(other),
                }
                continue;
            }
            match fact {
                TranscriptEvent::BackgroundStarted { id } => {
                    if self.background.len() < BACKGROUND_MAX {
                        self.background.insert(id);
                    }
                    continue;
                }
                TranscriptEvent::BackgroundEnded { id } => {
                    self.background.remove(&id);
                    continue;
                }
                _ => {}
            }
            assert!(self.background.len() <= BACKGROUND_MAX);
            if !self.turn {
                if self.own
                    && self.facts.len() < OWN_FACTS_MAX
                    && !matches!(fact, TranscriptEvent::ToolUse { .. })
                {
                    match fact {
                        TranscriptEvent::TurnFinished { success: true }
                            if !self.background.is_empty() =>
                        {
                            self.own_held = true;
                        }
                        TranscriptEvent::TurnStarted if self.own_held => self.own_held = false,
                        other => self.facts.push_back(other),
                    }
                }
                continue;
            }
            let event = match fact {
                TranscriptEvent::Text { text } => LaunchEvent::Text { text },
                TranscriptEvent::ToolUse { name } => LaunchEvent::ToolUse { name },
                TranscriptEvent::Interrupted => LaunchEvent::Interrupted,
                TranscriptEvent::TurnError { rate_limited } => {
                    LaunchEvent::TurnError { rate_limited }
                }
                TranscriptEvent::TurnFinished { success } => {
                    if success && !self.background.is_empty() {
                        self.held = true;
                        continue;
                    }
                    self.held = false;
                    self.turn = false;
                    LaunchEvent::TurnFinished { success }
                }
                TranscriptEvent::TurnStarted => {
                    self.held = false;
                    continue;
                }
                TranscriptEvent::Waiting
                | TranscriptEvent::BackgroundStarted { .. }
                | TranscriptEvent::BackgroundEnded { .. } => continue,
            };
            self.queue.push_back(event);
        }
        if self.checked.elapsed() >= ALIVE_EVERY {
            self.checked = Instant::now();
            let alive = match self.host.alive() {
                Ok(alive) => alive,
                Err(problem) => {
                    self.halt();
                    return Err(host_failure(problem));
                }
            };
            if !alive.contains(&self.agent) {
                self.exited = true;
                self.turn = false;
                self.queue.push_back(LaunchEvent::Exited { code: None });
            }
        }
        Ok(())
    }

    fn halt(&mut self) {
        if self.stopped {
            return;
        }
        self.stopped = true;
        self.turn = false;
        let _ = self.host.kill(&self.agent);
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        self.halt();
    }
}

/// Frames a prompt as one bracketed paste the app takes as typed text, split into host
/// inputs of at most `INPUT_BYTES_MAX` bytes on character boundaries. Control characters
/// other than newline and tab are dropped, so the prompt cannot end the paste or press keys.
pub fn paste(prompt: &str) -> Vec<String> {
    let clean: String = prompt
        .replace("\r\n", "\n")
        .chars()
        .map(|character| if character == '\r' { '\n' } else { character })
        .filter(|character| matches!(character, '\n' | '\t') || !character.is_control())
        .collect();
    let framed = format!("{PASTE_START}{clean}{PASTE_END}");
    let mut parts = Vec::new();
    let mut start = 0;
    while start < framed.len() {
        let mut end = (start + INPUT_BYTES_MAX).min(framed.len());
        while !framed.is_char_boundary(end) {
            end -= 1;
        }
        parts.push(framed[start..end].to_owned());
        start = end;
    }
    assert!(parts.iter().all(|part| part.len() <= INPUT_BYTES_MAX));
    assert_eq!(parts.concat(), framed);
    parts
}

/// One running agent, whichever way it was launched: the harness's interactive app in a
/// terminal pane, or its headless interface on pipes where no pane host is available.
pub enum AgentSession {
    Piped(Box<Session>),
    Terminal(Box<TerminalSession>),
}

impl AgentSession {
    /// Starts an agent of one project: in a pane named `pane.1` on host `pane.0` when given
    /// and the harness has an interactive app, otherwise headless on pipes.
    /// Inputs: the request, the project context and the optional pane. Output: the session.
    pub fn start(
        request: LaunchRequest,
        context: AgentContext,
        pane: Option<(Arc<dyn Host>, AgentId)>,
    ) -> Result<Self, LaunchError> {
        Self::start_resuming(request, context, pane, None)
    }

    /// Starts an agent as `start` does, a pane app continuing the harness conversation
    /// `resume` names when given; a headless agent on pipes never resumes one.
    /// Inputs: as for `start`, and the conversation. Output: the session.
    pub fn start_resuming(
        request: LaunchRequest,
        context: AgentContext,
        pane: Option<(Arc<dyn Host>, AgentId)>,
        resume: Option<&Resume>,
    ) -> Result<Self, LaunchError> {
        match pane {
            Some((host, agent)) if interactive::support(&request.harness) == Support::Terminal => {
                TerminalSession::start_resuming(host, agent, request, Some(context), resume)
                    .map(|session| Self::Terminal(Box::new(session)))
            }
            _ => Session::start_with_context(request, context)
                .map(|session| Self::Piped(Box::new(session))),
        }
    }

    /// Returns the process group that stopping the agent ends.
    pub fn process_group(&self) -> u32 {
        match self {
            Self::Piped(session) => session.process_group(),
            Self::Terminal(session) => session.process_group(),
        }
    }

    /// Counts the commands a pane app started in the background that still run; a headless
    /// session reports none. Output: the count.
    pub fn background_running(&self) -> usize {
        match self {
            Self::Piped(_) => 0,
            Self::Terminal(session) => session.background_running(),
        }
    }

    /// Says whether a pane app's engine-typed turn has ended and is held open only for its
    /// background commands; a headless session never holds one. Output: the answer.
    pub fn held(&self) -> bool {
        match self {
            Self::Piped(_) => false,
            Self::Terminal(session) => session.held(),
        }
    }

    /// Says whether the engine sees the commands the agent starts in the background, so a
    /// turn can be held for them or stopped with them: a pane app whose record reports
    /// them; a headless session reports none. Output: the answer.
    pub fn reports_background(&self) -> bool {
        match self {
            Self::Piped(_) => false,
            Self::Terminal(session) => session.reader.reports_background(),
        }
    }

    /// Sends one prompt. Output: Ok, or a plain refusal code such as `launch_busy`.
    pub fn send(&mut self, prompt: &str) -> Result<(), LaunchError> {
        match self {
            Self::Piped(session) => session.send(prompt),
            Self::Terminal(session) => session.send(prompt),
        }
    }

    /// Sends one prompt, giving up on a pane app's first screen once `cancel` is set.
    /// Inputs: prompt text and the cancel flag. Output: Ok, `launch_cancelled`, or a plain
    /// refusal code such as `launch_busy`.
    pub fn send_or_cancel(&mut self, prompt: &str, cancel: &AtomicBool) -> Result<(), LaunchError> {
        match self {
            Self::Piped(session) => session.send(prompt),
            Self::Terminal(session) => session.send_or_cancel(prompt, cancel),
        }
    }

    /// Asks the harness to stop the current turn. Output: Ok once asked, or a refusal code.
    pub fn interrupt(&mut self) -> Result<(), LaunchError> {
        match self {
            Self::Piped(session) => session.interrupt(),
            Self::Terminal(session) => session.interrupt(),
        }
    }

    /// Waits up to `wait` for the next event. Output: Some(event), None on timeout, or an
    /// error past a limit or once the harness ended.
    pub fn next_event(&mut self, wait: Duration) -> Result<Option<LaunchEvent>, LaunchError> {
        match self {
            Self::Piped(session) => session.next_event(wait),
            Self::Terminal(session) => session.next_event(wait),
        }
    }

    /// Keeps the turns a pane app starts by itself, see
    /// [`TerminalSession::follow_own_turns`]; a headless agent starts none. Takes the
    /// session. Returns nothing.
    pub fn follow_own_turns(&mut self) {
        if let Self::Terminal(session) = self {
            session.follow_own_turns();
        }
    }

    /// Takes the oldest kept fact of a turn the pane app started by itself; a headless
    /// agent has none. Takes the session. Returns the fact, or none.
    pub fn next_own_fact(&mut self) -> Option<TranscriptEvent> {
        match self {
            Self::Piped(_) => None,
            Self::Terminal(session) => session.next_own_fact(),
        }
    }

    /// Says whether a pane app's own turn has ended and is held open only for its
    /// background commands, see [`TerminalSession::own_held`]; a headless agent starts no
    /// turn of its own. Output: the answer.
    pub fn own_held(&self) -> bool {
        match self {
            Self::Piped(_) => false,
            Self::Terminal(session) => session.own_held(),
        }
    }

    /// Stops the agent and every process it started. Output: the exit code when known, or
    /// `harness_cleanup_uncertain` when a headless agent's warden did not confirm it.
    pub fn stop(self) -> Result<Option<i32>, LaunchError> {
        match self {
            Self::Piped(session) => session.stop(),
            Self::Terminal(session) => Ok(session.stop()),
        }
    }
}

#[cfg(test)]
mod tests;
