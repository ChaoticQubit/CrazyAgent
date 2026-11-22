#![no_std]
#![forbid(unsafe_code)]

use ::{core::fmt};

/// A formatter result.
pub fn done() -> fmt::Result {
    Ok(())
}
