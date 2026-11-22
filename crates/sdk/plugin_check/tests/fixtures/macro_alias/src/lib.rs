#![no_std]
#![forbid(unsafe_code)]

use core::include_str as inc;

/// The manifest through an aliased macro.
pub const MANIFEST: &str = inc!("../Cargo.toml");
