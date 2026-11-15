#![deny(unsafe_op_in_unsafe_fn)]
#![cfg_attr(not(target_os = "linux"), forbid(unsafe_code))]

pub mod filter;
pub mod rules;
#[cfg(target_os = "linux")]
pub mod serve;
#[cfg(target_os = "linux")]
mod sys;
