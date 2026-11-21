pub mod state_log;

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
