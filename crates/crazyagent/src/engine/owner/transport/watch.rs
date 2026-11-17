use super::*;
use crate::protocol::{WatchReply, encode_frame};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicUsize, Ordering};

/// The longest a watch waits for a commit, in ms (v2.5 plan section 6, held connections).
const WAIT_MS_MAX: u32 = 25_000;
/// The most a watch request is served for: its longest wait and the reads and owner checks
/// around it.
pub(super) const WATCH_TIMEOUT: Duration = Duration::from_secs(30);
/// The most watches waiting at once: one each for the TUI and the desktop, with room for a
/// window that reopens before its old watch ends. Past it a watch is refused, never queued.
const WATCHES_MAX: usize = 4;
/// The most bytes a watch request body may hold.
const REQUEST_BYTES_MAX: usize = 256;
/// The engine store's file in the engine root.
const DATABASE: &str = "engine.db";
const _: () = assert!(WATCH_TIMEOUT.as_millis() > WAIT_MS_MAX as u128);
const _: () = assert!(REQUEST_BYTES_MAX <= crate::protocol::FRAME_BYTES_MAX);

#[cfg(test)]
#[path = "watch_tests.rs"]
mod tests;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    watch: Request,
}

/// One watch: the cursor of the last event the client read, the mark of its last reply
/// (none for a client that has not watched yet) and how long to wait, in ms.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    after: Cursor,
    mark: Option<u64>,
    wait_ms: u32,
}

/// The owner server's watches: how many wait now, at most [`WATCHES_MAX`].
pub(super) struct Watches {
    waiting: AtomicUsize,
}

impl Watches {
    /// No watch waiting. Output: the watches.
    pub(super) fn new() -> Self {
        Self {
            waiting: AtomicUsize::new(0),
        }
    }
}

/// A watch counted while it waits; dropping it gives its place back.
struct Waiting<'a>(&'a AtomicUsize);

impl<'a> Waiting<'a> {
    /// Counts one more watch. Input: the count. Output: the guard, none when
    /// [`WATCHES_MAX`] already wait.
    fn take(count: &'a AtomicUsize) -> Option<Self> {
        let before = count
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |waiting| {
                (waiting < WATCHES_MAX).then_some(waiting + 1)
            })
            .ok()?;
        assert!(before < WATCHES_MAX);
        Some(Self(count))
    }
}

impl Drop for Waiting<'_> {
    fn drop(&mut self) {
        let before = self.0.fetch_sub(1, Ordering::SeqCst);
        assert!(before > 0 && before <= WATCHES_MAX);
    }
}

fn unknown() -> EngineError {
    failure("watch_outcome_unknown")
}

impl OwnerClient {
    /// Waits for the engine to commit something after the last watch (v2.5 S3a). Inputs:
    /// the cursor and mark of the last reply (cursor 0 and no mark for the first watch) and
    /// the longest wait in ms, at most [`WAIT_MS_MAX`]. Output: the reply, never `Rejected`,
    /// whose code is returned as the error instead.
    pub async fn watch(
        &self,
        after: Cursor,
        mark: Option<u64>,
        wait_ms: u32,
    ) -> Result<WatchReply, EngineError> {
        let request = Request {
            after,
            mark,
            wait_ms,
        };
        let limit = Duration::from_millis(u64::from(wait_ms.min(WAIT_MS_MAX))) + self.timeout;
        tokio::time::timeout(limit, async {
            let frame = encode_frame(&Envelope { watch: request })
                .map_err(|_| failure("watch_request_bounds"))?;
            self.root.verify()?;
            if self.root.socket_identity()? != self.socket {
                return Err(failure("owner_socket_replaced"));
            }
            let mut stream = crate::spawn_gate::connect_unix(self.root.path.join(SOCKET))
                .await
                .map_err(|_| failure("watch_unavailable"))?;
            verify_peer(&stream)?;
            let mut packet = [0; 37];
            packet[..4].copy_from_slice(b"CAO1");
            packet[4] = match self.kind {
                OwnerClientKind::Tui => 0,
                OwnerClientKind::Desktop => 1,
            };
            packet[5..].copy_from_slice(&self.token);
            stream
                .write_all(&packet)
                .await
                .map_err(|_| failure("watch_unavailable"))?;
            stream
                .write_all(&frame)
                .await
                .map_err(|_| failure("watch_unavailable"))?;
            let frame = read_frame(&mut stream).await.map_err(|_| unknown())?;
            match serde_json::from_slice(&frame[4..])
                .map_err(|error| self.skewed_reply(&error, Some("watch"), unknown))?
            {
                WatchReply::Rejected { code } => Err(EngineError(code)),
                reply => Ok(reply),
            }
        })
        .await
        .map_err(|_| unknown())?
    }
}

/// Encodes this protocol's refusal of a request the engine did not run.
/// Input: the refusal code. Output: the reply frame.
pub(super) fn refusal(code: &str) -> Result<Vec<u8>, EngineError> {
    encode_frame(&WatchReply::Rejected { code: code.into() }).map_err(|_| unknown())
}

/// Serves one admitted watch request: waits for a commit and writes the reply, a refusal in
/// the reply's shape on any error. Inputs: the stream, the engine, the server authority, the
/// handshake packet, its admission and the request body. Output: Ok once the reply is
/// written.
pub(super) async fn serve(
    stream: &mut UnixStream,
    engine: &Arc<Engine>,
    authority: &Arc<Authority>,
    packet: &[u8; 37],
    admission: &Admission,
    bytes: &[u8],
) -> Result<(), EngineError> {
    let reply = match handle(engine, authority, packet, admission, bytes).await {
        Ok(reply) => reply,
        Err(error) => WatchReply::Rejected { code: error.0 },
    };
    let frame = encode_frame(&reply).map_err(|_| unknown())?;
    stream.write_all(&frame).await.map_err(io_error)
}

/// Checks the owner, waits, and checks the owner again, so an owner revoked while it waited
/// is told nothing. Inputs: as [`serve`]. Output: the reply, or the refusal.
async fn handle(
    engine: &Arc<Engine>,
    authority: &Arc<Authority>,
    packet: &[u8; 37],
    admission: &Admission,
    bytes: &[u8],
) -> Result<WatchReply, EngineError> {
    let request = parse(bytes)?;
    admitted(engine, authority, packet, admission).await?;
    let database = super::changes::engine_root(authority)?.join(DATABASE);
    let reply = wait(&authority.watches, &database, request).await?;
    admitted(engine, authority, packet, admission).await?;
    Ok(reply)
}

/// Whether the handshake still names the admitted owner, read behind the owner barrier.
/// Inputs: the engine, the authority, the packet and the admission. Output: Ok, or
/// `owner_grant_stale_or_revoked`.
async fn admitted(
    engine: &Arc<Engine>,
    authority: &Arc<Authority>,
    packet: &[u8; 37],
    admission: &Admission,
) -> Result<(), EngineError> {
    let _barrier = engine.owner_egress.write().await;
    if &authority.authenticate(packet)? != admission {
        return Err(failure("owner_grant_stale_or_revoked"));
    }
    Ok(())
}

/// Reads a watch request body. Input: the bytes. Output: the request, or
/// `watch_request_invalid` for one too long, malformed or waiting past [`WAIT_MS_MAX`].
fn parse(bytes: &[u8]) -> Result<Request, EngineError> {
    let invalid = || failure("watch_request_invalid");
    if bytes.len() > REQUEST_BYTES_MAX {
        return Err(invalid());
    }
    let envelope: Envelope = serde_json::from_slice(bytes).map_err(|_| invalid())?;
    if envelope.watch.wait_ms > WAIT_MS_MAX {
        return Err(invalid());
    }
    Ok(envelope.watch)
}

/// Waits for a commit to the engine store after the request's cursor or mark, at most its
/// wait. Inputs: the watches, the store's path and the request. Output: the reply, or
/// `watch_busy` past [`WATCHES_MAX`] or `watch_unavailable`.
async fn wait(
    watches: &Watches,
    _database: &Path,
    request: Request,
) -> Result<WatchReply, EngineError> {
    let _waiting = Waiting::take(&watches.waiting);
    Ok(WatchReply::Quiet {
        cursor: request.after,
        mark: 0,
    })
}
