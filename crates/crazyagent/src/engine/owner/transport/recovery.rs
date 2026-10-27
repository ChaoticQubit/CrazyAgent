use super::*;
use crate::ledger::{self, ActionKind};
use crate::protocol::encode_frame;
use serde::{Deserialize, Serialize};

const REQUEST_BYTES_MAX: usize = 1024;
pub(super) const RECOVERY_TIMEOUT: Duration = Duration::from_secs(30);
const REPLY_WRITE_TIME: Duration = Duration::from_secs(2);

/// Every code a Recovery request can answer an owner client with: the ledger's refusals,
/// this protocol's own, and the owner connection's it travels on.
pub const RECOVERY_CODES: &[&str] = &[
    "ledger_finished",
    "ledger_missing",
    "ledger_not_notice",
    "ledger_unavailable",
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
    "recovery_outcome_unknown",
    "recovery_reply_bounds",
    "recovery_request_bounds",
    "recovery_request_invalid",
    "recovery_unavailable",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Action {
    Dismiss { action_kind: ActionKind, id: i64 },
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    recovery: Action,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Reply {
    Dismissed,
    Rejected { code: String },
}

fn unknown() -> EngineError {
    failure("recovery_outcome_unknown")
}

impl OwnerClient {
    /// Dismisses one recovery notice the owner was shown: the engine closes it with the
    /// outcome it showed, and the agent start it held back may run again. A pending action
    /// a check still settles is refused with `ledger_not_notice`. Inputs: the notice's
    /// kind and number from the ledger. Output: Ok once it is dismissed.
    pub async fn recovery_dismiss(&self, kind: ActionKind, id: i64) -> Result<(), EngineError> {
        let action = Action::Dismiss {
            action_kind: kind,
            id,
        };
        match self.recovery_exchange(action).await? {
            Reply::Dismissed => Ok(()),
            Reply::Rejected { .. } => Err(unknown()),
        }
    }

    async fn recovery_exchange(&self, action: Action) -> Result<Reply, EngineError> {
        tokio::time::timeout(self.timeout.max(RECOVERY_TIMEOUT), async {
            let frame = encode_frame(&Envelope { recovery: action })
                .map_err(|_| failure("recovery_request_bounds"))?;
            self.root.verify()?;
            if self.root.socket_identity()? != self.socket {
                return Err(failure("owner_socket_replaced"));
            }
            let mut stream = crate::spawn_gate::connect_unix(self.root.path.join(SOCKET))
                .await
                .map_err(|_| failure("recovery_unavailable"))?;
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
                .map_err(|_| failure("recovery_unavailable"))?;
            stream
                .write_all(&frame)
                .await
                .map_err(|_| failure("recovery_unavailable"))?;
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

/// Encodes this protocol's refusal of a request the engine did not run.
/// Input: the refusal code. Output: the reply frame.
pub(super) fn refusal(code: &str) -> Result<Vec<u8>, EngineError> {
    encode_frame(&Reply::Rejected { code: code.into() }).map_err(|_| unknown())
}

/// Serves one Recovery request. Under the owner barrier the grant is checked again, the
/// notice is dismissed and the reply is written within REPLY_WRITE_TIME, so a revocation
/// that returned saw every dismissal admitted before it and lets none through after, and a
/// client that stops reading holds the barrier no longer than that. Inputs: the connection,
/// engine, authority, packet, admission and body. Output: done once the reply is written.
pub(super) async fn serve(
    stream: &mut UnixStream,
    engine: &Arc<Engine>,
    authority: &Arc<Authority>,
    packet: &[u8; 37],
    admission: &Admission,
    bytes: &[u8],
) -> Result<(), EngineError> {
    let action = parse(bytes);
    let _barrier = engine.owner_egress.write().await;
    let reply = match handle(authority, packet, admission, action).await {
        Ok(reply) => reply,
        Err(error) => Reply::Rejected { code: error.0 },
    };
    let frame = match encode_frame(&reply) {
        Ok(frame) => frame,
        Err(_) => encode_frame(&Reply::Rejected {
            code: "recovery_reply_bounds".into(),
        })
        .map_err(|_| unknown())?,
    };
    tokio::time::timeout(REPLY_WRITE_TIME, stream.write_all(&frame))
        .await
        .map_err(|_| unknown())?
        .map_err(io_error)
}

/// Answers a parsed request under the owner barrier its caller holds: checks the grant
/// again, then dismisses. Inputs: the authority, packet, admission and the parsed request.
/// Output: the reply, or the refusal.
async fn handle(
    authority: &Authority,
    packet: &[u8; 37],
    admission: &Admission,
    action: Result<Action, EngineError>,
) -> Result<Reply, EngineError> {
    let action = action?;
    if &authority.authenticate(packet)? != admission {
        return Err(failure("owner_grant_stale_or_revoked"));
    }
    let root = super::changes::engine_root(authority)?;
    tokio::task::spawn_blocking(move || run(&root, action))
        .await
        .map_err(|_| failure("recovery_unavailable"))?
}

/// Reads one Recovery request from an owner frame body, at most REQUEST_BYTES_MAX bytes.
/// Input: the body. Output: the action, or `recovery_request_bounds` or
/// `recovery_request_invalid`.
fn parse(bytes: &[u8]) -> Result<Action, EngineError> {
    if bytes.len() > REQUEST_BYTES_MAX {
        return Err(failure("recovery_request_bounds"));
    }
    let Envelope { recovery } =
        serde_json::from_slice(bytes).map_err(|_| failure("recovery_request_invalid"))?;
    Ok(recovery)
}

/// Runs one authenticated Recovery action against the action ledger in the engine root.
/// Inputs: the engine root and the action. Output: the reply, or the ledger's refusal.
fn run(root: &Path, action: Action) -> Result<Reply, EngineError> {
    let db = ledger::open(root)?;
    match action {
        Action::Dismiss { action_kind, id } => {
            ledger::dismiss(&db, action_kind, id)?;
            Ok(Reply::Dismissed)
        }
    }
}

#[cfg(test)]
#[path = "recovery_tests.rs"]
mod tests;
