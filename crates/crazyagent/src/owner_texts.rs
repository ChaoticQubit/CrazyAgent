use crate::engine::EngineError;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[cfg(test)]
mod tests;

/// The most bytes one kept text may hold.
pub const TEXT_BYTES_MAX: usize = 1 << 20;
const KEY_BYTES: usize = 32;

/// A kept text's digest as an event body carries it (red stub, V250-OWNER-TEXT).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct TextDigest(String);

impl TextDigest {
    /// Red stub. Input: the text. Output: none yet.
    pub fn new(_text: String) -> Result<Self, EngineError> {
        todo!("V250-OWNER-TEXT green")
    }

    /// Red stub. Output: none yet.
    pub fn as_str(&self) -> &str {
        todo!("V250-OWNER-TEXT green")
    }
}

/// Red stub. Input: the connection. Output: none yet.
pub fn initialize(_db: &Connection) -> Result<(), EngineError> {
    todo!("V250-OWNER-TEXT green")
}

/// Red stub. Inputs: the key and the text. Output: none yet.
fn keyed(_key: &[u8; KEY_BYTES], _text: &[u8]) -> TextDigest {
    let _ = Sha256::digest(b"");
    todo!("V250-OWNER-TEXT green")
}

/// Red stub. Inputs: the transaction, the holder and the text. Output: none yet.
pub fn keep(_db: &Connection, _holder: &str, _text: &[u8]) -> Result<TextDigest, EngineError> {
    todo!("V250-OWNER-TEXT green")
}

/// Red stub. Inputs: the transaction and the holder. Output: none yet.
pub fn release(_db: &Connection, _holder: &str) -> Result<(), EngineError> {
    todo!("V250-OWNER-TEXT green")
}

/// Red stub. Inputs: the connection and the digest. Output: none yet.
pub fn text(_db: &Connection, _digest: &TextDigest) -> Result<Option<Vec<u8>>, EngineError> {
    todo!("V250-OWNER-TEXT green")
}

/// Red stub. Inputs: the connection, the digest and the missing list. Output: none yet.
pub fn replayed(
    _db: &Connection,
    _digest: &TextDigest,
    _missing: &mut Vec<TextDigest>,
) -> Result<Vec<u8>, EngineError> {
    todo!("V250-OWNER-TEXT green")
}
