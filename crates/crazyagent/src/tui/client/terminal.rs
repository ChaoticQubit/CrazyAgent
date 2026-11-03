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

struct Session {
    agent: AgentId,
    pane: Option<String>,
    commands: mpsc::Sender<Vec<u8>>,
    frames: mpsc::Receiver<TerminalFrame>,
}

/// An attach the screens asked for, started at the next poll or key so the size the screens
/// send right after it goes with it and the engine sizes the window before it reads the screen.
struct Wanted {
    agent: AgentId,
    pane: Option<String>,
    size: Option<(u16, u16)>,
}

/// The TUI's live agent terminal: one attached session at a time, each on its
/// own thread and owner connection so a slow engine read never holds back
/// typing. A new size starts the view again at that size, because tmux reflows its
/// screen on a resize and the screens' vt100 copy cannot. Input: terminal requests
/// from the screens. Output: terminal frames polled by the run loop, ending with
/// Closed or Rejected.
pub(in crate::tui) struct Terminals {
    socket: PathBuf,
    timeout: Duration,
    session: Option<Session>,
    wanted: Option<Wanted>,
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
            wanted: None,
            threads: Vec::new(),
            notices: VecDeque::new(),
        }
    }

    /// Runs one terminal request from the screens.
    /// Input: an attach, input, resize or detach request. Output: none; any
    /// failure comes back from `poll` as a Closed or Rejected frame.
    pub fn handle(&mut self, request: Request) {
        match request {
            Request::TerminalAttach { agent } => self.want(agent, None),
            Request::TerminalAttachPane { agent, pane } => {
                assert!(crate::terminal::pane_id_valid(&pane));
                self.want(agent, Some(pane));
            }
            Request::TerminalInput { agent, bytes } => {
                assert!(!bytes.is_empty() && bytes.len() <= INPUT_BYTES_MAX);
                self.start();
                self.command(&agent, bytes);
            }
            Request::TerminalResize { agent, cols, rows } => {
                assert!((SIZE_MIN..=SIZE_MAX).contains(&cols));
                assert!((SIZE_MIN..=SIZE_MAX).contains(&rows));
                self.resize(agent, cols, rows);
            }
            Request::TerminalDetach { agent } => {
                if self
                    .session
                    .as_ref()
                    .is_some_and(|session| session.agent == agent)
                {
                    self.session = None;
                }
                if self
                    .wanted
                    .as_ref()
                    .is_some_and(|wanted| wanted.agent == agent)
                {
                    self.wanted = None;
                }
            }
            other => unreachable!("not a terminal request: {other:?}"),
        }
    }

    /// Takes the next frame for the screens, oldest first.
    /// Input: none. Output: the agent and frame, or None while nothing waits;
    /// a Closed or Rejected frame ends the session.
    pub fn poll(&mut self) -> Option<(AgentId, TerminalFrame)> {
        self.start();
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
        self.session.is_some() || self.wanted.is_some() || !self.notices.is_empty()
    }

    /// Lets go of the attached session and asks for a view of the agent, or one pane of
    /// its window, that starts at the next poll or key. Takes the agent and the pane.
    fn want(&mut self, agent: AgentId, pane: Option<String>) {
        self.session = None;
        self.wanted = Some(Wanted {
            agent,
            pane,
            size: None,
        });
    }

    /// Gives a waiting attach its size, or starts the attached view again at the new size
    /// so the engine sends the screen as tmux reflowed it. A size for an agent with no
    /// session or attach is dropped. Takes the agent, columns and rows.
    fn resize(&mut self, agent: AgentId, cols: u16, rows: u16) {
        if let Some(wanted) = self.wanted.as_mut().filter(|wanted| wanted.agent == agent) {
            wanted.size = Some((cols, rows));
            return;
        }
        if let Some(session) = self.session.take_if(|session| session.agent == agent) {
            self.wanted = Some(Wanted {
                agent,
                pane: session.pane,
                size: Some((cols, rows)),
            });
        }
    }

    /// Starts the waiting attach on its own thread once fewer than `THREADS_MAX` earlier
    /// sessions are still closing; until then it keeps waiting, since each one ends within
    /// its timeout and `DRAIN_TIME`. Input: none. Output: none.
    fn start(&mut self) {
        self.threads.retain(|thread| !thread.is_finished());
        if self.threads.len() >= THREADS_MAX {
            return;
        }
        let Some(Wanted { agent, pane, size }) = self.wanted.take() else {
            return;
        };
        let (commands, receiver) = mpsc::channel(COMMANDS_QUEUED);
        let (sender, frames) = mpsc::channel(FRAMES_QUEUED);
        let socket = self.socket.clone();
        let timeout = self.timeout;
        let target = agent.clone();
        let view = pane.clone();
        let spawned = std::thread::Builder::new()
            .name("tui-terminal".into())
            .spawn(move || stream(socket, timeout, target, view, size, receiver, sender));
        match spawned {
            Ok(thread) => {
                self.threads.push(thread);
                assert!(self.threads.len() <= THREADS_MAX);
                self.session = Some(Session {
                    agent,
                    pane,
                    commands,
                    frames,
                });
            }
            Err(_) => self.notice(agent, "terminal_unavailable"),
        }
    }

    fn command(&mut self, agent: &AgentId, bytes: Vec<u8>) {
        let Some(session) = self
            .session
            .as_ref()
            .filter(|session| session.agent == *agent)
        else {
            return;
        };
        let reason = match session.commands.try_send(bytes) {
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

/// Runs one terminal session on its own thread: attaches at the size, if any,
/// forwards frames and sends queued keys until the screens let go, then detaches.
/// Inputs: engine socket, timeout, agent, the pane of its window to view
/// instead of its own, the size, key and frame channels.
/// Output: none; the thread ends once the session is over.
fn stream(
    socket: PathBuf,
    timeout: Duration,
    agent: AgentId,
    pane: Option<String>,
    size: Option<(u16, u16)>,
    mut commands: mpsc::Receiver<Vec<u8>>,
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
        } = match open(socket, timeout, agent, pane, size).await {
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
        while let Some(bytes) = commands.recv().await {
            if control.input(bytes).await.is_err() {
                break;
            }
        }
        let _ = control.detach().await;
        let _ = tokio::time::timeout(DRAIN_TIME, reader).await;
    });
}

/// Opens an owner connection and attaches to the agent's terminal, or to one
/// pane of its window such as a native teammate's, sized first when a size is given.
/// Inputs: engine socket, timeout, agent, the pane and the size, if any. Output: the
/// session, or a plain bounded code such as `terminal_agent_unknown`.
async fn open(
    socket: PathBuf,
    timeout: Duration,
    agent: AgentId,
    pane: Option<String>,
    size: Option<(u16, u16)>,
) -> Result<TerminalSession, String> {
    let problem = |error: crate::engine::EngineError| bounded_problem(&error.0);
    let client = Client::new(socket, timeout).map_err(problem)?;
    let (_, owner, _) = client
        .with_owner(OwnerClientKind::Tui)
        .await
        .map_err(problem)?;
    match (pane, size) {
        (pane, Some((cols, rows))) => owner.terminal_attach_sized(agent, pane, cols, rows).await,
        (Some(pane), None) => owner.terminal_attach_pane(agent, pane).await,
        (None, None) => owner.terminal_attach(agent).await,
    }
    .map_err(problem)
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
