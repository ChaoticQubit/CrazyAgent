use super::*;
use crate::ide::extensions::{self as policy, Approved, AskView, Registry};
use crate::protocol::{ProjectId, encode_frame};
use serde::{Deserialize, Serialize};

const REQUEST_BYTES_MAX: usize = 1024;
pub(super) const IDE_EXTENSIONS_TIMEOUT: Duration = Duration::from_secs(7 * 60);

/// Every code an IDE extensions request can answer an owner client with: the extension
/// policy's refusals, this protocol's own, and the owner connection's it travels on.
pub const IDE_EXTENSIONS_CODES: &[&str] = &[
    "changes_unavailable",
    "ide_entropy_unavailable",
    "ide_extension_ai_locked",
    "ide_extension_ask_missing",
    "ide_extension_download_failed",
    "ide_extension_hash_invalid",
    "ide_extension_hash_mismatch",
    "ide_extension_hash_missing",
    "ide_extension_invalid",
    "ide_extension_metadata_invalid",
    "ide_extension_not_found",
    "ide_extension_source_refused",
    "ide_extension_too_large",
    "ide_extensions_full",
    "ide_extensions_outcome_unknown",
    "ide_extensions_owner_unavailable",
    "ide_extensions_reply_bounds",
    "ide_extensions_request_bounds",
    "ide_extensions_request_invalid",
    "ide_extensions_unavailable",
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
    Asks {
        project: ProjectId,
    },
    Approve {
        project: ProjectId,
        id: String,
        version: String,
    },
    Decline {
        project: ProjectId,
        id: String,
        version: String,
    },
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    ide_extensions: Action,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Reply {
    Asks { asks: Vec<AskView> },
    Approved { approved: Approved },
    Done,
    Rejected { code: String },
}

/// Names the requests that only read, for the log of replies this client could not read.
/// Input: the request. Output: its name, None for a request that changes something.
fn read_kind(action: &Action) -> Option<&'static str> {
    match action {
        Action::Asks { .. } => Some("ide_extensions.asks"),
        _ => None,
    }
}

fn unknown() -> EngineError {
    failure("ide_extensions_outcome_unknown")
}

impl OwnerClient {
    /// Reads the installs and updates a project's IDE asked the owner to confirm, oldest
    /// first. Input: the project. Output: each ask with the version approved now, if any.
    pub async fn ide_extension_asks(
        &self,
        project: ProjectId,
    ) -> Result<Vec<AskView>, EngineError> {
        match self
            .ide_extensions_exchange(Action::Asks { project })
            .await?
        {
            Reply::Asks { asks } => Ok(asks),
            _ => Err(unknown()),
        }
    }

    /// The owner's approval of one ask: the engine downloads the VSIX from Open VSX,
    /// checks its sha256 and pins that exact version. Inputs: the project, the id and
    /// the version or `latest` as asked. Output: the pin.
    pub async fn ide_extension_approve(
        &self,
        project: ProjectId,
        id: String,
        version: String,
    ) -> Result<Approved, EngineError> {
        let action = Action::Approve {
            project,
            id,
            version,
        };
        match self.ide_extensions_exchange(action).await? {
            Reply::Approved { approved } => Ok(approved),
            _ => Err(unknown()),
        }
    }

    /// The owner's Not now on one ask: it is dropped and nothing is approved. Inputs:
    /// the project, the id and the version as asked.
    pub async fn ide_extension_decline(
        &self,
        project: ProjectId,
        id: String,
        version: String,
    ) -> Result<(), EngineError> {
        let action = Action::Decline {
            project,
            id,
            version,
        };
        match self.ide_extensions_exchange(action).await? {
            Reply::Done => Ok(()),
            _ => Err(unknown()),
        }
    }

    async fn ide_extensions_exchange(&self, action: Action) -> Result<Reply, EngineError> {
        tokio::time::timeout(self.timeout.max(IDE_EXTENSIONS_TIMEOUT), async {
            let read = read_kind(&action);
            let frame = encode_frame(&Envelope {
                ide_extensions: action,
            })
            .map_err(|_| failure("ide_extensions_request_bounds"))?;
            self.root.verify()?;
            if self.root.socket_identity()? != self.socket {
                return Err(failure("owner_socket_replaced"));
            }
            let mut stream = crate::spawn_gate::connect_unix(self.root.path.join(SOCKET))
                .await
                .map_err(|_| failure("ide_extensions_owner_unavailable"))?;
            verify_peer(&stream)?;
            let mut packet = [0; 37];
            packet[..4].copy_from_slice(b"CAO1");
            packet[4] = match self.kind {
                OwnerClientKind::Tui => 0,
                OwnerClientKind::Desktop => 1,
            };
            packet[5..].copy_from_slice(&self.token);
            stream
                .write_all(&packet)
                .await
                .map_err(|_| failure("ide_extensions_owner_unavailable"))?;
            stream
                .write_all(&frame)
                .await
                .map_err(|_| failure("ide_extensions_owner_unavailable"))?;
            let frame = read_frame(&mut stream).await.map_err(|_| unknown())?;
            match serde_json::from_slice(&frame[4..])
                .map_err(|error| self.skewed_reply(&error, read, unknown))?
            {
                Reply::Rejected { code } => Err(EngineError(code)),
                reply => Ok(reply),
            }
        })
        .await
        .map_err(|_| unknown())?
    }
}

/// Encodes this protocol's refusal of a request the engine did not run.
/// Input: the refusal code. Output: the reply frame.
pub(super) fn refusal(code: &str) -> Result<Vec<u8>, EngineError> {
    encode_frame(&Reply::Rejected { code: code.into() }).map_err(|_| unknown())
}

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
            code: "ide_extensions_reply_bounds".into(),
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
        return Err(failure("ide_extensions_request_bounds"));
    }
    let Envelope {
        ide_extensions: action,
    } = serde_json::from_slice(bytes).map_err(|_| failure("ide_extensions_request_invalid"))?;
    {
        let _barrier = engine.owner_egress.write().await;
        if &authority.authenticate(packet)? != admission {
            return Err(failure("owner_grant_stale_or_revoked"));
        }
    }
    let root = super::changes::engine_root(authority)?;
    tokio::task::spawn_blocking(move || {
        let now = crate::onboarding::now_ms();
        Ok(match action {
            Action::Asks { project } => Reply::Asks {
                asks: policy::asks(&root, &project, now)?,
            },
            Action::Approve {
                project,
                id,
                version,
            } => Reply::Approved {
                approved: policy::approve(
                    &root,
                    &Registry::open_vsx(),
                    &project,
                    &id,
                    &version,
                    now,
                )?,
            },
            Action::Decline {
                project,
                id,
                version,
            } => {
                policy::decline(&root, &project, &id, &version)?;
                Reply::Done
            }
        })
    })
    .await
    .map_err(|_| failure("ide_extensions_unavailable"))?
}
