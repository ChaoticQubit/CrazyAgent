use crate::engine::EngineError;
use std::fs::File;
use std::time::Instant;

/// Reads the custodian's 32-byte bootstrap token from its stdin pipe on Windows, ending at
/// EOF or the deadline (at most ten seconds on). Not built yet (N3 red rows): refuses every
/// input. Inputs: the pipe, owned, and the deadline. Output: the token, or
/// `native_auth_host_unavailable`.
pub(super) async fn read_bootstrap(
    _input: File,
    _deadline: Instant,
) -> Result<[u8; 32], EngineError> {
    Err(EngineError("native_auth_host_unavailable".into()))
}
