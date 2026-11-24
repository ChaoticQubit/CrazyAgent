#![no_std]
#![forbid(unsafe_code)]

extern crate core as c;

/// One.
pub fn one() -> u8 {
    c::convert::identity(1)
}
