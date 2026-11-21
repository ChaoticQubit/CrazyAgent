#![no_std]
#![forbid(unsafe_code)]

/// Reads a file through a raw std.
pub fn read() -> bool {
    r#std::fs::read("x").is_ok()
}

/// Names whether the root exists through a raw std inside a macro.
pub fn args() -> String {
    format!("{:?}", r#std::fs::metadata("/").is_ok())
}
