use super::super::launch::{LaunchError, failure};
use super::super::transcript::{EVENTS_PER_READ_MAX, TranscriptEvent};
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// How long the history file must stay the same length before a turn that has reply text
/// counts as finished, when no next prompt has ended it first.
pub const QUIET: Duration = Duration::from_secs(2);
/// The most bytes of one line read as text; a longer line is passed over.
pub const LINE_BYTES_MAX: usize = 64 * 1024;
/// The most bytes one read takes from the file; the rest waits for the next read.
pub const READ_BYTES_MAX: usize = 1024 * 1024;
/// The code of a record path that is a link, a folder or anything but a plain file.
pub const UNSAFE: &str = "record_history_unsafe";

pub(super) const SESSION_HEADER: &str = "# aider chat started at";
pub(super) const USER_HEADING: &str = "####";

/// Where the current turn stands: none open, a prompt with no reply yet, or reply text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Turn {
    Closed,
    Prompt,
    Reply,
}

/// Follows a Markdown chat history file as a harness appends to it, aider's
/// `.aider.chat.history.md` shape: a user heading line (`#### `) starts a turn, and further
/// heading lines before any reply belong to the same prompt; the plain text lines after it
/// are the reply, one fact per line; a quoted line (`>`) is tool output; a session header
/// line (`# aider chat started at`) interrupts an open turn. A turn ends at the next user
/// heading, or once the file has stayed the same for [`QUIET`] after reply text. Holds the
/// folder the engine holds and the file's path below it, the offset of the last whole line
/// read, the bytes read past it, the unfinished last line, whether a line past the bound is
/// being passed over, and the turn.
pub struct MarkdownHistory {
    base: PathBuf,
    below: PathBuf,
    offset: u64,
    position: u64,
    held: Vec<u8>,
    skipping: bool,
    changed: Instant,
    turn: Turn,
}

impl MarkdownHistory {
    /// A reader that starts at `offset`. Inputs: the folder the engine holds, the history
    /// file's path below it as plain names, the offset already read, 0 for none, and the time
    /// now. Output: the reader.
    pub fn new(base: PathBuf, below: PathBuf, offset: u64, now: Instant) -> Self {
        Self {
            base,
            below,
            offset,
            position: offset,
            held: Vec::new(),
            skipping: false,
            changed: now,
            turn: Turn::Closed,
        }
    }

    /// The offset read to, the position a resumed reader starts at. Output: it.
    pub fn offset(&self) -> u64 {
        self.offset
    }

    /// Reads the facts of the lines finished since the last read, at most
    /// [`EVENTS_PER_READ_MAX`], and ends a turn the file has been quiet on for [`QUIET`]. The
    /// file opens one part at a time below the held folder, following no link. Input: the
    /// time now. Output: the facts, none while the file does not exist yet, or [`UNSAFE`] for
    /// a link in any part below the folder, a folder or anything but a plain file.
    pub fn read(&mut self, now: Instant) -> Result<Vec<TranscriptEvent>, LaunchError> {
        let mut file = match crate::platform::open_plain_beneath(&self.base, &self.below) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Vec::new());
            }
            Err(_) => return Err(failure(UNSAFE)),
        };
        let metadata = file.metadata().map_err(|_| failure(UNSAFE))?;
        if !metadata.file_type().is_file() {
            return Err(failure(UNSAFE));
        }
        let length = metadata.len();
        if length < self.position {
            let base = std::mem::take(&mut self.base);
            let below = std::mem::take(&mut self.below);
            *self = Self::new(base, below, 0, now);
        }
        let mut events = Vec::new();
        if length > self.position {
            let mut chunk = Vec::new();
            file.seek(SeekFrom::Start(self.position))
                .map_err(|_| failure(UNSAFE))?;
            let left = length - self.position;
            file.take(left.min(u64::try_from(READ_BYTES_MAX).unwrap_or(u64::MAX)))
                .read_to_end(&mut chunk)
                .map_err(|_| failure(UNSAFE))?;
            assert!(chunk.len() <= READ_BYTES_MAX);
            if !chunk.is_empty() {
                self.changed = now;
            }
            self.take(&chunk, &mut events);
        }
        if self.turn == Turn::Reply && now.saturating_duration_since(self.changed) >= QUIET {
            events.push(TranscriptEvent::TurnFinished { success: true });
            self.turn = Turn::Closed;
        }
        assert!(self.offset <= self.position && self.held.len() <= LINE_BYTES_MAX);
        assert!(events.len() <= EVENTS_PER_READ_MAX);
        Ok(events)
    }

    fn take(&mut self, chunk: &[u8], events: &mut Vec<TranscriptEvent>) {
        for piece in chunk.split_inclusive(|byte| *byte == b'\n') {
            if events.len() + 3 > EVENTS_PER_READ_MAX {
                assert!(self.offset == self.position && self.held.is_empty());
                break;
            }
            self.position += u64::try_from(piece.len()).unwrap_or(u64::MAX);
            let (content, whole) = match piece.strip_suffix(b"\n") {
                Some(content) => (content, true),
                None => (piece, false),
            };
            if self.skipping || self.held.len() + content.len() > LINE_BYTES_MAX {
                self.skipping = !whole;
                self.held.clear();
            } else {
                self.held.extend_from_slice(content);
                if whole {
                    let line = std::mem::take(&mut self.held);
                    self.line(&line, events);
                }
            }
            if whole {
                self.offset = self.position;
            }
        }
    }

    fn line(&mut self, line: &[u8], events: &mut Vec<TranscriptEvent>) {
        let text = String::from_utf8_lossy(line);
        let text = text.trim_end_matches('\r');
        if text.starts_with(SESSION_HEADER) {
            if self.turn != Turn::Closed {
                events.push(TranscriptEvent::Interrupted);
            }
            self.turn = Turn::Closed;
        } else if text == USER_HEADING || text.starts_with("#### ") {
            match self.turn {
                Turn::Closed => events.push(TranscriptEvent::TurnStarted),
                Turn::Prompt => {}
                Turn::Reply => events.extend([
                    TranscriptEvent::TurnFinished { success: true },
                    TranscriptEvent::TurnStarted,
                ]),
            }
            self.turn = Turn::Prompt;
        } else if !text.starts_with('>') && !text.trim().is_empty() && self.turn != Turn::Closed {
            events.push(TranscriptEvent::Text {
                text: text.to_owned(),
            });
            self.turn = Turn::Reply;
        }
    }
}

#[cfg(test)]
#[path = "markdown_history_tests.rs"]
mod tests;
