use crate::engine::{EngineError, io_error};
use rustix::fs::{OFlags, fcntl_getfl, fcntl_setfl};
use std::os::fd::{AsFd, OwnedFd};
use std::time::{Duration, Instant};
use tokio::io::Interest;
use tokio::io::unix::AsyncFd;

const TOKEN_BYTES: usize = 32;
const BOOTSTRAP_TIME_MAX: Duration = Duration::from_secs(10);

pub(super) async fn read_bootstrap(
    input: &OwnedFd,
    deadline: Instant,
) -> Result<[u8; 32], EngineError> {
    let entered = Instant::now();
    if entered >= deadline {
        return Err(EngineError("native_auth_bootstrap_timeout".to_owned()));
    }
    let deadline = deadline.min(entered + BOOTSTRAP_TIME_MAX);
    let flags = fcntl_getfl(input).map_err(io_error)?;
    fcntl_setfl(input, flags | OFlags::NONBLOCK).map_err(io_error)?;
    let input = AsyncFd::with_interest(input.as_fd(), Interest::READABLE).map_err(io_error)?;
    let expires = tokio::time::Instant::from_std(deadline);
    let mut buffer = [0; TOKEN_BYTES + 1];
    let mut used = 0;
    loop {
        assert!(used <= TOKEN_BYTES);
        if Instant::now() >= deadline {
            return Err(EngineError("native_auth_bootstrap_timeout".to_owned()));
        }
        let mut ready = tokio::time::timeout_at(expires, input.readable())
            .await
            .map_err(|_| EngineError("native_auth_bootstrap_timeout".to_owned()))?
            .map_err(io_error)?;
        if Instant::now() >= deadline {
            return Err(EngineError("native_auth_bootstrap_timeout".to_owned()));
        }
        let read = ready.try_io(|fd| {
            rustix::io::read(fd.get_ref(), &mut buffer[used..]).map_err(std::io::Error::from)
        });
        match read {
            Ok(Ok(0)) if used == TOKEN_BYTES => {
                let mut token = [0; TOKEN_BYTES];
                token.copy_from_slice(&buffer[..TOKEN_BYTES]);
                return Ok(token);
            }
            Ok(Ok(0)) => {
                return Err(EngineError("native_auth_bootstrap_truncated".to_owned()));
            }
            Ok(Ok(size)) => {
                used += size;
                assert!(used <= buffer.len());
                if used > TOKEN_BYTES {
                    return Err(EngineError("native_auth_bootstrap_excess".to_owned()));
                }
            }
            Ok(Err(error)) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Ok(Err(error)) => return Err(io_error(error)),
            Err(_would_block) => continue,
        }
    }
}
