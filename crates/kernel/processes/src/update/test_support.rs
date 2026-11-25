use super::{Unpacked, UpdateError, Verified};
use std::io::{Read, Write};
use std::net::TcpStream;

/// The stable code of a failed update step, for rows that compare codes.
/// Input: a result that must be a failure. Output: its code.
pub fn code<T: std::fmt::Debug>(result: Result<T, UpdateError>) -> &'static str {
    result.unwrap_err().code
}

/// The code and sentence of an update failure, for rows that compare both.
/// Input: the failure. Output: its code and message.
pub fn said(error: UpdateError) -> (&'static str, String) {
    (error.code, error.message)
}

/// Reads one HTTP request head from a client, up to its blank line or the stream end.
/// Input: the accepted stream. Output: none; the head is read and dropped.
pub fn read_request(stream: &mut TcpStream) {
    let mut request = Vec::new();
    let mut byte = [0_u8; 1];
    while !request.ends_with(b"\r\n\r\n") {
        if stream.read(&mut byte).unwrap_or(0) == 0 {
            return;
        }
        request.push(byte[0]);
    }
}

/// The result of a row or fixture whose setup can fail at a file, socket or thread.
pub type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

/// Names the path a filesystem step failed on. Inputs: the path and the step's result.
/// Output: the value, or an error naming the path.
pub fn at<T>(path: &std::path::Path, result: std::io::Result<T>) -> TestResult<T> {
    result.map_err(|error| format!("{path:?}: {error}").into())
}

/// Waits for a fixture thread and hands back what it returned, its own socket or file
/// error included. Input: the thread. Output: its value, or an error when it failed or
/// panicked.
pub fn joined<T>(thread: std::thread::JoinHandle<std::io::Result<T>>) -> TestResult<T> {
    Ok(thread.join().map_err(|_| "a fixture thread panicked")??)
}

/// A real HTTP server on this computer's loopback address that answers one request per
/// response, in order. Input: the raw responses. Output: the URL to fetch and the server
/// thread, which ends after the last response and returns the first accept error, or an
/// error when the loopback socket cannot be opened.
pub fn serve(
    responses: Vec<Vec<u8>>,
) -> TestResult<(String, std::thread::JoinHandle<std::io::Result<()>>)> {
    let listener = crate::spawn_gate::bind_tcp_std("127.0.0.1:0")?;
    let address = listener.local_addr()?;
    let server = std::thread::spawn(move || {
        for response in responses {
            let (mut stream, _) = crate::spawn_gate::accept_tcp_std(&listener)?;
            read_request(&mut stream);
            let _ = stream.write_all(&response);
        }
        Ok(())
    });
    Ok((format!("http://{address}/release"), server))
}

/// One raw HTTP response. Inputs: the status line and headers ending in a blank line, and
/// the body. Output: the bytes to send.
pub fn response(head: &str, body: &[u8]) -> Vec<u8> {
    let mut bytes = head.as_bytes().to_vec();
    bytes.extend_from_slice(body);
    bytes
}

/// An archive taken as verified without a signature, for rows of [`super::unpack`] that
/// need no release key. Input: the archive. Output: the token.
pub fn verified(archive: &std::path::Path) -> Verified {
    Verified {
        archive: archive.to_path_buf(),
    }
}

/// A folder taken as unpacked from a verified archive, for rows of [`super::swap`] that
/// stage their files by hand. Input: the folder. Output: the token.
pub fn unpacked(folder: &std::path::Path) -> Unpacked {
    Unpacked {
        folder: folder.to_path_buf(),
    }
}
