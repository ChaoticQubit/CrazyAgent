//! The display part of a Linux Keep awake hold (DEC-167 addendum 12): an inhibit on the
//! session's screensaver, GNOME's session manager first, else the freedesktop screensaver.
//! Both drop an inhibit when the caller's bus connection closes, so the connection stays open
//! for the whole hold. It runs over a socket that tells zbus descriptors cannot pass, so the
//! bus never routes one to the engine, where it could leak into an agent started meanwhile.

use super::hold::{HoldError, REASON};
use async_io::Async;
use std::io;
use std::os::fd::OwnedFd;
use std::os::linux::net::SocketAddrExt;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::{SocketAddr, UnixStream};
use std::sync::Arc;
use std::time::Duration;
use zbus::address::transport::{Transport, UnixSocket};
use zbus::connection::socket::{ReadHalf, Split, WriteHalf};

const SESSION_WAIT: Duration = Duration::from_secs(2);
const APPLICATION: &str = "CrazyAgent";
const GNOME_TOPLEVEL: u32 = 0;
const GNOME_IDLE: u32 = 8;

/// One session service that keeps the screen from blanking: its bus name, object and
/// interface, and the name of the call that gives an inhibit back.
#[derive(Debug, PartialEq, Eq)]
struct Inhibitor {
    service: &'static str,
    object: &'static str,
    interface: &'static str,
    give_back: &'static str,
}

const GNOME: Inhibitor = Inhibitor {
    service: "org.gnome.SessionManager",
    object: "/org/gnome/SessionManager",
    interface: "org.gnome.SessionManager",
    give_back: "Uninhibit",
};

const FREEDESKTOP: Inhibitor = Inhibitor {
    service: "org.freedesktop.ScreenSaver",
    object: "/org/freedesktop/ScreenSaver",
    interface: "org.freedesktop.ScreenSaver",
    give_back: "UnInhibit",
};

/// The read half of the session socket: zbus's own, except that it says descriptors cannot
/// pass, so the handshake never asks the bus for them, and a message that still carries one
/// is refused.
#[derive(Debug)]
struct NoDescriptors(Arc<Async<UnixStream>>);

#[async_trait::async_trait]
impl ReadHalf for NoDescriptors {
    async fn recvmsg(&mut self, buf: &mut [u8]) -> io::Result<(usize, Vec<OwnedFd>)> {
        let (read, descriptors) = ReadHalf::recvmsg(&mut self.0, buf).await?;
        if !descriptors.is_empty() {
            return Err(io::Error::other("the session bus sent a descriptor"));
        }
        assert!(read <= buf.len());
        Ok((read, descriptors))
    }

    fn can_pass_unix_fd(&self) -> bool {
        false
    }
}

/// Connects to the session bus the environment names, read the way zbus reads it. Output:
/// the connected socket, close-on-exec as every std socket is, or `Unavailable`.
fn session_socket() -> Result<UnixStream, HoldError> {
    let address = zbus::Address::session()
        .map_err(|problem| HoldError::unavailable("session bus", problem))?;
    let Transport::Unix(unix) = address.transport() else {
        return Err(HoldError::unavailable("session bus", "not a local socket"));
    };
    let connected = match unix.path() {
        UnixSocket::File(path) => UnixStream::connect(path),
        UnixSocket::Abstract(name) => SocketAddr::from_abstract_name(name.as_bytes())
            .and_then(|at| UnixStream::connect_addr(&at)),
        _ => return Err(HoldError::unavailable("session bus", "not a socket path")),
    };
    connected.map_err(|problem| HoldError::unavailable("session bus", problem))
}

/// Opens a session connection that takes no descriptor; its calls wait at most
/// `SESSION_WAIT`. Output: the connection, or `Unavailable`.
fn session() -> Result<zbus::blocking::Connection, HoldError> {
    let socket = Async::new(session_socket()?)
        .map_err(|problem| HoldError::unavailable("session bus", problem))?;
    let socket = Arc::new(socket);
    let read: Box<dyn ReadHalf> = Box::new(NoDescriptors(Arc::clone(&socket)));
    assert!(!read.can_pass_unix_fd());
    let write: Box<dyn WriteHalf> = Box::new(socket);
    let connection = zbus::blocking::connection::Builder::socket(Split::new(read, write))
        .method_timeout(SESSION_WAIT)
        .build()
        .map_err(|problem| HoldError::unavailable("session bus", problem))?;
    assert!(connection.unique_name().is_some());
    Ok(connection)
}

/// The display part of a Linux hold: one screensaver inhibit, held while its connection is
/// open. `give_back` ends it; a value dropped without it ends with its connection.
#[derive(Debug)]
pub(super) struct DisplayHold {
    connection: zbus::blocking::Connection,
    inhibitor: &'static Inhibitor,
    cookie: u32,
}

impl DisplayHold {
    /// Takes the inhibit: GNOME's session manager (idle flag) first, else the freedesktop
    /// screensaver. Output: the hold, or `Unavailable` with both refusals.
    pub(super) fn take() -> Result<Self, HoldError> {
        let connection = session()?;
        let gnome = connection.call_method(
            Some(GNOME.service),
            GNOME.object,
            Some(GNOME.interface),
            "Inhibit",
            &(APPLICATION, GNOME_TOPLEVEL, REASON, GNOME_IDLE),
        );
        let (inhibitor, reply) = match gnome {
            Ok(reply) => (&GNOME, reply),
            Err(first) => match connection.call_method(
                Some(FREEDESKTOP.service),
                FREEDESKTOP.object,
                Some(FREEDESKTOP.interface),
                "Inhibit",
                &(APPLICATION, REASON),
            ) {
                Ok(reply) => (&FREEDESKTOP, reply),
                Err(second) => {
                    let both = format!("{first}; {second}");
                    return Err(HoldError::unavailable("display inhibit", both));
                }
            },
        };
        let cookie: u32 = reply
            .body()
            .deserialize()
            .map_err(|problem| HoldError::unavailable("display inhibit", problem))?;
        assert!(*inhibitor == GNOME || *inhibitor == FREEDESKTOP);
        Ok(Self {
            connection,
            inhibitor,
            cookie,
        })
    }

    /// Gives the inhibit back and closes the connection, which ends it too. A refused
    /// give-back is logged. Input: the hold. Output: none.
    pub(super) fn give_back(self) {
        let inhibitor = self.inhibitor;
        assert!(*inhibitor == GNOME || *inhibitor == FREEDESKTOP);
        let given = self.connection.call_method(
            Some(inhibitor.service),
            inhibitor.object,
            Some(inhibitor.interface),
            inhibitor.give_back,
            &(self.cookie,),
        );
        if let Err(problem) = given {
            eprintln!("keep awake: the display inhibit was not given back: {problem}");
        }
    }
}
