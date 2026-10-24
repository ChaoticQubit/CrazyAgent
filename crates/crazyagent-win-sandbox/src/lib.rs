#![deny(unsafe_op_in_unsafe_fn)]
#![cfg_attr(not(windows), forbid(unsafe_code))]

pub mod dacl;
pub mod plan;
pub mod profile;
pub mod quote;
pub mod request;
#[cfg(windows)]
mod win;

#[cfg(windows)]
pub use win::{BoundaryError, run};
