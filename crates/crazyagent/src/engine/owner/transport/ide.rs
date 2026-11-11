use super::*;
use crate::ide::bridge::reveal::{self, RevealFocus, RevealTarget};
use crate::protocol::{ProjectId, RequestId, encode_frame};
use serde::{Deserialize, Serialize};

const REQUEST_BYTES_MAX: usize = 8192;
const DESKTOP_SLOT: u8 = 1;
pub(super) const IDE_TIMEOUT: Duration = Duration::from_secs(reveal::REVEAL_WAIT.as_secs() + 5);

/// Every code an IDE request can answer an owner client with: the reveal's refusals, this
/// protocol's own, and the owner connection's it travels on.
pub const IDE_CODES: &[&str] = &[
    "changes_not_found",
    "ide_focus_desktop_only",
    "ide_not_running",
    "ide_outcome_unknown",
    "ide_path_unreadable",
    "ide_project_folder_invalid",
    "ide_project_unknown",
    "ide_reply_bounds",
    "ide_request_bounds",
    "ide_request_invalid",
    "ide_reveal_busy",
    "ide_reveal_invalid",
    "ide_reveal_missing",
    "ide_reveal_not_listed",
    "ide_reveal_outside",
    "ide_reveal_unanswered",
    "ide_reveal_unavailable",
    "ide_unavailable",
    "ide_workspace_unavailable",
    "owner_authentication_failed",
    "owner_authority_unavailable",
    "owner_busy",
    "owner_grant_stale_or_revoked",
    "owner_request_failed",
    "owner_root_replaced_or_unsafe",
    "owner_root_unavailable",
    "owner_socket_replaced",
    "owner_socket_unsafe",
    "peer_owner_mismatch",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Action {
    Reveal {
        project: ProjectId,
        target: RevealTarget,
    },
    Focus,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    ide: Action,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Reply {
    Revealed { request: RequestId },
    Focus { focus: Option<RevealFocus> },
    Rejected { code: String },
}

fn unknown() -> EngineError {
    failure("ide_outcome_unknown")
}

impl OwnerClient {
    /// Shows a file or folder in the project's open desktop IDE and waits until the IDE
    /// says it showed it. Inputs: the project and the target. Output: the reveal's id, or
    /// `ide_not_running` when the desktop has no IDE open for the project.
    pub async fn ide_reveal(
        &self,
        project: ProjectId,
        target: RevealTarget,
    ) -> Result<RequestId, EngineError> {
        match self
            .ide_exchange(Action::Reveal { project, target })
            .await?
        {
            Reply::Revealed { request } => Ok(request),
            _ => Err(unknown()),
        }
    }

    /// Takes the project whose IDE tab the desktop should bring forward after a reveal,
    /// once. Only the desktop client may take it. Output: the project and reveal, or none.
    pub async fn ide_focus(&self) -> Result<Option<RevealFocus>, EngineError> {
        match self.ide_exchange(Action::Focus).await? {
            Reply::Focus { focus } => Ok(focus),
            _ => Err(unknown()),
        }
    }

    async fn ide_exchange(&self, action: Action) -> Result<Reply, EngineError> {
        tokio::time::timeout(self.timeout.max(IDE_TIMEOUT), async {
            let frame = encode_frame(&Envelope { ide: action })
                .map_err(|_| failure("ide_request_bounds"))?;
            self.root.verify()?;
            if self.root.socket_identity()? != self.socket {
                return Err(failure("owner_socket_replaced"));
            }
            let mut stream = crate::spawn_gate::connect_unix(self.root.path.join(SOCKET))
                .await
                .map_err(|_| failure("ide_unavailable"))?;
            verify_peer(&stream)?;
            let mut packet = [0; 37];
            packet[..4].copy_from_slice(b"CAO1");
            packet[4] = match self.kind {
                OwnerClientKind::Tui => 0,
                OwnerClientKind::Desktop => DESKTOP_SLOT,
            };
            packet[5..].copy_from_slice(&self.token);
            stream
                .write_all(&packet)
                .await
                .map_err(|_| failure("ide_unavailable"))?;
            stream
                .write_all(&frame)
                .await
                .map_err(|_| failure("ide_unavailable"))?;
            let frame = read_frame(&mut stream).await.map_err(|_| unknown())?;
            match serde_json::from_slice(&frame[4..]).map_err(|_| unknown())? {
                Reply::Rejected { code } => Err(EngineError(code)),
                reply => Ok(reply),
            }
        })
        .await
        .map_err(|_| unknown())?
    }
}

/// Whether a request body is a reveal, which waits on the IDE and so is served past the
/// owner exchange; a focus read is answered at once. Input: the body. Output: the answer,
/// false for a body that does not parse.
pub(super) fn deferred(bytes: &[u8]) -> bool {
    matches!(
        serde_json::from_slice::<Envelope>(bytes),
        Ok(Envelope {
            ide: Action::Reveal { .. }
        })
    )
}

/// Encodes this protocol's refusal of a request the engine did not run.
/// Input: the refusal code. Output: the reply frame.
pub(super) fn refusal(code: &str) -> Result<Vec<u8>, EngineError> {
    encode_frame(&Reply::Rejected { code: code.into() }).map_err(|_| unknown())
}

/// Serves one admitted IDE request and writes its reply. Inputs: the stream, the engine,
/// the server authority, the handshake packet, its admission and the request body.
/// Output: Ok once the reply is written.
pub(super) async fn serve(
    stream: &mut UnixStream,
    engine: &Arc<Engine>,
    authority: &Arc<Authority>,
    packet: &[u8; 37],
    admission: &Admission,
    bytes: &[u8],
) -> Result<(), EngineError> {
    let reply = match handle(engine, authority, packet, admission, bytes).await {
        Ok(reply) => reply,
        Err(error) => Reply::Rejected { code: error.0 },
    };
    let frame = match encode_frame(&reply) {
        Ok(frame) => frame,
        Err(_) => encode_frame(&Reply::Rejected {
            code: "ide_reply_bounds".into(),
        })
        .map_err(|_| unknown())?,
    };
    stream.write_all(&frame).await.map_err(io_error)
}

async fn handle(
    engine: &Arc<Engine>,
    authority: &Arc<Authority>,
    packet: &[u8; 37],
    admission: &Admission,
    bytes: &[u8],
) -> Result<Reply, EngineError> {
    if bytes.len() > REQUEST_BYTES_MAX {
        return Err(failure("ide_request_bounds"));
    }
    let Envelope { ide: action } =
        serde_json::from_slice(bytes).map_err(|_| failure("ide_request_invalid"))?;
    {
        let _barrier = engine.owner_egress.write().await;
        if &authority.authenticate(packet)? != admission {
            return Err(failure("owner_grant_stale_or_revoked"));
        }
    }
    let runtime = OwnerRuntimeHandle {
        engine: engine.clone(),
        authority: authority.clone(),
    };
    match action {
        Action::Reveal { project, target } => Ok(Reply::Revealed {
            request: reveal::reveal(&runtime, &project, target).await?,
        }),
        Action::Focus if packet[4] == DESKTOP_SLOT => Ok(Reply::Focus {
            focus: reveal::focus(&runtime),
        }),
        Action::Focus => Err(failure("ide_focus_desktop_only")),
    }
}
