use super::bounded_problem;
use crate::engine::Client;
use crate::engine::owner::{OwnerClientKind, TerminalOutput, TerminalSession};
use crate::terminal::{AgentId, INPUT_BYTES_MAX, SIZE_MAX, SIZE_MIN, TerminalFrame};
use crate::tui::seam::Request;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::thread::JoinHandle;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::sync::mpsc::error::{TryRecvError, TrySendError};

#[cfg(all(test, unix))]
mod tests;

const COMMANDS_QUEUED: usize = 256;
const FRAMES_QUEUED: usize = 64;
const THREADS_MAX: usize = 4;
const NOTICES_MAX: usize = 8;
const DRAIN_TIME: Duration = Duration::from_secs(5);

enum Command {
    Input(Vec<u8>),
    Resize { cols: u16, rows: u16 },
}

struct Session {
    agent: AgentId,
    commands: mpsc::Sender<Command>,
    frames: mpsc::Receiver<TerminalFrame>,
}

/// The TUI's live agent terminal: one attached session at a time, each on its
/// own thread and owner connection so a slow engine read never holds back
/// typing. Input: terminal requests from the screens. Output: terminal frames
/// polled by the run loop, ending with Closed or Rejected.
pub(in crate::tui) struct Terminals {
    socket: PathBuf,
    timeout: Duration,
    session: Option<Session>,
    threads: Vec<JoinHandle<()>>,
    notices: VecDeque<(AgentId, TerminalFrame)>,
}

impl Terminals {
    /// Terminals reached through one engine.
    /// Inputs: the engine socket and the bounded request timeout. Output: no
    /// session attached.
    pub fn new(socket: PathBuf, timeout: Duration) -> Self {
        Self {
            socket,
            timeout,
            session: None,
            threads: Vec::new(),
            notices: VecDeque::new(),
        }
    }

    /// Runs one terminal request from the screens.
    /// Input: an attach, input, resize or detach request. Output: none; any
    /// failure comes back from `poll` as a Closed or Rejected frame.
    pub fn handle(&mut self, request: Request) {
        match request {
            Request::TerminalAttach { agent } => self.attach(agent),
            Request::TerminalInput { agent, bytes } => {
                assert!(!bytes.is_empty() && bytes.len() <= INPUT_BYTES_MAX);
                self.command(&agent, Command::Input(bytes));
            }
            Request::TerminalResize { agent, cols, rows } => {
                assert!((SIZE_MIN..=SIZE_MAX).contains(&cols));
                assert!((SIZE_MIN..=SIZE_MAX).contains(&rows));
                self.command(&agent, Command::Resize { cols, rows });
            }
            Request::TerminalDetach { agent } => {
                if self
                    .session
                    .as_ref()
                    .is_some_and(|session| session.agent == agent)
                {
                    self.session = None;
                }
            }
            other => unreachable!("not a terminal request: {other:?}"),
        }
    }

    /// Takes the next frame for the screens, oldest first.
    /// Input: none. Output: the agent and frame, or None while nothing waits;
    /// a Closed or Rejected frame ends the session.
    pub fn poll(&mut self) -> Option<(AgentId, TerminalFrame)> {
        if let Some(notice) = self.notices.pop_front() {
            return Some(notice);
        }
        let session = self.session.as_mut()?;
        let frame = match session.frames.try_recv() {
            Ok(frame) => frame,
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => TerminalFrame::Closed {
                reason: "terminal_closed".into(),
            },
        };
        let agent = session.agent.clone();
        if matches!(
            frame,
            TerminalFrame::Closed { .. } | TerminalFrame::Rejected { .. }
        ) {
            self.session = None;
        }
        Some((agent, frame))
    }

    /// Says whether a session is attached, so the run loop wakes often
    /// enough to show its output as it arrives.
    /// Input: none. Output: true while frames can arrive.
    pub fn live(&self) -> bool {
        self.session.is_some() || !self.notices.is_empty()
    }

    fn attach(&mut self, agent: AgentId) {
        self.session = None;
        self.threads.retain(|thread| !thread.is_finished());
        if self.threads.len() >= THREADS_MAX {
            self.notice(agent, "terminal_busy");
            return;
        }
        let (commands, receiver) = mpsc::channel(COMMANDS_QUEUED);
        let (sender, frames) = mpsc::channel(FRAMES_QUEUED);
        let socket = self.socket.clone();
        let timeout = self.timeout;
        let target = agent.clone();
        let spawned = std::thread::Builder::new()
            .name("tui-terminal".into())
            .spawn(move || stream(socket, timeout, target, receiver, sender));
        match spawned {
            Ok(thread) => {
                self.threads.push(thread);
                assert!(self.threads.len() <= THREADS_MAX);
                self.session = Some(Session {
                    agent,
                    commands,
                    frames,
                });
            }
            Err(_) => self.notice(agent, "terminal_unavailable"),
        }
    }

    fn command(&mut self, agent: &AgentId, command: Command) {
        let Some(session) = self
            .session
            .as_ref()
            .filter(|session| session.agent == *agent)
        else {
            return;
        };
        let reason = match session.commands.try_send(command) {
            Ok(()) => return,
            Err(TrySendError::Full(_)) => "terminal_input_backlog",
            Err(TrySendError::Closed(_)) => "terminal_closed",
        };
        let agent = session.agent.clone();
        self.session = None;
        if self.notices.len() < NOTICES_MAX {
            self.notices.push_back((
                agent,
                TerminalFrame::Closed {
                    reason: reason.into(),
                },
            ));
        }
    }

    fn notice(&mut self, agent: AgentId, code: &str) {
        if self.notices.len() < NOTICES_MAX {
            self.notices
                .push_back((agent, TerminalFrame::Rejected { code: code.into() }));
        }
    }
}

/// Runs one terminal session on its own thread: attaches, forwards frames
/// and sends queued keys and sizes until the screens let go, then detaches.
/// Inputs: engine socket, timeout, agent, command and frame channels.
/// Output: none; the thread ends once the session is over.
fn stream(
    socket: PathBuf,
    timeout: Duration,
    agent: AgentId,
    mut commands: mpsc::Receiver<Command>,
    frames: mpsc::Sender<TerminalFrame>,
) {
    let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        let _ = frames.blocking_send(TerminalFrame::Rejected {
            code: "terminal_unavailable".into(),
        });
        return;
    };
    runtime.block_on(async move {
        let TerminalSession {
            agent,
            cols,
            rows,
            output,
            mut control,
        } = match open(socket, timeout, agent).await {
            Ok(session) => session,
            Err(code) => {
                let _ = frames.send(TerminalFrame::Rejected { code }).await;
                return;
            }
        };
        let attached = TerminalFrame::Attached {
            agent_id: agent,
            cols,
            rows,
        };
        if frames.send(attached).await.is_err() {
            return;
        }
        let reader = tokio::spawn(forward(output, frames));
        while let Some(command) = commands.recv().await {
            let sent = match command {
                Command::Input(bytes) => control.input(bytes).await,
                Command::Resize { cols, rows } => control.resize(cols, rows).await,
            };
            if sent.is_err() {
                break;
            }
        }
        let _ = control.detach().await;
        let _ = tokio::time::timeout(DRAIN_TIME, reader).await;
    });
}

/// Opens an owner connection and attaches to the agent's terminal.
/// Inputs: engine socket, timeout and agent. Output: the session, or a plain
/// bounded code such as `terminal_agent_unknown`.
async fn open(
    socket: PathBuf,
    timeout: Duration,
    agent: AgentId,
) -> Result<TerminalSession, String> {
    let problem = |error: crate::engine::EngineError| bounded_problem(&error.0);
    let client = Client::new(socket, timeout).map_err(problem)?;
    let (_, owner, _) = client
        .with_owner(OwnerClientKind::Tui)
        .await
        .map_err(problem)?;
    owner.terminal_attach(agent).await.map_err(problem)
}

/// Forwards engine frames to the screens in order until the terminal closes
/// or the screens let go.
/// Inputs: the output half and the frame channel. Output: none.
async fn forward(mut output: TerminalOutput, frames: mpsc::Sender<TerminalFrame>) {
    loop {
        let frame = output
            .next()
            .await
            .unwrap_or_else(|error| TerminalFrame::Closed {
                reason: bounded_problem(&error.0),
            });
        let last = matches!(frame, TerminalFrame::Closed { .. });
        if frames.send(frame).await.is_err() || last {
            return;
        }
    }
}
