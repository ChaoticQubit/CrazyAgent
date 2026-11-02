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
/// The most new connections one lead's socket accepts a tick.
pub const CALLERS_PER_TICK_MAX: usize = 16;
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
const ID_BYTES: usize = 32;
const DIGEST_BYTES: usize = 32;
const LINE_BYTES: usize = ID_BYTES + 1 + 2 * DIGEST_BYTES + 1;
const PROCESSES_BYTES_MAX: usize = 1 << 20;
const NAMED: &[u8; 3] = b"ok\n";
const UNNAMED: &[u8; 3] = b"no\n";
const WHO: &[u8; 4] = b"who\n";
/// The most bytes of the engine's answer to `who`: a tmux pane id and its newline.
const PANE_REPLY_BYTES_MAX: usize = 16;

const _: () = assert!(PENDING_PER_PANE_MAX < PENDING_MAX && STAMPS_PER_PANE_MAX < STAMPS_MAX);

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

/// Asks the engine which pane the asking process runs in (DEC-111 Q18): connects to the
/// lead's caller socket, sends `who` and waits up to `wait` for the pane the engine reached
/// from the process the kernel says connected, never from the caller's environment. Takes
/// the mailbox and the wait. Returns the pane id, or None for no socket, no pane, a reply
/// that is no pane id or no reply in time.
pub fn own_pane(mailbox: &Path, wait: Duration) -> Option<String> {
    let socket = socket_for_mailbox(mailbox)?;
    let mut stream = crate::spawn_gate::connect_unix_std(&socket).ok()?;
    stream.set_read_timeout(Some(wait)).ok()?;
    stream.set_write_timeout(Some(wait)).ok()?;
    stream.write_all(WHO).ok()?;
    let deadline = Instant::now() + wait;
    let mut reply = Vec::with_capacity(PANE_REPLY_BYTES_MAX);
    let mut byte = [0_u8; 1];
    while reply.len() < PANE_REPLY_BYTES_MAX && Instant::now() < deadline {
        stream.read_exact(&mut byte).ok()?;
        if byte[0] == b'\n' {
            let pane = String::from_utf8(reply).ok()?;
            return crate::terminal::pane_id_valid(&pane).then_some(pane);
        }
        reply.push(byte[0]);
    }
    None
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

/// One native lead's caller socket (DEC-111 Q18): the engine names each team call by the
/// process the kernel says connected, walked up its parents to a pane the lead's tmux
/// server lists, never by anything the call or the caller's environment says. A name
/// holds the call id and the digest of the call's bytes, so a teammate that shares the
/// mailbox can neither reuse another caller's name nor swap the named call's contents.
/// The folder and the socket node are pinned by (device, inode) when bound, so the engine
/// can tell either replaced and never acts through a folder an agent swapped.
pub struct Callers {
    listener: UnixListener,
    path: PathBuf,
    folder: (u64, u64),
    node: (u64, u64),
    pending: Vec<Pending>,
    stamps: Vec<Stamp>,
}

struct Pending {
    stream: UnixStream,
    pane: Option<String>,
    line: Vec<u8>,
    since: Instant,
}

struct Stamp {
    call: String,
    digest: [u8; DIGEST_BYTES],
    pane: String,
    at: Instant,
}

enum Received {
    Partial,
    Broken,
    Who,
    Line(String, [u8; DIGEST_BYTES]),
}

impl Pending {
    fn receive(&mut self) -> Received {
        let mut buffer = [0_u8; LINE_BYTES];
        let room = LINE_BYTES - self.line.len();
        match self.stream.read(&mut buffer[..room]) {
            Ok(0) => Received::Broken,
            Ok(read) => {
                self.line.extend_from_slice(&buffer[..read]);
                assert!(self.line.len() <= LINE_BYTES);
                if self.line.len() < LINE_BYTES {
                    return if self.line == WHO {
                        Received::Who
                    } else if self.line.contains(&b'\n') {
                        Received::Broken
                    } else {
                        Received::Partial
                    };
                }
                parse_line(&self.line).map_or(Received::Broken, |(call, digest)| {
                    Received::Line(call, digest)
                })
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => Received::Partial,
            Err(_) => Received::Broken,
        }
    }
}

impl Callers {
    /// Binds a lead's caller socket at `path`. The folder must still be this user's own
    /// private folder, not a link, and the very folder the engine pinned when it made it,
    /// if it did (DEC-111 S1R1-1); then whatever an earlier run left at `path` is removed.
    /// The socket is kept non-blocking so a tick never waits on it. Takes the path in the
    /// lead's short temp folder and the folder's pin, if known. Returns the socket, or the
    /// I/O failure, `PermissionDenied` for a changed folder.
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
        if let Err(error) = std::fs::remove_file(path)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            return Err(error);
        }
        let listener = crate::spawn_gate::bind_unix_std(path)?;
        listener.set_nonblocking(true)?;
        let node = socket_pin(path).ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::NotFound, "the bound socket is gone")
        })?;
        Ok(Self {
            listener,
            path: path.to_path_buf(),
            folder,
            node,
            pending: Vec::new(),
            stamps: Vec::new(),
        })
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
    /// call by that pane, answering yes or no; a `who` request is answered with that pane, or
    /// no (DEC-111 Q18). A connection whose line has not fully arrived
    /// waits, at most `PENDING_PER_PANE_MAX` of one pane at once, and is dropped with no
    /// full line within `PENDING_WAIT`. Takes the lead's panes as (pane id, pid), listed
    /// with the process table at most once a tick, only when a connection arrives. Returns
    /// how many it named.
    pub fn serve(&mut self, panes: &mut dyn FnMut() -> Vec<(String, u32)>) -> usize {
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
                Received::Who => tell(pending),
                Received::Line(call, digest) => {
                    named += usize::from(self.answer(pending, call, digest, now));
                }
            }
        }
        for mut pending in std::mem::take(&mut self.pending) {
            match pending.receive() {
                Received::Partial => self.pending.push(pending),
                Received::Broken => {}
                Received::Who => tell(pending),
                Received::Line(call, digest) => {
                    named += usize::from(self.answer(pending, call, digest, now));
                }
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
        let named = pane.is_some();
        if let Some(pane) = pane {
            self.make_room(&pane);
            self.stamps.push(Stamp {
                call,
                digest,
                pane,
                at: now,
            });
        }
        let _ = pending
            .stream
            .write_all(if named { NAMED } else { UNNAMED });
        named
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

    /// Takes the name of one call, once: the pane the engine named it by, only when the
    /// call file's bytes have the digest the caller sent. Takes the call id and the digest
    /// of the file the engine read. Returns the pane id, or none.
    pub fn take(&mut self, call: &str, digest: &[u8; DIGEST_BYTES]) -> Option<String> {
        let at = self.stamps.iter().position(|stamp| stamp.call == call)?;
        let stamp = self.stamps.remove(at);
        (stamp.digest == *digest).then_some(stamp.pane)
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
        } else if self.intact() {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// Answers a `who` request with the pane the connection reached, or no for a process in
/// no pane or a pane id that is not plain. Takes the connection.
fn tell(mut pending: Pending) {
    let reply = pending
        .pane
        .take()
        .filter(|pane| crate::terminal::pane_id_valid(pane) && pane.len() < PANE_REPLY_BYTES_MAX)
        .map_or_else(|| UNNAMED.to_vec(), |pane| format!("{pane}\n").into_bytes());
    let _ = pending.stream.write_all(&reply);
}

fn socket_pin(path: &Path) -> Option<(u64, u64)> {
    std::fs::symlink_metadata(path)
        .ok()
        .filter(|metadata| metadata.file_type().is_socket())
        .map(|metadata| (metadata.dev(), metadata.ino()))
}

#[cfg(target_os = "macos")]
fn peer_pid(stream: &UnixStream) -> Option<u32> {
    nix::sys::socket::getsockopt(stream, nix::sys::socket::sockopt::LocalPeerPid)
        .ok()
        .and_then(|pid| u32::try_from(pid).ok())
}

#[cfg(target_os = "linux")]
fn peer_pid(stream: &UnixStream) -> Option<u32> {
    nix::sys::socket::getsockopt(stream, nix::sys::socket::sockopt::PeerCredentials)
        .ok()
        .and_then(|credentials| u32::try_from(credentials.pid()).ok())
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn peer_pid(_stream: &UnixStream) -> Option<u32> {
    None
}

fn pane_of(pid: u32, panes: &[(String, u32)], parents: &HashMap<u32, u32>) -> Option<String> {
    let mut current = pid;
    for _ in 0..ANCESTORS_MAX {
        if let Some((pane, _)) = panes.iter().find(|(_, listed)| *listed == current) {
            return Some(pane.clone());
        }
        if current <= 1 {
            return None;
        }
        current = *parents.get(&current)?;
    }
    None
}

fn process_parents() -> Option<HashMap<u32, u32>> {
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
