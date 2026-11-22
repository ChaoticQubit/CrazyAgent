#![no_std]
#![forbid(unsafe_code)]

/// The manifest bytes.
pub const MANIFEST: &str = include_str!("../Cargo.toml");
