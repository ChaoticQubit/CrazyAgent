use crate::engine::EngineError;
use std::io::{ErrorKind, Read};

const READ_BYTES: usize = 4096;
const READ_BATCH: usize = 16;

/// The most bytes custody takes from one output pipe of a sign-in login child over its life.
pub(super) const OUTPUT_BYTES_MAX: usize = 65_536;

/// One output pipe of a sign-in login child as custody reads it: a read never blocks, takes
/// at most [`OUTPUT_BYTES_MAX`] bytes over the pipe's life and refuses with the pipe's bound
/// code past that, and marks the pipe ended at its end. On Unix the source is the
/// pipe itself, made non-blocking by [`nonblocking`]; on Windows it is S1's pumped output.
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

    /// Takes what the pipe holds now, at most [`READ_BATCH`] reads of [`READ_BYTES`], and
    /// marks it ended at its end. Output: the bytes, the bound refusal, or the failed read.
    pub(super) fn read(&mut self) -> Result<Vec<u8>, EngineError> {
        let mut taken = Vec::new();
        let mut buffer = [0_u8; READ_BYTES];
        for _ in 0..READ_BATCH {
            match self.source.read(&mut buffer[..]) {
                Ok(0) => {
                    self.eof = true;
                    break;
                }
                Ok(size) => {
                    if size > OUTPUT_BYTES_MAX.saturating_sub(self.bytes) {
                        return Err(EngineError(self.bound.into()));
                    }
                    self.bytes += size;
                    taken.extend_from_slice(&buffer[..size]);
                }
                Err(error) if error.kind() == ErrorKind::WouldBlock => break,
                Err(error) if error.kind() == ErrorKind::Interrupted => continue,
                Err(error) => return Err(crate::engine::io_error(error)),
            }
        }
        assert!(self.bytes <= OUTPUT_BYTES_MAX);
        assert!(taken.len() <= self.bytes);
        Ok(taken)
    }

    /// Whether the pipe has ended. Output: the answer.
    pub(super) fn ended(&self) -> bool {
        self.eof
    }
}

/// Makes a login child's pipe end non-blocking, so a read or write says `WouldBlock` when
/// it would wait. Input: the pipe end. Output: none, or the failed step.
#[cfg(unix)]
pub(super) fn nonblocking(pipe: &std::os::fd::OwnedFd) -> Result<(), EngineError> {
    use rustix::fs::{OFlags, fcntl_getfl, fcntl_setfl};
    let flags = fcntl_getfl(pipe).map_err(crate::engine::io_error)?;
    fcntl_setfl(pipe, flags | OFlags::NONBLOCK).map_err(crate::engine::io_error)?;
    assert!(
        fcntl_getfl(pipe)
            .map_err(crate::engine::io_error)?
            .contains(OFlags::NONBLOCK)
    );
    Ok(())
}

/// A login child's output pipe end as a [`LoginPipe`], made non-blocking first. Inputs: the
/// pipe end and the bound code. Output: the pipe, or the failed step.
#[cfg(unix)]
pub(super) fn unix_pipe(
    pipe: std::os::fd::OwnedFd,
    bound: &'static str,
) -> Result<LoginPipe<std::fs::File>, EngineError> {
    nonblocking(&pipe)?;
    Ok(LoginPipe::new(std::fs::File::from(pipe), bound))
}
