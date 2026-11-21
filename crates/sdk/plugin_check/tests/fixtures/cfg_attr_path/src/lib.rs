#![no_std]
#![forbid(unsafe_code)]

#[cfg_attr(all(), path = "../hidden/payload.rs")]
mod payload;

/// Runs the payload.
pub fn run() -> bool {
    payload::run()
}
