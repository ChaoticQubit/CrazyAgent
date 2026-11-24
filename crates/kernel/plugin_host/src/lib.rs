#![forbid(unsafe_code)]

/// Why the plugin host refused a plugin file: a stable code and the 1-based line of the
/// refused part, 1 when the refusal names the whole file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Refused {
    pub code: &'static str,
    pub line: usize,
}

#[cfg(feature = "store")]
pub mod assets;
#[cfg(feature = "store")]
pub mod embedding;
#[cfg(feature = "store")]
pub mod grants;
/// Tables capgen writes from the `.cap` files and the bundled plugins' manifests.
pub mod generated {
    pub mod permissions;
}
pub mod manifest;
pub mod permission;
pub mod problem;
#[cfg(feature = "store")]
pub mod registry;
#[cfg(feature = "store")]
pub mod store;
#[cfg(feature = "store")]
pub mod trust;
#[cfg(feature = "store")]
pub mod usage;
pub mod views;

#[cfg(all(test, feature = "store"))]
mod embedding_tests;
#[cfg(all(test, feature = "store", unix))]
mod grants_tests;
#[cfg(test)]
mod permission_tests;
#[cfg(all(test, feature = "store"))]
mod problem_tests;
#[cfg(all(test, feature = "store", unix))]
mod registry_tests;
#[cfg(test)]
mod tests;
#[cfg(all(test, feature = "store"))]
mod usage_tests;
#[cfg(all(test, feature = "store"))]
mod views_tests;
