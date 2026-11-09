use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// Where a Codex app-server and its TUI meet inside an agent's `CODEX_HOME`: the default
/// `unix://` control socket of Codex 0.164, a link to the real socket in the shared
/// daemon folder.
pub const CONTROL_SOCKET: &str = "app-server-control/app-server-control.sock";

/// The Codex releases, as major and minor, whose app-server protocol the bridge was
/// checked against on the installed binary (DEC-104 Q13). Any other release fails closed,
/// so the engine falls back to typing.
pub const VERSIONS: [(u64, u64); 1] = [(0, 164)];

/// What one bridge attempt did: the text reached the agent's Codex thread through
/// `turn/start` or `turn/steer`, into the turn whose id the server named (none when its
/// answer named none of at most `TURN_ID_BYTES_MAX` bytes), or the bridge cannot be used
/// now, with a plain reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Bridged {
    Delivered(Option<String>),
    Unavailable(&'static str),
}

/// The longest Codex turn id the bridge keeps; 0.164 names turns by UUID.
pub const TURN_ID_BYTES_MAX: usize = 64;

/// The real socket a Codex app-server binds for an agent home on this machine: Codex
/// hashes the canonical control socket path with SHA-256 into
/// `/private/tmp/codex-daemon-<uid>/` and links it from the home. Takes the agent's
/// `CODEX_HOME`. Returns the path, or none off macOS or for a home that does not resolve.
pub fn socket(home: &Path) -> Option<PathBuf> {
    let control = home.canonicalize().ok()?.join(CONTROL_SOCKET);
    let name = format!("{:x}", Sha256::digest(control.to_str()?.as_bytes()));
    Some(daemon_folder()?.join(name))
}

/// Makes the shared Codex daemon folder before an agent launches, owned by the owner and
/// private (0700), so no agent sandbox needs to create or change it; only the agent's own
/// socket and lock inside are granted. Takes nothing. Returns the folder, or a plain
/// reason when it cannot be made or someone else made it.
pub fn prepare_folder() -> Result<PathBuf, &'static str> {
    let folder = daemon_folder().ok_or("codex_bridge_unsupported")?;
    wire::private_folder(&folder)?;
    Ok(folder)
}

/// Delivers text into the Codex thread an agent's TUI shows, through the agent's own
/// app-server: `turn/start` while the thread is idle, only when `start` allows a turn of
/// its own, and `turn/steer` on the active turn while it works (DEC-104 Q13). The socket
/// must be the owner's own, the release one of `VERSIONS`, and the whole exchange fits in
/// `wire::DEADLINE` and bounded reads; any doubt fails closed. Takes the agent's
/// `CODEX_HOME`, the text and whether an idle thread may start a turn. Returns the outcome.
pub fn deliver(home: &Path, text: &str, start: bool) -> Bridged {
    let Some(socket) = socket(home) else {
        return Bridged::Unavailable("codex_bridge_unsupported");
    };
    match wire::deliver(&socket, text, start) {
        Ok(turn) => Bridged::Delivered(turn),
        Err(reason) => Bridged::Unavailable(reason),
    }
}

/// Keeps a turn id the app-server named when it is short, plain text, so a stored id
/// never carries more than `TURN_ID_BYTES_MAX` printable ASCII bytes. Takes the id, if
/// any. Returns it, or none for a missing, empty, long or unprintable one.
#[cfg(any(target_os = "macos", all(test, unix)))]
fn turn_id(id: Option<&str>) -> Option<String> {
    id.filter(|id| {
        !id.is_empty()
            && id.len() <= TURN_ID_BYTES_MAX
            && id.bytes().all(|byte| byte.is_ascii_graphic())
    })
    .map(str::to_owned)
}

/// The shared folder Codex binds app-server sockets in. Takes nothing. Returns
/// `/private/tmp/codex-daemon-<uid>` on macOS, none elsewhere.
fn daemon_folder() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        Some(PathBuf::from(format!(
            "/private/tmp/codex-daemon-{}",
            rustix::process::getuid().as_raw()
        )))
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

/// Reads a Codex app-server's `userAgent`, `<client>/<release> (...)`, and checks the
/// release against `VERSIONS`. Takes the text. Returns whether the bridge may run.
#[cfg(any(target_os = "macos", all(test, unix)))]
fn supported(user_agent: &str) -> bool {
    user_agent
        .split_whitespace()
        .next()
        .and_then(|head| head.split_once('/'))
        .and_then(|(_, release)| semver::Version::parse(release).ok())
        .is_some_and(|release| VERSIONS.contains(&(release.major, release.minor)))
}

#[cfg(all(test, unix))]
mod tests;

#[cfg(target_os = "macos")]
mod wire {
    use super::{supported, turn_id};
    use serde_json::{Value, json};
    use std::io::{Read, Write};
    use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt};
    use std::os::unix::net::UnixStream;
    use std::path::Path;
    use std::time::{Duration, Instant};

    /// The longest one bridge exchange may take, connect to last reply.
    pub(super) const DEADLINE: Duration = Duration::from_secs(3);
    const HEAD_BYTES_MAX: usize = 4096;
    const MESSAGE_BYTES_MAX: usize = 1024 * 1024;
    const READ_BYTES_MAX: usize = 8 * MESSAGE_BYTES_MAX;
    const CONTROL_BYTES_MAX: usize = 125;
    const CHUNK_BYTES: usize = 8192;
    const THREADS_MAX: usize = 16;
    const ATTEMPTS_MAX: usize = 2;

    const _: () = assert!(HEAD_BYTES_MAX < MESSAGE_BYTES_MAX && MESSAGE_BYTES_MAX < READ_BYTES_MAX);

    /// One WebSocket connection to an app-server socket, as Codex speaks it over `unix://`,
    /// with JSON-RPC on top. Reads stop at the deadline or after `READ_BYTES_MAX` bytes.
    struct Wire {
        stream: UnixStream,
        buffer: Vec<u8>,
        read: usize,
        deadline: Instant,
        next_id: i64,
    }

    /// What one try to put text into a thread did: it went into a turn, named by the id
    /// the server gave when that id is a plain one (`turn_id`), or the turn changed under
    /// the request.
    enum Sent {
        Into(Option<String>),
        Moved,
    }

    pub(super) fn private_folder(folder: &Path) -> Result<(), &'static str> {
        match std::fs::DirBuilder::new().mode(0o700).create(folder) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(_) => return Err("codex_bridge_folder_unsafe"),
        }
        let metadata =
            std::fs::symlink_metadata(folder).map_err(|_| "codex_bridge_folder_unsafe")?;
        let mine = metadata.uid() == rustix::process::getuid().as_raw();
        if !metadata.is_dir() || !mine || metadata.mode() & 0o077 != 0 {
            return Err("codex_bridge_folder_unsafe");
        }
        Ok(())
    }

    pub(super) fn deliver(
        socket: &Path,
        text: &str,
        start: bool,
    ) -> Result<Option<String>, &'static str> {
        let folder = socket.parent().ok_or("codex_bridge_unsupported")?;
        private_folder(folder)?;
        let metadata = match std::fs::symlink_metadata(socket) {
            Ok(metadata) => metadata,
            Err(_) => return Err("codex_bridge_not_running"),
        };
        if !metadata.file_type().is_socket() || metadata.uid() != rustix::process::getuid().as_raw()
        {
            return Err("codex_bridge_socket_unsafe");
        }
        let mut wire = Wire::open(socket)?;
        let started = wire.call(
            "initialize",
            json!({"clientInfo": {"name": "crazyagent", "version": env!("CARGO_PKG_VERSION")}}),
        )?;
        let user_agent = started
            .get("userAgent")
            .and_then(Value::as_str)
            .ok_or("codex_bridge_protocol_unknown")?;
        if !supported(user_agent) {
            return Err("codex_bridge_version_unknown");
        }
        wire.notify("initialized")?;
        let thread = wire.thread()?;
        for _ in 0..ATTEMPTS_MAX {
            if let Sent::Into(turn) = wire.send_into(&thread, text, start)? {
                return Ok(turn);
            }
        }
        Err("codex_bridge_turn_moved")
    }

    impl Wire {
        fn open(socket: &Path) -> Result<Self, &'static str> {
            let stream = crate::spawn_gate::connect_unix_std(socket)
                .map_err(|_| "codex_bridge_not_running")?;
            let (uid, _) =
                nix::unistd::getpeereid(&stream).map_err(|_| "codex_bridge_peer_unknown")?;
            if uid.as_raw() != rustix::process::getuid().as_raw() {
                return Err("codex_bridge_peer_unknown");
            }
            let mut wire = Self {
                stream,
                buffer: Vec::new(),
                read: 0,
                deadline: Instant::now() + DEADLINE,
                next_id: 0,
            };
            wire.handshake()?;
            Ok(wire)
        }

        fn handshake(&mut self) -> Result<(), &'static str> {
            let mut key = [0u8; 16];
            getrandom::fill(&mut key).map_err(|_| "codex_bridge_unavailable")?;
            let key = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, key);
            let request = format!(
                "GET / HTTP/1.1\r\nHost: localhost\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\r\n"
            );
            self.write(request.as_bytes())?;
            let end = loop {
                if let Some(at) = self.buffer.windows(4).position(|w| w == b"\r\n\r\n") {
                    break at + 4;
                }
                if self.buffer.len() >= HEAD_BYTES_MAX {
                    return Err("codex_bridge_protocol_unknown");
                }
                self.fill(self.buffer.len() + 1)?;
            };
            if !self.buffer.starts_with(b"HTTP/1.1 101 ") {
                return Err("codex_bridge_protocol_unknown");
            }
            self.buffer.drain(..end);
            Ok(())
        }

        fn write(&mut self, bytes: &[u8]) -> Result<(), &'static str> {
            let left = self
                .deadline
                .checked_duration_since(Instant::now())
                .filter(|left| !left.is_zero())
                .ok_or("codex_bridge_slow")?;
            self.stream
                .set_write_timeout(Some(left))
                .map_err(|_| "codex_bridge_unavailable")?;
            self.stream
                .write_all(bytes)
                .map_err(|_| "codex_bridge_closed")
        }

        fn fill(&mut self, need: usize) -> Result<(), &'static str> {
            let mut chunk = [0u8; CHUNK_BYTES];
            while self.buffer.len() < need {
                let left = self
                    .deadline
                    .checked_duration_since(Instant::now())
                    .filter(|left| !left.is_zero())
                    .ok_or("codex_bridge_slow")?;
                self.stream
                    .set_read_timeout(Some(left))
                    .map_err(|_| "codex_bridge_unavailable")?;
                let count = match self.stream.read(&mut chunk) {
                    Ok(0) => return Err("codex_bridge_closed"),
                    Ok(count) => count,
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => return Err("codex_bridge_slow"),
                };
                self.read += count;
                if self.read > READ_BYTES_MAX {
                    return Err("codex_bridge_reply_too_large");
                }
                self.buffer.extend_from_slice(&chunk[..count]);
            }
            Ok(())
        }

        fn frame(&mut self, opcode: u8, payload: &[u8]) -> Result<(), &'static str> {
            assert!(opcode < 16 && payload.len() <= MESSAGE_BYTES_MAX);
            let mut mask = [0u8; 4];
            getrandom::fill(&mut mask).map_err(|_| "codex_bridge_unavailable")?;
            let mut out = Vec::with_capacity(payload.len() + 14);
            out.push(0x80 | opcode);
            if let Ok(short) = u8::try_from(payload.len())
                && short < 126
            {
                out.push(0x80 | short);
            } else if let Ok(middle) = u16::try_from(payload.len()) {
                out.push(0x80 | 126);
                out.extend(middle.to_be_bytes());
            } else {
                out.push(0x80 | 127);
                out.extend(
                    u64::try_from(payload.len())
                        .expect("usize fits u64")
                        .to_be_bytes(),
                );
            }
            out.extend(mask);
            out.extend(
                payload
                    .iter()
                    .enumerate()
                    .map(|(at, byte)| byte ^ mask[at % 4]),
            );
            self.write(&out)
        }

        /// Reads the next whole text message, answering pings and dropping any message
        /// over `MESSAGE_BYTES_MAX`, which the bridge never needs. Returns the message.
        fn message(&mut self) -> Result<Vec<u8>, &'static str> {
            let mut message = Vec::new();
            let mut dropped = false;
            loop {
                self.fill(2)?;
                let (head, second) = (self.buffer[0], self.buffer[1]);
                if second & 0x80 != 0 {
                    return Err("codex_bridge_protocol_unknown");
                }
                let (length, start) = match second & 0x7f {
                    126 => {
                        self.fill(4)?;
                        (
                            usize::from(u16::from_be_bytes([self.buffer[2], self.buffer[3]])),
                            4,
                        )
                    }
                    127 => {
                        self.fill(10)?;
                        let mut wide = [0u8; 8];
                        wide.copy_from_slice(&self.buffer[2..10]);
                        let length = usize::try_from(u64::from_be_bytes(wide))
                            .map_err(|_| "codex_bridge_reply_too_large")?;
                        (length, 10)
                    }
                    short => (usize::from(short), 2),
                };
                if length > READ_BYTES_MAX || (head & 0x08 != 0 && length > CONTROL_BYTES_MAX) {
                    return Err("codex_bridge_reply_too_large");
                }
                self.fill(start + length)?;
                let payload: Vec<u8> = self.buffer.drain(..start + length).skip(start).collect();
                let (last, opcode) = (head & 0x80 != 0, head & 0x0f);
                match opcode {
                    0x8 => return Err("codex_bridge_closed"),
                    0x9 => {
                        self.frame(0xa, &payload)?;
                        continue;
                    }
                    0xa => continue,
                    0x0..=0x2 => {}
                    _ => return Err("codex_bridge_protocol_unknown"),
                }
                dropped |= message.len() + payload.len() > MESSAGE_BYTES_MAX;
                if !dropped {
                    message.extend_from_slice(&payload);
                }
                if last {
                    if !dropped {
                        return Ok(message);
                    }
                    message.clear();
                    dropped = false;
                }
            }
        }

        fn notify(&mut self, method: &str) -> Result<(), &'static str> {
            let text = json!({"jsonrpc": "2.0", "method": method}).to_string();
            self.frame(0x1, text.as_bytes())
        }

        /// Sends one request and reads until its answer, passing over notifications and
        /// the server's own requests, which the agent's TUI answers. Takes the method and
        /// its parameters. Returns the result, or a plain reason for an error answer.
        fn call(&mut self, method: &str, params: Value) -> Result<Value, &'static str> {
            self.next_id += 1;
            let id = self.next_id;
            let text =
                json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}).to_string();
            if text.len() > MESSAGE_BYTES_MAX {
                return Err("codex_bridge_text_invalid");
            }
            self.frame(0x1, text.as_bytes())?;
            loop {
                let bytes = self.message()?;
                let Ok(reply) = serde_json::from_slice::<Value>(&bytes) else {
                    continue;
                };
                if reply.get("method").is_some()
                    || reply.get("id").and_then(Value::as_i64) != Some(id)
                {
                    continue;
                }
                if let Some(result) = reply.get("result") {
                    return Ok(result.clone());
                }
                return Err("codex_bridge_refused");
            }
        }

        /// Finds the thread the agent's TUI shows: of the loaded threads, the newest one
        /// that is neither a Codex sub-agent's nor ephemeral. Returns its id.
        fn thread(&mut self) -> Result<String, &'static str> {
            let loaded = self.call("thread/loaded/list", json!({"limit": THREADS_MAX}))?;
            let ids: Vec<String> = loaded
                .get("data")
                .and_then(Value::as_array)
                .ok_or("codex_bridge_protocol_unknown")?
                .iter()
                .filter_map(Value::as_str)
                .take(THREADS_MAX)
                .map(str::to_owned)
                .collect();
            let mut newest: Option<(i64, String)> = None;
            for id in ids {
                let read = self.call(
                    "thread/read",
                    json!({"threadId": id, "includeTurns": false}),
                )?;
                let thread = read.get("thread").ok_or("codex_bridge_protocol_unknown")?;
                let own = thread.get("parentThreadId").is_none_or(Value::is_null)
                    && !thread
                        .get("ephemeral")
                        .and_then(Value::as_bool)
                        .unwrap_or(false);
                let recent = ["recencyAt", "createdAt"]
                    .iter()
                    .find_map(|key| thread.get(*key).and_then(Value::as_i64))
                    .unwrap_or(0);
                if own && newest.as_ref().is_none_or(|(at, _)| recent > *at) {
                    newest = Some((recent, id));
                }
            }
            newest.map(|(_, id)| id).ok_or("codex_bridge_thread_absent")
        }

        /// Puts the text into the thread: a new turn while it is idle, when `start` allows
        /// one, a steer of the running turn while it works. Takes the thread id, the text
        /// and `start`. Returns the turn it went into once the server took it (the started
        /// turn's id from the answer, or the steered turn the server checked), or `Moved`
        /// when the turn changed under the request.
        fn send_into(
            &mut self,
            thread: &str,
            text: &str,
            start: bool,
        ) -> Result<Sent, &'static str> {
            let input = json!([{"type": "text", "text": text, "text_elements": []}]);
            let read = self.call(
                "thread/read",
                json!({"threadId": thread, "includeTurns": false}),
            )?;
            let status = read
                .pointer("/thread/status/type")
                .and_then(Value::as_str)
                .ok_or("codex_bridge_protocol_unknown")?;
            match status {
                "idle" if !start => Err("codex_bridge_thread_idle"),
                "idle" => {
                    match self.call("turn/start", json!({"threadId": thread, "input": input})) {
                        Ok(started) => Ok(Sent::Into(turn_id(
                            started.pointer("/turn/id").and_then(Value::as_str),
                        ))),
                        Err("codex_bridge_refused") => Ok(Sent::Moved),
                        Err(reason) => Err(reason),
                    }
                }
                "active" => {
                    let turns = self.call(
                        "thread/turns/list",
                        json!({"threadId": thread, "limit": 1, "itemsView": "notLoaded"}),
                    )?;
                    let Some(turn) = turns
                        .pointer("/data/0")
                        .filter(|turn| {
                            turn.get("status").and_then(Value::as_str) == Some("inProgress")
                        })
                        .and_then(|turn| turn.get("id"))
                        .and_then(Value::as_str)
                    else {
                        return Ok(Sent::Moved);
                    };
                    let steer = json!({"threadId": thread, "expectedTurnId": turn, "input": input});
                    match self.call("turn/steer", steer) {
                        Ok(_) => Ok(Sent::Into(turn_id(Some(turn)))),
                        Err("codex_bridge_refused") => Ok(Sent::Moved),
                        Err(reason) => Err(reason),
                    }
                }
                _ => Err("codex_bridge_thread_unavailable"),
            }
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod wire {
    use std::path::Path;

    pub(super) fn private_folder(_folder: &Path) -> Result<(), &'static str> {
        Err("codex_bridge_unsupported")
    }

    pub(super) fn deliver(
        _socket: &Path,
        _text: &str,
        _start: bool,
    ) -> Result<Option<String>, &'static str> {
        Err("codex_bridge_unsupported")
    }
}
