#![forbid(unsafe_code)]

pub mod error;
pub mod exposure;
#[cfg(target_os = "macos")]
pub mod keychain;
pub mod net;
pub mod relay_token;
pub mod secret;
pub mod secrets;
pub mod store;

#[cfg(test)]
#[path = "../../common/src/perf_kit.rs"]
mod perf_kit;
#[cfg(test)]
mod perf_tests;
