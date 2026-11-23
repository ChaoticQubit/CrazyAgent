#![forbid(unsafe_code)]

extern crate alloc;

/// The host binding capgen writes from the `.cap` files: `Providers`, `Bound` and
/// `impl sdk::Host for Bound`, every method running the bus gateway's `Caller::call`.
pub mod generated;
pub use generated::{Bound, Providers};

#[cfg(test)]
mod overhead_tests;
#[cfg(test)]
mod tests;
