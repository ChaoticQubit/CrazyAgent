#[cfg(windows)]
use crate::engine::{EngineError, io_error};
#[cfg(windows)]
use std::fs::File;
#[cfg(windows)]
use std::io::Read;
use std::time::Duration;
#[cfg(windows)]
use std::time::Instant;

/// The bytes of the token a custodian reads on its standard input before anything else.
pub(super) const TOKEN_BYTES: usize = 32;
/// The longest a custodian waits for its token and the pipe's end.
pub(super) const BOOTSTRAP_TIME_MAX: Duration = Duration::from_secs(10);

/// Reads the custodian's 32-byte bootstrap token from its stdin pipe on Windows, ending at
/// EOF or the deadline, at most ten seconds on. A detached thread does the blocking read, so
/// neither a runtime's end nor the custodian's exit waits for a pipe that stays open. Inputs:
/// the pipe, owned, and the deadline. Output: the token, or the timeout, truncated or excess
/// refusal.
#[cfg(windows)]
pub(super) async fn read_bootstrap(
    input: File,
    deadline: Instant,
) -> Result<[u8; TOKEN_BYTES], EngineError> {
    let entered = Instant::now();
    if entered >= deadline {
        return Err(EngineError("native_auth_bootstrap_timeout".to_owned()));
    }
    let deadline = deadline.min(entered + BOOTSTRAP_TIME_MAX);
    let (sender, read) = tokio::sync::oneshot::channel();
    std::thread::Builder::new()
        .name("native-auth-bootstrap".into())
        .spawn(move || {
            let _ = sender.send(token_of(input));
        })
        .map_err(io_error)?;
    let read = tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), read)
        .await
        .map_err(|_| EngineError("native_auth_bootstrap_timeout".to_owned()))?;
    if Instant::now() >= deadline {
        return Err(EngineError("native_auth_bootstrap_timeout".to_owned()));
    }
    read.expect("the bootstrap reader sends its answer before it ends")
}

/// Reads a pipe to its end, keeping at most one byte past the token. Input: the pipe.
/// Output: the token, `native_auth_bootstrap_truncated` for fewer bytes,
/// `native_auth_bootstrap_excess` for more, or the read error.
#[cfg(windows)]
fn token_of(mut input: File) -> Result<[u8; TOKEN_BYTES], EngineError> {
    let mut buffer = [0; TOKEN_BYTES + 1];
    let mut used = 0;
    loop {
        assert!(used <= TOKEN_BYTES);
        let size = match input.read(&mut buffer[used..]) {
            Ok(size) => size,
            Err(error) if error.kind() == std::io::ErrorKind::BrokenPipe => 0,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(io_error(error)),
        };
        if size == 0 && used == TOKEN_BYTES {
            let mut token = [0; TOKEN_BYTES];
            token.copy_from_slice(&buffer[..TOKEN_BYTES]);
            return Ok(token);
        }
        if size == 0 {
            return Err(EngineError("native_auth_bootstrap_truncated".to_owned()));
        }
        used += size;
        assert!(used <= buffer.len());
        if used > TOKEN_BYTES {
            return Err(EngineError("native_auth_bootstrap_excess".to_owned()));
        }
    }
}
