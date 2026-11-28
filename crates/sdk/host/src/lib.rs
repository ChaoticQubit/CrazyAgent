#![forbid(unsafe_code)]

extern crate alloc;

/// The host binding capgen writes from the `.cap` files: `Providers`, `Bound` and
/// `impl sdk::Host for Bound`, every method running the bus gateway's `Caller::call`.
pub mod generated;
pub use generated::{Bound, Providers};

#[cfg(test)]
mod fixture_served;
#[cfg(test)]
mod overhead_tests;
#[cfg(test)]
#[path = "../../../kernel/common/src/perf_kit.rs"]
mod perf_kit;
#[cfg(test)]
mod perf_tests;
#[cfg(test)]
mod served_tests;
#[cfg(test)]
mod tests;
