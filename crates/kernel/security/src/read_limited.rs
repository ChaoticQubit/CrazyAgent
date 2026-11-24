use std::io::Read;

/// Reads at most `limit + 1` bytes of a child's output into one buffer reserved for all of
/// them before the first byte, so no growth frees a copy of a secret it holds and the read
/// never passes bytes through a stack probe; the extra byte lets the caller refuse output
/// over `limit`. Inputs: the reader and the limit. Output: the result and the bytes.
pub fn read_limited(reader: impl Read, limit: usize) -> (std::io::Result<usize>, Vec<u8>) {
    let bound = limit
        .checked_add(1)
        .expect("a read limit leaves room for one more byte");
    let mut bytes = Vec::with_capacity(bound);
    let capacity = bytes.capacity();
    let read = reader.take(bound as u64).read_to_end(&mut bytes);
    assert!(bytes.len() <= bound);
    assert_eq!(bytes.capacity(), capacity);
    (read, bytes)
}
