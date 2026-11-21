#![forbid(unsafe_code)]

use std as s;

/// Reads a file through a renamed std.
pub fn read() -> bool {
    s::fs::read("x").is_ok()
}
