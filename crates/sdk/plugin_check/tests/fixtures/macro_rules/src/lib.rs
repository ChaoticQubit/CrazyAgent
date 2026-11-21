#![no_std]
#![forbid(unsafe_code)]

macro_rules! twice {
    ($x:expr) => {
        $x * 2
    };
}

/// Doubles.
pub fn double(x: u8) -> u8 {
    twice!(x)
}
