use crate::coord::relay::{ANSWERED, REFUSED, REPLY_BYTES_MAX};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// The socket a native lead's team servers name each call on, in the lead's short temp
/// folder beside its tmux socket (DEC-111 Q18).
pub const CALLER_SOCKET: &str = "caller.sock";
/// How long a team server waits for the engine to name its call before giving up.
pub const CALLER_WAIT: Duration = Duration::from_secs(10);
/// The most new connections one lead's socket accepts a tick: room for every session's
/// hook and monitor requests of a full team within one tick (relay over the caller socket,
/// OQ-7 B).
pub const CALLERS_PER_TICK_MAX: usize = 64;
/// The most bytes of one relay request line, its newline included.
pub const RELAY_LINE_BYTES_MAX: usize = 4096;
const PENDING_MAX: usize = 16;
/// The most connections from one pane, or from processes in no pane, that wait for their
/// line at once (DEC-111 S1R1-3).
const PENDING_PER_PANE_MAX: usize = 4;
const PENDING_WAIT: Duration = Duration::from_secs(5);
const ANCESTORS_MAX: usize = 32;
const STAMPS_MAX: usize = 64;
/// The most names one pane holds unclaimed; its oldest makes room for a new one
/// (DEC-111 S1R1-3).
const STAMPS_PER_PANE_MAX: usize = 8;
const STAMP_TTL: Duration = Duration::from_secs(60);
/// The engine's handler of a relay request from a pane (OQ-7 B). Takes the pane, the pane
/// process the connection was found under, the request line and the connection, on which
/// it writes the answer.
pub type RelayHandler<'a> = dyn FnMut(&str, u32, &[u8], &mut dyn Write) + 'a;
/// The most panes whose first naming process the socket remembers; the oldest makes room.
const PINS_MAX: usize = 64;
const ID_BYTES: usize = 32;
const DIGEST_BYTES: usize = 32;
const LINE_BYTES: usize = ID_BYTES + 1 + 2 * DIGEST_BYTES + 1;
const PROCESSES_BYTES_MAX: usize = 1 << 20;
const NAMED: &[u8; 3] = b"ok\n";
const UNNAMED: &[u8; 3] = b"no\n";
const WHO: &[u8; 4] = b"who\n";
/// The word and space that open a relay request line.
const RELAY: &[u8; 6] = b"relay ";
/// The most bytes of the pane id in the engine's answer to `who`.
const PANE_REPLY_BYTES_MAX: usize = 16;
const TEAMMATE_WORD: &str = " teammate ";
/// The most bytes of the engine's whole answer to `who`: a pane id, the teammate word, a
/// worktree name and the newline.
const WHO_REPLY_BYTES_MAX: usize =
    PANE_REPLY_BYTES_MAX + TEAMMATE_WORD.len() + crate::agent_git::WORKTREE_NAME_BYTES_MAX + 1;

const _: () = assert!(PENDING_PER_PANE_MAX < PENDING_MAX && STAMPS_PER_PANE_MAX < STAMPS_MAX);
const _: () = assert!(RELAY_LINE_BYTES_MAX > LINE_BYTES && RELAY_LINE_BYTES_MAX > WHO.len());

/// The caller socket of the agent whose team mailbox is `mailbox`, found from the engine's
/// folder layout (`agent-<id>/tmp/<MAILBOX_FOLDER>` beside `agent-<id>/home`), so a team
/// server needs no argument for it. Takes the mailbox. Returns the socket path, or none
/// for a mailbox outside that layout.
pub fn socket_for_mailbox(mailbox: &Path) -> Option<PathBuf> {
    if mailbox.file_name()? != super::tool::MAILBOX_FOLDER {
        return None;
    }
    let tmp = mailbox.parent()?;
    if tmp.file_name()? != "tmp" {
        return None;
    }
    let home = tmp.parent()?.join("home");
    Some(crate::sandbox::toolchains::tmp_path(&home).join(CALLER_SOCKET))
}

/// The name in the same folder that a caller socket is bound at before one rename moves it
/// over the socket's own path, so that path never stands empty while the engine binds again
/// (rv-secreview RH2R2-1). Takes the socket path. Returns the sibling, the socket's name
/// with the `new` extension.
pub fn binding_path(socket: &Path) -> PathBuf {
    socket.with_extension("new")
}

/// Asks the engine to name a call before it is written: connects to the lead's caller
/// socket, sends the call id and the digest of the call's bytes, and waits up to
/// `CALLER_WAIT` for the engine's yes. Takes the mailbox, the call id and the digest.
/// Returns true when the engine named it or the agent has no caller socket (it leads no
/// native team), false for any other outcome.
pub fn name_call(mailbox: &Path, id: &str, digest: &[u8; DIGEST_BYTES]) -> bool {
    assert!(super::tool::call_id_valid(id));
    let Some(socket) = socket_for_mailbox(mailbox) else {
        return true;
    };
    if let Err(error) = std::fs::symlink_metadata(&socket)
        && error.kind() == std::io::ErrorKind::NotFound
    {
        return true;
    }
    let Ok(mut stream) = crate::spawn_gate::connect_unix_std(&socket) else {
        return false;
    };
    let mut line = Vec::with_capacity(LINE_BYTES);
    line.extend_from_slice(id.as_bytes());
    line.push(b' ');
    for byte in digest {
        line.extend_from_slice(format!("{byte:02x}").as_bytes());
    }
    line.push(b'\n');
    assert_eq!(line.len(), LINE_BYTES);
    let mut reply = [0_u8; 3];
    stream.set_read_timeout(Some(CALLER_WAIT)).is_ok()
        && stream.set_write_timeout(Some(CALLER_WAIT)).is_ok()
        && stream.write_all(&line).is_ok()
        && stream.read_exact(&mut reply).is_ok()
        && &reply == NAMED
}

/// Asks the engine where the asking process runs (DEC-111 Q18, RH-2): connects to the
/// lead's caller socket, sends `who` and waits up to `wait` for the pane the engine reached
/// from the process the kernel says connected and who the engine's rows put there, never
/// from the caller's environment or the agents' files. Takes the mailbox and the wait.
/// Returns `Alone` for a mailbox outside the engine's layout or no node at the socket path,
/// the agent leading no native team; `Placed` for an answer; `Unplaced` for no, a malformed
/// reply, a socket no engine answers on or no reply in time.
pub fn own_place(mailbox: &Path, wait: Duration) -> Place {
    let Some(socket) = socket_for_mailbox(mailbox) else {
        return Place::Alone;
    };
    if let Err(error) = std::fs::symlink_metadata(&socket)
        && error.kind() == std::io::ErrorKind::NotFound
    {
        return Place::Alone;
    }
    ask_place(&socket, wait).map_or(Place::Unplaced, |(pane, whom)| Place::Placed(pane, whom))
}

/// Sends `who` on the caller socket and reads one answer line within `wait`. Takes the
/// socket and the wait. Returns the pane and who runs in it, or None.
fn ask_place(socket: &Path, wait: Duration) -> Option<(String, Whom)> {
    let mut stream = crate::spawn_gate::connect_unix_std(socket).ok()?;
    stream.set_read_timeout(Some(wait)).ok()?;
    stream.set_write_timeout(Some(wait)).ok()?;
    stream.write_all(WHO).ok()?;
    let deadline = Instant::now() + wait;
    let mut reply = Vec::with_capacity(WHO_REPLY_BYTES_MAX);
    let mut byte = [0_u8; 1];
    while reply.len() < WHO_REPLY_BYTES_MAX && Instant::now() < deadline {
        stream.read_exact(&mut byte).ok()?;
        if byte[0] == b'\n' {
            return parse_place(&reply);
        }
        reply.push(byte[0]);
    }
    None
}

/// Sends one relay request of a native teammate's hook or monitor on its lead's caller
/// socket and reads the engine's whole answer (relay over the caller socket, OQ-7 B). The
/// engine acts for the pane it finds the connecting process in, never for anything the
/// request says. Takes the mailbox, the request without its `relay` word or newline, and
/// the wait. Returns the answer after its `ok` line, or None for a request that does not fit
/// one line, no socket, a refusal, an answer past the relay's `REPLY_BYTES_MAX` or none in
/// time.
pub fn relay(mailbox: &Path, request: &str, wait: Duration) -> Option<Vec<u8>> {
    if request.contains('\n') || RELAY.len() + request.len() + 1 > RELAY_LINE_BYTES_MAX {
        return None;
    }
    let socket = socket_for_mailbox(mailbox)?;
    let mut stream = crate::spawn_gate::connect_unix_std(&socket).ok()?;
    stream.set_read_timeout(Some(wait)).ok()?;
    stream.set_write_timeout(Some(wait)).ok()?;
    let mut line = Vec::with_capacity(RELAY.len() + request.len() + 1);
    line.extend_from_slice(RELAY);
    line.extend_from_slice(request.as_bytes());
    line.push(b'\n');
    assert!(line.len() <= RELAY_LINE_BYTES_MAX);
    stream.write_all(&line).ok()?;
    let deadline = Instant::now() + wait;
    let mut answer = Vec::new();
    let mut chunk = [0_u8; 8192];
    loop {
        if Instant::now() >= deadline {
            return None;
        }
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(read) => {
                answer.extend_from_slice(&chunk[..read]);
                if answer.len() > ANSWERED.len() + REPLY_BYTES_MAX {
                    return None;
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return None,
        }
    }
    answer.strip_prefix(ANSWERED).map(<[u8]>::to_vec)
}

/// Reads one `who` answer without its newline: `<pane> lead` or `<pane> teammate <name>`,
/// with a plain pane id and a name that names a worktree. Takes the bytes. Returns the
/// pane and who runs in it, or None for anything else.
fn parse_place(reply: &[u8]) -> Option<(String, Whom)> {
    let text = std::str::from_utf8(reply).ok()?;
    let mut parts = text.split(' ');
    let pane = parts
        .next()
        .filter(|pane| crate::terminal::pane_id_valid(pane) && pane.len() < PANE_REPLY_BYTES_MAX)?;
    let whom = match (parts.next()?, parts.next(), parts.next()) {
        ("lead", None, None) => Whom::Lead,
        ("teammate", Some(name), None) if crate::agent_git::worktree_name_valid(name) => {
            Whom::Teammate(name.to_owned())
        }
        _ => return None,
    };
    Some((pane.to_owned(), whom))
}

fn parse_line(line: &[u8]) -> Option<(String, [u8; DIGEST_BYTES])> {
    if line.len() != LINE_BYTES || line[ID_BYTES] != b' ' || line[LINE_BYTES - 1] != b'\n' {
        return None;
    }
    let id = std::str::from_utf8(&line[..ID_BYTES]).ok()?;
    if !super::tool::call_id_valid(id) {
        return None;
    }
    let hex = &line[ID_BYTES + 1..LINE_BYTES - 1];
    if !hex
        .iter()
        .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
    {
        return None;
    }
    let mut digest = [0_u8; DIGEST_BYTES];
    for (at, pair) in hex.as_chunks::<2>().0.iter().enumerate() {
        digest[at] = u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok()?;
    }
    Some((id.to_owned(), digest))
}

/// Who the engine says runs in the pane a `who` request came from, from its own rows and
/// never from a file the agents write (DEC-111 Q18, RH-2): the native lead itself, or one of
/// its live teammates by the name the engine first registered for that teammate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Whom {
    Lead,
    Teammate(String),
}

/// Where the engine places a hook or monitor that asked `who` (DEC-111 Q18, RH-2): alone
/// when its agent leads no native team, at a pane with who runs there, or nowhere when the
/// engine said no or gave no answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Place {
    Alone,
    Placed(String, Whom),
    Unplaced,
}

/// One native lead's caller socket (DEC-111 Q18): the engine names each team call by the
/// process the kernel says connected, walked up its parents to a pane the lead's tmux
/// server lists, never by anything the call or the caller's environment says. A name
/// holds the call id and the digest of the call's bytes, so a teammate that shares the
/// mailbox can neither reuse another caller's name nor swap the named call's contents.
/// The folder and the socket node are pinned by (device, inode) when bound, so the engine
/// can tell either replaced and never acts through a folder an agent swapped. Each pane is
/// also pinned to the pane process it first named a call by, so a pane respawned with
/// another process under the same id names nothing. Dropping it leaves the node, so a hook
/// that asks while no engine answers is refused, never taken for an agent that leads no
/// native team (RH-2).
pub struct Callers {
    listener: UnixListener,
    path: PathBuf,
    folder: (u64, u64),
    node: (u64, u64),
    pending: Vec<Pending>,
    stamps: Vec<Stamp>,
    pins: Vec<(String, u32)>,
}

struct Pending {
    stream: UnixStream,
    pane: Option<(String, u32)>,
    line: Vec<u8>,
    since: Instant,
}

struct Stamp {
    call: String,
    digest: [u8; DIGEST_BYTES],
    pane: String,
    pid: u32,
    at: Instant,
}

enum Received {
    Partial,
    Broken,
    Who,
    Line(String, [u8; DIGEST_BYTES]),
    Relay(Vec<u8>),
}

impl Pending {
    fn receive(&mut self) -> Received {
        let mut buffer = [0_u8; RELAY_LINE_BYTES_MAX];
        let room = RELAY_LINE_BYTES_MAX - self.line.len();
        match self.stream.read(&mut buffer[..room]) {
            Ok(0) => Received::Broken,
            Ok(read) => {
                self.line.extend_from_slice(&buffer[..read]);
                assert!(self.line.len() <= RELAY_LINE_BYTES_MAX);
                self.received()
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => Received::Partial,
            Err(_) => Received::Broken,
        }
    }

    /// Reads what has arrived of the connection's one line: a relay request ends at its
    /// first newline, which must be its last byte, within `RELAY_LINE_BYTES_MAX`; `who` is
    /// those bytes alone; a call line is exactly `LINE_BYTES`. Takes the connection.
    /// Returns what it is, Partial while more may complete it.
    fn received(&self) -> Received {
        let line = &self.line;
        if line.starts_with(RELAY) {
            return match line.iter().position(|byte| *byte == b'\n') {
                Some(end) if end + 1 == line.len() => {
                    Received::Relay(line[RELAY.len()..end].to_vec())
                }
                None if line.len() < RELAY_LINE_BYTES_MAX => Received::Partial,
                _ => Received::Broken,
            };
        }
        if line.len() < LINE_BYTES {
            return if line == WHO {
                Received::Who
            } else if line.contains(&b'\n') {
                Received::Broken
            } else {
                Received::Partial
            };
        }
        if line.len() > LINE_BYTES {
            return Received::Broken;
        }
        parse_line(line).map_or(Received::Broken, |(call, digest)| {
            Received::Line(call, digest)
        })
    }
}

impl Callers {
    /// Binds a lead's caller socket at `path`. The folder must still be this user's own
    /// private folder, not a link, and the very folder the engine pinned when it made it,
    /// if it did (DEC-111 S1R1-1). The socket is bound at [`binding_path`], after whatever
    /// an earlier bind left there is removed, and one rename moves it over whatever is at
    /// `path`, so a hook asking meanwhile finds the old node or the new socket and never an
    /// empty path (rv-secreview RH2R2-1). The socket is kept non-blocking so a tick never
    /// waits on it. Takes the path in the lead's short temp folder and the folder's pin, if
    /// known. Returns the socket, or the I/O failure, `PermissionDenied` for a changed
    /// folder and `NotFound` when the node moved over `path` is not the one bound.
    pub fn bind(path: &Path, pinned: Option<(u64, u64)>) -> std::io::Result<Self> {
        let folder = path
            .parent()
            .and_then(crate::sandbox::toolchains::private_folder_pin)
            .filter(|folder| pinned.is_none_or(|pinned| pinned == *folder))
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "the caller socket folder changed",
                )
            })?;
        let binding = binding_path(path);
        if let Err(error) = std::fs::remove_file(&binding)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            return Err(error);
        }
        let listener = crate::spawn_gate::bind_unix_std(&binding)?;
        listener.set_nonblocking(true)?;
        let gone = || std::io::Error::new(std::io::ErrorKind::NotFound, "the bound socket is gone");
        let made = socket_pin(&binding).ok_or_else(gone)?;
        std::fs::rename(&binding, path)?;
        let node = socket_pin(path)
            .filter(|node| *node == made)
            .ok_or_else(gone)?;
        Ok(Self {
            listener,
            path: path.to_path_buf(),
            folder,
            node,
            pending: Vec::new(),
            stamps: Vec::new(),
            pins: Vec::new(),
        })
    }

    /// Carries the pane pins of the socket this one replaces in the same folder, so binding
    /// again never frees a pane to be named by another process. Takes the new socket and
    /// the old one's pins. Returns the socket.
    #[must_use]
    pub fn with_pins(mut self, pins: Vec<(String, u32)>) -> Self {
        assert!(self.pins.is_empty() && pins.len() <= PINS_MAX);
        self.pins = pins;
        self
    }

    /// The pane pins held, for [`Self::with_pins`]. Takes the socket. Returns them.
    pub fn pins(&self) -> Vec<(String, u32)> {
        self.pins.clone()
    }

    /// Tells whether the socket node at the path is still the one bound, in one look: false
    /// once anyone unlinked or replaced it (DEC-111 S1R1-2). Takes the socket. Returns the
    /// answer.
    pub fn intact(&self) -> bool {
        socket_pin(&self.path) == Some(self.node)
    }

    /// The (device, inode) pin of the folder the socket was bound in, for binding again in
    /// the same folder. Takes the socket. Returns the pin.
    pub fn folder(&self) -> (u64, u64) {
        self.folder
    }

    /// Accepts the connections waiting on the socket, at most `CALLERS_PER_TICK_MAX`, finds
    /// on accepting each the pane whose process is the connecting process or one of its
    /// first `ANCESTORS_MAX` parents, then reads each one's call id and digest and names the
    /// call by that pane, answering yes or no; a `who` request is answered with that pane and
    /// who the engine's rows put there, or no (DEC-111 Q18, RH-2); a relay request goes to
    /// the engine with that pane (relay over the caller socket, OQ-7 B). A connection whose
    /// line has not fully arrived waits, at most `PENDING_PER_PANE_MAX` of one pane at once,
    /// and is dropped with no full line within `PENDING_WAIT`. Takes the lead's panes as
    /// (pane id, pid), listed with the process table at most once a tick, only when a
    /// connection arrives, the lookup of who runs in a pane by the pane process the
    /// connection was found under, and the engine's handler of a relay request from a pane,
    /// which answers on the connection. Returns how many it named.
    pub fn serve(
        &mut self,
        panes: &mut dyn FnMut() -> Vec<(String, u32)>,
        whom: &mut dyn FnMut(&str, u32) -> Option<Whom>,
        relay: &mut RelayHandler<'_>,
    ) -> usize {
        let now = Instant::now();
        self.stamps
            .retain(|stamp| now.duration_since(stamp.at) < STAMP_TTL);
        self.pending
            .retain(|pending| now.duration_since(pending.since) < PENDING_WAIT);
        let mut listed: Option<Vec<(String, u32)>> = None;
        let mut parents: Option<HashMap<u32, u32>> = None;
        let mut named = 0;
        for _ in 0..CALLERS_PER_TICK_MAX {
            if self.pending.len() >= PENDING_MAX {
                break;
            }
            let Ok(stream) = crate::spawn_gate::accept_unix_std(&self.listener) else {
                break;
            };
            let Some(pid) = peer_pid(&stream) else {
                continue;
            };
            if stream.set_nonblocking(true).is_err() {
                continue;
            }
            let pane = pane_of(
                pid,
                listed.get_or_insert_with(&mut *panes),
                parents.get_or_insert_with(|| process_parents().unwrap_or_default()),
            );
            let mut pending = Pending {
                stream,
                pane,
                line: Vec::new(),
                since: now,
            };
            match pending.receive() {
                Received::Partial => {
                    let waiting = self
                        .pending
                        .iter()
                        .filter(|held| held.pane == pending.pane)
                        .count();
                    if waiting < PENDING_PER_PANE_MAX {
                        self.pending.push(pending);
                    }
                }
                Received::Broken => {}
                Received::Who => self.tell(pending, whom),
                Received::Line(call, digest) => {
                    named += usize::from(self.answer(pending, call, digest, now));
                }
                Received::Relay(request) => self.relay(pending, &request, relay),
            }
        }
        for mut pending in std::mem::take(&mut self.pending) {
            match pending.receive() {
                Received::Partial => self.pending.push(pending),
                Received::Broken => {}
                Received::Who => self.tell(pending, whom),
                Received::Line(call, digest) => {
                    named += usize::from(self.answer(pending, call, digest, now));
                }
                Received::Relay(request) => self.relay(pending, &request, relay),
            }
        }
        assert!(self.pending.len() <= PENDING_MAX && self.stamps.len() <= STAMPS_MAX);
        named
    }

    /// Names one call by its connection's pane, unless that call id was named already or
    /// the connection reached no pane, and answers yes or no on the connection. Takes the
    /// socket, the connection, the call id, the digest and the time. Returns whether it
    /// named the call.
    fn answer(
        &mut self,
        mut pending: Pending,
        call: String,
        digest: [u8; DIGEST_BYTES],
        now: Instant,
    ) -> bool {
        let fresh = !self.stamps.iter().any(|stamp| stamp.call == call);
        let pane = if fresh { pending.pane.take() } else { None };
        let pane = pane.filter(|(pane, pid)| self.pinned(pane, *pid));
        let named = pane.is_some();
        if let Some((pane, pid)) = pane {
            self.make_room(&pane);
            self.stamps.push(Stamp {
                call,
                digest,
                pane,
                pid,
                at: now,
            });
        }
        let _ = pending
            .stream
            .write_all(if named { NAMED } else { UNNAMED });
        named
    }

    /// Answers a `who` request with the pane the connection reached, under the same pin a
    /// call is named by, and who the engine's rows put there: `<pane> lead` or `<pane>
    /// teammate <name>` (DEC-111 Q18, RH-2). A process in no pane, a pane respawned under
    /// another process, a pane id that is not plain, a pane the rows name no one by and a
    /// name that names no worktree get no. Takes the socket, the connection and the lookup.
    fn tell(&mut self, mut pending: Pending, whom: &mut dyn FnMut(&str, u32) -> Option<Whom>) {
        let reply = pending
            .pane
            .take()
            .filter(|(pane, pid)| self.pinned(pane, *pid))
            .filter(|(pane, _)| {
                crate::terminal::pane_id_valid(pane) && pane.len() < PANE_REPLY_BYTES_MAX
            })
            .and_then(|(pane, pid)| match whom(&pane, pid)? {
                Whom::Lead => Some(format!("{pane} lead\n")),
                Whom::Teammate(name) => crate::agent_git::worktree_name_valid(&name)
                    .then(|| format!("{pane}{TEAMMATE_WORD}{name}\n")),
            })
            .map_or_else(|| UNNAMED.to_vec(), String::into_bytes);
        let _ = pending.stream.write_all(&reply);
    }

    /// Hands one relay request to the engine with the pane the connection reached, under the
    /// same pin a call is named by; a process in no pane and a pane respawned under another
    /// process get no (relay over the caller socket, OQ-7 B). The connection stays
    /// non-blocking with a send buffer that holds a whole answer, so the engine writes it at
    /// once and never waits on a hook that stopped reading. Takes the socket, the connection,
    /// the request and the engine's handler.
    fn relay(&mut self, mut pending: Pending, request: &[u8], relay: &mut RelayHandler<'_>) {
        let room = 2 * (ANSWERED.len() + REPLY_BYTES_MAX);
        let placed = pending
            .pane
            .take()
            .filter(|(pane, pid)| self.pinned(pane, *pid))
            .filter(|_| {
                nix::sys::socket::setsockopt(
                    &pending.stream,
                    nix::sys::socket::sockopt::SndBuf,
                    &room,
                )
                .is_ok()
            });
        match placed {
            Some((pane, pid)) => relay(&pane, pid, request, &mut pending.stream),
            None => {
                let _ = pending.stream.write_all(REFUSED);
            }
        }
    }

    /// Whether a pane may name a call by this pane process: the first one seen pins the pane
    /// to itself, and any other process under that pane id later is refused. The oldest pin
    /// makes room past `PINS_MAX`. Takes the socket, the pane id and the pane process.
    /// Returns the answer.
    fn pinned(&mut self, pane: &str, pid: u32) -> bool {
        if let Some((_, held)) = self.pins.iter().find(|(pinned, _)| pinned == pane) {
            return *held == pid;
        }
        if self.pins.len() >= PINS_MAX {
            self.pins.remove(0);
        }
        self.pins.push((pane.to_owned(), pid));
        assert!(self.pins.len() <= PINS_MAX);
        true
    }

    /// Frees a place for one more name of `pane`: its own oldest when it holds
    /// `STAMPS_PER_PANE_MAX`, otherwise, when all `STAMPS_MAX` are held, the oldest of the
    /// pane holding the most, so no pane takes another's place (DEC-111 S1R1-3). Takes the
    /// socket and the pane.
    fn make_room(&mut self, pane: &str) {
        let full = {
            let mut held: HashMap<&str, usize> = HashMap::new();
            for stamp in &self.stamps {
                *held.entry(stamp.pane.as_str()).or_default() += 1;
            }
            if held
                .get(pane)
                .is_some_and(|count| *count >= STAMPS_PER_PANE_MAX)
            {
                Some(pane.to_owned())
            } else if self.stamps.len() >= STAMPS_MAX {
                held.into_iter()
                    .max_by_key(|(_, count)| *count)
                    .map(|(fullest, _)| fullest.to_owned())
            } else {
                None
            }
        };
        if let Some(at) =
            full.and_then(|full| self.stamps.iter().position(|stamp| stamp.pane == full))
        {
            self.stamps.remove(at);
        }
        assert!(self.stamps.len() < STAMPS_MAX);
    }

    /// Takes the name of one call, once: the pane the engine named it by and the pane
    /// process it found the caller under, only when the call file's bytes have the digest
    /// the caller sent. Takes the call id and the digest of the file the engine read.
    /// Returns the pane id and that process, or none.
    pub fn take(&mut self, call: &str, digest: &[u8; DIGEST_BYTES]) -> Option<(String, u32)> {
        let at = self.stamps.iter().position(|stamp| stamp.call == call)?;
        let stamp = self.stamps.remove(at);
        (stamp.digest == *digest).then_some((stamp.pane, stamp.pid))
    }
}

impl Drop for Callers {
    fn drop(&mut self) {
        let folder = self
            .path
            .parent()
            .and_then(crate::sandbox::toolchains::private_folder_pin);
        if folder != Some(self.folder) {
            eprintln!("team_caller_folder_changed");
        }
    }
}

fn socket_pin(path: &Path) -> Option<(u64, u64)> {
    std::fs::symlink_metadata(path)
        .ok()
        .filter(|metadata| metadata.file_type().is_socket())
        .map(|metadata| (metadata.dev(), metadata.ino()))
}

#[cfg(target_os = "macos")]
pub(crate) fn peer_pid(stream: &UnixStream) -> Option<u32> {
    nix::sys::socket::getsockopt(stream, nix::sys::socket::sockopt::LocalPeerPid)
        .ok()
        .and_then(|pid| u32::try_from(pid).ok())
}

#[cfg(target_os = "linux")]
pub(crate) fn peer_pid(stream: &UnixStream) -> Option<u32> {
    nix::sys::socket::getsockopt(stream, nix::sys::socket::sockopt::PeerCredentials)
        .ok()
        .and_then(|credentials| u32::try_from(credentials.pid()).ok())
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub(crate) fn peer_pid(_stream: &UnixStream) -> Option<u32> {
    None
}

fn pane_of(
    pid: u32,
    panes: &[(String, u32)],
    parents: &HashMap<u32, u32>,
) -> Option<(String, u32)> {
    let mut current = pid;
    for _ in 0..ANCESTORS_MAX {
        if let Some(found) = panes.iter().find(|(_, listed)| *listed == current) {
            return Some(found.clone());
        }
        if current <= 1 {
            return None;
        }
        current = *parents.get(&current)?;
    }
    None
}

pub(crate) fn process_parents() -> Option<HashMap<u32, u32>> {
    let output = crate::spawn_gate::output(
        Command::new("/bin/ps")
            .args(["-A", "-o", "pid=,ppid="])
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null()),
    )
    .ok()?;
    if !output.status.success() || output.stdout.len() > PROCESSES_BYTES_MAX {
        return None;
    }
    std::str::from_utf8(&output.stdout)
        .ok()?
        .lines()
        .map(|line| {
            let mut fields = line.split_whitespace();
            let pid = fields.next()?.parse().ok()?;
            let parent = fields.next()?.parse().ok()?;
            fields.next().is_none().then_some((pid, parent))
        })
        .collect()
}

#[cfg(test)]
mod tests;
