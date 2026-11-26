use super::{Authority, OwnerClient, OwnerRuntimeHandle};
use crate::engine::owner::native_auth::{self as auth, Command, Envelope};
use crate::engine::owner::{
    Admission, NativeAuthCancel, NativeAuthLogout, NativeAuthQuery, NativeAuthReply,
    NativeAuthStart, failure,
};
use crate::engine::{Engine, EngineError, io_error, read_frame};
use crate::ipc::Stream;
use crate::protocol::encode_frame;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::time::Instant;

impl OwnerClient {
    pub async fn start_native_auth(
        &self,
        request: NativeAuthStart,
    ) -> Result<NativeAuthReply, EngineError> {
        self.native_auth(Envelope {
            native_auth: Command::Start(request),
        })
        .await
    }

    pub async fn read_native_auth(
        &self,
        request: NativeAuthQuery,
    ) -> Result<NativeAuthReply, EngineError> {
        self.native_auth(Envelope {
            native_auth: Command::Read(request),
        })
        .await
    }

    pub async fn cancel_native_auth(
        &self,
        request: NativeAuthCancel,
    ) -> Result<NativeAuthReply, EngineError> {
        self.native_auth(Envelope {
            native_auth: Command::Cancel(request),
        })
        .await
    }

    pub async fn logout_native_auth(
        &self,
        request: NativeAuthLogout,
    ) -> Result<NativeAuthReply, EngineError> {
        self.native_auth(Envelope {
            native_auth: Command::Logout(request),
        })
        .await
    }

    async fn native_auth(&self, envelope: Envelope) -> Result<NativeAuthReply, EngineError> {
        envelope.native_auth.validate()?;
        let frame = encode_frame(&envelope).map_err(|_| failure("native_auth_request_bounds"))?;
        if frame.len() - 4 > auth::REQUEST_BYTES_MAX {
            return Err(failure("native_auth_request_bounds"));
        }
        tokio::time::timeout(self.timeout, async {
            let mut stream = self
                .connect_checked(|_| failure("native_auth_owner_unavailable"))
                .await?;
            let packet = crate::engine::secret_frame::owner_packet(
                b"CAO1",
                crate::engine::secret_frame::client_tag(self.kind),
                &self.token,
            );
            stream
                .write_all(&*packet)
                .await
                .map_err(|_| auth::outcome_unknown())?;
            stream
                .write_all(&frame)
                .await
                .map_err(|_| auth::outcome_unknown())?;
            let bytes = read_frame(&mut stream)
                .await
                .map_err(|_| auth::outcome_unknown())?;
            if self.root.socket_identity().ok() != Some(self.socket) {
                return Err(auth::outcome_unknown());
            }
            auth::decode_reply(&bytes[4..], &envelope.native_auth)
        })
        .await
        .map_err(|_| auth::outcome_unknown())?
    }
}

/// Encodes this protocol's refusal of a request the engine did not run, naming the
/// request it answers. Inputs: the request body and the refusal code. Output: the frame.
pub(super) fn refusal(bytes: &[u8], code: &str) -> Result<Vec<u8>, EngineError> {
    let envelope: Envelope =
        serde_json::from_slice(bytes).map_err(|_| failure("native_auth_request_invalid"))?;
    auth::encode_reply(
        Err(failure(code)),
        envelope.native_auth.request_id().clone(),
    )
}

/// The most time before the owner's exchange is cut that its native-auth answer leaves for
/// the reply after a pending answer: the barrier, the second check and the write. A short
/// exchange leaves an eighth of what remains instead, so its answer is never due at once.
const ANSWER_MARGIN: Duration = Duration::from_millis(250);

/// Serves one native-auth request of the owner, its answer due a margin before the exchange
/// is cut, so a pending answer is written, never cut. Inputs: the stream, engine,
/// authority, packet, admission, request bytes and the exchange deadline. Output: none once
/// the reply is written, else the refusal.
pub(super) async fn serve(
    stream: &mut Stream,
    engine: &Arc<Engine>,
    authority: &Arc<Authority>,
    packet: &[u8; 37],
    admission: &Admission,
    bytes: &[u8],
    deadline: Instant,
) -> Result<(), EngineError> {
    if bytes.len() > auth::REQUEST_BYTES_MAX {
        return Err(failure("native_auth_request_bounds"));
    }
    let envelope: Envelope =
        serde_json::from_slice(bytes).map_err(|_| failure("native_auth_request_invalid"))?;
    let command = envelope.native_auth;
    let request_id = command.request_id().clone();
    {
        let _barrier = engine.owner_egress.write().await;
        if &authority.authenticate(packet)? != admission {
            return Err(failure("owner_grant_stale_or_revoked"));
        }
    }
    let host = OwnerRuntimeHandle {
        engine: engine.clone(),
        authority: authority.clone(),
    };
    let margin = ANSWER_MARGIN.min(deadline.saturating_duration_since(Instant::now()) / 8);
    let answer_by = deadline.checked_sub(margin).unwrap_or(deadline);
    assert!(answer_by <= deadline);
    let ask = crate::storage::NativeAuthAsk::Owner { answer_by };
    let result = match host
        .native_auth_command(admission.clone(), command, ask)
        .await
    {
        Err(error) if error.0 == "native_auth_host_busy" => {
            Err(failure("native_auth_recovery_required"))
        }
        result => result,
    };
    let _barrier = engine.owner_egress.write().await;
    if &authority.authenticate(packet)? != admission {
        return Err(failure("owner_grant_stale_or_revoked"));
    }
    let frame = auth::encode_reply(result, request_id)?;
    stream.write_all(&frame).await.map_err(io_error)
}
