use super::codex_folder::{self, Screen};
use super::interactive::{self, Run, Support, Transcript};
use super::launch::{
    Ending, LaunchError, LaunchEvent, LaunchLimits, LaunchRequest, Session, failure, harness_id,
    route, validate,
};
use super::leftovers::{FOLLOW_EVERY, LINEAGE_VARIABLE, Lineage, LineageToken, Unstopped};
use super::transcript::{TranscriptEvent, TranscriptReader};
use crate::launch_context::{AgentContext, Installed};
use crate::model_gateway::relay::{Relay, is_placeholder};
use crate::terminal::{AgentId, Host, HostError, INPUT_BYTES_MAX, TerminalStream};
use sha2::{Digest, Sha256};
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
const GROUP_FRESH: Duration = Duration::from_secs(5);
#[cfg(target_os = "macos")]
const KEEPER_WAIT: Duration = Duration::from_secs(6);
#[cfg(target_os = "macos")]
const KEEPER_POLL: Duration = Duration::from_millis(50);
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
    folder: Option<FolderStep>,
    turn: bool,
    held: bool,
    over_hold: bool,
    noticed: bool,
    own_held: bool,
    live: bool,
    own: bool,
    facts: VecDeque<TranscriptEvent>,
    background: BTreeSet<String>,
    exited: bool,
    stopped: bool,
    group_ended: bool,
    uncertain: bool,
    unconfined: bool,
    unkept: bool,
    lineage: Option<Result<Lineage, Unstopped>>,
    followed: Instant,
    record: Option<PaneRecord>,
    _key: Option<KeyFile>,
    _context: Option<Installed>,
    _relay: Option<Relay>,
}

/// The most bytes a key file the engine reads back may hold: the longest key it writes.
const KEY_FILE_BYTES_MAX: u64 = crate::model_gateway::relay::KEY_BYTES_MAX as u64;

/// How far a launch Codex opens behind its folder access screen has got.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FolderStep {
    /// The screen has not been seen yet.
    Waiting,
    /// The engine pressed Enter once and waits for the input box.
    Answered,
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

/// An account's key file and the SHA-256 of the exact bytes this launch wrote into it, so
/// the guard keeps no copy of the key. The file is removed when the agent session holding it
/// ends, and only while it still holds those bytes, so a refused or torn-down launch never
/// deletes a key file another launch has since written for a different, still-running agent
/// on the same account home.
pub(super) struct KeyFile {
    path: PathBuf,
    digest: [u8; 32],
}

impl KeyFile {
    /// Guards the key file at `path` that holds `content`. Inputs: the path and the bytes.
    /// Output: the guard, which removes the file on drop only while it still holds them.
    pub(super) fn new(path: PathBuf, content: &[u8]) -> Self {
        assert!(content.len() as u64 <= KEY_FILE_BYTES_MAX);
        Self {
            path,
            digest: Sha256::digest(content).into(),
        }
    }
}

impl Drop for KeyFile {
    /// Removes the key file only while it is still a regular file holding this guard's
    /// bytes, read through one open that follows no link and waits on no FIFO, since the
    /// path sits in the agent-writable account home. Input: the guard. Output: none.
    fn drop(&mut self) {
        if crate::platform::read_plain_file(&self.path, KEY_FILE_BYTES_MAX)
            .is_some_and(|current| <[u8; 32]>::from(Sha256::digest(current)) == self.digest)
        {
            let _ = std::fs::remove_file(&self.path);
        }
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
            interactive::write_key(request.account.home(), key)
                .map(|path| KeyFile::new(path, key.reveal().as_bytes()))
        })
        .transpose()
}

fn host_failure(error: HostError) -> LaunchError {
    LaunchError(error.0, None)
}

/// The refusal code for an agent pane on a system that cannot hold it under the macOS
/// confinement: every system but macOS, whatever the pane's unconfined flag, which also marks
/// Full access agents. Only agent launches come here, never the owner's own shell. Input: the
/// operating system name as `std::env::consts::OS` gives it. Output: the code, or None.
fn agent_pane_unavailable(os: &str) -> Option<&'static str> {
    match os {
        "macos" => None,
        "windows" => Some("launch_unavailable_on_windows"),
        "linux" => Some("launch_unavailable_on_linux"),
        _ => Some("launch_unavailable"),
    }
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
        if let Some(code) = agent_pane_unavailable(std::env::consts::OS) {
            return Err(failure(code));
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
                key_file: key.as_ref().map(|key| key.path.as_path()),
                resume: resume.is_some(),
            },
        )?;
        let folder = codex_folder::behind_a_screen(&command.argv).then_some(FolderStep::Waiting);
        if let Some(team) = installed.as_ref().and_then(Installed::native) {
            interactive::write_teammate_command(&home, &command, &request, team)?;
        }
        #[cfg(target_os = "macos")]
        let command = if request.unconfined {
            assert_ne!(command.argv[0], crate::sandbox::SANDBOX_EXEC);
            command
        } else {
            interactive::InteractiveCommand {
                argv: crate::sandbox::kept_pane(command.argv)
                    .ok_or_else(|| failure("launch_sandbox_required"))?,
                ..command
            }
        };
        if command.transcript == Transcript::Unavailable {
            return Err(failure("launch_transcript_unavailable"));
        }
        let token = LineageToken::new().ok_or_else(|| failure("launch_spawn_failed"))?;
        let mut command = command;
        command
            .env
            .push((LINEAGE_VARIABLE.to_owned(), token.as_str().to_owned()));
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
        let unkept = host.unkept(&agent);
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
            folder,
            turn: false,
            held: false,
            over_hold: false,
            noticed: false,
            own_held: false,
            live: false,
            own: false,
            facts: VecDeque::new(),
            background: BTreeSet::new(),
            exited: false,
            stopped: false,
            group_ended: false,
            uncertain: false,
            unconfined: request.unconfined,
            unkept,
            lineage: lineage_of(pane.pid, unkept, request.unconfined, Some(token.clone())),
            followed: Instant::now(),
            record: PaneRecord::new(
                &command.transcript,
                &command.cwd,
                since,
                pane.pid,
                key.as_ref().map(|key| key.path.as_path()),
            )
            .map(|record| PaneRecord {
                resumed,
                context: installed.as_ref().and_then(Installed::files).cloned(),
                proxy: installed.as_ref().and_then(Installed::model_proxy),
                lineage: Some(token),
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
        self.over_hold = false;
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

    /// Ends the pane and every process its harness started, then removes the key file and
    /// the generated context. On macOS a sandboxed pane's keeper first has the run's warden
    /// kill every process in the sandbox, sessions of their own included; then every process
    /// left in the pane's process group is killed, as a dev server started in the background.
    /// A Full access pane has no Seatbelt and so no keeper, whose warden only arms inside a
    /// sandbox, so the processes found in its lineage while the pane ran, and those carrying
    /// its launch's token, are checked after it. A pane its host runs without the keeper, as
    /// a native lead's own server does, has no warden either: its lineage is checked the same
    /// way. Output: none, since a pane reports no exit code, [`Unstopped::Uncertain`] when the
    /// keeper did not confirm in time, or the group outlived the wait or was found by an id no
    /// longer proven to be this pane's, or [`Unstopped::Left`] naming what a Full access pane,
    /// or one run without the keeper, left running.
    pub fn stop(mut self) -> Result<Option<i32>, Unstopped> {
        let lineage = self.lineage.take().map(|lineage| {
            lineage.map(|mut lineage| {
                lineage.before_stop();
                lineage
            })
        });
        self.halt();
        if self.uncertain {
            return Err(Unstopped::Uncertain);
        }
        if let Some(lineage) = lineage {
            lineage?.settle()?;
        }
        Ok(None)
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
    /// since the first look. A Codex launch behind its folder access screen is ready only
    /// once that screen took its one Enter and the input box is up, and fails instead of
    /// typing when that does not happen in time. A failed look keeps the wait, so the next
    /// look tries again within the same bound. Output: the answer, the failed look's
    /// problem, a folder access failure, or `launch_closed` once the pane ended.
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
        if let Some(step) = self.folder {
            if first.started.elapsed() >= codex_folder::SCREEN_WAIT {
                return Err(failure(match step {
                    FolderStep::Waiting => codex_folder::SCREEN_MISSING,
                    FolderStep::Answered => codex_folder::BOX_MISSING,
                }));
            }
        } else if first.started.elapsed() >= READY_WAIT {
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
                    let held = self.hold(true);
                    self.end_group(true, held);
                    return Err(failure("launch_closed"));
                }
            }
        }
        let ready = first.drawn
            && first.quiet.elapsed() >= READY_QUIET
            && self.host.input_live(&self.agent).map_err(host_failure)?
            && self.past_folder_screen()?;
        if ready {
            self.ready = true;
            self.first = None;
        }
        Ok(ready)
    }

    /// Takes a launch one step past Codex's folder access screen: Enter once, only on
    /// exactly that screen, then the input box. A launch not behind the screen is past it
    /// already. Output: whether the app can take a prompt, or the failed look's problem.
    fn past_folder_screen(&mut self) -> Result<bool, LaunchError> {
        let Some(step) = self.folder else {
            return Ok(true);
        };
        let screen = self.host.screen(&self.agent).map_err(host_failure)?;
        match (step, codex_folder::read_screen(&screen)) {
            (FolderStep::Waiting, Screen::Menu) => {
                self.type_bytes(ENTER)?;
                self.folder = Some(FolderStep::Answered);
                Ok(false)
            }
            (FolderStep::Answered, Screen::InputBox) => {
                self.folder = None;
                Ok(true)
            }
            _ => Ok(false),
        }
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
                    let ended = self.background.remove(&id);
                    self.noticed |= self.held && ended;
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
                TranscriptEvent::Interrupted | TranscriptEvent::TurnError { .. }
                    if self.over_hold =>
                {
                    continue;
                }
                TranscriptEvent::Text { text } => LaunchEvent::Text { text },
                TranscriptEvent::ToolUse { name } => LaunchEvent::ToolUse { name },
                TranscriptEvent::Interrupted => LaunchEvent::Interrupted,
                TranscriptEvent::TurnError { rate_limited } => {
                    LaunchEvent::TurnError { rate_limited }
                }
                TranscriptEvent::TurnFinished { success } => {
                    if !self.background.is_empty() && (success || self.over_hold) {
                        self.held = true;
                        continue;
                    }
                    let success = success || self.over_hold;
                    self.held = false;
                    self.over_hold = false;
                    self.turn = false;
                    LaunchEvent::TurnFinished { success }
                }
                TranscriptEvent::TurnStarted => {
                    if self.held {
                        self.over_hold = !self.background.is_empty() && !self.noticed;
                    }
                    self.held = false;
                    self.noticed = false;
                    continue;
                }
                TranscriptEvent::Waiting
                | TranscriptEvent::BackgroundStarted { .. }
                | TranscriptEvent::BackgroundEnded { .. } => continue,
            };
            self.queue.push_back(event);
        }
        if self.checked.elapsed() >= ALIVE_EVERY {
            let seen = self.checked.elapsed() <= GROUP_FRESH;
            let alive = match self.host.alive() {
                Ok(alive) => alive,
                Err(problem) => {
                    self.halt();
                    return Err(host_failure(problem));
                }
            };
            self.checked = Instant::now();
            if !alive.contains(&self.agent) {
                self.exited = true;
                self.turn = false;
                let held = self.hold(seen);
                self.end_group(seen, held);
                self.queue.push_back(LaunchEvent::Exited { code: None });
            }
        }
        if !self.exited
            && self.followed.elapsed() >= FOLLOW_EVERY
            && let Some(Ok(lineage)) = &mut self.lineage
        {
            lineage.follow();
            self.followed = Instant::now();
        }
        Ok(())
    }

    fn halt(&mut self) {
        if self.stopped {
            return;
        }
        self.stopped = true;
        self.turn = false;
        let seen = !self.group_ended
            && (self.checked.elapsed() <= GROUP_FRESH
                || self
                    .host
                    .alive()
                    .is_ok_and(|alive| alive.contains(&self.agent)));
        let held = self.hold(seen);
        #[cfg(target_os = "macos")]
        if seen && !self.unconfined {
            let ended = stop_keeper(self.host.as_ref(), &self.agent, self.pid);
            self.uncertain |= !ended && !self.unkept;
        }
        let _ = self.host.kill(&self.agent);
        self.end_group(seen, held);
    }

    /// Kills what the harness left in the pane's process group once the pane is gone, as a
    /// dev server started in the background, and records whether the group outlived the
    /// wait. `seen` tells the pane was alive within `GROUP_FRESH`: its id then still names
    /// this pane's group, which keeps the id while any member lives. An id unseen for longer
    /// may name a reused group: it is left alone, and a group found by it is uncertain.
    /// `held` is this session's hold on the group, taken before the host's kill so that
    /// close leaves the group to this session (CB-2 one signal); with none, as when the close
    /// holds the group or just saw it end, the group is only watched (CB-2 r1 F1).
    fn end_group(&mut self, seen: bool, held: Option<Ending>) {
        if std::mem::replace(&mut self.group_ended, true) {
            return;
        }
        if !seen {
            #[cfg(unix)]
            {
                self.uncertain |= super::launch::group_alive(self.pid);
            }
            return;
        }
        #[cfg(unix)]
        {
            self.uncertain |= !super::launch::end_pane_group(self.pid, held);
        }
        #[cfg(not(unix))]
        drop(held);
    }

    /// Takes this session's hold on its pane's group while it may still end it: the group
    /// is not ended yet and `seen` tells the pane was alive within `GROUP_FRESH`. Output:
    /// the hold, or none, as when another path holds the group or just saw it end.
    fn hold(&self, seen: bool) -> Option<Ending> {
        if seen && !self.group_ended {
            Ending::claim(self.pid)
        } else {
            None
        }
    }
}

/// The lineage a pane's stop checks, read from the pane's start so a process that later
/// leaves the pane's session is still known, and keyed by the launch's token so one that
/// also lost its parent is found at the stop (SECAUDIT-SETSID-LEFTOVER): sandboxed for a
/// pane its host runs without the keeper, Full access for an unconfined one, none otherwise.
/// Inputs: the pane's first process, the two answers and the token. Output: the lineage.
fn lineage_of(
    pid: u32,
    unkept: bool,
    unconfined: bool,
    token: Option<LineageToken>,
) -> Option<Result<Lineage, Unstopped>> {
    if unkept {
        Some(Lineage::of_sandboxed(pid, token))
    } else {
        unconfined.then(|| Lineage::of(pid, token))
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        self.halt();
    }
}

/// Ends an agent pane no session holds, as one an earlier engine left, and every process its
/// harness started: on macOS its keeper first empties the run's sandbox, then the window ends
/// and every process left in the pane's process group is killed, found by the first process
/// the host names while the pane runs, held from before the window ends so the host's close
/// leaves that group to this cleanup (CB-2 one signal). A pane its host runs without the
/// keeper is confirmed only by a listing of its processes showing none left. A cleanup not
/// confirmed is reported on stderr.
/// Inputs: the host, the pane and the owner's label, such as `research`. Output: whether
/// every process was confirmed gone, or the host's failure, such as
/// `terminal_agent_unknown` for a pane already gone.
pub fn end_left_pane(host: &dyn Host, agent: &AgentId, owner: &str) -> Result<bool, HostError> {
    let pid = host.pane_pid(agent)?;
    let held = pid.and_then(Ending::claim);
    let unkept = host.unkept(agent);
    let lineage = pid
        .filter(|_| unkept)
        .map(|pid| Lineage::of_sandboxed(pid, None));
    #[cfg(target_os = "macos")]
    let kept = pid.is_none_or(|pid| stop_keeper(host, agent, pid));
    #[cfg(not(target_os = "macos"))]
    let kept = true;
    match host.kill(agent) {
        Ok(()) => {}
        Err(problem) if problem.0 == "terminal_agent_unknown" => {}
        Err(problem) => return Err(problem),
    }
    #[cfg(unix)]
    let ended = pid.is_some_and(|pid| super::launch::end_pane_group(pid, held));
    #[cfg(not(unix))]
    let ended = {
        drop(held);
        pid.is_none()
    };
    let swept = match lineage {
        Some(lineage) => lineage.and_then(Lineage::settle).is_ok(),
        None => kept && !unkept,
    };
    let confirmed = swept && ended;
    if !confirmed {
        eprintln!("{owner}_pane_cleanup_uncertain agent={}", agent.as_str());
    }
    Ok(confirmed)
}

/// Asks a pane's first process to stop with terminate and waits up to `KEEPER_WAIT` for the
/// pane to end, after the host kept its last screen, since the pane ends before any kill.
/// A [`crate::sandbox::KEEPER`] then has the run's warden kill every process in the sandbox
/// and ends only once the warden confirmed that or found signals not confined, so the pane
/// ending in time confirms it. Inputs: the host, the pane and its first process, proven the
/// pane's within `GROUP_FRESH`. Output: whether the pane is gone.
#[cfg(target_os = "macos")]
pub(crate) fn stop_keeper(host: &dyn Host, agent: &AgentId, pid: u32) -> bool {
    let gone = || host.alive().is_ok_and(|alive| !alive.contains(agent));
    if gone() {
        return true;
    }
    let Some(process) = i32::try_from(pid)
        .ok()
        .and_then(rustix::process::Pid::from_raw)
    else {
        return false;
    };
    host.keep_last(agent);
    let _ = rustix::process::kill_process(process, rustix::process::Signal::TERM);
    let deadline = Instant::now() + KEEPER_WAIT;
    while !gone() {
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(KEEPER_POLL);
    }
    true
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

    /// Stops the agent and every process it started. Output: the exit code when known,
    /// [`Unstopped::Uncertain`] when a headless agent's warden did not confirm it or a pane's
    /// process group was not confirmed gone, or [`Unstopped::Left`] naming what a Full access
    /// agent, or a pane run without the keeper, left running.
    pub fn stop(self) -> Result<Option<i32>, Unstopped> {
        match self {
            Self::Piped(session) => session.stop(),
            Self::Terminal(session) => session.stop(),
        }
    }
}

#[cfg(test)]
mod tests;

#[cfg(all(test, target_os = "macos"))]
pub(crate) mod folder_screen_tests;
