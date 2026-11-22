#![forbid(unsafe_code)]

use sha2::{Digest, Sha256};

/// Hashes bytes with SHA-256, the digest the engine keys socket names, profile references
/// and stored records by. Input: the bytes. Output: the digest as 64 lowercase hex digits.
pub fn hash(bytes: &[u8]) -> String {
    let digest = format!("{:x}", Sha256::digest(bytes));
    assert_eq!(digest.len(), 64);
    digest
}

#[cfg(test)]
mod tests {
    #[test]
    fn hash_is_the_lowercase_hex_sha256() {
        assert_eq!(
            super::hash(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            super::hash(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
