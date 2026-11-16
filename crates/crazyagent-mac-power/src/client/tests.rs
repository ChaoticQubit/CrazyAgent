use super::{CallError, Endpoint, PRODUCTION, call};
use crate::protocol::{LidDisplay, REPLY_BYTES_MAX, Reply, Request, encode_request, write_reply};
use crate::server;
use std::cell::Cell;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::fs::MetadataExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const WAIT: Duration = Duration::from_millis(500);
const LATE_MAX: Duration = Duration::from_millis(1_500);
const STAND_IN_READ_MAX: u64 = 4096;
const HOLD: Request = Request::Hold {
    lease_s: 120,
    lid_display: LidDisplay::Sleep,
};

/// A temp folder with the path of a stand-in helper socket in it, and our own uid.
struct Place {
    _temp: tempfile::TempDir,
    socket: PathBuf,
    my_uid: u32,
}

fn place() -> Place {
    let temp = tempfile::tempdir().unwrap();
    let my_uid = std::fs::metadata(temp.path()).unwrap().uid();
    Place {
        socket: temp.path().join("helper.sock"),
        my_uid,
        _temp: temp,
    }
}

fn endpoint(socket: &Path, peer_uid: u32) -> Endpoint<'_> {
    Endpoint::stand_in(socket.to_str().unwrap(), peer_uid, WAIT)
}

/// A gate that only runs the connect, as the engine's spawn gate does with no child starting.
fn open_gate(connect: &mut dyn FnMut() -> io::Result<UnixStream>) -> io::Result<UnixStream> {
    connect()
}

/// One stand-in helper on the socket: accepts one connection, reads what arrives within `WAIT`,
/// answers `answer` after `pause` when a full line came, and hands back what it read. Inputs:
/// the socket path, the answer bytes and the pause. Output: the thread.
fn stand_in(socket: &Path, answer: Vec<u8>, pause: Duration) -> JoinHandle<Vec<u8>> {
    let listener = UnixListener::bind(socket).unwrap();
    thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        stream.set_read_timeout(Some(WAIT)).unwrap();
        let mut line = Vec::new();
        let _ = BufReader::new(&stream)
            .take(STAND_IN_READ_MAX)
            .read_until(b'\n', &mut line);
        if line.ends_with(b"\n") {
            thread::sleep(pause);
            let _ = (&stream).write_all(&answer);
        }
        line
    })
}

fn reply_line(reply: &Reply) -> Vec<u8> {
    let mut line = Vec::new();
    write_reply(&mut line, reply).unwrap();
    line
}

#[test]
fn a_call_sends_one_request_line_and_reads_one_reply() {
    let place = place();
    let held = Reply::Held { lease_left_s: 120 };
    let served = stand_in(&place.socket, reply_line(&held), Duration::ZERO);
    let reply = call(&endpoint(&place.socket, place.my_uid), &HOLD, &open_gate);
    assert_eq!(reply, Ok(held));
    assert_eq!(served.join().unwrap(), encode_request(&HOLD));
}

#[test]
fn a_helper_socket_not_owned_by_root_is_refused() {
    let place = place();
    let served = stand_in(&place.socket, reply_line(&Reply::Released), Duration::ZERO);
    let reply = call(
        &endpoint(&place.socket, PRODUCTION.peer_uid),
        &Request::Release,
        &open_gate,
    );
    assert_eq!(PRODUCTION.peer_uid, 0);
    assert_eq!(reply, Err(CallError::PeerUid(place.my_uid)));
    assert!(served.join().unwrap().is_empty(), "nothing is written to a peer that is not root");
}

#[test]
fn the_helper_socket_is_made_only_inside_the_gate() {
    let place = place();
    let listener = UnixListener::bind(&place.socket).unwrap();
    listener.set_nonblocking(true).unwrap();
    let shut = |_: &mut dyn FnMut() -> io::Result<UnixStream>| -> io::Result<UnixStream> {
        Err(io::Error::from(io::ErrorKind::PermissionDenied))
    };
    let reply = call(&endpoint(&place.socket, place.my_uid), &Request::Read, &shut);
    assert_eq!(reply, Err(CallError::Connect(io::ErrorKind::PermissionDenied)));
    let accepted = listener.accept().map(|_| ()).map_err(|error| error.kind());
    assert_eq!(accepted, Err(io::ErrorKind::WouldBlock), "no connection outside the gate");
    drop(listener);
    std::fs::remove_file(&place.socket).unwrap();
    let calls = Cell::new(0_u32);
    let counted = |connect: &mut dyn FnMut() -> io::Result<UnixStream>| -> io::Result<UnixStream> {
        calls.set(calls.get() + 1);
        connect()
    };
    let served = stand_in(&place.socket, reply_line(&Reply::Released), Duration::ZERO);
    let reply = call(&endpoint(&place.socket, place.my_uid), &Request::Release, &counted);
    assert_eq!(reply, Ok(Reply::Released));
    assert_eq!(calls.get(), 1, "one connect, through the gate");
    served.join().unwrap();
}

#[test]
fn a_reply_over_the_bound_or_without_its_newline_is_refused() {
    let place = place();
    let long = vec![b'a'; REPLY_BYTES_MAX + 1];
    let served = stand_in(&place.socket, long, Duration::ZERO);
    let reply = call(&endpoint(&place.socket, place.my_uid), &Request::Read, &open_gate);
    assert_eq!(reply, Err(CallError::Reply));
    served.join().unwrap();
    std::fs::remove_file(&place.socket).unwrap();
    let mut cut = reply_line(&Reply::Released);
    assert_eq!(cut.pop(), Some(b'\n'));
    let served = stand_in(&place.socket, cut, Duration::ZERO);
    let reply = call(&endpoint(&place.socket, place.my_uid), &Request::Read, &open_gate);
    assert_eq!(reply, Err(CallError::Reply), "a reply closed before its newline");
    served.join().unwrap();
}

#[test]
fn a_helper_that_never_answers_is_cut_off_at_the_endpoint_wait() {
    let place = place();
    let served = stand_in(&place.socket, reply_line(&Reply::Released), WAIT * 3);
    let started = Instant::now();
    let reply = call(&endpoint(&place.socket, place.my_uid), &Request::Read, &open_gate);
    let took = started.elapsed();
    assert!(matches!(reply, Err(CallError::Exchange(_))), "{reply:?}");
    assert!(took >= WAIT, "{took:?}");
    assert!(took < LATE_MAX, "one deadline over the whole call: {took:?}");
    served.join().unwrap();
}

#[test]
fn the_production_endpoint_is_launchds_socket_run_as_root() {
    assert_eq!(PRODUCTION.socket, format!("/private{}", server::SOCKET));
    assert_eq!(PRODUCTION.peer_uid, 0);
    assert_eq!(PRODUCTION.wait, Duration::from_secs(12));
}
