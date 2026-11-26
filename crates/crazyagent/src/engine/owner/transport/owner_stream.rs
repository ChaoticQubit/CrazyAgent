use super::*;

impl OwnerClient {
    /// Opens a connection to this client's owner endpoint, checking before the connect and
    /// again after it that the endpoint is still the one the client admitted. Input: what a
    /// failed connect becomes. Output: the checked stream, `owner_socket_replaced`, or the
    /// failed connect.
    pub(super) async fn connect_checked(
        &self,
        unavailable: impl FnOnce(std::io::Error) -> EngineError,
    ) -> Result<Stream, EngineError> {
        if self.root.socket_identity()? != self.socket {
            return Err(failure("owner_socket_replaced"));
        }
        let stream = crate::ipc::owner_connect(self.root.path.join(SOCKET))
            .await
            .map_err(unavailable)?;
        self.checked(stream)
    }

    /// The check after the connect: the server runs as this user and the endpoint is still
    /// the one the client admitted, so no capability is written to a replaced endpoint.
    /// Input: the connected stream. Output: the stream, or the refusal.
    pub(super) fn checked(&self, stream: Stream) -> Result<Stream, EngineError> {
        verify_server(&stream)?;
        if self.root.socket_identity()? != self.socket {
            return Err(failure("owner_socket_replaced"));
        }
        Ok(stream)
    }
}
