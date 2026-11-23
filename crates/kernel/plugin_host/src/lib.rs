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
pub mod manifest;
pub mod problem;
pub mod views;

#[cfg(all(test, feature = "store"))]
mod embedding_tests;
#[cfg(test)]
mod problem_tests;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod views_tests;
