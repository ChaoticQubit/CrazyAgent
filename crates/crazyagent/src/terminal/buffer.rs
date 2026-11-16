use super::{CHUNK_BYTES_MAX, STREAM_CHUNKS_MAX, VIEWER_BUFFER_BYTES_MAX};
use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex};
use tokio::sync::mpsc::error::TryRecvError;

const _: () = assert!(VIEWER_BUFFER_BYTES_MAX / CHUNK_BYTES_MAX < STREAM_CHUNKS_MAX);

#[derive(Debug, Default)]
struct Queue {
    chunks: VecDeque<Vec<u8>>,
    bytes: usize,
    behind: bool,
    ended: bool,
    gone: bool,
}

#[derive(Debug, Default)]
struct Shared {
    queue: Mutex<Queue>,
    ready: Condvar,
    notify: tokio::sync::Notify,
}

/// One read from a viewer's buffer: output in order, or `Behind` once output was lost
/// because the viewer fell more than `VIEWER_BUFFER_BYTES_MAX` behind or the host could not
/// read it. After `Behind` the stream ends, and the viewer attaches again to draw the screen
/// as it is now; it never reads a gap or a line about one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Chunk {
    Output(Vec<u8>),
    Behind,
}

/// The host's end of one viewer's output buffer.
/// `push` never blocks and adds small writes to the last queued chunk; past the byte bound
/// the queued output is dropped and the viewer reads `Chunk::Behind`.
/// Dropping it ends the viewer's stream once the viewer has read what is left.
#[derive(Debug)]
pub struct Feed {
    shared: Arc<Shared>,
}

/// The viewer's end of its output buffer, read in order, then `Chunk::Behind` if output
/// was lost, then the end. Dropping it tells the host the viewer is gone.
#[derive(Debug)]
pub struct Chunks {
    shared: Arc<Shared>,
}

/// Opens one viewer's bounded output buffer.
/// Takes nothing; the bound is `VIEWER_BUFFER_BYTES_MAX` bytes.
/// Returns the host's end and the viewer's end.
pub fn buffer() -> (Feed, Chunks) {
    let shared = Arc::new(Shared::default());
    (
        Feed {
            shared: shared.clone(),
        },
        Chunks { shared },
    )
}

fn fall_behind(queue: &mut Queue) {
    queue.chunks.clear();
    queue.bytes = 0;
    queue.behind = true;
}

impl Feed {
    /// Adds output for the viewer: it fills the last queued chunk up to `CHUNK_BYTES_MAX`
    /// before it starts another, so many small writes take few chunks. Takes the bytes;
    /// past the byte bound the queued output goes and the viewer is told it is behind.
    /// Returns false once the viewer is gone, so the host can forget it.
    pub fn push(&self, bytes: &[u8]) -> bool {
        let Ok(mut queue) = self.shared.queue.lock() else {
            return false;
        };
        if queue.gone {
            return false;
        }
        if !queue.behind {
            let mut rest = bytes;
            while !rest.is_empty() {
                let room = match queue.chunks.back() {
                    Some(last) if last.len() < CHUNK_BYTES_MAX => CHUNK_BYTES_MAX - last.len(),
                    _ => {
                        queue.chunks.push_back(Vec::new());
                        CHUNK_BYTES_MAX
                    }
                };
                let (head, tail) = rest.split_at(room.min(rest.len()));
                queue
                    .chunks
                    .back_mut()
                    .expect("a chunk with room is queued")
                    .extend_from_slice(head);
                queue.bytes += head.len();
                rest = tail;
            }
            if queue.bytes > VIEWER_BUFFER_BYTES_MAX {
                fall_behind(&mut queue);
            }
        }
        assert!(queue.bytes <= VIEWER_BUFFER_BYTES_MAX && queue.chunks.len() <= STREAM_CHUNKS_MAX);
        assert!(!queue.behind || queue.chunks.is_empty());
        drop(queue);
        self.shared.ready.notify_all();
        self.shared.notify.notify_one();
        true
    }

    /// Tells the viewer that output the host could not read was lost, such as a control
    /// line longer than the host keeps, so it reads `Chunk::Behind` next. Takes the feed.
    /// Returns false once the viewer is gone, so the host can forget it.
    pub fn lost(&self) -> bool {
        let Ok(mut queue) = self.shared.queue.lock() else {
            return false;
        };
        if queue.gone {
            return false;
        }
        fall_behind(&mut queue);
        drop(queue);
        self.shared.ready.notify_all();
        self.shared.notify.notify_one();
        true
    }

    /// Tells whether the viewer still reads. Takes the feed.
    /// Returns false once the viewer dropped its end or read that it is behind.
    pub fn open(&self) -> bool {
        self.shared.queue.lock().is_ok_and(|queue| !queue.gone)
    }
}

impl Drop for Feed {
    fn drop(&mut self) {
        if let Ok(mut queue) = self.shared.queue.lock() {
            queue.ended = true;
        }
        self.shared.ready.notify_all();
        self.shared.notify.notify_one();
    }
}

impl Drop for Chunks {
    fn drop(&mut self) {
        if let Ok(mut queue) = self.shared.queue.lock() {
            queue.gone = true;
            queue.chunks.clear();
            queue.bytes = 0;
        }
    }
}

fn take(queue: &mut Queue) -> Option<Option<Chunk>> {
    if queue.behind {
        assert!(queue.chunks.is_empty() && queue.bytes == 0);
        queue.behind = false;
        queue.gone = true;
        queue.ended = true;
        return Some(Some(Chunk::Behind));
    }
    if let Some(chunk) = queue.chunks.pop_front() {
        queue.bytes -= chunk.len();
        assert!(!chunk.is_empty() && chunk.len() <= CHUNK_BYTES_MAX);
        return Some(Some(Chunk::Output(chunk)));
    }
    queue.ended.then_some(None)
}

impl Chunks {
    /// Waits for the next read. Takes the viewer's end.
    /// Returns output, `Chunk::Behind`, or none once the terminal ended and all was read.
    pub async fn recv(&mut self) -> Option<Chunk> {
        loop {
            {
                let mut queue = self.shared.queue.lock().ok()?;
                if let Some(next) = take(&mut queue) {
                    return next;
                }
            }
            self.shared.notify.notified().await;
        }
    }

    /// Waits for the next read outside an async runtime. Takes the viewer's end.
    /// Returns output, `Chunk::Behind`, or none once the terminal ended and all was read.
    pub fn blocking_recv(&mut self) -> Option<Chunk> {
        let mut queue = self.shared.queue.lock().ok()?;
        loop {
            if let Some(next) = take(&mut queue) {
                return next;
            }
            queue = self.shared.ready.wait(queue).ok()?;
        }
    }

    /// Takes the next read without waiting. Takes the viewer's end.
    /// Returns output or `Chunk::Behind`, `Empty` when nothing is buffered, or
    /// `Disconnected` once the terminal ended and all was read.
    pub fn try_recv(&mut self) -> Result<Chunk, TryRecvError> {
        let mut queue = self
            .shared
            .queue
            .lock()
            .map_err(|_| TryRecvError::Disconnected)?;
        match take(&mut queue) {
            Some(Some(chunk)) => Ok(chunk),
            Some(None) => Err(TryRecvError::Disconnected),
            None => Err(TryRecvError::Empty),
        }
    }
}

#[cfg(test)]
#[path = "buffer/tests.rs"]
mod tests;
