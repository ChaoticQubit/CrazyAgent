#![no_std]
#![forbid(unsafe_code)]

use std::net::TcpStream;

/// Opens a socket.
pub fn open() -> bool {
    TcpStream::connect("127.0.0.1:1").is_ok()
}
