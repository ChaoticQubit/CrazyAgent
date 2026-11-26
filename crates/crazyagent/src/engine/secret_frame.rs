use crate::engine::EngineError;
use crate::engine::owner::AccountSecret;
use kernel_security::secret::Zeroizing;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

const PROFILE_SECRET_MAX: usize = 8192;

/// Writes one secret to an owner stream as its 4-byte big-endian length and its bytes, or a
/// zero length when there is none, and wipes the bytes. Inputs: the stream and the secret.
/// Output: the write's result.
pub async fn write_secret_frame<S: AsyncWrite + Unpin>(
    stream: &mut S,
    secret: Option<AccountSecret>,
) -> std::io::Result<()> {
    let bytes = Zeroizing::new(secret.map(AccountSecret::into_bytes).unwrap_or_default());
    let mut framed = Zeroizing::new(Vec::with_capacity(4 + bytes.len()));
    framed.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
    framed.extend_from_slice(&bytes);
    drop(bytes);
    stream.write_all(&framed).await
}

/// Reads one secret sent as its 4-byte big-endian length and its bytes, of at most `max`
/// bytes; a zero length reads as no bytes. Inputs: the stream and the bound. Output: the
/// bytes, or none when the length is over the bound or the stream ends first.
pub async fn read_secret_frame<S: AsyncRead + Unpin>(
    stream: &mut S,
    max: usize,
) -> Option<Vec<u8>> {
    let size = stream.read_u32().await.ok()? as usize;
    if size > max {
        return None;
    }
    let mut bytes = Zeroizing::new(vec![0; size]);
    stream.read_exact(&mut bytes).await.ok()?;
    Some(std::mem::take(&mut *bytes))
}

/// Reads the secret frame of an account profile request, which must be the last bytes the
/// client sends: a secret when one was announced, a zero length when none was. Inputs: the
/// stream and whether a secret was announced. Output: the secret, none, or
/// `account_profile_secret_frame_invalid`.
pub async fn read_profile_secret<S: AsyncRead + Unpin>(
    stream: &mut S,
    present: bool,
) -> Result<Option<AccountSecret>, EngineError> {
    let invalid = || EngineError("account_profile_secret_frame_invalid".into());
    let mut bytes = Zeroizing::new(
        read_secret_frame(stream, PROFILE_SECRET_MAX)
            .await
            .ok_or_else(invalid)?,
    );
    if present == bytes.is_empty() {
        return Err(invalid());
    }
    let mut extra = [0; 1];
    if stream.read(&mut extra).await.map_err(|_| invalid())? != 0 {
        return Err(invalid());
    }
    if present {
        AccountSecret::new(std::mem::take(&mut *bytes)).map(Some)
    } else {
        Ok(None)
    }
}

/// Reads what a child's pipe holds now, up to eight reads of 1 KiB, into `bytes`, marking
/// the end of the pipe. Inputs: the pipe, the buffer, the end mark and the most bytes the
/// buffer may hold. Output: Ok, or `account_profile_outcome_unknown` past `maximum` or on a
/// read error.
#[cfg(unix)]
pub fn drain(
    descriptor: &std::os::fd::OwnedFd,
    bytes: &mut Vec<u8>,
    eof: &mut bool,
    maximum: usize,
) -> Result<(), EngineError> {
    let unknown = || EngineError("account_profile_outcome_unknown".into());
    let mut buffer = [0; 1024];
    for _ in 0..8 {
        match rustix::io::read(descriptor, &mut buffer) {
            Ok(0) => {
                *eof = true;
                return Ok(());
            }
            Ok(size) => {
                if size > maximum.saturating_sub(bytes.len()) {
                    return Err(unknown());
                }
                bytes.extend_from_slice(&buffer[..size]);
            }
            Err(rustix::io::Errno::AGAIN) => return Ok(()),
            Err(rustix::io::Errno::INTR) => {}
            Err(_) => return Err(unknown()),
        }
    }
    Ok(())
}

/// Reads and wipes what a child's error pipe holds now, counting the bytes; the child's
/// error output may echo the secret it was sent. Inputs: the pipe, the running count, of at
/// most 4096 bytes, and the end mark. Output: Ok, or the refusals of [`drain`].
#[cfg(unix)]
pub fn drain_errors(
    descriptor: &std::os::fd::OwnedFd,
    count: &mut usize,
    eof: &mut bool,
) -> Result<(), EngineError> {
    let mut bytes = Zeroizing::new(Vec::with_capacity(4096usize.saturating_sub(*count)));
    drain(
        descriptor,
        &mut bytes,
        eof,
        4096usize.saturating_sub(*count),
    )?;
    *count += bytes.len();
    Ok(())
}
