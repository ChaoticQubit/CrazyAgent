#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;

use alloc::vec;

/// Loads a module file from a block inside macro arguments.
pub fn run() -> usize {
    let ran = vec![{
        #[path = "../hidden/payload.rs"]
        mod m;
        m::run()
    }];
    ran.len()
}
