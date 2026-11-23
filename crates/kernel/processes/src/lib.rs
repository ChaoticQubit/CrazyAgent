#![forbid(unsafe_code)]

#[cfg(any(windows, test))]
pub mod batch;
pub mod fd_wait;
/// Program finding: where the engine looks for the tools it runs, which files it refuses
/// and why, with the engine's own checks passed in ([`finding::FindingHooks`]).
pub mod finding;
/// Setup's process helpers: bounded command runs, version reads, file hashes and the
/// private folders setup stages into, with the error code and version types they return.
pub mod run;
/// The short address a Unix socket is bound and reached at when its own path is too long
/// for a socket address.
#[cfg(unix)]
pub mod socket_address;
/// The one place the engine starts child processes and creates sockets, so no child
/// inherits a pipe or socket another thread has not marked close-on-exec yet.
pub mod spawn_gate;
pub mod version;

#[cfg(test)]
mod tests;
