use super::{Authority, OwnerClient, SOCKET};
use crate::engine::owner::native_auth::{self as auth, Command, Envelope};
use crate::engine::owner::{
    Admission, NativeAuthCancel, NativeAuthLogout, NativeAuthQuery, NativeAuthReply,
    NativeAuthStart, OwnerClientKind, failure,
};
use crate::engine::{Engine, EngineError, io_error, read_frame, verify_peer};
use crate::protocol::encode_frame;
use crate::storage::Job;
use tokio::io::AsyncWriteExt;
use tokio::net::UnixStream;

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
            if self.root.socket_identity()? != self.socket {
                return Err(failure("owner_socket_replaced"));
            }
            let mut stream = UnixStream::connect(self.root.path.join(SOCKET))
                .await
                .map_err(|_| failure("native_auth_owner_unavailable"))?;
            verify_peer(&stream)?;
            if self.root.socket_identity()? != self.socket {
                return Err(failure("owner_socket_replaced"));
            }
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

pub(super) async fn serve(
    stream: &mut UnixStream,
    engine: &Engine,
    authority: &Authority,
    packet: &[u8; 37],
    admission: &Admission,
    bytes: &[u8],
) -> Result<(), EngineError> {
    if bytes.len() > auth::REQUEST_BYTES_MAX {
        return Err(failure("native_auth_request_bounds"));
    }
    let envelope: Envelope =
        serde_json::from_slice(bytes).map_err(|_| failure("native_auth_request_invalid"))?;
    let command = envelope.native_auth;
    let request_id = command.request_id().clone();
    let _barrier = engine.owner_egress.write().await;
    if &authority.authenticate(packet)? != admission {
        return Err(failure("owner_grant_stale_or_revoked"));
    }
    let result = match command.validate() {
        Ok(()) => {
            engine
                .owner_job(|reply| Job::NativeAuth(admission.clone(), command, reply))
                .await
        }
        Err(error) => Err(error),
    };
    if &authority.authenticate(packet)? != admission {
        return Err(failure("owner_grant_stale_or_revoked"));
    }
    let frame = auth::encode_reply(result, request_id)?;
    stream.write_all(&frame).await.map_err(io_error)
}
