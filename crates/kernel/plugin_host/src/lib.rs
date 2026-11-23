#![forbid(unsafe_code)]

/// Why the plugin host refused a plugin file: a stable code and the 1-based line of the
/// refused part, 1 when the refusal names the whole file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Refused {
    pub code: &'static str,
    pub line: usize,
}

#[cfg(feature = "store")]
pub mod embedding;
pub mod manifest;
pub mod problem;
#[cfg(feature = "store")]
pub mod registry;
#[cfg(feature = "store")]
pub mod store;
#[cfg(feature = "store")]
pub mod trust;
pub mod views;

#[cfg(all(test, feature = "store"))]
mod embedding_tests;
#[cfg(all(test, feature = "store"))]
mod problem_tests;
#[cfg(all(test, feature = "store", unix))]
mod registry_tests;
#[cfg(test)]
mod tests;
#[cfg(all(test, feature = "store"))]
mod views_tests;
