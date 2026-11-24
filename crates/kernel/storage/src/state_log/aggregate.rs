use serde::{Serialize, Serializer};

/// The longest stored aggregate name, in bytes.
pub const AGGREGATE_BYTES_MAX: usize = 64;

/// One aggregate's stored name in `state_log.aggregate` and in snapshot bodies: 1 to
/// [`AGGREGATE_BYTES_MAX`] bytes of `[a-z0-9_]`, checked when it is made (at compile time
/// for a `const`). It serializes as its string. Input: the name. Output: the name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AggregateName(&'static str);

impl AggregateName {
    /// Whether text has an aggregate name's shape: 1 to [`AGGREGATE_BYTES_MAX`] bytes of
    /// `[a-z0-9_]`, as [`AggregateName::new`] demands, without panicking. Input: the text.
    /// Output: the answer.
    pub const fn shaped(name: &str) -> bool {
        if name.is_empty() || name.len() > AGGREGATE_BYTES_MAX {
            return false;
        }
        let bytes = name.as_bytes();
        let mut at = 0;
        while at < bytes.len() {
            let byte = bytes[at];
            if !(byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_') {
                return false;
            }
            at += 1;
        }
        true
    }

    /// Checks and keeps one aggregate name. Input: the name. Output: the name; a name out
    /// of bounds or with another byte panics, at compile time in a `const`.
    pub const fn new(name: &'static str) -> Self {
        assert!(
            !name.is_empty() && name.len() <= AGGREGATE_BYTES_MAX,
            "an aggregate name is 1 to 64 bytes"
        );
        let bytes = name.as_bytes();
        let mut at = 0;
        while at < bytes.len() {
            let byte = bytes[at];
            assert!(
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_',
                "an aggregate name holds only [a-z0-9_]"
            );
            at += 1;
        }
        Self(name)
    }

    /// The stored name. Output: the name, as given to [`AggregateName::new`].
    pub const fn name(self) -> &'static str {
        self.0
    }
}

impl Serialize for AggregateName {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.0)
    }
}

#[cfg(test)]
mod tests;
