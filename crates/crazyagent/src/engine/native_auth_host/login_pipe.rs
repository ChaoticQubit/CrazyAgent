use crate::engine::EngineError;
use std::io::Read;

/// The most bytes custody takes from one output pipe of a sign-in login child over its life.
pub(super) const OUTPUT_BYTES_MAX: usize = 65_536;

/// One output pipe of a sign-in login child as custody reads it: a read never blocks, takes
/// at most [`OUTPUT_BYTES_MAX`] bytes over the pipe's life and refuses with the pipe's bound
/// code past that, and marks the pipe ended at its end. Not built yet (N4a red rows).
pub(super) struct LoginPipe<R> {
    source: R,
    bytes: usize,
    eof: bool,
    bound: &'static str,
}

impl<R: Read> LoginPipe<R> {
    /// Wraps a source whose read says `WouldBlock` while it holds nothing. Inputs: the
    /// source and the code a read refuses with past the bound. Output: the pipe.
    pub(super) fn new(source: R, bound: &'static str) -> Self {
        Self {
            source,
            bytes: 0,
            eof: false,
            bound,
        }
    }

    /// Takes what the pipe holds now. Not built yet (N4a red rows): it takes nothing.
    /// Output: the bytes, the bound refusal, or the failed read.
    pub(super) fn read(&mut self) -> Result<Vec<u8>, EngineError> {
        let _ = (&mut self.source, self.bytes, self.bound);
        Ok(Vec::new())
    }

    /// Whether the pipe has ended. Output: the answer.
    pub(super) fn ended(&self) -> bool {
        self.eof
    }
}
