#![forbid(unsafe_code)]

/// The ELF reader: whether a Linux program is loaded by glibc, and so resolves names over
/// TCP when the sandbox asks it to, judged from its program headers alone.
pub mod elf;
/// Why a project sandbox is refused: the one error type every sandbox build and install
/// returns, with the plain reason a refused Windows read root reads as.
pub mod error;
/// The plain lines and refusal codes a launch shows when a Linux or Windows sandbox cannot
/// hold something, and the check that tells the Windows loopback refusal apart.
pub mod lines;
/// A sandboxed agent's network reach: open, or only the listed loopback and any-host ports.
pub mod net;
/// Path checks and spellings every sandbox root is held to: one spelling per path, the
/// canonical form, the plain-path tests for Unix and Windows, the owner's home and the
/// Seatbelt filter for a path and what is below it.
pub mod paths;
/// The lowest port a sandbox bridge may take.
pub mod ports;
/// Boundary qualification: the signed report a release build carries for each sandbox
/// capability, read and checked against the artifacts it names before the engine admits a
/// boundary as qualified.
pub mod qualification;
/// The owner's app-wide sandbox settings: the three Linux options and the stored value with
/// the version of the read that showed it, as the engine saves them and every client reads
/// them.
pub mod settings;

#[cfg(test)]
mod tests;
