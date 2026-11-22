#![no_std]
#![forbid(unsafe_code)]

use {::std::fs::read_to_string};

/// Reads a file through a use syn keeps as Verbatim.
pub fn read() -> usize {
    read_to_string("/etc/hosts").map_or(0, |text| text.len())
}
