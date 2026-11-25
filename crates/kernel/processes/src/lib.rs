#![forbid(unsafe_code)]

#[cfg(any(windows, test))]
pub mod batch;
/// The editor server seam: the kernel starts only a command its caller confined, and
/// refuses an unconfined one where this machine requires confinement.
#[cfg(unix)]
pub mod editor;
pub mod fd_wait;
/// Program finding: where the engine looks for the tools it runs, which files it refuses
/// and why, with the engine's own checks passed in ([`finding::FindingHooks`]).
pub mod finding;
/// One path's hold on ending a process group, so a pane's stop and the terminal host's
/// close never both signal one group and no path claims a number a new group may lead.
pub mod group;
/// Pinned installer recipes: the typed fields of each installer the engine runs, and the
/// checks that keep every field to its bounds and the bundled lists.
pub mod install;
/// Setup's process helpers: bounded command runs, version reads, file hashes and the
/// private folders setup stages into, with the error code and version types they return.
pub mod run;
/// The engine login service per platform (launchd, systemd, Task Scheduler): its definition,
/// manager commands, install and repair, and the checks that refuse an engine other users
/// can change. Engine-only steps come in through [`service::Hooks`].
pub mod service;
/// The short address a Unix socket is bound and reached at when its own path is too long
/// for a socket address.
#[cfg(unix)]
pub mod socket_address;
/// The one place the engine starts child processes and creates sockets, so no child
/// inherits a pipe or socket another thread has not marked close-on-exec yet.
pub mod spawn_gate;
/// The terminal core: pane names, the limits every host keeps, the `Host` trait a
/// terminal host implements, the bounded output buffer viewers read and the pane views
/// built on a host.
pub mod terminal;
/// App self-update: release channel, signed download, verification and the executable swap
/// that `crazyagent update` runs, with every engine stop and start left to the caller.
pub mod update;
pub mod version;

#[cfg(all(test, target_os = "macos"))]
#[path = "../../common/src/perf_kit.rs"]
mod perf_kit;
#[cfg(all(test, target_os = "macos"))]
mod perf_tests;
#[cfg(test)]
mod tests;

#[cfg(all(test, unix))]
#[path = "../../../crazyagent/tests/poll_support/mod.rs"]
mod poll_support;
