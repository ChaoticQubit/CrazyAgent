use std::io;
use std::path::Path;

pub(crate) use crate::platform::unix_socket::{
    OwnedReadHalf, OwnedWriteHalf, ReadHalf, UnixListener as Listener, UnixStream as Stream,
};

/// Binds the owner-class listener at `path`: a socket made inside the spawn gate, guarded by
/// its parent, the protected root. Must run inside a Tokio runtime.
/// Input: the endpoint path. Output: the listener.
#[cfg(unix)]
pub(crate) fn owner_listener(path: impl AsRef<Path>) -> io::Result<Listener> {
    crate::spawn_gate::bind_unix(path)
}

/// Accepts one owner-class connection, taking the spawn gate only for each accept attempt.
/// Input: the listener. Output: the stream, whose peer is not checked yet.
pub(crate) async fn owner_accept(listener: &Listener) -> io::Result<Stream> {
    crate::spawn_gate::accept_unix(listener)
        .await
        .map(|(stream, _)| stream)
}

/// Connects to the owner-class endpoint at `path`, whose server is not checked yet.
/// Input: the endpoint path. Output: the stream, or `unix_socket_unavailable` where this
/// platform has no owner-class transport yet.
pub(crate) async fn owner_connect(path: impl AsRef<Path>) -> io::Result<Stream> {
    crate::spawn_gate::connect_unix(path).await
}

/// The server side's check of an accepted stream: its client runs as this user.
/// Input: the stream. Output: Ok, `peer_owner_mismatch`, or the error reading the peer.
pub(crate) fn verify_client(stream: &Stream) -> io::Result<()> {
    same_user(stream)
}

/// The client side's check, right after connect and before any byte is written: the server
/// runs as this user. Input: the stream. Output: Ok, `peer_owner_mismatch`, or the error
/// reading the peer.
pub(crate) fn verify_server(stream: &Stream) -> io::Result<()> {
    same_user(stream)
}

/// The Unix peer check both sides share: the kernel's credentials of the peer name this
/// process's user. Input: the stream. Output: Ok, `peer_owner_mismatch`, or the error
/// reading the credentials.
#[cfg(unix)]
fn same_user(stream: &Stream) -> io::Result<()> {
    #[cfg(target_os = "macos")]
    let uid = nix::unistd::getpeereid(stream)
        .map_err(io::Error::other)?
        .0
        .as_raw();
    #[cfg(not(target_os = "macos"))]
    let uid = stream.peer_cred()?.uid();
    if uid != rustix::process::getuid().as_raw() {
        return Err(io::Error::other("peer_owner_mismatch"));
    }
    Ok(())
}

/// The peer check of a stream that cannot exist where there is no owner-class transport.
/// Input: that stream. Output: unreachable by construction.
#[cfg(not(unix))]
fn same_user(stream: &Stream) -> io::Result<()> {
    match *stream {}
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::FileTypeExt;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn an_owner_endpoint_serves_a_client_of_this_user_at_its_path() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("engine.sock");
        let listener = owner_listener(&path).unwrap();
        let kind = std::fs::symlink_metadata(&path).unwrap().file_type();
        assert!(kind.is_socket());
        let (client, served) = tokio::join!(owner_connect(&path), owner_accept(&listener));
        let (mut client, mut served) = (client.unwrap(), served.unwrap());
        verify_server(&client).unwrap();
        verify_client(&served).unwrap();
        client.write_all(b"ping").await.unwrap();
        let mut bytes = [0; 4];
        served.read_exact(&mut bytes).await.unwrap();
        assert_eq!(&bytes, b"ping");
    }
}

#[cfg(all(test, windows))]
mod windows_tests {
    #[tokio::test]
    async fn an_owner_connect_refuses_until_the_pipe_transport_lands() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("engine.sock");
        let refused = super::owner_connect(&path).await.unwrap_err();
        assert_eq!(refused.to_string(), "unix_socket_unavailable");
    }
}
