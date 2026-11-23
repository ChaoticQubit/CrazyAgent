#![forbid(unsafe_code)]

use sha2::{Digest, Sha256};

pub mod names;
pub mod net;

#[cfg(test)]
mod names_tests;
#[cfg(test)]
mod net_tests;

/// Hashes bytes with SHA-256, the digest the engine keys socket names, profile references
/// and stored records by. Input: the bytes. Output: the digest as 64 lowercase hex digits.
pub fn hash(bytes: &[u8]) -> String {
    let digest = format!("{:x}", Sha256::digest(bytes));
    assert_eq!(digest.len(), 64);
    digest
}

const WINDOWS_DEVICE_NAMES: [&str; 22] = [
    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
    "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];

/// True when Windows opens the name as a device instead of a file: CON, PRN, AUX, NUL,
/// COM1 to COM9 or LPT1 to LPT9 in any case, alone, before an extension or before one
/// trailing colon, with or without spaces before the dot. Input: one path component.
/// Output: whether it is reserved on Windows.
pub fn windows_device(name: &str) -> bool {
    let name = name.strip_suffix(':').unwrap_or(name);
    let stem = name
        .split_once('.')
        .map_or(name, |(stem, _)| stem)
        .trim_end_matches(' ');
    WINDOWS_DEVICE_NAMES
        .iter()
        .any(|device| stem.eq_ignore_ascii_case(device))
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
