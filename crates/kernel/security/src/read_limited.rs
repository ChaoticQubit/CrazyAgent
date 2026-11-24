use std::io::Read;

/// Reads at most `limit + 1` bytes of a child's output into one buffer, the extra byte
/// letting the caller refuse output over `limit`. Inputs: the reader and the limit. Output:
/// the read's result and the bytes, which the caller wipes.
pub fn read_limited(reader: impl Read, limit: usize) -> (std::io::Result<usize>, Vec<u8>) {
    let bound = limit
        .checked_add(1)
        .expect("a read limit leaves room for one more byte");
    let mut bytes = Vec::new();
    let read = reader.take(bound as u64).read_to_end(&mut bytes);
    assert!(bytes.len() <= bound);
    (read, bytes)
}
