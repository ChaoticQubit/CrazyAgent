#![forbid(unsafe_code)]

extern crate alloc;

/// A boxed byte.
pub fn boxed() -> alloc::boxed::Box<u8> {
    alloc::boxed::Box::new(1)
}
