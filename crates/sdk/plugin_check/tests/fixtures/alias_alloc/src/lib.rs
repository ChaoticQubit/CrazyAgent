#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;

use crate::alloc as a;

/// A shared byte through a renamed alloc.
pub fn shared() -> a::sync::Arc<u8> {
    a::sync::Arc::new(1)
}
