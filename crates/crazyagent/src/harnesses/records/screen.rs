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
/// a turn the session started ends once the screen has changed since the capture before
/// it, then stayed the same for [`QUIET`] with its last line an empty prompt mark (the mark
/// with nothing typed after it). The reply is the text between the last line before that
/// one that starts with the mark, where the prompt was typed, and the empty mark. It gives
/// no tool names and can cut or repeat text. A turn opened before any capture is measured
/// from the first capture after it. Holds the mark, the last screen, when it last changed
/// and the open turn, with whether the screen has changed since it started.
pub struct ScreenReader {
    mark: String,
    last: Vec<u8>,
    changed: Option<Instant>,
    turn: Option<bool>,
}

impl ScreenReader {
    /// A reader for one prompt mark. Input: the manifest's `prompt_mark`. Output: the
    /// reader, or none for a mark that is empty, only spaces or longer than
    /// [`MARK_BYTES_MAX`] bytes.
    pub fn new(mark: &str) -> Option<Self> {
        if mark.trim_end().is_empty() || mark.len() > MARK_BYTES_MAX {
            return None;
        }
        Some(Self {
            mark: mark.to_owned(),
            last: Vec::new(),
            changed: None,
            turn: None,
        })
    }

    /// Opens a turn once the session has typed a prompt. Input: the time now. Output: the
    /// turn start fact.
    pub fn turn_started(&mut self, now: Instant) -> Vec<TranscriptEvent> {
        let _ = now;
        self.turn = Some(false);
        vec![TranscriptEvent::TurnStarted]
    }

    /// Reads one capture of the screen. Inputs: the plain screen text and the time now.
    /// Output: the reply and the turn end once the open turn is over, otherwise none.
    pub fn observe(&mut self, screen: &[u8], now: Instant) -> Vec<TranscriptEvent> {
        let screen = tail(screen);
        assert!(screen.len() <= SCREEN_BYTES_MAX);
        if screen != self.last.as_slice() {
            if self.turn == Some(false) && !self.last.is_empty() {
                self.turn = Some(true);
            }
            self.last = screen.to_vec();
            self.changed = Some(now);
            return Vec::new();
        }
        let quiet = self
            .changed
            .is_some_and(|at| now.saturating_duration_since(at) >= QUIET);
        if self.turn != Some(true) || !quiet {
            return Vec::new();
        }
        let Some(reply) = self.reply() else {
            return Vec::new();
        };
        self.turn = None;
        let mut events = Vec::new();
        if !reply.is_empty() {
            events.push(TranscriptEvent::Text { text: reply });
        }
        events.push(TranscriptEvent::TurnFinished { success: true });
        events
    }

    fn reply(&self) -> Option<String> {
        let text = String::from_utf8_lossy(&self.last);
        let lines: Vec<&str> = text.lines().map(str::trim_end).collect();
        let idle = lines.iter().rposition(|line| !line.is_empty())?;
        if lines[idle] != self.mark.trim_end() {
            return None;
        }
        let typed = lines[..idle]
            .iter()
            .rposition(|line| line.starts_with(&self.mark) || *line == self.mark.trim_end())
            .map_or(0, |index| index + 1);
        let shown = lines[typed..idle].join("\n");
        let mut reply = shown.trim_matches('\n').to_owned();
        if reply.len() > SCREEN_TEXT_BYTES_MAX {
            let mut end = SCREEN_TEXT_BYTES_MAX;
            while !reply.is_char_boundary(end) {
                end -= 1;
            }
            reply.truncate(end);
        }
        assert!(reply.len() <= SCREEN_TEXT_BYTES_MAX);
        Some(reply)
    }
}

/// The last [`SCREEN_BYTES_MAX`] bytes of a capture, from the first whole line in them.
/// Input: the capture. Output: the part read.
fn tail(screen: &[u8]) -> &[u8] {
    if screen.len() <= SCREEN_BYTES_MAX {
        return screen;
    }
    let end = &screen[screen.len() - SCREEN_BYTES_MAX..];
    match end.iter().position(|byte| *byte == b'\n') {
        Some(index) => &end[index + 1..],
        None => end,
    }
}

#[cfg(test)]
#[path = "screen_tests.rs"]
mod tests;
