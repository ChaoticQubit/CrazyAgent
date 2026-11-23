use super::UpdateError;
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

/// A real HTTP server on this computer's loopback address that answers one request per
/// response, in order. Input: the raw responses. Output: the URL to fetch and the server
/// thread, which ends after the last response.
pub fn serve(responses: Vec<Vec<u8>>) -> (String, std::thread::JoinHandle<()>) {
    let listener = crate::spawn_gate::bind_tcp_std("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        for response in responses {
            let (mut stream, _) = crate::spawn_gate::accept_tcp_std(&listener).unwrap();
            read_request(&mut stream);
            let _ = stream.write_all(&response);
        }
    });
    (format!("http://{address}/release"), server)
}

/// One raw HTTP response. Inputs: the status line and headers ending in a blank line, and
/// the body. Output: the bytes to send.
pub fn response(head: &str, body: &[u8]) -> Vec<u8> {
    let mut bytes = head.as_bytes().to_vec();
    bytes.extend_from_slice(body);
    bytes
}
