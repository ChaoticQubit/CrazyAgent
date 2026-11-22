#![no_std]
#![forbid(unsafe_code)]

/// Names whether the root exists.
pub fn args() -> String {
    format!("{:?}", std::fs::metadata("/").is_ok())
}
