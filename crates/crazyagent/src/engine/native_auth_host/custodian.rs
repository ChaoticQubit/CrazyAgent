use crate::engine::EngineError;

/// What a custodian wrote on its error stream on Windows, kept up to one code and its line
/// end and read by a thread that drains the rest, so a custodian that writes more never
/// blocks on a full pipe. Not built yet (N3 red rows): it is never made.
pub(super) enum CustodianCause {}

impl CustodianCause {
    /// Starts reading a custodian's error stream. Not built yet: refuses every stream.
    /// Input: the stream, unused. Output: `native_auth_host_unavailable`.
    pub(super) fn new(_stream: std::process::ChildStderr) -> Result<Self, EngineError> {
        Err(EngineError("native_auth_host_unavailable".into()))
    }

    /// Takes what the reader thread holds so far. Never runs, as no cause is made.
    /// Output: none.
    pub(super) fn read(&mut self) -> Result<(), EngineError> {
        match *self {}
    }

    /// The code the custodian wrote: one line of lowercase letters and underscores of at
    /// most 128 bytes. Never runs, as no cause is made. Output: none.
    pub(super) fn code(&self) -> Option<&str> {
        match *self {}
    }
}
