use std::collections::HashMap;
use std::io::{Read, Write};
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
const PENDING_WAIT: Duration = Duration::from_secs(5);
const ANCESTORS_MAX: usize = 32;
const STAMPS_MAX: usize = 64;
const STAMP_TTL: Duration = Duration::from_secs(60);
const ID_BYTES: usize = 32;
const DIGEST_BYTES: usize = 32;
const LINE_BYTES: usize = ID_BYTES + 1 + 2 * DIGEST_BYTES + 1;
const PROCESSES_BYTES_MAX: usize = 1 << 20;
const NAMED: &[u8; 3] = b"ok\n";
const UNNAMED: &[u8; 3] = b"no\n";

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
pub struct Callers {
    listener: UnixListener,
    path: PathBuf,
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
                    return if self.line.contains(&b'\n') {
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
    /// Binds a lead's caller socket at `path`, first removing whatever an earlier run left
    /// there, and keeps it non-blocking so a tick never waits on it. Takes the path in the
    /// lead's short temp folder. Returns the socket, or the I/O failure.
    pub fn bind(path: &Path) -> std::io::Result<Self> {
        if let Err(error) = std::fs::remove_file(path)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            return Err(error);
        }
        let listener = crate::spawn_gate::bind_unix_std(path)?;
        listener.set_nonblocking(true)?;
        Ok(Self {
            listener,
            path: path.to_path_buf(),
            pending: Vec::new(),
            stamps: Vec::new(),
        })
    }

    /// Accepts the connections waiting on the socket, at most `CALLERS_PER_TICK_MAX`, finds
    /// on accepting each the pane whose process is the connecting process or one of its
    /// first `ANCESTORS_MAX` parents, then reads each one's call id and digest and names the
    /// call by that pane, answering yes or no. A connection with no full line within
    /// `PENDING_WAIT` is dropped. Takes the lead's panes as (pane id, pid), listed with the
    /// process table at most once a tick, only when a connection arrives. Returns how many
    /// it named.
    pub fn serve(&mut self, panes: &mut dyn FnMut() -> Vec<(String, u32)>) -> usize {
        let now = Instant::now();
        self.stamps
            .retain(|stamp| now.duration_since(stamp.at) < STAMP_TTL);
        self.pending
            .retain(|pending| now.duration_since(pending.since) < PENDING_WAIT);
        let mut listed: Option<Vec<(String, u32)>> = None;
        let mut parents: Option<HashMap<u32, u32>> = None;
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
            self.pending.push(Pending {
                stream,
                pane,
                line: Vec::new(),
                since: now,
            });
        }
        let mut named = 0;
        for mut pending in std::mem::take(&mut self.pending) {
            match pending.receive() {
                Received::Partial => self.pending.push(pending),
                Received::Broken => {}
                Received::Line(call, digest) => {
                    let fresh = self.stamps.len() < STAMPS_MAX
                        && !self.stamps.iter().any(|stamp| stamp.call == call);
                    let pane = if fresh { pending.pane.take() } else { None };
                    let reply = match pane {
                        Some(pane) => {
                            self.stamps.push(Stamp {
                                call,
                                digest,
                                pane,
                                at: now,
                            });
                            named += 1;
                            NAMED
                        }
                        None => UNNAMED,
                    };
                    let _ = pending.stream.write_all(reply);
                }
            }
        }
        assert!(self.pending.len() <= PENDING_MAX && self.stamps.len() <= STAMPS_MAX);
        named
    }

    /// Takes the name of one call, once: the pane the engine named it by, only when the
    /// call file's bytes have the digest the caller sent. Takes the call id and the digest
    /// of the file the engine read. Returns the pane id, or none.
    pub fn take(&mut self, call: &str, digest: &[u8; DIGEST_BYTES]) -> Option<String> {
        let at = self.stamps.iter().position(|stamp| stamp.call == call)?;
        let stamp = self.stamps.swap_remove(at);
        (stamp.digest == *digest).then_some(stamp.pane)
    }
}

impl Drop for Callers {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
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
