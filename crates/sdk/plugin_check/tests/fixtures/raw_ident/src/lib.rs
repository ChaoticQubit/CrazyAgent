#![no_std]
#![forbid(unsafe_code)]

/// The manifest through a raw macro name.
pub const MANIFEST: &str = r#include_str!("../Cargo.toml");
