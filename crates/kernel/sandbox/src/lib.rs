#![forbid(unsafe_code)]

/// Boundary qualification: the signed report a release build carries for each sandbox
/// capability, read and checked against the artifacts it names before the engine admits a
/// boundary as qualified.
pub mod qualification;

#[cfg(test)]
mod tests;
