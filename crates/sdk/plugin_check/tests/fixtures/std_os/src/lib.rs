#![forbid(unsafe_code)]

use std::os::unix::net::UnixStream;

/// Opens a unix socket.
pub fn open() -> bool {
    UnixStream::connect("/tmp/x").is_ok()
}
