#![no_std]
#![forbid(unsafe_code)]

/// Names the program arguments.
pub fn args() -> String {
    format!("{:?}", std::env::args().count())
}
