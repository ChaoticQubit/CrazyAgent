#![no_std]
#![forbid(unsafe_code)]

#[cfg(target_os = "macos")]
mod payload;
#[cfg(target_os = "macos")]
pub use self::payload::run;
