use crate::engine::EngineError;
use crate::engine::owner::{AccountSecret, OwnerClientKind};
use kernel_security::secret::Zeroizing;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

const PROFILE_SECRET_MAX: usize = 8192;

/// The bytes of an owner packet: four magic bytes, a tag byte and a 32-byte token.
pub const OWNER_PACKET_BYTES: usize = 37;

/// An owner packet in a buffer on the heap from its birth, wiped when dropped, so moving a
/// holder copies only the pointer and no hop copies the token out of it.
pub type OwnerPacket = Box<Zeroizing<[u8; OWNER_PACKET_BYTES]>>;

/// Lays out one owner packet, the magic, the tag (a client kind, a runner slot or 0) and
/// the token, in a buffer wiped when dropped, on a write error or a cancel too. Inputs: the
/// magic, the tag and the token. Output: the packet.
pub fn owner_packet(magic: &[u8; 4], tag: u8, token: &[u8; 32]) -> OwnerPacket {
    let mut packet = owner_packet_buffer();
    packet[..4].copy_from_slice(magic);
    packet[4] = tag;
    packet[5..].copy_from_slice(token);
    packet
}

/// A zeroed owner packet buffer, wiped when dropped, for a server to read a client's packet
/// into. Output: the buffer.
pub fn owner_packet_buffer() -> OwnerPacket {
    Box::new(Zeroizing::new([0; OWNER_PACKET_BYTES]))
}

/// A second holder's copy of an owner packet, allocated first and then filled from the
/// borrowed bytes, so the packet never passes through a stack value. Input: the packet.
/// Output: the copy.
pub fn owner_packet_copy(packet: &[u8; OWNER_PACKET_BYTES]) -> OwnerPacket {
    let mut copy = owner_packet_buffer();
    copy.copy_from_slice(packet);
    copy
}

/// One 32-byte owner, runner, custody or IDE capability token, held on the heap from its
/// birth so moving a holder copies only the pointer, and wiped when its holder drops it.
pub type OwnerToken = Box<Zeroizing<[u8; 32]>>;

/// A zeroed capability token on the heap, wiped when dropped, to generate or read a token
/// into. Output: the token.
pub fn owner_token() -> OwnerToken {
    Box::new(Zeroizing::new([0; 32]))
}

/// A second holder's copy of a capability token, allocated first and then filled from the
/// heap, so the bytes never pass through a stack value. Input: the token. Output: the copy.
pub fn owner_token_copy(token: &[u8; 32]) -> OwnerToken {
    let mut copy = owner_token();
    copy.copy_from_slice(token);
    copy
}

/// The tag byte an owner client sends for its kind. Input: the kind. Output: 0 for the
/// terminal client, 1 for the desktop app.
pub fn client_tag(kind: OwnerClientKind) -> u8 {
    match kind {
        OwnerClientKind::Tui => 0,
        OwnerClientKind::Desktop => 1,
    }
}

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
/// the end of the pipe; the 1 KiB read buffer is wiped on every return. Inputs: the pipe,
/// the buffer, the end mark and the most bytes the buffer may hold. Output: Ok, or
/// `account_profile_outcome_unknown` past `maximum` or on a read error.
#[cfg(unix)]
pub fn drain(
    descriptor: &std::os::fd::OwnedFd,
    bytes: &mut Vec<u8>,
    eof: &mut bool,
    maximum: usize,
) -> Result<(), EngineError> {
    let unknown = || EngineError("account_profile_outcome_unknown".into());
    let mut buffer = Zeroizing::new([0; 1024]);
    for _ in 0..8 {
        match rustix::io::read(descriptor, &mut *buffer) {
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

#[cfg(test)]
mod tests {
    use super::{OwnerPacket, OwnerToken};

    /// An owner token lives behind a pointer, so moving any holder of one copies only the
    /// pointer and leaves no copy of the token's bytes behind.
    #[test]
    fn owner_token_is_a_pointer() {
        assert_eq!(size_of::<OwnerToken>(), size_of::<usize>());
    }

    /// An owner packet lives behind a pointer, so moving any holder of one copies only the
    /// pointer and leaves no copy of the token it carries behind.
    #[test]
    fn owner_packet_is_a_pointer() {
        assert_eq!(size_of::<OwnerPacket>(), size_of::<usize>());
    }
}
