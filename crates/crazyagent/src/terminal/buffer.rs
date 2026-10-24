use super::{CHUNK_BYTES_MAX, STREAM_CHUNKS_MAX, VIEWER_BUFFER_BYTES_MAX};
use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex};
use tokio::sync::mpsc::error::TryRecvError;

#[derive(Debug, Default)]
struct Queue {
    chunks: VecDeque<Vec<u8>>,
    bytes: usize,
    skipped: u64,
    ended: bool,
    gone: bool,
}

#[derive(Debug, Default)]
struct Shared {
    queue: Mutex<Queue>,
    ready: Condvar,
    notify: tokio::sync::Notify,
}

/// The host's end of one viewer's output buffer.
/// `push` never blocks: past the bound the oldest output is dropped and counted.
/// Dropping it ends the viewer's stream once the viewer has read what is left.
#[derive(Debug)]
pub struct Feed {
    shared: Arc<Shared>,
}

/// The viewer's end of its output buffer, read in order.
/// After dropped output it first gives one marker chunk saying how many bytes were skipped.
/// Dropping it tells the host the viewer is gone.
#[derive(Debug)]
pub struct Chunks {
    shared: Arc<Shared>,
}

/// Opens one viewer's bounded output buffer.
/// Takes nothing; the bound is `VIEWER_BUFFER_BYTES_MAX` bytes and `STREAM_CHUNKS_MAX` chunks.
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

/// The chunk a viewer reads in place of output it fell too far behind to keep.
/// Takes the number of bytes dropped.
/// Returns a short line on a fresh line with attributes reset, within `CHUNK_BYTES_MAX`.
pub fn skipped_marker(bytes: u64) -> Vec<u8> {
    let marker = format!("\r\n\x1b[0m[{bytes} bytes of output skipped]\r\n").into_bytes();
    assert!(marker.len() <= CHUNK_BYTES_MAX);
    marker
}

impl Feed {
    /// Adds output for the viewer, split into chunks of at most `CHUNK_BYTES_MAX` bytes.
    /// Takes the bytes; the oldest chunks go once the buffer passes its bound.
    /// Returns false once the viewer is gone, so the host can forget it.
    pub fn push(&self, bytes: &[u8]) -> bool {
        let Ok(mut queue) = self.shared.queue.lock() else {
            return false;
        };
        if queue.gone {
            return false;
        }
        for chunk in bytes.chunks(CHUNK_BYTES_MAX) {
            queue.bytes += chunk.len();
            queue.chunks.push_back(chunk.to_vec());
        }
        while queue.bytes > VIEWER_BUFFER_BYTES_MAX || queue.chunks.len() > STREAM_CHUNKS_MAX {
            let Some(oldest) = queue.chunks.pop_front() else {
                break;
            };
            queue.bytes -= oldest.len();
            queue.skipped += oldest.len() as u64;
        }
        assert!(queue.bytes <= VIEWER_BUFFER_BYTES_MAX && queue.chunks.len() <= STREAM_CHUNKS_MAX);
        drop(queue);
        self.shared.ready.notify_all();
        self.shared.notify.notify_one();
        true
    }

    /// Tells whether the viewer still reads. Takes the feed.
    /// Returns false once the viewer dropped its end.
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

fn take(queue: &mut Queue) -> Option<Option<Vec<u8>>> {
    if queue.skipped > 0 {
        let skipped = std::mem::take(&mut queue.skipped);
        return Some(Some(skipped_marker(skipped)));
    }
    if let Some(chunk) = queue.chunks.pop_front() {
        queue.bytes -= chunk.len();
        return Some(Some(chunk));
    }
    queue.ended.then_some(None)
}

impl Chunks {
    /// Waits for the next chunk. Takes the viewer's end.
    /// Returns the chunk or a skipped marker, or none once the terminal ended and all was read.
    pub async fn recv(&mut self) -> Option<Vec<u8>> {
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

    /// Waits for the next chunk outside an async runtime. Takes the viewer's end.
    /// Returns the chunk or a skipped marker, or none once the terminal ended and all was read.
    pub fn blocking_recv(&mut self) -> Option<Vec<u8>> {
        let mut queue = self.shared.queue.lock().ok()?;
        loop {
            if let Some(next) = take(&mut queue) {
                return next;
            }
            queue = self.shared.ready.wait(queue).ok()?;
        }
    }

    /// Takes the next chunk without waiting. Takes the viewer's end.
    /// Returns the chunk or a skipped marker, `Empty` when nothing is buffered, or
    /// `Disconnected` once the terminal ended and all was read.
    pub fn try_recv(&mut self) -> Result<Vec<u8>, TryRecvError> {
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
