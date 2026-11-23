#![forbid(unsafe_code)]

/// Boundary qualification: the signed report a release build carries for each sandbox
/// capability, read and checked against the artifacts it names before the engine admits a
/// boundary as qualified.
/// The ELF reader: whether a Linux program is loaded by glibc, and so resolves names over
/// TCP when the sandbox asks it to, judged from its program headers alone.
pub mod elf;
pub mod qualification;
/// The owner's app-wide sandbox settings: the three Linux options and the stored value with
/// the version of the read that showed it, as the engine saves them and every client reads
/// them.
pub mod settings;

#[cfg(test)]
mod tests;
