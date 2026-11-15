use super::super::transcript::TranscriptEvent;
use std::time::{Duration, Instant};

/// How long the screen must stay the same, with the prompt mark back, before a turn ends.
pub const QUIET: Duration = Duration::from_millis(1500);
/// The most bytes of one screen capture read; a longer one is read from its end.
pub const SCREEN_BYTES_MAX: usize = 1024 * 1024;
/// The most bytes of reply text one turn gives; longer text is cut at a character boundary.
pub const SCREEN_TEXT_BYTES_MAX: usize = 64 * 1024;
/// The most bytes of a manifest's prompt mark.
pub const MARK_BYTES_MAX: usize = 64;

/// Reads a harness's turns from its terminal screen, for a harness with no readable record:
/// a turn the session started ends once the screen has changed, then stayed the same for
/// [`QUIET`] with its last line starting with the prompt mark. The reply is the text between
/// the line the prompt was typed on and that last line. It gives no tool names and can cut
/// or repeat text. Holds the mark, the last screen, when it last changed and the open turn.
pub struct ScreenReader {
    mark: String,
    last: Vec<u8>,
    changed: Option<Instant>,
    turn: Option<bool>,
}

impl ScreenReader {
    /// A reader for one prompt mark. Input: the manifest's `prompt_mark`, 1 to
    /// [`MARK_BYTES_MAX`] bytes. Output: the reader.
    pub fn new(mark: &str) -> Self {
        Self {
            mark: mark.to_owned(),
            last: Vec::new(),
            changed: None,
            turn: None,
        }
    }

    /// Opens a turn once the session has typed a prompt. Input: the time now. Output: the
    /// turn start fact.
    pub fn turn_started(&mut self, now: Instant) -> Vec<TranscriptEvent> {
        let _ = now;
        Vec::new()
    }

    /// Reads one capture of the screen. Inputs: the plain screen text and the time now.
    /// Output: the reply and the turn end once the open turn is over, otherwise none.
    pub fn observe(&mut self, screen: &[u8], now: Instant) -> Vec<TranscriptEvent> {
        let _ = (&self.mark, &self.last, self.changed, self.turn, screen, now);
        Vec::new()
    }
}

#[cfg(test)]
#[path = "screen_tests.rs"]
mod tests;
