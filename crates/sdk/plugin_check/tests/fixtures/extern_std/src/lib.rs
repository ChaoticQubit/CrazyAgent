#![no_std]
#![forbid(unsafe_code)]

extern crate std;

/// Reads a file through std named back in.
pub fn read() -> bool {
    std::fs::metadata("/").is_ok()
}
