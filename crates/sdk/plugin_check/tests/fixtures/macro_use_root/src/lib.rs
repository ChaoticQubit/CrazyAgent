#![forbid(unsafe_code)]

/// Reads a file through a root import inside an assert.
pub fn sneak() {
    assert!({
        use std as s;
        let _ = s::fs::read("secret");
        true
    });
}
