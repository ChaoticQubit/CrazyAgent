#![forbid(unsafe_code)]

/// Reads a file.
pub fn read() -> bool {
    std::fs::read("x").is_ok()
}
