use super::*;
use crate::protocol::{Topic, WatchReply, encode_frame};
use crate::storage::{Changes, changes};
use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::sync::PoisonError;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

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
/// How often the sampler reads the store's `data_version`, so a commit wakes a watch within
/// a second (v2.5 plan row S3a).
const SAMPLE_EVERY: Duration = Duration::from_millis(250);
/// A sampler's first mark is below this, so a JavaScript client reads every mark exactly.
const MARK_SEED_MAX: u64 = 1 << 52;
const _: () = assert!(WATCH_TIMEOUT.as_millis() > WAIT_MS_MAX as u128);
const _: () = assert!(REQUEST_BYTES_MAX <= crate::protocol::FRAME_BYTES_MAX);

#[cfg(all(test, unix))]
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

/// The owner server's watches: how many wait now, at most [`WATCHES_MAX`], and the sampler
/// of the engine store they wait on, started by the first watch.
pub(super) struct Watches {
    waiting: AtomicUsize,
    sampler: Mutex<Option<Sampler>>,
}

impl Watches {
    /// No watch waiting and no sampler. Output: the watches.
    pub(super) fn new() -> Self {
        Self {
            waiting: AtomicUsize::new(0),
            sampler: Mutex::new(None),
        }
    }

    /// The mark's receiver. The first watch starts the sampler, and a watch after it stopped
    /// on a read error starts a new one from a new seed, so every older mark differs. Input:
    /// the store's path. Output: the receiver, or `watch_unavailable`.
    fn subscribe(&self, database: &Path) -> Result<tokio::sync::watch::Receiver<u64>, EngineError> {
        let mut sampler = self.sampler.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(running) = sampler.as_ref().filter(|it| !it.thread.is_finished()) {
            return Ok(running.mark.subscribe());
        }
        let started = Sampler::start(database)?;
        let marks = started.mark.subscribe();
        *sampler = Some(started);
        Ok(marks)
    }
}

/// A thread reading the engine store's `PRAGMA data_version` on a read-only connection of
/// its own every [`SAMPLE_EVERY`]. The value changes once any other connection or process
/// commits, the storage actor's owner requests included, and each change moves the mark by
/// one. It stops when dropped or on a read error.
struct Sampler {
    mark: Arc<tokio::sync::watch::Sender<u64>>,
    stop: Arc<AtomicBool>,
    thread: std::thread::JoinHandle<()>,
}

impl Sampler {
    /// Opens the store and starts sampling it, the mark seeded from the clock. Input: the
    /// store's path. Output: the sampler, or `watch_unavailable`.
    fn start(database: &Path) -> Result<Self, EngineError> {
        let db = open(database)?;
        let first = data_version(&db).map_err(|_| unavailable())?;
        let seed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_nanos() as u64)
            % MARK_SEED_MAX;
        let mark = Arc::new(tokio::sync::watch::channel(seed).0);
        let stop = Arc::new(AtomicBool::new(false));
        let (moves, stops) = (Arc::clone(&mark), Arc::clone(&stop));
        let thread = std::thread::Builder::new()
            .name("owner-watch".into())
            .spawn(move || sample(&db, first, &moves, &stops))
            .map_err(|_| unavailable())?;
        Ok(Self { mark, stop, thread })
    }
}

impl Drop for Sampler {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

/// The sampler's loop: moves the mark on each change of `data_version`, and once more on a
/// read error before it stops, so no waiting watch misses a commit. Inputs: the connection,
/// the value first read, the mark and the stop flag. Output: none.
fn sample(
    db: &Connection,
    mut last: i64,
    mark: &tokio::sync::watch::Sender<u64>,
    stop: &AtomicBool,
) {
    while !stop.load(Ordering::SeqCst) {
        std::thread::sleep(SAMPLE_EVERY);
        let now = data_version(db);
        if now.as_ref().ok() != Some(&last) {
            mark.send_modify(|mark| *mark = mark.wrapping_add(1));
        }
        match now {
            Ok(now) => last = now,
            Err(_) => return,
        }
    }
}

/// Opens the engine store read-only, waiting at most one sample behind a writer. Input: the
/// path. Output: the connection, or `watch_unavailable`.
fn open(database: &Path) -> Result<Connection, EngineError> {
    let db = Connection::open_with_flags(
        database,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|_| unavailable())?;
    db.busy_timeout(SAMPLE_EVERY).map_err(|_| unavailable())?;
    Ok(db)
}

/// The connection's `PRAGMA data_version`. Input: the connection. Output: the value.
fn data_version(db: &Connection) -> rusqlite::Result<i64> {
    db.query_row("PRAGMA data_version", [], |row| row.get(0))
}

fn unavailable() -> EngineError {
    failure("watch_unavailable")
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

/// Serves one admitted watch request: waits for a commit and writes the reply while holding
/// the owner barrier taken for the last owner check, so a revocation cannot land between
/// that check and the write, which is capped as every barrier holder's is. A refusal, in
/// the reply's shape, tells nothing and is written without it. Inputs: the stream, the
/// engine, the server authority, the handshake packet, its admission and the request body.
/// Output: Ok once the reply is written.
pub(super) async fn serve(
    stream: &mut UnixStream,
    engine: &Arc<Engine>,
    authority: &Arc<Authority>,
    packet: &[u8; 37],
    admission: &Admission,
    bytes: &[u8],
) -> Result<(), EngineError> {
    let (reply, _barrier) = match handle(engine, authority, packet, admission, bytes).await {
        Ok((reply, barrier)) => (reply, Some(barrier)),
        Err(error) => (WatchReply::Rejected { code: error.0 }, None),
    };
    let frame = encode_frame(&reply).map_err(|_| unknown())?;
    write_reply(stream, &frame).await
}

/// Checks the owner, waits, and checks the owner again, so an owner revoked while it waited
/// is told nothing. Inputs: as [`serve`]. Output: the reply with the barrier of the last
/// check, or the refusal.
async fn handle<'e>(
    engine: &'e Arc<Engine>,
    authority: &Arc<Authority>,
    packet: &[u8; 37],
    admission: &Admission,
    bytes: &[u8],
) -> Result<(WatchReply, tokio::sync::RwLockWriteGuard<'e, ()>), EngineError> {
    let request = parse(bytes)?;
    drop(admitted(engine, authority, packet, admission).await?);
    let database = super::changes::engine_root(authority)?.join(DATABASE);
    let reply = wait(&authority.watches, &database, request).await?;
    let barrier = admitted(engine, authority, packet, admission).await?;
    Ok((reply, barrier))
}

/// Whether the handshake still names the admitted owner, read behind the owner barrier.
/// Inputs: the engine, the authority, the packet and the admission. Output: the barrier,
/// held by the caller for as long as what it does must not overlap a revocation, the
/// handshake's own refusal (`owner_authentication_failed` once the owner is revoked), or
/// `owner_grant_stale_or_revoked` for a handshake that now names another admission.
async fn admitted<'e>(
    engine: &'e Arc<Engine>,
    authority: &Arc<Authority>,
    packet: &[u8; 37],
    admission: &Admission,
) -> Result<tokio::sync::RwLockWriteGuard<'e, ()>, EngineError> {
    let barrier = engine.owner_egress.write().await;
    if &authority.authenticate(packet)? != admission {
        return Err(failure("owner_grant_stale_or_revoked"));
    }
    Ok(barrier)
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
/// wait. It answers at once with `Refresh` for a pruned or ahead cursor, with the topics of
/// the events after the cursor, and with `Unknown` among them when the mark moved, since any
/// commit may have changed what the client shows. Inputs: the watches, the store's path and
/// the request. Output: the reply, or `watch_busy` past [`WATCHES_MAX`] or
/// `watch_unavailable`.
async fn wait(
    watches: &Watches,
    database: &Path,
    request: Request,
) -> Result<WatchReply, EngineError> {
    let _waiting = Waiting::take(&watches.waiting).ok_or_else(|| failure("watch_busy"))?;
    let mut marks = watches.subscribe(database)?;
    let deadline = Instant::now() + Duration::from_millis(u64::from(request.wait_ms));
    loop {
        let mark = *marks.borrow_and_update();
        let (path, after) = (database.to_path_buf(), request.after.clone());
        let read = tokio::task::spawn_blocking(move || changes(&mut open(&path)?, &after))
            .await
            .map_err(|_| unavailable())??;
        let (cursor, logged) = match read {
            Changes::Refresh { cursor } => return Ok(WatchReply::Refresh { cursor, mark }),
            Changes::Logged { cursor, topics } => (cursor, topics),
        };
        let mut topics: BTreeSet<Topic> = logged.into_iter().collect();
        if request.mark != Some(mark) {
            topics.insert(Topic::Unknown);
        }
        if !topics.is_empty() {
            let topics = topics.into_iter().collect();
            return Ok(WatchReply::Changed {
                cursor,
                mark,
                topics,
            });
        }
        match tokio::time::timeout_at(deadline, marks.changed()).await {
            Err(_) => return Ok(WatchReply::Quiet { cursor, mark }),
            Ok(Err(_)) => return Err(unavailable()),
            Ok(Ok(())) => {}
        }
    }
}
