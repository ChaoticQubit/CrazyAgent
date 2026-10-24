use super::*;
use crate::protocol::encode_frame;
use crate::terminal::{
    AgentId, CHUNK_BYTES_MAX, Host, HostError, INPUT_BYTES_MAX, TerminalFrame, TerminalRequest,
    TerminalStream,
};
use tokio::io::AsyncWrite;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};

pub(super) const MAGIC: &[u8; 4] = b"CAT1";
const GRANT_CHECK: Duration = Duration::from_secs(5);
const WRITE_TIME: Duration = Duration::from_secs(10);
const REQUESTS_QUEUED: usize = 16;

/// An owner's live view of one agent terminal: its size at attach, then output and controls.
/// `output` gives the screen and then live output in order; `control` types, resizes and detaches.
/// Drive each half from one task: a frame read dropped part way loses the connection.
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
    pub async fn terminal_attach(&self, agent: AgentId) -> Result<TerminalSession, EngineError> {
        let unavailable = || failure("terminal_owner_unavailable");
        let (stream, cols, rows) = tokio::time::timeout(self.timeout, async {
            let frame = encode_frame(&TerminalRequest::Attach {
                agent_id: agent.clone(),
            })
            .map_err(|_| failure("terminal_request_bounds"))?;
            self.root.verify()?;
            if self.root.socket_identity()? != self.socket {
                return Err(failure("owner_socket_replaced"));
            }
            let mut stream = UnixStream::connect(self.root.path.join(SOCKET))
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
    /// Returns the engine's terminal host on its private tmux server, opening it on first use.
    /// Engine launchers start every interactive agent through it; viewers attach over the
    /// owner socket. While tmux is missing every call fails with `terminal_unavailable`.
    pub fn terminal(&self) -> Result<Arc<dyn Host>, EngineError> {
        self.engine.terminal(&engine_root(&self.authority)?)
    }
}

impl Engine {
    pub(crate) fn terminal(&self, root: &Path) -> Result<Arc<dyn Host>, EngineError> {
        let mut slot = self
            .terminal
            .lock()
            .map_err(|_| failure("terminal_unavailable"))?;
        if let Some((true, host)) = slot.as_ref() {
            return Ok(host.clone());
        }
        let (ready, host) = crate::terminal::platform(root);
        *slot = Some((ready, host.clone()));
        Ok(host)
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

async fn act(
    host: &Arc<dyn Host>,
    agent: &AgentId,
    request: TerminalRequest,
) -> Result<(), HostError> {
    let host = host.clone();
    let agent = agent.clone();
    tokio::task::spawn_blocking(move || match request {
        TerminalRequest::Input { bytes } => host.input(&agent, &bytes),
        TerminalRequest::Resize { cols, rows } => host.resize(&agent, cols, rows),
        TerminalRequest::Attach { .. } | TerminalRequest::Detach => Ok(()),
    })
    .await
    .unwrap_or_else(|_| Err(HostError("terminal_unavailable".into())))
}

async fn open(
    stream: &mut UnixStream,
    packet: &[u8; 37],
    engine: &Arc<Engine>,
    authority: &Arc<Authority>,
) -> Result<(Admission, TerminalStream, Arc<dyn Host>), EngineError> {
    verify_peer(stream)?;
    let admission = granted(engine, authority, packet).await?;
    let frame = read_frame(stream).await?;
    let TerminalRequest::Attach { agent_id } =
        serde_json::from_slice(&frame[4..]).map_err(|_| failure("terminal_request_invalid"))?
    else {
        return Err(failure("terminal_request_invalid"));
    };
    let host = engine.terminal(&engine_root(authority)?)?;
    let attaching = host.clone();
    let viewer = tokio::task::spawn_blocking(move || attaching.attach(&agent_id))
        .await
        .map_err(|_| failure("terminal_unavailable"))??;
    write(
        stream,
        &TerminalFrame::Attached {
            agent_id: viewer.agent.clone(),
            cols: viewer.cols,
            rows: viewer.rows,
        },
    )
    .await?;
    for chunk in viewer.screen.chunks(CHUNK_BYTES_MAX) {
        write(
            stream,
            &TerminalFrame::Output {
                bytes: chunk.to_vec(),
            },
        )
        .await?;
    }
    Ok((admission, viewer, host))
}

async fn watch(
    stream: UnixStream,
    packet: &[u8; 37],
    admission: Admission,
    mut viewer: TerminalStream,
    host: Arc<dyn Host>,
    engine: &Engine,
    authority: &Authority,
) {
    let agent = viewer.agent.clone();
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
    let mut check = tokio::time::interval(GRANT_CHECK);
    let reason = loop {
        tokio::select! {
            chunk = viewer.next() => match chunk {
                Some(bytes) => {
                    if write(&mut writer, &TerminalFrame::Output { bytes }).await.is_err() {
                        break "terminal_viewer_gone";
                    }
                }
                None => break "terminal_ended",
            },
            request = asked.recv() => match request {
                None | Some(TerminalRequest::Detach) => break "terminal_detached",
                Some(TerminalRequest::Attach { .. }) => break "terminal_request_invalid",
                Some(request) => {
                    if act(&host, &agent, request).await.is_err_and(|error| error.0 == "terminal_agent_unknown") {
                        break "terminal_ended";
                    }
                }
            },
            _ = check.tick() => {
                if granted(engine, authority, packet).await.ok().as_ref() != Some(&admission) {
                    break "owner_grant_stale_or_revoked";
                }
            }
        }
    };
    listener.abort();
    let _ = write(
        &mut writer,
        &TerminalFrame::Closed {
            reason: reason.into(),
        },
    )
    .await;
    let _ = writer.shutdown().await;
}

/// Serves one owner terminal connection: checks the owner, attaches, then streams until the
/// viewer detaches, the terminal ends or the owner's grant changes.
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
        Ok((admission, viewer, host)) => {
            watch(
                stream, &packet, admission, viewer, host, &engine, &authority,
            )
            .await;
        }
        Err(error) => {
            let _ = write(&mut stream, &TerminalFrame::Rejected { code: error.0 }).await;
            let _ = stream.shutdown().await;
        }
    }
}
