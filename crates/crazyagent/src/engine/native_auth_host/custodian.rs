use crate::engine::{EngineError, io_error};
#[cfg(unix)]
use rustix::fs::OFlags;
#[cfg(windows)]
use std::io::Read;
#[cfg(unix)]
use std::os::fd::OwnedFd;
#[cfg(windows)]
use std::sync::mpsc::{Receiver, Sender};

/// The longest failure code a custodian may name: the bound of a stored sign-in fact's code.
pub(super) const CAUSE_BYTES_MAX: usize = 128;
/// The bytes a cause keeps: one code, its line end and one more byte, so a longer line still
/// reads as too long.
const CAUSE_KEPT_MAX: usize = CAUSE_BYTES_MAX + 2;

/// What a custodian wrote on its error stream, kept up to one code and its line end and read
/// without ever waiting, so a custodian that writes more never blocks on a full pipe.
#[cfg(unix)]
pub(super) struct CustodianCause {
    stream: OwnedFd,
    pub(super) text: Vec<u8>,
}

/// What a custodian wrote on its error stream on Windows, kept up to one code and its line
/// end. A thread of its own drains the stream to its end, since a Windows pipe read cannot
/// be made to return at once, so a custodian that writes more never blocks on a full pipe.
#[cfg(windows)]
pub(super) struct CustodianCause {
    kept: Receiver<Vec<u8>>,
    pub(super) text: Vec<u8>,
}

#[cfg(unix)]
impl CustodianCause {
    /// Takes the custodian's error stream and makes reads on it return at once.
    /// Input: the stream. Output: the cause, empty so far.
    pub(super) fn new(stream: std::process::ChildStderr) -> Result<Self, EngineError> {
        let stream = OwnedFd::from(stream);
        let flags = rustix::fs::fcntl_getfl(&stream).map_err(io_error)?;
        rustix::fs::fcntl_setfl(&stream, flags | OFlags::NONBLOCK).map_err(io_error)?;
        Ok(Self {
            stream,
            text: Vec::new(),
        })
    }

    /// Reads what the stream holds now, keeping at most one code, its line end and one more
    /// byte, so a longer line still reads as too long. Output: done once nothing waits in it.
    pub(super) fn read(&mut self) -> Result<(), EngineError> {
        let mut buffer = [0; 256];
        loop {
            match rustix::io::read(&self.stream, &mut buffer) {
                Ok(0) | Err(rustix::io::Errno::AGAIN) => return Ok(()),
                Ok(size) => {
                    let room = CAUSE_KEPT_MAX.saturating_sub(self.text.len());
                    self.text.extend_from_slice(&buffer[..size.min(room)]);
                }
                Err(rustix::io::Errno::INTR) => {}
                Err(error) => return Err(io_error(error)),
            }
        }
    }
}

#[cfg(windows)]
impl CustodianCause {
    /// Takes the custodian's error stream and starts the thread that drains it, which ends
    /// at the stream's end, once the custodian and every holder of its error stream ended.
    /// Input: the stream. Output: the cause, empty so far, or the thread start error.
    pub(super) fn new(stream: std::process::ChildStderr) -> Result<Self, EngineError> {
        let (sender, kept) = std::sync::mpsc::channel();
        std::thread::Builder::new()
            .name("native-auth-cause".into())
            .spawn(move || drain(stream, &sender))
            .map_err(io_error)?;
        Ok(Self {
            kept,
            text: Vec::new(),
        })
    }

    /// Takes what the thread kept so far, never waiting. Output: Ok, also once the thread
    /// ended.
    pub(super) fn read(&mut self) -> Result<(), EngineError> {
        while let Ok(bytes) = self.kept.try_recv() {
            let room = CAUSE_KEPT_MAX.saturating_sub(self.text.len());
            self.text.extend_from_slice(&bytes[..bytes.len().min(room)]);
        }
        assert!(self.text.len() <= CAUSE_KEPT_MAX);
        Ok(())
    }
}

impl CustodianCause {
    /// The code the custodian named: its one line when that line is a code a sign-in fact can
    /// carry, 1 to 128 lowercase letters and `_`. Output: the code, or None for any other text.
    pub(super) fn code(&self) -> Option<&str> {
        let line = self.text.strip_suffix(b"\n").unwrap_or(&self.text);
        let plain = !line.is_empty()
            && line.len() <= CAUSE_BYTES_MAX
            && line
                .iter()
                .all(|byte| byte.is_ascii_lowercase() || *byte == b'_');
        plain.then(|| std::str::from_utf8(line).ok()).flatten()
    }
}

/// Reads a custodian's error stream to its end on the cause's thread, sending at most
/// [`CAUSE_KEPT_MAX`] bytes and dropping the rest. A read error ends it, which closes the
/// stream, so the custodian's next write fails rather than blocks. Inputs: the stream and
/// the cause's channel.
#[cfg(windows)]
fn drain(mut stream: std::process::ChildStderr, sender: &Sender<Vec<u8>>) {
    let mut buffer = [0; 4096];
    let mut sent = 0;
    loop {
        match stream.read(&mut buffer) {
            Ok(0) => return,
            Ok(size) => {
                let taken = size.min(CAUSE_KEPT_MAX - sent);
                if taken > 0 && sender.send(buffer[..taken].to_vec()).is_ok() {
                    sent += taken;
                }
                assert!(sent <= CAUSE_KEPT_MAX);
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return,
        }
    }
}
