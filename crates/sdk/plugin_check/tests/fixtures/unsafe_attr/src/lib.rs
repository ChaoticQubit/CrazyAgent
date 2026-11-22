#![no_std]
#![forbid(unsafe_code)]

/// An exported symbol.
#[unsafe(no_mangle)]
pub extern "C" fn plug() {}

/// A placed static.
#[link_section = ".init_array"]
pub static PLACED: u8 = 1;
