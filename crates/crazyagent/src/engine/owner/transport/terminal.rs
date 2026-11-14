use super::*;
use crate::platform::unix_socket::{OwnedReadHalf, OwnedWriteHalf};
use crate::protocol::encode_frame;
use crate::terminal::{
    AgentId, Host, INPUT_BYTES_MAX, PROMPT_BYTES_MAX, TerminalFrame, TerminalRequest,
    TerminalStream,
};
#[cfg(unix)]
use crate::terminal::{CHUNK_BYTES_MAX, HostError, type_prompt};
use tokio::io::AsyncWrite;

pub(super) const MAGIC: &[u8; 4] = b"CAT1";
#[cfg(unix)]
const GRANT_CHECK: Duration = Duration::from_secs(5);
const WRITE_TIME: Duration = Duration::from_secs(10);
const VIEWERS_FULL: &str = "terminal_viewers_full";
#[cfg(unix)]
const REQUESTS_QUEUED: usize = 16;
#[cfg(unix)]
const VIEWER_GONE: &str = "terminal_viewer_gone";
#[cfg(unix)]
const GRANT_ENDED: &str = "owner_grant_stale_or_revoked";

/// An owner's live view of one agent terminal: its size at attach, then output and controls.
/// `output` gives the screen and then live output in order; `control` types, resizes and detaches.
/// Drive each half from one task: a frame read dropped part way loses the connection.
/// Dropping `control` closes the viewer's side, which the engine takes as a detach.
pub struct TerminalSession {
    pub agent: AgentId,
    pub cols: u16,
    pub rows: u16,
    pub output: TerminalOutput,
    pub control: TerminalControl,
}

/// The engine-to-viewer half of a terminal session.
pub struct TerminalOutput {
    reader: OwnedReadHalf,
}

/// The viewer-to-engine half of a terminal session.
pub struct TerminalControl {
    writer: OwnedWriteHalf,
}

impl TerminalOutput {
    /// Waits for the next frame from the engine. Takes the output half.
    /// Returns `Output` bytes, or the final `Closed` with its reason; a broken connection
    /// fails with `terminal_closed`.
    pub async fn next(&mut self) -> Result<TerminalFrame, EngineError> {
        let frame = read_frame(&mut self.reader)
            .await
            .map_err(|_| failure("terminal_closed"))?;
        match serde_json::from_slice(&frame[4..]).map_err(|_| failure("terminal_closed"))? {
            frame @ (TerminalFrame::Output { .. } | TerminalFrame::Closed { .. }) => Ok(frame),
            _ => Err(failure("terminal_closed")),
        }
    }
}

impl TerminalControl {
    /// Types bytes into the agent's terminal exactly as given, control keys included.
    /// Takes at most `INPUT_BYTES_MAX` bytes.
    /// Returns once the request is sent.
    pub async fn input(&mut self, bytes: Vec<u8>) -> Result<(), EngineError> {
        if bytes.len() > INPUT_BYTES_MAX {
            return Err(failure("terminal_input_bounds"));
        }
        self.send(&TerminalRequest::Input { bytes }).await
    }

    /// Types keys the view made from the owner's mouse wheel into the agent's terminal, as
    /// `input` types keys. Takes at most `INPUT_BYTES_MAX` bytes.
    /// Returns once the request is sent.
    pub async fn scroll(&mut self, bytes: Vec<u8>) -> Result<(), EngineError> {
        if bytes.len() > INPUT_BYTES_MAX {
            return Err(failure("terminal_input_bounds"));
        }
        self.send(&TerminalRequest::Scroll { bytes }).await
    }

    /// Sends the owner's explicit message to the agent: the engine types it as one submission
    /// and Enter, and a sub-agent's master hears a note of it on its next turn.
    /// Takes non-blank text of at most `PROMPT_BYTES_MAX` bytes.
    /// Returns once the request is sent.
    pub async fn prompt(&mut self, text: String) -> Result<(), EngineError> {
        if text.trim().is_empty() || text.len() > PROMPT_BYTES_MAX {
            return Err(failure("terminal_prompt_bounds"));
        }
        self.send(&TerminalRequest::Prompt { text }).await
    }

    /// Sets the agent terminal's size for its app and every viewer.
    /// Takes columns and rows; the engine applies sizes within the terminal bounds.
    /// Returns once the request is sent.
    pub async fn resize(&mut self, cols: u16, rows: u16) -> Result<(), EngineError> {
        self.send(&TerminalRequest::Resize { cols, rows }).await
    }

    /// Ends this view; the agent and its terminal keep running.
    /// Takes the control half.
    /// Returns once the engine was told and the connection closed.
    pub async fn detach(mut self) -> Result<(), EngineError> {
        self.send(&TerminalRequest::Detach).await?;
        self.writer
            .shutdown()
            .await
            .map_err(|_| failure("terminal_closed"))
    }

    async fn send(&mut self, request: &TerminalRequest) -> Result<(), EngineError> {
        let frame = encode_frame(request).map_err(|_| failure("terminal_request_bounds"))?;
        tokio::time::timeout(WRITE_TIME, self.writer.write_all(&frame))
            .await
            .map_err(|_| failure("terminal_closed"))?
            .map_err(|_| failure("terminal_closed"))
    }
}

impl OwnerClient {
    /// Opens a live view of one agent's terminal over the owner socket.
    /// Takes the terminal name; fails with the engine's plain code, such as
    /// `terminal_agent_unknown` or `terminal_unavailable`.
    /// Returns the session with the size, the screen then live output, and the controls.
    #[cfg(unix)]
    pub async fn terminal_attach(&self, agent: AgentId) -> Result<TerminalSession, EngineError> {
        let request = TerminalRequest::Attach {
            agent_id: agent.clone(),
        };
        self.attach_with(agent, &request).await
    }

    /// Opens a live view of one pane of an agent terminal's own window, such as a native
    /// teammate's, over the owner socket; typing, prompts and resizes then go to that pane.
    /// Takes the terminal name and a pane id the team lists for it; fails like
    /// `terminal_attach`, or with `terminal_pane_unknown` for a pane of another window.
    /// Returns the session.
    #[cfg(unix)]
    pub async fn terminal_attach_pane(
        &self,
        agent: AgentId,
        pane: String,
    ) -> Result<TerminalSession, EngineError> {
        let request = TerminalRequest::AttachPane {
            agent_id: agent.clone(),
            pane,
        };
        self.attach_with(agent, &request).await
    }

    /// Opens a live view like `terminal_attach`, or `terminal_attach_pane` with a pane, after
    /// the engine sizes the agent's window to the viewer, so the screen and every output
    /// byte after it are drawn for that size and a viewer that cannot reflow stays in step.
    /// Takes the terminal name, the pane if any, and the viewer's columns and rows within
    /// `SIZE_MIN..=SIZE_MAX`; fails like those, or with `terminal_size_bounds`.
    /// Returns the session with the size the engine reports.
    #[cfg(unix)]
    pub async fn terminal_attach_sized(
        &self,
        agent: AgentId,
        pane: Option<String>,
        cols: u16,
        rows: u16,
    ) -> Result<TerminalSession, EngineError> {
        let request = TerminalRequest::AttachSized {
            agent_id: agent.clone(),
            pane,
            cols,
            rows,
        };
        self.attach_with(agent, &request).await
    }

    #[cfg(unix)]
    async fn attach_with(
        &self,
        agent: AgentId,
        request: &TerminalRequest,
    ) -> Result<TerminalSession, EngineError> {
        let unavailable = || failure("terminal_owner_unavailable");
        let (stream, cols, rows) = tokio::time::timeout(self.timeout, async {
            let frame = encode_frame(request).map_err(|_| failure("terminal_request_bounds"))?;
            self.root.verify()?;
            if self.root.socket_identity()? != self.socket {
                return Err(failure("owner_socket_replaced"));
            }
            let mut stream = crate::spawn_gate::connect_unix(self.root.path.join(SOCKET))
                .await
                .map_err(|_| unavailable())?;
            verify_peer(&stream)?;
            let mut packet = [0; 37];
            packet[..4].copy_from_slice(MAGIC);
            packet[4] = match self.kind {
                OwnerClientKind::Tui => 0,
                OwnerClientKind::Desktop => 1,
            };
            packet[5..].copy_from_slice(&self.token);
            stream.write_all(&packet).await.map_err(|_| unavailable())?;
            stream.write_all(&frame).await.map_err(|_| unavailable())?;
            let reply = read_frame(&mut stream)
                .await
                .map_err(|_| failure("terminal_outcome_unknown"))?;
            match serde_json::from_slice(&reply[4..])
                .map_err(|_| failure("terminal_outcome_unknown"))?
            {
                TerminalFrame::Attached {
                    agent_id,
                    cols,
                    rows,
                } if agent_id == agent => Ok((stream, cols, rows)),
                TerminalFrame::Rejected { code } => Err(EngineError(code)),
                _ => Err(failure("terminal_outcome_unknown")),
            }
        })
        .await
        .map_err(|_| failure("terminal_outcome_unknown"))??;
        let (reader, writer) = stream.into_split();
        Ok(TerminalSession {
            agent,
            cols,
            rows,
            output: TerminalOutput { reader },
            control: TerminalControl { writer },
        })
    }
}

impl OwnerRuntimeHandle {
    /// Returns the engine's terminal host on its private tmux server, opening it on first use
    /// and again whenever the tmux program found changes; see [`Engine::terminal`].
    /// Engine launchers start every interactive agent through it; viewers attach over the
    /// owner socket. While tmux is missing every call fails with `terminal_unavailable`.
    pub fn terminal(&self) -> Result<Arc<dyn Host>, EngineError> {
        self.engine.terminal(&engine_root(&self.authority)?)
    }
}

impl Engine {
    /// Gives the terminal host on the tmux program found now. The host is kept while that
    /// program stays the one found and opened anew when it changes, such as when setup
    /// installs its own copy over an unsupported one or an upgrade removes the old one.
    /// Takes the engine root. Returns the host, or `terminal_unavailable`.
    #[cfg(unix)]
    pub(crate) fn terminal(&self, root: &Path) -> Result<Arc<dyn Host>, EngineError> {
        self.terminal_on(root, crate::terminal::tmux::installed(root))
    }

    /// Gives the terminal host on the tmux program `found`, as [`Engine::terminal`] does with
    /// the program found now: the kept host while `found` is its program, else a new host on
    /// `found`, kept in its place. Takes the engine root and the program, none when tmux is
    /// not installed. Returns the host, or `terminal_unavailable`.
    #[cfg(unix)]
    pub(super) fn terminal_on(
        &self,
        root: &Path,
        found: Option<std::path::PathBuf>,
    ) -> Result<Arc<dyn Host>, EngineError> {
        let mut slot = self
            .terminal
            .lock()
            .map_err(|_| failure("terminal_unavailable"))?;
        if let Some((program, host)) = slot.as_ref()
            && *program == found
        {
            return Ok(host.clone());
        }
        let host = crate::terminal::tmux_host(root, found.clone());
        *slot = Some((found, host.clone()));
        Ok(host)
    }

    /// Names in a team graph read the terminals of finished workers whose last screen the
    /// open terminal host still keeps (N-D21). Any other read, and every read before a host
    /// opened, passes unchanged: a host opened later keeps no screen yet. Takes the read.
    /// Returns it, or `terminal_unavailable` when the host check could not finish.
    #[cfg(unix)]
    pub(crate) async fn kept_named(
        &self,
        read: OwnerReadReply,
    ) -> Result<OwnerReadReply, EngineError> {
        let OwnerReadReply {
            revision,
            config_id,
            data,
        } = read;
        let host = self
            .terminal
            .lock()
            .ok()
            .and_then(|slot| slot.as_ref().map(|(_, host)| host.clone()));
        let data = match (data, host) {
            (OwnerData::TeamGraph { mut graph }, Some(host)) => {
                tokio::task::spawn_blocking(move || {
                    crate::team::graph::name_kept(&mut graph, |agent| host.kept(agent));
                    OwnerData::TeamGraph { graph }
                })
                .await
                .map_err(|_| failure("terminal_unavailable"))?
            }
            (data, _) => data,
        };
        Ok(OwnerReadReply {
            revision,
            config_id,
            data,
        })
    }

    /// Passes a read unchanged where no terminal host opens, so no finished worker's screen
    /// is kept to name. Takes the read. Returns it.
    #[cfg(not(unix))]
    pub(crate) async fn kept_named(
        &self,
        read: OwnerReadReply,
    ) -> Result<OwnerReadReply, EngineError> {
        Ok(read)
    }

    /// Refuses a terminal host where no tmux runs: no agent pane opens off the macOS
    /// confinement, and the engine does not run on Windows in this version. Takes the engine
    /// root, unused. Returns `terminal_unavailable`.
    #[cfg(not(unix))]
    pub(crate) fn terminal(&self, _root: &Path) -> Result<Arc<dyn Host>, EngineError> {
        Err(failure("terminal_unavailable"))
    }
}

fn engine_root(authority: &Authority) -> Result<PathBuf, EngineError> {
    authority.root.verify()?;
    authority
        .root
        .path
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| failure("terminal_unavailable"))
}

async fn granted(
    engine: &Engine,
    authority: &Authority,
    packet: &[u8; 37],
) -> Result<Admission, EngineError> {
    let _barrier = engine.owner_egress.write().await;
    authority.authenticate(packet)
}

/// Checks before each typed key, prompt or resize that the viewer's grant still stands.
/// It reads the authority directly, without the owner barrier, so typing never waits on
/// other owner work and a revocation published a moment ago already refuses the next key.
/// Inputs: the authority, the viewer's packet and its admission. Output: whether it holds.
#[cfg(unix)]
fn still_granted(authority: &Authority, packet: &[u8; 37], admission: &Admission) -> bool {
    authority.authenticate(packet).ok().as_ref() == Some(admission)
}

async fn write(
    stream: &mut (impl AsyncWrite + Unpin),
    frame: &TerminalFrame,
) -> Result<(), EngineError> {
    let bytes = encode_frame(frame).map_err(|_| failure("terminal_frame_bounds"))?;
    tokio::time::timeout(WRITE_TIME, stream.write_all(&bytes))
        .await
        .map_err(|_| failure("terminal_viewer_slow"))?
        .map_err(io_error)
}

#[cfg(unix)]
async fn act(
    host: &Arc<dyn Host>,
    agent: &AgentId,
    request: TerminalRequest,
) -> Result<(), HostError> {
    let host = host.clone();
    let agent = agent.clone();
    tokio::task::spawn_blocking(move || match request {
        TerminalRequest::Input { bytes } | TerminalRequest::Scroll { bytes } => {
            host.input(&agent, &bytes)
        }
        TerminalRequest::Prompt { text } => type_prompt(host.as_ref(), &agent, &text).map(drop),
        TerminalRequest::Resize { cols, rows } => host.resize(&agent, cols, rows),
        TerminalRequest::Attach { .. }
        | TerminalRequest::AttachPane { .. }
        | TerminalRequest::AttachSized { .. }
        | TerminalRequest::Detach => Ok(()),
    })
    .await
    .unwrap_or_else(|_| Err(HostError("terminal_unavailable".into())))
}

#[cfg(unix)]
async fn prompt(host: &Arc<dyn Host>, agent: &AgentId, text: String) -> Result<bool, HostError> {
    let host = host.clone();
    let agent = agent.clone();
    tokio::task::spawn_blocking(move || type_prompt(host.as_ref(), &agent, &text))
        .await
        .unwrap_or_else(|_| Err(HostError("terminal_unavailable".into())))
}

/// The attach handshake, all of it bounded by the attach deadline: checks the peer and the
/// owner grant, reads the `Attach`, `AttachPane` or `AttachSized` request, sizes the window
/// first for `AttachSized`, and attaches to the agent terminal or that one pane of its
/// window. Takes the connection, its packet, the engine and
/// the authority. Writes nothing, so a refusal can still be sent whole. Returns the
/// admission, the attached viewer, its host and the team agent the owner's prompts and
/// typing count for, none for a pane, which is not that agent's own app.
async fn open(
    stream: &mut UnixStream,
    packet: &[u8; 37],
    engine: &Arc<Engine>,
    authority: &Arc<Authority>,
) -> Result<(Admission, TerminalStream, Arc<dyn Host>, Option<i64>), EngineError> {
    verify_peer(stream)?;
    let admission = granted(engine, authority, packet).await?;
    let frame = read_frame(stream).await?;
    let (agent_id, pane, size) = match serde_json::from_slice(&frame[4..])
        .map_err(|_| failure("terminal_request_invalid"))?
    {
        TerminalRequest::Attach { agent_id } => (agent_id, None, None),
        TerminalRequest::AttachPane { agent_id, pane } => (agent_id, Some(pane), None),
        TerminalRequest::AttachSized {
            agent_id,
            pane,
            cols,
            rows,
        } => (agent_id, pane, Some((cols, rows))),
        _ => return Err(failure("terminal_request_invalid")),
    };
    let member = agent_id.team_agent().filter(|_| pane.is_none());
    let engine_host = engine.terminal(&engine_root(authority)?)?;
    let host = match &pane {
        Some(pane) => crate::terminal::pane_view(engine_host, pane)?,
        None => engine_host,
    };
    let attaching = host.clone();
    let viewer = tokio::task::spawn_blocking(move || {
        if let Some((cols, rows)) = size {
            attaching.resize(&agent_id, cols, rows)?;
        }
        attaching.attach(&agent_id)
    })
    .await
    .map_err(|_| failure("terminal_unavailable"))??;
    Ok((admission, viewer, host, member))
}

/// Starts an attached view: the `Attached` frame, then the pane's history and screen, each
/// frame bounded by `WRITE_TIME` instead of the attach deadline and the grant checked again
/// every `GRANT_CHECK`. Takes the writer, the viewer, the engine, authority, packet and
/// admission. Returns why the view ends early: `VIEWER_GONE` after a failed write, when the
/// stream may hold part of a frame, or `GRANT_ENDED`.
#[cfg(unix)]
async fn replay(
    writer: &mut (impl AsyncWrite + Unpin),
    viewer: &TerminalStream,
    engine: &Engine,
    authority: &Authority,
    packet: &[u8; 37],
    admission: &Admission,
) -> Result<(), &'static str> {
    let attached = TerminalFrame::Attached {
        agent_id: viewer.agent.clone(),
        cols: viewer.cols,
        rows: viewer.rows,
    };
    write(writer, &attached).await.map_err(|_| VIEWER_GONE)?;
    let mut checked = Instant::now();
    for chunk in viewer
        .history
        .chunks(CHUNK_BYTES_MAX)
        .chain(viewer.screen.chunks(CHUNK_BYTES_MAX))
    {
        if checked.elapsed() >= GRANT_CHECK {
            if granted(engine, authority, packet).await.ok().as_ref() != Some(admission) {
                return Err(GRANT_ENDED);
            }
            checked = Instant::now();
        }
        let output = TerminalFrame::Output {
            bytes: chunk.to_vec(),
        };
        write(writer, &output).await.map_err(|_| VIEWER_GONE)?;
    }
    Ok(())
}

/// What a sent prompt does to the owner's line for the owner typing signal: a line sent
/// with Enter.
#[cfg(unix)]
const PROMPT_SENT: &[u8] = b"\r";

/// The keys an owner request puts into a team agent's terminal, for the owner typing
/// signal: the typed keys, or for a prompt a line sent with Enter, since a prompt is the
/// owner's own text too and can answer a box on that screen. Takes the request. Returns
/// the keys, or none for a request that types nothing or only the view's wheel keys, which
/// are the owner reading, not typing (OB-10). A wheel request holding any other byte counts
/// as typed keys (OB10-L1).
#[cfg(unix)]
fn owner_keys(request: &TerminalRequest) -> Option<&[u8]> {
    match request {
        TerminalRequest::Scroll { bytes } if crate::team::master::wheel_only(bytes) => None,
        TerminalRequest::Input { bytes } | TerminalRequest::Scroll { bytes } => {
            Some(bytes.as_slice())
        }
        TerminalRequest::Prompt { .. } => Some(PROMPT_SENT),
        TerminalRequest::Attach { .. }
        | TerminalRequest::AttachPane { .. }
        | TerminalRequest::AttachSized { .. }
        | TerminalRequest::Resize { .. }
        | TerminalRequest::Detach => None,
    }
}

/// Records a prompt the owner sent to a team sub-agent for the master's next turn.
/// Takes the engine database, the team agent id and the prompt. A failure loses only the
/// note; the prompt already reached the terminal.
#[cfg(unix)]
async fn heard(database: PathBuf, agent: i64, text: String) {
    let _ = tokio::task::spawn_blocking(move || {
        let connection = rusqlite::Connection::open_with_flags(
            &database,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE
                | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX
                | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )
        .ok()?;
        connection.busy_timeout(Duration::from_secs(5)).ok()?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .and_then(|elapsed| i64::try_from(elapsed.as_millis()).ok())?;
        let _ = crate::team::direct::record(&connection, agent, &text, now);
        Some(())
    })
    .await;
}

#[cfg(not(unix))]
impl OwnerClient {
    /// Refuses a terminal view where the owner socket is a Unix domain socket the platform
    /// does not have. Takes the terminal name, unused. Returns `terminal_owner_unavailable`.
    pub async fn terminal_attach(&self, _agent: AgentId) -> Result<TerminalSession, EngineError> {
        Err(failure("terminal_owner_unavailable"))
    }

    /// Refuses a pane view for the same reason. Takes the terminal name and pane id, unused.
    /// Returns `terminal_owner_unavailable`.
    pub async fn terminal_attach_pane(
        &self,
        _agent: AgentId,
        _pane: String,
    ) -> Result<TerminalSession, EngineError> {
        Err(failure("terminal_owner_unavailable"))
    }

    /// Refuses a sized view for the same reason. Takes the terminal name, pane and size,
    /// unused. Returns `terminal_owner_unavailable`.
    pub async fn terminal_attach_sized(
        &self,
        _agent: AgentId,
        _pane: Option<String>,
        _cols: u16,
        _rows: u16,
    ) -> Result<TerminalSession, EngineError> {
        Err(failure("terminal_owner_unavailable"))
    }
}

/// Never runs where no Unix domain socket can exist: its stream has no value, so there is
/// no viewer to serve. Takes the same inputs as the Unix version, all unused.
#[cfg(not(unix))]
#[allow(clippy::too_many_arguments)]
async fn watch(
    stream: UnixStream,
    _packet: &[u8; 37],
    _admission: Admission,
    _viewer: TerminalStream,
    _host: Arc<dyn Host>,
    _member: Option<i64>,
    _engine: &Engine,
    _authority: &Authority,
) {
    match stream {}
}

#[cfg(unix)]
#[allow(clippy::too_many_arguments)]
async fn watch(
    stream: UnixStream,
    packet: &[u8; 37],
    admission: Admission,
    mut viewer: TerminalStream,
    host: Arc<dyn Host>,
    member: Option<i64>,
    engine: &Engine,
    authority: &Authority,
) {
    let agent = viewer.agent.clone();
    let database = engine_root(authority)
        .ok()
        .map(|root| root.join("engine.db"));
    let (mut reader, mut writer) = stream.into_split();
    let (requests, mut asked) = tokio::sync::mpsc::channel(REQUESTS_QUEUED);
    let listener = tokio::spawn(async move {
        while let Ok(frame) = read_frame(&mut reader).await {
            let Ok(request) = serde_json::from_slice::<TerminalRequest>(&frame[4..]) else {
                break;
            };
            if requests.send(request).await.is_err() {
                break;
            }
        }
    });
    let replayed = replay(&mut writer, &viewer, engine, authority, packet, &admission).await;
    let mut check = tokio::time::interval(GRANT_CHECK);
    let reason = match replayed {
        Err(reason) => reason,
        Ok(()) => loop {
            tokio::select! {
                chunk = viewer.next() => match chunk {
                    Some(bytes) => {
                        if write(&mut writer, &TerminalFrame::Output { bytes }).await.is_err() {
                            break VIEWER_GONE;
                        }
                    }
                    None => break "terminal_ended",
                },
                request = asked.recv() => match request {
                    None | Some(TerminalRequest::Detach) => break "terminal_detached",
                    Some(
                        TerminalRequest::Attach { .. }
                        | TerminalRequest::AttachPane { .. }
                        | TerminalRequest::AttachSized { .. },
                    ) => {
                        break "terminal_request_invalid";
                    }
                    Some(_) if !still_granted(authority, packet, &admission) => {
                        break GRANT_ENDED;
                    }
                    Some(request) => {
                        if let (Some(keys), Some(member)) = (owner_keys(&request), member)
                            && let Some(agents) = engine.agents_if_open()
                        {
                            agents.typing().record(member, keys, crate::team::now_ms());
                        }
                        match request {
                            TerminalRequest::Prompt { text } => {
                                let typed = prompt(&host, &agent, text.clone()).await;
                                if typed.as_ref().is_err_and(|error| error.0 == "terminal_agent_unknown") {
                                    break "terminal_ended";
                                }
                                if let (Ok(true), Some(member), Some(database)) = (typed, member, &database) {
                                    heard(database.clone(), member, text).await;
                                }
                            }
                            request => {
                                if act(&host, &agent, request).await.is_err_and(|error| error.0 == "terminal_agent_unknown") {
                                    break "terminal_ended";
                                }
                            }
                        }
                    }
                },
                _ = check.tick() => {
                    if granted(engine, authority, packet).await.ok().as_ref() != Some(&admission) {
                        break GRANT_ENDED;
                    }
                }
            }
        },
    };
    listener.abort();
    if reason != VIEWER_GONE {
        let closed = TerminalFrame::Closed {
            reason: reason.into(),
        };
        let _ = write(&mut writer, &closed).await;
    }
    let _ = writer.shutdown().await;
}

/// Serves one owner terminal connection: checks the owner and attaches within the attach
/// deadline, then replays and streams under per-write limits until the viewer detaches, the
/// terminal ends or the owner's grant changes. Only a failed handshake gets `Rejected`.
/// Takes the connection, its `CAT1` packet, the engine, the authority and the attach deadline.
pub(super) async fn serve(
    mut stream: UnixStream,
    auth: [u8; 37],
    engine: Arc<Engine>,
    authority: Arc<Authority>,
    deadline: Instant,
) {
    let mut packet = auth;
    packet[..4].copy_from_slice(b"CAO1");
    let opened = tokio::time::timeout_at(deadline, open(&mut stream, &packet, &engine, &authority))
        .await
        .unwrap_or_else(|_| Err(failure("terminal_attach_timeout")));
    match opened {
        Ok((admission, viewer, host, member)) => {
            watch(
                stream, &packet, admission, viewer, host, member, &engine, &authority,
            )
            .await;
        }
        Err(error) => {
            let _ = write(&mut stream, &TerminalFrame::Rejected { code: error.0 }).await;
            let _ = stream.shutdown().await;
        }
    }
}

/// Answers a terminal view the engine has no room for with `terminal_viewers_full` in the
/// attach reply's own shape, once the peer and its owner grant check out and its attach
/// request is read, all within the attach deadline; then closes the connection.
/// Takes the connection, its `CAT1` packet, the authority and the attach deadline.
pub(super) async fn refuse(
    mut stream: UnixStream,
    auth: [u8; 37],
    authority: Arc<Authority>,
    deadline: Instant,
) {
    let mut packet = auth;
    packet[..4].copy_from_slice(b"CAO1");
    let refused = async {
        verify_peer(&stream)?;
        authority.authenticate(&packet)?;
        read_frame(&mut stream).await?;
        let full = TerminalFrame::Rejected {
            code: VIEWERS_FULL.into(),
        };
        write(&mut stream, &full).await
    };
    let _ = tokio::time::timeout_at(deadline, refused).await;
    let _ = stream.shutdown().await;
}

#[cfg(all(test, unix))]
mod tests;
