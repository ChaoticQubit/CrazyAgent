#[cfg(unix)]
pub(crate) use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf, ReadHalf};
#[cfg(unix)]
pub(crate) use tokio::net::{UnixListener, UnixStream};

#[cfg(not(unix))]
pub(crate) use unavailable::{OwnedReadHalf, OwnedWriteHalf, ReadHalf, UnixListener, UnixStream};

#[cfg(not(unix))]
mod unavailable {
    use std::io;
    use std::path::Path;
    use std::pin::Pin;
    use std::task::{Context, Poll};
    use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

    /// The error every Unix domain socket entry point returns where the platform has none.
    /// Takes nothing. Returns an `Unsupported` I/O error reading `unix_socket_unavailable`.
    fn unavailable() -> io::Error {
        io::Error::new(io::ErrorKind::Unsupported, "unix_socket_unavailable")
    }

    /// A Unix domain socket stream, which cannot exist where the platform has none.
    /// `connect` reports `unix_socket_unavailable`, so every method below is unreachable
    /// by construction until the Windows named-pipe transport replaces it.
    #[derive(Debug)]
    pub(crate) enum UnixStream {}

    /// A Unix domain socket listener, which cannot exist where the platform has none.
    /// `bind` reports `unix_socket_unavailable`, so `accept` is unreachable by construction.
    #[derive(Debug)]
    pub(crate) enum UnixListener {}

    /// The address of an accepted peer; no value exists because no listener can.
    #[derive(Debug)]
    pub(crate) enum SocketAddr {}

    /// The owned read half of a stream that cannot exist here.
    #[derive(Debug)]
    pub(crate) enum OwnedReadHalf {}

    /// The owned write half of a stream that cannot exist here.
    #[derive(Debug)]
    pub(crate) enum OwnedWriteHalf {}

    /// The borrowed read half of a stream that cannot exist here.
    #[derive(Debug)]
    pub(crate) struct ReadHalf<'a>(&'a UnixStream);

    /// The borrowed write half of a stream that cannot exist here.
    #[derive(Debug)]
    pub(crate) struct WriteHalf<'a>(&'a UnixStream);

    impl UnixStream {
        pub(crate) async fn connect(_path: impl AsRef<Path>) -> io::Result<Self> {
            Err(unavailable())
        }

        #[cfg(unix)]
        pub(crate) fn into_split(self) -> (OwnedReadHalf, OwnedWriteHalf) {
            match self {}
        }

        pub(crate) fn split(&mut self) -> (ReadHalf<'_>, WriteHalf<'_>) {
            match *self {}
        }
    }

    impl UnixListener {
        #[cfg(unix)]
        pub(crate) fn bind(_path: impl AsRef<Path>) -> io::Result<Self> {
            Err(unavailable())
        }

        pub(crate) async fn accept(&self) -> io::Result<(UnixStream, SocketAddr)> {
            match *self {}
        }
    }

    impl AsyncRead for UnixStream {
        fn poll_read(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            _buf: &mut ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            match *self {}
        }
    }

    impl AsyncWrite for UnixStream {
        fn poll_write(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            _buf: &[u8],
        ) -> Poll<io::Result<usize>> {
            match *self {}
        }

        fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            match *self {}
        }

        fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            match *self {}
        }
    }

    impl AsyncRead for OwnedReadHalf {
        fn poll_read(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            _buf: &mut ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            match *self {}
        }
    }

    impl AsyncWrite for OwnedWriteHalf {
        fn poll_write(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            _buf: &[u8],
        ) -> Poll<io::Result<usize>> {
            match *self {}
        }

        fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            match *self {}
        }

        fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            match *self {}
        }
    }

    impl AsyncRead for ReadHalf<'_> {
        fn poll_read(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            _buf: &mut ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            match *self.0 {}
        }
    }

    impl AsyncWrite for WriteHalf<'_> {
        fn poll_write(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            _buf: &[u8],
        ) -> Poll<io::Result<usize>> {
            match *self.0 {}
        }

        fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            match *self.0 {}
        }

        fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            match *self.0 {}
        }
    }
}
