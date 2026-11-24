#![forbid(unsafe_code)]

/// The watch's event log in the engine store: the bodies after a cursor, read in bounded
/// pages, and the write that logs one revision's body and prunes the old ones.
pub mod events;
pub mod migrations;
/// One private key-value namespace per plugin on the state log, rebuilt by replay. A
/// namespace is not secret custody: its values sit in the log, snapshots and backups, so
/// secrets stay in the security module.
pub mod namespace;
/// The owner's restore of the pinned 2.4 backup, with the engine's parts as hooks, and the
/// private record that says how the last restore stands while no engine answers.
#[cfg(unix)]
pub mod restore;
pub mod state_log;
/// The engine.db upgrade runner: the version of record, the forward steps run in one
/// transaction after a checked backup, and the recovery when a step fails.
pub mod upgrade;

/// A refusal or failure of a storage call, as its stable code or message text. The engine
/// maps it to its own error with the same text, so refusals read the same after a move.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageError(pub String);

impl std::fmt::Display for StorageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for StorageError {}

#[cfg(test)]
mod tests;
