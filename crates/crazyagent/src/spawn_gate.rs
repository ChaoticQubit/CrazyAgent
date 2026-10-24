use std::cell::Cell;
use std::io;
use std::process::{Child, Command, ExitStatus, Output};
use std::sync::{Mutex, MutexGuard, PoisonError};

static GATE: Mutex<()> = Mutex::new(());

thread_local! {
    static HELD: Cell<bool> = const { Cell::new(false) };
}

struct Entered(#[allow(dead_code)] MutexGuard<'static, ()>);

impl Drop for Entered {
    fn drop(&mut self) {
        HELD.set(false);
    }
}

fn enter() -> Entered {
    assert!(
        !HELD.get(),
        "spawn_gate entered again on a thread that already holds it"
    );
    let guard = GATE.lock().unwrap_or_else(PoisonError::into_inner);
    HELD.set(true);
    Entered(guard)
}

/// Runs a call that creates file descriptors while no other thread of this
/// process starts a child, for sockets and anything else the platform cannot
/// create close-on-exec in one step. Input: the call, which must not enter the
/// gate again. Output: what the call returned.
pub fn guarded<T>(create: impl FnOnce() -> T) -> T {
    let _gate = enter();
    create()
}

/// Starts a child process while no other thread of this process starts one or
/// creates a socket, so the child inherits only its own pipes.
/// Input: the configured command. Output: the child, or the error that kept it
/// from starting.
#[allow(clippy::disallowed_methods)]
pub fn spawn(command: &mut Command) -> io::Result<Child> {
    guarded(|| command.spawn())
}

/// Starts a child through the gate and waits for it outside the gate.
/// Input: the configured command; its stdio is inherited unless set, as with
/// `Command::status`. Output: the exit status.
pub fn status(command: &mut Command) -> io::Result<ExitStatus> {
    spawn(command)?.wait()
}

/// Starts a child through the gate and collects its output outside the gate.
/// Input: the configured command with stdin, stdout and stderr set; unlike
/// `Command::output` nothing is defaulted. Output: status, stdout and stderr.
pub fn output(command: &mut Command) -> io::Result<Output> {
    spawn(command)?.wait_with_output()
}

/// Accepts one connection on a Tokio Unix listener, taking the gate only for
/// each accept attempt, never while waiting.
/// Input: the listener. Output: the stream and its peer address.
#[cfg(unix)]
#[allow(clippy::disallowed_methods)]
pub async fn accept_unix(
    listener: &tokio::net::UnixListener,
) -> io::Result<(tokio::net::UnixStream, tokio::net::unix::SocketAddr)> {
    std::future::poll_fn(|context| guarded(|| listener.poll_accept(context))).await
}

/// Accepts on the platform's stand-in listener where there are no Unix sockets;
/// no such listener can exist, so nothing is created.
/// Input: the listener. Output: unreachable by construction.
#[cfg(not(unix))]
pub(crate) async fn accept_unix(
    listener: &crate::platform::unix_socket::UnixListener,
) -> io::Result<(
    crate::platform::unix_socket::UnixStream,
    crate::platform::unix_socket::SocketAddr,
)> {
    listener.accept().await
}

/// Accepts one connection on a Tokio TCP listener, taking the gate only for
/// each accept attempt, never while waiting.
/// Input: the listener. Output: the stream and its peer address.
#[allow(clippy::disallowed_methods)]
pub async fn accept_tcp(
    listener: &tokio::net::TcpListener,
) -> io::Result<(tokio::net::TcpStream, std::net::SocketAddr)> {
    std::future::poll_fn(|context| guarded(|| listener.poll_accept(context))).await
}

/// Connects a Tokio Unix stream; only the socket is created inside the gate.
/// Input: the socket path. Output: the connected stream.
#[cfg(unix)]
#[allow(clippy::disallowed_methods)]
pub async fn connect_unix(path: impl AsRef<std::path::Path>) -> io::Result<tokio::net::UnixStream> {
    guarded(tokio::net::UnixSocket::new_stream)?
        .connect(path)
        .await
}

/// Reports the platform's `unix_socket_unavailable` where there are no Unix
/// sockets; nothing is created. Input: the socket path. Output: that error.
#[cfg(not(unix))]
pub(crate) async fn connect_unix(
    path: impl AsRef<std::path::Path>,
) -> io::Result<crate::platform::unix_socket::UnixStream> {
    crate::platform::unix_socket::UnixStream::connect(path).await
}

/// Connects a Tokio TCP stream to the first address of `host:port` that
/// accepts; only each socket is created inside the gate.
/// Input: the address text. Output: the connected stream or the last error.
#[allow(clippy::disallowed_methods)]
pub async fn connect_tcp(
    address: impl tokio::net::ToSocketAddrs,
) -> io::Result<tokio::net::TcpStream> {
    let mut last = io::Error::new(io::ErrorKind::InvalidInput, "no address to connect to");
    for target in tokio::net::lookup_host(address).await? {
        let socket = guarded(|| match target {
            std::net::SocketAddr::V4(_) => tokio::net::TcpSocket::new_v4(),
            std::net::SocketAddr::V6(_) => tokio::net::TcpSocket::new_v6(),
        })?;
        match socket.connect(target).await {
            Ok(stream) => return Ok(stream),
            Err(error) => last = error,
        }
    }
    Err(last)
}

/// Binds a Tokio TCP listener to the first address of `address` that binds;
/// each socket is created inside the gate. Must run inside a Tokio runtime.
/// Input: the address. Output: the listener or the last bind error.
#[allow(clippy::disallowed_methods)]
pub async fn bind_tcp(
    address: impl tokio::net::ToSocketAddrs,
) -> io::Result<tokio::net::TcpListener> {
    let mut last = io::Error::new(io::ErrorKind::InvalidInput, "no address to bind");
    for target in tokio::net::lookup_host(address).await? {
        match guarded(|| std::net::TcpListener::bind(target)) {
            Ok(listener) => {
                listener.set_nonblocking(true)?;
                return tokio::net::TcpListener::from_std(listener);
            }
            Err(error) => last = error,
        }
    }
    Err(last)
}

/// Binds a Tokio Unix listener at `path` inside the gate. Must run inside a
/// Tokio runtime. Input: the socket path. Output: the listener.
#[cfg(unix)]
#[allow(clippy::disallowed_methods)]
pub fn bind_unix(path: impl AsRef<std::path::Path>) -> io::Result<tokio::net::UnixListener> {
    guarded(|| tokio::net::UnixListener::bind(path))
}

/// Connects a blocking Unix stream inside the gate; a Unix connect never waits
/// on the network. Input: the socket path. Output: the stream.
#[cfg(unix)]
#[allow(clippy::disallowed_methods)]
pub fn connect_unix_std(
    path: impl AsRef<std::path::Path>,
) -> io::Result<std::os::unix::net::UnixStream> {
    guarded(|| std::os::unix::net::UnixStream::connect(path))
}

/// Binds a blocking Unix listener inside the gate.
/// Input: the socket path. Output: the listener.
#[cfg(unix)]
#[allow(clippy::disallowed_methods)]
pub fn bind_unix_std(
    path: impl AsRef<std::path::Path>,
) -> io::Result<std::os::unix::net::UnixListener> {
    guarded(|| std::os::unix::net::UnixListener::bind(path))
}

/// Binds a Unix datagram socket inside the gate.
/// Input: the socket path. Output: the socket.
#[cfg(unix)]
#[allow(clippy::disallowed_methods)]
pub fn bind_unix_datagram_std(
    path: impl AsRef<std::path::Path>,
) -> io::Result<std::os::unix::net::UnixDatagram> {
    guarded(|| std::os::unix::net::UnixDatagram::bind(path))
}

/// Creates a connected pair of blocking Unix streams inside the gate.
/// Input: none. Output: both ends.
#[cfg(unix)]
#[allow(clippy::disallowed_methods)]
pub fn unix_pair_std() -> io::Result<(
    std::os::unix::net::UnixStream,
    std::os::unix::net::UnixStream,
)> {
    guarded(std::os::unix::net::UnixStream::pair)
}

/// Binds a blocking TCP listener inside the gate.
/// Input: the address. Output: the listener.
#[allow(clippy::disallowed_methods)]
pub fn bind_tcp_std(address: impl std::net::ToSocketAddrs) -> io::Result<std::net::TcpListener> {
    guarded(|| std::net::TcpListener::bind(address))
}

/// Connects a blocking TCP stream inside the gate, so the gate is held for the
/// whole connect; use it for loopback peers only.
/// Input: the address. Output: the stream.
#[allow(clippy::disallowed_methods)]
pub fn connect_tcp_std(address: impl std::net::ToSocketAddrs) -> io::Result<std::net::TcpStream> {
    guarded(|| std::net::TcpStream::connect(address))
}

/// Connects a blocking TCP stream within `timeout` inside the gate, holding it
/// for at most that long; use it for loopback peers only.
/// Input: the address and timeout. Output: the stream.
#[allow(clippy::disallowed_methods)]
pub fn connect_tcp_std_timeout(
    address: &std::net::SocketAddr,
    timeout: std::time::Duration,
) -> io::Result<std::net::TcpStream> {
    guarded(|| std::net::TcpStream::connect_timeout(address, timeout))
}

/// Accepts one connection on a blocking or non-blocking TCP listener. A blocking
/// listener is polled without the gate every few milliseconds and keeps its mode;
/// the accepted stream is blocking. Input: the listener. Output: the stream and peer.
#[cfg(unix)]
#[allow(clippy::disallowed_methods)]
pub fn accept_tcp_std(
    listener: &std::net::TcpListener,
) -> io::Result<(std::net::TcpStream, std::net::SocketAddr)> {
    let waits = !nonblocking(listener)?;
    if waits {
        listener.set_nonblocking(true)?;
    }
    let accepted = loop {
        match guarded(|| listener.accept()) {
            Err(error) if waits && error.kind() == io::ErrorKind::WouldBlock => {
                std::thread::sleep(ACCEPT_POLL);
            }
            other => break other,
        }
    };
    if waits {
        listener.set_nonblocking(false)?;
    }
    let (stream, peer) = accepted?;
    stream.set_nonblocking(false)?;
    Ok((stream, peer))
}

/// Accepts one connection where child processes inherit no socket by default.
/// Input: the listener. Output: the stream and peer.
#[cfg(not(unix))]
#[allow(clippy::disallowed_methods)]
pub fn accept_tcp_std(
    listener: &std::net::TcpListener,
) -> io::Result<(std::net::TcpStream, std::net::SocketAddr)> {
    listener.accept()
}

/// Accepts connections forever, as `TcpListener::incoming` does, through
/// [`accept_tcp_std`]. Input: the listener. Output: an endless iterator of streams.
pub fn incoming_tcp_std(
    listener: &std::net::TcpListener,
) -> impl Iterator<Item = io::Result<std::net::TcpStream>> + '_ {
    std::iter::repeat_with(move || accept_tcp_std(listener).map(|(stream, _)| stream))
}

/// Creates an unconnected Unix stream socket inside the gate, for checks that
/// need a socket with no peer. Input: none. Output: the socket as a stream.
#[cfg(all(test, target_os = "macos"))]
#[allow(clippy::disallowed_methods)]
pub(crate) fn unconnected_unix_stream() -> io::Result<std::os::unix::net::UnixStream> {
    use nix::sys::socket::{AddressFamily, SockFlag, SockType, socket};
    guarded(|| {
        socket(
            AddressFamily::Unix,
            SockType::Stream,
            SockFlag::empty(),
            None,
        )
    })
    .map(std::os::unix::net::UnixStream::from)
    .map_err(io::Error::from)
}

#[cfg(unix)]
const ACCEPT_POLL: std::time::Duration = std::time::Duration::from_millis(5);

#[cfg(unix)]
fn nonblocking(listener: &std::net::TcpListener) -> io::Result<bool> {
    let flags = rustix::fs::fcntl_getfl(listener)?;
    Ok(flags.contains(rustix::fs::OFlags::NONBLOCK))
}

#[cfg(test)]
mod tests;
